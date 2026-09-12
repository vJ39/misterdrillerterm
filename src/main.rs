//! ミスドリTERM: メインループ(spec.md 9章)。
//! Phase1(ノーマルコース シングルプレイ)のみを実装する。

mod audio;
mod autoplay;
mod constants;
mod debug_log;
mod game;
mod input;
mod settings;
mod ui;

use std::io;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use rand::RngExt;
use rodio::mixer::Mixer;

use constants::{
    ATTRACT_MODE_IDLE_MS, BOMB_SPAWN_RATE_PERCENT_MAX, BOMB_SPAWN_RATE_PERCENT_MIN,
    BOMB_SPAWN_RATE_PERCENT_STEP, CHAIN_VANISH_INTERVAL_MS_MAX, CHAIN_VANISH_INTERVAL_MS_STEP,
    COLOR_CLUSTER_RATE_PERCENT_MIN, COLOR_COUNT_MAX, COLOR_COUNT_MIN, DEBUG_FALL_TICK_MS_MAX,
    DEBUG_FALL_TICK_MS_MIN, DEBUG_FALL_TICK_STEP_MS, DIAMOND_SPAWN_RATE_PERCENT_MIN,
    DODGE_RECOVERY_MS_MAX, DODGE_RECOVERY_MS_STEP, FIELD_WIDTH_MAX, FIELD_WIDTH_MIN,
    FIELD_WIDTH_STEP, FRAME_INTERVAL_MS, ITEM_SPAWN_RATE_PERCENT_MIN, MOVE_COOLDOWN_MS_MAX,
    MOVE_COOLDOWN_MS_MIN, MOVE_COOLDOWN_MS_STEP, SOUND_VOLUME_PERCENT_MAX,
    SOUND_VOLUME_PERCENT_MIN, SOUND_VOLUME_PERCENT_STEP, SPAWN_RATE_PERCENT_MAX,
    SPAWN_RATE_PERCENT_MIN, SPAWN_RATE_PERCENT_STEP, SPAWN_RATE_REROLL_SAFE_MARGIN_ROWS,
    STAR_SPAWN_RATE_PERCENT_MAX, STAR_SPAWN_RATE_PERCENT_MIN, STAR_SPAWN_RATE_PERCENT_STEP,
};
use game::{Game, GameEvent, GameOverChoice, GameStatus, InputAction};
use settings::Settings;

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();

    // Kittyキーボードプロトコル(対応ターミナルのみ)を有効化する。レガシーANSIでは
    // 矢印キーと1文字キーの同時押しで生バイト列の解釈が曖昧になり得るため、
    // DISAMBIGUATE_ESCAPE_CODESで解消する。非対応ターミナルでは何もしない。
    let keyboard_enhancement_enabled = crossterm::terminal::supports_keyboard_enhancement()
        .unwrap_or(false)
        && execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok();

    let result = run(&mut terminal);

    if keyboard_enhancement_enabled {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        let _ = io::stdout().flush();
    }
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal) -> io::Result<()> {
    // 音声出力デバイスを開く。ヘッドレス環境等でデバイスが無い場合でも
    // ゲーム自体はプレイ続行できるよう、失敗時はNoneにして以後の再生をスキップする。
    let sink_handle = rodio::DeviceSinkBuilder::open_default_sink().ok();
    let mixer: Option<Mixer> = sink_handle.as_ref().map(|handle| handle.mixer().clone());

    // MUSIC/SE個別ON/OFF設定(spec.md 10章)。前回終了時の状態を復元し、
    // BGMスレッド・SE再生の双方から参照できるよう`Arc<AtomicBool>`で共有する。
    let mut settings = Settings::load();
    // タイトル画面用・プレイ中用でBGMを別トラックにする。同時に両方鳴らないよう
    // `effective_title_bgm_enabled`/`effective_gameplay_bgm_enabled`は排他的になるよう
    // 設計している。起動直後はタイトル画面から始まる。
    let title_music_enabled = Arc::new(AtomicBool::new(effective_title_bgm_enabled(
        settings.music_enabled,
        &Screen::Title,
    )));
    let gameplay_music_enabled = Arc::new(AtomicBool::new(effective_gameplay_bgm_enabled(
        settings.music_enabled,
        &Screen::Title,
    )));
    let se_enabled = Arc::new(AtomicBool::new(settings.se_enabled));
    // MUSIC音量(#224)。タイトル用・プレイ中用の両BGMスレッドで共有する
    // (MUSIC音量は画面によらず1つ)。
    let music_volume_percent = Arc::new(AtomicU32::new(settings.music_volume_percent));
    // タイトル画面へ戻るたびにタイトルBGMを先頭から再生し直すためのフラグ。
    // 起動直後の初回表示は「戻ってきた」わけではないので、ここではまだ立てない。
    let title_bgm_restart = Arc::new(AtomicBool::new(false));
    let mut was_title_bgm_enabled = title_music_enabled.load(Ordering::Relaxed);
    // タイトル画面へ戻るたびにプレイ中BGMも先頭の曲・先頭位置からリセットするフラグ。
    // `title_bgm_restart`とはトリガー条件が異なる(あちらは「無効→有効」の切り替わり、
    // こちらは「タイトル画面へ戻った瞬間」)ため、別フラグとして扱う。
    let gameplay_bgm_restart = Arc::new(AtomicBool::new(false));

    let bgm_stop = Arc::new(AtomicBool::new(false));
    if let Some(m) = &mixer {
        audio::bgm::spawn_title_bgm_thread(
            m.clone(),
            Arc::clone(&bgm_stop),
            Arc::clone(&title_music_enabled),
            Arc::clone(&title_bgm_restart),
            Arc::clone(&music_volume_percent),
        );
        audio::bgm::spawn_gameplay_bgm_thread(
            m.clone(),
            Arc::clone(&bgm_stop),
            Arc::clone(&gameplay_music_enabled),
            Arc::clone(&gameplay_bgm_restart),
            Arc::clone(&music_volume_percent),
        );
    }

    // 通常プレイはOS乱数から生成したシードを使う(spec.md 3章)。
    let mut rng = rand::rng();

    // アプリの画面状態(spec.md 1章)。タイトル画面でのEscのみアプリを終了し、それ以外の
    // 画面でのEscはGameを作り直してタイトルへ戻す(酸素・スコア・深度等が全てリセットされる)。
    let mut screen = Screen::Title;
    let mut last_tick = Instant::now();
    // モードセレクト画面での現在の選択(イージー/ノーマル)。タイトルから開くたびに、
    // 前回選んだコース(`settings.last_course_depth_m`)を初期選択として引き継ぐ。
    let mut mode_select_choice =
        ui::render::CourseChoice::from_depth_goal_m(settings.last_course_depth_m);
    // 設定画面での現在の選択項目。
    let mut settings_selection = ui::render::SettingsChoice::Music;
    // 一時停止中にオーバーレイ表示する設定/ヘルプ画面。Gameを作り直さずScreen::Playingの
    // まま上に重ねて描画するだけなので、画面遷移ではなくこのローカルな状態フラグで管理する。
    let mut pause_overlay = PauseOverlay::None;

    // ヘルプ画面(タイトルから開く独立画面)のジュークボックス状態。カーソル位置は画面を
    // 離れても保持する。再生中の曲は、その再生を制御するハンドル(stop/finishedフラグ)と
    // セットで持つ。
    let mut help_jukebox_selection: usize = 0;
    let mut help_jukebox_playing: Option<(usize, audio::bgm::JukeboxPreview)> = None;

    // オートプレイ(TERM独自拡張。#218)。Tキーで生成・破棄する。`Some`の間、
    // 毎フレーム`decide`が返す仮想入力を人間の操作と同じ経路(`Game::apply_input`)へ
    // 流し込む。Gameを作り直す場面(タイトルへ戻る)では必ずNoneへ戻す。
    let mut autopilot: Option<autoplay::Autopilot> = None;
    // 現在のオートプレイが、タイトル画面放置による自動デモ(アトラクトモード)として
    // 始まったものかどうか。手動でTキーを押した場合(=操作を引き継ぎたい)と、デモを
    // 見ていた人が割り込んだ場合(=タイトルへ戻す)で挙動を分けるために区別する。
    let mut autopilot_is_attract_demo = false;
    // タイトル画面で最後にキーが押されてからの経過時間。`ATTRACT_MODE_IDLE_MS`を
    // 超えるとアトラクトモードを自動開始する。
    let mut title_idle = Duration::ZERO;

    loop {
        // Playing→Titleへの遷移フラグ。`screen`自体への再代入は、`game`(screenを
        // 借用したバインディング)の生存期間が終わった後、if/else全体を抜けてから
        // 行う(借用中のscreenへ同時に代入できないため)。
        let mut back_to_title = false;

        if let Screen::Playing(game) = &mut screen {
            // poll_input_batch: 1フレーム内にキューされた全キーイベントを処理する。
            // 移動・向き変更と掘削をほぼ同時に押しても、同一フレームに届いた
            // 両方のイベントを取りこぼさず反映するため。
            for action in input::poll_input_batch(FRAME_INTERVAL_MS)? {
                if back_to_title {
                    break; // Quit済みなら以降のキューされたアクションは処理しない
                }
                // アトラクトモード(タイトル放置から始まった自動デモ)中に人が何か
                // 操作したら、そこから引き継ぐのではなくタイトルへ戻す(TERM独自拡張。
                // #218。デモを見ていた人の割り込みは「やめる」意思表示とみなす)。
                if autopilot_is_attract_demo {
                    // アトラクトモードは無人デモの安全策として無敵を強制ONにしている。
                    // 人が割り込んだこの時点でデモ開始前の状態へ戻す。
                    if let Some(pilot) = autopilot.take() {
                        game.set_invincible(pilot.restore_invincible());
                    }
                    back_to_title = true;
                    break;
                }
                match action {
                    // オーバーレイ(設定/ヘルプ)が開いている間のQはタイトルへ戻らず、
                    // オーバーレイを閉じるだけにする。
                    InputAction::Quit if pause_overlay != PauseOverlay::None => {
                        pause_overlay = PauseOverlay::None;
                    }
                    InputAction::Quit => back_to_title = true,
                    InputAction::TogglePause => {
                        game.toggle_pause();
                        pause_overlay = PauseOverlay::None;
                    }
                    // ポーズ解除はPだけでなく、ショートカット未割り当ての任意キーでも行える。
                    // オーバーレイ(設定/ヘルプ)表示中は対象外(そちらはQ/S/Hで明示的に閉じる)。
                    InputAction::UnboundKey
                        if game.status == GameStatus::Paused
                            && pause_overlay == PauseOverlay::None =>
                    {
                        game.toggle_pause();
                    }
                    InputAction::UnboundKey => {}
                    // M/EキーでのMUSIC/SE切り替えは、一時停止画面でのみ意味を持つ
                    // (spec.md 1章・10章)。プレイ中(Paused以外)は無視する。
                    InputAction::ToggleMusic => {
                        if game.status == GameStatus::Paused {
                            settings.music_enabled = !settings.music_enabled;
                            // ここはScreen::Playing(かつPaused)確定なので、タイトル用
                            // BGMは触れず(既に無音のはず)、プレイ中BGMのみ即時反映する。
                            gameplay_music_enabled.store(settings.music_enabled, Ordering::Relaxed);
                            settings.save();
                        }
                    }
                    InputAction::ToggleSe => {
                        if game.status == GameStatus::Paused {
                            settings.se_enabled = !settings.se_enabled;
                            se_enabled.store(settings.se_enabled, Ordering::Relaxed);
                            settings.save();
                        }
                    }
                    // S/Hキーでの設定/ヘルプ画面オーバーレイ表示。プレイ中に押した場合は
                    // 自動的に一時停止してからオーバーレイを開く。同じキーの再入力で閉じる
                    // (閉じても一時停止状態はそのまま、Pキーで別途再開する)。
                    InputAction::OpenSettings => {
                        if game.status == GameStatus::Playing {
                            game.toggle_pause();
                        }
                        if game.status == GameStatus::Paused {
                            pause_overlay = if pause_overlay == PauseOverlay::Settings {
                                PauseOverlay::None
                            } else {
                                PauseOverlay::Settings
                            };
                        }
                    }
                    InputAction::OpenHelp => {
                        if game.status == GameStatus::Playing {
                            game.toggle_pause();
                        }
                        if game.status == GameStatus::Paused {
                            pause_overlay = if pause_overlay == PauseOverlay::Help {
                                PauseOverlay::None
                            } else {
                                PauseOverlay::Help
                            };
                        }
                    }
                    // 設定オーバーレイ表示中は上下キー/Spaceを選択操作として扱う(タイトル画面
                    // のScreen::Settingsと同じ操作感)。
                    InputAction::FaceUp if pause_overlay == PauseOverlay::Settings => {
                        settings_selection = settings_selection.cycle_back();
                    }
                    InputAction::FaceDown if pause_overlay == PauseOverlay::Settings => {
                        settings_selection = settings_selection.cycle();
                    }
                    InputAction::Drill if pause_overlay == PauseOverlay::Settings => {
                        match settings_selection {
                            ui::render::SettingsChoice::Music => {
                                settings.music_enabled = !settings.music_enabled;
                                gameplay_music_enabled
                                    .store(settings.music_enabled, Ordering::Relaxed);
                                settings.save();
                            }
                            ui::render::SettingsChoice::Se => {
                                settings.se_enabled = !settings.se_enabled;
                                se_enabled.store(settings.se_enabled, Ordering::Relaxed);
                                settings.save();
                            }
                            // 調査用のブロック状態遷移ログのON/OFF。一時停止中の
                            // オーバーレイからは、稼働中のgameへも即座に反映する
                            // (無効化時は記録を止め、有効化時は新規にログを開き直す)。
                            ui::render::SettingsChoice::DebugLogEnabled => {
                                settings.debug_log_enabled = !settings.debug_log_enabled;
                                game.refresh_debug_log(settings.debug_log_enabled);
                                settings.save();
                            }
                            // 配分率・色数・落下速度・回避硬直時間・音量は←→で調整するので、
                            // Spaceは無効(トグル対象ではない)。
                            ui::render::SettingsChoice::MusicVolume
                            | ui::render::SettingsChoice::SeVolume
                            | ui::render::SettingsChoice::RockRate
                            | ui::render::SettingsChoice::AirRate
                            | ui::render::SettingsChoice::StarRate
                            | ui::render::SettingsChoice::DiamondRate
                            | ui::render::SettingsChoice::ItemClearAboveRate
                            | ui::render::SettingsChoice::ItemUnifyColorsRate
                            | ui::render::SettingsChoice::ItemStarifyScreenRate
                            | ui::render::SettingsChoice::ColorCount
                            | ui::render::SettingsChoice::ColorClusterRate
                            | ui::render::SettingsChoice::FieldWidth
                            | ui::render::SettingsChoice::BlockFallSpeed
                            | ui::render::SettingsChoice::PlayerFallSpeed
                            | ui::render::SettingsChoice::MoveSpeed
                            | ui::render::SettingsChoice::DodgeRecoveryMs
                            | ui::render::SettingsChoice::BombRate
                            | ui::render::SettingsChoice::ChainVanishInterval => {}
                        }
                    }
                    // MUSIC/SEのトグルは←→キーでも行える。トグルなので方向は問わず、
                    // 押されたら反転する。
                    InputAction::MoveLeft | InputAction::MoveRight
                        if pause_overlay == PauseOverlay::Settings
                            && matches!(
                                settings_selection,
                                ui::render::SettingsChoice::Music
                                    | ui::render::SettingsChoice::Se
                                    | ui::render::SettingsChoice::DebugLogEnabled
                            ) =>
                    {
                        match settings_selection {
                            ui::render::SettingsChoice::Music => {
                                settings.music_enabled = !settings.music_enabled;
                                gameplay_music_enabled
                                    .store(settings.music_enabled, Ordering::Relaxed);
                            }
                            ui::render::SettingsChoice::Se => {
                                settings.se_enabled = !settings.se_enabled;
                                se_enabled.store(settings.se_enabled, Ordering::Relaxed);
                            }
                            ui::render::SettingsChoice::DebugLogEnabled => {
                                settings.debug_log_enabled = !settings.debug_log_enabled;
                                game.refresh_debug_log(settings.debug_log_enabled);
                            }
                            _ => {}
                        }
                        settings.save();
                    }
                    // MUSIC/SEの音量調整(#224)。ON/OFFとは別に、アプリ内部のミックス
                    // ゲインだけを変える。SE音量は変更のたびに確認用サンプルSE(酸素
                    // カプセル取得音)を1回鳴らす。
                    InputAction::MoveLeft | InputAction::MoveRight
                        if pause_overlay == PauseOverlay::Settings
                            && matches!(
                                settings_selection,
                                ui::render::SettingsChoice::MusicVolume
                                    | ui::render::SettingsChoice::SeVolume
                            ) =>
                    {
                        let increase = action == InputAction::MoveRight;
                        match settings_selection {
                            ui::render::SettingsChoice::MusicVolume => {
                                settings.music_volume_percent = adjust_sound_volume_percent(
                                    settings.music_volume_percent,
                                    increase,
                                );
                                music_volume_percent
                                    .store(settings.music_volume_percent, Ordering::Relaxed);
                            }
                            ui::render::SettingsChoice::SeVolume => {
                                settings.se_volume_percent = adjust_sound_volume_percent(
                                    settings.se_volume_percent,
                                    increase,
                                );
                                if settings.se_enabled
                                    && let Some(m) = mixer.as_ref()
                                {
                                    audio::sfx::play_oxygen_pickup(
                                        m,
                                        audio::sfx::se_gain(settings.se_volume_percent),
                                    );
                                }
                            }
                            _ => {}
                        }
                        settings.save();
                    }
                    // ブロック落下速度・キャラ落下速度・回避硬直時間の調整。
                    // 配分率・色数と異なり盤面の書き換えを伴わないため、即座にgameへ反映してよい。
                    InputAction::MoveLeft | InputAction::MoveRight
                        if pause_overlay == PauseOverlay::Settings
                            && matches!(
                                settings_selection,
                                ui::render::SettingsChoice::BlockFallSpeed
                                    | ui::render::SettingsChoice::PlayerFallSpeed
                                    | ui::render::SettingsChoice::MoveSpeed
                                    | ui::render::SettingsChoice::DodgeRecoveryMs
                                    | ui::render::SettingsChoice::BombRate
                                    | ui::render::SettingsChoice::ChainVanishInterval
                            ) =>
                    {
                        let increase = action == InputAction::MoveRight;
                        match settings_selection {
                            ui::render::SettingsChoice::BlockFallSpeed => {
                                settings.block_fall_tick_ms =
                                    adjust_fall_speed_ms(settings.block_fall_tick_ms, increase);
                                game.set_block_fall_tick_ms(settings.block_fall_tick_ms);
                            }
                            ui::render::SettingsChoice::PlayerFallSpeed => {
                                settings.player_fall_tick_ms =
                                    adjust_fall_speed_ms(settings.player_fall_tick_ms, increase);
                                game.set_player_fall_tick_ms(settings.player_fall_tick_ms);
                            }
                            ui::render::SettingsChoice::MoveSpeed => {
                                settings.move_cooldown_ms =
                                    adjust_move_cooldown_ms(settings.move_cooldown_ms, increase);
                                game.set_move_cooldown_ms(settings.move_cooldown_ms);
                            }
                            ui::render::SettingsChoice::DodgeRecoveryMs => {
                                settings.dodge_recovery_ms =
                                    adjust_dodge_recovery_ms(settings.dodge_recovery_ms, increase);
                                game.set_dodge_recovery_ms(settings.dodge_recovery_ms);
                            }
                            ui::render::SettingsChoice::BombRate => {
                                settings.bomb_spawn_rate_percent = adjust_bomb_rate_percent(
                                    settings.bomb_spawn_rate_percent,
                                    increase,
                                );
                                game.set_bomb_spawn_rate_percent(settings.bomb_spawn_rate_percent);
                            }
                            ui::render::SettingsChoice::ChainVanishInterval => {
                                settings.chain_vanish_interval_ms = adjust_chain_vanish_interval_ms(
                                    settings.chain_vanish_interval_ms,
                                    increase,
                                );
                                game.set_chain_vanish_interval_ms(
                                    settings.chain_vanish_interval_ms,
                                );
                            }
                            _ => {}
                        }
                        settings.save();
                    }
                    // フィールド幅(列数)の調整。盤面の列数そのものを変えるため
                    // 現在の盤面には反映できず、次回の新規ゲーム開始時にのみ適用される。
                    InputAction::MoveLeft | InputAction::MoveRight
                        if pause_overlay == PauseOverlay::Settings
                            && settings_selection == ui::render::SettingsChoice::FieldWidth =>
                    {
                        let increase = action == InputAction::MoveRight;
                        settings.field_width = adjust_field_width(settings.field_width, increase);
                        settings.save();
                    }
                    // Xブロック/AIR/スター/ダイヤの配分率・色数調整。プレイ中なので、
                    // 既に画面に見えている範囲は変えず、十分先(画面外)から新しい配分率を反映する。
                    InputAction::MoveLeft | InputAction::MoveRight
                        if pause_overlay == PauseOverlay::Settings
                            && matches!(
                                settings_selection,
                                ui::render::SettingsChoice::RockRate
                                    | ui::render::SettingsChoice::AirRate
                                    | ui::render::SettingsChoice::StarRate
                                    | ui::render::SettingsChoice::DiamondRate
                                    | ui::render::SettingsChoice::ItemClearAboveRate
                                    | ui::render::SettingsChoice::ItemUnifyColorsRate
                                    | ui::render::SettingsChoice::ItemStarifyScreenRate
                                    | ui::render::SettingsChoice::ColorCount
                                    | ui::render::SettingsChoice::ColorClusterRate
                            ) =>
                    {
                        let increase = action == InputAction::MoveRight;
                        adjust_spawn_rate_setting(&mut settings, settings_selection, increase);
                        settings.save();
                        let from_row = game.player.row + SPAWN_RATE_REROLL_SAFE_MARGIN_ROWS;
                        game.reroll_spawn_rates_from(
                            from_row,
                            settings.rock_spawn_rate_percent,
                            settings.air_spawn_rate_percent,
                            settings.star_spawn_rate_percent,
                            settings.diamond_spawn_rate_percent,
                            settings.item_clear_above_rate_percent,
                            settings.item_unify_colors_rate_percent,
                            settings.item_starify_screen_rate_percent,
                            settings.color_count,
                            settings.color_cluster_rate_percent,
                        );
                    }
                    // GameOverダイアログ中は上下キー/Spaceを選択操作として扱う
                    // (タイトルへ戻るか、その場から復活して再開するかを選ぶ)。
                    InputAction::FaceUp | InputAction::FaceDown
                        if game.status == GameStatus::GameOver =>
                    {
                        game.toggle_game_over_selection();
                    }
                    InputAction::Confirm if game.status == GameStatus::GameOver => {
                        match game.game_over_selection() {
                            GameOverChoice::BackToTitle => back_to_title = true,
                            GameOverChoice::Revive => game.revive(),
                        }
                    }
                    InputAction::Confirm => {}
                    // 移動・向き・掘削の5操作は`apply_input`へ統一する(TERM独自拡張。
                    // #218)。オートプレイの仮想入力と全く同じ経路を通ることで、
                    // AIだけが使える裏口が生まれないようにする。人が実際に操作した
                    // 時点でオートプレイは解除し、無敵も開始前の状態へ戻す。
                    InputAction::MoveLeft
                    | InputAction::MoveRight
                    | InputAction::FaceUp
                    | InputAction::FaceDown
                    | InputAction::Drill => {
                        // 人が操作した時点でオートプレイは解除する。無敵はGキーが
                        // 単独で管理するため、ここでは変更しない(#221)。
                        autopilot = None;
                        let events = game.apply_input(action);
                        handle_events(
                            &events,
                            mixer.as_ref(),
                            &se_enabled,
                            settings.se_volume_percent,
                        );
                    }
                    // T: オートプレイのON/OFF。無敵は連動させず、Gキーの状態をそのまま
                    // 残す(#221。AIが無敵に頼らず生き延びられるかをTだけで試せるように
                    // するため。無人のアトラクトモードだけは安全策として無敵もONにする)。
                    InputAction::DebugToggleAutopilot => {
                        autopilot = match autopilot.take() {
                            Some(_) => None,
                            None => Some(autoplay::Autopilot::new(game.is_invincible())),
                        };
                    }
                    // G: 無敵の単独トグル。オートプレイとは独立して切り替えられる。
                    InputAction::DebugToggleInvincible => {
                        game.set_invincible(!game.is_invincible());
                    }
                    InputAction::DebugUnifyNearbyColors => {
                        let events = game.debug_unify_nearby_colors();
                        handle_events(
                            &events,
                            mixer.as_ref(),
                            &se_enabled,
                            settings.se_volume_percent,
                        );
                    }
                    InputAction::DebugAddLife => game.debug_add_life(),
                    InputAction::DebugFillAir => game.debug_fill_air(),
                    InputAction::DebugClearAbovePlayer => game.debug_clear_above_player(),
                    InputAction::DebugStarifyVisibleScreen => game.debug_starify_visible_screen(),
                    InputAction::DebugPlaceBomb => game.debug_place_bomb(),
                    InputAction::DebugBlockFallSlower => {
                        game.debug_adjust_block_fall_speed(false);
                        settings.block_fall_tick_ms = game.block_fall_tick_ms();
                        settings.save();
                    }
                    InputAction::DebugBlockFallFaster => {
                        game.debug_adjust_block_fall_speed(true);
                        settings.block_fall_tick_ms = game.block_fall_tick_ms();
                        settings.save();
                    }
                    InputAction::DebugPlayerFallSlower => {
                        game.debug_adjust_player_fall_speed(false);
                        settings.player_fall_tick_ms = game.player_fall_tick_ms();
                        settings.save();
                    }
                    InputAction::DebugPlayerFallFaster => {
                        game.debug_adjust_player_fall_speed(true);
                        settings.player_fall_tick_ms = game.player_fall_tick_ms();
                        settings.save();
                    }
                    InputAction::DebugShakeDurationLonger => {
                        game.debug_adjust_shake_duration(true);
                        settings.shake_duration_ms = game.shake_duration_ms();
                        settings.save();
                    }
                    InputAction::DebugShakeDurationShorter => {
                        game.debug_adjust_shake_duration(false);
                        settings.shake_duration_ms = game.shake_duration_ms();
                        settings.save();
                    }
                }
            }

            if !back_to_title {
                // オートプレイ(TERM独自拡張。#218)。盤面から決めた仮想入力を、人の
                // 操作と同じ経路へ流し込む。GameOverからの自動復活だけはダイアログの
                // 選択操作にあたるため、Confirmをrevive()の呼び出しへ読み替える。
                if let Some(pilot) = autopilot.as_mut() {
                    for action in pilot.decide(game) {
                        let events = match action {
                            InputAction::Confirm => {
                                game.revive();
                                Vec::new()
                            }
                            other => game.apply_input(other),
                        };
                        handle_events(
                            &events,
                            mixer.as_ref(),
                            &se_enabled,
                            settings.se_volume_percent,
                        );
                    }
                }

                let now = Instant::now();
                let delta = now.duration_since(last_tick);
                last_tick = now;

                let events = game.update(delta.min(Duration::from_millis(250)));
                handle_events(
                    &events,
                    mixer.as_ref(),
                    &se_enabled,
                    settings.se_volume_percent,
                );

                let music_on = gameplay_music_enabled.load(Ordering::Relaxed);
                let se_on = se_enabled.load(Ordering::Relaxed);
                let autoplay_on = autopilot.is_some();
                terminal.draw(|frame| {
                    ui::render::draw(frame, game, music_on, se_on, autoplay_on);
                    // 一時停止中の設定/ヘルプオーバーレイ。Screen::Playingのまま
                    // Gameを手放さずに上へ重ね描きするだけで、専用のScreen遷移は行わない。
                    match pause_overlay {
                        PauseOverlay::None => {}
                        PauseOverlay::Settings => ui::render::draw_settings(
                            frame,
                            settings_selection,
                            music_on,
                            se_on,
                            settings.music_volume_percent,
                            settings.se_volume_percent,
                            settings.rock_spawn_rate_percent,
                            settings.air_spawn_rate_percent,
                            settings.star_spawn_rate_percent,
                            settings.diamond_spawn_rate_percent,
                            settings.item_clear_above_rate_percent,
                            settings.item_unify_colors_rate_percent,
                            settings.item_starify_screen_rate_percent,
                            settings.color_count,
                            settings.color_cluster_rate_percent,
                            settings.field_width,
                            settings.block_fall_tick_ms,
                            settings.player_fall_tick_ms,
                            settings.move_cooldown_ms,
                            settings.dodge_recovery_ms,
                            settings.bomb_spawn_rate_percent,
                            settings.debug_log_enabled,
                            settings.chain_vanish_interval_ms,
                            false,
                        ),
                        PauseOverlay::Help => ui::render::draw_help(frame, None, false),
                    }
                })?;
            }
        } else if let Screen::Settings = screen {
            let music_on = settings.music_enabled;
            let se_on = settings.se_enabled;
            terminal.draw(|frame| {
                ui::render::draw_settings(
                    frame,
                    settings_selection,
                    music_on,
                    se_on,
                    settings.music_volume_percent,
                    settings.se_volume_percent,
                    settings.rock_spawn_rate_percent,
                    settings.air_spawn_rate_percent,
                    settings.star_spawn_rate_percent,
                    settings.diamond_spawn_rate_percent,
                    settings.item_clear_above_rate_percent,
                    settings.item_unify_colors_rate_percent,
                    settings.item_starify_screen_rate_percent,
                    settings.color_count,
                    settings.color_cluster_rate_percent,
                    settings.field_width,
                    settings.block_fall_tick_ms,
                    settings.player_fall_tick_ms,
                    settings.move_cooldown_ms,
                    settings.dodge_recovery_ms,
                    settings.bomb_spawn_rate_percent,
                    settings.debug_log_enabled,
                    settings.chain_vanish_interval_ms,
                    true,
                )
            })?;

            // 設定画面もpoll_input_batchを使う(FaceUp/FaceDown=選択切替、Drill=トグル、
            // MoveLeft/MoveRight=配分率調整、Quit=タイトルへ戻る、と既存のInputActionを
            // そのまま再利用できるため)。
            for action in input::poll_input_batch(FRAME_INTERVAL_MS)? {
                match action {
                    InputAction::Quit => screen = Screen::Title,
                    InputAction::FaceUp => {
                        settings_selection = settings_selection.cycle_back();
                    }
                    InputAction::FaceDown => {
                        settings_selection = settings_selection.cycle();
                    }
                    InputAction::Drill => match settings_selection {
                        ui::render::SettingsChoice::Music => {
                            settings.music_enabled = !settings.music_enabled;
                            gameplay_music_enabled.store(settings.music_enabled, Ordering::Relaxed);
                            settings.save();
                        }
                        ui::render::SettingsChoice::Se => {
                            settings.se_enabled = !settings.se_enabled;
                            se_enabled.store(settings.se_enabled, Ordering::Relaxed);
                            settings.save();
                        }
                        // 調査用のブロック状態遷移ログのON/OFF。このScreen::Settings
                        // (タイトルから開く独立画面)にはgameが無いため、次回のゲーム
                        // 開始時(refresh_debug_log呼び出し時)に反映される。
                        ui::render::SettingsChoice::DebugLogEnabled => {
                            settings.debug_log_enabled = !settings.debug_log_enabled;
                            settings.save();
                        }
                        ui::render::SettingsChoice::MusicVolume
                        | ui::render::SettingsChoice::SeVolume
                        | ui::render::SettingsChoice::RockRate
                        | ui::render::SettingsChoice::AirRate
                        | ui::render::SettingsChoice::StarRate
                        | ui::render::SettingsChoice::DiamondRate
                        | ui::render::SettingsChoice::ItemClearAboveRate
                        | ui::render::SettingsChoice::ItemUnifyColorsRate
                        | ui::render::SettingsChoice::ItemStarifyScreenRate
                        | ui::render::SettingsChoice::ColorCount
                        | ui::render::SettingsChoice::ColorClusterRate
                        | ui::render::SettingsChoice::FieldWidth
                        | ui::render::SettingsChoice::BlockFallSpeed
                        | ui::render::SettingsChoice::PlayerFallSpeed
                        | ui::render::SettingsChoice::MoveSpeed
                        | ui::render::SettingsChoice::DodgeRecoveryMs
                        | ui::render::SettingsChoice::BombRate
                        | ui::render::SettingsChoice::ChainVanishInterval => {}
                    },
                    // MUSIC/SEのトグルはSpace(TogglePause)・←→キーでも行える(ヘルプ表示
                    // 「Spaceか←→でトグル」と一致させるため)。一時停止中のオーバーレイの
                    // Spaceは別途「閉じて再開する」処理を持つため対象外。方向は問わず反転する。
                    InputAction::TogglePause | InputAction::MoveLeft | InputAction::MoveRight
                        if matches!(
                            settings_selection,
                            ui::render::SettingsChoice::Music
                                | ui::render::SettingsChoice::Se
                                | ui::render::SettingsChoice::DebugLogEnabled
                        ) =>
                    {
                        match settings_selection {
                            ui::render::SettingsChoice::Music => {
                                settings.music_enabled = !settings.music_enabled;
                                gameplay_music_enabled
                                    .store(settings.music_enabled, Ordering::Relaxed);
                            }
                            ui::render::SettingsChoice::Se => {
                                settings.se_enabled = !settings.se_enabled;
                                se_enabled.store(settings.se_enabled, Ordering::Relaxed);
                            }
                            ui::render::SettingsChoice::DebugLogEnabled => {
                                settings.debug_log_enabled = !settings.debug_log_enabled;
                            }
                            _ => {}
                        }
                        settings.save();
                    }
                    // MUSIC/SEの音量調整(#224)。一時停止オーバーレイと同じ挙動で、
                    // SE音量は変更のたびに確認用サンプルSEを1回鳴らす。
                    InputAction::MoveLeft | InputAction::MoveRight
                        if matches!(
                            settings_selection,
                            ui::render::SettingsChoice::MusicVolume
                                | ui::render::SettingsChoice::SeVolume
                        ) =>
                    {
                        let increase = action == InputAction::MoveRight;
                        match settings_selection {
                            ui::render::SettingsChoice::MusicVolume => {
                                settings.music_volume_percent = adjust_sound_volume_percent(
                                    settings.music_volume_percent,
                                    increase,
                                );
                                music_volume_percent
                                    .store(settings.music_volume_percent, Ordering::Relaxed);
                            }
                            ui::render::SettingsChoice::SeVolume => {
                                settings.se_volume_percent = adjust_sound_volume_percent(
                                    settings.se_volume_percent,
                                    increase,
                                );
                                if settings.se_enabled
                                    && let Some(m) = mixer.as_ref()
                                {
                                    audio::sfx::play_oxygen_pickup(
                                        m,
                                        audio::sfx::se_gain(settings.se_volume_percent),
                                    );
                                }
                            }
                            _ => {}
                        }
                        settings.save();
                    }
                    InputAction::MoveLeft | InputAction::MoveRight
                        if matches!(
                            settings_selection,
                            ui::render::SettingsChoice::RockRate
                                | ui::render::SettingsChoice::AirRate
                                | ui::render::SettingsChoice::StarRate
                                | ui::render::SettingsChoice::DiamondRate
                                | ui::render::SettingsChoice::ItemClearAboveRate
                                | ui::render::SettingsChoice::ItemUnifyColorsRate
                                | ui::render::SettingsChoice::ItemStarifyScreenRate
                                | ui::render::SettingsChoice::ColorCount
                                | ui::render::SettingsChoice::ColorClusterRate
                                | ui::render::SettingsChoice::FieldWidth
                                | ui::render::SettingsChoice::BlockFallSpeed
                                | ui::render::SettingsChoice::PlayerFallSpeed
                                | ui::render::SettingsChoice::MoveSpeed
                                | ui::render::SettingsChoice::DodgeRecoveryMs
                                | ui::render::SettingsChoice::BombRate
                                | ui::render::SettingsChoice::ChainVanishInterval
                        ) =>
                    {
                        let increase = action == InputAction::MoveRight;
                        if adjust_spawn_rate_setting(&mut settings, settings_selection, increase) {
                            settings.save();
                            continue;
                        }
                        match settings_selection {
                            ui::render::SettingsChoice::FieldWidth => {
                                settings.field_width =
                                    adjust_field_width(settings.field_width, increase);
                            }
                            ui::render::SettingsChoice::BlockFallSpeed => {
                                settings.block_fall_tick_ms =
                                    adjust_fall_speed_ms(settings.block_fall_tick_ms, increase);
                            }
                            ui::render::SettingsChoice::PlayerFallSpeed => {
                                settings.player_fall_tick_ms =
                                    adjust_fall_speed_ms(settings.player_fall_tick_ms, increase);
                            }
                            ui::render::SettingsChoice::MoveSpeed => {
                                settings.move_cooldown_ms =
                                    adjust_move_cooldown_ms(settings.move_cooldown_ms, increase);
                            }
                            ui::render::SettingsChoice::DodgeRecoveryMs => {
                                settings.dodge_recovery_ms =
                                    adjust_dodge_recovery_ms(settings.dodge_recovery_ms, increase);
                            }
                            ui::render::SettingsChoice::BombRate => {
                                settings.bomb_spawn_rate_percent = adjust_bomb_rate_percent(
                                    settings.bomb_spawn_rate_percent,
                                    increase,
                                );
                            }
                            ui::render::SettingsChoice::ChainVanishInterval => {
                                settings.chain_vanish_interval_ms = adjust_chain_vanish_interval_ms(
                                    settings.chain_vanish_interval_ms,
                                    increase,
                                );
                            }
                            _ => {}
                        }
                        settings.save();
                    }
                    _ => {}
                }
            }
        } else if let Screen::Help = screen {
            // 曲が最後まで自然に終わっていたら、再生中表示を消す。
            if help_jukebox_playing
                .as_ref()
                .is_some_and(|(_, preview)| preview.is_finished())
            {
                help_jukebox_playing = None;
            }

            let jukebox_state = ui::render::HelpJukeboxState {
                selection: help_jukebox_selection,
                playing: help_jukebox_playing.as_ref().map(|(idx, _)| *idx),
            };
            terminal.draw(|frame| ui::render::draw_help(frame, Some(&jukebox_state), true))?;

            // ヘルプ画面はEscキーでタイトルへ戻る。↑/↓で曲を選び、
            // X/Zで再生・停止するジュークボックス操作を持つ。
            for action in input::poll_input_batch(FRAME_INTERVAL_MS)? {
                match action {
                    InputAction::Quit => {
                        if let Some((_, preview)) = help_jukebox_playing.take() {
                            preview.stop();
                        }
                        screen = Screen::Title;
                    }
                    InputAction::FaceUp => {
                        help_jukebox_selection = cycle_jukebox_selection(
                            help_jukebox_selection,
                            audio::bgm::JUKEBOX_TRACKS.len(),
                            false,
                        );
                    }
                    InputAction::FaceDown => {
                        help_jukebox_selection = cycle_jukebox_selection(
                            help_jukebox_selection,
                            audio::bgm::JUKEBOX_TRACKS.len(),
                            true,
                        );
                    }
                    InputAction::Drill => {
                        if let Some(m) = &mixer {
                            let already_playing_selection =
                                help_jukebox_playing.as_ref().map(|(idx, _)| *idx)
                                    == Some(help_jukebox_selection);
                            if let Some((_, preview)) = help_jukebox_playing.take() {
                                preview.stop();
                            }
                            if !already_playing_selection {
                                let (_, track) = audio::bgm::JUKEBOX_TRACKS[help_jukebox_selection];
                                let preview = audio::bgm::start_jukebox_preview(
                                    m,
                                    track,
                                    settings.music_volume_percent,
                                );
                                help_jukebox_playing = Some((help_jukebox_selection, preview));
                            }
                        }
                    }
                    _ => {}
                }
            }
        } else if let Screen::ModeSelect = screen {
            terminal.draw(|frame| ui::render::draw_mode_select(frame, mode_select_choice))?;

            // モードセレクト画面。↑/↓・←/→どちらでもイージー/ノーマルを
            // 切り替えられるようにする(設定画面の選択操作と揃える)。
            for action in input::poll_input_batch(FRAME_INTERVAL_MS)? {
                match action {
                    InputAction::Quit => screen = Screen::Title,
                    InputAction::FaceUp
                    | InputAction::FaceDown
                    | InputAction::MoveLeft
                    | InputAction::MoveRight => {
                        mode_select_choice = mode_select_choice.toggle();
                    }
                    InputAction::Confirm => {
                        let depth_goal_m = mode_select_choice.depth_goal_m();
                        settings.last_course_depth_m = depth_goal_m;
                        settings.save();
                        let game = start_new_game(rng.random(), &settings, depth_goal_m);
                        screen = Screen::Playing(Box::new(game));
                        last_tick = Instant::now();
                    }
                    _ => {}
                }
            }
        } else {
            terminal.draw(ui::render::draw_title)?;

            let title_frame_started = Instant::now();
            let key = input::poll_any_key(FRAME_INTERVAL_MS)?;
            // アトラクトモード(TERM独自拡張。#218)のアイドル計測。`poll_any_key`は
            // 最大FRAME_INTERVAL_MSだけ待つが、キーが来れば早く返るため、実際の
            // 経過時間で数える。どのキーであっても(画面遷移しないキーでも)
            // 押された時点でタイマーは0へ戻す。
            if key.is_some() {
                title_idle = Duration::ZERO;
            } else {
                title_idle += title_frame_started.elapsed();
            }

            if let Some(action) = key {
                match action {
                    input::AnyKeyAction::Quit => break,
                    input::AnyKeyAction::OpenSettings => screen = Screen::Settings,
                    input::AnyKeyAction::OpenHelp => screen = Screen::Help,
                    input::AnyKeyAction::Advance => {
                        mode_select_choice = ui::render::CourseChoice::from_depth_goal_m(
                            settings.last_course_depth_m,
                        );
                        screen = Screen::ModeSelect;
                    }
                    // 画面遷移は起こさないが、アイドルタイマーのリセットは上で済んでいる。
                    input::AnyKeyAction::Ignored => {}
                }
            } else if title_idle >= Duration::from_millis(ATTRACT_MODE_IDLE_MS) {
                // 放置されたので自動デモを始める。モードセレクトは挟まず、前回選んだ
                // コースでそのまま開始し、無敵ONのオートプレイに操作を任せる。
                let mut game =
                    start_new_game(rng.random(), &settings, settings.last_course_depth_m);
                // 無人で回り続けるデモなので、手動のTキー(#221で無敵と切り離した)とは
                // 違い、ここだけは安全策として無敵も自動でONにする。
                game.set_invincible(true);
                // 新規に作ったゲームなので、デモ開始前の無敵状態は常にOFF。
                autopilot = Some(autoplay::Autopilot::new(false));
                autopilot_is_attract_demo = true;
                title_idle = Duration::ZERO;
                screen = Screen::Playing(Box::new(game));
                last_tick = Instant::now();
            }
        }

        if back_to_title {
            screen = Screen::Title;
            pause_overlay = PauseOverlay::None;
            // タイトルへ戻るとGameごと破棄されるため、オートプレイも必ず手放す
            // (TERM独自拡張。#218)。アイドルタイマーも0から数え直す。
            autopilot = None;
            autopilot_is_attract_demo = false;
            title_idle = Duration::ZERO;
            // タイトル画面へ戻った瞬間にプレイ中BGMもリセットする。次にプレイを始めた
            // とき、前回の再生位置・曲順を引きずらず必ず1曲目の先頭から鳴るようにする。
            gameplay_bgm_restart.store(true, Ordering::Relaxed);
        }

        // 画面遷移(タイトルへ戻る/タイトルから抜ける)を反映して、BGMスレッドが参照する
        // 実効MUSIC状態を毎フレーム同期する。タイトル用・プレイ中用のいずれか一方だけがtrueになる。
        let title_bgm_now_enabled = effective_title_bgm_enabled(settings.music_enabled, &screen);
        title_music_enabled.store(title_bgm_now_enabled, Ordering::Relaxed);
        gameplay_music_enabled.store(
            effective_gameplay_bgm_enabled(settings.music_enabled, &screen),
            Ordering::Relaxed,
        );
        // タイトル画面へ戻ってきた(無効→有効に転じた)瞬間に、タイトルBGMを
        // 先頭から再生し直す。
        if should_restart_title_bgm(was_title_bgm_enabled, title_bgm_now_enabled) {
            title_bgm_restart.store(true, Ordering::Relaxed);
        }
        was_title_bgm_enabled = title_bgm_now_enabled;
    }

    bgm_stop.store(true, Ordering::Relaxed);

    Ok(())
}

/// 新規ゲームを1つ作り、永続化された設定を全て反映して返す。モードセレクトでEnterを
/// 押した場合と、タイトル画面の放置から始まるアトラクトモード(TERM独自拡張。#218)の
/// どちらからも同じ経路を通すため、共通の関数として切り出している。
fn start_new_game(seed: u64, settings: &Settings, depth_goal_m: usize) -> Game {
    // フィールド幅(列数)設定は新規ゲーム開始時にのみ反映される。
    let mut game = Game::new_with_width(seed, settings.field_width, depth_goal_m);
    // 調査用のブロック状態遷移ログをタイトルからのゲーム開始時に毎回作り直す。
    // 設定画面のトグルで無効化していれば記録自体を行わない。
    game.refresh_debug_log(settings.debug_log_enabled);
    // 速度系デバッグショートカットの調整値は設定ファイルに永続化されており
    // (settings.rs)、新しいゲーム開始時にも引き継ぐ。
    game.set_block_fall_tick_ms(settings.block_fall_tick_ms);
    game.set_player_fall_tick_ms(settings.player_fall_tick_ms);
    game.set_shake_duration_ms(settings.shake_duration_ms);
    game.set_dodge_recovery_ms(settings.dodge_recovery_ms);
    game.set_move_cooldown_ms(settings.move_cooldown_ms);
    game.set_bomb_spawn_rate_percent(settings.bomb_spawn_rate_percent);
    game.set_chain_vanish_interval_ms(settings.chain_vanish_interval_ms);
    // Xブロック/AIR/スター/ダイヤの配分率設定も、新規ゲーム開始時に
    // 安全地帯明け(行2)以降の全体へ反映する。
    game.reroll_spawn_rates_from(
        2,
        settings.rock_spawn_rate_percent,
        settings.air_spawn_rate_percent,
        settings.star_spawn_rate_percent,
        settings.diamond_spawn_rate_percent,
        settings.item_clear_above_rate_percent,
        settings.item_unify_colors_rate_percent,
        settings.item_starify_screen_rate_percent,
        settings.color_count,
        settings.color_cluster_rate_percent,
    );
    game
}

/// MUSIC設定・現在の画面から、実際にタイトル画面用BGMを鳴らすべきかを判定する。
/// タイトル画面にいる間だけ鳴らす。
fn effective_title_bgm_enabled(settings_music_enabled: bool, screen: &Screen) -> bool {
    // モードセレクト画面はタイトルから直接つながる短い経由画面のため、
    // タイトルBGMをそのまま鳴らし続ける(往復で途切れさせない)。
    settings_music_enabled && matches!(screen, Screen::Title | Screen::ModeSelect)
}

/// MUSIC設定・現在の画面から、実際にプレイ中BGM(交代制プレイリスト)を鳴らすべきかを
/// 判定する。タイトル画面・ゲームオーバー中・ゴールクリア後はMUSIC設定のON/OFFに
/// 関わらず常に無音にする(クリアファンファーレはBGMと別にSEとして再生される)。
fn effective_gameplay_bgm_enabled(settings_music_enabled: bool, screen: &Screen) -> bool {
    if !settings_music_enabled {
        return false;
    }
    match screen {
        Screen::Title | Screen::ModeSelect => false,
        Screen::Playing(game) => matches!(game.status, GameStatus::Playing | GameStatus::Paused),
        Screen::Settings => true,
        // 独立画面としてのヘルプはジュークボックス試聴の置き場のため、プレイ中BGMを
        // 流すと試聴と二重に聞こえてしまう。常に無音にし、聞こえる音は選んだ曲のプレビュー
        // だけにする。一時停止中のヘルプオーバーレイは`Screen::Playing`のままなので対象外。
        Screen::Help => false,
    }
}

/// タイトルBGMを先頭から再生し直すべきかを、直前フレームの有効状態(`was_enabled`)と
/// 現在の有効状態(`now_enabled`)から判定する。無効→有効に転じた瞬間だけtrueを返す
/// (有効のまま/無効のままでは巻き戻さない)。
fn should_restart_title_bgm(was_enabled: bool, now_enabled: bool) -> bool {
    now_enabled && !was_enabled
}

/// ヘルプ画面のジュークボックスの選択カーソルを`len`個の巡回範囲内で動かす。
/// `forward`がtrueなら次へ、falseなら前へ進み、端では反対の端へ巡回する。
fn cycle_jukebox_selection(selection: usize, len: usize, forward: bool) -> usize {
    if forward {
        (selection + 1) % len
    } else {
        selection.checked_sub(1).unwrap_or(len - 1)
    }
}

/// アプリ全体の画面状態(spec.md 1章)。`Game`は演出・補間用の状態が増え
/// バリアント間のサイズ差が大きくなったため`Box`で包む。
enum Screen {
    Title,
    /// コース選択画面。タイトルでEnterを押した直後に経由し、
    /// ここでEnterを押すと実際にゲームが始まる。
    ModeSelect,
    Settings,
    Help,
    Playing(Box<Game>),
}

/// 一時停止中にオーバーレイ表示する画面。`Screen::Playing`のまま
/// (Gameを手放さず)上に重ねて描画するだけなので、独立した状態として持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PauseOverlay {
    None,
    Settings,
    Help,
}

/// Xブロック/AIR等の出現率設定(%)を1ステップぶん増減し、指定した下限〜
/// `SPAWN_RATE_PERCENT_MAX`にクランプする。スターは上限・刻み幅が異なるため
/// `adjust_star_rate_percent`を別に使う。
fn adjust_rate_percent(current: u32, increase: bool, min: u32) -> u32 {
    if increase {
        current
            .saturating_add(SPAWN_RATE_PERCENT_STEP)
            .min(SPAWN_RATE_PERCENT_MAX)
    } else {
        current.saturating_sub(SPAWN_RATE_PERCENT_STEP).max(min)
    }
}

/// スターブロックの出現率設定(%)を1ステップぶん増減する。他ブロックと共通の
/// `adjust_rate_percent`とは異なる、スター専用の上限(`STAR_SPAWN_RATE_PERCENT_MAX`)・
/// 刻み幅(`STAR_SPAWN_RATE_PERCENT_STEP`)を使う。
fn adjust_star_rate_percent(current: u32, increase: bool) -> u32 {
    if increase {
        current
            .saturating_add(STAR_SPAWN_RATE_PERCENT_STEP)
            .min(STAR_SPAWN_RATE_PERCENT_MAX)
    } else {
        // 現在STAR_SPAWN_RATE_PERCENT_MIN=0のためu32のsaturating_sub結果への
        // .max()は無意味と判定されるが(clippy::unnecessary_min_or_max)、下限を
        // 明示するための記述として意図的に残す(将来0以外に変える場合の安全策)。
        #[allow(clippy::unnecessary_min_or_max)]
        current
            .saturating_sub(STAR_SPAWN_RATE_PERCENT_STEP)
            .max(STAR_SPAWN_RATE_PERCENT_MIN)
    }
}

/// 配分率・色数系の設定項目(岩/AIR/スター/ダイヤ/アイテム3種/色数/色結合率)を1ステップ
/// ぶん調整する。タイトルのSettings画面と一時停止オーバーレイで同じロジックが要るため共通化。
/// `choice`が対象外なら何もせず`false`を返す。盤面への反映・`Settings::save`は呼び出し側の責務。
fn adjust_spawn_rate_setting(
    settings: &mut Settings,
    choice: ui::render::SettingsChoice,
    increase: bool,
) -> bool {
    match choice {
        ui::render::SettingsChoice::RockRate => {
            settings.rock_spawn_rate_percent = adjust_rate_percent(
                settings.rock_spawn_rate_percent,
                increase,
                SPAWN_RATE_PERCENT_MIN,
            );
        }
        ui::render::SettingsChoice::AirRate => {
            settings.air_spawn_rate_percent = adjust_rate_percent(
                settings.air_spawn_rate_percent,
                increase,
                SPAWN_RATE_PERCENT_MIN,
            );
        }
        ui::render::SettingsChoice::StarRate => {
            settings.star_spawn_rate_percent =
                adjust_star_rate_percent(settings.star_spawn_rate_percent, increase);
        }
        ui::render::SettingsChoice::DiamondRate => {
            settings.diamond_spawn_rate_percent = adjust_rate_percent(
                settings.diamond_spawn_rate_percent,
                increase,
                DIAMOND_SPAWN_RATE_PERCENT_MIN,
            );
        }
        ui::render::SettingsChoice::ItemClearAboveRate => {
            settings.item_clear_above_rate_percent = adjust_rate_percent(
                settings.item_clear_above_rate_percent,
                increase,
                ITEM_SPAWN_RATE_PERCENT_MIN,
            );
        }
        ui::render::SettingsChoice::ItemUnifyColorsRate => {
            settings.item_unify_colors_rate_percent = adjust_rate_percent(
                settings.item_unify_colors_rate_percent,
                increase,
                ITEM_SPAWN_RATE_PERCENT_MIN,
            );
        }
        ui::render::SettingsChoice::ItemStarifyScreenRate => {
            settings.item_starify_screen_rate_percent = adjust_rate_percent(
                settings.item_starify_screen_rate_percent,
                increase,
                ITEM_SPAWN_RATE_PERCENT_MIN,
            );
        }
        ui::render::SettingsChoice::ColorCount => {
            settings.color_count = adjust_color_count(settings.color_count, increase);
        }
        ui::render::SettingsChoice::ColorClusterRate => {
            settings.color_cluster_rate_percent = adjust_rate_percent(
                settings.color_cluster_rate_percent,
                increase,
                COLOR_CLUSTER_RATE_PERCENT_MIN,
            );
        }
        _ => return false,
    }
    true
}

/// ボム出現頻度設定(%)を1ステップぶん増減する。他の配分率と共通の
/// `adjust_rate_percent`とは異なる、ボム専用の上限(`BOMB_SPAWN_RATE_PERCENT_MAX`)・
/// 刻み幅(`BOMB_SPAWN_RATE_PERCENT_STEP`)を使う。
fn adjust_bomb_rate_percent(current: u32, increase: bool) -> u32 {
    if increase {
        current
            .saturating_add(BOMB_SPAWN_RATE_PERCENT_STEP)
            .min(BOMB_SPAWN_RATE_PERCENT_MAX)
    } else {
        // 現在BOMB_SPAWN_RATE_PERCENT_MIN=0のためu32のsaturating_sub結果への
        // .max()は無意味と判定されるが(clippy::unnecessary_min_or_max)、下限を
        // 明示するための記述として意図的に残す(将来0以外に変える場合の安全策)。
        #[allow(clippy::unnecessary_min_or_max)]
        current
            .saturating_sub(BOMB_SPAWN_RATE_PERCENT_STEP)
            .max(BOMB_SPAWN_RATE_PERCENT_MIN)
    }
}

/// 出現する色ブロックの色数(`COLOR_COUNT_MIN`〜`COLOR_COUNT_MAX`)を1ずつ増減する。
fn adjust_color_count(current: u8, increase: bool) -> u8 {
    if increase {
        current.saturating_add(1).min(COLOR_COUNT_MAX)
    } else {
        current.saturating_sub(1).max(COLOR_COUNT_MIN)
    }
}

/// ブロック落下速度(tick間隔, ms)を`DEBUG_FALL_TICK_STEP_MS`ぶん増減する。
/// `increase`はms値そのものの増減方向(true=ms増加=遅くなる)を表す。
fn adjust_fall_speed_ms(current: u64, increase: bool) -> u64 {
    if increase {
        current
            .saturating_add(DEBUG_FALL_TICK_STEP_MS)
            .min(DEBUG_FALL_TICK_MS_MAX)
    } else {
        current
            .saturating_sub(DEBUG_FALL_TICK_STEP_MS)
            .max(DEBUG_FALL_TICK_MS_MIN)
    }
}

/// 横移動のクールダウン間隔(ms)を`MOVE_COOLDOWN_MS_STEP`ぶん増減する。
/// `increase`はms値そのものの増減方向(true=ms増加=遅くなる)を表す。
fn adjust_move_cooldown_ms(current: u64, increase: bool) -> u64 {
    if increase {
        current
            .saturating_add(MOVE_COOLDOWN_MS_STEP)
            .min(MOVE_COOLDOWN_MS_MAX)
    } else {
        current
            .saturating_sub(MOVE_COOLDOWN_MS_STEP)
            .max(MOVE_COOLDOWN_MS_MIN)
    }
}

/// フィールド幅(列数)を`FIELD_WIDTH_STEP`ぶん増減する。新規ゲーム開始時にのみ反映される。
fn adjust_field_width(current: usize, increase: bool) -> usize {
    if increase {
        current
            .saturating_add(FIELD_WIDTH_STEP)
            .min(FIELD_WIDTH_MAX)
    } else {
        current
            .saturating_sub(FIELD_WIDTH_STEP)
            .max(FIELD_WIDTH_MIN)
    }
}

/// ヒヤリ回避スライダー後の硬直時間(ms)を`DODGE_RECOVERY_MS_STEP`ぶん増減する。
fn adjust_dodge_recovery_ms(current: u64, increase: bool) -> u64 {
    if increase {
        current
            .saturating_add(DODGE_RECOVERY_MS_STEP)
            .min(DODGE_RECOVERY_MS_MAX)
    } else {
        // DODGE_RECOVERY_MS_MINは0固定のため、saturating_subの結果に対する.max()は不要
        // (clippy::unnecessary_min_or_max)。
        current.saturating_sub(DODGE_RECOVERY_MS_STEP)
    }
}

/// 自動消滅の連鎖インターバル(ms)を`CHAIN_VANISH_INTERVAL_MS_STEP`ぶん増減する。
/// 連鎖消滅を0ms連続でなく一定間隔で進めるための設定。
fn adjust_chain_vanish_interval_ms(current: u64, increase: bool) -> u64 {
    if increase {
        current
            .saturating_add(CHAIN_VANISH_INTERVAL_MS_STEP)
            .min(CHAIN_VANISH_INTERVAL_MS_MAX)
    } else {
        // CHAIN_VANISH_INTERVAL_MS_MINは0固定のため、saturating_subの結果に対する.max()は
        // 不要(clippy::unnecessary_min_or_max)。
        current.saturating_sub(CHAIN_VANISH_INTERVAL_MS_STEP)
    }
}

/// SE/MUSIC音量(%)を`SOUND_VOLUME_PERCENT_STEP`ぶん増減する(TERM独自拡張。#224)。
#[allow(clippy::unnecessary_min_or_max)]
fn adjust_sound_volume_percent(current: u32, increase: bool) -> u32 {
    if increase {
        current
            .saturating_add(SOUND_VOLUME_PERCENT_STEP)
            .min(SOUND_VOLUME_PERCENT_MAX)
    } else {
        current
            .saturating_sub(SOUND_VOLUME_PERCENT_STEP)
            .max(SOUND_VOLUME_PERCENT_MIN)
    }
}

/// ゲームイベントを対応する効果音再生へ変換する。SE OFF設定中、またはSE音量が0%の
/// 間は何もしない。
fn handle_events(
    events: &[GameEvent],
    mixer: Option<&Mixer>,
    se_enabled: &Arc<AtomicBool>,
    se_volume_percent: u32,
) {
    if !se_enabled.load(Ordering::Relaxed) || se_volume_percent == 0 {
        return;
    }
    let Some(mixer) = mixer else {
        return;
    };
    let gain = audio::sfx::se_gain(se_volume_percent);

    for event in events {
        match event {
            GameEvent::DrillImpact => audio::sfx::play_dig(mixer, gain),
            GameEvent::RockHitIntact => audio::sfx::play_rock_hit(mixer, gain),
            GameEvent::BlockDestroyed { blocks } => audio::sfx::play_destroy(mixer, *blocks, gain),
            GameEvent::RockDestroyed { blocks } => {
                audio::sfx::play_rock_destroy(mixer, *blocks, gain)
            }
            GameEvent::DodgeTriggered => audio::sfx::play_dodge(mixer, gain),
            GameEvent::OxygenCollected => audio::sfx::play_oxygen_pickup(mixer, gain),
            // ダイヤ取得の専用SEはspec.md 10章のSE一覧に定義が無いため無音(得点加算のみ)。
            GameEvent::DiamondCollected => {}
            GameEvent::OxygenWarningTick => audio::sfx::play_oxygen_warning(mixer, gain),
            GameEvent::LevelUp { .. } => audio::sfx::play_level_up(mixer, gain),
            GameEvent::ExtraLifeAtLevel { .. } => audio::sfx::play_extra_life(mixer, gain),
            // 死因(cause)はソークテストの集計専用で、SE再生では区別しない。
            GameEvent::LifeLost { .. } => audio::sfx::play_life_lost(mixer, gain),
            GameEvent::Revived => audio::sfx::play_revive(mixer, gain),
            GameEvent::GameOverMiss { .. } => audio::sfx::play_miss(mixer, gain),
            GameEvent::Cleared => audio::sfx::play_clear_fanfare(mixer, gain),
            GameEvent::ItemCollected(_) => audio::sfx::play_item_collected(mixer, gain),
            GameEvent::BombExploded => audio::sfx::play_bomb_explosion(mixer, gain),
            GameEvent::BombFuseWarning => audio::sfx::play_bomb_fuse_warning(mixer, gain),
            GameEvent::BombFuseTick => audio::sfx::play_bomb_fuse_tick(mixer, gain),
            // 100mごとのチェックポイント到達。最終ゴール(Cleared)と同じファンファーレを使い回す。
            GameEvent::Checkpoint100m { .. } => audio::sfx::play_clear_fanfare(mixer, gain),
            // 無敵によるミス回避(TERM独自拡張。#218)。デバッグ用の記録専用イベントで、
            // 演出もSEも伴わない(回数はHUDのGOD表示とデバッグログに残る)。
            GameEvent::MissAverted { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_title_bgm_enabled_is_true_only_on_title() {
        // タイトル画面にいる間だけタイトル用BGMを鳴らす。
        assert!(effective_title_bgm_enabled(true, &Screen::Title));
        assert!(!effective_title_bgm_enabled(false, &Screen::Title));
        assert!(!effective_title_bgm_enabled(true, &Screen::Settings));
        assert!(!effective_title_bgm_enabled(true, &Screen::Help));

        let game = Game::new(1);
        assert!(!effective_title_bgm_enabled(
            true,
            &Screen::Playing(Box::new(game))
        ));
    }

    #[test]
    fn effective_gameplay_bgm_enabled_is_always_false_on_title_regardless_of_setting() {
        // タイトル画面は専用曲(タイトルBGM)の担当のため、プレイ中BGM側は常に鳴らないはず。
        assert!(!effective_gameplay_bgm_enabled(true, &Screen::Title));
        assert!(!effective_gameplay_bgm_enabled(false, &Screen::Title));
    }

    #[test]
    fn mode_select_screen_keeps_the_title_bgm_playing_and_never_the_gameplay_bgm() {
        // モードセレクト画面はタイトルから直接つながる短い経由画面のため、
        // タイトルBGMを途切れさせずそのまま鳴らし続ける。
        assert!(effective_title_bgm_enabled(true, &Screen::ModeSelect));
        assert!(!effective_title_bgm_enabled(false, &Screen::ModeSelect));
        assert!(!effective_gameplay_bgm_enabled(true, &Screen::ModeSelect));
        assert!(!effective_gameplay_bgm_enabled(false, &Screen::ModeSelect));
    }

    #[test]
    fn effective_gameplay_bgm_enabled_follows_the_setting_on_settings_screen() {
        assert!(effective_gameplay_bgm_enabled(true, &Screen::Settings));
        assert!(!effective_gameplay_bgm_enabled(false, &Screen::Settings));
    }

    #[test]
    fn effective_gameplay_bgm_enabled_is_always_false_on_the_standalone_help_screen() {
        // ヘルプ画面(タイトルから開く独立画面)はジュークボックスの置き場のため、
        // 自動でプレイ中BGMを流し続けると試聴と二重に聞こえてしまう。常に無音にする。
        assert!(!effective_gameplay_bgm_enabled(true, &Screen::Help));
        assert!(!effective_gameplay_bgm_enabled(false, &Screen::Help));
    }

    #[test]
    fn effective_gameplay_bgm_enabled_is_true_while_playing_or_paused() {
        let game = Game::new(1);
        assert!(effective_gameplay_bgm_enabled(
            true,
            &Screen::Playing(Box::new(game))
        ));

        let mut game = Game::new(1);
        game.status = GameStatus::Paused;
        assert!(effective_gameplay_bgm_enabled(
            true,
            &Screen::Playing(Box::new(game))
        ));
    }

    #[test]
    fn effective_gameplay_bgm_enabled_is_false_on_game_over() {
        // ゲームオーバー時は短いミス音の後、BGMを停止する。
        let mut game = Game::new(1);
        game.status = GameStatus::GameOver;
        assert!(!effective_gameplay_bgm_enabled(
            true,
            &Screen::Playing(Box::new(game))
        ));
    }

    #[test]
    fn effective_gameplay_bgm_enabled_is_false_on_cleared() {
        // クリア時はBGMを止め、ファンファーレはSEとして別途再生する。
        let mut game = Game::new(1);
        game.status = GameStatus::Cleared;
        assert!(!effective_gameplay_bgm_enabled(
            true,
            &Screen::Playing(Box::new(game))
        ));
    }

    #[test]
    fn title_and_gameplay_bgm_are_never_both_enabled_at_once() {
        // BGMは2系統(タイトル用・プレイ中用)あり、同時に両方鳴ると不自然なので、
        // どの画面状態でも排他的であることを確認する。
        let labeled_screens: Vec<(&str, Screen)> = vec![
            ("Title", Screen::Title),
            ("Settings", Screen::Settings),
            ("Help", Screen::Help),
            ("Playing", Screen::Playing(Box::new(Game::new(1)))),
        ];
        for (label, screen) in &labeled_screens {
            let title_on = effective_title_bgm_enabled(true, screen);
            let gameplay_on = effective_gameplay_bgm_enabled(true, screen);
            assert!(
                !(title_on && gameplay_on),
                "{label}でタイトル用・プレイ中用の両方が有効になっている"
            );
        }
    }

    #[test]
    fn should_restart_title_bgm_only_on_the_disabled_to_enabled_transition() {
        // 無効→有効に転じた瞬間だけ巻き戻すべきで、有効のまま/無効のままでは巻き戻さない。
        assert!(
            should_restart_title_bgm(false, true),
            "無効→有効の遷移では巻き戻すはず"
        );
        assert!(
            !should_restart_title_bgm(true, true),
            "有効のままなら巻き戻さないはず"
        );
        assert!(
            !should_restart_title_bgm(false, false),
            "無効のままなら巻き戻さないはず"
        );
        assert!(
            !should_restart_title_bgm(true, false),
            "有効→無効の遷移では巻き戻さないはず"
        );
    }

    #[test]
    fn cycle_jukebox_selection_wraps_around_at_both_ends() {
        // ↑/↓での選択移動が両端で正しく巡回することを確認する。
        assert_eq!(cycle_jukebox_selection(0, 4, true), 1);
        assert_eq!(cycle_jukebox_selection(3, 4, true), 0, "末尾の次は先頭へ");
        assert_eq!(cycle_jukebox_selection(2, 4, false), 1);
        assert_eq!(cycle_jukebox_selection(0, 4, false), 3, "先頭の前は末尾へ");
    }

    #[test]
    fn adjust_rate_percent_saturates_at_max_instead_of_panicking_when_current_is_corrupted() {
        // 破損したsettings.jsonでcurrentがu32::MAX付近になっていても、raw加算での
        // オーバーフローpanicはせず、上限へ飽和するだけのはず。
        assert_eq!(
            adjust_rate_percent(u32::MAX, true, SPAWN_RATE_PERCENT_MIN),
            SPAWN_RATE_PERCENT_MAX
        );
    }

    #[test]
    fn adjust_star_rate_percent_can_reach_the_higher_star_specific_max() {
        // 他ブロックと共通のSPAWN_RATE_PERCENT_MAX(300%)より大きい、
        // スター専用の上限まで増やせるはず。
        assert_eq!(
            adjust_star_rate_percent(u32::MAX, true),
            STAR_SPAWN_RATE_PERCENT_MAX,
            "破損データでのオーバーフローpanicもせず上限へ飽和するはず"
        );
        assert_eq!(
            adjust_star_rate_percent(0, false),
            STAR_SPAWN_RATE_PERCENT_MIN
        );
    }

    #[test]
    fn adjust_bomb_rate_percent_can_reach_the_higher_bomb_specific_max() {
        // 他ブロックと共通のSPAWN_RATE_PERCENT_MAX(300%)より大きい、
        // ボム専用の上限まで増やせるはず。
        assert_eq!(
            adjust_bomb_rate_percent(u32::MAX, true),
            BOMB_SPAWN_RATE_PERCENT_MAX,
            "破損データでのオーバーフローpanicもせず上限へ飽和するはず"
        );
        assert_eq!(
            adjust_bomb_rate_percent(0, false),
            BOMB_SPAWN_RATE_PERCENT_MIN
        );
    }

    #[test]
    fn adjust_color_count_saturates_at_max_instead_of_panicking_when_current_is_corrupted() {
        assert_eq!(adjust_color_count(u8::MAX, true), COLOR_COUNT_MAX);
    }

    #[test]
    fn adjust_fall_speed_ms_saturates_at_max_instead_of_panicking_when_current_is_corrupted() {
        assert_eq!(adjust_fall_speed_ms(u64::MAX, true), DEBUG_FALL_TICK_MS_MAX);
    }

    #[test]
    fn adjust_move_cooldown_ms_saturates_at_max_instead_of_panicking_when_current_is_corrupted() {
        assert_eq!(
            adjust_move_cooldown_ms(u64::MAX, true),
            MOVE_COOLDOWN_MS_MAX
        );
    }

    #[test]
    fn adjust_field_width_saturates_at_max_instead_of_panicking_when_current_is_corrupted() {
        assert_eq!(adjust_field_width(usize::MAX, true), FIELD_WIDTH_MAX);
    }

    #[test]
    fn adjust_dodge_recovery_ms_saturates_at_max_instead_of_panicking_when_current_is_corrupted() {
        assert_eq!(
            adjust_dodge_recovery_ms(u64::MAX, true),
            DODGE_RECOVERY_MS_MAX
        );
    }

    #[test]
    fn adjust_sound_volume_percent_saturates_at_max_instead_of_panicking_when_current_is_corrupted()
    {
        assert_eq!(
            adjust_sound_volume_percent(u32::MAX, true),
            SOUND_VOLUME_PERCENT_MAX
        );
    }

    #[test]
    fn adjust_sound_volume_percent_saturates_at_min() {
        assert_eq!(
            adjust_sound_volume_percent(0, false),
            SOUND_VOLUME_PERCENT_MIN
        );
    }

    #[test]
    fn adjust_sound_volume_percent_decreases_by_one_step() {
        assert_eq!(adjust_sound_volume_percent(100, false), 90);
    }

    #[test]
    fn adjust_sound_volume_percent_increases_by_one_step() {
        assert_eq!(adjust_sound_volume_percent(90, true), 100);
    }
}

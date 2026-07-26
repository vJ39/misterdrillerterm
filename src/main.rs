//! ミスドリTERM: メインループ(spec.md 9章)。
//! Phase1(ノーマルコース シングルプレイ)のみを実装する。

mod audio;
mod constants;
mod debug_log;
mod game;
mod input;
mod settings;
mod ui;

use std::io;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use rand::RngExt;
use rodio::mixer::Mixer;

use constants::{
    BOMB_SPAWN_RATE_PERCENT_MIN, COLOR_CLUSTER_RATE_PERCENT_MIN, COLOR_COUNT_MAX, COLOR_COUNT_MIN,
    DEBUG_FALL_TICK_MS_MAX, DEBUG_FALL_TICK_MS_MIN, DEBUG_FALL_TICK_STEP_MS,
    DIAMOND_SPAWN_RATE_PERCENT_MIN, DODGE_RECOVERY_MS_MAX, DODGE_RECOVERY_MS_STEP, FIELD_WIDTH_MAX,
    FIELD_WIDTH_MIN, FIELD_WIDTH_STEP, ITEM_SPAWN_RATE_PERCENT_MIN, MOVE_COOLDOWN_MS_MAX,
    MOVE_COOLDOWN_MS_MIN, MOVE_COOLDOWN_MS_STEP, SPAWN_RATE_PERCENT_MAX, SPAWN_RATE_PERCENT_MIN,
    SPAWN_RATE_PERCENT_STEP, SPAWN_RATE_REROLL_SAFE_MARGIN_ROWS, STAR_SPAWN_RATE_PERCENT_MIN,
};
use game::{Game, GameEvent, GameOverChoice, GameStatus, InputAction};
use settings::Settings;

/// メインループの目安フレーム間隔(spec.md 9章 ポーリング間隔目安16〜33ms=30〜60fps相当)。
const FRAME_INTERVAL_MS: u64 = 33;

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();

    // Kittyキーボードプロトコル(対応ターミナルのみ)を有効化する(TERM独自拡張。
    // ユーザー指摘: 「z/xとカーソルキー同時押しできるようにして」)。レガシーの
    // ANSIエスケープシーケンスでは、矢印キー(複数バイトのエスケープシーケンス)と
    // 単純な1文字キーがほぼ同時に押された場合、ターミナル側の生バイト列の解釈が
    // 曖昧になり得るため、対応ターミナル(kitty/WezTerm/Alacritty/foot等)では
    // DISAMBIGUATE_ESCAPE_CODESで曖昧さを解消する。非対応ターミナルでは何もしない
    // (`supports_keyboard_enhancement`がfalse/Errの場合は従来通り)。
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

    // MUSIC/SE個別ON/OFF設定(TERM独自拡張、spec.md 10章)。前回終了時の状態を復元し、
    // BGMスレッド・SE再生の双方から参照できるよう`Arc<AtomicBool>`で共有する。
    let mut settings = Settings::load();
    // タイトル画面用・プレイ中用でBGMを別トラックにする(TERM独自拡張。#145/#146。
    // ユーザー指摘: 「タイトル画面は、これで!」「プレイ中はこの２つを交互に鳴らす
    // ことにする」)。同時に両方鳴らないよう、`effective_title_bgm_enabled`/
    // `effective_gameplay_bgm_enabled`は排他的になるよう設計している。起動直後は
    // タイトル画面から始まる。
    let title_music_enabled = Arc::new(AtomicBool::new(effective_title_bgm_enabled(
        settings.music_enabled,
        &Screen::Title,
    )));
    let gameplay_music_enabled = Arc::new(AtomicBool::new(effective_gameplay_bgm_enabled(
        settings.music_enabled,
        &Screen::Title,
    )));
    let se_enabled = Arc::new(AtomicBool::new(settings.se_enabled));
    // タイトル画面へ戻るたびにタイトルBGMを先頭から再生し直すためのフラグ
    // (TERM独自拡張。#150。ユーザー指摘: 「タイトルに戻ったら最初から再生ね」)。
    // 起動直後の初回表示は「戻ってきた」わけではないので、ここではまだ立てない。
    let title_bgm_restart = Arc::new(AtomicBool::new(false));
    let mut was_title_bgm_enabled = title_music_enabled.load(Ordering::Relaxed);

    let bgm_stop = Arc::new(AtomicBool::new(false));
    if let Some(m) = &mixer {
        audio::bgm::spawn_title_bgm_thread(
            m.clone(),
            Arc::clone(&bgm_stop),
            Arc::clone(&title_music_enabled),
            Arc::clone(&title_bgm_restart),
        );
        audio::bgm::spawn_gameplay_bgm_thread(
            m.clone(),
            Arc::clone(&bgm_stop),
            Arc::clone(&gameplay_music_enabled),
        );
    }

    // 通常プレイはOS乱数から生成したシードを使う(spec.md 3章)。
    let mut rng = rand::rng();

    // アプリの画面状態(spec.md 1章末尾「Qキーはタイトルへ戻る」)。タイトル画面自体で
    // Qが押された場合のみアプリを終了する。ゲームプレイ中・ポーズ中・ゲームオーバー・
    // クリア画面でのQは、Gameを作り直してタイトルへ戻す(酸素・スコア・深度等が
    // 全てリセットされる)。
    let mut screen = Screen::Title;
    let mut last_tick = Instant::now();
    // 設定画面(TERM独自拡張)での現在の選択項目。
    let mut settings_selection = ui::render::SettingsChoice::Music;
    // 一時停止中にオーバーレイ表示する設定/ヘルプ画面(TERM独自拡張。ユーザー指摘:
    // 「一時停止中にもヘルプページを開けるようにする」「プレイ中に設定画面を呼び出せる
    // ようにし、ファイルに保存されている設定をいじれるものとする」)。Gameを作り直さず
    // Screen::Playingのまま上に重ねて描画するだけなので、画面遷移ではなくこのローカルな
    // 状態フラグで管理する。
    let mut pause_overlay = PauseOverlay::None;

    // ヘルプ画面(タイトルから開く独立画面)のジュークボックス状態(TERM独自拡張。
    // #151。ユーザー指摘: 「ヘルプページミュージック選んで再生する機能ほしい」)。
    // カーソル位置は画面を離れても保持する。再生中の曲は、その曲の再生を
    // 制御するハンドル(stop/finishedフラグ)とセットで持つ。
    let mut help_jukebox_selection: usize = 0;
    let mut help_jukebox_playing: Option<(usize, audio::bgm::JukeboxPreview)> = None;

    loop {
        // Playing→Titleへの遷移フラグ。`screen`自体への再代入は、`game`(screenを
        // 借用したバインディング)の生存期間が終わった後、if/else全体を抜けてから
        // 行う(借用中のscreenへ同時に代入できないため)。
        let mut back_to_title = false;

        if let Screen::Playing(game) = &mut screen {
            // poll_input_batch: 1フレーム内にキューされた全キーイベントを処理する
            // (TERM独自拡張)。矢印キー(移動・向き変更)とスペースキー(掘削)を
            // ほぼ同時に押した場合でも、同一フレームに届いた両方のイベントを
            // 取りこぼさず反映できるようにするため。
            for action in input::poll_input_batch(FRAME_INTERVAL_MS)? {
                if back_to_title {
                    break; // Quit済みなら以降のキューされたアクションは処理しない
                }
                match action {
                    // オーバーレイ(設定/ヘルプ)が開いている間のQはタイトルへ戻らず、
                    // オーバーレイを閉じるだけにする(TERM独自拡張)。
                    InputAction::Quit if pause_overlay != PauseOverlay::None => {
                        pause_overlay = PauseOverlay::None;
                    }
                    InputAction::Quit => back_to_title = true,
                    InputAction::TogglePause => {
                        game.toggle_pause();
                        pause_overlay = PauseOverlay::None;
                    }
                    // ユーザー指摘: 「ポーズ解除は、Pだけじゃなく、ショートカット設定
                    // されていない任意のキー入力でも解除されるように」。オーバーレイ
                    // (設定/ヘルプ)表示中は対象外にする(そちらはQ/S/Hで明示的に閉じる)。
                    InputAction::UnboundKey
                        if game.status == GameStatus::Paused
                            && pause_overlay == PauseOverlay::None =>
                    {
                        game.toggle_pause();
                    }
                    InputAction::UnboundKey => {}
                    // M/EキーでのMUSIC/SE切り替えは、一時停止画面でのみ意味を持つ
                    // (spec.md 1章・10章、TERM独自拡張)。プレイ中(Paused以外)は無視する。
                    InputAction::ToggleMusic => {
                        if game.status == GameStatus::Paused {
                            settings.music_enabled = !settings.music_enabled;
                            // ここはScreen::Playing(かつPaused)確定なので、タイトル用
                            // BGMは触れず(既に無音のはず)、プレイ中BGMのみ即時反映する
                            // (TERM独自拡張。#145/#146)。
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
                    // S/Hキーでの設定/ヘルプ画面オーバーレイ表示(TERM独自拡張。ユーザー指摘:
                    // 「一時停止中にもヘルプページを開けるようにする」「プレイ中に設定画面を
                    // 呼び出せるようにし、ファイルに保存されている設定をいじれるものとする」
                    // 「設定(S)はポーズ(P)せずに出せるように」)。プレイ中に押した場合は
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
                            // 配分率・色数・落下速度・回避硬直時間は←→で調整するので、Spaceは無効(トグル対象ではない)。
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
                            | ui::render::SettingsChoice::BombRate => {}
                        }
                    }
                    // MUSIC/SEのトグルは←→キーでも行える(TERM独自拡張。ユーザー指摘:
                    // 「設定画面のMUSIC, SEのトグルをカーソル左右ボタンで切り替えできる
                    // ように」)。トグルなので方向は問わず、押されたら反転する。
                    InputAction::MoveLeft | InputAction::MoveRight
                        if pause_overlay == PauseOverlay::Settings
                            && matches!(
                                settings_selection,
                                ui::render::SettingsChoice::Music | ui::render::SettingsChoice::Se
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
                            _ => {}
                        }
                        settings.save();
                    }
                    // ブロック落下速度・キャラ落下速度・回避硬直時間の調整(TERM独自拡張)。
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
                                settings.bomb_spawn_rate_percent = adjust_rate_percent(
                                    settings.bomb_spawn_rate_percent,
                                    increase,
                                    BOMB_SPAWN_RATE_PERCENT_MIN,
                                );
                                game.set_bomb_spawn_rate_percent(settings.bomb_spawn_rate_percent);
                            }
                            _ => {}
                        }
                        settings.save();
                    }
                    // フィールド幅(列数、TERM独自拡張)の調整。盤面の列数そのものを変えるため
                    // 現在の盤面には反映できず、次回の新規ゲーム開始時にのみ適用される
                    // (ユーザー指摘: 「設定値に列の数を変更できるようにして」)。
                    InputAction::MoveLeft | InputAction::MoveRight
                        if pause_overlay == PauseOverlay::Settings
                            && settings_selection == ui::render::SettingsChoice::FieldWidth =>
                    {
                        let increase = action == InputAction::MoveRight;
                        settings.field_width = adjust_field_width(settings.field_width, increase);
                        settings.save();
                    }
                    // Xブロック/AIR/スター/ダイヤの配分率・色数調整(TERM独自拡張)。プレイ中なので、
                    // 既に画面に見えている範囲は変えず、十分先(画面外)から新しい配分率を反映
                    // する(ユーザー指摘: 「プレイ中でもその数値をいじれるようにしたい」)。
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
                        match settings_selection {
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
                                settings.star_spawn_rate_percent = adjust_rate_percent(
                                    settings.star_spawn_rate_percent,
                                    increase,
                                    STAR_SPAWN_RATE_PERCENT_MIN,
                                );
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
                                settings.color_count =
                                    adjust_color_count(settings.color_count, increase);
                            }
                            ui::render::SettingsChoice::ColorClusterRate => {
                                settings.color_cluster_rate_percent = adjust_rate_percent(
                                    settings.color_cluster_rate_percent,
                                    increase,
                                    COLOR_CLUSTER_RATE_PERCENT_MIN,
                                );
                            }
                            _ => {}
                        }
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
                    // (TERM独自拡張。ユーザー指摘: 「タイトルに戻るか、その場から復活して
                    // 再開するか、ダイアログ表示してカーソルで選べるように」)。
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
                    InputAction::MoveLeft => {
                        let events = game.try_move_left();
                        handle_events(&events, mixer.as_ref(), &se_enabled);
                    }
                    InputAction::MoveRight => {
                        let events = game.try_move_right();
                        handle_events(&events, mixer.as_ref(), &se_enabled);
                    }
                    InputAction::FaceUp => game.face_up(),
                    InputAction::FaceDown => game.face_down(),
                    InputAction::Drill => {
                        let events = game.try_drill();
                        handle_events(&events, mixer.as_ref(), &se_enabled);
                    }
                    InputAction::DebugUnifyNearbyColors => {
                        let events = game.debug_unify_nearby_colors();
                        handle_events(&events, mixer.as_ref(), &se_enabled);
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
                let now = Instant::now();
                let delta = now.duration_since(last_tick);
                last_tick = now;

                let events = game.update(delta.min(Duration::from_millis(250)));
                handle_events(&events, mixer.as_ref(), &se_enabled);

                let music_on = gameplay_music_enabled.load(Ordering::Relaxed);
                let se_on = se_enabled.load(Ordering::Relaxed);
                terminal.draw(|frame| {
                    ui::render::draw(frame, game, music_on, se_on);
                    // 一時停止中の設定/ヘルプオーバーレイ(TERM独自拡張)。Screen::Playingの
                    // ままGameを手放さずに上へ重ね描きするだけで、専用のScreen遷移は行わない。
                    match pause_overlay {
                        PauseOverlay::None => {}
                        PauseOverlay::Settings => ui::render::draw_settings(
                            frame,
                            settings_selection,
                            music_on,
                            se_on,
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
                        ),
                        PauseOverlay::Help => ui::render::draw_help(frame, None),
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
                )
            })?;

            // 設定画面もpoll_input_batchを使う(FaceUp/FaceDown=選択切替、Drill=トグル、
            // MoveLeft/MoveRight=配分率調整、Quit=タイトルへ戻る、を既存のInputActionそのまま
            // 再利用できるため。TERM独自拡張。ユーザー指摘: 「カーソルで選んでスペースで
            // トグル」「設定でXブロックの配分量・AIRの配分量をいじれるようにしたい」)。
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
                        | ui::render::SettingsChoice::BombRate => {}
                    },
                    // MUSIC/SEのトグルは←→キーでも行える(TERM独自拡張。ユーザー指摘:
                    // 「設定画面のMUSIC, SEのトグルをカーソル左右ボタンで切り替えできる
                    // ように」)。トグルなので方向は問わず、押されたら反転する。
                    InputAction::MoveLeft | InputAction::MoveRight
                        if matches!(
                            settings_selection,
                            ui::render::SettingsChoice::Music | ui::render::SettingsChoice::Se
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
                                | ui::render::SettingsChoice::ColorCount
                                | ui::render::SettingsChoice::ColorClusterRate
                                | ui::render::SettingsChoice::FieldWidth
                                | ui::render::SettingsChoice::BlockFallSpeed
                                | ui::render::SettingsChoice::PlayerFallSpeed
                                | ui::render::SettingsChoice::MoveSpeed
                                | ui::render::SettingsChoice::DodgeRecoveryMs
                                | ui::render::SettingsChoice::BombRate
                        ) =>
                    {
                        let increase = action == InputAction::MoveRight;
                        match settings_selection {
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
                                settings.star_spawn_rate_percent = adjust_rate_percent(
                                    settings.star_spawn_rate_percent,
                                    increase,
                                    STAR_SPAWN_RATE_PERCENT_MIN,
                                );
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
                                settings.color_count =
                                    adjust_color_count(settings.color_count, increase);
                            }
                            ui::render::SettingsChoice::ColorClusterRate => {
                                settings.color_cluster_rate_percent = adjust_rate_percent(
                                    settings.color_cluster_rate_percent,
                                    increase,
                                    COLOR_CLUSTER_RATE_PERCENT_MIN,
                                );
                            }
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
                                settings.bomb_spawn_rate_percent = adjust_rate_percent(
                                    settings.bomb_spawn_rate_percent,
                                    increase,
                                    BOMB_SPAWN_RATE_PERCENT_MIN,
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
            // 曲が最後まで自然に終わっていたら、再生中表示を消す(TERM独自拡張。#151)。
            if help_jukebox_playing
                .as_ref()
                .is_some_and(|(_, preview)| preview.finished.load(Ordering::Relaxed))
            {
                help_jukebox_playing = None;
            }

            let jukebox_state = ui::render::HelpJukeboxState {
                selection: help_jukebox_selection,
                playing: help_jukebox_playing.as_ref().map(|(idx, _)| *idx),
            };
            terminal.draw(|frame| ui::render::draw_help(frame, Some(&jukebox_state)))?;

            // ヘルプ画面はQキーでタイトルへ戻る(TERM独自拡張。ユーザー指摘:
            // 「ショートカットのヘルプページも必要」)。↑/↓で曲を選び、X/Zで
            // 再生・停止するジュークボックス操作を追加した(#151)。
            for action in input::poll_input_batch(FRAME_INTERVAL_MS)? {
                match action {
                    InputAction::Quit => {
                        if let Some((_, preview)) = help_jukebox_playing.take() {
                            preview.stop.store(true, Ordering::Relaxed);
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
                                preview.stop.store(true, Ordering::Relaxed);
                            }
                            if !already_playing_selection {
                                let (_, track) = audio::bgm::JUKEBOX_TRACKS[help_jukebox_selection];
                                let preview =
                                    audio::bgm::spawn_jukebox_preview_thread(m.clone(), track);
                                help_jukebox_playing = Some((help_jukebox_selection, preview));
                            }
                        }
                    }
                    _ => {}
                }
            }
        } else {
            terminal.draw(ui::render::draw_title)?;

            if let Some(action) = input::poll_any_key(FRAME_INTERVAL_MS)? {
                match action {
                    input::AnyKeyAction::Quit => break,
                    input::AnyKeyAction::OpenSettings => screen = Screen::Settings,
                    input::AnyKeyAction::OpenHelp => screen = Screen::Help,
                    input::AnyKeyAction::Advance => {
                        let seed: u64 = rng.random();
                        // フィールド幅(列数)設定は新規ゲーム開始時にのみ反映される(TERM独自
                        // 拡張。ユーザー指摘: 「設定値に列の数を変更できるようにして」)。
                        let mut game = Game::new_with_width(seed, settings.field_width);
                        // #85調査用のブロック状態遷移ログをタイトルからのゲーム開始時に
                        // 毎回作り直す(TERM独自拡張。ユーザー指摘: 「タイトルからゲーム
                        // スタートした時点でログdbは毎回リフレッシュするものとする」)。
                        game.refresh_debug_log();
                        // 速度系デバッグショートカットの調整値は設定ファイルに永続化されており
                        // (settings.rs)、新しいゲーム開始時にも引き継ぐ(TERM独自拡張)。
                        game.set_block_fall_tick_ms(settings.block_fall_tick_ms);
                        game.set_player_fall_tick_ms(settings.player_fall_tick_ms);
                        game.set_shake_duration_ms(settings.shake_duration_ms);
                        game.set_dodge_recovery_ms(settings.dodge_recovery_ms);
                        game.set_move_cooldown_ms(settings.move_cooldown_ms);
                        game.set_bomb_spawn_rate_percent(settings.bomb_spawn_rate_percent);
                        // Xブロック/AIR/スター/ダイヤの配分率設定も、新規ゲーム開始時に
                        // 安全地帯明け(行2)以降の全体へ反映する(TERM独自拡張)。
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
                        screen = Screen::Playing(Box::new(game));
                        last_tick = Instant::now();
                    }
                }
            }
        }

        if back_to_title {
            screen = Screen::Title;
            pause_overlay = PauseOverlay::None;
        }

        // 画面遷移(タイトルへ戻る/タイトルから抜ける)を反映して、BGMスレッドが
        // 参照する実効MUSIC状態を毎フレーム同期する(TERM独自拡張。ユーザー指摘:
        // 「タイトル画面ではMUSIC無し」→のちに#146で「タイトル画面は専用曲を鳴らす」
        // へ変更)。タイトル用・プレイ中用のいずれか一方だけがtrueになる。
        let title_bgm_now_enabled = effective_title_bgm_enabled(settings.music_enabled, &screen);
        title_music_enabled.store(title_bgm_now_enabled, Ordering::Relaxed);
        gameplay_music_enabled.store(
            effective_gameplay_bgm_enabled(settings.music_enabled, &screen),
            Ordering::Relaxed,
        );
        // タイトル画面へ戻ってきた(無効→有効に転じた)瞬間に、タイトルBGMを
        // 先頭から再生し直す(TERM独自拡張。#150。ユーザー指摘: 「タイトルに戻ったら
        // 最初から再生ね」)。
        if should_restart_title_bgm(was_title_bgm_enabled, title_bgm_now_enabled) {
            title_bgm_restart.store(true, Ordering::Relaxed);
        }
        was_title_bgm_enabled = title_bgm_now_enabled;
    }

    bgm_stop.store(true, Ordering::Relaxed);

    Ok(())
}

/// MUSIC設定・現在の画面から、実際にタイトル画面用BGMを鳴らすべきかを判定する
/// (TERM独自拡張。#146。ユーザー指摘: 「タイトル画面は、これで!」)。タイトル画面に
/// いる間だけ鳴らす。
fn effective_title_bgm_enabled(settings_music_enabled: bool, screen: &Screen) -> bool {
    settings_music_enabled && matches!(screen, Screen::Title)
}

/// MUSIC設定・現在の画面から、実際にプレイ中BGM(交代制プレイリスト、#145)を
/// 鳴らすべきかを判定する(TERM独自拡張。ユーザー指摘: 「ゲームオーバーになったら、
/// ゲームオーバーの短いミス音の後、MUSIC停止」「ゴールしたらMUSICとめてファンファーレ
/// でしょう」)。タイトル画面・ゲームオーバー中・ゴールクリア後はMUSIC設定のON/OFFに
/// 関わらず常に無音にする(クリアファンファーレはBGMと別にSEとして再生される)。
fn effective_gameplay_bgm_enabled(settings_music_enabled: bool, screen: &Screen) -> bool {
    if !settings_music_enabled {
        return false;
    }
    match screen {
        Screen::Title => false,
        Screen::Playing(game) => matches!(game.status, GameStatus::Playing | GameStatus::Paused),
        Screen::Settings => true,
        // タイトルから開く独立画面としてのヘルプ(Screen::Help)は、#151で曲を選んで
        // 試聴できるジュークボックスの置き場になったため、以前のように自動で
        // プレイ中BGMのローテーションを流し続けると、ジュークボックスの試聴と
        // 二重に聞こえてしまう。そのため常に無音にし、聞こえる音は選んだ曲の
        // プレビューだけにする(TERM独自拡張。ユーザー指摘: 「ヘルプページ
        // ミュージック選んで再生する機能ほしい」)。プレイ中に一時停止して開く
        // ヘルプオーバーレイは`screen`自体は`Screen::Playing`のままなのでこの
        // 分岐には来ず、影響を受けない。
        Screen::Help => false,
    }
}

/// タイトルBGMを先頭から再生し直すべきかを、直前フレームの有効状態
/// (`was_enabled`)と現在の有効状態(`now_enabled`)から判定する(TERM独自拡張。
/// #150。ユーザー指摘: 「タイトルに戻ったら最初から再生ね」)。無効→有効に
/// 転じた瞬間だけtrueを返す(有効のまま/無効のままでは巻き戻さない)。
fn should_restart_title_bgm(was_enabled: bool, now_enabled: bool) -> bool {
    now_enabled && !was_enabled
}

/// ヘルプ画面のジュークボックスの選択カーソルを`len`個の巡回範囲内で動かす
/// (TERM独自拡張。#151)。`forward`がtrueなら次へ、falseなら前へ進み、
/// 端では反対の端へ巡回する。
fn cycle_jukebox_selection(selection: usize, len: usize, forward: bool) -> usize {
    if forward {
        (selection + 1) % len
    } else {
        selection.checked_sub(1).unwrap_or(len - 1)
    }
}

/// アプリ全体の画面状態。タイトル画面・設定画面・プレイ中(Gameを保持)の3値
/// (spec.md 1章、設定画面はTERM独自拡張)。`Game`は演出・補間用の状態が増え
/// バリアント間のサイズ差が大きくなったため`Box`で包む。
enum Screen {
    Title,
    Settings,
    Help,
    Playing(Box<Game>),
}

/// 一時停止中にオーバーレイ表示する画面(TERM独自拡張)。`Screen::Playing`のまま
/// (Gameを手放さず)上に重ねて描画するだけなので、独立した状態として持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PauseOverlay {
    None,
    Settings,
    Help,
}

/// Xブロック/AIR/スターの出現率設定(%)を1ステップぶん増減し、指定した下限
/// (岩/AIRは`SPAWN_RATE_PERCENT_MIN`、スターは0まで下げられる`STAR_SPAWN_RATE_PERCENT_MIN`)
/// 〜`SPAWN_RATE_PERCENT_MAX`にクランプする(TERM独自拡張。ユーザー指摘: 「設定で
/// Xブロックの配分量・AIRの配分量をいじれるようにしたい」「スターブロック比率0〜」)。
fn adjust_rate_percent(current: u32, increase: bool, min: u32) -> u32 {
    if increase {
        current
            .saturating_add(SPAWN_RATE_PERCENT_STEP)
            .min(SPAWN_RATE_PERCENT_MAX)
    } else {
        current.saturating_sub(SPAWN_RATE_PERCENT_STEP).max(min)
    }
}

/// 出現する色ブロックの色数(`COLOR_COUNT_MIN`〜`COLOR_COUNT_MAX`)を1ずつ増減する
/// (TERM独自拡張。ユーザー指摘: 「出現する色ブロックの色数を設定で選べるようにしたい
/// (1〜4)」)。
fn adjust_color_count(current: u8, increase: bool) -> u8 {
    if increase {
        current.saturating_add(1).min(COLOR_COUNT_MAX)
    } else {
        current.saturating_sub(1).max(COLOR_COUNT_MIN)
    }
}

/// ブロック落下速度(tick間隔, ms)を`DEBUG_FALL_TICK_STEP_MS`ぶん増減する
/// (TERM独自拡張。ユーザー指摘: 「ブロックが落ちるスピードの設定値がないよね」)。
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

/// 横移動のクールダウン間隔(ms)を`MOVE_COOLDOWN_MS_STEP`ぶん増減する(TERM独自拡張。
/// ユーザー指摘: 「横移動のスピードを設定で変えられるように」)。`increase`はms値
/// そのものの増減方向(true=ms増加=遅くなる)を表す。
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

/// フィールド幅(列数)を`FIELD_WIDTH_STEP`ぶん増減する(TERM独自拡張。ユーザー指摘:
/// 「設定値に列の数を変更できるようにして」)。新規ゲーム開始時にのみ反映される。
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

/// ヒヤリ回避スライダー後の硬直時間(ms)を`DODGE_RECOVERY_MS_STEP`ぶん増減する
/// (TERM独自拡張。ユーザー指摘: 「スライダー直後その状態で起き上がるまでに1秒
/// インターバル=この設定値も作る」)。
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

/// ゲームイベントを対応する効果音再生へ変換する。SE OFF設定中は何もしない。
fn handle_events(events: &[GameEvent], mixer: Option<&Mixer>, se_enabled: &Arc<AtomicBool>) {
    if !se_enabled.load(Ordering::Relaxed) {
        return;
    }
    let Some(mixer) = mixer else {
        return;
    };

    for event in events {
        match event {
            GameEvent::DrillImpact => audio::sfx::play_dig(mixer),
            GameEvent::RockHitIntact => audio::sfx::play_rock_hit(mixer),
            GameEvent::BlockDestroyed { blocks } => audio::sfx::play_destroy(mixer, *blocks),
            GameEvent::RockDestroyed { blocks } => audio::sfx::play_rock_destroy(mixer, *blocks),
            GameEvent::DodgeTriggered => audio::sfx::play_dodge(mixer),
            GameEvent::OxygenCollected => audio::sfx::play_oxygen_pickup(mixer),
            // ダイヤ取得の専用SEはspec.md 10章のSE一覧に定義が無いため無音(得点加算のみ)。
            GameEvent::DiamondCollected => {}
            GameEvent::OxygenWarningTick => audio::sfx::play_oxygen_warning(mixer),
            GameEvent::LevelUp { .. } => audio::sfx::play_level_up(mixer),
            GameEvent::LifeLost => audio::sfx::play_life_lost(mixer),
            GameEvent::Revived => audio::sfx::play_revive(mixer),
            GameEvent::GameOverMiss => audio::sfx::play_miss(mixer),
            GameEvent::Cleared => audio::sfx::play_clear_fanfare(mixer),
            GameEvent::ItemCollected(_) => audio::sfx::play_item_collected(mixer),
            GameEvent::BombExploded => audio::sfx::play_bomb_explosion(mixer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_title_bgm_enabled_is_true_only_on_title() {
        // ユーザー指摘: 「タイトル画面は、これで!」(#146)。タイトル画面にいる間
        // だけタイトル用BGMを鳴らす。
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
        // ユーザー指摘: 「タイトル画面ではMUSIC無し」(#86。タイトル画面は#146の
        // 専用曲の担当になったため、プレイ中BGM側は常に鳴らないはず)。
        assert!(!effective_gameplay_bgm_enabled(true, &Screen::Title));
        assert!(!effective_gameplay_bgm_enabled(false, &Screen::Title));
    }

    #[test]
    fn effective_gameplay_bgm_enabled_follows_the_setting_on_settings_screen() {
        assert!(effective_gameplay_bgm_enabled(true, &Screen::Settings));
        assert!(!effective_gameplay_bgm_enabled(false, &Screen::Settings));
    }

    #[test]
    fn effective_gameplay_bgm_enabled_is_always_false_on_the_standalone_help_screen() {
        // #151でヘルプ画面(タイトルから開く独立画面)はジュークボックスの
        // 置き場になったため、自動でプレイ中BGMを流し続けると試聴と二重に
        // 聞こえてしまう。常に無音にする。
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
        // ユーザー指摘: 「ゲームオーバーになったら、ゲームオーバーの短いミス音の後、
        // MUSIC停止」。
        let mut game = Game::new(1);
        game.status = GameStatus::GameOver;
        assert!(!effective_gameplay_bgm_enabled(
            true,
            &Screen::Playing(Box::new(game))
        ));
    }

    #[test]
    fn effective_gameplay_bgm_enabled_is_false_on_cleared() {
        // ユーザー指摘: 「ゴールしたらMUSICとめてファンファーレでしょう」。
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
        // #145/#146でBGMを2系統に分けた際、同時に両方鳴ってしまうと不自然なので、
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
        // ユーザー指摘: 「タイトルに戻ったら最初から再生ね」(#150)。無効→有効に
        // 転じた瞬間だけ巻き戻すべきで、有効のまま/無効のままでは巻き戻さない。
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
        // ユーザー指摘: 「ヘルプページミュージック選んで再生する機能ほしい」(#151)。
        // ↑/↓での選択移動が両端で正しく巡回することを確認する。
        assert_eq!(cycle_jukebox_selection(0, 4, true), 1);
        assert_eq!(cycle_jukebox_selection(3, 4, true), 0, "末尾の次は先頭へ");
        assert_eq!(cycle_jukebox_selection(2, 4, false), 1);
        assert_eq!(cycle_jukebox_selection(0, 4, false), 3, "先頭の前は末尾へ");
    }

    #[test]
    fn adjust_rate_percent_saturates_at_max_instead_of_panicking_when_current_is_corrupted() {
        // 破損したsettings.jsonでcurrentがu32::MAX付近になっていても、raw加算での
        // オーバーフローpanicはせず、上限へ飽和するだけのはず(TERM独自拡張。#153)。
        assert_eq!(
            adjust_rate_percent(u32::MAX, true, SPAWN_RATE_PERCENT_MIN),
            SPAWN_RATE_PERCENT_MAX
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
}

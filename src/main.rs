//! ミスタードリラーTERM: メインループ(spec.md 9章)。
//! Phase1(ノーマルコース シングルプレイ)のみを実装する。

mod audio;
mod constants;
mod game;
mod input;
mod settings;
mod ui;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::RngExt;
use rodio::mixer::Mixer;

use constants::{
    DIAMOND_SPAWN_RATE_PERCENT_MIN, SPAWN_RATE_PERCENT_MAX, SPAWN_RATE_PERCENT_MIN, SPAWN_RATE_PERCENT_STEP,
    SPAWN_RATE_REROLL_SAFE_MARGIN_ROWS, STAR_SPAWN_RATE_PERCENT_MIN,
};
use game::{Game, GameEvent, GameOverChoice, GameStatus, InputAction};
use settings::Settings;

/// メインループの目安フレーム間隔(spec.md 9章 ポーリング間隔目安16〜33ms=30〜60fps相当)。
const FRAME_INTERVAL_MS: u64 = 33;

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal);
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal) -> io::Result<()> {
    if !show_intro(terminal)? {
        return Ok(()); // スプラッシュ画面でQキーが押されたので、そのままアプリを終了する。
    }

    // 音声出力デバイスを開く。ヘッドレス環境等でデバイスが無い場合でも
    // ゲーム自体はプレイ続行できるよう、失敗時はNoneにして以後の再生をスキップする。
    let sink_handle = rodio::DeviceSinkBuilder::open_default_sink().ok();
    let mixer: Option<Mixer> = sink_handle.as_ref().map(|handle| handle.mixer().clone());

    // MUSIC/SE個別ON/OFF設定(TERM独自拡張、spec.md 10章)。前回終了時の状態を復元し、
    // BGMスレッド・SE再生の双方から参照できるよう`Arc<AtomicBool>`で共有する。
    let mut settings = Settings::load();
    let music_enabled = Arc::new(AtomicBool::new(settings.music_enabled));
    let se_enabled = Arc::new(AtomicBool::new(settings.se_enabled));

    let bgm_stop = Arc::new(AtomicBool::new(false));
    if let Some(m) = &mixer {
        audio::bgm::spawn_bgm_thread(m.clone(), Arc::clone(&bgm_stop), Arc::clone(&music_enabled));
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
                    // M/EキーでのMUSIC/SE切り替えは、一時停止画面でのみ意味を持つ
                    // (spec.md 1章・10章、TERM独自拡張)。プレイ中(Paused以外)は無視する。
                    InputAction::ToggleMusic => {
                        if game.status == GameStatus::Paused {
                            settings.music_enabled = !settings.music_enabled;
                            music_enabled.store(settings.music_enabled, Ordering::Relaxed);
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
                                music_enabled.store(settings.music_enabled, Ordering::Relaxed);
                                settings.save();
                            }
                            ui::render::SettingsChoice::Se => {
                                settings.se_enabled = !settings.se_enabled;
                                se_enabled.store(settings.se_enabled, Ordering::Relaxed);
                                settings.save();
                            }
                            // 配分率は←→で調整するので、Spaceは無効(トグル対象ではない)。
                            ui::render::SettingsChoice::RockRate
                            | ui::render::SettingsChoice::AirRate
                            | ui::render::SettingsChoice::StarRate
                            | ui::render::SettingsChoice::DiamondRate => {}
                        }
                    }
                    // Xブロック/AIR/スター/ダイヤの配分率調整(TERM独自拡張)。プレイ中なので、
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
                            ) =>
                    {
                        let increase = action == InputAction::MoveRight;
                        match settings_selection {
                            ui::render::SettingsChoice::RockRate => {
                                settings.rock_spawn_rate_percent =
                                    adjust_rate_percent(settings.rock_spawn_rate_percent, increase, SPAWN_RATE_PERCENT_MIN);
                            }
                            ui::render::SettingsChoice::AirRate => {
                                settings.air_spawn_rate_percent =
                                    adjust_rate_percent(settings.air_spawn_rate_percent, increase, SPAWN_RATE_PERCENT_MIN);
                            }
                            ui::render::SettingsChoice::StarRate => {
                                settings.star_spawn_rate_percent =
                                    adjust_rate_percent(settings.star_spawn_rate_percent, increase, STAR_SPAWN_RATE_PERCENT_MIN);
                            }
                            ui::render::SettingsChoice::DiamondRate => {
                                settings.diamond_spawn_rate_percent = adjust_rate_percent(
                                    settings.diamond_spawn_rate_percent,
                                    increase,
                                    DIAMOND_SPAWN_RATE_PERCENT_MIN,
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
                        );
                    }
                    // GameOverダイアログ中は上下キー/Spaceを選択操作として扱う
                    // (TERM独自拡張。ユーザー指摘: 「タイトルに戻るか、その場から復活して
                    // 再開するか、ダイアログ表示してカーソルで選べるように」)。
                    InputAction::FaceUp | InputAction::FaceDown if game.status == GameStatus::GameOver => {
                        game.toggle_game_over_selection();
                    }
                    InputAction::Drill if game.status == GameStatus::GameOver => match game.game_over_selection() {
                        GameOverChoice::BackToTitle => back_to_title = true,
                        GameOverChoice::Revive => game.revive(),
                    },
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
                    InputAction::DebugClearAbovePlayer => game.debug_clear_above_player(),
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

                let music_on = music_enabled.load(Ordering::Relaxed);
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
                        ),
                        PauseOverlay::Help => ui::render::draw_help(frame),
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
                    InputAction::Drill => {
                        match settings_selection {
                            ui::render::SettingsChoice::Music => {
                                settings.music_enabled = !settings.music_enabled;
                                music_enabled.store(settings.music_enabled, Ordering::Relaxed);
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
                            | ui::render::SettingsChoice::DiamondRate => {}
                        }
                    }
                    InputAction::MoveLeft | InputAction::MoveRight
                        if matches!(
                            settings_selection,
                            ui::render::SettingsChoice::RockRate
                                | ui::render::SettingsChoice::AirRate
                                | ui::render::SettingsChoice::StarRate
                                | ui::render::SettingsChoice::DiamondRate
                        ) =>
                    {
                        let increase = action == InputAction::MoveRight;
                        match settings_selection {
                            ui::render::SettingsChoice::RockRate => {
                                settings.rock_spawn_rate_percent =
                                    adjust_rate_percent(settings.rock_spawn_rate_percent, increase, SPAWN_RATE_PERCENT_MIN);
                            }
                            ui::render::SettingsChoice::AirRate => {
                                settings.air_spawn_rate_percent =
                                    adjust_rate_percent(settings.air_spawn_rate_percent, increase, SPAWN_RATE_PERCENT_MIN);
                            }
                            ui::render::SettingsChoice::StarRate => {
                                settings.star_spawn_rate_percent =
                                    adjust_rate_percent(settings.star_spawn_rate_percent, increase, STAR_SPAWN_RATE_PERCENT_MIN);
                            }
                            ui::render::SettingsChoice::DiamondRate => {
                                settings.diamond_spawn_rate_percent = adjust_rate_percent(
                                    settings.diamond_spawn_rate_percent,
                                    increase,
                                    DIAMOND_SPAWN_RATE_PERCENT_MIN,
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
            terminal.draw(ui::render::draw_help)?;

            // ヘルプ画面はQキーでタイトルへ戻るだけ(TERM独自拡張。ユーザー指摘:
            // 「ショートカットのヘルプページも必要」)。
            if let Some(action) = input::poll_any_key(FRAME_INTERVAL_MS)?
                && matches!(action, input::AnyKeyAction::Quit)
            {
                screen = Screen::Title;
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
                        let mut game = Game::new(seed);
                        // 速度系デバッグショートカットの調整値は設定ファイルに永続化されており
                        // (settings.rs)、新しいゲーム開始時にも引き継ぐ(TERM独自拡張)。
                        game.set_block_fall_tick_ms(settings.block_fall_tick_ms);
                        game.set_player_fall_tick_ms(settings.player_fall_tick_ms);
                        game.set_shake_duration_ms(settings.shake_duration_ms);
                        // Xブロック/AIR/スター/ダイヤの配分率設定も、新規ゲーム開始時に
                        // 安全地帯明け(行2)以降の全体へ反映する(TERM独自拡張)。
                        game.reroll_spawn_rates_from(
                            2,
                            settings.rock_spawn_rate_percent,
                            settings.air_spawn_rate_percent,
                            settings.star_spawn_rate_percent,
                            settings.diamond_spawn_rate_percent,
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
    }

    bgm_stop.store(true, Ordering::Relaxed);

    Ok(())
}

/// 起動時のスプラッシュ画面(AAピクセルアート、spec.md独自拡張)を表示し、
/// 何らかのキー入力があるまでループする。`Ok(true)`なら通常通りタイトル画面へ
/// 続行し、`Ok(false)`ならこの画面でQキーが押されたのでアプリを終了する。
fn show_intro(terminal: &mut ratatui::DefaultTerminal) -> io::Result<bool> {
    use input::AnyKeyAction;

    loop {
        terminal.draw(ui::render::draw_intro)?;

        if let Some(action) = input::poll_any_key(FRAME_INTERVAL_MS)? {
            match action {
                AnyKeyAction::Quit => return Ok(false),
                AnyKeyAction::Advance | AnyKeyAction::OpenSettings | AnyKeyAction::OpenHelp => return Ok(true),
            }
        }
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
        (current + SPAWN_RATE_PERCENT_STEP).min(SPAWN_RATE_PERCENT_MAX)
    } else {
        current.saturating_sub(SPAWN_RATE_PERCENT_STEP).max(min)
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
            GameEvent::BlockDestroyed { .. } => audio::sfx::play_destroy(mixer),
            GameEvent::OxygenCollected => audio::sfx::play_oxygen_pickup(mixer),
            // ダイヤ取得の専用SEはspec.md 10章のSE一覧に定義が無いため無音(得点加算のみ)。
            GameEvent::DiamondCollected => {}
            GameEvent::OxygenWarningTick => audio::sfx::play_oxygen_warning(mixer),
            GameEvent::LevelUp { .. } => audio::sfx::play_level_up(mixer),
            GameEvent::LifeLost => audio::sfx::play_life_lost(mixer),
            GameEvent::GameOverMiss => audio::sfx::play_miss(mixer),
            GameEvent::Cleared => audio::sfx::play_clear_fanfare(mixer),
        }
    }
}

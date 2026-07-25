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
                    InputAction::Quit => back_to_title = true,
                    InputAction::TogglePause => game.toggle_pause(),
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
                    InputAction::DebugUnifyNearbyColors => game.debug_unify_nearby_colors(),
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
                terminal.draw(|frame| ui::render::draw(frame, game, music_on, se_on))?;
            }
        } else if let Screen::Settings = screen {
            let music_on = settings.music_enabled;
            let se_on = settings.se_enabled;
            terminal.draw(|frame| ui::render::draw_settings(frame, settings_selection, music_on, se_on))?;

            // 設定画面もpoll_input_batchを使う(FaceUp/FaceDown=選択切替、Drill=トグル、
            // Quit=タイトルへ戻る、を既存のInputActionそのまま再利用できるため。
            // TERM独自拡張。ユーザー指摘: 「カーソルで選んでスペースでトグル」)。
            for action in input::poll_input_batch(FRAME_INTERVAL_MS)? {
                match action {
                    InputAction::Quit => screen = Screen::Title,
                    InputAction::FaceUp | InputAction::FaceDown => {
                        settings_selection = match settings_selection {
                            ui::render::SettingsChoice::Music => ui::render::SettingsChoice::Se,
                            ui::render::SettingsChoice::Se => ui::render::SettingsChoice::Music,
                        };
                    }
                    InputAction::Drill => {
                        match settings_selection {
                            ui::render::SettingsChoice::Music => {
                                settings.music_enabled = !settings.music_enabled;
                                music_enabled.store(settings.music_enabled, Ordering::Relaxed);
                            }
                            ui::render::SettingsChoice::Se => {
                                settings.se_enabled = !settings.se_enabled;
                                se_enabled.store(settings.se_enabled, Ordering::Relaxed);
                            }
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
                        screen = Screen::Playing(Box::new(game));
                        last_tick = Instant::now();
                    }
                }
            }
        }

        if back_to_title {
            screen = Screen::Title;
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

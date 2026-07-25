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

use game::{Game, GameEvent, GameStatus, InputAction};
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

    // サウンドON/OFF設定(TERM独自拡張、spec.md 10章)。前回終了時の状態を復元し、
    // BGMスレッド・SE再生の双方から参照できるよう`Arc<AtomicBool>`で共有する。
    let mut settings = Settings::load();
    let sound_enabled = Arc::new(AtomicBool::new(settings.sound_enabled));

    let bgm_stop = Arc::new(AtomicBool::new(false));
    if let Some(m) = &mixer {
        audio::bgm::spawn_bgm_thread(m.clone(), Arc::clone(&bgm_stop), Arc::clone(&sound_enabled));
    }

    // 通常プレイはOS乱数から生成したシードを使う(spec.md 3章)。
    let mut rng = rand::rng();

    // アプリの画面状態(spec.md 1章末尾「Qキーはタイトルへ戻る」)。タイトル画面自体で
    // Qが押された場合のみアプリを終了する。ゲームプレイ中・ポーズ中・ゲームオーバー・
    // クリア画面でのQは、Gameを作り直してタイトルへ戻す(酸素・スコア・深度等が
    // 全てリセットされる)。
    let mut screen = Screen::Title;
    let mut last_tick = Instant::now();

    loop {
        // Playing→Titleへの遷移フラグ。`screen`自体への再代入は、`game`(screenを
        // 借用したバインディング)の生存期間が終わった後、if/else全体を抜けてから
        // 行う(借用中のscreenへ同時に代入できないため)。
        let mut back_to_title = false;

        if let Screen::Playing(game) = &mut screen {
            if let Some(action) = input::poll_input(FRAME_INTERVAL_MS)? {
                match action {
                    InputAction::Quit => back_to_title = true,
                    InputAction::TogglePause => game.toggle_pause(),
                    // Sキーでのサウンド切り替えは、タイトル画面・一時停止画面でのみ意味を
                    // 持つ(spec.md 1章・10章)。プレイ中(Paused以外)は無視する。
                    InputAction::ToggleSound => {
                        if game.status == GameStatus::Paused {
                            toggle_sound(&mut settings, &sound_enabled);
                        }
                    }
                    InputAction::MoveLeft => {
                        let events = game.try_move_left();
                        handle_events(&events, mixer.as_ref(), &sound_enabled);
                    }
                    InputAction::MoveRight => {
                        let events = game.try_move_right();
                        handle_events(&events, mixer.as_ref(), &sound_enabled);
                    }
                    InputAction::FaceUp => game.face_up(),
                    InputAction::FaceDown => game.face_down(),
                    InputAction::Drill => {
                        let events = game.try_drill();
                        handle_events(&events, mixer.as_ref(), &sound_enabled);
                    }
                }
            }

            if !back_to_title {
                let now = Instant::now();
                let delta = now.duration_since(last_tick);
                last_tick = now;

                let events = game.update(delta.min(Duration::from_millis(250)));
                handle_events(&events, mixer.as_ref(), &sound_enabled);

                let sound_on = sound_enabled.load(Ordering::Relaxed);
                terminal.draw(|frame| ui::render::draw(frame, game, sound_on))?;
            }
        } else {
            let sound_on = sound_enabled.load(Ordering::Relaxed);
            terminal.draw(|frame| ui::render::draw_title(frame, sound_on))?;

            if let Some(action) = input::poll_any_key(FRAME_INTERVAL_MS)? {
                match action {
                    input::AnyKeyAction::Quit => break,
                    input::AnyKeyAction::ToggleSound => toggle_sound(&mut settings, &sound_enabled),
                    input::AnyKeyAction::Advance => {
                        let seed: u64 = rng.random();
                        screen = Screen::Playing(Box::new(Game::new(seed)));
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
                AnyKeyAction::Advance | AnyKeyAction::ToggleSound => return Ok(true),
            }
        }
    }
}

/// アプリ全体の画面状態。タイトル画面とプレイ中(Gameを保持)の2値(spec.md 1章)。
/// `Game`は演出・補間用の状態が増え両バリアントのサイズ差が大きくなったため`Box`で包む。
enum Screen {
    Title,
    Playing(Box<Game>),
}

/// サウンド設定をトグルし、BGMスレッドと共有するフラグ・設定ファイルの両方へ反映する
/// (spec.md 10章、TERM独自拡張)。
fn toggle_sound(settings: &mut Settings, sound_enabled: &Arc<AtomicBool>) {
    settings.sound_enabled = !settings.sound_enabled;
    sound_enabled.store(settings.sound_enabled, Ordering::Relaxed);
    settings.save();
}

/// ゲームイベントを対応する効果音再生へ変換する。サウンドOFF設定中は何もしない。
fn handle_events(events: &[GameEvent], mixer: Option<&Mixer>, sound_enabled: &Arc<AtomicBool>) {
    if !sound_enabled.load(Ordering::Relaxed) {
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

//! ミスタードリラーTERM: メインループ(spec.md 9章)。
//! Phase1(ノーマルコース シングルプレイ)のみを実装する。

mod audio;
mod constants;
mod game;
mod input;
mod ui;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::RngExt;
use rodio::mixer::Mixer;

use game::{Game, GameEvent};
use input::InputAction;

/// メインループの目安フレーム間隔(spec.md 9章 ポーリング間隔目安16〜33ms=30〜60fps相当)。
const FRAME_INTERVAL_MS: u64 = 33;

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal);
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal) -> io::Result<()> {
    // 音声出力デバイスを開く。ヘッドレス環境等でデバイスが無い場合でも
    // ゲーム自体はプレイ続行できるよう、失敗時はNoneにして以後の再生をスキップする。
    let sink_handle = rodio::DeviceSinkBuilder::open_default_sink().ok();
    let mixer: Option<Mixer> = sink_handle.as_ref().map(|handle| handle.mixer().clone());

    let bgm_stop = Arc::new(AtomicBool::new(false));
    if let Some(m) = &mixer {
        audio::bgm::spawn_bgm_thread(m.clone(), Arc::clone(&bgm_stop));
    }

    // 通常プレイはOS乱数から生成したシードを使う(spec.md 3章)。
    let mut rng = rand::rng();
    let seed: u64 = rng.random();
    let mut game = Game::new(seed);

    let mut last_tick = Instant::now();

    loop {
        if let Some(action) = input::poll_input(FRAME_INTERVAL_MS)? {
            match action {
                InputAction::Quit => break,
                InputAction::TogglePause => game.toggle_pause(),
                InputAction::Move(dir) => {
                    let events = game.try_input_move(dir);
                    handle_events(&events, mixer.as_ref());
                }
            }
        }

        let now = Instant::now();
        let delta = now.duration_since(last_tick);
        last_tick = now;

        let events = game.update(delta.min(Duration::from_millis(250)));
        handle_events(&events, mixer.as_ref());

        terminal.draw(|frame| ui::render::draw(frame, &game))?;
    }

    bgm_stop.store(true, Ordering::Relaxed);

    Ok(())
}

/// ゲームイベントを対応する効果音再生へ変換する。
fn handle_events(events: &[GameEvent], mixer: Option<&Mixer>) {
    let Some(mixer) = mixer else {
        return;
    };

    for event in events {
        match event {
            GameEvent::Dig => audio::sfx::play_dig(mixer),
            GameEvent::DigFailRock => audio::sfx::play_dig_fail(mixer),
            GameEvent::BlockDestroyed => audio::sfx::play_destroy(mixer),
            GameEvent::OxygenCollected => audio::sfx::play_oxygen_pickup(mixer),
            // ダイヤ取得の専用SEはspec.md 10章のSE一覧に定義が無いため無音(得点加算のみ)。
            GameEvent::DiamondCollected => {}
            GameEvent::Checkpoint(cp) => {
                if cp.is_clear {
                    audio::sfx::play_clear_fanfare(mixer);
                } else {
                    audio::sfx::play_checkpoint(mixer);
                }
            }
            GameEvent::OxygenWarningTick => audio::sfx::play_oxygen_warning(mixer),
            GameEvent::Crushed | GameEvent::OutOfOxygen => audio::sfx::play_miss(mixer),
            // クリアファンファーレはCheckpoint(is_clear=true)側で鳴らし済みのため二重再生しない。
            GameEvent::Cleared => {}
        }
    }
}

//! BGM: 矩形波による簡易チップチューン、4小節ループ(spec.md 10章)。
//! テンポ140BPM、1小節=8分音符8ステップの単声ベースライン。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rodio::mixer::Mixer;
use rodio::Player;

use crate::audio::sfx::square_tone;

/// BGM再生音量の目安(spec.md 10章「BGM用Sinkで0.25前後」)。
const BGM_VOLUME: f32 = 0.25;

/// 1ステップ(8分音符)の長さ(ms)。テンポ140BPMでは1拍(4分音符)=約428.6ms、
/// 8分音符はその半分=約214.3ms(整数msに丸め)。
const STEP_MS: u64 = 214;

/// 各ノートの生成振幅(Player側のset_volumeとは別に波形自体の音量を抑える)。
const NOTE_AMPLITUDE: f32 = 0.5;

// 音名 -> 周波数(Hz)。十二平均律、A4=440Hz基準。
const C4: f32 = 261.63;
const D4: f32 = 293.66;
const E4: f32 = 329.63;
const F3: f32 = 174.61;
const G3: f32 = 196.00;
const G4: f32 = 392.00;
const A3: f32 = 220.00;
const B3: f32 = 246.94;

/// 4小節ぶんのノート列(spec.md 10章「参考ノート列」)。
const MEASURES: [[f32; 8]; 4] = [
    [C4, E4, G4, E4, C4, E4, G4, E4],
    [A3, C4, E4, C4, A3, C4, E4, C4],
    [F3, A3, C4, A3, F3, A3, C4, A3],
    [G3, B3, D4, B3, G3, B3, D4, B3],
];

/// 別スレッドでBGMのループ再生を開始する。
///
/// `stop_flag`がtrueになったら、キリの良い(次のノート境界の)タイミングで再生を止めて
/// スレッドを終了する。呼び出し側はミキサーを共有するだけで、スレッドの結合(join)は
/// 必須ではない(アプリ終了時にプロセスごと終わるため)。
pub fn spawn_bgm_thread(mixer: Mixer, stop_flag: Arc<AtomicBool>) {
    thread::spawn(move || {
        let player = Player::connect_new(&mixer);
        player.set_volume(BGM_VOLUME);
        let step_duration = Duration::from_millis(STEP_MS);

        'outer: loop {
            for measure in MEASURES {
                for freq in measure {
                    if stop_flag.load(Ordering::Relaxed) {
                        break 'outer;
                    }
                    player.append(square_tone(freq, STEP_MS, NOTE_AMPLITUDE));
                    thread::sleep(step_duration);
                }
            }
        }

        player.stop();
    });
}

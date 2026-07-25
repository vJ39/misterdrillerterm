//! BGM: 矩形波(メロディ)・三角波(ベースライン)・サイン波和音(ハーモニー/パッド)の
//! 3チャンネル構成による4小節ループ(spec.md 10章を拡張、TERM独自拡張)。
//! テンポ140BPM、1小節=8分音符8ステップ。各音にアタック・ディケイのエンベロープを
//! 付けて単調なビープ感を抑えている。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rodio::mixer::Mixer;
use rodio::Player;

use crate::audio::sfx::{sine_chord, square_tone_enveloped, triangle_tone_enveloped};

/// メロディ(矩形波)チャンネルの再生音量の目安。
const MELODY_VOLUME: f32 = 0.20;
/// ベースライン(三角波)チャンネルの再生音量の目安。
const BASS_VOLUME: f32 = 0.18;
/// ハーモニー/パッド(サイン波和音)チャンネルの再生音量の目安。
/// spec.md 10章の「BGM用Sinkで0.25前後」を3チャンネル合計で概ね踏襲する。
const HARMONY_VOLUME: f32 = 0.13;

/// 1ステップ(8分音符)の長さ(ms)。テンポ140BPMでは1拍(4分音符)=約428.6ms、
/// 8分音符はその半分=約214.3ms(整数msに丸め)。
const STEP_MS: u64 = 214;
/// 1小節(8ステップ)の長さ(ms)。ハーモニー/パッドの持続時間に使う。
const MEASURE_MS: u64 = STEP_MS * 8;
/// 半小節(4ステップ)の長さ(ms)。ベースラインの1音の長さに使う。
const HALF_MEASURE_MS: u64 = STEP_MS * 4;

/// 各ノートの生成振幅(Player側のset_volumeとは別に波形自体の音量を抑える)。
const MELODY_AMPLITUDE: f32 = 0.5;
const BASS_AMPLITUDE: f32 = 0.6;
const HARMONY_AMPLITUDE: f32 = 0.5;

/// メロディ(矩形波)のアタック・ディケイ。短い8分音符の粒立ちを整え、
/// ベタ打ちのビープ感を抑える。
const MELODY_ATTACK_MS: u64 = 4;
const MELODY_DECAY_MS: u64 = 40;
const MELODY_SUSTAIN: f32 = 0.6;

/// ベースライン(三角波)のアタック・ディケイ。輪郭のはっきりした低音の立ち上がりから
/// 緩やかに減衰させる。
const BASS_ATTACK_MS: u64 = 8;
const BASS_DECAY_MS: u64 = 200;
const BASS_SUSTAIN: f32 = 0.5;

/// ハーモニー/パッド(サイン波和音)のアタック・ディケイ。ゆっくり立ち上がり、
/// 小節の間ずっと薄く持続するパッド的な鳴り方にする。
const HARMONY_ATTACK_MS: u64 = 40;
const HARMONY_DECAY_MS: u64 = 500;
const HARMONY_SUSTAIN: f32 = 0.45;

// 音名 -> 周波数(Hz)。十二平均律、A4=440Hz基準。
const C3: f32 = 130.81;
const F2: f32 = 87.31;
const G2: f32 = 98.00;
const A2: f32 = 110.00;
const C4: f32 = 261.63;
const D4: f32 = 293.66;
const E4: f32 = 329.63;
const F3: f32 = 174.61;
const G3: f32 = 196.00;
const G4: f32 = 392.00;
const A3: f32 = 220.00;
const B3: f32 = 246.94;

/// 4小節ぶんのメロディノート列(spec.md 10章「参考ノート列」)。
const MEASURES: [[f32; 8]; 4] = [
    [C4, E4, G4, E4, C4, E4, G4, E4],
    [A3, C4, E4, C4, A3, C4, E4, C4],
    [F3, A3, C4, A3, F3, A3, C4, A3],
    [G3, B3, D4, B3, G3, B3, D4, B3],
];

/// 各小節のベースライン根音(メロディの主音の1オクターブ下、TERM独自拡張)。
/// `MEASURES`と同じ小節インデックスで対応する。
const BASS_ROOTS: [f32; 4] = [C3, A2, F2, G2];

/// 各小節のハーモニー和音(メロディのアルペジオと同じ構成音=ルート・3度・5度、
/// TERM独自拡張)。`MEASURES`と同じ小節インデックスで対応する。
const HARMONY_CHORDS: [[f32; 3]; 4] = [
    [C4, E4, G4],
    [A3, C4, E4],
    [F3, A3, C4],
    [G3, B3, D4],
];

/// 別スレッドでBGMのループ再生を開始する。
///
/// メロディ(矩形波)・ベースライン(三角波)・ハーモニー/パッド(サイン波和音)の
/// 3チャンネルを、それぞれ専用の`Player`で同一ミキサーへ同時再生することで
/// 単声のビープ感から脱した厚みのあるサウンドにする(TERM独自拡張、spec.md 10章の
/// 単声ベースラインを拡張)。
///
/// `stop_flag`がtrueになったら、キリの良い(次のノート境界の)タイミングで再生を止めて
/// スレッドを終了する。呼び出し側はミキサーを共有するだけで、スレッドの結合(join)は
/// 必須ではない(アプリ終了時にプロセスごと終わるため)。
///
/// `sound_enabled`がfalseの間はノートの再生自体をスキップする(無音)。ループの進行位置は
/// 止めずに数え続けるため、再度ONにした際は途切れた小節の途中から自然に復帰する
/// (TERM独自拡張、spec.md 10章)。
pub fn spawn_bgm_thread(mixer: Mixer, stop_flag: Arc<AtomicBool>, sound_enabled: Arc<AtomicBool>) {
    thread::spawn(move || {
        let melody_player = Player::connect_new(&mixer);
        melody_player.set_volume(MELODY_VOLUME);
        let bass_player = Player::connect_new(&mixer);
        bass_player.set_volume(BASS_VOLUME);
        let harmony_player = Player::connect_new(&mixer);
        harmony_player.set_volume(HARMONY_VOLUME);

        let step_duration = Duration::from_millis(STEP_MS);

        'outer: loop {
            for (measure_idx, measure) in MEASURES.iter().enumerate() {
                for (step_idx, freq) in measure.iter().enumerate() {
                    if stop_flag.load(Ordering::Relaxed) {
                        break 'outer;
                    }
                    if sound_enabled.load(Ordering::Relaxed) {
                        // ハーモニー/パッドは小節の頭で1回だけ、小節いっぱいの長さで鳴らす。
                        if step_idx == 0 {
                            harmony_player.append(sine_chord(
                                &HARMONY_CHORDS[measure_idx],
                                MEASURE_MS,
                                HARMONY_AMPLITUDE,
                                HARMONY_ATTACK_MS,
                                HARMONY_DECAY_MS,
                                HARMONY_SUSTAIN,
                            ));
                        }
                        // ベースラインは半小節ごとに根音を打ち直し、パッドと違う動きをつける。
                        if step_idx == 0 || step_idx == 4 {
                            bass_player.append(triangle_tone_enveloped(
                                BASS_ROOTS[measure_idx],
                                HALF_MEASURE_MS,
                                BASS_AMPLITUDE,
                                BASS_ATTACK_MS,
                                BASS_DECAY_MS,
                                BASS_SUSTAIN,
                            ));
                        }
                        melody_player.append(square_tone_enveloped(
                            *freq,
                            STEP_MS,
                            MELODY_AMPLITUDE,
                            MELODY_ATTACK_MS,
                            MELODY_DECAY_MS,
                            MELODY_SUSTAIN,
                        ));
                    }
                    thread::sleep(step_duration);
                }
            }
        }

        melody_player.stop();
        bass_player.stop();
        harmony_player.stop();
    });
}

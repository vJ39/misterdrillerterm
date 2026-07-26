//! 効果音(SE)・BGM用の波形ジェネレータ。rodioの`Source`トレイトを自前実装し、
//! 矩形波・三角波・サイン波(和音)をその場で生成する(spec.md 10章)。
//! 実機音源(WAV/MP3等のサンプル素材)は使用しない。

use std::num::NonZero;
use std::time::Duration;

use rodio::mixer::Mixer;
use rodio::{ChannelCount, Player, SampleRate, Source};

/// 生成する波形のサンプルレート(Hz)。
const SAMPLE_RATE: u32 = 44100;

/// 末尾フェードアウトの長さ(ms)。クリック音(プチノイズ)防止用(spec.md 10章)。
const FADE_MS: u64 = 3;

/// SE再生音量の目安(spec.md 10章「SE用Sinkで0.6〜0.8」)。
pub const SE_VOLUME: f32 = 0.7;

/// 指定サンプルレート・長さ(ms)から、総サンプル数とフェードサンプル数を求める。
fn sample_counts(sample_rate: u32, duration_ms: u64) -> (u64, u64) {
    let total = (sample_rate as u64 * duration_ms / 1000).max(1);
    let fade = (sample_rate as u64 * FADE_MS / 1000).clamp(1, total / 2 + 1).min(total);
    (total, fade)
}

/// 終端に向けて振幅を線形に0へ収束させるエンベロープ値(0.0〜1.0)を返す。
fn tail_envelope(sample_index: u64, total_samples: u64, fade_samples: u64) -> f32 {
    let remaining = total_samples.saturating_sub(sample_index);
    if remaining < fade_samples {
        remaining as f32 / fade_samples as f32
    } else {
        1.0
    }
}

/// 各波形共通の「サンプル進行・エンベロープ管理」状態。
/// 波形そのものの計算(位相の進め方)だけが型ごとに異なるため、その他の管理項目と
/// `Source`トレイト実装をここに集約する。
///
/// エンベロープはアタック(0→1に立ち上がる)・ディケイ(1→`sustain_level`まで減衰する)・
/// サステイン(`sustain_level`を維持する)・末尾フェード(クリック音防止のため0へ収束する)の
/// 4段で構成する簡易AD(+S+末尾フェード)方式(BGMの単調なビープ感を抑えるためのTERM独自拡張)。
/// 既存のSE(attack=0, decay=0, sustain=1.0)は末尾フェードのみとなり、従来通りの挙動を保つ。
struct Oscillator {
    sample_rate: u32,
    sample_index: u64,
    total_samples: u64,
    fade_samples: u64,
    attack_samples: u64,
    decay_samples: u64,
    sustain_level: f32,
    amplitude: f32,
}

impl Oscillator {
    fn new(duration_ms: u64, amplitude: f32) -> Self {
        Oscillator::new_with_envelope(duration_ms, amplitude, 0, 0, 1.0)
    }

    /// アタック・ディケイ付きのオシレータを生成する。
    /// `attack_ms`で0→1に立ち上がった後、`decay_ms`かけて`sustain_level`まで減衰し、
    /// 以降は末尾フェードが始まるまで`sustain_level`を維持する。
    fn new_with_envelope(
        duration_ms: u64,
        amplitude: f32,
        attack_ms: u64,
        decay_ms: u64,
        sustain_level: f32,
    ) -> Self {
        let (total_samples, fade_samples) = sample_counts(SAMPLE_RATE, duration_ms);
        let attack_samples = (SAMPLE_RATE as u64 * attack_ms / 1000).min(total_samples);
        let remaining_after_attack = total_samples.saturating_sub(attack_samples);
        let decay_samples = (SAMPLE_RATE as u64 * decay_ms / 1000).min(remaining_after_attack);
        Oscillator {
            sample_rate: SAMPLE_RATE,
            sample_index: 0,
            total_samples,
            fade_samples,
            attack_samples,
            decay_samples,
            sustain_level: sustain_level.clamp(0.0, 1.0),
            amplitude,
        }
    }

    fn finished(&self) -> bool {
        self.sample_index >= self.total_samples
    }

    /// アタック→ディケイ→サステインの段階を表す振幅係数(0.0〜1.0)。
    fn attack_decay_level(&self) -> f32 {
        if self.attack_samples > 0 && self.sample_index < self.attack_samples {
            return self.sample_index as f32 / self.attack_samples as f32;
        }
        let after_attack = self.sample_index.saturating_sub(self.attack_samples);
        if self.decay_samples == 0 || after_attack >= self.decay_samples {
            self.sustain_level
        } else {
            let progress = after_attack as f32 / self.decay_samples as f32;
            1.0 - progress * (1.0 - self.sustain_level)
        }
    }

    fn envelope(&self) -> f32 {
        self.attack_decay_level() * tail_envelope(self.sample_index, self.total_samples, self.fade_samples)
    }

    fn advance(&mut self) {
        self.sample_index += 1;
    }
}

/// `Oscillator`を内包する型に対し、`rodio::Source`の共通4メソッドを実装するマクロ。
/// 波形固有の`Iterator::next`実装だけを型ごとに書けばよくする。
macro_rules! impl_source_via_oscillator {
    ($ty:ty) => {
        impl Source for $ty {
            fn current_span_len(&self) -> Option<usize> {
                Some((self.osc.total_samples - self.osc.sample_index) as usize)
            }

            fn channels(&self) -> ChannelCount {
                NonZero::new(1).expect("channel count 1 is non-zero")
            }

            fn sample_rate(&self) -> SampleRate {
                NonZero::new(self.osc.sample_rate).expect("sample rate is non-zero")
            }

            fn total_duration(&self) -> Option<Duration> {
                Some(Duration::from_secs_f64(
                    self.osc.total_samples as f64 / self.osc.sample_rate as f64,
                ))
            }
        }
    };
}

/// 単一周波数の矩形波(有限長・末尾フェード付き)。
pub struct SquareWave {
    freq: f32,
    osc: Oscillator,
}

impl SquareWave {
    fn new(freq: f32, duration_ms: u64, amplitude: f32) -> Self {
        SquareWave {
            freq,
            osc: Oscillator::new(duration_ms, amplitude),
        }
    }

    /// アタック・ディケイのエンベロープ付きで生成する(BGMのメロディ用)。
    fn with_envelope(
        freq: f32,
        duration_ms: u64,
        amplitude: f32,
        attack_ms: u64,
        decay_ms: u64,
        sustain_level: f32,
    ) -> Self {
        SquareWave {
            freq,
            osc: Oscillator::new_with_envelope(duration_ms, amplitude, attack_ms, decay_ms, sustain_level),
        }
    }
}

impl Iterator for SquareWave {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.osc.finished() {
            return None;
        }
        let t = self.osc.sample_index as f32 / self.osc.sample_rate as f32;
        let phase = (t * self.freq).fract();
        let raw = if phase < 0.5 { self.osc.amplitude } else { -self.osc.amplitude };
        let value = raw * self.osc.envelope();
        self.osc.advance();
        Some(value)
    }
}

impl_source_via_oscillator!(SquareWave);

/// 周波数がfreq_startからfreq_endへ線形に変化する矩形波(下降/上昇チャープ)。
/// 位相はサンプル毎に周波数を積分して連続的に進めるため、周波数変化によるクリックは出ない。
pub struct SquareChirp {
    freq_start: f32,
    freq_end: f32,
    phase: f32,
    osc: Oscillator,
}

impl SquareChirp {
    fn new(freq_start: f32, freq_end: f32, duration_ms: u64, amplitude: f32) -> Self {
        SquareChirp {
            freq_start,
            freq_end,
            phase: 0.0,
            osc: Oscillator::new(duration_ms, amplitude),
        }
    }
}

impl Iterator for SquareChirp {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.osc.finished() {
            return None;
        }
        let progress = self.osc.sample_index as f32 / self.osc.total_samples as f32;
        let freq = self.freq_start + (self.freq_end - self.freq_start) * progress;
        self.phase = (self.phase + freq / self.osc.sample_rate as f32).fract();
        let raw = if self.phase < 0.5 { self.osc.amplitude } else { -self.osc.amplitude };
        let value = raw * self.osc.envelope();
        self.osc.advance();
        Some(value)
    }
}

impl_source_via_oscillator!(SquareChirp);

/// 単一周波数の三角波(有限長・AD/末尾フェード付き)。BGMのベースライン用。
/// 矩形波より倍音が少なく丸い音色になるため、低音のベースパートに向く。
pub struct TriangleWave {
    freq: f32,
    osc: Oscillator,
}

impl TriangleWave {
    fn with_envelope(
        freq: f32,
        duration_ms: u64,
        amplitude: f32,
        attack_ms: u64,
        decay_ms: u64,
        sustain_level: f32,
    ) -> Self {
        TriangleWave {
            freq,
            osc: Oscillator::new_with_envelope(duration_ms, amplitude, attack_ms, decay_ms, sustain_level),
        }
    }
}

impl Iterator for TriangleWave {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.osc.finished() {
            return None;
        }
        let t = self.osc.sample_index as f32 / self.osc.sample_rate as f32;
        let phase = (t * self.freq).fract();
        // 標準的な三角波: 0→1で-1→1へ線形上昇、1→0.5→1(次周期)で1→-1へ線形下降。
        let raw = if phase < 0.5 { -1.0 + 4.0 * phase } else { 3.0 - 4.0 * phase };
        let value = raw * self.osc.amplitude * self.osc.envelope();
        self.osc.advance();
        Some(value)
    }
}

impl_source_via_oscillator!(TriangleWave);

/// 複数周波数のサイン波を加算合成した和音(有限長・AD/末尾フェード付き)。
/// BGMのハーモニー/パッド用(中音域に長めの和音を敷いて音の厚みを出す)。
pub struct SineChord {
    freqs: Vec<f32>,
    osc: Oscillator,
}

impl SineChord {
    fn with_envelope(
        freqs: Vec<f32>,
        duration_ms: u64,
        amplitude: f32,
        attack_ms: u64,
        decay_ms: u64,
        sustain_level: f32,
    ) -> Self {
        SineChord {
            freqs,
            osc: Oscillator::new_with_envelope(duration_ms, amplitude, attack_ms, decay_ms, sustain_level),
        }
    }
}

impl Iterator for SineChord {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.osc.finished() {
            return None;
        }
        let t = self.osc.sample_index as f32 / self.osc.sample_rate as f32;
        let voice_count = self.freqs.len().max(1) as f32;
        let raw: f32 = self
            .freqs
            .iter()
            .map(|freq| (2.0 * std::f32::consts::PI * freq * t).sin())
            .sum::<f32>()
            / voice_count;
        let value = raw * self.osc.amplitude * self.osc.envelope();
        self.osc.advance();
        Some(value)
    }
}

impl_source_via_oscillator!(SineChord);

/// 単一周波数の矩形波を作る(BGM用にも共用)。
pub fn square_tone(freq: f32, duration_ms: u64, amplitude: f32) -> SquareWave {
    SquareWave::new(freq, duration_ms, amplitude)
}

/// アタック・ディケイ付きの矩形波を作る(BGMのメロディ用)。
pub fn square_tone_enveloped(
    freq: f32,
    duration_ms: u64,
    amplitude: f32,
    attack_ms: u64,
    decay_ms: u64,
    sustain_level: f32,
) -> SquareWave {
    SquareWave::with_envelope(freq, duration_ms, amplitude, attack_ms, decay_ms, sustain_level)
}

/// アタック・ディケイ付きの三角波を作る(BGMのベースライン用)。
pub fn triangle_tone_enveloped(
    freq: f32,
    duration_ms: u64,
    amplitude: f32,
    attack_ms: u64,
    decay_ms: u64,
    sustain_level: f32,
) -> TriangleWave {
    TriangleWave::with_envelope(freq, duration_ms, amplitude, attack_ms, decay_ms, sustain_level)
}

/// アタック・ディケイ付きのサイン波和音を作る(BGMのハーモニー/パッド用)。
pub fn sine_chord(
    freqs: &[f32],
    duration_ms: u64,
    amplitude: f32,
    attack_ms: u64,
    decay_ms: u64,
    sustain_level: f32,
) -> SineChord {
    SineChord::with_envelope(freqs.to_vec(), duration_ms, amplitude, attack_ms, decay_ms, sustain_level)
}

/// 周波数が線形に変化する矩形波(チャープ)を作る。
pub fn square_chirp(freq_start: f32, freq_end: f32, duration_ms: u64, amplitude: f32) -> SquareChirp {
    SquareChirp::new(freq_start, freq_end, duration_ms, amplitude)
}

/// 単発の音をミキサーへ即座に流す(fire-and-forget、他の音との重複再生も可)。
fn play_tone<S>(mixer: &Mixer, source: S, volume: f32)
where
    S: Source + Send + 'static,
{
    mixer.add(source.amplify(volume));
}

/// 複数音を順番に鳴らす(2音・4音アルペジオ等)。専用の`Player`をミキサーへ接続し、
/// 音を`.append()`で連結してから`detach()`することで、呼び出し元に控えを持たせずに
/// 最後まで再生させる。
fn play_sequence(mixer: &Mixer, tones: Vec<Box<dyn Source<Item = f32> + Send>>, volume: f32) {
    let player = Player::connect_new(mixer);
    player.set_volume(volume);
    for tone in tones {
        player.append(tone);
    }
    player.detach();
}

/// 掘削音: 色ブロックの直接掘削、または岩ブロックへのヒットが実際に発生した瞬間。
/// 矩形波 440Hz, 20ms(spec.md 10章)。
pub fn play_dig(mixer: &Mixer) {
    play_tone(mixer, square_tone(440.0, 20, 0.5), SE_VOLUME);
}

/// 岩ブロックヒット音(未破壊): 岩ブロックへ掘削入力し、5回目未満でまだ破壊に至らない瞬間。
/// 矩形波(短い低音) 220Hz, 20ms(spec.md 10章)。
pub fn play_rock_hit(mixer: &Mixer) {
    play_tone(mixer, square_tone(220.0, 20, 0.5), SE_VOLUME);
}

/// 破壊音の下降チャープを何連にするか(4個消滅ごとに1つ追加)の上限(TERM独自拡張)。
/// 大量消滅時に音がどこまでも長く伸び続けるのを防ぐ。
const MAX_DESTROY_CHIRPS: usize = 5;

/// 破壊音: ブロックが破壊され消滅した瞬間。矩形波(下降チャープ) 220Hz→110Hz, 60ms。
/// `blocks`(消滅数)が4個増えるごとにチャープを1つ追加し、たくさん消えるほど
/// 「たくさん消えてる」感が出るようにする(ユーザー指摘: 「ブロックが消えたときの
/// SEが必要たくさん消えるといっぱい消えてる感じに」)。1〜3個は従来通り単発のまま。
pub fn play_destroy(mixer: &Mixer, blocks: usize) {
    let chirp_count = (blocks / 4 + 1).min(MAX_DESTROY_CHIRPS);
    if chirp_count == 1 {
        play_tone(mixer, square_chirp(220.0, 110.0, 60, 0.5), SE_VOLUME);
        return;
    }
    let tones: Vec<Box<dyn Source<Item = f32> + Send>> = (0..chirp_count)
        .map(|i| {
            let start = 220.0 - i as f32 * 30.0;
            Box::new(square_chirp(start, start * 0.5, 45, 0.5)) as Box<dyn Source<Item = f32> + Send>
        })
        .collect();
    play_sequence(mixer, tones, SE_VOLUME);
}

/// 岩ブロック(Xブロック)破壊音: 色ブロックの破壊音より低く粗い「ゴツッ」という質感の
/// 2音(TERM独自拡張。ユーザー指摘: 「Xブロックを壊したときに専用SEを鳴らす」)。
/// `blocks`(消滅数)が4個増えるごとにもう1組追加し、`play_destroy`と同様に大量消滅時の
/// 「たくさん消えてる」感を出す。
pub fn play_rock_destroy(mixer: &Mixer, blocks: usize) {
    let clunk_count = (blocks / 4 + 1).min(MAX_DESTROY_CHIRPS);
    let tones: Vec<Box<dyn Source<Item = f32> + Send>> = (0..clunk_count)
        .flat_map(|i| {
            let start = 150.0 - i as f32 * 15.0;
            [
                Box::new(square_tone(start, 40, 0.5)) as Box<dyn Source<Item = f32> + Send>,
                Box::new(square_tone(start * 0.6, 50, 0.5)) as Box<dyn Source<Item = f32> + Send>,
            ]
        })
        .collect();
    play_sequence(mixer, tones, SE_VOLUME);
}

/// ヒヤリ回避スライダー発動音: ブロックが落ち始める直前に間一髪回避した瞬間の「わ〜!」
/// という驚きを表す、素早く上昇するチャープ(TERM独自拡張。ユーザー指摘: 「キャラが
/// スライディングした瞬間...専用SEを鳴らす」)。他の効果音は全て下降チャープなので、
/// 唯一の上昇チャープとして区別できるようにする。
pub fn play_dodge(mixer: &Mixer) {
    play_tone(mixer, square_chirp(300.0, 700.0, 80, 0.5), SE_VOLUME);
}

/// air取得音: 酸素カプセル取得時。矩形波2音 523Hz(60ms)→784Hz(60ms)。
pub fn play_oxygen_pickup(mixer: &Mixer) {
    play_sequence(
        mixer,
        vec![
            Box::new(square_tone(523.0, 60, 0.5)),
            Box::new(square_tone(784.0, 60, 0.5)),
        ],
        SE_VOLUME,
    );
}

/// 酸素警告音: 酸素残量が20以下の間、1秒間隔で。矩形波 880Hz, 200ms。
pub fn play_oxygen_warning(mixer: &Mixer) {
    play_tone(mixer, square_tone(880.0, 200, 0.5), SE_VOLUME);
}

/// レベルアップ音: 30mごとのレベル到達時(spec.md 7章)。矩形波4音アルペジオ
/// 523/659/784/1046Hz、各80ms(spec.md 10章)。
pub fn play_level_up(mixer: &Mixer) {
    play_sequence(
        mixer,
        vec![
            Box::new(square_tone(523.0, 80, 0.5)),
            Box::new(square_tone(659.0, 80, 0.5)),
            Box::new(square_tone(784.0, 80, 0.5)),
            Box::new(square_tone(1046.0, 80, 0.5)),
        ],
        SE_VOLUME,
    );
}

/// クリアファンファーレ: 1000m到達時。矩形波(チェックポイント音の拡張)
/// 523/659/784/1046/1318Hz、各100ms。
pub fn play_clear_fanfare(mixer: &Mixer) {
    play_sequence(
        mixer,
        vec![
            Box::new(square_tone(523.0, 100, 0.5)),
            Box::new(square_tone(659.0, 100, 0.5)),
            Box::new(square_tone(784.0, 100, 0.5)),
            Box::new(square_tone(1046.0, 100, 0.5)),
            Box::new(square_tone(1318.0, 100, 0.5)),
        ],
        SE_VOLUME,
    );
}

/// ミス音(ゲームオーバー): 最後のライフを失った瞬間。矩形波(下降チャープ)
/// 440Hz→110Hz, 500ms(spec.md 10章)。
pub fn play_miss(mixer: &Mixer) {
    play_tone(mixer, square_chirp(440.0, 110.0, 500, 0.5), SE_VOLUME);
}

/// ライフロス音: ライフを1つ失ったが、まだライフが残っている瞬間。矩形波(短い下降チャープ)
/// 440Hz→220Hz, 250ms(spec.md 10章)。
pub fn play_life_lost(mixer: &Mixer) {
    play_tone(mixer, square_chirp(440.0, 220.0, 250, 0.5), SE_VOLUME);
}

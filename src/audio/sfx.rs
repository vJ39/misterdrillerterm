//! 効果音(SE)。rodioの`Source`トレイトを自前実装し、矩形波をその場で生成する
//! (spec.md 10章)。実機音源(WAV/MP3等のサンプル素材)は使用しない。

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

/// `SquareWave`/`SquareChirp`共通の「サンプル進行・末尾フェード管理」状態。
/// 波形そのものの計算(位相の進め方)だけが両者で異なるため、その他の管理項目と
/// `Source`トレイト実装をここに集約する。
struct Oscillator {
    sample_rate: u32,
    sample_index: u64,
    total_samples: u64,
    fade_samples: u64,
    amplitude: f32,
}

impl Oscillator {
    fn new(duration_ms: u64, amplitude: f32) -> Self {
        let (total_samples, fade_samples) = sample_counts(SAMPLE_RATE, duration_ms);
        Oscillator {
            sample_rate: SAMPLE_RATE,
            sample_index: 0,
            total_samples,
            fade_samples,
            amplitude,
        }
    }

    fn finished(&self) -> bool {
        self.sample_index >= self.total_samples
    }

    fn envelope(&self) -> f32 {
        tail_envelope(self.sample_index, self.total_samples, self.fade_samples)
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

/// 単一周波数の矩形波を作る(BGM用にも共用)。
pub fn square_tone(freq: f32, duration_ms: u64, amplitude: f32) -> SquareWave {
    SquareWave::new(freq, duration_ms, amplitude)
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

/// 掘削音: 掘削入力を受け付けた瞬間(ブロックの有無に関わらず)。矩形波 440Hz, 20ms。
pub fn play_dig(mixer: &Mixer) {
    play_tone(mixer, square_tone(440.0, 20, 0.5), SE_VOLUME);
}

/// 破壊音: ブロックが破壊され消滅した瞬間。矩形波(下降チャープ) 220Hz→110Hz, 60ms。
pub fn play_destroy(mixer: &Mixer) {
    play_tone(mixer, square_chirp(220.0, 110.0, 60, 0.5), SE_VOLUME);
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

/// チェックポイント到達音: 200/400/600/800m到達時。矩形波4音アルペジオ
/// 523/659/784/1046Hz、各80ms。
pub fn play_checkpoint(mixer: &Mixer) {
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

/// ミス音: ゲームオーバー時。矩形波(下降チャープ) 440Hz→110Hz, 500ms。
pub fn play_miss(mixer: &Mixer) {
    play_tone(mixer, square_chirp(440.0, 110.0, 500, 0.5), SE_VOLUME);
}

/// 掘削失敗音(任意): 岩ブロックへ入力した時。矩形波(低音単発) 110Hz, 30ms。
pub fn play_dig_fail(mixer: &Mixer) {
    play_tone(mixer, square_tone(110.0, 30, 0.5), SE_VOLUME);
}

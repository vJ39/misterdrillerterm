//! BGM: 「地底のダンス」を6トラックのノートイベントデータ(`bgm_data`)から
//! 再生する(TERM独自拡張。#131)。従来は手動で調律・和声修正した16小節ループの
//! 4声ステップシーケンサーだったが、ユーザーが原曲から生成した6ステム
//! (vocals/bass/other_voice1/other_voice2/percussion_low/percussion_high)を
//! ピッチ検出ツール`mp3tobeep`でノートイベント化した実測データへ差し替えた。
//! 各トラックの開始秒に合わせて絶対時刻ベースで鳴らし分け、原曲の全長
//! (`bgm_data::DURATION_SEC`)が経過したら先頭へループする。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rodio::mixer::Mixer;
use rodio::Player;

use crate::audio::bgm_data::{self, NoteEvent};
use crate::audio::sfx::{square_tone_enveloped, triangle_tone_enveloped};

/// 各トラックの再生カーソルを進める間隔。ノートの最短の長さ(打楽器で約80ms)
/// より十分細かく、かつCPU負荷を抑えられる粒度にしている。
const TICK_MS: u64 = 10;

const VOCALS_VOLUME: f32 = 0.17;
const VOCALS_AMPLITUDE: f32 = 0.50;
const VOCALS_ATTACK_MS: u64 = 3;
const VOCALS_DECAY_MS: u64 = 30;
const VOCALS_SUSTAIN: f32 = 0.55;

const VOICE1_VOLUME: f32 = 0.09;
const VOICE1_AMPLITUDE: f32 = 0.34;
const VOICE1_ATTACK_MS: u64 = 2;
const VOICE1_DECAY_MS: u64 = 20;
const VOICE1_SUSTAIN: f32 = 0.42;

const VOICE2_VOLUME: f32 = 0.08;
const VOICE2_AMPLITUDE: f32 = 0.30;
const VOICE2_ATTACK_MS: u64 = 2;
const VOICE2_DECAY_MS: u64 = 22;
const VOICE2_SUSTAIN: f32 = 0.40;

const BASS_VOLUME: f32 = 0.17;
const BASS_AMPLITUDE: f32 = 0.58;
const BASS_ATTACK_MS: u64 = 5;
const BASS_DECAY_MS: u64 = 90;
const BASS_SUSTAIN: f32 = 0.54;

const PERC_LOW_VOLUME: f32 = 0.15;
const PERC_LOW_AMPLITUDE: f32 = 0.60;
const PERC_LOW_ATTACK_MS: u64 = 1;
const PERC_LOW_DECAY_MS: u64 = 40;
const PERC_LOW_SUSTAIN: f32 = 0.15;

const PERC_HIGH_VOLUME: f32 = 0.10;
const PERC_HIGH_AMPLITUDE: f32 = 0.50;
const PERC_HIGH_ATTACK_MS: u64 = 1;
const PERC_HIGH_DECAY_MS: u64 = 25;
const PERC_HIGH_SUSTAIN: f32 = 0.10;

/// 実際に鳴らす長さ(ms)を、ノート本来の長さ(`note.duration_sec`)と次のノートの
/// 開始時刻までの間隔(`max_gap_sec`)のうち短い方に基づいて決める(TERM独自拡張。
/// #135。ユーザー指摘: 「bgmだが、速くなったり遅くなったりするのが気になる」)。
/// `beepcode.json`は同一トラック内でもノートの長さが次のノート開始時刻を超えて
/// 重複していることが多く(ピッチ検出の推定値のため)、そのまま鳴らすと1トラック
/// につき1本しかないPlayerのキューに重複ぶんが積み重なってバックログとなり、
/// トラックごとに実時刻からのズレが別々に広がって「テンポが不安定」に聞こえる
/// バグがあった。次のノートが来るまでに必ず鳴らし終える長さへクリップすることで
/// バックログが発生しないようにする。
fn duration_ms(note: &NoteEvent, max_gap_sec: f32) -> u64 {
    let capped_sec = note.duration_sec.min(max_gap_sec.max(0.0));
    (capped_sec * 1000.0).round().max(1.0) as u64
}

/// ノート本来の音量(`base`)にvelocity(0.0〜1.0)を掛け合わせる。
fn amplitude(base: f32, note: &NoteEvent) -> f32 {
    base * note.velocity.clamp(0.0, 1.0)
}

/// 1トラックぶんの再生カーソル(TERM独自拡張。#131)。`bgm_data`の各トラックは
/// `start_sec`昇順に並んでいるため、直前に処理した位置から単調に読み進めるだけで
/// 「経過秒数までに開始すべきノート」を取りこぼしなく列挙できる。
struct TrackCursor<'a> {
    notes: &'a [NoteEvent],
    next: usize,
}

impl<'a> TrackCursor<'a> {
    fn new(notes: &'a [NoteEvent]) -> Self {
        Self { notes, next: 0 }
    }

    fn reset(&mut self) {
        self.next = 0;
    }

    /// `elapsed_sec`までに開始すべきノートそれぞれについて`f`を呼び、カーソルを進める。
    /// `f`にはノート本体に加え、次のノートの開始時刻までの間隔(秒。次のノートが
    /// 無ければそのノート自身の長さ)も渡す。同一トラック内でノートが重複していても
    /// キューにバックログを溜めないよう、再生時間をこの間隔でクリップするための
    /// 情報(TERM独自拡張。#135)。
    fn for_each_due(&mut self, elapsed_sec: f32, mut f: impl FnMut(&NoteEvent, f32)) {
        while let Some(note) = self.notes.get(self.next) {
            if note.start_sec > elapsed_sec {
                break;
            }
            let max_gap_sec = self
                .notes
                .get(self.next + 1)
                .map(|next_note| next_note.start_sec - note.start_sec)
                .unwrap_or(note.duration_sec);
            f(note, max_gap_sec);
            self.next += 1;
        }
    }
}

pub fn spawn_bgm_thread(mixer: Mixer, stop_flag: Arc<AtomicBool>, music_enabled: Arc<AtomicBool>) {
    thread::spawn(move || {
        let vocals_player = Player::connect_new(&mixer);
        vocals_player.set_volume(VOCALS_VOLUME);
        let voice1_player = Player::connect_new(&mixer);
        voice1_player.set_volume(VOICE1_VOLUME);
        let voice2_player = Player::connect_new(&mixer);
        voice2_player.set_volume(VOICE2_VOLUME);
        let bass_player = Player::connect_new(&mixer);
        bass_player.set_volume(BASS_VOLUME);
        let perc_low_player = Player::connect_new(&mixer);
        perc_low_player.set_volume(PERC_LOW_VOLUME);
        let perc_high_player = Player::connect_new(&mixer);
        perc_high_player.set_volume(PERC_HIGH_VOLUME);

        let mut vocals = TrackCursor::new(bgm_data::VOCALS);
        let mut voice1 = TrackCursor::new(bgm_data::OTHER_VOICE1);
        let mut voice2 = TrackCursor::new(bgm_data::OTHER_VOICE2);
        let mut bass = TrackCursor::new(bgm_data::BASS);
        let mut perc_low = TrackCursor::new(bgm_data::PERCUSSION_LOW);
        let mut perc_high = TrackCursor::new(bgm_data::PERCUSSION_HIGH);

        let tick = Duration::from_millis(TICK_MS);
        let mut loop_start = Instant::now();

        loop {
            if stop_flag.load(Ordering::Relaxed) {
                break;
            }

            let elapsed = loop_start.elapsed().as_secs_f32();
            if elapsed >= bgm_data::DURATION_SEC {
                loop_start = Instant::now();
                vocals.reset();
                voice1.reset();
                voice2.reset();
                bass.reset();
                perc_low.reset();
                perc_high.reset();
            } else if music_enabled.load(Ordering::Relaxed) {
                vocals.for_each_due(elapsed, |note, max_gap_sec| {
                    vocals_player.append(square_tone_enveloped(
                        note.freq_hz,
                        duration_ms(note, max_gap_sec),
                        amplitude(VOCALS_AMPLITUDE, note),
                        VOCALS_ATTACK_MS,
                        VOCALS_DECAY_MS,
                        VOCALS_SUSTAIN,
                    ));
                });
                voice1.for_each_due(elapsed, |note, max_gap_sec| {
                    voice1_player.append(square_tone_enveloped(
                        note.freq_hz,
                        duration_ms(note, max_gap_sec),
                        amplitude(VOICE1_AMPLITUDE, note),
                        VOICE1_ATTACK_MS,
                        VOICE1_DECAY_MS,
                        VOICE1_SUSTAIN,
                    ));
                });
                voice2.for_each_due(elapsed, |note, max_gap_sec| {
                    voice2_player.append(square_tone_enveloped(
                        note.freq_hz,
                        duration_ms(note, max_gap_sec),
                        amplitude(VOICE2_AMPLITUDE, note),
                        VOICE2_ATTACK_MS,
                        VOICE2_DECAY_MS,
                        VOICE2_SUSTAIN,
                    ));
                });
                bass.for_each_due(elapsed, |note, max_gap_sec| {
                    bass_player.append(triangle_tone_enveloped(
                        note.freq_hz,
                        duration_ms(note, max_gap_sec),
                        amplitude(BASS_AMPLITUDE, note),
                        BASS_ATTACK_MS,
                        BASS_DECAY_MS,
                        BASS_SUSTAIN,
                    ));
                });
                perc_low.for_each_due(elapsed, |note, max_gap_sec| {
                    perc_low_player.append(square_tone_enveloped(
                        note.freq_hz,
                        duration_ms(note, max_gap_sec),
                        amplitude(PERC_LOW_AMPLITUDE, note),
                        PERC_LOW_ATTACK_MS,
                        PERC_LOW_DECAY_MS,
                        PERC_LOW_SUSTAIN,
                    ));
                });
                perc_high.for_each_due(elapsed, |note, max_gap_sec| {
                    perc_high_player.append(square_tone_enveloped(
                        note.freq_hz,
                        duration_ms(note, max_gap_sec),
                        amplitude(PERC_HIGH_AMPLITUDE, note),
                        PERC_HIGH_ATTACK_MS,
                        PERC_HIGH_DECAY_MS,
                        PERC_HIGH_SUSTAIN,
                    ));
                });
            } else {
                // 無効中もカーソルだけは進め、再有効化した時に空白期間ぶんの
                // ノートをまとめて鳴らしてしまわないようにする。
                vocals.for_each_due(elapsed, |_, _| {});
                voice1.for_each_due(elapsed, |_, _| {});
                voice2.for_each_due(elapsed, |_, _| {});
                bass.for_each_due(elapsed, |_, _| {});
                perc_low.for_each_due(elapsed, |_, _| {});
                perc_high.for_each_due(elapsed, |_, _| {});
            }

            thread::sleep(tick);
        }

        vocals_player.stop();
        voice1_player.stop();
        voice2_player.stop();
        bass_player.stop();
        perc_low_player.stop();
        perc_high_player.stop();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_tracks_have_valid_finite_notes_sorted_by_start_time() {
        let tracks: [&[NoteEvent]; 6] = [
            bgm_data::VOCALS,
            bgm_data::OTHER_VOICE1,
            bgm_data::OTHER_VOICE2,
            bgm_data::BASS,
            bgm_data::PERCUSSION_LOW,
            bgm_data::PERCUSSION_HIGH,
        ];
        for notes in tracks {
            assert!(!notes.is_empty());
            let mut prev_start = 0.0f32;
            for note in notes {
                assert!(note.freq_hz.is_finite() && note.freq_hz > 0.0);
                assert!(note.duration_sec.is_finite() && note.duration_sec > 0.0);
                assert!((0.0..=1.0).contains(&note.velocity));
                assert!(note.start_sec >= prev_start, "start_secは昇順のはず");
                prev_start = note.start_sec;
            }
        }
    }

    #[test]
    fn track_cursor_yields_notes_up_to_the_elapsed_time_in_order_without_repeats() {
        let notes = [
            NoteEvent { freq_hz: 100.0, start_sec: 0.0, duration_sec: 0.1, velocity: 1.0 },
            NoteEvent { freq_hz: 200.0, start_sec: 0.5, duration_sec: 0.1, velocity: 1.0 },
            NoteEvent { freq_hz: 300.0, start_sec: 1.0, duration_sec: 0.1, velocity: 1.0 },
        ];
        let mut cursor = TrackCursor::new(&notes);

        let mut seen = Vec::new();
        cursor.for_each_due(0.6, |note, _| seen.push(note.freq_hz));
        assert_eq!(seen, vec![100.0, 200.0], "経過0.6秒までに開始する2つだけ列挙されるはず");

        // 同じ経過秒数で再度呼んでも、既に処理済みのノートは重複しないはず。
        let mut seen_again = Vec::new();
        cursor.for_each_due(0.6, |note, _| seen_again.push(note.freq_hz));
        assert!(seen_again.is_empty(), "同じ経過秒数を再度渡しても重複して鳴らさないはず");

        let mut seen_rest = Vec::new();
        cursor.for_each_due(1.0, |note, _| seen_rest.push(note.freq_hz));
        assert_eq!(seen_rest, vec![300.0]);
    }

    #[test]
    fn track_cursor_reset_replays_from_the_beginning() {
        let notes = [NoteEvent { freq_hz: 100.0, start_sec: 0.0, duration_sec: 0.1, velocity: 1.0 }];
        let mut cursor = TrackCursor::new(&notes);

        let mut count = 0;
        cursor.for_each_due(1.0, |_, _| count += 1);
        assert_eq!(count, 1);

        cursor.reset();
        let mut count_after_reset = 0;
        cursor.for_each_due(1.0, |_, _| count_after_reset += 1);
        assert_eq!(count_after_reset, 1, "resetの後は先頭から再び列挙されるはず");
    }

    #[test]
    fn track_cursor_reports_the_gap_to_the_next_note_even_when_notes_overlap() {
        // ユーザー指摘: 「bgmだが、速くなったり遅くなったりするのが気になる」。
        // 原曲データ(beepcode.json)は同一トラック内でもノートの長さが次のノートの
        // 開始時刻を超えて重複していることが多い。この重複を検知できないと
        // Playerのキューにバックログが溜まりテンポが不安定になる(#135)。
        let notes = [
            // 0.5秒のノートだが、次のノートは0.2秒後に開始する(0.3秒ぶん重複)。
            NoteEvent { freq_hz: 100.0, start_sec: 0.0, duration_sec: 0.5, velocity: 1.0 },
            NoteEvent { freq_hz: 200.0, start_sec: 0.2, duration_sec: 0.1, velocity: 1.0 },
        ];
        let mut cursor = TrackCursor::new(&notes);

        let mut gaps = Vec::new();
        cursor.for_each_due(0.2, |note, max_gap_sec| gaps.push((note.freq_hz, max_gap_sec)));
        assert_eq!(
            gaps,
            vec![(100.0, 0.2), (200.0, notes[1].duration_sec)],
            "1つ目は次のノート開始までの間隔(0.2秒、本来の長さ0.5秒より短い)、\
             2つ目は次が無いので自身の長さがそのまま返るはず"
        );
    }

    #[test]
    fn duration_ms_rounds_seconds_to_milliseconds_with_a_minimum_of_one() {
        let note = NoteEvent { freq_hz: 100.0, start_sec: 0.0, duration_sec: 0.0001, velocity: 1.0 };
        assert_eq!(
            duration_ms(&note, note.duration_sec),
            1,
            "極端に短い長さでも最低1msは確保するはず"
        );

        let note = NoteEvent { freq_hz: 100.0, start_sec: 0.0, duration_sec: 0.2, velocity: 1.0 };
        assert_eq!(duration_ms(&note, note.duration_sec), 200, "間隔が十分あれば本来の長さのまま");
    }

    #[test]
    fn duration_ms_is_capped_by_the_gap_to_the_next_note_so_the_queue_never_backs_up() {
        // #135の本体: ノート本来の長さが次のノートまでの間隔より長い場合、
        // 間隔の方にクリップされることを確認する(バックログ防止)。
        let note = NoteEvent { freq_hz: 100.0, start_sec: 0.0, duration_sec: 0.5, velocity: 1.0 };
        assert_eq!(
            duration_ms(&note, 0.2),
            200,
            "次のノートまでの間隔(0.2秒)が本来の長さ(0.5秒)より短ければクリップされるはず"
        );
    }

    #[test]
    fn amplitude_scales_base_by_velocity() {
        let note = NoteEvent { freq_hz: 100.0, start_sec: 0.0, duration_sec: 0.1, velocity: 0.5 };
        assert!((amplitude(1.0, &note) - 0.5).abs() < f32::EPSILON);
    }
}

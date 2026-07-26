//! BGM: 90年代後半のアーケードゲーム感を狙った完全オリジナルの8小節ループ。
//! 矩形波リード、矩形波の対旋律、三角波ベース、サイン波コードの4チャンネル構成。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use rodio::Player;
use rodio::mixer::Mixer;

use crate::audio::sfx::{sine_chord, square_tone_enveloped, triangle_tone_enveloped};

const LEAD_VOLUME: f32 = 0.17;
const COUNTER_VOLUME: f32 = 0.09;
const BASS_VOLUME: f32 = 0.17;
const CHORD_VOLUME: f32 = 0.11;

const BPM: u64 = 156;
const STEP_MS: u64 = 60_000 / BPM / 4;
const FOUR_STEPS_MS: u64 = STEP_MS * 4;

const LEAD_AMPLITUDE: f32 = 0.48;
const COUNTER_AMPLITUDE: f32 = 0.34;
const BASS_AMPLITUDE: f32 = 0.58;
const CHORD_AMPLITUDE: f32 = 0.46;

const LEAD_ATTACK_MS: u64 = 3;
const LEAD_DECAY_MS: u64 = 28;
const LEAD_SUSTAIN: f32 = 0.56;

const COUNTER_ATTACK_MS: u64 = 2;
const COUNTER_DECAY_MS: u64 = 20;
const COUNTER_SUSTAIN: f32 = 0.42;

const BASS_ATTACK_MS: u64 = 5;
const BASS_DECAY_MS: u64 = 90;
const BASS_SUSTAIN: f32 = 0.54;

const CHORD_ATTACK_MS: u64 = 8;
const CHORD_DECAY_MS: u64 = 150;
const CHORD_SUSTAIN: f32 = 0.32;

const C2: f32 = 65.41;
const D2: f32 = 73.42;
const E2: f32 = 82.41;
const F2: f32 = 87.31;
const G2: f32 = 98.00;
const A2: f32 = 110.00;
const B2: f32 = 123.47;

const C3: f32 = 130.81;
const D3: f32 = 146.83;
const E3: f32 = 164.81;
const F3: f32 = 174.61;
const G3: f32 = 196.00;
const A3: f32 = 220.00;
const B3: f32 = 246.94;

const C4: f32 = 261.63;
const CS4: f32 = 277.18;
const D4: f32 = 293.66;
const E4: f32 = 329.63;
const F4: f32 = 349.23;
const FS4: f32 = 369.99;
const G4: f32 = 392.00;
const A4: f32 = 440.00;
const B4: f32 = 493.88;

const C5: f32 = 523.25;
const D5: f32 = 587.33;
const E5: f32 = 659.25;
const G5: f32 = 783.99;

#[derive(Clone, Copy)]
struct Note {
    freq: f32,
    steps: u8,
}

impl Note {
    const fn new(freq: f32, steps: u8) -> Self {
        Self { freq, steps }
    }
}

type Pattern = [Option<Note>; 16];

const fn n(freq: f32, steps: u8) -> Option<Note> {
    Some(Note::new(freq, steps))
}

const LEAD: [Pattern; 8] = [
    [
        n(E4, 2),
        None,
        n(G4, 1),
        n(A4, 1),
        n(G4, 2),
        None,
        n(E4, 1),
        n(D4, 1),
        n(C4, 2),
        None,
        n(E4, 1),
        n(G4, 1),
        n(A4, 2),
        None,
        n(G4, 2),
        None,
    ],
    [
        n(E4, 1),
        n(G4, 1),
        n(A4, 2),
        None,
        n(C5, 2),
        None,
        n(B4, 1),
        n(A4, 1),
        n(G4, 2),
        n(E4, 1),
        n(D4, 1),
        n(E4, 2),
        None,
        n(G4, 1),
        n(A4, 1),
        n(B4, 2),
    ],
    [
        n(A4, 2),
        None,
        n(C5, 1),
        n(B4, 1),
        n(A4, 2),
        n(G4, 1),
        n(E4, 1),
        n(F4, 2),
        None,
        n(A4, 1),
        n(C5, 1),
        n(D5, 2),
        None,
        n(C5, 1),
        n(A4, 1),
        n(G4, 2),
    ],
    [
        n(G4, 1),
        n(A4, 1),
        n(B4, 2),
        n(D5, 2),
        None,
        n(B4, 1),
        n(G4, 1),
        n(FS4, 2),
        n(G4, 1),
        n(A4, 1),
        n(B4, 1),
        n(D5, 1),
        n(E5, 2),
        n(D5, 1),
        n(B4, 1),
        n(G4, 2),
    ],
    [
        n(E5, 2),
        None,
        n(D5, 1),
        n(C5, 1),
        n(B4, 2),
        n(G4, 1),
        n(E4, 1),
        n(A4, 2),
        None,
        n(C5, 1),
        n(B4, 1),
        n(A4, 2),
        n(G4, 1),
        n(E4, 1),
        n(D4, 2),
        None,
    ],
    [
        n(F4, 1),
        n(A4, 1),
        n(C5, 2),
        n(E5, 2),
        None,
        n(D5, 1),
        n(C5, 1),
        n(A4, 2),
        n(F4, 1),
        n(G4, 1),
        n(A4, 2),
        n(C5, 1),
        n(D5, 1),
        n(E5, 2),
        None,
        None,
    ],
    [
        n(G4, 2),
        n(B4, 1),
        n(D5, 1),
        n(G5, 2),
        None,
        n(D5, 1),
        n(B4, 1),
        n(A4, 2),
        n(B4, 1),
        n(C5, 1),
        n(D5, 2),
        n(B4, 1),
        n(G4, 1),
        n(FS4, 2),
        None,
        None,
    ],
    [
        n(E4, 1),
        n(G4, 1),
        n(A4, 1),
        n(B4, 1),
        n(C5, 2),
        n(B4, 1),
        n(A4, 1),
        n(G4, 1),
        n(E4, 1),
        None,
        n(D4, 1),
        n(CS4, 1),
        n(D4, 1),
        n(E4, 1),
        n(G4, 1),
        n(C5, 2),
    ],
];

const COUNTER: [Pattern; 8] = [
    [
        None,
        None,
        n(C5, 1),
        None,
        None,
        n(B4, 1),
        None,
        None,
        None,
        n(G4, 1),
        None,
        None,
        None,
        n(B4, 1),
        None,
        None,
    ],
    [
        None,
        n(C5, 1),
        None,
        None,
        None,
        n(E5, 1),
        None,
        None,
        None,
        n(B4, 1),
        None,
        None,
        n(D5, 1),
        None,
        None,
        None,
    ],
    [
        None,
        None,
        n(E5, 1),
        None,
        None,
        n(C5, 1),
        None,
        None,
        None,
        n(A4, 1),
        None,
        None,
        None,
        n(E5, 1),
        None,
        None,
    ],
    [
        None,
        n(D5, 1),
        None,
        None,
        None,
        n(G5, 1),
        None,
        None,
        None,
        n(D5, 1),
        None,
        None,
        n(FS4, 1),
        None,
        None,
        None,
    ],
    [
        None,
        None,
        n(G4, 1),
        None,
        n(A4, 1),
        None,
        None,
        None,
        None,
        n(E5, 1),
        None,
        None,
        None,
        n(C5, 1),
        None,
        None,
    ],
    [
        None,
        n(C5, 1),
        None,
        None,
        None,
        n(A4, 1),
        None,
        None,
        None,
        n(C5, 1),
        None,
        None,
        n(G4, 1),
        None,
        None,
        None,
    ],
    [
        None,
        None,
        n(D5, 1),
        None,
        None,
        n(B4, 1),
        None,
        None,
        None,
        n(G5, 1),
        None,
        None,
        None,
        n(D5, 1),
        None,
        None,
    ],
    [
        None,
        n(C5, 1),
        None,
        n(B4, 1),
        None,
        n(A4, 1),
        None,
        n(G4, 1),
        None,
        n(E4, 1),
        None,
        n(G4, 1),
        None,
        n(B4, 1),
        None,
        n(E5, 1),
    ],
];

const BASS: [Pattern; 8] = [
    [
        n(C2, 2),
        None,
        n(G2, 2),
        None,
        n(C3, 2),
        None,
        n(G2, 2),
        None,
        n(A2, 2),
        None,
        n(E2, 2),
        None,
        n(G2, 2),
        None,
        n(B2, 2),
        None,
    ],
    [
        n(C2, 2),
        None,
        n(G2, 2),
        None,
        n(A2, 2),
        None,
        n(E2, 2),
        None,
        n(F2, 2),
        None,
        n(C3, 2),
        None,
        n(G2, 2),
        None,
        n(D3, 2),
        None,
    ],
    [
        n(A2, 2),
        None,
        n(E2, 2),
        None,
        n(A2, 2),
        None,
        n(C3, 2),
        None,
        n(F2, 2),
        None,
        n(C3, 2),
        None,
        n(A2, 2),
        None,
        n(E2, 2),
        None,
    ],
    [
        n(G2, 2),
        None,
        n(D2, 2),
        None,
        n(G2, 2),
        None,
        n(B2, 2),
        None,
        n(D3, 2),
        None,
        n(B2, 2),
        None,
        n(G2, 2),
        None,
        n(D2, 2),
        None,
    ],
    [
        n(C2, 2),
        None,
        n(G2, 2),
        None,
        n(E2, 2),
        None,
        n(G2, 2),
        None,
        n(A2, 2),
        None,
        n(E2, 2),
        None,
        n(C3, 2),
        None,
        n(B2, 2),
        None,
    ],
    [
        n(F2, 2),
        None,
        n(C3, 2),
        None,
        n(F2, 2),
        None,
        n(A2, 2),
        None,
        n(D2, 2),
        None,
        n(A2, 2),
        None,
        n(D3, 2),
        None,
        n(C3, 2),
        None,
    ],
    [
        n(G2, 2),
        None,
        n(D3, 2),
        None,
        n(G2, 2),
        None,
        n(B2, 2),
        None,
        n(E2, 2),
        None,
        n(B2, 2),
        None,
        n(D3, 2),
        None,
        n(FS4 / 4.0, 2),
        None,
    ],
    [
        n(A2, 2),
        None,
        n(E3, 2),
        None,
        n(F2, 2),
        None,
        n(C3, 2),
        None,
        n(G2, 2),
        None,
        n(D3, 2),
        None,
        n(G2, 1),
        n(A2, 1),
        n(B2, 1),
        n(C3, 1),
    ],
];

const CHORDS: [[[f32; 3]; 4]; 8] = [
    [[C3, E3, G3], [C3, E3, G3], [A2, C3, E3], [G2, B2, D3]],
    [[C3, E3, G3], [A2, C3, E3], [F2, A2, C3], [G2, B2, D3]],
    [[A2, C3, E3], [A2, C3, E3], [F2, A2, C3], [G2, B2, D3]],
    [
        [G2, B2, D3],
        [G2, B2, D3],
        [D3, FS4 / 2.0, A3],
        [G2, B2, D3],
    ],
    [[C3, E3, G3], [E3, G3, B3], [A2, C3, E3], [A2, C3, E3]],
    [[F2, A2, C3], [F2, A2, C3], [D3, F3, A3], [G2, B2, D3]],
    [
        [G2, B2, D3],
        [G2, B2, D3],
        [E2, G2, B2],
        [D3, FS4 / 2.0, A3],
    ],
    [[A2, C3, E3], [F2, A2, C3], [G2, B2, D3], [G2, B2, D3]],
];

fn note_duration_ms(note: Note) -> u64 {
    STEP_MS * u64::from(note.steps).max(1) * 9 / 10
}

pub fn spawn_bgm_thread(mixer: Mixer, stop_flag: Arc<AtomicBool>, music_enabled: Arc<AtomicBool>) {
    thread::spawn(move || {
        let lead_player = Player::connect_new(&mixer);
        lead_player.set_volume(LEAD_VOLUME);
        let counter_player = Player::connect_new(&mixer);
        counter_player.set_volume(COUNTER_VOLUME);
        let bass_player = Player::connect_new(&mixer);
        bass_player.set_volume(BASS_VOLUME);
        let chord_player = Player::connect_new(&mixer);
        chord_player.set_volume(CHORD_VOLUME);

        let step_duration = Duration::from_millis(STEP_MS);

        'outer: loop {
            for measure_idx in 0..LEAD.len() {
                for step_idx in 0..16 {
                    if stop_flag.load(Ordering::Relaxed) {
                        break 'outer;
                    }

                    if music_enabled.load(Ordering::Relaxed) {
                        if let Some(note) = LEAD[measure_idx][step_idx] {
                            lead_player.append(square_tone_enveloped(
                                note.freq,
                                note_duration_ms(note),
                                LEAD_AMPLITUDE,
                                LEAD_ATTACK_MS,
                                LEAD_DECAY_MS,
                                LEAD_SUSTAIN,
                            ));
                        }

                        if let Some(note) = COUNTER[measure_idx][step_idx] {
                            counter_player.append(square_tone_enveloped(
                                note.freq,
                                note_duration_ms(note),
                                COUNTER_AMPLITUDE,
                                COUNTER_ATTACK_MS,
                                COUNTER_DECAY_MS,
                                COUNTER_SUSTAIN,
                            ));
                        }

                        if let Some(note) = BASS[measure_idx][step_idx] {
                            bass_player.append(triangle_tone_enveloped(
                                note.freq,
                                note_duration_ms(note),
                                BASS_AMPLITUDE,
                                BASS_ATTACK_MS,
                                BASS_DECAY_MS,
                                BASS_SUSTAIN,
                            ));
                        }

                        if step_idx % 4 == 0 {
                            chord_player.append(sine_chord(
                                &CHORDS[measure_idx][step_idx / 4],
                                FOUR_STEPS_MS * 3 / 4,
                                CHORD_AMPLITUDE,
                                CHORD_ATTACK_MS,
                                CHORD_DECAY_MS,
                                CHORD_SUSTAIN,
                            ));
                        }
                    }

                    thread::sleep(step_duration);
                }
            }
        }

        lead_player.stop();
        counter_player.stop();
        bass_player.stop();
        chord_player.stop();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_have_matching_measure_counts() {
        assert_eq!(LEAD.len(), COUNTER.len());
        assert_eq!(LEAD.len(), BASS.len());
        assert_eq!(LEAD.len(), CHORDS.len());
    }

    #[test]
    fn all_notes_have_valid_frequency_and_duration() {
        for pattern in LEAD.iter().chain(COUNTER.iter()).chain(BASS.iter()) {
            for note in pattern.iter().flatten() {
                assert!(note.freq.is_finite() && note.freq > 0.0);
                assert!((1..=4).contains(&note.steps));
                assert!(note_duration_ms(*note) <= FOUR_STEPS_MS);
            }
        }
    }

    #[test]
    fn tempo_produces_positive_step_length() {
        assert!(STEP_MS > 0);
    }
}

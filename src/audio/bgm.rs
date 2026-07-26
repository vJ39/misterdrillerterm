//! BGM: 埋め込みmp3をそのまま再生する(TERM独自拡張。#139)。以前(#131)はmp3tobeepで
//! 6トラックのノートイベントを抽出しビープ音源で再現していたが、rodioが標準で
//! (symphonia経由)mp3デコードに対応しているため、実音源をそのまま埋め込んで再生する
//! 方式に変更した。ピッチ検出特有のノイズや、複数トラックの再生キューがずれてテンポが
//! 不安定に聞こえる問題(#135)も、この方式では原理的に発生しない。
//!
//! タイトル画面用(1曲固定)とプレイ中用(複数曲を順番に交代、#145)の2系統を、
//! それぞれ独立したスレッド+`Player`で再生する(ユーザー指摘: 「/Users/work/Downloads/
//! Last_Coin_Standing.mp3 タイトル画面は、これで!」)。呼び出し側(main.rs)が
//! 画面状態に応じて双方の`music_enabled`を排他的に(同時に両方trueにならないよう)
//! 切り替えることで、常にどちらか一方だけが聞こえる。

use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use rodio::mixer::Mixer;
use rodio::{Decoder, Player};

/// BGM再生音量。SEとのバランス調整(TERM独自拡張。#147。ユーザー指摘: 「SEが
/// うるさくてBGMちいさい」)で0.35→0.55へ引き上げた(SE側は`sfx::SE_VOLUME`を
/// 0.7→0.45へ引き下げ)。
const BGM_VOLUME: f32 = 0.55;

/// タイトル画面用BGM(TERM独自拡張。#146。原曲「Last Coin Standing」)。
const TITLE_TRACK: &[u8] = include_bytes!("../../assets/bgm-title.mp3");

/// プレイ中BGM(TERM独自拡張。#145。ユーザー指摘: 「プレイ中はこの２つを交互に
/// 鳴らすことにする」「これも３曲目に入れようかな気に入ってるし」)。1曲終わる
/// たびに次の曲へ進み、最後まで行ったら先頭へ戻ってループする。いずれも
/// 192kbps前後の原曲を96kbps monoへ再エンコードして埋め込みサイズを抑えている
/// (ユーザー指摘: 「mp3データがメガ単位だから、圧縮できたりせんのかな」)。
const GAMEPLAY_TRACKS: [&[u8]; 3] = [
    include_bytes!("../../assets/bgm-token.mp3"), // The Last Token
    include_bytes!("../../assets/bgm.mp3"),       // Last Piece Dropping
    include_bytes!("../../assets/bgm-chitei.mp3"), // 地底のダンス
];

/// MUSIC設定の切り替え・曲の再生完了を確認する間隔。
const POLL_MS: u64 = 100;

/// `tracks`を順番に交代しながらループ再生するBGMスレッドを立てる。`tracks`が1曲
/// だけならその曲を単純にループする(タイトル画面用)。`restart_requested`が
/// `Some`の場合、そのフラグがtrueになったら現在の再生位置を捨てて曲の先頭
/// (プレイリストの先頭)から再生し直す(TERM独自拡張。#150。ユーザー指摘:
/// 「タイトルに戻ったら最初から再生ね」)。単純なpause/playでは一時停止した
/// 位置から再開するだけで曲の途中に戻ってしまうため、別の仕組みが要る。
fn spawn_playlist_thread(
    mixer: Mixer,
    stop_flag: Arc<AtomicBool>,
    music_enabled: Arc<AtomicBool>,
    tracks: &'static [&'static [u8]],
    restart_requested: Option<Arc<AtomicBool>>,
) {
    thread::spawn(move || {
        let player = Player::connect_new(&mixer);
        player.set_volume(BGM_VOLUME);
        let poll = Duration::from_millis(POLL_MS);
        let mut track_index = 0;

        loop {
            if stop_flag.load(Ordering::Relaxed) {
                break;
            }

            let decoder = Decoder::new(Cursor::new(tracks[track_index]))
                .expect("embedded BGM track must be a valid, bundled mp3");
            player.append(decoder);
            track_index = (track_index + 1) % tracks.len();

            'playing: while !player.empty() {
                if stop_flag.load(Ordering::Relaxed) {
                    player.stop();
                    return;
                }

                if let Some(restart) = &restart_requested
                    && restart.swap(false, Ordering::Relaxed)
                {
                    player.stop();
                    track_index = 0;
                    break 'playing;
                }

                // MUSIC設定に合わせて一時停止/再開する(#131以前のステップ
                // シーケンサー方式では「無効中は新しいノートを鳴らさない」
                // だけだったが、単一のmp3を再生する方式ではpause/playで
                // 同じ位置から止め・再開できる方が自然)。
                if music_enabled.load(Ordering::Relaxed) {
                    if player.is_paused() {
                        player.play();
                    }
                } else if !player.is_paused() {
                    player.pause();
                }

                thread::sleep(poll);
            }
        }

        player.stop();
    });
}

/// タイトル画面用BGMスレッドを立てる(TERM独自拡張。#146)。`restart_requested`は
/// タイトル画面へ戻るたびに曲を先頭から再生し直すためのフラグ(#150)。
pub fn spawn_title_bgm_thread(
    mixer: Mixer,
    stop_flag: Arc<AtomicBool>,
    music_enabled: Arc<AtomicBool>,
    restart_requested: Arc<AtomicBool>,
) {
    spawn_playlist_thread(
        mixer,
        stop_flag,
        music_enabled,
        &[TITLE_TRACK],
        Some(restart_requested),
    );
}

/// プレイ中BGMスレッドを立てる(TERM独自拡張。#145)。
pub fn spawn_gameplay_bgm_thread(
    mixer: Mixer,
    stop_flag: Arc<AtomicBool>,
    music_enabled: Arc<AtomicBool>,
) {
    spawn_playlist_thread(mixer, stop_flag, music_enabled, &GAMEPLAY_TRACKS, None);
}

/// ヘルプ画面のジュークボックスで選んで試聴できる曲の一覧(TERM独自拡張。#151。
/// ユーザー指摘: 「ヘルプページミュージック選んで再生する機能ほしい」)。
/// タイトル用・プレイ中用トラックをまとめて試聴できるようにする。
pub const JUKEBOX_TRACKS: [(&str, &[u8]); 4] = [
    ("Last Coin Standing(タイトル)", TITLE_TRACK),
    ("The Last Token", GAMEPLAY_TRACKS[0]),
    ("Last Piece Dropping", GAMEPLAY_TRACKS[1]),
    ("地底のダンス", GAMEPLAY_TRACKS[2]),
];

/// ジュークボックスで再生中の1曲を制御するハンドル(TERM独自拡張。#151)。
/// `stop`をtrueにすると次のポーリングで即座に停止する(次の曲を選んだ時・
/// ヘルプ画面を閉じた時に呼び出し側が使う)。曲が最後まで自然に終わった場合、
/// または`stop`により止めた場合のいずれも、スレッドが`finished`をtrueにする。
pub struct JukeboxPreview {
    pub stop: Arc<AtomicBool>,
    pub finished: Arc<AtomicBool>,
}

/// 選んだ1曲を1回だけ再生するプレビュースレッドを立てる(TERM独自拡張。#151)。
/// タイトル用・プレイ中用の常駐スレッドとは別に、ヘルプ画面にいる間だけ
/// 存在する使い捨てのスレッド。
pub fn spawn_jukebox_preview_thread(mixer: Mixer, track: &'static [u8]) -> JukeboxPreview {
    let stop = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread_finished = Arc::clone(&finished);

    thread::spawn(move || {
        let player = Player::connect_new(&mixer);
        player.set_volume(BGM_VOLUME);
        let decoder = Decoder::new(Cursor::new(track))
            .expect("embedded BGM track must be a valid, bundled mp3");
        player.append(decoder);

        let poll = Duration::from_millis(POLL_MS);
        while !player.empty() {
            if thread_stop.load(Ordering::Relaxed) {
                player.stop();
                break;
            }
            thread::sleep(poll);
        }
        thread_finished.store(true, Ordering::Relaxed);
    });

    JukeboxPreview { stop, finished }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_title_track_decodes_successfully() {
        Decoder::new(Cursor::new(TITLE_TRACK))
            .expect("assets/bgm-title.mp3 must be a valid, decodable mp3");
    }

    #[test]
    fn embedded_gameplay_tracks_all_decode_successfully() {
        for track in GAMEPLAY_TRACKS {
            Decoder::new(Cursor::new(track))
                .expect("each embedded gameplay BGM track must be a valid, decodable mp3");
        }
    }
}

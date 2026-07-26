//! BGM: 埋め込みmp3(`assets/bgm.mp3`、原曲「Last Piece Dropping」)をそのまま
//! ループ再生する(TERM独自拡張。#139)。以前(#131)はmp3tobeepで6トラックの
//! ノートイベントを抽出しビープ音源で再現していたが、rodioが標準で(symphonia経由)
//! mp3デコードに対応しているため、実音源をそのまま埋め込んで再生する方式に
//! 変更した。ピッチ検出特有のノイズや、複数トラックの再生キューがずれてテンポが
//! 不安定に聞こえる問題(#135)も、この方式では原理的に発生しない。

use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rodio::mixer::Mixer;
use rodio::{Decoder, Player};

const BGM_VOLUME: f32 = 0.35;

/// バイナリに埋め込んだBGM本体。192kbps stereo(約4.1MB)の原曲を96kbps monoへ
/// 再エンコードして埋め込みサイズを抑えている(ユーザー指摘: 「mp3データが
/// メガ単位だから、圧縮できたりせんのかな」)。
const BGM_MP3: &[u8] = include_bytes!("../../assets/bgm.mp3");

/// MUSIC設定の切り替え・曲の再生完了を確認する間隔。
const POLL_MS: u64 = 100;

pub fn spawn_bgm_thread(mixer: Mixer, stop_flag: Arc<AtomicBool>, music_enabled: Arc<AtomicBool>) {
    thread::spawn(move || {
        let player = Player::connect_new(&mixer);
        player.set_volume(BGM_VOLUME);
        let poll = Duration::from_millis(POLL_MS);

        loop {
            if stop_flag.load(Ordering::Relaxed) {
                break;
            }

            let decoder = Decoder::new(Cursor::new(BGM_MP3)).expect("assets/bgm.mp3 must be a valid, bundled mp3");
            player.append(decoder);

            while !player.empty() {
                if stop_flag.load(Ordering::Relaxed) {
                    player.stop();
                    return;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_bgm_mp3_decodes_successfully() {
        Decoder::new(Cursor::new(BGM_MP3)).expect("assets/bgm.mp3 must be a valid, decodable mp3");
    }
}

//! `src/main.rs`から責務ごとに切り出したアプリ層のロジック(#258〜#262のgame/配下分割に
//! 続く、main.rs自体の分割の段階a)。
//!
//! `audio`(BGM実効状態判定・イベント→SE変換)と、既存の低レベル音声再生実装である
//! `crate::audio`(`mod audio;`、main.rsから直接参照)は別物なので混同しないこと。

pub mod audio;
pub mod settings_menu;

//! 起動時のスプラッシュ画面(AAピクセルアート表示)。
//!
//! `bombermanterm`(`src/render/intro.rs`)の手法を踏襲する: 静止画像を
//! ビルド時に `include_bytes!` でバイナリへ埋め込み、実行時に外部ファイルへ
//! 依存せずデコード→端末表示用の解像度へリサイズ→[`PixelCanvas`]へ展開し、
//! 起動直後に一度だけ表示して何らかのキー入力で閉じる。
//!
//! 元画像(`assets/intro.png`)はRGBAで実際に透過(アルファ)を持たせた画像
//! なので、bombermantermの「ほぼ白の画素を背景とみなす」しきい値判定ではなく、
//! アルファ値そのもので背景(透過)かどうかを判定する。

use image::imageops::FilterType;
use image::{GenericImageView, Rgba};
use ratatui::style::Color;

use super::colors;
use super::pixel_canvas::PixelCanvas;

/// 埋め込み画像(`assets/intro.png`)。
const INTRO_IMAGE_BYTES: &[u8] = include_bytes!("../../assets/intro.png");

/// 表示解像度(論理ピクセル、長辺基準)。元画像は縦長(1024x1536)なので、
/// 長辺(高さ)をこの値にリサイズする。
///
/// この値と`render::TITLE_ART_SCALE`は対で決める(TERM独自拡張。#124。ユーザー
/// 指摘: 「スプラッシュもっと解像度高く」)。以前は64にリサイズした後で
/// `TITLE_ART_SCALE`(0.75)により最近傍サンプリングでさらに間引いていたが、
/// この二段階目は`image`クレートのフィルタを経ない粗い間引きのため画質が
/// 損なわれる。そこで`TITLE_ART_SCALE`を1.0にし、最終的に欲しい表示行数ぶんの
/// 解像度をここで直接指定する(=フィルタ付きリサイズ1回で完結させる)。
///
/// 60→80へさらに引き上げた(#127。ユーザー指摘: 「もっとくっきり見えるように
/// ならん？」。60でも大きな色ブロックのモザイクにしか見えなかった)。
///
/// 80→26へ縮小(TERM独自拡張。#129。ユーザー指摘: 「スプラッシュの構成を黄金比に」
/// →アート:ロゴ/案内文の高さ比を黄金比(約1.618:1)にする方向で確認済み)。
/// アート解像度を優先してきた#124/#127の方針とは逆行するが、構成比を優先する
/// というユーザーの明示的な選択による(art_rows=13、text_rows=8として
/// 13/8≈1.625で黄金比に近づける)。
const DISPLAY_SIZE: u32 = 26;

/// アルファ値がこれ未満の画素は透過(背景)として扱う。
const ALPHA_THRESHOLD: u8 = 128;

/// 起動時スプラッシュ用のピクセルキャンバスを組み立てる。
/// 呼び出しごとに画像のデコード・リサイズを行う(起動時に1回しか呼ばないため
/// キャッシュは設けていない)。
pub fn build_canvas() -> PixelCanvas {
    let background = colors::LETTERBOX_BG;

    let decoded = image::load_from_memory(INTRO_IMAGE_BYTES)
        .expect("assets/intro.png must be a valid, bundled PNG");
    let (src_w, src_h) = decoded.dimensions();
    let (target_w, target_h) = if src_w >= src_h {
        (DISPLAY_SIZE, DISPLAY_SIZE * src_h / src_w.max(1))
    } else {
        (DISPLAY_SIZE * src_w / src_h.max(1), DISPLAY_SIZE)
    };
    // Lanczos3はTriangle(双線形)より輪郭・コントラストの保持に優れ、この規模の
    // 縮小(1024x1536→数十px)でも模様が潰れにくい(TERM独自拡張。#127)。
    let resized = decoded
        .resize_exact(target_w.max(1), target_h.max(1), FilterType::Lanczos3)
        .to_rgba8();

    let mut canvas = PixelCanvas::new(
        resized.width() as usize,
        resized.height() as usize,
        background,
    );
    for (x, y, Rgba([r, g, b, a])) in resized.enumerate_pixels() {
        if *a < ALPHA_THRESHOLD {
            continue; // 透過(背景)はキャンバスの下地色のまま残す。
        }
        canvas.set(x as usize, y as usize, Color::Rgb(*r, *g, *b));
    }

    canvas
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_canvas_produces_non_empty_output() {
        let canvas = build_canvas();
        let lines = canvas.to_lines(1.0);
        assert!(!lines.is_empty());
    }
}

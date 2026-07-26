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
const DISPLAY_SIZE: u32 = 64;

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
    let resized = decoded
        .resize_exact(target_w.max(1), target_h.max(1), FilterType::Triangle)
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

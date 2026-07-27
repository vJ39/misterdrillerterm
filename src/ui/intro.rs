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

/// アルファ値がこれ未満の画素は透過(背景)として扱う。
const ALPHA_THRESHOLD: u8 = 128;

/// 起動時スプラッシュ用のピクセルキャンバスを、端末セル`target_cols`×`target_rows`
/// ぶんの画面全体を覆うサイズで組み立てる(TERM独自拡張。#148。ユーザー提案:
/// 「フルスクリーンにAAいっぱいにして題字を挿入したらよくね?」)。
///
/// 以前(#129)はアート:案内文の高さ比を黄金比にする方針だったため、画面全体を
/// 使わずアート自体の解像度を絞る必要があり、結果としてアートが低解像度で
/// 潰れて見える問題があった。ロゴ・案内文をアートの上に重ね描きする方式に
/// 変えたことで、アートは画面全体をそのまま使えるようになった。
///
/// 1端末セル=横1論理ピクセル・縦2論理ピクセル(ハーフブロック疑似2倍解像度、
/// `pixel_canvas.rs`参照)なので、画面いっぱいに隙間なく表示するには
/// `target_cols`×`target_rows*2`論理ピクセル分の画像が要る。元画像のアスペクト比
/// を保ったまま画面全体を覆う(CSSの`background-size: cover`と同じ考え方)よう、
/// 縦横どちらかがぴったり収まるまで拡大してから、はみ出した分を中央基準で
/// 切り出す(引き伸ばして歪めることはしない)。
pub fn build_canvas(target_cols: u16, target_rows: u16) -> PixelCanvas {
    // タイトル画面専用の白背景(TERM独自拡張。#191。ユーザー指摘: 「タイトル画面の
    // 背景白色って可能？」)。元画像は透過(アルファ)背景なので、この色をそのまま
    // 透過部分の下地として使うだけで白背景化できる。
    let background = colors::TITLE_BG;

    let decoded = image::load_from_memory(INTRO_IMAGE_BYTES)
        .expect("assets/intro.png must be a valid, bundled PNG");
    let (src_w, src_h) = decoded.dimensions();

    let target_w = (target_cols as u32).max(1);
    let target_h = (target_rows as u32).max(1) * 2;

    let scale = (target_w as f32 / src_w.max(1) as f32).max(target_h as f32 / src_h.max(1) as f32);
    let resized_w = ((src_w as f32) * scale).ceil().max(target_w as f32) as u32;
    let resized_h = ((src_h as f32) * scale).ceil().max(target_h as f32) as u32;

    // Lanczos3はTriangle(双線形)より輪郭・コントラストの保持に優れ、大きな縮小率
    // でも模様が潰れにくい(TERM独自拡張。#127)。
    let resized = decoded
        .resize_exact(resized_w, resized_h, FilterType::Lanczos3)
        .to_rgba8();

    let crop_x = (resized_w - target_w) / 2;
    let crop_y = (resized_h - target_h) / 2;
    let cropped =
        image::imageops::crop_imm(&resized, crop_x, crop_y, target_w, target_h).to_image();

    let mut canvas = PixelCanvas::new(target_w as usize, target_h as usize, background);
    for (x, y, Rgba([r, g, b, a])) in cropped.enumerate_pixels() {
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
        let canvas = build_canvas(80, 24);
        let lines = canvas.to_lines(1.0);
        assert!(!lines.is_empty());
    }

    #[test]
    fn build_canvas_fills_the_exact_requested_terminal_size() {
        // #148: アートは画面いっぱいに表示する(端末セル数と1:1で一致するはず)。
        let canvas = build_canvas(100, 40);
        let lines = canvas.to_lines(1.0);
        assert_eq!(lines.len(), 40, "行数は要求したtarget_rowsと一致するはず");
        assert_eq!(
            lines[0].spans.len(),
            100,
            "幅は要求したtarget_colsと一致するはず"
        );
    }

    #[test]
    fn build_canvas_handles_a_tiny_terminal_without_panicking() {
        let canvas = build_canvas(1, 1);
        let lines = canvas.to_lines(1.0);
        assert!(!lines.is_empty());
    }
}

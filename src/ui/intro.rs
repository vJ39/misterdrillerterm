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

/// containとcover、どちらの拡大率で合成するかのブレンド係数(0.0=contain
/// 相当・全身は入るが横長端末では左右の余白が大きい、1.0=cover相当・画面は
/// 埋まるが縦長の元画像では上下を大きく切り落とす。TERM独自拡張。#201
/// フォローアップ、#127。ユーザー指摘:「ここまでアップじゃないとこまで、
/// スプラッシュだけ戻したい」「バカでかすぎw フルサイズであってほしいけど」)。
/// 両案を実際にレンダリングして比較した結果、containとcoverの中間(往復の
/// 4割ほどcover寄り)がバランス良いと確認した上でこの値にした
/// (ユーザー確認:「中間案でいいと思う」)。
const SPLASH_ZOOM_BLEND: f32 = 0.4;

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
/// `pixel_canvas.rs`参照)。containの拡大率(欠けずに収まるが左右の余白が
/// 大きくなりうる)とcoverの拡大率(画面は埋まるが縦長の元画像では上下を
/// 大きく切り落とす)を`SPLASH_ZOOM_BLEND`で混ぜ合わせ、全身が見えつつ画面も
/// そこそこ埋まる中間の拡大率にする(TERM独自拡張。#201フォローアップ)。
/// はみ出した分は中央基準で切り落とし、余った分はキャンバスの下地色のまま
/// 残す(引き伸ばして歪めることはしない)。
pub fn build_canvas(target_cols: u16, target_rows: u16) -> PixelCanvas {
    let background = colors::LETTERBOX_BG;

    let decoded = image::load_from_memory(INTRO_IMAGE_BYTES)
        .expect("assets/intro.png must be a valid, bundled PNG");
    let (src_w, src_h) = decoded.dimensions();

    let target_w = (target_cols as u32).max(1);
    let target_h = (target_rows as u32).max(1) * 2;

    let scale_w = target_w as f32 / src_w.max(1) as f32;
    let scale_h = target_h as f32 / src_h.max(1) as f32;
    let contain_scale = scale_w.min(scale_h);
    let cover_scale = scale_w.max(scale_h);
    let scale = contain_scale + SPLASH_ZOOM_BLEND * (cover_scale - contain_scale);
    let resized_w = ((src_w as f32) * scale).round().max(1.0) as u32;
    let resized_h = ((src_h as f32) * scale).round().max(1.0) as u32;

    // Lanczos3はTriangle(双線形)より輪郭・コントラストの保持に優れ、大きな縮小率
    // でも模様が潰れにくい(TERM独自拡張。#127)。
    let resized = decoded
        .resize_exact(resized_w, resized_h, FilterType::Lanczos3)
        .to_rgba8();

    // ブレンドしたスケールでは、縦横どちらかがtargetより大きくなりうる(はみ出した
    // 分は切り落とす)し、どちらかがtargetより小さくなりうる(余った分は下地色の
    // まま残す)。オフセットを符号付きで扱い、キャンバス範囲外の画素は単純に
    // 描かないことで、切り落とし・余白のどちらも同じロジックで処理する。
    let offset_x = (target_w as i64 - resized_w as i64) / 2;
    let offset_y = (target_h as i64 - resized_h as i64) / 2;

    let mut canvas = PixelCanvas::new(target_w as usize, target_h as usize, background);
    for (x, y, Rgba([r, g, b, a])) in resized.enumerate_pixels() {
        if *a < ALPHA_THRESHOLD {
            continue; // 透過(背景)はキャンバスの下地色のまま残す。
        }
        let cx = x as i64 + offset_x;
        let cy = y as i64 + offset_y;
        if cx < 0 || cy < 0 || cx >= target_w as i64 || cy >= target_h as i64 {
            continue; // 画面外にはみ出した分は描かない。
        }
        canvas.set(cx as usize, cy as usize, Color::Rgb(*r, *g, *b));
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

//! 論理ピクセルグリッド + ハーフブロック疑似2倍解像度変換。
//!
//! `bombermanterm`(`src/render/pixel_canvas.rs`)の手法を踏襲する: 論理ピクセルを
//! 縦2つ1組にし、上半分ブロック文字 `▀`(U+2580, fg=上ピクセル色, bg=下ピクセル色)で
//! 1端末セルにまとめて描画する。端末フォントは縦長なため、この方式で見た目が
//! ほぼ正方形のピクセルになる。`scale` は最近傍サンプリングによる任意倍率(f32)。
//!
//! このリポジトリでは起動時スプラッシュ(`ui::intro`)専用に使うため、
//! bombermantermにあったスプライト合成(`blit_sprite`)は移植していない。

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// これより小さい倍率は指定できない(0や負数だと出力ピクセル数が0になり得るため)。
const MIN_SCALE: f32 = 0.05;

/// 論理ピクセルグリッド。原点は左上、(x, y) は (列, 行)。
pub struct PixelCanvas {
    width: usize,
    height: usize,
    pixels: Vec<Color>,
}

impl PixelCanvas {
    pub fn new(width: usize, height: usize, background: Color) -> Self {
        PixelCanvas {
            width,
            height,
            pixels: vec![background; width * height],
        }
    }

    fn index(&self, x: usize, y: usize) -> Option<usize> {
        if x < self.width && y < self.height {
            Some(y * self.width + x)
        } else {
            None
        }
    }

    /// 範囲内であれば指定座標を塗る。範囲外は無視する(呼び出し側での境界チェックを省ける)。
    pub fn set(&mut self, x: usize, y: usize, color: Color) {
        if let Some(idx) = self.index(x, y) {
            self.pixels[idx] = color;
        }
    }

    fn get(&self, x: usize, y: usize) -> Color {
        match self.index(x, y) {
            Some(idx) => self.pixels[idx],
            None => Color::Reset,
        }
    }

    /// `scale` 倍(1論理ピクセル→約scale文字分)に最近傍サンプリングしつつ、
    /// ハーフブロック文字でratatuiの `Line` 列に変換する。`scale` が1.0未満なら
    /// 縮小(間引き)になる。
    pub fn to_lines(&self, scale: f32) -> Vec<Line<'static>> {
        let scale = scale.max(MIN_SCALE);
        let scaled_width = ((self.width as f32) * scale).round().max(1.0) as usize;
        let scaled_height = ((self.height as f32) * scale).round().max(1.0) as usize;
        let max_x = self.width.saturating_sub(1);
        let max_y = self.height.saturating_sub(1);

        let mut lines = Vec::with_capacity(scaled_height.div_ceil(2));
        let mut row = 0;
        while row < scaled_height {
            let top_logical_y = (((row as f32) / scale) as usize).min(max_y);
            let bottom_logical_y = ((((row + 1) as f32) / scale) as usize).min(max_y);

            let mut spans = Vec::with_capacity(scaled_width);
            for col in 0..scaled_width {
                let logical_x = (((col as f32) / scale) as usize).min(max_x);
                let top = self.get(logical_x, top_logical_y);
                let bottom = if row + 1 < scaled_height {
                    self.get(logical_x, bottom_logical_y)
                } else {
                    // 高さが奇数の場合、最終行は上ピクセルのみを使い下段は同色で埋める。
                    top
                };
                spans.push(Span::styled("▀", Style::default().fg(top).bg(bottom)));
            }
            lines.push(Line::from(spans));
            row += 2;
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_fills_with_background_color() {
        let canvas = PixelCanvas::new(4, 4, Color::Rgb(1, 2, 3));
        assert_eq!(canvas.get(0, 0), Color::Rgb(1, 2, 3));
        assert_eq!(canvas.get(3, 3), Color::Rgb(1, 2, 3));
    }

    #[test]
    fn set_out_of_bounds_is_ignored_not_panicking() {
        let mut canvas = PixelCanvas::new(2, 2, Color::Black);
        canvas.set(100, 100, Color::White);
        assert_eq!(canvas.get(0, 0), Color::Black);
    }

    #[test]
    fn to_lines_pairs_rows_into_half_block_lines() {
        // 高さ2の単色キャンバスは1行のハーフブロック行にまとまるはず。
        let canvas = PixelCanvas::new(2, 2, Color::Rgb(9, 9, 9));
        let lines = canvas.to_lines(1.0);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn to_lines_handles_odd_height_by_reusing_top_pixel_for_the_last_row() {
        let canvas = PixelCanvas::new(1, 3, Color::Rgb(5, 5, 5));
        let lines = canvas.to_lines(1.0);
        // height=3 -> ceil(3/2) = 2 行。
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn to_lines_scale_multiplies_output_dimensions() {
        let canvas = PixelCanvas::new(2, 2, Color::Rgb(1, 1, 1));
        let lines = canvas.to_lines(3.0);
        // 幅方向: (2*3) = 6 スパン、高さ方向: ceil(2*3/2) = 3 行。
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].spans.len(), 6);
    }

    #[test]
    fn to_lines_scale_below_one_shrinks_output_dimensions() {
        // 20x20を0.5倍に縮小すると、幅は10スパン、高さはceil(10/2)=5行になる。
        let canvas = PixelCanvas::new(20, 20, Color::Rgb(2, 2, 2));
        let lines = canvas.to_lines(0.5);
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0].spans.len(), 10);
    }

    #[test]
    fn to_lines_clamps_scale_below_the_minimum() {
        // 極端に小さい倍率を渡しても、出力が0にならず必ず1行以上残る。
        let canvas = PixelCanvas::new(50, 50, Color::Rgb(3, 3, 3));
        let lines = canvas.to_lines(0.0);
        assert!(!lines.is_empty());
        assert!(!lines[0].spans.is_empty());
    }
}

//! カラーパレット(spec.md 9章「セル表示の対応」)。
//! 全てトゥルーカラー(`ratatui::style::Color::Rgb`)でパステル調に統一する。

use ratatui::style::Color;

use crate::game::board::{Cell, ColorKind};

pub const COLOR_RED_BG: Color = Color::Rgb(255, 179, 179);
pub const COLOR_BLUE_BG: Color = Color::Rgb(179, 204, 255);
pub const COLOR_GREEN_BG: Color = Color::Rgb(186, 255, 201);
pub const COLOR_YELLOW_BG: Color = Color::Rgb(255, 245, 179);
pub const COLOR_PURPLE_BG: Color = Color::Rgb(224, 179, 255);
pub const ROCK_BG: Color = Color::Rgb(150, 150, 150);
pub const OXYGEN_BG: Color = Color::Rgb(179, 240, 255);
pub const DIAMOND_BG: Color = Color::Rgb(255, 255, 255);

/// フィールド背景(未掘削の空洞部分)。
pub const FIELD_EMPTY_BG: Color = Color::Rgb(20, 20, 30);

/// プレイヤーの前景色。
pub const PLAYER_FG: Color = Color::Rgb(255, 255, 0);

/// 色ブロックの種別ごとの背景色。
pub fn color_kind_bg(kind: ColorKind) -> Color {
    match kind {
        ColorKind::Red => COLOR_RED_BG,
        ColorKind::Blue => COLOR_BLUE_BG,
        ColorKind::Green => COLOR_GREEN_BG,
        ColorKind::Yellow => COLOR_YELLOW_BG,
        ColorKind::Purple => COLOR_PURPLE_BG,
    }
}

/// セルの背景色。Emptyは`FIELD_EMPTY_BG`を返す。
pub fn cell_bg(cell: Cell) -> Color {
    match cell {
        Cell::Empty => FIELD_EMPTY_BG,
        Cell::Color(kind) => color_kind_bg(kind),
        Cell::Rock => ROCK_BG,
        Cell::Oxygen => OXYGEN_BG,
        Cell::Diamond => DIAMOND_BG,
    }
}

/// セルの表示グリフ(2文字幅)。
pub fn cell_glyph(cell: Cell) -> &'static str {
    match cell {
        Cell::Empty => "  ",
        Cell::Color(_) => "██",
        Cell::Rock => "▓▓",
        Cell::Oxygen => "▲▲",
        Cell::Diamond => "◆◆",
    }
}

//! カラーパレット(spec.md 9.6 配色設計)。
//!
//! 全てトゥルーカラー(`ratatui::style::Color::Rgb`)で指定する。旧版のパステル調
//! パレットは廃止し、初代寄りのビビッドな原色コントラストへ作り直す。

use ratatui::style::Color;

use crate::game::board::ColorKind;

/// 色ブロック1色ぶんの3段階トーン(spec.md 9.3・9.6)。
#[derive(Debug, Clone, Copy)]
pub struct ColorTriple {
    /// 縦の連なりの中間段。
    pub base: Color,
    /// 縦の連なりの最上段(明るい色調)。罫線(枠)の前景色にも常用する。
    pub highlight: Color,
    /// 縦の連なりの最下段(暗い色調)。
    pub shadow: Color,
}

pub const RED: ColorTriple = ColorTriple {
    base: Color::Rgb(220, 40, 40),
    highlight: Color::Rgb(255, 120, 110),
    shadow: Color::Rgb(150, 20, 20),
};
pub const BLUE: ColorTriple = ColorTriple {
    base: Color::Rgb(40, 90, 220),
    highlight: Color::Rgb(110, 170, 255),
    shadow: Color::Rgb(20, 55, 150),
};
pub const GREEN: ColorTriple = ColorTriple {
    base: Color::Rgb(30, 170, 70),
    highlight: Color::Rgb(120, 235, 140),
    shadow: Color::Rgb(15, 110, 45),
};
pub const YELLOW: ColorTriple = ColorTriple {
    base: Color::Rgb(230, 190, 20),
    highlight: Color::Rgb(255, 230, 100),
    shadow: Color::Rgb(160, 125, 10),
};

/// `ColorKind`に対応する3段階トーンを引く。
pub fn triple(kind: ColorKind) -> ColorTriple {
    match kind {
        ColorKind::Red => RED,
        ColorKind::Blue => BLUE,
        ColorKind::Green => GREEN,
        ColorKind::Yellow => YELLOW,
    }
}

/// 縦の連なりにおける自セルの位置(spec.md 9.3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shading {
    /// 縦の連なりの最上段。上下とも非同色の孤立1マスもここに含む(上端優先)。
    Highlight,
    /// 縦の連なりの中間段。
    Base,
    /// 縦の連なりの最下段。
    Shadow,
}

/// 上下が同色かどうかからシェーディング段階を決める(spec.md 9.3、描画専用の判定)。
pub fn shade(up_same: bool, down_same: bool) -> Shading {
    if !up_same {
        Shading::Highlight
    } else if !down_same {
        Shading::Shadow
    } else {
        Shading::Base
    }
}

/// 色ブロックの塗り色(fill/罫線の背景色に使う。spec.md 9.3)。
pub fn shaded_color(kind: ColorKind, shading: Shading) -> Color {
    let t = triple(kind);
    match shading {
        Shading::Highlight => t.highlight,
        Shading::Base => t.base,
        Shading::Shadow => t.shadow,
    }
}

/// 罫線(角・辺)の前景色。常にそのセルのHIGHLIGHT色を使う(spec.md 9.3)。
pub fn highlight_color(kind: ColorKind) -> Color {
    triple(kind).highlight
}

/// フィールド背景(未掘削の空洞部分。spec.md 9.6「暗い青灰色」)。
pub const FIELD_EMPTY_BG: Color = Color::Rgb(28, 35, 47);

/// レターボックス余白(spec.md 9.2・9.8)。
pub const LETTERBOX_BG: Color = Color::Rgb(10, 12, 16);

/// 岩ブロック(ヒット0、無傷)の背景色。
pub const ROCK_BG_INTACT: Color = Color::Rgb(110, 70, 40);
/// 岩ブロック(ヒット4、あと1発で破壊)の背景色。
pub const ROCK_BG_CRACKED: Color = Color::Rgb(175, 125, 80);
/// 岩ブロックのXマーク/ヒビの前景色。
pub const ROCK_X_FG: Color = Color::Rgb(240, 225, 200);

pub const OXYGEN_BG: Color = Color::Rgb(20, 190, 200);
pub const OXYGEN_FG: Color = Color::Rgb(255, 255, 255);

pub const DIAMOND_BG: Color = Color::Rgb(200, 225, 235);
pub const DIAMOND_FG: Color = Color::Rgb(255, 255, 255);

/// プレイヤーの前景色。
pub const PLAYER_FG: Color = Color::Rgb(255, 170, 40);

/// 押し潰されて「潰れた」演出中のプレイヤーの前景色(TERM独自拡張、9章)。
pub const CRUSH_FLASH_FG: Color = Color::Rgb(255, 60, 60);

/// パネル枠線(フィールド/HUD双方の罫線に使う)。
pub const PANEL_BORDER: Color = Color::Rgb(90, 90, 100);
/// パネル文字色。
pub const PANEL_TEXT: Color = Color::Rgb(230, 230, 230);

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8
}

/// 岩ブロックの背景色を残りヒット数から補間する(spec.md 9.4・9.6、`hits / 4.0`で線形補間)。
pub fn rock_bg(hits: u8) -> Color {
    let t = (hits as f32 / 4.0).clamp(0.0, 1.0);
    let Color::Rgb(ar, ag, ab) = ROCK_BG_INTACT else {
        unreachable!("ROCK_BG_INTACTは常にColor::Rgb")
    };
    let Color::Rgb(br, bg, bb) = ROCK_BG_CRACKED else {
        unreachable!("ROCK_BG_CRACKEDは常にColor::Rgb")
    };
    Color::Rgb(lerp_u8(ar, br, t), lerp_u8(ag, bg, t), lerp_u8(ab, bb, t))
}

/// 酸素ゲージのバー色(spec.md 9.6、残量比率0.0〜1.0)。
pub fn oxygen_bar_color(ratio: f32) -> Color {
    if ratio >= 0.6 {
        Color::Rgb(60, 200, 90) // 緑
    } else if ratio >= 0.3 {
        Color::Rgb(230, 190, 30) // 黄
    } else {
        Color::Rgb(220, 50, 50) // 赤
    }
}

//! カラーパレット(spec.md 9.6 配色設計)。
//!
//! 全てトゥルーカラー(`ratatui::style::Color::Rgb`)で指定する。旧版のパステル調
//! パレットは廃止し、初代寄りのビビッドな原色コントラストへ作り直す。

use ratatui::style::Color;

use crate::game::board::ColorKind;

/// 色ブロック1色ぶんの配色(TERM独自拡張)。
///
/// 以前は縦の連なりの位置(最上段/中間/最下段)で塗り色を明→暗の3段階に変えていたが、
/// 「結合ブロックがどこまで同じ色か見分けづらい」というユーザー指摘(「色だけね、
/// 濃い色薄い色ってのが。ややこしい」)を受け、塗り色(`base`)は常に単一で固定する。
/// 罫線(枠)の前景色だけは引き続き`highlight`を使う。
#[derive(Debug, Clone, Copy)]
pub struct ColorTriple {
    /// 塗り色(fillの背景色に使う)。
    pub base: Color,
    /// 罫線(枠)の前景色。
    pub highlight: Color,
}

pub const RED: ColorTriple = ColorTriple {
    base: Color::Rgb(220, 40, 40),
    highlight: Color::Rgb(255, 120, 110),
};
pub const BLUE: ColorTriple = ColorTriple {
    base: Color::Rgb(40, 90, 220),
    highlight: Color::Rgb(110, 170, 255),
};
pub const GREEN: ColorTriple = ColorTriple {
    base: Color::Rgb(30, 170, 70),
    highlight: Color::Rgb(120, 235, 140),
};
pub const YELLOW: ColorTriple = ColorTriple {
    base: Color::Rgb(230, 190, 20),
    highlight: Color::Rgb(255, 230, 100),
};

/// `ColorKind`に対応する配色を引く。
pub fn triple(kind: ColorKind) -> ColorTriple {
    match kind {
        ColorKind::Red => RED,
        ColorKind::Blue => BLUE,
        ColorKind::Green => GREEN,
        ColorKind::Yellow => YELLOW,
    }
}

/// 色ブロックの塗り色(fillの背景色に使う)。連結位置によらず常に単一の色を返す。
pub fn fill_color(kind: ColorKind) -> Color {
    triple(kind).base
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

/// スターブロックの背景色(無傷時)。
pub const STAR_BG: Color = Color::Rgb(230, 200, 40);
/// スターブロックの前景色。
pub const STAR_FG: Color = Color::Rgb(255, 250, 210);

/// 白ブロック(結合しないブロック、TERM独自拡張)の背景色。
pub const WHITE_BG: Color = Color::Rgb(235, 235, 235);
/// 白ブロックの前景色(罫線)。白背景に対してコントラストが出るよう暗めのグレーにする。
pub const WHITE_FG: Color = Color::Rgb(90, 90, 90);

/// スターブロックの背景色を溶解の進行度(`melting / STAR_MELT_TICKS`)から、
/// フィールド背景色(`FIELD_EMPTY_BG`)へ向けて補間する。
pub fn star_bg(melting: u8, melt_ticks: u8) -> Color {
    let t = if melt_ticks == 0 {
        0.0
    } else {
        (melting as f32 / melt_ticks as f32).clamp(0.0, 1.0)
    };
    let Color::Rgb(ar, ag, ab) = STAR_BG else {
        unreachable!("STAR_BGは常にColor::Rgb")
    };
    let Color::Rgb(br, bg, bb) = FIELD_EMPTY_BG else {
        unreachable!("FIELD_EMPTY_BGは常にColor::Rgb")
    };
    Color::Rgb(lerp_u8(ar, br, t), lerp_u8(ag, bg, t), lerp_u8(ab, bb, t))
}

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

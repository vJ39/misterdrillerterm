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
    base: Color::Rgb(220, 80, 80),
    highlight: Color::Rgb(255, 120, 110),
};
pub const BLUE: ColorTriple = ColorTriple {
    base: Color::Rgb(80, 120, 220),
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

/// ダイヤブロックはXブロック(岩ブロック)系統の代物という位置づけ(TERM独自拡張。
/// ユーザー指摘: 「ダイヤブロックはXブロック系の代物なので、色味は白じゃなくて
/// 黄土色がいい」)のため、白系ではなく黄土色系にする。
pub const DIAMOND_BG: Color = Color::Rgb(196, 149, 60);
pub const DIAMOND_FG: Color = Color::Rgb(255, 244, 214);

/// スターブロックの背景色(無傷時、TERM独自拡張。ユーザー指摘: 「スターブロックって、
/// 白い見た目の独立したブロックだよ」)。
pub const STAR_BG: Color = Color::Rgb(245, 245, 245);
/// スターブロックの前景色(☆/★マークの色)。
pub const STAR_FG: Color = Color::Rgb(255, 210, 60);

/// アイテムブロック(ClearAbove、ショートカットRと同じ効果)の背景・前景色
/// (TERM独自拡張)。対応するデバッグショートカットの文字をそのままグリフに使う。
pub const ITEM_CLEAR_ABOVE_BG: Color = Color::Rgb(200, 60, 140);
pub const ITEM_CLEAR_ABOVE_FG: Color = Color::Rgb(255, 230, 245);

/// アイテムブロック(UnifyColors、ショートカットCと同じ効果)の背景・前景色
/// (TERM独自拡張)。
pub const ITEM_UNIFY_COLORS_BG: Color = Color::Rgb(50, 160, 170);
pub const ITEM_UNIFY_COLORS_FG: Color = Color::Rgb(230, 255, 250);

/// アイテムブロック(StarifyScreen、ショートカットKと同じ効果)の背景・前景色
/// (TERM独自拡張。ユーザー指摘: 「ショートカットKアイテムつくって」)。スターブロック
/// (`STAR_FG`)を思わせる金色系にする。
pub const ITEM_STARIFY_SCREEN_BG: Color = Color::Rgb(80, 70, 130);
pub const ITEM_STARIFY_SCREEN_FG: Color = Color::Rgb(255, 215, 120);

/// ボムの丸い本体の色(TERM独自拡張。#96/#125/#130。ユーザー指摘: 「ボムは、丸い
/// 「いかにもな」爆弾の形状しておいてほしい」)。#130で「背景と同化してる」という
/// 指摘を受け、本体セル自体を塗りつぶす色として使うよう変更した(以前はグリフの
/// 前景色のみに使い、周囲は透過してフィールド背景色のまま=輪郭が薄かった)。
pub const BOMB_BODY_FG: Color = Color::Rgb(20, 20, 24);
/// ボム本体の縁取り・ハイライトの色(TERM独自拡張。#130。ユーザー指摘: 「爆弾、
/// 背景と同化してるから、もっと輪郭くっきり」)。本体色(ほぼ黒)よりはっきり明るい
/// 銀灰色にし、フィールド背景色との対比よりも本体そのものとの対比で輪郭を出す。
pub const BOMB_RIM_FG: Color = Color::Rgb(205, 210, 220);
/// 起爆間際に本体が激しく点滅する警告色(TERM独自拡張。#138。ユーザー指摘:
/// 「爆発直前で爆弾がチカチカ激しく赤く光るようにして」)。
pub const BOMB_BODY_DANGER_FG: Color = Color::Rgb(220, 30, 30);
/// 導火線の火花の色(点滅の暗い方)。
pub const BOMB_SPARK_DIM: Color = Color::Rgb(200, 120, 40);
/// 導火線の火花の色(点滅の明るい方)。起爆が近づくほど点滅を速める(既存の「揺れ」
/// 「スター点滅」と同じく、爆発前に必ず視覚的な予兆を出す設計方針)。
pub const BOMB_SPARK_BRIGHT: Color = Color::Rgb(255, 220, 60);

/// 白ボン(TERM独自拡張。#123)の前景色。名前の通り白系にする。
pub const SHIROBON_FG: Color = Color::Rgb(240, 240, 240);

/// ボム爆発時の炎アニメーション(TERM独自拡張。#126。ユーザー指摘: 「爆弾が爆発する
/// ときは、ボンバーマンTERMのように炎アニメーションほしい」)。bombermantermの爆風
/// スプライトに倣い、爆心地に近いほど白熱、遠いほど赤黒くなる3段階の色調にする。
pub const EXPLOSION_FLAME_CORE: Color = Color::Rgb(255, 250, 220);
pub const EXPLOSION_FLAME_MID: Color = Color::Rgb(255, 170, 40);
pub const EXPLOSION_FLAME_OUTER: Color = Color::Rgb(230, 60, 10);

/// 炎の色が白熱(CORE)から赤黒(OUTER)へ移り変わりきる距離(マス数、TERM独自拡張。
/// #142)。以前は「tier0=CORE・tier1=MID・tier2以降は全てOUTER固定」という3段階の
/// 決め打ちだったが、爆風範囲が画面全域まで拡大された(#142)ことで、その大部分が
/// tier2以降=同じ赤一色になってしまい「白熱の炎をちゃんと表示して」というユーザー
/// 指摘を招いた。実際の爆風到達距離(縦横で大きさが異なる`BOMB_BLAST_ROW_RANGE`/
/// `BOMB_BLAST_COL_RANGE`)には依存させず、見た目のグラデーションが自然な範囲で
/// 白熱から赤黒まで滑らかに移り変わるようにし、それより遠いセルはOUTERで飽和させる。
const EXPLOSION_FLAME_GRADIENT_TIER_MAX: u8 = 6;

fn lerp_rgb(a: Color, b: Color, t: f32) -> Color {
    let Color::Rgb(ar, ag, ab) = a else {
        unreachable!("色は常にColor::Rgb")
    };
    let Color::Rgb(br, bg, bb) = b else {
        unreachable!("色は常にColor::Rgb")
    };
    Color::Rgb(lerp_u8(ar, br, t), lerp_u8(ag, bg, t), lerp_u8(ab, bb, t))
}

/// 爆風が届いたセルの炎の色を、爆心地からの距離(`tier`: 0=爆心地、数値が大きいほど
/// 遠い)と演出の進捗(0.0=爆発直後、1.0=演出完了直前)から求める。距離に応じて
/// CORE→MID→OUTERへ滑らかに変化させ(`EXPLOSION_FLAME_GRADIENT_TIER_MAX`。#142)、
/// 進捗が進むにつれ爆発後に実際に変化する先の見た目(`fade_to`、TERM独自拡張。#137。
/// 以前はスターブロックの地色`STAR_BG`固定だったが、色ブロックの一色統一(#137)の
/// ようにスター化以外の結果もあり得るため、呼び出し側がそのセルの実際の変化先の色を
/// 渡すようにした)へ補間し、炎から本来の輝きへ自然に移り変わるようにする。
pub fn explosion_flame_bg(tier: u8, progress: f32, fade_to: Color) -> Color {
    let frac = (tier as f32 / EXPLOSION_FLAME_GRADIENT_TIER_MAX as f32).clamp(0.0, 1.0);
    let base = if frac <= 0.5 {
        lerp_rgb(EXPLOSION_FLAME_CORE, EXPLOSION_FLAME_MID, frac / 0.5)
    } else {
        lerp_rgb(
            EXPLOSION_FLAME_MID,
            EXPLOSION_FLAME_OUTER,
            (frac - 0.5) / 0.5,
        )
    };
    lerp_rgb(base, fade_to, progress.clamp(0.0, 1.0))
}

/// スターブロックの背景色を溶解の進行度から、フィールド背景色(`FIELD_EMPTY_BG`)へ
/// 向けて補間する。`visible_ms`が`grace_ms`に達するまでは無傷(進行度0)のまま、
/// その後`melt_duration_ms`かけて進行度が1.0(消滅)まで進む。
pub fn star_bg(visible_ms: u32, grace_ms: u32, melt_duration_ms: u32) -> Color {
    let melt_elapsed = visible_ms.saturating_sub(grace_ms);
    let t = if melt_duration_ms == 0 {
        0.0
    } else {
        (melt_elapsed as f32 / melt_duration_ms as f32).clamp(0.0, 1.0)
    };
    let Color::Rgb(ar, ag, ab) = STAR_BG else {
        unreachable!("STAR_BGは常にColor::Rgb")
    };
    let Color::Rgb(br, bg, bb) = FIELD_EMPTY_BG else {
        unreachable!("FIELD_EMPTY_BGは常にColor::Rgb")
    };
    Color::Rgb(lerp_u8(ar, br, t), lerp_u8(ag, bg, t), lerp_u8(ab, bb, t))
}

/// ブロックが消滅した瞬間の明るいフラッシュ色(TERM独自拡張。ユーザー指摘:
/// 「ブロックが消える瞬間に消える演出してほしい」)。
const VANISH_FLASH_BG: Color = Color::Rgb(255, 255, 255);

/// 消滅フラッシュ演出の背景色を進捗(0.0=消滅直後、1.0=演出完了直前)から、
/// フィールド背景色(`FIELD_EMPTY_BG`)へ向けて補間する。
pub fn vanish_flash_bg(progress: f32) -> Color {
    let t = progress.clamp(0.0, 1.0);
    let Color::Rgb(ar, ag, ab) = VANISH_FLASH_BG else {
        unreachable!("VANISH_FLASH_BGは常にColor::Rgb")
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
    (a as f32 + (b as f32 - a as f32) * t)
        .round()
        .clamp(0.0, 255.0) as u8
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

#[cfg(test)]
mod explosion_flame_tests {
    use super::*;

    #[test]
    fn tier_zero_at_the_start_of_the_flash_is_the_core_color() {
        assert_eq!(
            explosion_flame_bg(0, 0.0, Color::Rgb(0, 0, 0)),
            EXPLOSION_FLAME_CORE
        );
    }

    #[test]
    fn tier_at_the_gradient_max_at_the_start_of_the_flash_is_the_outer_color() {
        assert_eq!(
            explosion_flame_bg(EXPLOSION_FLAME_GRADIENT_TIER_MAX, 0.0, Color::Rgb(0, 0, 0)),
            EXPLOSION_FLAME_OUTER
        );
    }

    #[test]
    fn tier_far_beyond_the_gradient_max_saturates_at_the_outer_color() {
        // 爆風範囲が画面全域(最大20マス超)まで拡大されても(#142)、遠いセルは
        // OUTERで飽和するだけで、CORE/MIDへ巻き戻ったりしないはず。
        assert_eq!(
            explosion_flame_bg(200, 0.0, Color::Rgb(0, 0, 0)),
            EXPLOSION_FLAME_OUTER
        );
    }

    #[test]
    fn a_middling_tier_is_different_from_both_core_and_outer() {
        // tier=4(EXPLOSION_FLAME_GRADIENT_TIER_MAX=6の後半区間)は、CORE/OUTER
        // どちらとも異なる中間色になるはず(急に赤一色へ飽和せず滑らかに変化する)。
        let mid = explosion_flame_bg(4, 0.0, Color::Rgb(0, 0, 0));
        assert_ne!(
            mid, EXPLOSION_FLAME_CORE,
            "中間tierはCOREそのものではないはず"
        );
        assert_ne!(
            mid, EXPLOSION_FLAME_OUTER,
            "中間tierはOUTERそのものではないはず"
        );
    }

    #[test]
    fn progress_at_one_fully_reaches_the_fade_to_color() {
        let fade_to = Color::Rgb(10, 20, 30);
        assert_eq!(explosion_flame_bg(0, 1.0, fade_to), fade_to);
    }
}

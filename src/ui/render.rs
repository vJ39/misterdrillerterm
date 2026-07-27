//! ratatui描画(spec.md 9章 TUI仕様)。
//!
//! 1論理セルを横4文字×縦2ターミナル行の大型ブロックとして描画する(9.2)。
//! 旧版のhalf-block方式(1論理セルを1文字に圧縮)は完全に廃止した。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{
    Alignment, Constraint, Direction as LayoutDirection, Layout, Position, Rect,
};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::constants::{
    BOMB_DANGER_MS, BOMB_ROLL_MS, BONUS_FLOOR_DEPTH_M, CHECKPOINT_SAFE_ZONE_M, CHECKPOINT_STEP_M,
    OXYGEN_MAX, STAR_MELT_DURATION_MS, STAR_SPARKLE_PERIOD_MS, STAR_VISIBLE_GRACE_MS,
    TITLE_PROMPT_BLINK_MS,
};
use crate::game::board::{Board, Cell as BoardCell, ColorKind, ItemEffect, Pos};
use crate::game::player::Direction;
use crate::game::{BombPhase, Game, GameOverChoice, GameStatus};
use crate::ui::colors;

use super::intro;

// ---------------------------------------------------------------------------
// 9.1・9.2・9.8 画面サイズ関連の定数
// ---------------------------------------------------------------------------

/// 固定フレームの目安サイズ(9.1)。
const TOTAL_SCREEN_W: u16 = 74;
const TOTAL_SCREEN_H: u16 = 32;

/// ステータスパネル幅(9.1・9.7)。
const HUD_PANE_W: u16 = 24;
/// 縮退表示時のHUDペイン最小幅(9.8)。
const HUD_PANE_W_MIN: u16 = 16;

/// これを下回ったら警告メッセージのみ表示する(9.8)。
const MIN_TERMINAL_W: u16 = 50;
const MIN_TERMINAL_H: u16 = 16;

/// 可視論理行数の基本値(9.2)。
const FIELD_VISIBLE_ROWS: usize = 14;

/// 1論理セルの文字グリッドサイズ(9.2)。
const CELL_W: u16 = 4;
const CELL_H: u16 = 2;

/// プレイヤーを画面内の何行目(可視行数に対する比率)に固定表示するか(9.1)。
const PLAYER_SCREEN_ROW_RATIO_NUM: usize = 1;
const PLAYER_SCREEN_ROW_RATIO_DEN: usize = 3;

// ---------------------------------------------------------------------------
// レイアウト計算(9.1・9.8・9.10)
// ---------------------------------------------------------------------------

/// 1フレームぶんのレイアウト計算結果。
struct LayoutPlan {
    /// フィールドの罫線ボックス(内部に12×可視行数セルを描画する)。
    field_rect: Rect,
    /// ステータスパネルの罫線ボックス。
    hud_rect: Rect,
    /// 可視論理行数。
    visible_rows: usize,
    /// オーバーレイ(ポーズ/ゲームオーバー等)を中央配置する基準となる、
    /// ゲーム画面全体のフレーム(9.10「旧`centered_rect`はオーバーレイ専用として残す」)。
    game_frame: Rect,
}

/// フィールドペイン幅(列数×4文字+左右ボーダー2文字、9.2)。列数(TERM独自拡張。
/// ユーザー指摘: 「設定値に列の数を変更できるようにして」)に応じて可変になる。
fn field_pane_w(field_width: usize) -> u16 {
    field_width as u16 * CELL_W + 2
}

/// フレーム全体の幅(フィールドペイン+HUDペイン)。列数によって可変になる。
fn total_screen_w(field_width: usize) -> u16 {
    field_pane_w(field_width) + HUD_PANE_W
}

fn compute_layout(area: Rect, field_width: usize) -> LayoutPlan {
    let total_w = total_screen_w(field_width);
    let field_pane_w = field_pane_w(field_width);

    if area.width >= total_w && area.height >= TOTAL_SCREEN_H {
        let frame_rect = centered_fixed_rect(total_w, TOTAL_SCREEN_H, area);

        let cols = Layout::default()
            .direction(LayoutDirection::Horizontal)
            .constraints([
                Constraint::Length(field_pane_w),
                Constraint::Length(HUD_PANE_W),
            ])
            .split(frame_rect);
        let field_col = cols[0];
        let hud_rect = cols[1];

        // フィールドの罫線ボックス自体は 可視行数×2+上下ボーダー2行 の高さしか使わない。
        // field_col(32行)との差分は上下に均等な余白(LETTERBOX_BG、9.2)として残す。
        let field_box_h = FIELD_VISIBLE_ROWS as u16 * CELL_H + 2;
        let margin = field_col.height.saturating_sub(field_box_h) / 2;
        let field_rect = Rect {
            x: field_col.x,
            y: field_col.y + margin,
            width: field_col.width,
            height: field_box_h,
        };

        LayoutPlan {
            field_rect,
            hud_rect,
            visible_rows: FIELD_VISIBLE_ROWS,
            game_frame: frame_rect,
        }
    } else {
        // 縮退表示(9.8): セルサイズ(4×2)は変えず、可視行数とHUD幅だけを縮める。
        let field_width_px = field_pane_w.min(area.width);
        let field_rect = Rect {
            x: area.x,
            y: area.y,
            width: field_width_px,
            height: area.height,
        };
        let hud_width = area
            .width
            .saturating_sub(field_width_px)
            .max(HUD_PANE_W_MIN);
        let hud_rect = Rect {
            x: area.x + field_width_px,
            y: area.y,
            width: hud_width,
            height: area.height,
        };
        let visible_rows = ((area.height.saturating_sub(2)) / CELL_H).max(4) as usize;

        LayoutPlan {
            field_rect,
            hud_rect,
            visible_rows,
            game_frame: area,
        }
    }
}

/// `width`×`height`の固定サイズ矩形を`area`の中央に配置する(9.10)。
/// `area`より大きい場合は`area`いっぱいにクランプする。
fn centered_fixed_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// ポーズ/ゲームオーバー等のオーバーレイ専用の中央配置(9.10、パーセント指定)。
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(LayoutDirection::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(LayoutDirection::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

// ---------------------------------------------------------------------------
// エントリポイント
// ---------------------------------------------------------------------------

pub fn draw(frame: &mut Frame, game: &Game, music_enabled: bool, se_enabled: bool) {
    let area = frame.area();

    // 9.6実装上の注意: まずフレーム全体を明示的な背景色で塗りつぶしてから、その上に
    // ゲーム画面を重ねる(ターミナルのデフォルト背景色が縁に残ることを防ぐ)。
    frame.buffer_mut().set_style(
        area,
        Style::default()
            .fg(colors::LETTERBOX_BG)
            .bg(colors::LETTERBOX_BG),
    );

    if area.width < MIN_TERMINAL_W || area.height < MIN_TERMINAL_H {
        draw_size_warning(frame, area);
        return;
    }

    let plan = compute_layout(area, game.board.width());
    draw_field(frame, plan.field_rect, plan.visible_rows, game);
    draw_status(frame, plan.hud_rect, game);

    // チェックポイント(100mごと)到達演出(TERM独自拡張。#178)。短時間のバナー表示
    // だけで、盤面(draw_field)自体は裏で通常通り動き続けている。
    if let Some(depth_m) = game.checkpoint_flash_depth_m() {
        draw_checkpoint_banner(frame, plan.game_frame, depth_m);
    }

    match game.status {
        GameStatus::Paused => draw_overlay(
            frame,
            plan.game_frame,
            "PAUSED",
            &[
                "何かキーを押すと再開 / Qキーでタイトルへ",
                &format!(
                    "Mキーで音楽{} / Eキーで効果音{}",
                    on_off_label(music_enabled),
                    on_off_label(se_enabled)
                ),
                "Sキーで設定画面 / Hキーでヘルプ",
            ],
        ),
        // 押し潰されてのミスは、GameOverオーバーレイを出す前に一呼吸「潰れた」演出
        // (draw_field内のdraw_player)を見せる(spec.md 5章・9章TERM独自拡張)。
        GameStatus::GameOver if !game.crush_flash_active() => {
            draw_game_over_overlay(frame, plan.game_frame, game.game_over_selection())
        }
        GameStatus::GameOver => {}
        GameStatus::Cleared => {
            draw_overlay(frame, plan.game_frame, "CLEAR !", &["Qキーでタイトルへ"])
        }
        GameStatus::Playing => {}
    }
}

/// ON/OFF状態を短い日本語ラベルにする(TERM独自拡張、10章)。
fn on_off_label(enabled: bool) -> &'static str {
    if enabled { "ON" } else { "OFF" }
}

// ---------------------------------------------------------------------------
// タイトル画面(spec.md 1章「Qキーはタイトルへ戻る」の受け皿)
// ---------------------------------------------------------------------------

/// タイトル画面のアートは端末サイズいっぱいに表示する(TERM独自拡張。#148。
/// ユーザー提案: 「フルスクリーンにAAいっぱいにして題字を挿入したらよくね?」)。
/// 以前(#129)はアート:案内文の高さ比を黄金比にする方針だったため、アートの
/// 表示行数を絞る必要があり、その結果アートが低解像度で潰れて見える問題が
/// あった(#127で解像度を上げた直後に#129で再び縮小した経緯)。ロゴ・案内文を
/// アートと同じ領域(画面全体)へ上から重ね描きする方式に変えたことで、この
/// トレードオフ自体を解消している。
///
/// アートの構築(PNGデコード+Lanczos3リサイズ)は軽くない処理のため、端末サイズが
/// 変わらない限り再利用するキャッシュを`title_art_lines`に持たせている
/// (`draw_title`はタイトル画面にいる間、毎フレーム=約33msごとに呼ばれるため)。
type TitleArtCache = Option<((u16, u16), Vec<Line<'static>>)>;

fn title_art_lines(cols: u16, rows: u16) -> Vec<Line<'static>> {
    thread_local! {
        static CACHE: RefCell<TitleArtCache> = const { RefCell::new(None) };
    }
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((size, lines)) = cache.as_ref()
            && *size == (cols, rows)
        {
            return lines.clone();
        }
        let canvas = intro::build_canvas(cols, rows);
        let lines = canvas.to_lines(1.0);
        *cache = Some(((cols, rows), lines.clone()));
        lines
    })
}

/// タイトルワードマーク("MISDRI TERM")を構成する1文字ぶんの罫線フォント
/// (3行×3列、TERM独自拡張。ユーザー指摘: 「TERMMAPみたいにかっこいい題字
/// つくってくれ」)。T/E/R/Mは`vJ39/termmap`のワードマーク(`keymap.rs`の`LOGO`)と
/// 同じ字形をそのまま流用し、I/S/Dは同じ作法(角・ヒゲの罫線文字)で新規に起こした。
/// 半角スペースは2列ぶんの空白で単語の区切りに使う。
fn title_logo_glyph(c: char) -> &'static [&'static str; 3] {
    match c {
        'M' => &["┏┳┓", "┃┃┃", "╹╹╹"],
        'I' => &["╺┳╸", " ┃ ", "╺┻╸"],
        'S' => &["┏━╸", "┗━┓", "╺━┛"],
        // 単純な箱形("┏━┓"/"┃ ┃"/"┗━┛")だとOと見分けがつかなかった(ユーザー指摘の
        // スクショで実際に「MISORI TERM」に見えていた)ため、右側だけ丸角(細線)にして
        // 左の角ばった縦棒(太線)とのコントラストでDの丸みを出す。
        'D' => &["┏━╮", "┃ │", "┗━╯"],
        'R' => &["┏━┓", "┣┳┛", "╹┗╸"],
        'T' => &["╺┳╸", " ┃ ", " ╹ "],
        'E' => &["┏━╸", "┣╸ ", "┗━╸"],
        _ => &["  ", "  ", "  "],
    }
}

/// "MISDRI TERM"のワードマーク3行を、上ほど明るい金〜赤銅色のグラデーションで組む
/// (termmapの緑グラデーションと同じ発想。ゲーム内のダイヤブロック配色(黄土色系、#62)
/// に寄せた色にした)。この明るいグラデーションは黒背景で映える配色のため、
/// `draw_title`側でワードマーク部分だけ専用の黒地(LETTERBOX_BG)を敷いている
/// (#191フォローアップ。ユーザー提案:「矩形だけ黒背景にするのはありかな」)。
fn build_title_logo_lines() -> [Line<'static>; 3] {
    const GRADIENT: [Color; 3] = [
        Color::Rgb(255, 210, 90),
        Color::Rgb(225, 145, 55),
        Color::Rgb(165, 85, 35),
    ];
    let mut rows = [String::new(), String::new(), String::new()];
    for c in "MISDRI TERM".chars() {
        let glyph = title_logo_glyph(c);
        for (row, text) in rows.iter_mut().zip(glyph.iter()) {
            row.push_str(text);
        }
    }
    let [row0, row1, row2] = rows;
    [
        Line::from(Span::styled(
            row0,
            Style::default().fg(GRADIENT[0]).bg(colors::LETTERBOX_BG),
        )),
        Line::from(Span::styled(
            row1,
            Style::default().fg(GRADIENT[1]).bg(colors::LETTERBOX_BG),
        )),
        Line::from(Span::styled(
            row2,
            Style::default().fg(GRADIENT[2]).bg(colors::LETTERBOX_BG),
        )),
    ]
}

/// タイトル画面を描画する(起動時スプラッシュ画像+ゲーム名+スタート案内を
/// 1画面にまとめる)。このタイトル画面上でのみ、Qキーがアプリ終了として扱われる
/// (main.rsの画面遷移)。
pub fn draw_title(frame: &mut Frame) {
    let area = frame.area();

    // タイトル画面は白背景にしている(TERM独自拡張。#191。ユーザー指摘: 「タイトル
    // 画面の背景白色って可能？」)。他の画面(設定・ヘルプ・一時停止オーバーレイ等)は
    // 引き続きLETTERBOX_BG(ダーク)のままで、この画面専用の配色。
    frame.buffer_mut().set_style(
        area,
        Style::default().fg(colors::TITLE_BG).bg(colors::TITLE_BG),
    );

    // アートと文字は上下に分割し、重ねない(TERM独自拡張。#191フォローアップ)。
    // #148ではアートを画面いっぱいに表示し文字を上に重ね描きしていたが、文字パネル
    // がキャラクターの上に重なって隠してしまっていた(ユーザー指摘: 「スプラッシュ
    // があまりに隠れすぎるのはよくない」)。アート専用ゾーンを黄金比で確保し、文字と
    // 重ならないようにした(#129と同じ考え方。ユーザー指摘: 「それでいて黄金比で」)。
    let art_height = ((area.height as u32 * 618) / 1000).max(1) as u16;
    let art_zone = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: art_height.min(area.height),
    };
    let text_zone = Rect {
        x: area.x,
        y: area.y + art_zone.height,
        width: area.width,
        height: area.height.saturating_sub(art_zone.height),
    };

    // 元画像(assets/intro.png、1024x1536の縦長)は`title_art_lines`が
    // `background-size: cover`と同じ考え方で塗りつぶすため、art_zoneの幅を
    // そのまま渡すと横幅に対して縦が足りず、頭や足がクロップされて見切れて
    // しまう(TERM独自拡張。#191フォローアップ。ユーザー指摘: 「見切れちゃってる」)。
    // art_zoneの高さから元画像と同じアスペクト比になる幅を逆算し、その幅だけを
    // 中央に取ることでキャラクター全身が収まるようにした(左右は白背景のまま)。
    const INTRO_ART_ASPECT_W_OVER_H: f32 = 1024.0 / 1536.0;
    let art_height_px = art_zone.height as f32 * 2.0; // 1セル=縦2論理ピクセル分。
    let art_width =
        ((art_height_px * INTRO_ART_ASPECT_W_OVER_H).round() as u16).clamp(1, art_zone.width);
    let art_area = Rect {
        x: art_zone.x + (art_zone.width - art_width) / 2,
        y: art_zone.y,
        width: art_width,
        height: art_zone.height,
    };

    let art_lines = title_art_lines(art_area.width, art_area.height);
    frame.render_widget(
        Paragraph::new(Text::from(art_lines)).alignment(Alignment::Center),
        art_area,
    );

    draw_title_text(frame, text_zone);
}

/// タイトル画面下部(アートと重ならない黄金比の残り領域)にロゴ・案内文を描く。
fn draw_title_text(frame: &mut Frame, text_zone: Rect) {
    let logo_lines = build_title_logo_lines().to_vec();
    let logo_rows = logo_lines.len() as u16;
    let logo_width = logo_lines
        .iter()
        .map(|line| line.width())
        .max()
        .unwrap_or(0) as u16;

    // ロゴ・(明滅する)スタート案内・キーヒントをまとめて1ブロックとしてtext_zoneの
    // 中央に配置する(TERM独自拡張。#191フォローアップ)。
    const GAP_ABOVE_PROMPT: u16 = 1;
    const PROMPT_ROWS: u16 = 1;
    const GAP_ABOVE_HINTS: u16 = 2;
    const HINT_ROWS: u16 = 2;
    let content_height = logo_rows + GAP_ABOVE_PROMPT + PROMPT_ROWS + GAP_ABOVE_HINTS + HINT_ROWS;
    let content_area = centered_fixed_rect(text_zone.width, content_height, text_zone);

    // ワードマーク("MISDRI TERM")は黒背景向けの明るい金色グラデーションのため、
    // 専用の黒地(LETTERBOX_BG)の上に乗せる(#191フォローアップ。ユーザー提案:
    // 「矩形だけ黒背景にするのはありかな」)。text_zoneは常に白地(呼び出し元で
    // 塗り済み)なので、ロゴの矩形以外は追加の下地塗りなしでそのまま読める。
    let logo_box_width = (logo_width + 4).min(content_area.width);
    let logo_area = Rect {
        x: content_area.x + (content_area.width - logo_box_width) / 2,
        y: content_area.y,
        width: logo_box_width,
        height: logo_rows,
    };
    let solid_black = Style::default()
        .fg(colors::LETTERBOX_BG)
        .bg(colors::LETTERBOX_BG);
    frame.buffer_mut().set_style(logo_area, solid_black);
    frame.render_widget(
        Paragraph::new(logo_lines)
            .style(solid_black)
            .alignment(Alignment::Center),
        logo_area,
    );

    // 案内文は常時表示の大きな枠でごちゃっと出すのではなく、「Enterキーを押して
    // スタート」だけを明滅させるシンプルな構成にした(TERM独自拡張。#191フォロー
    // アップ。ユーザー指摘: 「わりとごちゃっとしてるから、PRESS ENTER KEY START的
    // な文言をチカチカ点滅させとけばよさげ」)。明滅中でもレイアウトが動かないよう、
    // 非表示の間も行の位置自体は固定しておく。
    let text_style = Style::default().fg(colors::TITLE_TEXT).bg(colors::TITLE_BG);
    let prompt_y = content_area.y + logo_rows + GAP_ABOVE_PROMPT;
    if title_prompt_blink_on() {
        let prompt_area = Rect {
            x: content_area.x,
            y: prompt_y,
            width: content_area.width,
            height: PROMPT_ROWS,
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Enterキーを押してスタート",
                text_style,
            )))
            .alignment(Alignment::Center),
            prompt_area,
        );
    }

    let hint_lines = vec![
        Line::from(Span::styled("(Qキーで終了)", text_style)),
        Line::from(Span::styled("(Sキーで設定 / Hキーでヘルプ)", text_style)),
    ];
    let hints_area = Rect {
        x: content_area.x,
        y: prompt_y + PROMPT_ROWS + GAP_ABOVE_HINTS,
        width: content_area.width,
        height: HINT_ROWS,
    };
    frame.render_widget(
        Paragraph::new(hint_lines).alignment(Alignment::Center),
        hints_area,
    );
}

/// タイトル画面の「Enterキーを押してスタート」を明滅させるon/off判定。`draw_title`
/// はゲーム開始前(ゲーム内時刻がまだ存在しない)に毎フレーム呼ばれるため、壁時計
/// (`SystemTime`)を直接使う(TERM独自拡張。#191フォローアップ)。
fn title_prompt_blink_on() -> bool {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    (millis / TITLE_PROMPT_BLINK_MS as u128).is_multiple_of(2)
}

// ---------------------------------------------------------------------------
// ヘルプ画面(TERM独自拡張。ユーザー指摘: 「ショートカットのヘルプページも必要」)
// ---------------------------------------------------------------------------

/// ヘルプ画面のジュークボックスUI状態(TERM独自拡張。#151。ユーザー指摘:
/// 「ヘルプページミュージック選んで再生する機能ほしい」)。カーソル位置
/// (`selection`)と現在再生中の曲(`playing`、無ければ`None`)を保持する。
pub struct HelpJukeboxState {
    pub selection: usize,
    pub playing: Option<usize>,
}

/// 操作キー・デバッグショートカット一覧を表示するヘルプ画面。`jukebox`が
/// `Some`の時のみ、埋め込みBGMを選んで試聴できるジュークボックス欄を表示する
/// (TERM独自拡張。#151)。一時停止中のヘルプオーバーレイでは実際のプレイ中BGMと
/// 混ざってしまうため対象外にし、タイトルから開く独立画面のみで有効にする。
/// `standalone`はタイトルから開いた独立画面(true、Qでタイトルへ戻る)か、プレイ中の
/// 一時停止オーバーレイ(false、Qはオーバーレイを閉じてプレイ再開するだけ)かを表す
/// (TERM独自拡張。#155。以前は文脈を問わず「Qキーでタイトルへ戻る」と表示しており、
/// 一時停止オーバーレイ表示中は実際の挙動と食い違っていた)。
pub fn draw_help(frame: &mut Frame, jukebox: Option<&HelpJukeboxState>, standalone: bool) {
    let area = frame.area();

    frame.buffer_mut().set_style(
        area,
        Style::default()
            .fg(colors::LETTERBOX_BG)
            .bg(colors::LETTERBOX_BG),
    );

    let frame_rect = centered_fixed_rect(TOTAL_SCREEN_W, TOTAL_SCREEN_H, area);
    let help_area = centered_rect(90, 90, frame_rect);
    frame.render_widget(Clear, help_area);

    let text_style = Style::default()
        .fg(colors::PANEL_TEXT)
        .bg(colors::LETTERBOX_BG);
    let heading_style = Style::default()
        .fg(colors::PANEL_BORDER)
        .bg(colors::LETTERBOX_BG);
    let selected_style = Style::default()
        .fg(colors::STAR_FG)
        .bg(colors::LETTERBOX_BG);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(colors::PANEL_BORDER)
                .bg(colors::LETTERBOX_BG),
        )
        .style(Style::default().bg(colors::LETTERBOX_BG));

    let line = |text: &str| Line::from(Span::styled(text.to_string(), text_style));
    let heading = |text: &str| Line::from(Span::styled(text.to_string(), heading_style));

    let mut lines = vec![
        heading("== 操作 =="),
        line("←/→: 移動(掘削なし)        ↑/↓: 向きを変える(移動なし)"),
        line("X/Z: 掘削(向いている方向)   Space/P: 一時停止"),
        line("Q: タイトルへ戻る/終了"),
        line("S: 設定画面   H: このヘルプ (プレイ中に押すと自動で一時停止する)"),
        line(""),
        heading("== 一時停止中のみ =="),
        line("M: MUSIC ON/OFF   E: SE ON/OFF"),
        line("設定画面: MUSIC/SE/Xブロック・AIR・スター・ダイヤの配分・色数を調整できる"),
        line(""),
        heading("== デバッグショートカット =="),
        line("C: 周辺ブロックを2色に統一   L: ライフ+1   A: AIRを100%に回復"),
        line("R: 自分より上のブロックを全削除   K: 画面内のX/ダイヤを全てスターに"),
        line("B: ボムを画面内のランダムな位置に設置"),
        line("[ / ]: ブロック落下速度 遅く/速く"),
        line("- / =: 自分の落下速度 遅く/速く"),
        line(", / .: 揺れ時間 長く/短く"),
    ];

    if let Some(jukebox) = jukebox {
        lines.push(Line::from(""));
        lines.push(heading("== ジュークボックス(↑/↓で選択、X/Zで再生/停止) =="));
        for (i, (name, _)) in crate::audio::bgm::JUKEBOX_TRACKS.iter().enumerate() {
            let marker = if jukebox.playing == Some(i) {
                "▶ "
            } else if jukebox.selection == i {
                "> "
            } else {
                "  "
            };
            let style = if jukebox.selection == i {
                selected_style
            } else {
                text_style
            };
            lines.push(Line::from(Span::styled(format!("{marker}{name}"), style)));
        }
    }

    lines.push(Line::from(""));
    lines.push(line(if standalone {
        "Qキーでタイトルへ戻る"
    } else {
        "Qキーで閉じてプレイに戻る"
    }));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .style(Style::default().bg(colors::LETTERBOX_BG))
        .alignment(Alignment::Left);
    frame.render_widget(paragraph, help_area);
}

// ---------------------------------------------------------------------------
// 設定画面(TERM独自拡張。ユーザー指摘: 「サウンドON/OFFではなくMUSIC/SEを
// それぞれトグルできるように。設定画面つくって、カーソルで選んでスペースで
// トグルできるように」)
// ---------------------------------------------------------------------------

/// 設定画面での選択項目。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsChoice {
    Music,
    Se,
    /// Xブロック(岩)の出現率(%)。TERM独自拡張。ユーザー指摘: 「設定でXブロックの
    /// 配分量・AIRの配分量をいじれるようにしたい」
    RockRate,
    /// AIR(酸素カプセル)の出現率(%)。TERM独自拡張。
    AirRate,
    /// スターブロックの出現率(%、0まで下げられる)。TERM独自拡張。
    /// ユーザー指摘: 「スターブロック比率0〜」
    StarRate,
    /// ダイヤブロックの出現率(%、0まで下げられる)。TERM独自拡張。
    /// ユーザー指摘: 「ダイヤブロック0%設定」
    DiamondRate,
    /// アイテムブロック(ClearAbove、ショートカットR効果)の出現率(%、0まで下げられる)。
    /// TERM独自拡張。ユーザー指摘: 「各種アイテムの出現頻度の設定項目増やして」
    ItemClearAboveRate,
    /// アイテムブロック(UnifyColors、ショートカットC効果)の出現率(%、同上)。
    ItemUnifyColorsRate,
    /// アイテムブロック(StarifyScreen、ショートカットK効果)の出現率(%、同上)。
    ItemStarifyScreenRate,
    /// 出現する色ブロックの色数(1〜4)。TERM独自拡張。ユーザー指摘: 「出現する色
    /// ブロックの色数を設定で選べるようにしたい(1〜4)」
    ColorCount,
    /// 色ブロックの結合しやすさ(%、0まで下げられる)。TERM独自拡張。
    /// ユーザー指摘: 「ブロック配置の結合関係の割合を設定できるようにして」
    ColorClusterRate,
    /// フィールド幅(列数)。TERM独自拡張。ユーザー指摘: 「設定値に列の数を変更
    /// できるようにして」。新規ゲーム開始時にのみ反映される。
    FieldWidth,
    /// ブロック落下速度(tick間隔, ms)。TERM独自拡張。従来はデバッグショートカット
    /// ([ ])でのみ調整可能だったが、ユーザー指摘: 「ブロックが落ちるスピードの
    /// 設定値がないよね」を受け、設定画面からも調整できるようにした。
    BlockFallSpeed,
    /// キャラ自身の自由落下速度(tick間隔, ms)。TERM独自拡張。従来はデバッグ
    /// ショートカット(-/=)でのみ調整可能だったが、ユーザー指摘: 「設定画面から、
    /// キャラに関する落下などの設定がなくなってる」を受け、設定画面からも
    /// 調整できるようにした。
    PlayerFallSpeed,
    /// 横移動(MoveLeft/MoveRight)のクールダウン間隔(ms、小さいほど速い)。TERM独自
    /// 拡張。ユーザー指摘: 「横移動のスピードを設定で変えられるように」。
    MoveSpeed,
    /// 「わ〜!」スライダー演出後、キャラが起き上がるまでの硬直インターバル(ms)。
    /// TERM独自拡張。ユーザー指摘: 「この設定値も作る」。
    DodgeRecoveryMs,
    /// ボム出現頻度(%、0まで下げられる)。TERM独自拡張。#96。
    BombRate,
    /// #85調査用のブロック状態遷移ログ(SQLite)を記録するかどうか(TERM独自拡張。
    /// #167。ユーザー指摘: 「デバッグ用のDB記録するしないトグル設定に追加」)。
    DebugLogEnabled,
    /// 4連結以上の自動消滅が連鎖するときのインターバル(ms、0=従来通り即座に連鎖)。
    /// TERM独自拡張。#187。ユーザー指摘: 「ブロックが消えて、連鎖的に次ブロックが
    /// 消えるとき、0msで連続するのではなく一定のインターバルで連鎖するように」。
    ChainVanishInterval,
}

impl SettingsChoice {
    /// ↓キーでの選択項目の巡回(TERM独自拡張)。
    pub fn cycle(self) -> Self {
        match self {
            SettingsChoice::Music => SettingsChoice::Se,
            SettingsChoice::Se => SettingsChoice::RockRate,
            SettingsChoice::RockRate => SettingsChoice::AirRate,
            SettingsChoice::AirRate => SettingsChoice::StarRate,
            SettingsChoice::StarRate => SettingsChoice::DiamondRate,
            SettingsChoice::DiamondRate => SettingsChoice::ItemClearAboveRate,
            SettingsChoice::ItemClearAboveRate => SettingsChoice::ItemUnifyColorsRate,
            SettingsChoice::ItemUnifyColorsRate => SettingsChoice::ItemStarifyScreenRate,
            SettingsChoice::ItemStarifyScreenRate => SettingsChoice::ColorCount,
            SettingsChoice::ColorCount => SettingsChoice::ColorClusterRate,
            SettingsChoice::ColorClusterRate => SettingsChoice::FieldWidth,
            SettingsChoice::FieldWidth => SettingsChoice::BlockFallSpeed,
            SettingsChoice::BlockFallSpeed => SettingsChoice::PlayerFallSpeed,
            SettingsChoice::PlayerFallSpeed => SettingsChoice::MoveSpeed,
            SettingsChoice::MoveSpeed => SettingsChoice::DodgeRecoveryMs,
            SettingsChoice::DodgeRecoveryMs => SettingsChoice::BombRate,
            SettingsChoice::BombRate => SettingsChoice::DebugLogEnabled,
            SettingsChoice::DebugLogEnabled => SettingsChoice::ChainVanishInterval,
            SettingsChoice::ChainVanishInterval => SettingsChoice::Music,
        }
    }

    /// ↑キーでの選択項目の巡回(`cycle`の逆方向、TERM独自拡張)。ユーザー指摘:
    /// 「設定画面でカーソル↑おしても下いくんやけど」を受け、FaceUp/FaceDownで
    /// 同じ`cycle`を呼んでいた(常に同じ向きにしか進めなかった)バグを修正するために追加した。
    pub fn cycle_back(self) -> Self {
        match self {
            SettingsChoice::Music => SettingsChoice::ChainVanishInterval,
            SettingsChoice::ChainVanishInterval => SettingsChoice::DebugLogEnabled,
            SettingsChoice::DebugLogEnabled => SettingsChoice::BombRate,
            SettingsChoice::BombRate => SettingsChoice::DodgeRecoveryMs,
            SettingsChoice::Se => SettingsChoice::Music,
            SettingsChoice::RockRate => SettingsChoice::Se,
            SettingsChoice::AirRate => SettingsChoice::RockRate,
            SettingsChoice::StarRate => SettingsChoice::AirRate,
            SettingsChoice::DiamondRate => SettingsChoice::StarRate,
            SettingsChoice::ItemClearAboveRate => SettingsChoice::DiamondRate,
            SettingsChoice::ItemUnifyColorsRate => SettingsChoice::ItemClearAboveRate,
            SettingsChoice::ItemStarifyScreenRate => SettingsChoice::ItemUnifyColorsRate,
            SettingsChoice::ColorCount => SettingsChoice::ItemStarifyScreenRate,
            SettingsChoice::ColorClusterRate => SettingsChoice::ColorCount,
            SettingsChoice::FieldWidth => SettingsChoice::ColorClusterRate,
            SettingsChoice::BlockFallSpeed => SettingsChoice::FieldWidth,
            SettingsChoice::PlayerFallSpeed => SettingsChoice::BlockFallSpeed,
            SettingsChoice::MoveSpeed => SettingsChoice::PlayerFallSpeed,
            SettingsChoice::DodgeRecoveryMs => SettingsChoice::MoveSpeed,
        }
    }
}

/// 設定画面を描画する。MUSIC/SEのON/OFF、Xブロック/AIR/スター/ダイヤ・アイテム3種の
/// 出現率(%)、色ブロックの色数、現在選択中の項目をカーソル(反転表示)で示す。
/// `standalone`はタイトルから開いた独立画面(true、Qでタイトルへ戻る)か、プレイ中の
/// 一時停止オーバーレイ(false、Qはオーバーレイを閉じてプレイ再開するだけ)かを表す
/// (TERM独自拡張。#155)。
#[allow(clippy::too_many_arguments)]
pub fn draw_settings(
    frame: &mut Frame,
    selection: SettingsChoice,
    music_enabled: bool,
    se_enabled: bool,
    rock_rate_percent: u32,
    air_rate_percent: u32,
    star_rate_percent: u32,
    diamond_rate_percent: u32,
    item_clear_above_rate_percent: u32,
    item_unify_colors_rate_percent: u32,
    item_starify_screen_rate_percent: u32,
    color_count: u8,
    color_cluster_rate_percent: u32,
    field_width: usize,
    block_fall_tick_ms: u64,
    player_fall_tick_ms: u64,
    move_cooldown_ms: u64,
    dodge_recovery_ms: u64,
    bomb_spawn_rate_percent: u32,
    debug_log_enabled: bool,
    chain_vanish_interval_ms: u64,
    standalone: bool,
) {
    let area = frame.area();

    frame.buffer_mut().set_style(
        area,
        Style::default()
            .fg(colors::LETTERBOX_BG)
            .bg(colors::LETTERBOX_BG),
    );

    let frame_rect = centered_fixed_rect(TOTAL_SCREEN_W, TOTAL_SCREEN_H, area);
    // 設定項目が増えるたびに縦に伸びてきた(#108でアイテム3種のrate行を追加した際、
    // 従来の50%では下部のms_line(落下速度等)が枠からクリップして見えなくなった。
    // ユーザー指摘: 「設定画面から時間要素の細かいものが結構消えてる」)。項目追加を
    // 見越して余裕を持たせる。
    let settings_area = centered_rect(60, 90, frame_rect);
    frame.render_widget(Clear, settings_area);

    let text_style = Style::default()
        .fg(colors::PANEL_TEXT)
        .bg(colors::LETTERBOX_BG);
    let selected_style = Style::default()
        .fg(colors::LETTERBOX_BG)
        .bg(colors::PANEL_TEXT);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(colors::PANEL_BORDER)
                .bg(colors::LETTERBOX_BG),
        )
        .style(Style::default().bg(colors::LETTERBOX_BG));

    let toggle_line = |label: &str, enabled: bool, is_selected: bool| {
        let prefix = if is_selected { "> " } else { "  " };
        let style = if is_selected {
            selected_style
        } else {
            text_style
        };
        Line::from(Span::styled(
            format!("{prefix}{label}: {}", on_off_label(enabled)),
            style,
        ))
    };
    let rate_line = |label: &str, percent: u32, is_selected: bool| {
        let prefix = if is_selected { "> " } else { "  " };
        let style = if is_selected {
            selected_style
        } else {
            text_style
        };
        Line::from(Span::styled(format!("{prefix}{label}: {percent}%"), style))
    };
    let count_line = |label: &str, count: u8, is_selected: bool| {
        let prefix = if is_selected { "> " } else { "  " };
        let style = if is_selected {
            selected_style
        } else {
            text_style
        };
        Line::from(Span::styled(format!("{prefix}{label}: {count}"), style))
    };
    let width_line = |label: &str, width: usize, is_selected: bool| {
        let prefix = if is_selected { "> " } else { "  " };
        let style = if is_selected {
            selected_style
        } else {
            text_style
        };
        Line::from(Span::styled(format!("{prefix}{label}: {width}"), style))
    };
    let ms_line = |label: &str, ms: u64, is_selected: bool| {
        let prefix = if is_selected { "> " } else { "  " };
        let style = if is_selected {
            selected_style
        } else {
            text_style
        };
        Line::from(Span::styled(format!("{prefix}{label}: {ms}ms"), style))
    };

    let paragraph = Paragraph::new(vec![
        Line::from(Span::styled("SETTINGS", text_style)),
        Line::from(""),
        toggle_line("MUSIC", music_enabled, selection == SettingsChoice::Music),
        toggle_line("SE", se_enabled, selection == SettingsChoice::Se),
        rate_line(
            "Xブロック配分",
            rock_rate_percent,
            selection == SettingsChoice::RockRate,
        ),
        rate_line(
            "AIR配分",
            air_rate_percent,
            selection == SettingsChoice::AirRate,
        ),
        rate_line(
            "スター配分",
            star_rate_percent,
            selection == SettingsChoice::StarRate,
        ),
        rate_line(
            "ダイヤ配分",
            diamond_rate_percent,
            selection == SettingsChoice::DiamondRate,
        ),
        rate_line(
            "Rアイテム配分",
            item_clear_above_rate_percent,
            selection == SettingsChoice::ItemClearAboveRate,
        ),
        rate_line(
            "Cアイテム配分",
            item_unify_colors_rate_percent,
            selection == SettingsChoice::ItemUnifyColorsRate,
        ),
        rate_line(
            "Kアイテム配分",
            item_starify_screen_rate_percent,
            selection == SettingsChoice::ItemStarifyScreenRate,
        ),
        count_line("色数", color_count, selection == SettingsChoice::ColorCount),
        rate_line(
            "色ブロック結合割合",
            color_cluster_rate_percent,
            selection == SettingsChoice::ColorClusterRate,
        ),
        width_line(
            "列数(次回開始時に反映)",
            field_width,
            selection == SettingsChoice::FieldWidth,
        ),
        ms_line(
            "ブロック落下速度(小さいほど速い)",
            block_fall_tick_ms,
            selection == SettingsChoice::BlockFallSpeed,
        ),
        ms_line(
            "キャラの落下速度(小さいほど速い)",
            player_fall_tick_ms,
            selection == SettingsChoice::PlayerFallSpeed,
        ),
        ms_line(
            "横移動速度(小さいほど速い)",
            move_cooldown_ms,
            selection == SettingsChoice::MoveSpeed,
        ),
        ms_line(
            "回避後の硬直時間",
            dodge_recovery_ms,
            selection == SettingsChoice::DodgeRecoveryMs,
        ),
        rate_line(
            "ボム出現頻度",
            bomb_spawn_rate_percent,
            selection == SettingsChoice::BombRate,
        ),
        toggle_line(
            "DEBUG LOG",
            debug_log_enabled,
            selection == SettingsChoice::DebugLogEnabled,
        ),
        ms_line(
            "連鎖消滅インターバル",
            chain_vanish_interval_ms,
            selection == SettingsChoice::ChainVanishInterval,
        ),
        Line::from(""),
        Line::from(Span::styled(
            "↑↓で選択 / MUSIC・SE・DEBUG LOGはSpaceか←→でトグル",
            text_style,
        )),
        Line::from(Span::styled(
            if standalone {
                "配分・色数は←→で調整 / Qでタイトルへ"
            } else {
                "配分・色数は←→で調整 / Qで閉じる"
            },
            text_style,
        )),
    ])
    .block(block)
    .style(Style::default().bg(colors::LETTERBOX_BG))
    .alignment(Alignment::Center);
    frame.render_widget(paragraph, settings_area);
}

fn draw_size_warning(frame: &mut Frame, area: Rect) {
    let message = format!(
        "ターミナルサイズが不足しています(現在 {}x{} / 最小 {}x{} / 推奨 {}x{})。ウィンドウを広げてください",
        area.width, area.height, MIN_TERMINAL_W, MIN_TERMINAL_H, TOTAL_SCREEN_W, TOTAL_SCREEN_H
    );
    let style = Style::default()
        .fg(colors::PANEL_TEXT)
        .bg(colors::LETTERBOX_BG);
    let paragraph = Paragraph::new(message)
        .style(style)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

// ---------------------------------------------------------------------------
// 9.2〜9.5 フィールド描画
// ---------------------------------------------------------------------------

fn draw_field(frame: &mut Frame, area: Rect, visible_rows: usize, game: &Game) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(colors::PANEL_BORDER)
                .bg(colors::LETTERBOX_BG),
        )
        .style(Style::default().bg(colors::FIELD_EMPTY_BG));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 || visible_rows == 0 {
        return;
    }

    let player_screen_row = (visible_rows * PLAYER_SCREEN_ROW_RATIO_NUM
        / PLAYER_SCREEN_ROW_RATIO_DEN)
        .min(visible_rows.saturating_sub(1));
    let top_row = game.player.row.saturating_sub(player_screen_row);

    let buf = frame.buffer_mut();

    // 直近の重力ティックで落下した(移動後の位置)→(移動前の位置)のマップ(TERM独自拡張。
    // ユーザー指摘: 「ブロックの落ち方をコマ送りでなくピクセル単位で滑らかにしてほしい」)。
    // 移動後の位置は、静的な通常描画では一旦Emptyとして扱い(まだ本来の場所に「到着」
    // していない、宙にある状態を表現するため)、実際の内容はこのあと`draw_falling_blocks`が
    // 移動前→移動後を補間した位置へ重ねて描画する。
    let moved_map: HashMap<Pos, Pos> = game.recently_moved_blocks().iter().copied().collect();

    for screen_row in 0..visible_rows {
        let y = inner.y + screen_row as u16 * CELL_H;
        if y + CELL_H > inner.y + inner.height {
            break; // 縮退表示でinner.heightが可視行数ぶんに満たない場合の防御
        }

        let board_row = top_row + screen_row;
        for col in 0..game.board.width() {
            let x = inner.x + col as u16 * CELL_W;
            if x + CELL_W > inner.x + inner.width {
                break;
            }

            let cell = if moved_map.contains_key(&(board_row, col)) {
                BoardCell::Empty
            } else if board_row < game.board.depth_rows() {
                game.board.cell(board_row, col)
            } else {
                BoardCell::Empty
            };
            // プレイヤーがいる論理セルも含め、常にそのマス本来の内容を描画する
            // (プレイヤーは掘削・移動でEmptyになったマスにしか進入できないため、
            // 通常はここでEmpty背景が描かれるだけになる)。プレイヤー自身のスプライトは
            // このループの外側で、見た目補間アニメーション込みで別途重ねて描画する
            // (spec.md 9.5・9章TERM独自拡張)。
            //
            // 支えを失って揺れている(落下開始前の猶予期間中の)ブロックは、左右に
            // 小刻みなジッターを加えて描画する(TERM独自拡張。ユーザー指摘: 「落下開始
            // までのアニメーションぐらぐらしてほしい(各種ブロック)」)。
            let draw_x = if game.is_cell_shaking(board_row, col) {
                let jitter = shake_jitter_x(game.player.elapsed_seconds, board_row, col);
                (x as i32 + jitter).clamp(
                    inner.x as i32,
                    (inner.x + inner.width).saturating_sub(CELL_W) as i32,
                ) as u16
            } else {
                x
            };
            // 消滅した直後のセルは一瞬フラッシュしてから背景色へ消えていく
            // (TERM独自拡張。ユーザー指摘: 「ブロックが消える瞬間に消える演出して
            // ほしい」)。
            // ボム爆発の爆風が届いた直後のセルは、スター変換後の見た目を炎の色で
            // 一瞬覆う(TERM独自拡張。#126。ユーザー指摘: 「爆弾が爆発するときは、
            // ボンバーマンTERMのように炎アニメーションほしい」)。
            // 最終ゴール(深度1000m)到達時、盤面の底(実際のフィールドより深い、
            // 本来は描画対象の無い行)に地底の地面を表示し、クリアした実感を出す
            // (TERM独自拡張。#182。ユーザー指摘: 「最終ゴールは地底の地面を表示して
            // クリアした感じにしてほしい」)。
            if game.status == GameStatus::Cleared && board_row >= game.board.depth_rows() {
                fill_bedrock_ground(buf, draw_x, y);
            } else if let Some((t, tier)) = game.explosion_flash_progress((board_row, col)) {
                fill_block(
                    buf,
                    draw_x,
                    y,
                    colors::explosion_flame_bg(tier, t, natural_cell_bg(cell)),
                );
            } else if cell == BoardCell::Empty
                && let Some(t) = game.vanish_flash_progress((board_row, col))
            {
                fill_block(buf, draw_x, y, colors::vanish_flash_bg(t));
            } else if cell == BoardCell::Empty && is_checkpoint_safe_zone_row(board_row) {
                fill_bedrock_ground(buf, draw_x, y);
            } else {
                draw_logical_cell(buf, draw_x, y, &game.board, board_row, col, cell);
            }
        }
    }

    draw_falling_blocks(buf, inner, top_row, visible_rows, game, &moved_map);
    draw_bombs(buf, inner, top_row, visible_rows, game);
    draw_player(buf, inner, top_row, game);
    draw_off_screen_bomb_warnings(buf, inner, top_row, visible_rows, game);
}

/// 画面外(まだスクロールインしていない、`top_row`より浅い行)にボムがある場合、
/// そのボムがある列全体を赤く点滅させて警告する(TERM独自拡張。#175。ユーザー指摘:
/// 「知らない間に画面外に爆弾がいるので縦列を赤くピカピカさせること」)。
fn draw_off_screen_bomb_warnings(
    buf: &mut Buffer,
    inner: Rect,
    top_row: usize,
    visible_rows: usize,
    game: &Game,
) {
    let warning_cols: HashSet<usize> = game
        .bombs()
        .iter()
        .filter(|b| b.pos.0 < top_row)
        .map(|b| b.pos.1)
        .collect();
    if warning_cols.is_empty() {
        return;
    }
    let blink_on = ((game.player.elapsed_seconds * 1000.0) as u32
        / OFF_SCREEN_BOMB_WARNING_BLINK_MS)
        .is_multiple_of(2);
    if !blink_on {
        return;
    }
    for col in warning_cols {
        let x = inner.x + col as u16 * CELL_W;
        if x + CELL_W > inner.x + inner.width {
            continue;
        }
        for screen_row in 0..visible_rows {
            let y = inner.y + screen_row as u16 * CELL_H;
            if y + CELL_H > inner.y + inner.height {
                break;
            }
            fill_block(buf, x, y, colors::BOMB_BODY_DANGER_FG);
        }
    }
}

/// ボム(TERM独自拡張。#96/#123/#125/#133)を盤面の上に重ねて描画する。ブロックとは
/// 別レイヤーのオブジェクトなので、通常のセル描画ループとは独立してここで扱う。段階
/// (`BombPhase`)に応じて、白ボンの登場(Entering)→ボムが転がってくる(Rolling)→
/// 設置されて点滅カウントダウン(Ticking)の3段階を描き分ける(ユーザー指摘: 「白ボンが
/// 画面の外からとことこやってきて、日のついた爆弾をぼーんとなげてこんこんころころ...
/// ってなって、爆発する」)。白ボンはボムより1行上に表示する(ユーザー指摘: 「爆弾は
/// キャラよりも下側にも配置されるようにしてほしい」)。起爆が近づくほど導火線の火花の
/// 点滅を速める(既存の「揺れ」「スター点滅」と同じ、爆発前に必ず視覚的な予兆を
/// 出す設計方針)。転がり中(Rolling)は横移動だけでなく`bomb_roll_is_bouncing_up`で
/// 縦にも弾ませる(#133。ユーザー指摘: 「ぽーんぽーんぽんぽんころころ...って弾ませ
/// ながらモーションがないと回転寿司みたいにすーって入ってきちゃ駄目」)。
fn draw_bombs(buf: &mut Buffer, inner: Rect, top_row: usize, visible_rows: usize, game: &Game) {
    for bomb in game.bombs() {
        // originとposは常に同じ行(TERM独自拡張。#123)で、ボム自体はその行に描く。
        // 白ボンはユーザー指摘により1行上に表示する。
        let bomb_row = bomb.pos.0;
        let shirobon_row = bomb_row.saturating_sub(1);

        match bomb.phase {
            BombPhase::Entering => {
                let Some((x, y)) =
                    cell_screen_pos(inner, top_row, visible_rows, shirobon_row, bomb.origin.1)
                else {
                    continue;
                };
                draw_shirobon_sprite(buf, x, y);
            }
            BombPhase::Rolling => {
                let t = (bomb.phase_elapsed_ms as f32 / BOMB_ROLL_MS as f32).clamp(0.0, 1.0);
                let col = bomb.origin.1 as f32 + (bomb.pos.1 as f32 - bomb.origin.1 as f32) * t;
                let display_row = if bomb_roll_is_bouncing_up(t) {
                    bomb_row.saturating_sub(1)
                } else {
                    bomb_row
                };
                let Some((x, y)) =
                    cell_screen_pos_f32(inner, top_row, visible_rows, display_row, col)
                else {
                    continue;
                };
                draw_bomb_sprite(
                    buf,
                    x,
                    y,
                    colors::BOMB_BODY_FG,
                    colors::BOMB_SPARK_DIM,
                    bomb.phase_elapsed_ms,
                );
            }
            BombPhase::Settling => {
                // 落下・左右バウンド中(TERM独自拡張。#140)は現在位置(`bomb.pos`、
                // 重力・跳ねに応じて毎tick更新される)へそのまま描く。起爆カウント
                // ダウンはまだ始まっていないため、火花は暗い方の色で固定する。
                let Some((x, y)) =
                    cell_screen_pos(inner, top_row, visible_rows, bomb.pos.0, bomb.pos.1)
                else {
                    continue;
                };
                draw_bomb_sprite(
                    buf,
                    x,
                    y,
                    colors::BOMB_BODY_FG,
                    colors::BOMB_SPARK_DIM,
                    bomb.phase_elapsed_ms,
                );
            }
            BombPhase::Ticking => {
                let Some((x, y)) =
                    cell_screen_pos(inner, top_row, visible_rows, bomb_row, bomb.pos.1)
                else {
                    continue;
                };
                let spark = if bomb_is_bright_frame(bomb.remaining_ms) {
                    colors::BOMB_SPARK_BRIGHT
                } else {
                    colors::BOMB_SPARK_DIM
                };
                draw_bomb_sprite(
                    buf,
                    x,
                    y,
                    bomb_body_color(bomb.remaining_ms),
                    spark,
                    bomb.remaining_ms,
                );
            }
        }
    }
}

/// フィールド内の論理セル位置(行・列)を、現在のスクロール位置(`top_row`)・
/// 可視行数を踏まえて画面座標(x, y)へ変換する(TERM独自拡張。#125)。範囲外なら`None`。
fn cell_screen_pos(
    inner: Rect,
    top_row: usize,
    visible_rows: usize,
    row: usize,
    col: usize,
) -> Option<(u16, u16)> {
    cell_screen_pos_f32(inner, top_row, visible_rows, row, col as f32)
}

/// `cell_screen_pos`の列位置を小数(補間中の途中位置)で受け取る版(TERM独自拡張。#125)。
fn cell_screen_pos_f32(
    inner: Rect,
    top_row: usize,
    visible_rows: usize,
    row: usize,
    col: f32,
) -> Option<(u16, u16)> {
    if row < top_row {
        return None;
    }
    let screen_row = row - top_row;
    if screen_row >= visible_rows || col < 0.0 {
        return None;
    }
    let y = inner.y + screen_row as u16 * CELL_H;
    let x = inner.x as f32 + col * CELL_W as f32;
    if x < inner.x as f32 {
        return None;
    }
    let x = x.round() as u16;
    if x + CELL_W > inner.x + inner.width || y + CELL_H > inner.y + inner.height {
        return None;
    }
    Some((x, y))
}

/// 白ボンのスプライト(TERM独自拡張。#123。ユーザー指摘: 「白ボンが画面の外から
/// とことこやってきて」)。プレイヤースプライトと同じ4文字×2行の描画方式を使う。
fn draw_shirobon_sprite(buf: &mut Buffer, x: u16, y: u16) {
    for (dy, line) in [" oo ", " () "].iter().enumerate() {
        for (dx, ch) in line.chars().enumerate() {
            put(
                buf,
                x + dx as u16,
                y + dy as u16,
                ch,
                colors::SHIROBON_FG,
                colors::FIELD_EMPTY_BG,
            );
        }
    }
}

/// 転がり中(Rolling)の弾みの周期。区間を`BOMB_ROLL_BOUNCE_COUNT`回に分割し、
/// 各区間の前半だけ1マス上へ跳ねさせる(TERM独自拡張。#133)。
const BOMB_ROLL_BOUNCE_COUNT: u32 = 3;

/// 進捗`t`(0.0=転がり開始、1.0=設置直前)の時点でボムが1マス上に跳ねている
/// (=ジャンプ中)かどうか(TERM独自拡張。#133。ユーザー指摘: 「爆弾がぽーん
/// ぽーんぽんぽんころころ...って弾ませながらモーションがないと回転寿司みたいに
/// すーって入ってきちゃ駄目」)。横方向の線形移動しかしていなかった従来の
/// 見た目(コンベア/回転寿司のようにすっと滑るだけ)を避けるため、縦方向にも
/// 複数回の跳ねを加える。
fn bomb_roll_is_bouncing_up(t: f32) -> bool {
    let t = t.clamp(0.0, 1.0);
    if t >= 1.0 {
        return false;
    }
    let segment = 1.0 / BOMB_ROLL_BOUNCE_COUNT as f32;
    let local = (t / segment).fract();
    local < 0.5
}

/// 導火線の火花が「ちりちり」明滅する周期(TERM独自拡張。#130)。この時間ごとに
/// 火花の位置・グリフを切り替え、単調な点滅でなく飛び散るような見た目にする。
const BOMB_CRACKLE_FRAME_MS: u32 = 70;
const BOMB_CRACKLE_GLYPHS: [char; 4] = ['\'', '`', '.', '*'];

/// ボム本体のスプライト(TERM独自拡張。#96/#125/#130/#138。ユーザー指摘: 「ボムは、
/// 丸い「いかにもな」爆弾の形状しておいてほしい」「爆弾、背景と同化してるから、
/// もっと輪郭くっきり、火花ちりちりアニメーションさせて」「爆発直前で爆弾がチカチカ
/// 激しく赤く光るようにして」)。以前は本体グリフの前景色にしか`BOMB_BODY_FG`を
/// 使わず、周囲はフィールド背景色のまま透過していたため、暗い色同士で輪郭が背景に
/// 溶け込んで見えていた。セル全体を本体色(`body`。起爆間際は`bomb_body_color`で
/// 赤く点滅させたものを渡す)で塗りつぶした上に、明るい縁取り色(`BOMB_RIM_FG`)で
/// 丸い輪郭を描き、上段の導火線の火花は`crackle_ms`に応じて位置・グリフを切り替えて
/// ちりちりと弾けるようにする。
fn draw_bomb_sprite(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    body: Color,
    spark_color: Color,
    crackle_ms: u32,
) {
    let rim = colors::BOMB_RIM_FG;

    let frame = (crackle_ms / BOMB_CRACKLE_FRAME_MS) as usize;
    let spark_glyph = BOMB_CRACKLE_GLYPHS[frame % BOMB_CRACKLE_GLYPHS.len()];
    let spark_on_left = frame.is_multiple_of(2);

    put(buf, x, y, ' ', body, body);
    put(
        buf,
        x + 1,
        y,
        if spark_on_left { spark_glyph } else { ' ' },
        spark_color,
        body,
    );
    put(
        buf,
        x + 2,
        y,
        if spark_on_left { ' ' } else { spark_glyph },
        spark_color,
        body,
    );
    put(buf, x + 3, y, ' ', body, body);

    put(buf, x, y + 1, '(', rim, body);
    put(buf, x + 1, y + 1, '●', rim, body);
    put(buf, x + 2, y + 1, '●', rim, body);
    put(buf, x + 3, y + 1, ')', rim, body);
}

/// ボムの点滅が「明るい方」の周期かどうか(TERM独自拡張。#96)。残り時間が
/// `BOMB_BLINK_FAST_THRESHOLD_MS`を切ると点滅周期を短くし、起爆間近であることを
/// 強調する。
const BOMB_BLINK_PERIOD_MS: u32 = 400;
const BOMB_BLINK_PERIOD_FAST_MS: u32 = 150;
const BOMB_BLINK_FAST_THRESHOLD_MS: u32 = 1000;

fn bomb_is_bright_frame(remaining_ms: u32) -> bool {
    let period = if remaining_ms <= BOMB_BLINK_FAST_THRESHOLD_MS {
        BOMB_BLINK_PERIOD_FAST_MS
    } else {
        BOMB_BLINK_PERIOD_MS
    };
    (remaining_ms / period).is_multiple_of(2)
}

/// 起爆間際は導火線の火花だけでなく本体そのものも激しく赤く点滅させる
/// (TERM独自拡張。#138。ユーザー指摘: 「爆発直前で爆弾がチカチカ激しく赤く
/// 光るようにして」)。残り時間が`BOMB_DANGER_MS`(#168で導火線カウントダウンSE
/// と共有する定数に切り出した)を切ったら、`BOMB_BODY_FLASH_PERIOD_MS`ごとに
/// 通常の本体色と警告色(赤)を切り替える。
const BOMB_BODY_FLASH_PERIOD_MS: u32 = 100;

/// 画面外のボム警告(縦列の赤ピカピカ)の点滅周期(ms、TERM独自拡張。#175)。
const OFF_SCREEN_BOMB_WARNING_BLINK_MS: u32 = 400;

fn bomb_body_color(remaining_ms: u32) -> Color {
    if remaining_ms > BOMB_DANGER_MS {
        return colors::BOMB_BODY_FG;
    }
    if (remaining_ms / BOMB_BODY_FLASH_PERIOD_MS).is_multiple_of(2) {
        colors::BOMB_BODY_DANGER_FG
    } else {
        colors::BOMB_BODY_FG
    }
}

/// 直近の重力ティックで落下したブロックを、移動前の位置から移動後の位置へ向けて
/// 滑らかに補間した画面座標へ描画する(TERM独自拡張。ユーザー指摘: 「ブロックの落ち方を
/// コマ送りでなくピクセル単位で滑らかにしてほしい」)。連結・接続表現(丸み縁取り等)は
/// 移動が完了してから通常描画に委ねるため、ここでは単色の塗りつぶしのみ行う。
fn draw_falling_blocks(
    buf: &mut Buffer,
    inner: Rect,
    top_row: usize,
    visible_rows: usize,
    game: &Game,
    moved_map: &HashMap<Pos, Pos>,
) {
    let t = game.block_fall_progress();
    for (&(to_row, to_col), &(from_row, from_col)) in moved_map {
        let cell = if to_row < game.board.depth_rows() {
            game.board.cell(to_row, to_col)
        } else {
            BoardCell::Empty
        };
        // 着地と同一tickで4連結自動消滅した場合、盤面は既にEmptyだが消滅フラッシュは
        // まだ残っている(TERM独自拡張。#172。ユーザー指摘: 「崩れてきたブロックが、
        // 接地する1コマ前でスルスルと消えてしまう」)。盤面から読めない間は消滅直前の
        // 種類で補い、最後まで落ちきってからフラッシュへ移る見た目にする。
        let cell = match cell {
            BoardCell::Empty => {
                let resolved = game.recently_vanished_kind((to_row, to_col));
                // #174: このフォールバック分岐に入ったこと自体(補えたか/丸ごと
                // スキップしたか)をログに残す。この分岐は同一tick着地+自動消滅の
                // ような稀なケースでしか通らないため、毎フレーム描画中でも記録量は
                // 少ない。
                game.log_render_fallback((to_row, to_col), (from_row, from_col), resolved);
                match resolved {
                    Some(kind) => kind,
                    None => continue, // 押し潰し等で既に消滅済み、表示すべき内容がない
                }
            }
            other => other,
        };

        let interp_row = from_row as f32 + (to_row as f32 - from_row as f32) * t;
        let interp_col = from_col as f32 + (to_col as f32 - from_col as f32) * t;
        let screen_row = interp_row - top_row as f32;
        if screen_row < 0.0 || screen_row > visible_rows as f32 {
            continue; // 画面外
        }

        let px = inner.x as f32 + interp_col * CELL_W as f32;
        let py = inner.y as f32 + screen_row * CELL_H as f32;
        if px < 0.0 || py < 0.0 {
            continue;
        }
        let x = px.round() as u16;
        let y = py.round() as u16;
        if x + CELL_W > inner.x + inner.width || y + CELL_H > inner.y + inner.height {
            continue; // 補間の一時的なはみ出しは描画をスキップする
        }
        // 落下中も静止時と同じグリフ模様(色ブロックの接続罫線・岩のXマーク・
        // ダイヤ/スター等の固定グリフ)で描画する(TERM独自拡張。ユーザー指摘:
        // 「落下アニメーションで模様が消えて、色味だけでしか認識できない」
        // 「あいまいな物体が落ちているように見える」)。接続罫線の判定は着地先
        // (to_row, to_col)時点の盤面を基準にする(その時点で既に確定している)。
        draw_logical_cell(buf, x, y, &game.board, to_row, to_col, cell);
    }
}

/// 揺れ中のブロックにかける、左右の小刻みなジッター(文字数単位、TERM独自拡張)。
/// セルの座標から求めた位相をずらすことで、隣接セルが機械的に完全同期して見えるのを
/// 避けつつ、同じ塊はおおむね一体で震える(ユーザー指摘: 「落下開始までのアニメーション
/// ぐらぐらしてほしい(各種ブロック)」)。
fn shake_jitter_x(elapsed_secs: f32, row: usize, col: usize) -> i32 {
    const FREQ: f32 = 18.0;
    let phase = (row as f32 * 0.7 + col as f32 * 1.3) % std::f32::consts::TAU;
    let s = (elapsed_secs * FREQ + phase).sin();
    if s > 0.3 {
        1
    } else if s < -0.3 {
        -1
    } else {
        0
    }
}

/// プレイヤーのスプライトを、直前の論理位置から現在位置へ補間した画面座標へ描画する
/// (TERM独自拡張、9章)。ロジック上の当たり判定・掘削・落下判定は常に整数マス基準の
/// ままで、ここで行うのはあくまで描画位置の補間のみ。
fn draw_player(buf: &mut Buffer, inner: Rect, top_row: usize, game: &Game) {
    let (prev_row, prev_col) = game.render_prev_position();
    let (cur_row, cur_col) = game.player.position();
    let t = game.move_anim_progress();

    let interp_row = prev_row as f32 + (cur_row as f32 - prev_row as f32) * t;
    let interp_col = prev_col as f32 + (cur_col as f32 - prev_col as f32) * t;

    let screen_row = interp_row - top_row as f32;
    if screen_row < 0.0 {
        return; // スクロール範囲外(補間中に上端を跨ぐ極端なケースの防御)
    }

    // 「わ〜!」スライダー演出中(TERM独自拡張)は、直前の移動方向へさらに滑り込み、
    // 進捗が進むにつれ本来の位置へ戻ってくる(ユーザー指摘: 「ブロックが落ち始める
    // 直前に移動してにげたとき、「わ〜!」ってスライダー(アニメーションしてねキャラ)
    // して切り間に合う感じ」)。
    let dodge_offset_cells = if game.is_dodge_sliding() {
        let dir_col = (cur_col as f32 - prev_col as f32).signum();
        (1.0 - game.dodge_slide_progress()) * DODGE_SLIDE_OFFSET_CELLS * dir_col
    } else {
        0.0
    };
    let px = inner.x as f32 + interp_col * CELL_W as f32 + dodge_offset_cells * CELL_W as f32;
    // 「天に召される」演出中(TERM独自拡張)は、進捗に応じてスプライトを上へ
    // ドリフトさせる(ユーザー指摘: 「潰れたとき、もっとわかりやすいように死んで、
    // 一度天に召される演出をして」)。
    let ascend_offset = game.ascend_progress() * ASCEND_RISE_CELLS * CELL_H as f32;
    let py = inner.y as f32 + screen_row * CELL_H as f32 - ascend_offset;
    if px < 0.0 || py < 0.0 {
        return;
    }
    let x = px.round() as u16;
    let y = py.round() as u16;
    if y < inner.y {
        return; // 天に召される演出で上端より高く昇った分は描画しない(そのまま見えなくなる)
    }
    if x + CELL_W > inner.x + inner.width || y + CELL_H > inner.y + inner.height {
        return; // 補間の一時的なはみ出しは描画をスキップする(クラッシュ防止)
    }

    let cur_cell = if cur_row < game.board.depth_rows() {
        game.board.cell(cur_row, cur_col)
    } else {
        BoardCell::Empty
    };
    let bg = natural_cell_bg(cur_cell);

    if game.crush_flash_active() {
        draw_crushed_sprite(buf, x, y, bg);
    } else if game.is_dodge_sliding() {
        draw_player_sprite(buf, x, y, DODGE_SPRITE, bg);
    } else {
        draw_player_sprite(
            buf,
            x,
            y,
            player_sprite(game.player.facing, game.drilling_frame()),
            bg,
        );
    }
}

/// 「わ〜!」スライダー演出で最大どれだけ滑らせるか(論理セル単位、TERM独自拡張)。
const DODGE_SLIDE_OFFSET_CELLS: f32 = 0.6;

/// 「天に召される」演出でスプライトが上へ昇る距離(論理セル単位、TERM独自拡張)。
const ASCEND_RISE_CELLS: f32 = 2.0;

/// 1論理セルぶん(4文字×2行)を描画する。
fn draw_logical_cell(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    board: &Board,
    row: usize,
    col: usize,
    cell: BoardCell,
) {
    match cell {
        BoardCell::Empty => fill_block(buf, x, y, colors::FIELD_EMPTY_BG),
        BoardCell::Color(kind) => draw_color_block(buf, x, y, board, row, col, kind),
        BoardCell::Rock { hits } => draw_rock_block(buf, x, y, board, row, col, hits),
        // AIRはカプセル(丸薬)らしいシルエットにする(TERM独自拡張。#106/#128。
        // ユーザー指摘: 「AIRはカプセルの形状をしていてほしい 正方形ではなくて」。
        // #106時点では枠線の角glyphを丸めるだけで、セル自体の背景は正方形のまま
        // 塗りつぶされていたため依然として正方形に見えていた。`draw_rounded_unit`
        // は四隅のセルをフィールド背景色で斜めに欠き取り、実際に輪郭が丸まった
        // シルエットになるようにする。
        BoardCell::Oxygen => draw_rounded_unit(
            buf,
            x,
            y,
            [['◜', '◝'], ['◟', '◞']],
            colors::OXYGEN_FG,
            colors::OXYGEN_BG,
        ),
        BoardCell::Diamond => draw_diamond_block(buf, x, y, board, row, col),
        // スターブロックは氷の結晶のようにきらめかせる(TERM独自拡張。#134。ユーザー
        // 指摘: 「スター化したブロックはもっとキラキラしたモーションしてほしい
        // 本当に氷見たく いまのままだとただの白い正方形だ」)。#128/#132と同じく
        // `draw_rounded_unit`で正方形の塗りつぶしをやめ、さらに4マスを同時に一斉
        // 点滅させるのでなく`star_sparkle_content`で位置ごとに位相をずらし、複数
        // 箇所が順にきらめくようにする。
        BoardCell::Star { visible_ms } => draw_rounded_unit(
            buf,
            x,
            y,
            star_sparkle_content(visible_ms),
            colors::STAR_FG,
            colors::star_bg(visible_ms, STAR_VISIBLE_GRACE_MS, STAR_MELT_DURATION_MS),
        ),
        // アイテムブロックも他ブロック同様、効果ごとに専用の形状にする(TERM独自拡張。
        // ユーザー指摘: 「他のアイテムも相手有無特有の形状にしたい」)。ClearAboveは
        // 頭上を吹き飛ばすイメージで上向き矢印を、UnifyColorsは色が混ざり合う
        // イメージで陰陽風の分割円を上段に添える。AIR(#128)と同じく、`draw_fixed_unit`
        // の「セル全体を正方形に塗りつぶす」見た目ではアイテムらしさが薄いという
        // 指摘(#132。ユーザー指摘: 「C/R/Kアイテムもアイテムっぽい形状に変えよう」)
        // を受け、`draw_rounded_unit`で四隅を欠き取った輪郭にした。
        BoardCell::Item(ItemEffect::ClearAbove) => draw_rounded_unit(
            buf,
            x,
            y,
            [['↑', '↑'], ['R', 'R']],
            colors::ITEM_CLEAR_ABOVE_FG,
            colors::ITEM_CLEAR_ABOVE_BG,
        ),
        BoardCell::Item(ItemEffect::UnifyColors) => draw_rounded_unit(
            buf,
            x,
            y,
            [['◐', '◑'], ['C', 'C']],
            colors::ITEM_UNIFY_COLORS_FG,
            colors::ITEM_UNIFY_COLORS_BG,
        ),
        // StarifyScreenはスターブロックを連想させる☆をあしらう。
        BoardCell::Item(ItemEffect::StarifyScreen) => draw_rounded_unit(
            buf,
            x,
            y,
            [['☆', '☆'], ['K', 'K']],
            colors::ITEM_STARIFY_SCREEN_FG,
            colors::ITEM_STARIFY_SCREEN_BG,
        ),
    }
}

/// スターブロックのキラキラ点滅グリフ(TERM独自拡張。ユーザー指摘: 「スターブロックは
/// 消えるまえからキラキラしてほしい」)。画面内に入ってから消えるまでの間ずっと、
/// `STAR_SPARKLE_PERIOD_MS`ごとに☆/★を交互に切り替える。
fn star_glyph(visible_ms: u32) -> char {
    if (visible_ms / STAR_SPARKLE_PERIOD_MS).is_multiple_of(2) {
        '☆'
    } else {
        '★'
    }
}

/// スターブロック内の4マス(2列×2行)ぶんの位相ずれ(TERM独自拡張。#134。ユーザー
/// 指摘: 「スター化したブロックはもっとキラキラしたモーションしてほしい 本当に
/// 氷見たく いまのままだとただの白い正方形だ」)。4マスが完全に同時に一斉点滅する
/// と結局「均一な四角」にしか見えないため、位置ごとに`STAR_SPARKLE_PERIOD_MS`の
/// 1/4ずつ位相をずらし、氷の結晶のように複数箇所が順にきらめくようにする。
const STAR_SPARKLE_PHASE_OFFSETS_MS: [[u32; 2]; 2] = [
    [0, STAR_SPARKLE_PERIOD_MS / 4],
    [STAR_SPARKLE_PERIOD_MS / 2, STAR_SPARKLE_PERIOD_MS * 3 / 4],
];

/// スターブロックの`draw_rounded_unit`用の中央2列×2行のコンテンツを、位置ごとに
/// 位相をずらした`star_glyph`で組み立てる(TERM独自拡張。#134)。
fn star_sparkle_content(visible_ms: u32) -> [[char; 2]; 2] {
    let mut content = [[' '; 2]; 2];
    for (row, offsets) in STAR_SPARKLE_PHASE_OFFSETS_MS.iter().enumerate() {
        for (col, &offset) in offsets.iter().enumerate() {
            content[row][col] = star_glyph(visible_ms.wrapping_add(offset));
        }
    }
    content
}

/// バッファ1マスへ文字・前景色・背景色を明示的に設定する(範囲外は無視)。
fn put(buf: &mut Buffer, x: u16, y: u16, ch: char, fg: Color, bg: Color) {
    if let Some(cell) = buf.cell_mut(Position::new(x, y)) {
        cell.set_char(ch).set_fg(fg).set_bg(bg);
    }
}

/// 4文字×2行を単色の空白で塗りつぶす(Cell::Empty用)。
fn fill_block(buf: &mut Buffer, x: u16, y: u16, bg: Color) {
    for dy in 0..CELL_H {
        for dx in 0..CELL_W {
            put(buf, x + dx, y + dy, ' ', bg, bg);
        }
    }
}

/// 最終ゴール(深度1000m)到達時、盤面の底に見える地底の地面(TERM独自拡張。#182。
/// ユーザー指摘: 「最終ゴールは地底の地面を表示してクリアした感じにしてほしい」)。
/// 単色の塗りつぶしではなく、岩肌のようなハッチング模様にして「掘り進めない本当の
/// 底に到達した」ことを見た目でも伝える。
const BEDROCK_GROUND_GLYPHS: [[char; 4]; 2] = [['▓', '▒', '▓', '▒'], ['▒', '▓', '▒', '▓']];

fn fill_bedrock_ground(buf: &mut Buffer, x: u16, y: u16) {
    for (dy, row) in BEDROCK_GROUND_GLYPHS.iter().enumerate() {
        for (dx, &ch) in row.iter().enumerate() {
            put(
                buf,
                x + dx as u16,
                y + dy as u16,
                ch,
                colors::BEDROCK_GROUND_FG,
                colors::BEDROCK_GROUND_BG,
            );
        }
    }
}

/// `board_row`が、100mごとのチェックポイント通過後の安全地帯(TERM独自拡張。
/// #181/#185)に含まれるかどうか(TERM独自拡張。#186。ユーザー指摘: 「100mごとの
/// 先はどうせクリアするのでいったん何もなし(地面みたいにしてほしい)」)。安全地帯は
/// 通過すると必ず`Cell::Empty`になる区間なので、素の空背景ではなく最終ゴールと
/// 同じ地底の地面ビジュアルで表示する。500mはボーナスフロア(アイテム/AIR配置。
/// #179)であり空にはならないため対象外にする。
fn is_checkpoint_safe_zone_row(board_row: usize) -> bool {
    if board_row < CHECKPOINT_STEP_M {
        return false;
    }
    let checkpoint_start = (board_row / CHECKPOINT_STEP_M) * CHECKPOINT_STEP_M;
    if checkpoint_start == BONUS_FLOOR_DEPTH_M {
        return false;
    }
    board_row < checkpoint_start + CHECKPOINT_SAFE_ZONE_M
}

// --- 9.3 色ブロックの塊表現(接続マスク・丸み縁取り・ハイライト/陰影) ---

/// 隣接セルとの接続関係(描画専用の判定。spec.md 9.3)。
struct ConnMask {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
}

/// `same`(隣接セルが自分と同種と言えるか)を基準に4方向の接続有無を求める共通処理。
/// 色ブロック(同色判定)・岩ブロック(hitsを問わずRockかどうかの判定)の両方で使う。
fn conn_mask_by(
    board: &Board,
    row: usize,
    col: usize,
    same: impl Fn(BoardCell) -> bool,
) -> ConnMask {
    let check = |r: isize, c: isize| -> bool {
        r >= 0
            && (r as usize) < board.depth_rows()
            && c >= 0
            && (c as usize) < board.width()
            && same(board.cell(r as usize, c as usize))
    };
    ConnMask {
        up: check(row as isize - 1, col as isize),
        down: check(row as isize + 1, col as isize),
        left: check(row as isize, col as isize - 1),
        right: check(row as isize, col as isize + 1),
    }
}

fn conn_mask(board: &Board, row: usize, col: usize, kind: ColorKind) -> ConnMask {
    conn_mask_by(board, row, col, |cell| cell == BoardCell::Color(kind))
}

/// 岩ブロック用の接続判定。ヒット数(hits)が違っていても同じ岩ブロック種別として
/// 連結しているとみなす(spec.md 4章「岩ブロックもhitsを問わず連結対象」、game::board::hit_rock参照)。
fn conn_mask_rock(board: &Board, row: usize, col: usize) -> ConnMask {
    conn_mask_by(board, row, col, |cell| {
        matches!(cell, BoardCell::Rock { .. })
    })
}

/// ダイヤブロック用の接続判定(TERM独自拡張。#141。ユーザー指摘: 「ダイヤブロック
/// の見た目を岩ボコのような形状にして」)。岩ブロックと同じく、隣接するダイヤ
/// ブロック同士の境界を消して1つの塊(ゴツゴツした岩のような連続した形状)に
/// 見えるようにする。
fn conn_mask_diamond(board: &Board, row: usize, col: usize) -> ConnMask {
    conn_mask_by(board, row, col, |cell| matches!(cell, BoardCell::Diamond))
}

fn draw_color_block(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    board: &Board,
    row: usize,
    col: usize,
    kind: ColorKind,
) {
    let mask = conn_mask(board, row, col, kind);
    let bg = colors::fill_color(kind);
    let border_fg = colors::highlight_color(kind);

    // 角(上下行×左右列の4隅)
    put_corner(buf, x, y, mask.up, mask.left, '╭', border_fg, bg);
    put_corner(buf, x + 3, y, mask.up, mask.right, '╮', border_fg, bg);
    put_corner(buf, x, y + 1, mask.down, mask.left, '╰', border_fg, bg);
    put_corner(buf, x + 3, y + 1, mask.down, mask.right, '╯', border_fg, bg);

    // 辺(上辺・下辺の中間2列)
    put_edge(buf, x + 1, y, mask.up, border_fg, bg);
    put_edge(buf, x + 2, y, mask.up, border_fg, bg);
    put_edge(buf, x + 1, y + 1, mask.down, border_fg, bg);
    put_edge(buf, x + 2, y + 1, mask.down, border_fg, bg);
}

/// 角1マスぶんの罫線文字を決めて描く(spec.md 9.3の表)。
/// `a_conn`/`b_conn`はこの角に関係する2方向(例: 左上角ならup, left)の接続有無。
#[allow(clippy::too_many_arguments)] // 描画座標・接続フラグ・グリフ・配色をまとめた薄いヘルパーのため許容する
fn put_corner(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    a_conn: bool,
    b_conn: bool,
    none_glyph: char,
    fg: Color,
    bg: Color,
) {
    let ch = match (a_conn, b_conn) {
        (false, false) => none_glyph,
        (false, true) => '─',
        (true, false) => '│',
        (true, true) => ' ', // 内部(fill): 両方とも接続 -> 境界を消す
    };
    put(buf, x, y, ch, fg, bg);
}

/// 上辺/下辺1マスぶんの罫線文字を決めて描く。
fn put_edge(buf: &mut Buffer, x: u16, y: u16, connected: bool, fg: Color, bg: Color) {
    let ch = if connected { ' ' } else { '─' };
    put(buf, x, y, ch, fg, bg);
}

// --- 9.4 岩・酸素・ダイヤブロックの描画 ---

/// AIR・アイテムブロック共通の描画(TERM独自拡張。#106/#128/#132。ユーザー指摘:
/// 「AIRはカプセルの形状をしていてほしい 正方形ではなくて」「C/R/Kアイテムも
/// アイテムっぽい形状に変えよう」)。角に丸罫線の"glyph"を乗せるだけでセル自体の
/// 背景は正方形のまま塗りつぶす描画だと、依然として正方形に見えてしまう。ここでは
/// 四隅のセルを四分割ブロック文字(`▘▝▖▗`)でフィールド背景色(`FIELD_EMPTY_BG`)側に
/// 3/4欠き取り、実際に輪郭が斜めに丸まったシルエット(八角形状)になるようにする。
/// 中央2列×2行の`content`は呼び出し側で種類ごとに変える。
fn draw_rounded_unit(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    content: [[char; 2]; 2],
    fg: Color,
    bg: Color,
) {
    let field_bg = colors::FIELD_EMPTY_BG;
    // 各隅セルは、外向きの角(=フィールド側)を`field_bg`、内向きの1/4だけを`bg`
    // (本体色)で残す。
    put(buf, x, y, '▗', bg, field_bg);
    put(buf, x + 3, y, '▖', bg, field_bg);
    put(buf, x, y + 1, '▝', bg, field_bg);
    put(buf, x + 3, y + 1, '▘', bg, field_bg);

    put(buf, x + 1, y, content[0][0], fg, bg);
    put(buf, x + 2, y, content[0][1], fg, bg);
    put(buf, x + 1, y + 1, content[1][0], fg, bg);
    put(buf, x + 2, y + 1, content[1][1], fg, bg);
}

/// 岩ブロックのヒビ表現(spec.md 9.4)。固定順`[(0,0),(0,1),(1,0),(1,1)]`の先頭から
/// `hits`個ぶんを`*`に置き換える。
fn rock_glyphs(hits: u8) -> [[char; 2]; 2] {
    let mut flat = ['X'; 4];
    for slot in flat.iter_mut().take(hits.min(4) as usize) {
        *slot = '*';
    }
    [[flat[0], flat[1]], [flat[2], flat[3]]]
}

/// 岩ブロック(Xブロック)の描画。色ブロックと同様、隣接する岩ブロック同士は角の罫線を
/// 接続させ1つの塊として繋がって見えるようにする(ユーザー指摘反映: 「Xブロックも接触
/// したら結合しないと」・横方向も対象)。ヒビ/Xマーク(rock_glyphs)は視認性を優先し、
/// 接続の有無に関わらず中央2列には常に表示する(色ブロックのように空白へは置き換えない)。
fn draw_rock_block(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    board: &Board,
    row: usize,
    col: usize,
    hits: u8,
) {
    let mask = conn_mask_rock(board, row, col);
    let bg = colors::rock_bg(hits);
    let fg = colors::ROCK_X_FG;

    put_corner(buf, x, y, mask.up, mask.left, '╭', fg, bg);
    put_corner(buf, x + 3, y, mask.up, mask.right, '╮', fg, bg);
    put_corner(buf, x, y + 1, mask.down, mask.left, '╰', fg, bg);
    put_corner(buf, x + 3, y + 1, mask.down, mask.right, '╯', fg, bg);

    let glyphs = rock_glyphs(hits);
    put(buf, x + 1, y, glyphs[0][0], fg, bg);
    put(buf, x + 2, y, glyphs[0][1], fg, bg);
    put(buf, x + 1, y + 1, glyphs[1][0], fg, bg);
    put(buf, x + 2, y + 1, glyphs[1][1], fg, bg);
}

/// ダイヤブロックの描画(TERM独自拡張。#141。ユーザー指摘: 「ダイヤブロックの
/// 見た目を岩ボコのような形状にして」)。以前は`draw_fixed_unit`で常に単独の
/// 角丸ボックスとして描いていたため、隣接していても1個ずつ独立した箱の並びに
/// しか見えなかった。岩ブロック(`draw_rock_block`)と同じ接続判定
/// (`conn_mask_diamond`)を使い、隣接するダイヤブロック同士の境界を消すことで、
/// ゴツゴツした岩の塊のような連続した形状になるようにする。
fn draw_diamond_block(buf: &mut Buffer, x: u16, y: u16, board: &Board, row: usize, col: usize) {
    let mask = conn_mask_diamond(board, row, col);
    let bg = colors::DIAMOND_BG;
    let fg = colors::DIAMOND_FG;

    put_corner(buf, x, y, mask.up, mask.left, '╭', fg, bg);
    put_corner(buf, x + 3, y, mask.up, mask.right, '╮', fg, bg);
    put_corner(buf, x, y + 1, mask.down, mask.left, '╰', fg, bg);
    put_corner(buf, x + 3, y + 1, mask.down, mask.right, '╯', fg, bg);

    put(buf, x + 1, y, '◆', fg, bg);
    put(buf, x + 2, y, '◆', fg, bg);
    put(buf, x + 1, y + 1, '◆', fg, bg);
    put(buf, x + 2, y + 1, '◆', fg, bg);
}

// --- 9.5 プレイヤースプライト ---

/// プレイヤーが立っているマス本来の背景色(9.5「そのマスの本来の背景色をそのまま使う」)。
fn natural_cell_bg(cell: BoardCell) -> Color {
    match cell {
        BoardCell::Empty => colors::FIELD_EMPTY_BG,
        BoardCell::Color(kind) => colors::fill_color(kind),
        BoardCell::Rock { hits } => colors::rock_bg(hits),
        BoardCell::Oxygen => colors::OXYGEN_BG,
        BoardCell::Diamond => colors::DIAMOND_BG,
        BoardCell::Star { visible_ms } => {
            colors::star_bg(visible_ms, STAR_VISIBLE_GRACE_MS, STAR_MELT_DURATION_MS)
        }
        BoardCell::Item(ItemEffect::ClearAbove) => colors::ITEM_CLEAR_ABOVE_BG,
        BoardCell::Item(ItemEffect::UnifyColors) => colors::ITEM_UNIFY_COLORS_BG,
        BoardCell::Item(ItemEffect::StarifyScreen) => colors::ITEM_STARIFY_SCREEN_BG,
    }
}

fn draw_player_sprite(buf: &mut Buffer, x: u16, y: u16, lines: [&str; 2], bg: Color) {
    for (dy, line) in lines.iter().enumerate() {
        for (dx, ch) in line.chars().enumerate() {
            put(buf, x + dx as u16, y + dy as u16, ch, colors::PLAYER_FG, bg);
        }
    }
}

/// プレイヤーの向き・掘削演出フレームに応じたスプライト(4文字×2行)を返す(TERM独自
/// 拡張。ユーザー指摘: 「上に掘る時、上向きながらピヨンピヨン跳ねる。左右に掘る時、
/// 横にドリルをぐいぐい。下に掘る時、下向きながらドリルをぐいぐい」)。`drilling_frame`が
/// `None`なら静止スプライト、`Some(_)`なら`DRILL_ANIM_FRAME_MS`ごとに交互する方向別の
/// 2フレームを返す(掘削は常にfacing方向に対して行われるため、facingがそのまま
/// 掘削方向になる)。
///
/// 目(oo/OO)を丸括弧で挟んでヘルメットの縁を表現し、進行方向の先端は開けたまま
/// にする(掘削の刃が突き出る側、TERM独自拡張。#160。ユーザー指摘: 「キャラが
/// せめて、ドリルかなんか、それっぽいやつにしてほしい ホリススムくんのシルエット
/// に見えたらなおよし」)。上下は正面から見た形なので左右対称に閉じ、左右は
/// 進行方向側だけ`<`/`>`のドリル先端を開けておく。
fn player_sprite(facing: Direction, drilling_frame: Option<bool>) -> [&'static str; 2] {
    match (facing, drilling_frame) {
        (Direction::Down, Some(true)) => ["(oo)", " || "],
        (Direction::Down, _) => ["(oo)", " \\/ "],
        (Direction::Up, Some(true)) => [" /\\ ", "(OO)"],
        (Direction::Up, _) => [" /\\ ", "(oo)"],
        (Direction::Left, Some(true)) => ["<oo)", "<==="],
        (Direction::Left, _) => ["<oo)", "<== "],
        (Direction::Right, Some(true)) => ["(oo>", "===>"],
        (Direction::Right, _) => ["(oo>", " ==>"],
    }
}

/// 「わ〜!」スライダー演出中(TERM独自拡張)のプレイヤースプライト。方向によらず
/// 常にこの驚き顔で表示する。
const DODGE_SPRITE: [&str; 2] = ["!OO!", " /\\ "];

/// 落下ブロックに押し潰された際の「潰れた」演出用スプライト(TERM独自拡張、9章)。
/// GameOverオーバーレイの表示前に一呼吸`CRUSH_FLASH_MS`ぶんだけ表示する。1行目に
/// ×印を並べ、2行目は空白にすることで平たく潰れた見た目を表現する。
fn draw_crushed_sprite(buf: &mut Buffer, x: u16, y: u16, bg: Color) {
    for (dx, ch) in "××××".chars().enumerate() {
        put(buf, x + dx as u16, y, ch, colors::CRUSH_FLASH_FG, bg);
    }
    for dx in 0..CELL_W {
        put(buf, x + dx, y + 1, ' ', colors::CRUSH_FLASH_FG, bg);
    }
}

// ---------------------------------------------------------------------------
// 9.7 ステータスパネル(HUD)
// ---------------------------------------------------------------------------

fn draw_status(frame: &mut Frame, area: Rect, game: &Game) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(colors::PANEL_BORDER)
                .bg(colors::LETTERBOX_BG),
        )
        .style(Style::default().bg(colors::LETTERBOX_BG));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let buf = frame.buffer_mut();
    let label_style = Style::default()
        .fg(colors::PANEL_TEXT)
        .bg(colors::LETTERBOX_BG);
    let mut row: u16 = 0;

    write_line(buf, inner, &mut row, "DEPTH", label_style);
    write_line(
        buf,
        inner,
        &mut row,
        &format!("  {} m (Lv.{})", game.player.depth_m(), game.player.level()),
        label_style,
    );
    write_line(buf, inner, &mut row, "", label_style);

    write_line(buf, inner, &mut row, "SCORE", label_style);
    write_line(
        buf,
        inner,
        &mut row,
        &format!("  {}", format_with_commas(game.player.score)),
        label_style,
    );
    write_line(buf, inner, &mut row, "", label_style);

    write_line(buf, inner, &mut row, "AIR", label_style);
    let ratio = (game.player.oxygen / OXYGEN_MAX).clamp(0.0, 1.0);
    let air_style = Style::default()
        .fg(colors::oxygen_bar_color(ratio))
        .bg(colors::LETTERBOX_BG);
    let gauge = air_gauge_string(ratio, game.player.oxygen_display());
    let air_text = if ratio < 0.3 {
        format!("  {gauge} \u{2620}") // ☠ 骸骨アイコン(spec.md 9.7・6章)
    } else {
        format!("  {gauge}")
    };
    write_line(buf, inner, &mut row, &air_text, air_style);
    write_line(buf, inner, &mut row, "", label_style);

    write_line(buf, inner, &mut row, "LIVES", label_style);
    write_line(
        buf,
        inner,
        &mut row,
        &format!("  \u{2665} \u{d7}{}", game.player.lives),
        label_style,
    );
    write_line(buf, inner, &mut row, "", label_style);

    write_line(buf, inner, &mut row, "TIME", label_style);
    let elapsed = game.player.elapsed_seconds as u32;
    write_line(
        buf,
        inner,
        &mut row,
        &format!("  {:02}:{:02}", elapsed / 60, elapsed % 60),
        label_style,
    );
    write_line(buf, inner, &mut row, "", label_style);

    // #85(揺れているブロックが浮いたまま落下しない)の調査用(TERM独自拡張。
    // ユーザー指摘: 「フレームのユニーク番号を取得できるようにしておき」)。
    // ブロック状態遷移ログ(debug_log)の記録と突き合わせるための番号を表示する。
    write_line(buf, inner, &mut row, "FRAME", label_style);
    write_line(
        buf,
        inner,
        &mut row,
        &format!("  {}", game.debug_frame()),
        label_style,
    );
}

/// `inner`の`*row`行目(0始まり)へ、幅いっぱいにパディングした1行を明示スタイルで書く。
/// 書けたかどうかに関わらず`*row`を1進める(9.6「trailingの余白にも明示的に背景色」)。
fn write_line(buf: &mut Buffer, inner: Rect, row: &mut u16, text: &str, style: Style) {
    if *row < inner.height {
        let y = inner.y + *row;
        let width = inner.width as usize;
        let mut padded: String = text.chars().take(width).collect();
        let printed = padded.chars().count();
        if printed < width {
            padded.push_str(&" ".repeat(width - printed));
        }
        buf.set_string(inner.x, y, padded, style);
    }
    *row += 1;
}

/// 酸素ゲージの文字列表現(spec.md 9.7、幅固定10セル分)。`[########░░] 82%`のような形式。
fn air_gauge_string(ratio: f32, percent: u32) -> String {
    const TOTAL: usize = 10;
    let filled = ((ratio * TOTAL as f32).round() as usize).min(TOTAL);
    let empty = TOTAL - filled;
    format!(
        "[{}{}] {}%",
        "#".repeat(filled),
        "\u{2591}".repeat(empty),
        percent
    )
}

/// スコアを3桁区切りカンマ付きで表示する(spec.md 9.7の表示例「1,230」)。
fn format_with_commas(value: u64) -> String {
    let digits = value.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(bytes.len() + bytes.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

// ---------------------------------------------------------------------------
// オーバーレイ(ポーズ/ゲームオーバー/クリア)
// ---------------------------------------------------------------------------

fn draw_overlay(frame: &mut Frame, area: Rect, title: &str, hints: &[&str]) {
    let overlay_area = centered_rect(40, 20, area);
    frame.render_widget(Clear, overlay_area);

    let text_style = Style::default()
        .fg(colors::PANEL_TEXT)
        .bg(colors::LETTERBOX_BG);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(colors::PANEL_BORDER)
                .bg(colors::LETTERBOX_BG),
        )
        .style(Style::default().bg(colors::LETTERBOX_BG));

    let mut lines = vec![Line::from(Span::styled(title, text_style))];
    lines.extend(
        hints
            .iter()
            .map(|hint| Line::from(Span::styled(*hint, text_style))),
    );

    let paragraph = Paragraph::new(lines)
        .block(block)
        .style(Style::default().bg(colors::LETTERBOX_BG))
        .alignment(Alignment::Center);
    frame.render_widget(paragraph, overlay_area);
}

/// チェックポイント(100mごと)到達演出のバナー(TERM独自拡張。#178。ユーザー指摘:
/// 「100mごとのゴールSEと演出、アニメーションする」)。`draw_overlay`より一回り小さい
/// 箱を短時間(`checkpoint_flash_depth_m`がSomeの間)だけ中央に重ねるだけで、盤面
/// 自体(`draw_field`)は裏で通常通り動き続ける(押し潰し演出等と同じく、周囲の
/// 落下アニメーションを止めない設計方針)。
fn draw_checkpoint_banner(frame: &mut Frame, area: Rect, depth_m: usize) {
    let banner_area = centered_rect(30, 12, area);
    frame.render_widget(Clear, banner_area);

    let text_style = Style::default()
        .fg(colors::PANEL_TEXT)
        .bg(colors::LETTERBOX_BG);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(colors::PANEL_BORDER)
                .bg(colors::LETTERBOX_BG),
        )
        .style(Style::default().bg(colors::LETTERBOX_BG));

    let paragraph = Paragraph::new(Line::from(Span::styled(
        format!("- {depth_m}m -"),
        text_style,
    )))
    .block(block)
    .style(Style::default().bg(colors::LETTERBOX_BG))
    .alignment(Alignment::Center);
    frame.render_widget(paragraph, banner_area);
}

/// GameOverダイアログ(TERM独自拡張)。「タイトルへ戻る」「その場から復活」の2択を
/// 表示し、現在選択中の項目を反転表示(カーソル代わり)する。
fn draw_game_over_overlay(frame: &mut Frame, area: Rect, selection: GameOverChoice) {
    let overlay_area = centered_rect(40, 25, area);
    frame.render_widget(Clear, overlay_area);

    let text_style = Style::default()
        .fg(colors::PANEL_TEXT)
        .bg(colors::LETTERBOX_BG);
    let selected_style = Style::default()
        .fg(colors::LETTERBOX_BG)
        .bg(colors::PANEL_TEXT);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(colors::PANEL_BORDER)
                .bg(colors::LETTERBOX_BG),
        )
        .style(Style::default().bg(colors::LETTERBOX_BG));

    let choice_line = |label: &str, is_selected: bool| {
        let prefix = if is_selected { "> " } else { "  " };
        let style = if is_selected {
            selected_style
        } else {
            text_style
        };
        Line::from(Span::styled(format!("{prefix}{label}"), style))
    };

    let paragraph = Paragraph::new(vec![
        Line::from(Span::styled("GAME OVER", text_style)),
        Line::from(""),
        choice_line("タイトルへ戻る", selection == GameOverChoice::BackToTitle),
        choice_line("その場から復活", selection == GameOverChoice::Revive),
        Line::from(""),
        Line::from(Span::styled("↑↓で選択 / Enterで決定", text_style)),
    ])
    .block(block)
    .style(Style::default().bg(colors::LETTERBOX_BG))
    .alignment(Alignment::Center);
    frame.render_widget(paragraph, overlay_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::FIELD_WIDTH_DEFAULT as FIELD_WIDTH;

    #[test]
    fn player_sprite_lines_are_always_exactly_one_logical_cell_wide() {
        // #160でヘルメットの縁(丸括弧)を追加した際、幅がCELL_W(4文字)からずれると
        // 隣のセルとの描画位置がずれてしまうため回帰確認する。
        let directions = [
            Direction::Up,
            Direction::Down,
            Direction::Left,
            Direction::Right,
        ];
        for dir in directions {
            for drilling_frame in [None, Some(false), Some(true)] {
                let sprite = player_sprite(dir, drilling_frame);
                for line in sprite {
                    assert_eq!(
                        line.chars().count(),
                        CELL_W as usize,
                        "{dir:?}/{drilling_frame:?}の行\"{line}\"がCELL_W({CELL_W})文字ではない"
                    );
                }
            }
        }
    }

    #[test]
    fn player_sprite_keeps_the_eyes_open_on_the_leading_edge_facing_the_drill_direction() {
        // ユーザー指摘: 「キャラがせめて、ドリルかなんか、それっぽいやつにして
        // ほしい ホリススムくんのシルエットに見えたらなおよし」(#160)。ヘルメットの
        // 縁(丸括弧)は進行方向側を開けたままにし、ドリルの刃が突き出る側だと
        // わかるようにする。
        assert!(player_sprite(Direction::Left, None)[0].starts_with('<'));
        assert!(player_sprite(Direction::Right, None)[0].ends_with('>'));
    }

    #[test]
    fn star_glyph_toggles_every_sparkle_period_starting_from_visible() {
        // ユーザー指摘: 「スターブロックは消えるまえからキラキラしてほしい」。
        // 画面内に入った直後(visible_ms=0)から既にキラキラの切り替えが起きている
        // ことを確認する。
        assert_eq!(star_glyph(0), '☆');
        assert_eq!(star_glyph(STAR_SPARKLE_PERIOD_MS - 1), '☆');
        assert_eq!(star_glyph(STAR_SPARKLE_PERIOD_MS), '★');
        assert_eq!(star_glyph(STAR_SPARKLE_PERIOD_MS * 2 - 1), '★');
        assert_eq!(star_glyph(STAR_SPARKLE_PERIOD_MS * 2), '☆');
    }

    #[test]
    fn star_sparkle_content_staggers_the_four_positions_instead_of_flashing_in_unison() {
        // ユーザー指摘: 「スター化したブロックはもっとキラキラしたモーションして
        // ほしい 本当に氷見たく いまのままだとただの白い正方形だ」。4マスが完全に
        // 同時に切り替わってしまうと結局「均一な四角」にしか見えないため、位置に
        // よって☆/★の切り替わるタイミングがずれている(=ある瞬間には両方の記号が
        // 混在する)ことを確認する。
        let mut saw_mixed = false;
        for visible_ms in (0..STAR_SPARKLE_PERIOD_MS).step_by(10) {
            let content = star_sparkle_content(visible_ms);
            let flat = [content[0][0], content[0][1], content[1][0], content[1][1]];
            if flat.contains(&'☆') && flat.contains(&'★') {
                saw_mixed = true;
                break;
            }
        }
        assert!(
            saw_mixed,
            "位相がずれていれば、ある時点で☆と★が混在する瞬間があるはず"
        );
    }

    #[test]
    fn star_block_has_its_corners_cut_to_the_field_background_not_a_flat_square() {
        // ユーザー指摘: 「いまのままだとただの白い正方形だ」。AIR(#128)・アイテム
        // (#132)と同じく、四隅がフィールド背景色まで欠き取られていることを確認する。
        let inner = Rect::new(0, 0, 4, 2);
        let mut buf = Buffer::empty(inner);
        let bg = colors::star_bg(0, STAR_VISIBLE_GRACE_MS, STAR_MELT_DURATION_MS);
        draw_rounded_unit(&mut buf, 0, 0, star_sparkle_content(0), colors::STAR_FG, bg);

        for &(x, y) in &[(0u16, 0u16), (3, 0), (0, 1), (3, 1)] {
            assert_eq!(
                buf.cell(Position::new(x, y)).unwrap().bg,
                colors::FIELD_EMPTY_BG,
                "四隅({x},{y})はフィールド背景色まで欠き取られているはず"
            );
        }
    }

    #[test]
    fn bomb_roll_is_bouncing_up_hops_multiple_times_while_settling_by_the_end() {
        // ユーザー指摘: 「爆弾がぽーんぽーんぽんぽんころころ...って弾ませながら
        // モーションがないと回転寿司みたいにすーって入ってきちゃ駄目」。転がる
        // 区間(t=0.0〜1.0)の間に複数回跳ね、設置直前(t=1.0)には必ず地面に
        // 着地している(跳ねていない)ことを確認する。
        assert!(bomb_roll_is_bouncing_up(0.0), "転がり始めは跳ねているはず");
        assert!(
            !bomb_roll_is_bouncing_up(0.2),
            "1回目の跳ねの後半は着地しているはず"
        );
        assert!(
            bomb_roll_is_bouncing_up(1.0 / BOMB_ROLL_BOUNCE_COUNT as f32),
            "2回目の跳ねが始まるはず"
        );
        assert!(
            !bomb_roll_is_bouncing_up(1.0),
            "設置直前は必ず着地しているはず"
        );
        assert!(!bomb_roll_is_bouncing_up(0.999), "設置直前は着地に近いはず");
    }

    #[test]
    fn draw_bomb_sprite_fills_the_whole_cell_with_body_color_not_just_the_glyphs() {
        // ユーザー指摘: 「爆弾、背景と同化してるから、もっと輪郭くっきり」。以前は
        // グリフ以外の部分がフィールド背景色のまま透過していたため、本体の輪郭が
        // 背景に溶け込んで見えていた。セル全体の背景が本体色で塗りつぶされている
        // ことを確認する。
        let inner = Rect::new(0, 0, 4, 2);
        let mut buf = Buffer::empty(inner);
        draw_bomb_sprite(
            &mut buf,
            0,
            0,
            colors::BOMB_BODY_FG,
            colors::BOMB_SPARK_DIM,
            0,
        );

        for y in 0..2u16 {
            for x in 0..4u16 {
                let bg = buf.cell(Position::new(x, y)).unwrap().bg;
                assert_eq!(
                    bg,
                    colors::BOMB_BODY_FG,
                    "({x},{y})は本体色で塗りつぶされているはず"
                );
            }
        }
    }

    #[test]
    fn bomb_body_color_flashes_red_only_once_the_fuse_is_almost_out() {
        // ユーザー指摘: 「爆発直前で爆弾がチカチカ激しく赤く光るようにして」(#138)。
        // 残り時間が`BOMB_DANGER_MS`を超えている間は通常色のまま、
        // それを切ったら警告色(赤)と通常色を激しく切り替えることを確認する。
        assert_eq!(
            bomb_body_color(BOMB_DANGER_MS + 1),
            colors::BOMB_BODY_FG,
            "閾値を超えている間は通常色のはず"
        );
        assert_eq!(
            bomb_body_color(BOMB_DANGER_MS),
            colors::BOMB_BODY_DANGER_FG,
            "閾値ちょうどでは警告色に切り替わっているはず"
        );
        assert_eq!(
            bomb_body_color(BOMB_BODY_FLASH_PERIOD_MS - 1),
            colors::BOMB_BODY_DANGER_FG
        );
        assert_eq!(
            bomb_body_color(0),
            colors::BOMB_BODY_DANGER_FG,
            "起爆直前は警告色のはず"
        );
        assert_eq!(
            bomb_body_color(BOMB_BODY_FLASH_PERIOD_MS),
            colors::BOMB_BODY_FG,
            "半周期ずれた時点では通常色に戻り、点滅していることが確認できるはず"
        );
    }

    /// テスト用: プレイヤー周辺(±`STAR_VISIBLE_RANGE_ROWS`)を`fill_col`以外は全て
    /// 岩で埋め、指定の1マスだけをEmptyにしたうえで`debug_place_bomb`を呼び、
    /// その1マスへ確実にボムを設置する(`debug_place_bomb_spawns_at_the_only_empty_cell_within_visible_range`
    /// と同じ考え方)。
    fn place_bomb_at(game: &mut Game, row: usize, fill_col: usize) {
        let range = crate::constants::STAR_VISIBLE_RANGE_ROWS;
        for r in (game.player.row - range)..=(game.player.row + range) {
            for c in 0..game.board.width() {
                game.board.rows[r][c] = BoardCell::Rock { hits: 0 };
            }
        }
        game.board.rows[row][fill_col] = BoardCell::Empty;
        game.debug_place_bomb();
    }

    #[test]
    fn off_screen_bomb_column_flashes_red_only_while_blink_is_on() {
        // ユーザー指摘(#175): 「知らない間に画面外に爆弾がいるので縦列を赤く
        // ピカピカさせること」。top_rowより浅い(=まだスクロールインしていない
        // 画面外)位置にあるボムの列は、点滅周期に応じて赤く塗られる。
        let mut game = Game::new(1);
        game.player.row = 500;
        place_bomb_at(&mut game, 490, 3); // top_row(495)より浅い = 画面外
        assert_eq!(
            game.bombs().len(),
            1,
            "テスト前提: ボムが1個設置されていること"
        );

        let inner = Rect::new(0, 0, 20, 20);
        let top_row = 495;
        let visible_rows = 10;

        game.player.elapsed_seconds = 0.0;
        let mut buf_on = Buffer::empty(inner);
        draw_off_screen_bomb_warnings(&mut buf_on, inner, top_row, visible_rows, &game);
        assert!(
            buf_on
                .content
                .iter()
                .any(|c| c.bg == colors::BOMB_BODY_DANGER_FG),
            "点滅ON中は画面外ボムの列が赤く塗られるはず"
        );

        game.player.elapsed_seconds = OFF_SCREEN_BOMB_WARNING_BLINK_MS as f32 / 1000.0;
        let mut buf_off = Buffer::empty(inner);
        draw_off_screen_bomb_warnings(&mut buf_off, inner, top_row, visible_rows, &game);
        assert!(
            !buf_off
                .content
                .iter()
                .any(|c| c.bg == colors::BOMB_BODY_DANGER_FG),
            "点滅OFF中は赤く塗られないはず(点滅していることの確認)"
        );
    }

    #[test]
    fn on_screen_bomb_does_not_trigger_the_off_screen_column_warning() {
        // 画面内(top_row以降)にあるボムは、この警告表示の対象にならないはず
        // (画面内は既に見えているので警告の意味が無いため)。
        let mut game = Game::new(1);
        game.player.row = 500;
        game.player.elapsed_seconds = 0.0;
        place_bomb_at(&mut game, 500, 3); // top_row(495)以降 = 画面内
        assert_eq!(
            game.bombs().len(),
            1,
            "テスト前提: ボムが1個設置されていること"
        );

        let inner = Rect::new(0, 0, 20, 20);
        let mut buf = Buffer::empty(inner);
        draw_off_screen_bomb_warnings(&mut buf, inner, 495, 10, &game);

        assert!(
            !buf.content
                .iter()
                .any(|c| c.bg == colors::BOMB_BODY_DANGER_FG),
            "画面内のボムでは警告表示しないはず"
        );
    }

    #[test]
    fn draw_bomb_sprite_crackle_alternates_the_spark_glyph_and_position_over_time() {
        // ユーザー指摘: 「火花ちりちりアニメーションさせて」。異なる`crackle_ms`を
        // 渡すと、火花の位置(左右どちらのマス)かグリフが変わることを確認する。
        let inner = Rect::new(0, 0, 4, 2);
        let mut buf_a = Buffer::empty(inner);
        draw_bomb_sprite(
            &mut buf_a,
            0,
            0,
            colors::BOMB_BODY_FG,
            colors::BOMB_SPARK_DIM,
            0,
        );
        let mut buf_b = Buffer::empty(inner);
        draw_bomb_sprite(
            &mut buf_b,
            0,
            0,
            colors::BOMB_BODY_FG,
            colors::BOMB_SPARK_DIM,
            BOMB_CRACKLE_FRAME_MS,
        );

        let symbols_a: Vec<String> = (0..4)
            .map(|x| {
                buf_a
                    .cell(Position::new(x, 0))
                    .unwrap()
                    .symbol()
                    .to_string()
            })
            .collect();
        let symbols_b: Vec<String> = (0..4)
            .map(|x| {
                buf_b
                    .cell(Position::new(x, 0))
                    .unwrap()
                    .symbol()
                    .to_string()
            })
            .collect();
        assert_ne!(
            symbols_a, symbols_b,
            "crackle_msが進むと上段の見た目が変わるはず"
        );
    }

    fn board_with(rows: usize) -> Board {
        Board {
            rows: vec![vec![BoardCell::Empty; FIELD_WIDTH]; rows],
            width: FIELD_WIDTH,
        }
    }

    #[test]
    fn fill_bedrock_ground_paints_the_whole_cell_with_the_ground_texture_colors() {
        // ユーザー指摘(#182): 「最終ゴールは地底の地面を表示してクリアした感じにして
        // ほしい」。地底の地面セルは単色の空白ではなく、専用の色(BEDROCK_GROUND_BG/FG)
        // でハッチング模様に塗りつぶされることを確認する。
        let inner = Rect::new(0, 0, CELL_W, CELL_H);
        let mut buf = Buffer::empty(inner);

        fill_bedrock_ground(&mut buf, 0, 0);

        for cell in buf.content.iter() {
            assert_eq!(cell.bg, colors::BEDROCK_GROUND_BG);
            assert_eq!(cell.fg, colors::BEDROCK_GROUND_FG);
            assert_ne!(cell.symbol(), " ", "単色の空白ではなく地面らしい模様のはず");
        }
    }

    #[test]
    fn is_checkpoint_safe_zone_row_covers_only_the_checkpoint_safe_zone_band_excluding_the_bonus_floor()
     {
        // ユーザー指摘(#186): 「100mごとの先はどうせクリアするのでいったん何もなし
        // (地面みたいにしてほしい)」。各チェックポイント(100mごと)通過後の安全地帯
        // (CHECKPOINT_SAFE_ZONE_M行)だけが対象で、その手前・その先・500mの
        // ボーナスフロアは対象外のはず。
        assert!(!is_checkpoint_safe_zone_row(0));
        assert!(!is_checkpoint_safe_zone_row(99));
        assert!(is_checkpoint_safe_zone_row(100));
        assert!(is_checkpoint_safe_zone_row(
            100 + crate::constants::CHECKPOINT_SAFE_ZONE_M - 1
        ));
        assert!(!is_checkpoint_safe_zone_row(
            100 + crate::constants::CHECKPOINT_SAFE_ZONE_M
        ));
        assert!(
            !is_checkpoint_safe_zone_row(500),
            "500mはボーナスフロアなので対象外のはず"
        );
        assert!(is_checkpoint_safe_zone_row(600));
    }

    #[test]
    fn falling_diamond_still_shows_its_glyph_not_just_a_flat_fill() {
        // ユーザー指摘: 「落下アニメーションで模様が消えて、色味だけでしか認識できない」
        // 「あいまいな物体が落ちているように見える」。落下中も静止時と同じグリフ
        // (ダイヤなら◆)で描画されることを確認する。
        let mut game = Game::new(1);
        for row in game.board.rows.iter_mut() {
            for cell in row.iter_mut() {
                *cell = BoardCell::Empty;
            }
        }
        game.player.row = 1;
        game.player.col = 5;
        game.board.rows[0][3] = BoardCell::Diamond;

        let tick = (crate::constants::SHAKE_TICKS as u64 + 1) * crate::constants::FALL_TICK_MS + 10;
        game.update(std::time::Duration::from_millis(tick));

        let moved_map: HashMap<Pos, Pos> = game.recently_moved_blocks().iter().copied().collect();
        assert!(!moved_map.is_empty(), "ダイヤが落下しているはず");

        let inner = Rect::new(0, 0, 20, 10);
        let mut buf = Buffer::empty(inner);
        draw_falling_blocks(&mut buf, inner, 0, 10, &game, &moved_map);

        let has_diamond_glyph = buf.content.iter().any(|cell| cell.symbol() == "◆");
        assert!(
            has_diamond_glyph,
            "落下中もダイヤの◆グリフが描画されているはず"
        );
    }

    #[test]
    fn falling_block_that_auto_vanishes_on_the_same_tick_it_lands_still_renders_its_fall() {
        // ユーザー指摘(#172): 「崩れてきたブロックが、接地する1コマ前でスルスルと
        // 消えてしまう」。着地と同一tickで4連結自動消滅すると盤面は既にEmptyになるが、
        // それ以前は`draw_falling_blocks`が「盤面がEmpty=描画すべきものがない」と
        // 早合点して落下描画自体を丸ごとスキップしていた(実際には最後まで落ちきる
        // 見た目を出したい)。落下中も消滅直前の色ブロックの背景色で描画され続ける
        // ことを確認する。
        let mut game = Game::new(1);
        // row2を最深行にする(=常に支持される)ことで、着地を待つ静的な赤ブロックの
        // 支えを岩ブロックなしに単純化する(board.rsの`empty_board`系テストと同じ考え方)。
        game.board.rows.truncate(3);
        for row in game.board.rows.iter_mut() {
            for cell in row.iter_mut() {
                *cell = BoardCell::Empty;
            }
        }
        game.player.row = 0;
        game.player.col = 5;

        // 列0: 落下してくる赤ブロック(row0から最深行row2まで2マス落下し、着地先
        // (2,0)で(2,1)(2,2)(2,3)と4連結して消滅する)。
        game.board.rows[0][0] = BoardCell::Color(ColorKind::Red);
        // 列1〜3: 着地を待つ静的な赤ブロック(最深行=row2に置くことで常に支持され
        // 自身は落下しない)。
        for col in 1..=3 {
            game.board.rows[2][col] = BoardCell::Color(ColorKind::Red);
        }

        // 揺れ(SHAKE_TICKS)を経て、row0→row1→row2と2マス連続で落下しきる分の
        // 時間を与える(#31: 落下開始後は毎マス揺れ直さず連続で落ち続ける)。
        let tick = (crate::constants::SHAKE_TICKS as u64 + 2) * crate::constants::FALL_TICK_MS + 10;
        game.update(std::time::Duration::from_millis(tick));

        let moved_map: HashMap<Pos, Pos> = game.recently_moved_blocks().iter().copied().collect();
        assert!(
            !moved_map.is_empty(),
            "赤ブロックが(0,0)から落下しているはず"
        );
        assert_eq!(
            game.board.cell(2, 0),
            BoardCell::Empty,
            "着地と同一tickで4連結消滅し、盤面は既にEmptyになっているはず"
        );

        let inner = Rect::new(0, 0, 20, 10);
        let mut buf = Buffer::empty(inner);
        draw_falling_blocks(&mut buf, inner, 0, 10, &game, &moved_map);

        let red_bg = colors::fill_color(ColorKind::Red);
        let has_red_fill = buf
            .content
            .iter()
            .any(|cell| cell.bg == red_bg && cell.symbol() != " ");
        assert!(
            has_red_fill,
            "着地と同一tickで消滅していても、落下中は赤ブロックとして描画され続けるはず"
        );
    }

    #[test]
    fn oxygen_capsule_has_its_corners_cut_to_the_field_background_not_a_flat_square() {
        // ユーザー指摘: 「AIRはカプセルの形状をしていてほしい 正方形ではなくて」。
        // #106時点は角の罫線glyphを丸めるだけでセル自体の背景は正方形のまま
        // 塗りつぶされていたため、依然として正方形に見えていた。四隅のセルの
        // 背景色がフィールド背景色(`FIELD_EMPTY_BG`)まで欠き取られていることを
        // 確認する(中央2列×2行だけが酸素カプセルの地色`OXYGEN_BG`のまま残るはず)。
        let inner = Rect::new(0, 0, 4, 2);
        let mut buf = Buffer::empty(inner);
        draw_rounded_unit(
            &mut buf,
            0,
            0,
            [['◜', '◝'], ['◟', '◞']],
            colors::OXYGEN_FG,
            colors::OXYGEN_BG,
        );

        for &(x, y) in &[(0u16, 0u16), (3, 0), (0, 1), (3, 1)] {
            let bg = buf.cell(Position::new(x, y)).unwrap().bg;
            assert_eq!(
                bg,
                colors::FIELD_EMPTY_BG,
                "四隅({x},{y})はフィールド背景色まで欠き取られているはず"
            );
        }
        for &(x, y) in &[(1u16, 0u16), (2, 0), (1, 1), (2, 1)] {
            let bg = buf.cell(Position::new(x, y)).unwrap().bg;
            assert_eq!(
                bg,
                colors::OXYGEN_BG,
                "中央2列×2行({x},{y})はカプセル本体色のまま残るはず"
            );
        }
    }

    #[test]
    fn item_blocks_have_their_corners_cut_to_the_field_background_not_a_flat_square() {
        // ユーザー指摘: 「C/R/Kアイテムもアイテムっぽい形状に変えよう」(#132)。
        // AIR(#128)と同じく、C/R/Kアイテムも四隅がフィールド背景色まで欠き取られ、
        // 単なる正方形の塗りつぶしでなくなっていることを確認する。
        let items = [
            (
                [['↑', '↑'], ['R', 'R']],
                colors::ITEM_CLEAR_ABOVE_FG,
                colors::ITEM_CLEAR_ABOVE_BG,
            ),
            (
                [['◐', '◑'], ['C', 'C']],
                colors::ITEM_UNIFY_COLORS_FG,
                colors::ITEM_UNIFY_COLORS_BG,
            ),
            (
                [['☆', '☆'], ['K', 'K']],
                colors::ITEM_STARIFY_SCREEN_FG,
                colors::ITEM_STARIFY_SCREEN_BG,
            ),
        ];
        for (content, fg, bg) in items {
            let inner = Rect::new(0, 0, 4, 2);
            let mut buf = Buffer::empty(inner);
            draw_rounded_unit(&mut buf, 0, 0, content, fg, bg);

            for &(x, y) in &[(0u16, 0u16), (3, 0), (0, 1), (3, 1)] {
                assert_eq!(
                    buf.cell(Position::new(x, y)).unwrap().bg,
                    colors::FIELD_EMPTY_BG,
                    "四隅({x},{y})はフィールド背景色まで欠き取られているはず"
                );
            }
            for &(x, y) in &[(1u16, 0u16), (2, 0), (1, 1), (2, 1)] {
                assert_eq!(
                    buf.cell(Position::new(x, y)).unwrap().bg,
                    bg,
                    "中央2列×2行({x},{y})はアイテム本体色のまま残るはず"
                );
            }
        }
    }

    // --- 設定画面のカーソル移動(TERM独自拡張) ---

    #[test]
    fn settings_choice_cycle_back_is_the_exact_reverse_of_cycle() {
        // ユーザー指摘: 「設定画面でカーソル↑おしても下いくんやけど」。cycle_back()は
        // cycle()の逆方向であり、どの項目から始めても cycle().cycle_back() で元へ戻る。
        let all = [
            SettingsChoice::Music,
            SettingsChoice::Se,
            SettingsChoice::RockRate,
            SettingsChoice::AirRate,
            SettingsChoice::StarRate,
            SettingsChoice::DiamondRate,
            SettingsChoice::ItemClearAboveRate,
            SettingsChoice::ItemUnifyColorsRate,
            SettingsChoice::ItemStarifyScreenRate,
            SettingsChoice::ColorCount,
            SettingsChoice::ColorClusterRate,
            SettingsChoice::FieldWidth,
            SettingsChoice::BlockFallSpeed,
            SettingsChoice::PlayerFallSpeed,
            SettingsChoice::MoveSpeed,
            SettingsChoice::DodgeRecoveryMs,
            SettingsChoice::BombRate,
            SettingsChoice::DebugLogEnabled,
        ];
        for choice in all {
            assert_eq!(choice.cycle().cycle_back(), choice);
            assert_eq!(choice.cycle_back().cycle(), choice);
        }
    }

    #[test]
    fn title_screen_text_overlay_fits_within_a_reasonably_sized_terminal() {
        // #191フォローアップ: アートと文字を黄金比で上下分割するようにしたため、
        // 文字(ロゴ+スタート案内+キーヒント)は画面全体でなく残り38.2%の
        // text_zoneに収まる必要がある。55行はごく一般的なターミナルウィンドウの
        // 高さの目安(#127)。
        const ASSUMED_COMMON_TERMINAL_H: u16 = 55;
        const GAP_ABOVE_PROMPT: u16 = 1;
        const PROMPT_ROWS: u16 = 1;
        const GAP_ABOVE_HINTS: u16 = 2;
        const HINT_ROWS: u16 = 2;

        let art_height = ((ASSUMED_COMMON_TERMINAL_H as u32 * 618) / 1000).max(1) as u16;
        let text_zone_height = ASSUMED_COMMON_TERMINAL_H.saturating_sub(art_height);

        let logo_rows = build_title_logo_lines().len() as u16;
        let content_height =
            logo_rows + GAP_ABOVE_PROMPT + PROMPT_ROWS + GAP_ABOVE_HINTS + HINT_ROWS;

        assert!(
            content_height <= text_zone_height,
            "ロゴ+案内文がtext_zoneの高さ({text_zone_height}行)を超えている\
             (content_height={content_height})"
        );
    }

    #[test]
    fn title_art_lines_fills_the_exact_requested_terminal_size() {
        // #148: アートは画面いっぱいに表示するため、行数・幅とも要求した
        // 端末サイズと1:1で一致するはず。
        let lines = title_art_lines(100, 40);
        assert_eq!(lines.len(), 40);
        assert_eq!(lines[0].spans.len(), 100);
    }

    #[test]
    fn title_art_lines_cache_returns_the_same_size_on_repeated_calls() {
        // 同じ端末サイズでの再呼び出しはキャッシュから返されるが、内容(サイズ)は
        // 変わらないはず(TERM独自拡張。#148。`draw_title`は毎フレーム呼ばれるため
        // 再デコードを避けるキャッシュを持つ)。
        let first = title_art_lines(80, 24);
        let second = title_art_lines(80, 24);
        assert_eq!(first.len(), second.len());
        assert_eq!(first[0].spans.len(), second[0].spans.len());
    }

    #[test]
    fn help_screen_box_is_tall_enough_for_the_jukebox_section() {
        // #151でジュークボックス欄(見出し1+空行1+曲4行)を追加した際、枠の高さが
        // 実際の内容行数を収められているか回帰確認する。操作17行+ジュークボックス
        // 6行+空行1+末尾1行=25行、枠(上下)2行込みで27行必要。
        const REQUIRED_CONTENT_LINES: u16 = 25;
        let area = Rect::new(0, 0, 200, 60);
        let frame_rect = centered_fixed_rect(TOTAL_SCREEN_W, TOTAL_SCREEN_H, area);
        let help_area = centered_rect(90, 90, frame_rect);
        assert!(
            help_area.height >= REQUIRED_CONTENT_LINES + 2,
            "ヘルプ画面の枠が{}行分の内容を収めるには狭すぎる(高さ={})",
            REQUIRED_CONTENT_LINES,
            help_area.height
        );
    }

    #[test]
    fn settings_screen_box_is_tall_enough_for_all_content_lines() {
        // ユーザー指摘: 「設定画面から時間要素の細かいものが結構消えてるぞ」。設定項目が
        // 増えるたびに枠の高さが実際の内容行数を収められているか回帰確認する
        // (#108でアイテム3種のrate行を追加した際、枠が足りず下部のms_line(落下速度等)が
        // クリップして見えなくなっていた)。見出し1+空行1+MUSIC/SE(2)+岩/AIR/スター/
        // ダイヤ(4)+アイテム3種(3)+色数(1)+色結合(1)+列数(1)+落下速度系4種(4)+
        // ボム出現頻度(1)+DEBUG LOG(1、#167)+空行1+ヘルプ2行(2)=24行、
        // 枠(上下)2行込みで26行必要。今後さらに設定を追加したらこの定数も増やすこと。
        const REQUIRED_CONTENT_LINES: u16 = 24;
        let area = Rect::new(0, 0, 200, 60);
        let frame_rect = centered_fixed_rect(TOTAL_SCREEN_W, TOTAL_SCREEN_H, area);
        let settings_area = centered_rect(60, 90, frame_rect);
        assert!(
            settings_area.height >= REQUIRED_CONTENT_LINES + 2,
            "設定画面の枠が{}行分の内容を収めるには狭すぎる(高さ={})",
            REQUIRED_CONTENT_LINES,
            settings_area.height
        );
    }

    // --- フィールド幅(列数)可変レイアウト(TERM独自拡張) ---

    #[test]
    fn field_pane_w_and_total_screen_w_scale_with_field_width() {
        // ユーザー指摘: 「設定値に列の数を変更できるようにして」。列数が増えれば
        // フィールドペイン・フレーム全体の幅も広くなることを確認する。
        assert!(field_pane_w(20) > field_pane_w(12));
        assert!(field_pane_w(12) > field_pane_w(6));
        assert!(total_screen_w(20) > total_screen_w(12));
    }

    #[test]
    fn compute_layout_field_rect_widens_for_a_wider_field() {
        let area = Rect::new(0, 0, 200, 100);
        let narrow = compute_layout(area, 6);
        let wide = compute_layout(area, 20);
        assert!(wide.field_rect.width > narrow.field_rect.width);
    }

    #[test]
    fn compute_layout_visible_rows_never_exceeds_the_spawn_rate_reroll_safe_margin() {
        // ユーザー報告: 「掘っていないのに設置済みブロックが消える/落下する」(#83)。
        // 原因調査の結果、プレイ中の配分率再抽選(reroll_spawn_rates_from)が
        // `player.row + SPAWN_RATE_REROLL_SAFE_MARGIN_ROWS`より先だけを書き換える
        // 前提になっているが、この定数(以前は40)が縮退表示(9.8、幅不足のターミナル
        // では可視行数がターミナルの実高さから動的に計算される)の可視行数と
        // 独立に固定されていたため、非常に縦長のターミナルでは可視行数がこの定数を
        // 上回り、画面内の未掘削ブロックまで書き換わってしまうバグがあった。
        // 現実的なターミナルサイズの範囲(幅は縮退表示に入りやすい50〜120、高さは
        // 十分余裕を見て300行まで)・全field_width設定で、可視行数が安全マージンを
        // 超えないことを回帰確認する。field_widthが大きいほどtotal_wも大きくなり
        // 縮退表示に入りやすくなるため、設定可能な全範囲を確認する。
        for field_width in crate::constants::FIELD_WIDTH_MIN..=crate::constants::FIELD_WIDTH_MAX {
            for width in (50..120u16).step_by(5) {
                for height in (16..300u16).step_by(4) {
                    let area = Rect::new(0, 0, width, height);
                    let plan = compute_layout(area, field_width);
                    assert!(
                        plan.visible_rows <= crate::constants::SPAWN_RATE_REROLL_SAFE_MARGIN_ROWS,
                        "field_width={field_width} width={width} height={height}: \
                         可視行数({})が安全マージン({})を超えている",
                        plan.visible_rows,
                        crate::constants::SPAWN_RATE_REROLL_SAFE_MARGIN_ROWS
                    );
                }
            }
        }
    }

    // --- 揺れ(ぐらぐら)アニメーションのジッター(TERM独自拡張) ---

    #[test]
    fn shake_jitter_x_is_always_within_one_character() {
        for i in 0..200 {
            let elapsed = i as f32 * 0.037;
            for row in 0..3 {
                for col in 0..3 {
                    let jitter = shake_jitter_x(elapsed, row, col);
                    assert!(
                        (-1..=1).contains(&jitter),
                        "jitterは-1〜1の範囲のはず: {jitter}"
                    );
                }
            }
        }
    }

    #[test]
    fn shake_jitter_x_is_deterministic_for_the_same_inputs() {
        assert_eq!(shake_jitter_x(1.234, 5, 7), shake_jitter_x(1.234, 5, 7));
    }

    // --- 9.3 接続マスク(横方向のまとまり確認、spec.md 9.3) ---

    #[test]
    fn conn_mask_treats_same_color_neighbors_as_connected_only() {
        let mut board = board_with(3);
        board.rows[1][1] = BoardCell::Color(ColorKind::Red);
        board.rows[1][2] = BoardCell::Color(ColorKind::Red); // 右隣: 同色
        board.rows[0][1] = BoardCell::Color(ColorKind::Blue); // 上隣: 別色
        // 下隣(row2,col1)はEmptyのまま、左隣(col0)は盤内だがEmpty

        let mask = conn_mask(&board, 1, 1, ColorKind::Red);

        assert!(mask.right, "同色の右隣は接続とみなすはず");
        assert!(!mask.up, "別色の上隣は接続とみなさないはず");
        assert!(!mask.down, "Emptyの下隣は接続とみなさないはず");
        assert!(!mask.left, "Emptyの左隣は接続とみなさないはず");
    }

    #[test]
    fn conn_mask_out_of_bounds_neighbor_is_not_connected() {
        let mut board = board_with(2);
        board.rows[0][0] = BoardCell::Color(ColorKind::Green);

        let mask = conn_mask(&board, 0, 0, ColorKind::Green);

        assert!(!mask.up, "盤外(row=-1)は接続とみなさない");
        assert!(!mask.left, "盤外(col=-1)は接続とみなさない");
    }

    // --- 横に連結した同色セルは境界を消して背景色を共有する(spec.md 9.3) ---

    #[test]
    fn horizontally_connected_same_color_cells_form_one_unbroken_border_without_a_seam() {
        // 縦方向には繋がっていない(上下ともEmpty)横1行だけの連結の場合、角(x=3, x=4)は
        // 「上下どちらも非接続」なので内部fill(空白)にはならず、どちらも同じ'─'になる
        // (spec.md 9.3の角判定は縦横2方向の組み合わせで決まり、内部fillは両方向とも
        // 接続している場合のみ)。ここで確認したいのは、これが2マスにまたがる
        // "1本の途切れないボーダー"として繋がって見えること、すなわち継ぎ目に
        // 縦線'│'が入って区切られてしまわないこと。
        let mut board = board_with(3);
        board.rows[1][0] = BoardCell::Color(ColorKind::Red);
        board.rows[1][1] = BoardCell::Color(ColorKind::Red);
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 2));

        draw_color_block(&mut buf, 0, 0, &board, 1, 0, ColorKind::Red);
        draw_color_block(&mut buf, 4, 0, &board, 1, 1, ColorKind::Red);

        for y in [0u16, 1] {
            for x in [3u16, 4] {
                let symbol = buf.cell(Position::new(x, y)).unwrap().symbol();
                assert_eq!(
                    symbol, "─",
                    "継ぎ目(x={x},y={y})は縦線で区切られず、横線で繋がっているはず"
                );
            }
        }

        // 継ぎ目をまたぐ左右の背景色も一致し、色ムラなく1つの塊に見える。
        let left_bg = buf.cell(Position::new(3, 0)).unwrap().bg;
        let right_bg = buf.cell(Position::new(4, 0)).unwrap().bg;
        assert_eq!(
            left_bg, right_bg,
            "継ぎ目の左右で背景色(シェーディング)が食い違ってはいけない"
        );
    }

    #[test]
    fn horizontally_isolated_color_cell_keeps_its_border() {
        let mut board = board_with(3);
        board.rows[1][0] = BoardCell::Color(ColorKind::Red);
        board.rows[1][1] = BoardCell::Color(ColorKind::Blue); // 別色なので接続しない
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 2));

        draw_color_block(&mut buf, 0, 0, &board, 1, 0, ColorKind::Red);

        // 右隣が別色のため、右側の角は罫線(丸み)のままで空白にはならない。
        assert_eq!(buf.cell(Position::new(3, 0)).unwrap().symbol(), "╮");
        assert_eq!(buf.cell(Position::new(3, 1)).unwrap().symbol(), "╯");
    }

    #[test]
    fn horizontally_connected_diamond_cells_form_one_unbroken_border_without_a_seam() {
        // ユーザー指摘: 「ダイヤブロックの見た目を岩ボコのような形状にして」(#141)。
        // 岩ブロック・色ブロックと同じく、隣接するダイヤブロック同士は境界を消して
        // 1つの塊に見えるようにする。継ぎ目に縦線'│'が入って区切られないことを
        // 確認する。
        let mut board = board_with(3);
        board.rows[1][0] = BoardCell::Diamond;
        board.rows[1][1] = BoardCell::Diamond;
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 2));

        draw_diamond_block(&mut buf, 0, 0, &board, 1, 0);
        draw_diamond_block(&mut buf, 4, 0, &board, 1, 1);

        for y in [0u16, 1] {
            for x in [3u16, 4] {
                let symbol = buf.cell(Position::new(x, y)).unwrap().symbol();
                assert_eq!(
                    symbol, "─",
                    "継ぎ目(x={x},y={y})は縦線で区切られず、横線で繋がっているはず"
                );
            }
        }
    }

    #[test]
    fn horizontally_isolated_diamond_cell_keeps_its_border() {
        // ユーザー指摘: 「ダイヤブロックの見た目を岩ボコのような形状にして」(#141)。
        // 隣がダイヤブロックでなければ(#141以前と同じく)角の丸みが残ることを
        // 確認する。
        let mut board = board_with(3);
        board.rows[1][0] = BoardCell::Diamond;
        board.rows[1][1] = BoardCell::Rock { hits: 0 }; // ダイヤではないので接続しない
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 2));

        draw_diamond_block(&mut buf, 0, 0, &board, 1, 0);

        assert_eq!(buf.cell(Position::new(3, 0)).unwrap().symbol(), "╮");
        assert_eq!(buf.cell(Position::new(3, 1)).unwrap().symbol(), "╯");
    }
}

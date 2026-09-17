//! ratatui描画(spec.md 9章 TUI仕様)。
//! 1論理セルを横4文字×縦2ターミナル行の大型ブロックとして描画する(9.2)。

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

/// 設定画面・ヘルプ画面のオーバーレイ枠の高さ(`centered_rect`のパーセント指定)。
/// 内容行数が増えて枠に収まらなくなったら上げる(収まっているかは
/// `settings_screen_box_is_tall_enough_...` / `help_screen_box_is_tall_enough_...`で確認する)。
const SETTINGS_OVERLAY_PERCENT_Y: u16 = 95;
const HELP_OVERLAY_PERCENT_Y: u16 = 95;

/// 巻き戻し中オーバーレイ(#233)の枠の高さ(行数)。内容2行+上下ボーダー2行。
/// 中央ではなく画面下端に寄せるため、割合ではなく固定行数で指定する(GameOver
/// ダイアログ等、中央に出る他のオーバーレイと重ならないようにするため)。
const REWIND_OVERLAY_H: u16 = 4;

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
    /// オーバーレイ(ポーズ/ゲームオーバー等)を中央配置する基準となるゲーム画面全体のフレーム(9.10)。
    game_frame: Rect,
}

/// フィールドペイン幅(列数×4文字+左右ボーダー2文字、9.2)。設定の列数に応じて可変になる。
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

/// `area`の下端に寄せた、幅`percent_x`%・高さ`height`行の矩形(TERM独自拡張。#233)。
/// 中央に出る他のオーバーレイ(ポーズ・ゲームオーバー等)と重ならない位置へ案内を
/// 出したいときに使う。`area`より高い指定は`area`いっぱいにクランプする。
fn bottom_anchored_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let width = (area.width as u32 * percent_x as u32 / 100) as u16;
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + area.height - height,
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

/// `autoplay_enabled`はオートプレイが動作中かどうか。AIの実体は`Game`の外
/// (`autoplay::Autopilot`)にあるためgameからは判定できず、main.rsから渡す。
/// 無敵状態・回避したミス数は`Game`自身が持っているのでここでは受け取らない。
pub fn draw(
    frame: &mut Frame,
    game: &Game,
    music_enabled: bool,
    se_enabled: bool,
    autoplay_enabled: bool,
) {
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
    draw_status(frame, plan.hud_rect, game, autoplay_enabled);

    // チェックポイント(100mごと)到達演出。短時間のバナー表示だけで、
    // 盤面(draw_field)自体は裏で通常通り動き続けている。
    if let Some(depth_m) = game.checkpoint_flash_depth_m() {
        draw_checkpoint_banner(frame, plan.game_frame, depth_m);
    }

    match game.status {
        GameStatus::Paused => draw_overlay(
            frame,
            plan.game_frame,
            "PAUSED",
            &[
                "何かキーを押すと再開 / Escキーでタイトルへ",
                &format!(
                    "Mキーで音楽{} / Eキーで効果音{}",
                    on_off_label(music_enabled),
                    on_off_label(se_enabled)
                ),
                "Sキーで設定画面 / Hキーでヘルプ",
            ],
        ),
        // 押し潰されてのミスは、GameOverオーバーレイを出す前に一呼吸「潰れた」演出
        // (draw_field内のdraw_player)を見せる(spec.md 5章・9章)。
        GameStatus::GameOver if !game.crush_flash_active() => draw_game_over_overlay(
            frame,
            plan.game_frame,
            game.game_over_selection(),
            // 巻き戻せる状態なら、ダイアログにもその選択肢があることを示す(#233)。
            game.can_start_rewind().then(|| game.rewind_stock()),
        ),
        GameStatus::GameOver => {}
        GameStatus::Cleared => {
            draw_overlay(frame, plan.game_frame, "CLEAR !", &["Escキーでタイトルへ"])
        }
        GameStatus::Playing => {}
    }
}

/// ON/OFF状態を短いラベルにする(spec.md 10章)。
fn on_off_label(enabled: bool) -> &'static str {
    if enabled { "ON" } else { "OFF" }
}

// ---------------------------------------------------------------------------
// タイトル画面(spec.md 1章「Escキーはタイトルへ戻る」の受け皿)
// ---------------------------------------------------------------------------

/// タイトル画面のアートは端末いっぱいに表示し、ロゴ・案内文はその上に重ね描きする(アートの
/// 行数を絞ると低解像度で潰れるため)。アート構築(PNGデコード+Lanczos3リサイズ)は重く、
/// `draw_title`は毎フレーム呼ばれるため、端末サイズが変わらない限り再利用するキャッシュを持つ。
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

/// タイトルワードマーク("MISDRI TERM")を構成する1文字ぶんの罫線フォント(3行×3列)。
/// T/E/R/Mは`vJ39/termmap`のワードマーク(`keymap.rs`の`LOGO`)と同じ字形、I/S/Dは同じ作法
/// (角・ヒゲの罫線文字)で起こしたもの。半角スペースは2列ぶんの空白で単語の区切りに使う。
fn title_logo_glyph(c: char) -> &'static [&'static str; 3] {
    match c {
        'M' => &["┏┳┓", "┃┃┃", "╹╹╹"],
        'I' => &["╺┳╸", " ┃ ", "╺┻╸"],
        'S' => &["┏━╸", "┗━┓", "╺━┛"],
        // 単純な箱形("┏━┓"/"┃ ┃"/"┗━┛")だとOと見分けがつかない("MISORI"に読める)ため、
        // 右側だけ丸角(細線)にして左の角ばった縦棒(太線)とのコントラストでDの丸みを出す。
        'D' => &["┏━╮", "┃ │", "┗━╯"],
        'R' => &["┏━┓", "┣┳┛", "╹┗╸"],
        'T' => &["╺┳╸", " ┃ ", " ╹ "],
        'E' => &["┏━╸", "┣╸ ", "┗━╸"],
        _ => &["  ", "  ", "  "],
    }
}

/// "MISDRI TERM"のワードマーク3行を、上ほど明るい金〜赤銅色のグラデーションで組む
/// (ゲーム内のダイヤブロック配色(黄土色系)に寄せた色)。
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
/// 1画面にまとめる)。このタイトル画面上でのみ、Escキーがアプリ終了として扱われる
/// (main.rsの画面遷移)。
pub fn draw_title(frame: &mut Frame) {
    let area = frame.area();

    frame.buffer_mut().set_style(
        area,
        Style::default()
            .fg(colors::LETTERBOX_BG)
            .bg(colors::LETTERBOX_BG),
    );

    // アートを画面いっぱいに表示する。
    let art_lines = title_art_lines(area.width, area.height);
    frame.render_widget(
        Paragraph::new(Text::from(art_lines)).alignment(Alignment::Center),
        area,
    );

    // ロゴ・案内文はアートの上に重ね描きする。パネルの地色(LETTERBOX_BG)がその部分の
    // アートを覆い隠すため、背景の絵柄によらず文字が読める。
    let text_style = Style::default()
        .fg(colors::PANEL_TEXT)
        .bg(colors::LETTERBOX_BG);
    let mut text_lines = build_title_logo_lines().to_vec();
    text_lines.extend([
        Line::from(Span::styled("ミスドリTERM", text_style)),
        Line::from(""),
        Line::from(Span::styled("Enterキーを押してスタート", text_style)),
        Line::from(Span::styled("(Escキーで終了)", text_style)),
        Line::from(Span::styled("(Sキーで設定 / Hキーでヘルプ)", text_style)),
    ]);
    let text_rows = text_lines.len() as u16;

    let text_area = centered_fixed_rect(area.width, text_rows, area);
    frame.render_widget(
        Paragraph::new(text_lines)
            .style(Style::default().bg(colors::LETTERBOX_BG))
            .alignment(Alignment::Center),
        text_area,
    );
}

// ---------------------------------------------------------------------------
// ヘルプ画面
// ---------------------------------------------------------------------------

/// ヘルプ画面のジュークボックスUI状態。カーソル位置(`selection`)と
/// 現在再生中の曲(`playing`、無ければ`None`)を保持する。
pub struct HelpJukeboxState {
    pub selection: usize,
    pub playing: Option<usize>,
}

/// 操作キー・デバッグショートカット一覧のヘルプ画面。`jukebox`が`Some`の時のみジュークボックス
/// 欄を出す(一時停止中はプレイ中BGMと混ざるため、タイトルから開く独立画面のみ)。`standalone`
/// は独立画面(true、Escでタイトルへ)か一時停止オーバーレイ(false、Escは閉じるだけ)かを表す。
pub fn draw_help(frame: &mut Frame, jukebox: Option<&HelpJukeboxState>, standalone: bool) {
    let area = frame.area();

    frame.buffer_mut().set_style(
        area,
        Style::default()
            .fg(colors::LETTERBOX_BG)
            .bg(colors::LETTERBOX_BG),
    );

    let frame_rect = centered_fixed_rect(TOTAL_SCREEN_W, TOTAL_SCREEN_H, area);
    let help_area = centered_rect(90, HELP_OVERLAY_PERCENT_Y, frame_rect);
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
        line("Backspace/U: 巻き戻し(過去の状態へ戻ってやり直す)"),
        line("Esc: タイトルへ戻る/終了"),
        line("S: 設定画面   H: このヘルプ (プレイ中に押すと自動で一時停止する)"),
        line(""),
        heading("== 一時停止中のみ =="),
        line("M: MUSIC ON/OFF   E: SE ON/OFF"),
        line("設定画面: MUSIC/SE・音量・Xブロック・AIR・スター・ダイヤの配分・色数を調整できる"),
        line(""),
        heading("== デバッグショートカット =="),
        line("C: 周辺ブロックを2色に統一   L: ライフ+1   A: AIRを100%に回復"),
        line("R: 自分より上のブロックを全削除   K: 画面内のX/ダイヤを全てスターに"),
        line("B: ボムを画面内のランダムな位置に設置"),
        line("T: オートプレイ ON/OFF   G: 無敵(ミス無効) ON/OFF(Tとは独立)"),
        line("[ / ]: ブロック落下速度 遅く/速く"),
        line("- / =: 自分の落下速度 遅く/速く"),
        line(", / .: 落下待ち時間(揺れ) 長く/短く"),
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
        "Escキーでタイトルへ戻る"
    } else {
        "Escキーで閉じてプレイに戻る"
    }));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .style(Style::default().bg(colors::LETTERBOX_BG))
        .alignment(Alignment::Left);
    frame.render_widget(paragraph, help_area);
}

// ---------------------------------------------------------------------------
// モードセレクト画面。タイトルでEnterを押した直後に経由し、ここで選んだコースの
// ゴール深度で新しいゲームが始まる。
// ---------------------------------------------------------------------------

/// モードセレクト画面での選択(spec.md 1章の確定事実「コースは2種類: 500m
/// (イージー)と1000m(ノーマル)」に対応)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CourseChoice {
    Easy,
    Normal,
}

impl CourseChoice {
    /// ↑/↓・←/→どちらでも切り替える。選択肢が2つだけなので方向を問わず反転すればよい。
    pub fn toggle(self) -> Self {
        match self {
            CourseChoice::Easy => CourseChoice::Normal,
            CourseChoice::Normal => CourseChoice::Easy,
        }
    }

    /// この選択に対応するコースのゴール深度(m)。
    pub fn depth_goal_m(self) -> usize {
        match self {
            CourseChoice::Easy => crate::constants::COURSE_EASY_DEPTH_M,
            CourseChoice::Normal => crate::constants::COURSE_NORMAL_DEPTH_M,
        }
    }

    /// 保存済みのゴール深度から選択を復元する(前回選んだコースを次回起動時の初期選択に
    /// 引き継ぐ)。`COURSE_EASY_DEPTH_M`以下ならイージー、それより大きければノーマルとみなす。
    pub fn from_depth_goal_m(depth_goal_m: usize) -> Self {
        if depth_goal_m <= crate::constants::COURSE_EASY_DEPTH_M {
            CourseChoice::Easy
        } else {
            CourseChoice::Normal
        }
    }
}

/// モードセレクト画面を描画する。`selection`が現在カーソルの当たっている選択肢。
pub fn draw_mode_select(frame: &mut Frame, selection: CourseChoice) {
    let area = frame.area();

    frame.buffer_mut().set_style(
        area,
        Style::default()
            .fg(colors::LETTERBOX_BG)
            .bg(colors::LETTERBOX_BG),
    );

    let frame_rect = centered_fixed_rect(TOTAL_SCREEN_W, TOTAL_SCREEN_H, area);
    let mode_select_area = centered_rect(70, 40, frame_rect);
    frame.render_widget(Clear, mode_select_area);

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

    let option_line = |label: &str, is_selected: bool| {
        let marker = if is_selected { "> " } else { "  " };
        let style = if is_selected {
            selected_style
        } else {
            text_style
        };
        Line::from(Span::styled(format!("{marker}{label}"), style))
    };

    let lines = vec![
        Line::from(Span::styled("== コース選択 ==", heading_style)),
        Line::from(""),
        option_line("500m (イージー)", selection == CourseChoice::Easy),
        option_line("1000m (ノーマル)", selection == CourseChoice::Normal),
        Line::from(""),
        Line::from(Span::styled(
            "↑↓/←→で選択 / Enterで決定 / Escでタイトルへ",
            text_style,
        )),
    ];

    let paragraph = Paragraph::new(lines)
        .block(block)
        .style(Style::default().bg(colors::LETTERBOX_BG))
        .alignment(Alignment::Center);
    frame.render_widget(paragraph, mode_select_area);
}

// ---------------------------------------------------------------------------
// 設定画面
// ---------------------------------------------------------------------------

/// 設定画面での選択項目。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsChoice {
    Music,
    /// MUSIC(BGM)の音量(%、0〜100)。ON/OFFとは別の項目(TERM独自拡張。#224)。
    MusicVolume,
    Se,
    /// SE(効果音)の音量(%、0〜100)。ON/OFFとは別の項目(TERM独自拡張。#224)。
    SeVolume,
    /// Xブロック(岩)の出現率(%)。
    RockRate,
    /// AIR(酸素カプセル)の出現率(%)。
    AirRate,
    /// スターブロックの出現率(%、0まで下げられる)。
    StarRate,
    /// ダイヤブロックの出現率(%、0まで下げられる)。
    DiamondRate,
    /// アイテムブロック(ClearAbove、ショートカットR効果)の出現率(%、0まで下げられる)。
    ItemClearAboveRate,
    /// アイテムブロック(UnifyColors、ショートカットC効果)の出現率(%、同上)。
    ItemUnifyColorsRate,
    /// アイテムブロック(StarifyScreen、ショートカットK効果)の出現率(%、同上)。
    ItemStarifyScreenRate,
    /// 出現する色ブロックの色数(1〜4)。
    ColorCount,
    /// 色ブロックの結合しやすさ(%、0まで下げられる)。
    ColorClusterRate,
    /// フィールド幅(列数)。新規ゲーム開始時にのみ反映される。
    FieldWidth,
    /// ブロック落下速度(tick間隔, ms)。デバッグショートカット([ ])と同じ値を設定画面からも調整する。
    BlockFallSpeed,
    /// キャラ自身の自由落下速度(tick間隔, ms)。デバッグショートカット(-/=)と同じ値を設定画面からも調整する。
    PlayerFallSpeed,
    /// 揺れ時間(支えを失ってから実際に落下し始めるまでの猶予, ms)。デバッグショートカット(, .)と同じ値を設定画面からも調整する。
    ShakeDuration,
    /// 横移動(MoveLeft/MoveRight)のクールダウン間隔(ms、小さいほど速い)。
    MoveSpeed,
    /// 「わ〜!」スライダー演出後、キャラが起き上がるまでの硬直インターバル(ms)。
    DodgeRecoveryMs,
    /// ボム出現頻度(%、0まで下げられる)。
    BombRate,
    /// 調査用のブロック状態遷移ログ(SQLite)を記録するかどうか。
    DebugLogEnabled,
    /// 4連結以上の自動消滅が連鎖するときのインターバル(ms、0=即座に連鎖)。
    ChainVanishInterval,
    /// フレーム巻き戻し(#233)で持てるストック(使用回数)の上限。0=巻き戻し機能OFF。
    RewindStockMax,
}

impl SettingsChoice {
    /// ↓キーでの選択項目の巡回。
    pub fn cycle(self) -> Self {
        match self {
            SettingsChoice::Music => SettingsChoice::MusicVolume,
            SettingsChoice::MusicVolume => SettingsChoice::Se,
            SettingsChoice::Se => SettingsChoice::SeVolume,
            SettingsChoice::SeVolume => SettingsChoice::RockRate,
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
            SettingsChoice::PlayerFallSpeed => SettingsChoice::ShakeDuration,
            SettingsChoice::ShakeDuration => SettingsChoice::MoveSpeed,
            SettingsChoice::MoveSpeed => SettingsChoice::DodgeRecoveryMs,
            SettingsChoice::DodgeRecoveryMs => SettingsChoice::BombRate,
            SettingsChoice::BombRate => SettingsChoice::DebugLogEnabled,
            SettingsChoice::DebugLogEnabled => SettingsChoice::ChainVanishInterval,
            SettingsChoice::ChainVanishInterval => SettingsChoice::RewindStockMax,
            SettingsChoice::RewindStockMax => SettingsChoice::Music,
        }
    }

    /// ↑キーでの選択項目の巡回(`cycle`の厳密な逆方向)。
    pub fn cycle_back(self) -> Self {
        match self {
            SettingsChoice::Music => SettingsChoice::RewindStockMax,
            SettingsChoice::RewindStockMax => SettingsChoice::ChainVanishInterval,
            SettingsChoice::ChainVanishInterval => SettingsChoice::DebugLogEnabled,
            SettingsChoice::DebugLogEnabled => SettingsChoice::BombRate,
            SettingsChoice::BombRate => SettingsChoice::DodgeRecoveryMs,
            SettingsChoice::MusicVolume => SettingsChoice::Music,
            SettingsChoice::Se => SettingsChoice::MusicVolume,
            SettingsChoice::SeVolume => SettingsChoice::Se,
            SettingsChoice::RockRate => SettingsChoice::SeVolume,
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
            SettingsChoice::ShakeDuration => SettingsChoice::PlayerFallSpeed,
            SettingsChoice::MoveSpeed => SettingsChoice::ShakeDuration,
            SettingsChoice::DodgeRecoveryMs => SettingsChoice::MoveSpeed,
        }
    }
}

/// 設定画面を描画する。各設定値と、現在選択中の項目をカーソル(反転表示)で示す。
/// `standalone`はタイトルから開いた独立画面(true、Escでタイトルへ戻る)か、
/// 一時停止オーバーレイ(false、Escは閉じてプレイ再開するだけ)かを表す。
#[allow(clippy::too_many_arguments)]
pub fn draw_settings(
    frame: &mut Frame,
    selection: SettingsChoice,
    music_enabled: bool,
    se_enabled: bool,
    music_volume_percent: u32,
    se_volume_percent: u32,
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
    shake_duration_ms: u64,
    move_cooldown_ms: u64,
    dodge_recovery_ms: u64,
    bomb_spawn_rate_percent: u32,
    debug_log_enabled: bool,
    chain_vanish_interval_ms: u64,
    rewind_stock_max: u8,
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
    // 高さが足りないと下部の行が枠からクリップして見えなくなるため、項目追加を見越して
    // 縦に余裕を持たせる(必要行数はテスト`settings_screen_box_is_tall_enough_...`で確認)。
    // #233で項目が22個になり90%(28行)では1行あふれるため95%(30行)へ広げた。
    let settings_area = centered_rect(60, SETTINGS_OVERLAY_PERCENT_Y, frame_rect);
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
        rate_line(
            "MUSIC音量",
            music_volume_percent,
            selection == SettingsChoice::MusicVolume,
        ),
        toggle_line("SE", se_enabled, selection == SettingsChoice::Se),
        rate_line(
            "SE音量",
            se_volume_percent,
            selection == SettingsChoice::SeVolume,
        ),
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
            "落下待ち時間(揺れ)",
            shake_duration_ms,
            selection == SettingsChoice::ShakeDuration,
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
        count_line(
            "巻き戻しストック上限",
            rewind_stock_max,
            selection == SettingsChoice::RewindStockMax,
        ),
        Line::from(""),
        Line::from(Span::styled(
            "↑↓で選択 / MUSIC・SE・DEBUG LOGはSpaceか←→でトグル",
            text_style,
        )),
        Line::from(Span::styled(
            if standalone {
                "配分・音量・色数は←→で調整 / Escでタイトルへ"
            } else {
                "配分・音量・色数は←→で調整 / Escで閉じる"
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

    let cam = field_camera(game, player_screen_row(visible_rows));

    // 直近の重力ティックで落下した(移動後の位置)→(移動前の位置)のマップ。移動後の位置は
    // 静的な通常描画では一旦Emptyとして扱い(まだ到着していない宙にある状態)、実際の内容は
    // このあと`draw_falling_blocks`が移動前→移動後を補間した位置へ重ねて描画する。
    let moved_map: HashMap<Pos, Pos> = game.recently_moved_blocks().iter().copied().collect();

    let buf = frame.buffer_mut();

    draw_static_field(buf, inner, &cam, visible_rows, game, &moved_map);
    draw_falling_blocks(buf, inner, cam.row_f, visible_rows, game, &moved_map);
    draw_bombs(buf, inner, cam.row_f, visible_rows, game);
    draw_player(buf, inner, cam.row_f, game);
    draw_off_screen_bomb_warnings(buf, inner, cam.row_f, visible_rows, game);
}

/// プレイヤーを画面内の何行目に固定表示するか(9.1)。
fn player_screen_row(visible_rows: usize) -> usize {
    (visible_rows * PLAYER_SCREEN_ROW_RATIO_NUM / PLAYER_SCREEN_ROW_RATIO_DEN)
        .min(visible_rows.saturating_sub(1))
}

/// フィールドのスクロール位置(#242)。論理行の整数値でスナップさせると、足元のブロックが
/// 消えてプレイヤーが自由落下するたびに画面全体がセル1つぶん飛び、落下中の他のブロックが
/// 一瞬上へ逆走して見えるため、プレイヤーの補間後の位置から小数で求める。
struct FieldCamera {
    /// 画面最上段に来る論理行(小数)。
    row_f: f32,
    /// `row_f`の整数部。静的セルはこの行から論理行グリッド上に描く。
    top_row: usize,
    /// セル内の半端なスクロール量(端末行数、0〜`CELL_H`)。静的セルはこの分だけ上へずらして転写する。
    dy: u16,
}

fn field_camera(game: &Game, player_screen_row: usize) -> FieldCamera {
    let row_f = (interp_player_row(game) - player_screen_row as f32).max(0.0);
    FieldCamera {
        row_f,
        top_row: row_f.floor() as usize,
        dy: (row_f.fract() * CELL_H as f32).round() as u16,
    }
}

/// プレイヤーの補間後の論理行(整数のマス位置ではなく、移動アニメーション進捗を反映した
/// 小数の位置)。カメラ位置とプレイヤースプライトの描画位置が同じ値を基準にすることで、
/// スクロールを滑らかにしつつプレイヤーは画面内の固定位置に留まる(spec.md 9.2)。
fn interp_player_row(game: &Game) -> f32 {
    let (prev_row, _) = game.render_prev_position();
    let (cur_row, _) = game.player.position();
    prev_row as f32 + (cur_row as f32 - prev_row as f32) * game.move_anim_progress()
}

/// 盤面の静的セル(その場に留まっているマス)を描画する。論理行グリッド上にしか描けない
/// ため、1論理行ぶん高いオフスクリーンバッファへ描いてから、カメラの半端なスクロール量
/// (`cam.dy`)だけ上へずらして`inner`へ転写する(#242)。こうすると端末行単位の中間位置でも
/// 半端なセルが枠線をはみ出して汚さない。
fn draw_static_field(
    buf: &mut Buffer,
    inner: Rect,
    cam: &FieldCamera,
    visible_rows: usize,
    game: &Game,
    moved_map: &HashMap<Pos, Pos>,
) {
    let area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: inner.height + CELL_H,
    };
    let mut off = Buffer::empty(area);
    // セルを描かない領域(縮退表示で盤面が埋まらない場合)が未初期化の色で転写されないよう、
    // フィールド背景色の下地を敷いておく。
    off.set_style(area, Style::default().bg(colors::FIELD_EMPTY_BG));

    // 半端なスクロール量ぶん下からせり上がってくるぶん、可視行数より1行多く描く。
    for screen_row in 0..=visible_rows {
        let y = area.y + screen_row as u16 * CELL_H;
        if y + CELL_H > area.y + area.height {
            break; // 縮退表示でinner.heightが可視行数ぶんに満たない場合の防御
        }

        let board_row = cam.top_row + screen_row;
        for col in 0..game.board.width() {
            let x = area.x + col as u16 * CELL_W;
            if x + CELL_W > area.x + area.width {
                break;
            }

            let cell = if moved_map.contains_key(&(board_row, col)) {
                BoardCell::Empty
            } else if board_row < game.board.depth_rows() {
                game.board.cell(board_row, col)
            } else {
                BoardCell::Empty
            };
            // プレイヤーがいるセルも含め常にそのマス本来の内容を描画し、プレイヤーの
            // スプライトはループの外側で補間アニメーション込みで重ねる(spec.md 9.5)。
            // 支えを失って揺れている(落下開始前の猶予期間中の)ブロックは左右に小刻みなジッターを加える。
            let draw_x = if game.is_cell_shaking(board_row, col) {
                let jitter = shake_jitter_x(game.player.elapsed_seconds, board_row, col);
                (x as i32 + jitter).clamp(
                    area.x as i32,
                    (area.x + area.width).saturating_sub(CELL_W) as i32,
                ) as u16
            } else {
                x
            };
            draw_static_cell(&mut off, draw_x, y, game, (board_row, col), cell, moved_map);
        }
    }

    // オフスクリーンバッファの[dy, dy+inner.height)行を実際の描画領域へ転写する。
    for row in 0..inner.height {
        for col in 0..inner.width {
            let Some(src) = off
                .cell(Position::new(area.x + col, area.y + cam.dy + row))
                .cloned()
            else {
                continue;
            };
            if let Some(dst) = buf.cell_mut(Position::new(inner.x + col, inner.y + row)) {
                *dst = src;
            }
        }
    }
}

/// 盤面のセル1マスぶんを、その場(静止位置)に描画する。落下補間中のブロックは
/// `draw_falling_blocks`が別途上から重ねるため、ここでは扱わない。
///
/// 優先順: クリア後の盤面の底(フィールドより深い行)は地底の地面 > 爆風直後のセルは
/// 炎色で一瞬覆う > フラッシュ中のセルはフラッシュしてから背景色へ消える > 消滅は
/// 確定したが落下ブロックの到着待ちのセルは消滅前の見た目のまま(#234) >
/// チェックポイント安全地帯のEmptyは地面ビジュアル > 通常描画。
fn draw_static_cell(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    game: &Game,
    pos: Pos,
    cell: BoardCell,
    moved_map: &HashMap<Pos, Pos>,
) {
    let (board_row, col) = pos;
    if game.status == GameStatus::Cleared && board_row >= game.board.depth_rows() {
        fill_bedrock_ground(buf, x, y);
    } else if let Some((t, tier)) = game.explosion_flash_progress(pos) {
        fill_block(
            buf,
            x,
            y,
            colors::explosion_flame_bg(tier, t, natural_cell_bg(cell)),
        );
    } else if cell == BoardCell::Empty
        && let Some(t) = game.vanish_flash_progress(pos)
    {
        fill_block(buf, x, y, colors::vanish_flash_bg(t));
    } else if cell == BoardCell::Empty
        && !moved_map.contains_key(&pos)
        && let Some(kind) = game.pending_vanish_kind(pos)
    {
        // 消滅は確定したが、一緒に消える落下ブロックがまだ空中にいる間(TERM独自拡張。
        // #234)。落下してくる側は`draw_falling_blocks`が補間位置へ描くのでここでは
        // 扱わず、その場に留まっている側だけを消滅前の見た目のまま描き続ける。
        draw_pending_vanish_cell(buf, x, y, game, pos, kind, moved_map);
    } else if cell == BoardCell::Empty && is_checkpoint_safe_zone_row(board_row) {
        fill_bedrock_ground(buf, x, y);
    } else {
        draw_logical_cell(buf, x, y, &game.board, board_row, col, cell);
    }
}

/// 消滅は確定したがフラッシュ開始待ちのセルを、消滅直前の見た目のまま描く(#242)。
/// 盤面上は既にEmptyのため`draw_logical_cell`にそのまま任せると接続罫線が「隣もEmpty」
/// と判定され、繋がって見えるべき塊が1マスずつバラけて見える。ここでは待機中の隣接セルも
/// 繋がっているとみなしたマスクを与えて描く。
fn draw_pending_vanish_cell(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    game: &Game,
    pos: Pos,
    kind: BoardCell,
    moved_map: &HashMap<Pos, Pos>,
) {
    let (row, col) = pos;
    match kind {
        BoardCell::Color(color) => {
            let mask = conn_mask_pending(game, row, col, moved_map, |cell| {
                cell == BoardCell::Color(color)
            });
            draw_color_block_with_mask(buf, x, y, &mask, color);
        }
        BoardCell::Rock { hits } => {
            let mask = conn_mask_pending(game, row, col, moved_map, |cell| {
                matches!(cell, BoardCell::Rock { .. })
            });
            draw_rock_block_with_mask(buf, x, y, &mask, hits);
        }
        // 接続罫線を持たない種類(AIR・スター・アイテム等)はそのまま通常描画でよい。
        other => draw_logical_cell(buf, x, y, &game.board, row, col, other),
    }
}

/// 画面外(まだスクロールインしていない、カメラより浅い行)にボムがある場合、
/// そのボムがある列全体を赤く点滅させて警告する。
fn draw_off_screen_bomb_warnings(
    buf: &mut Buffer,
    inner: Rect,
    cam_row_f: f32,
    visible_rows: usize,
    game: &Game,
) {
    let warning_cols: HashSet<usize> = game
        .bombs()
        .iter()
        .filter(|b| (b.pos.0 as f32) < cam_row_f)
        .map(|b| b.pos.1)
        .collect();
    if warning_cols.is_empty() {
        return;
    }
    let cycle_ms = OFF_SCREEN_BOMB_WARNING_ON_MS + OFF_SCREEN_BOMB_WARNING_OFF_MS;
    let phase_ms = (game.player.elapsed_seconds * 1000.0) as u32 % cycle_ms;
    if phase_ms >= OFF_SCREEN_BOMB_WARNING_ON_MS {
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

/// ボムを盤面の上に重ねて描画する(ブロックとは別レイヤーなので通常のセル描画ループとは独立)。
/// `BombPhase`に応じて 白ボン登場(Entering)→転がり(Rolling、縦にも弾ませる)→落下・バウンド
/// (Settling)→設置後の点滅カウントダウン(Ticking) を描き分け、起爆が近づくほど点滅を速める。
fn draw_bombs(buf: &mut Buffer, inner: Rect, cam_row_f: f32, visible_rows: usize, game: &Game) {
    for bomb in game.bombs() {
        // originとposは常に同じ行で、ボム自体はその行に、白ボンはその1行上に描く。
        let bomb_row = bomb.pos.0;
        let shirobon_row = bomb_row.saturating_sub(1);

        match bomb.phase {
            BombPhase::Entering => {
                let Some((x, y)) =
                    cell_screen_pos(inner, cam_row_f, visible_rows, shirobon_row, bomb.origin.1)
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
                    cell_screen_pos_f32(inner, cam_row_f, visible_rows, display_row, col)
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
                // 落下・左右バウンド中は現在位置(`bomb.pos`、毎tick更新される)へそのまま描く。
                // 起爆カウントダウンはまだ始まっていないため、火花は暗い方の色で固定する。
                let Some((x, y)) =
                    cell_screen_pos(inner, cam_row_f, visible_rows, bomb.pos.0, bomb.pos.1)
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
                    cell_screen_pos(inner, cam_row_f, visible_rows, bomb_row, bomb.pos.1)
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

/// フィールド内の論理セル位置(行・列)を、現在のスクロール位置(`cam_row_f`)・
/// 可視行数を踏まえて画面座標(x, y)へ変換する。範囲外なら`None`。
fn cell_screen_pos(
    inner: Rect,
    cam_row_f: f32,
    visible_rows: usize,
    row: usize,
    col: usize,
) -> Option<(u16, u16)> {
    cell_screen_pos_f32(inner, cam_row_f, visible_rows, row, col as f32)
}

/// `cell_screen_pos`の列位置を小数(補間中の途中位置)で受け取る版。
fn cell_screen_pos_f32(
    inner: Rect,
    cam_row_f: f32,
    visible_rows: usize,
    row: usize,
    col: f32,
) -> Option<(u16, u16)> {
    // カメラは小数行で動くため、セル1つぶんに満たないはみ出し(上端で欠ける位置)も
    // ここでは描画対象外にする。
    let screen_row = row as f32 - cam_row_f;
    if screen_row < 0.0 || screen_row >= visible_rows as f32 || col < 0.0 {
        return None;
    }
    let y = inner.y as f32 + screen_row * CELL_H as f32;
    let x = inner.x as f32 + col * CELL_W as f32;
    if x < inner.x as f32 {
        return None;
    }
    let x = x.round() as u16;
    let y = y.round() as u16;
    if x + CELL_W > inner.x + inner.width || y + CELL_H > inner.y + inner.height {
        return None;
    }
    Some((x, y))
}

/// 白ボンのスプライト。プレイヤースプライトと同じ4文字×2行の描画方式を使う。
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

/// 転がり中(Rolling)の弾みの回数。区間をこの回数に分割し、各区間の前半だけ1マス上へ跳ねさせる。
const BOMB_ROLL_BOUNCE_COUNT: u32 = 3;

/// 進捗`t`(0.0=転がり開始、1.0=設置直前)の時点でボムが1マス上に跳ねているかどうか。
/// 横方向の線形移動だけだとコンベアのように滑って見えるため、縦方向にも複数回の跳ねを加える。
fn bomb_roll_is_bouncing_up(t: f32) -> bool {
    let t = t.clamp(0.0, 1.0);
    if t >= 1.0 {
        return false;
    }
    let segment = 1.0 / BOMB_ROLL_BOUNCE_COUNT as f32;
    let local = (t / segment).fract();
    local < 0.5
}

/// 導火線の火花が「ちりちり」明滅する周期。この時間ごとに火花の位置・グリフを
/// 切り替え、単調な点滅でなく飛び散るような見た目にする。
const BOMB_CRACKLE_FRAME_MS: u32 = 70;
const BOMB_CRACKLE_GLYPHS: [char; 4] = ['\'', '`', '.', '*'];

/// ボム本体のスプライト。セル全体を本体色`body`(起爆間際は`bomb_body_color`で赤点滅)で
/// 塗りつぶした上に明るい縁取り色(`BOMB_RIM_FG`)で丸い輪郭を描く(背景色を透過させると
/// 暗い色同士で輪郭が溶ける)。上段の火花は`crackle_ms`に応じて位置・グリフを切り替える。
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

/// 導火線の火花の点滅周期。残り時間が`BOMB_BLINK_FAST_THRESHOLD_MS`を切ると
/// 短い方の周期に切り替え、起爆間近であることを強調する。
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

/// 起爆間際は火花だけでなく本体も激しく赤く点滅させる。残り時間が`BOMB_DANGER_MS`
/// (導火線カウントダウンSEと共有)を切ったら、この周期で本体色と警告色(赤)を切り替える。
const BOMB_BODY_FLASH_PERIOD_MS: u32 = 100;

/// 画面外のボム警告(縦列の赤点滅)の点灯時間(ms)。常時赤に近い遅い点滅は盤面の可視性を
/// 奪うため、短く点灯してすぐ消える非対称なデューティ比(50ms点灯/500ms消灯)にする。
const OFF_SCREEN_BOMB_WARNING_ON_MS: u32 = 50;
/// 画面外のボム警告を消灯させておく時間(ms)。
const OFF_SCREEN_BOMB_WARNING_OFF_MS: u32 = 500;

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

/// 直近の重力ティックで落下したブロックを、移動前→移動後を滑らかに補間した画面座標へ描画する。
/// 静止時と同じグリフ模様で描き、接続罫線の判定は着地先時点の盤面を基準にする。
fn draw_falling_blocks(
    buf: &mut Buffer,
    inner: Rect,
    cam_row_f: f32,
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
        // 着地と同一tickで4連結自動消滅した場合、盤面は既にEmptyだが、フラッシュはまだ
        // 始まっていない(落下補間の完了を待っている)。盤面から読めない間は消滅直前の
        // 種類で補い、最後まで落ちきってからフラッシュへ移る見た目にする。
        let cell = match cell {
            BoardCell::Empty => {
                let resolved = game.pending_vanish_kind((to_row, to_col));
                // このフォールバック分岐に入ったこと(補えたか/スキップしたか)をログに残す。
                // 稀なケースでしか通らないため、毎フレーム描画中でも記録量は少ない。
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
        let screen_row = interp_row - cam_row_f;
        if screen_row < 0.0 || screen_row > visible_rows as f32 {
            continue; // 画面外
        }

        let px = inner.x as f32 + interp_col * CELL_W as f32;
        let py = inner.y as f32 + screen_row * CELL_H as f32;
        if px < inner.x as f32 || py < inner.y as f32 {
            continue;
        }
        let x = px.round() as u16;
        let y = py.round() as u16;
        if x + CELL_W > inner.x + inner.width || y + CELL_H > inner.y + inner.height {
            continue; // 補間の一時的なはみ出しは描画をスキップする
        }
        // 落下中も静止時と同じグリフ模様で描画する(単色塗りだと何が落ちているか分からない)。
        // 接続罫線の判定は着地先(to_row, to_col)時点の盤面を基準にする(その時点で既に確定している)。
        draw_logical_cell(buf, x, y, &game.board, to_row, to_col, cell);
    }
}

/// 揺れ中のブロックにかける、左右の小刻みなジッター(文字数単位)。セルの座標から求めた
/// 位相をずらすことで、隣接セルが機械的に完全同期して見えるのを避けつつ、同じ塊はおおむね一体で震える。
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

/// プレイヤーのスプライトを、直前の論理位置から現在位置へ補間した画面座標へ描画する(9章)。
/// ロジック上の当たり判定・掘削・落下判定は常に整数マス基準のままで、ここで行うのは描画位置の補間のみ。
fn draw_player(buf: &mut Buffer, inner: Rect, cam_row_f: f32, game: &Game) {
    let (_, prev_col) = game.render_prev_position();
    let (cur_row, cur_col) = game.player.position();
    let t = game.move_anim_progress();

    let interp_row = interp_player_row(game);
    let interp_col = prev_col as f32 + (cur_col as f32 - prev_col as f32) * t;

    let screen_row = interp_row - cam_row_f;
    if screen_row < 0.0 {
        return; // スクロール範囲外(補間中に上端を跨ぐ極端なケースの防御)
    }

    // 「わ〜!」スライダー演出中は、直前の移動方向へさらに滑り込み、
    // 進捗が進むにつれ本来の位置へ戻ってくる。
    let dodge_offset_cells = if game.is_dodge_sliding() {
        let dir_col = (cur_col as f32 - prev_col as f32).signum();
        (1.0 - game.dodge_slide_progress()) * DODGE_SLIDE_OFFSET_CELLS * dir_col
    } else {
        0.0
    };
    let px = inner.x as f32 + interp_col * CELL_W as f32 + dodge_offset_cells * CELL_W as f32;
    // 「天に召される」演出中は、進捗に応じてスプライトを上へドリフトさせる。
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

/// 「わ〜!」スライダー演出で最大どれだけ滑らせるか(論理セル単位)。
const DODGE_SLIDE_OFFSET_CELLS: f32 = 0.6;

/// 「天に召される」演出でスプライトが上へ昇る距離(論理セル単位)。
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
        // AIRはカプセル(丸薬)らしいシルエットにする。`draw_rounded_unit`が四隅を
        // フィールド背景色で欠き取り、正方形でなく輪郭の丸まったシルエットになる。
        BoardCell::Oxygen => draw_rounded_unit(
            buf,
            x,
            y,
            [['◜', '◝'], ['◟', '◞']],
            colors::OXYGEN_FG,
            colors::OXYGEN_BG,
        ),
        BoardCell::Diamond => draw_diamond_block(buf, x, y, board, row, col),
        // スターブロックは氷の結晶のようにきらめかせる。四隅を欠き取った輪郭にし、
        // `star_sparkle_content`で4マスの位相をずらして複数箇所が順にきらめくようにする。
        BoardCell::Star { visible_ms } => draw_rounded_unit(
            buf,
            x,
            y,
            star_sparkle_content(visible_ms),
            colors::STAR_FG,
            colors::star_bg(visible_ms, STAR_VISIBLE_GRACE_MS, STAR_MELT_DURATION_MS),
        ),
        // アイテムブロックは効果ごとに専用の形状にする。ClearAboveは頭上を吹き飛ばす
        // イメージで上向き矢印、UnifyColorsは色が混ざり合うイメージで陰陽風の分割円を
        // 上段に添え、いずれも四隅を欠き取った輪郭にする。
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

/// スターブロックのキラキラ点滅グリフ。画面内に入ってから消えるまでの間ずっと、
/// `STAR_SPARKLE_PERIOD_MS`ごとに☆/★を交互に切り替える。
fn star_glyph(visible_ms: u32) -> char {
    if (visible_ms / STAR_SPARKLE_PERIOD_MS).is_multiple_of(2) {
        '☆'
    } else {
        '★'
    }
}

/// スターブロック内の4マス(2列×2行)ぶんの位相ずれ。4マスが一斉点滅すると均一な四角に
/// しか見えないため、`STAR_SPARKLE_PERIOD_MS`の1/4ずつ位相をずらして順にきらめかせる。
const STAR_SPARKLE_PHASE_OFFSETS_MS: [[u32; 2]; 2] = [
    [0, STAR_SPARKLE_PERIOD_MS / 4],
    [STAR_SPARKLE_PERIOD_MS / 2, STAR_SPARKLE_PERIOD_MS * 3 / 4],
];

/// スターブロックの`draw_rounded_unit`用の中央2列×2行のコンテンツを、位置ごとに
/// 位相をずらした`star_glyph`で組み立てる。
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

/// 4文字×2行を単色の空白で塗りつぶす。
fn fill_block(buf: &mut Buffer, x: u16, y: u16, bg: Color) {
    for dy in 0..CELL_H {
        for dx in 0..CELL_W {
            put(buf, x + dx, y + dy, ' ', bg, bg);
        }
    }
}

/// 最終ゴール到達時の盤面の底やチェックポイント安全地帯に見せる地底の地面。単色でなく
/// 岩肌のようなハッチング模様にして「掘り進めない底に到達した」ことを見た目でも伝える。
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

/// `board_row`が100mごとのチェックポイント通過後の安全地帯に含まれるかどうか。安全地帯は
/// 必ず`Cell::Empty`になる区間なので、素の空背景でなく地底の地面ビジュアルで表示する。
/// 500mはボーナスフロア(アイテム/AIR配置)で空にはならないため対象外にする。
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

/// 4方向の隣接位置それぞれについて`connected`(盤面内のその位置と繋がって見せるか)を
/// 評価してマスクを組み立てる共通処理。盤面の範囲外は常に非接続として扱う。
fn conn_mask_from(
    board: &Board,
    row: usize,
    col: usize,
    connected: impl Fn(usize, usize) -> bool,
) -> ConnMask {
    let check = |r: isize, c: isize| -> bool {
        r >= 0
            && (r as usize) < board.depth_rows()
            && c >= 0
            && (c as usize) < board.width()
            && connected(r as usize, c as usize)
    };
    ConnMask {
        up: check(row as isize - 1, col as isize),
        down: check(row as isize + 1, col as isize),
        left: check(row as isize, col as isize - 1),
        right: check(row as isize, col as isize + 1),
    }
}

/// `same`(隣接セルが自分と同種と言えるか)を基準に4方向の接続有無を求める共通処理。
/// 色ブロック(同色判定)・岩ブロック(hitsを問わずRockかどうかの判定)の両方で使う。
fn conn_mask_by(
    board: &Board,
    row: usize,
    col: usize,
    same: impl Fn(BoardCell) -> bool,
) -> ConnMask {
    conn_mask_from(board, row, col, |r, c| same(board.cell(r, c)))
}

/// 消滅は確定したがフラッシュ開始待ちのセル(`pending_vanish_kind`)を描くための接続判定
/// (#242)。盤面上は既にEmptyなので現在の盤面だけで判定すると塊がバラけて見えるため、
/// 同じく待機中の隣接セルも接続しているとみなす。ただし落下中(=まだ到着しておらず
/// `draw_falling_blocks`が別途単独で描く)側は繋げない。
fn conn_mask_pending(
    game: &Game,
    row: usize,
    col: usize,
    moved_map: &HashMap<Pos, Pos>,
    same: impl Fn(BoardCell) -> bool,
) -> ConnMask {
    conn_mask_from(&game.board, row, col, |r, c| {
        same(game.board.cell(r, c))
            || (!moved_map.contains_key(&(r, c))
                && game.pending_vanish_kind((r, c)).is_some_and(&same))
    })
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

/// ダイヤブロック用の接続判定。岩ブロックと同じく、隣接するダイヤブロック同士の
/// 境界を消して1つの塊(ゴツゴツした岩のような連続した形状)に見えるようにする。
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
    draw_color_block_with_mask(buf, x, y, &mask, kind);
}

/// `draw_color_block`の、接続マスクを呼び出し側から与える版。現在の盤面からは正しい
/// 接続を導けない場面(消滅待機中のセル、#242)のために分離している。
fn draw_color_block_with_mask(buf: &mut Buffer, x: u16, y: u16, mask: &ConnMask, kind: ColorKind) {
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

/// AIR・スター・アイテムブロック共通の描画。角に丸罫線を乗せるだけでは背景が正方形のまま
/// 見えるため、四隅を四分割ブロック文字(`▘▝▖▗`)でフィールド背景色側に3/4欠き取り、
/// 輪郭が斜めに丸まったシルエット(八角形状)にする。中央2列×2行の`content`は呼び出し側で決める。
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

/// 岩ブロック(Xブロック)の描画。色ブロックと同様、隣接する岩ブロック同士は角の罫線を接続させ
/// 1つの塊に見せる。ヒビ/Xマーク(rock_glyphs)は視認性を優先し、接続の有無に関わらず
/// 中央2列には常に表示する(色ブロックのように空白へは置き換えない)。
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
    draw_rock_block_with_mask(buf, x, y, &mask, hits);
}

/// `draw_rock_block`の、接続マスクを呼び出し側から与える版(用途は
/// `draw_color_block_with_mask`と同じ)。
fn draw_rock_block_with_mask(buf: &mut Buffer, x: u16, y: u16, mask: &ConnMask, hits: u8) {
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

/// ダイヤブロックの描画。岩ブロック(`draw_rock_block`)と同じ接続判定(`conn_mask_diamond`)で
/// 隣接するダイヤブロック同士の境界を消し、ゴツゴツした岩の塊のような連続した形状にする。
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

/// プレイヤーの向き・掘削演出フレームに応じたスプライト(4文字×2行)。`drilling_frame`が
/// `None`なら静止、`Some(_)`なら方向別の2フレームを交互に返す(掘削は常にfacing方向)。
/// 目(oo/OO)を丸括弧で挟んでヘルメットの縁とし、左右向きは進行方向側だけ`<`/`>`のドリル先端を開ける。
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

/// 「わ〜!」スライダー演出中のプレイヤースプライト。方向によらず常にこの驚き顔で表示する。
const DODGE_SPRITE: [&str; 2] = ["!OO!", " /\\ "];

/// 落下ブロックに押し潰された際の「潰れた」演出用スプライト(9章)。GameOverオーバーレイの
/// 表示前に`CRUSH_FLASH_MS`ぶんだけ表示する。1行目に×印を並べ、2行目は空白にして平たく潰れた見た目にする。
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

fn draw_status(frame: &mut Frame, area: Rect, game: &Game, autoplay_enabled: bool) {
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

    // フレーム巻き戻し(#233)の残り回数。上限0(=設定で機能OFF)なら行ごと省略し、
    // 使わない人のHUDの見た目は変えない。
    if game.rewind_stock_max() > 0 {
        write_line(buf, inner, &mut row, "REWIND", label_style);
        write_line(
            buf,
            inner,
            &mut row,
            &format!("  \u{21ba} \u{d7}{}", game.rewind_stock()),
            label_style,
        );
        write_line(buf, inner, &mut row, "", label_style);
    }

    // ブロック状態遷移ログ(debug_log)の記録と突き合わせるためのフレーム番号を表示する。
    write_line(buf, inner, &mut row, "FRAME", label_style);
    write_line(
        buf,
        inner,
        &mut row,
        &format!("  {}", game.debug_frame()),
        label_style,
    );

    // オートプレイ・無敵の状態表示。どちらもデバッグ機能なので、有効な間だけ行を足す
    // (通常プレイのHUDの見た目は変えない)。
    if autoplay_enabled || game.is_invincible() {
        write_line(buf, inner, &mut row, "", label_style);
        let debug_style = Style::default()
            .fg(colors::STAR_FG)
            .bg(colors::LETTERBOX_BG);
        if autoplay_enabled {
            write_line(buf, inner, &mut row, "AUTO", debug_style);
        }
        if game.is_invincible() {
            write_line(
                buf,
                inner,
                &mut row,
                &format!("GOD x{}", game.misses_averted()),
                debug_style,
            );
        }
    }
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

/// チェックポイント(100mごと)到達演出のバナー。`draw_overlay`より一回り小さい箱を
/// `checkpoint_flash_depth_m`がSomeの間だけ中央に重ねる。盤面(`draw_field`)は裏で
/// 通常通り動き続ける(押し潰し演出等と同じく、周囲の落下アニメーションを止めない設計方針)。
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

/// GameOverダイアログ。「タイトルへ戻る」「その場から復活」の2択を表示し、
/// 現在選択中の項目を反転表示(カーソル代わり)する。
///
/// `rewind_hint`が`Some(残りストック数)`なら、巻き戻しでやり直せることを案内する行を
/// 1行足す(TERM独自拡張。#233)。その1行ぶん枠も縦に広げるが、ヒントが無い場合の
/// 見た目は従来通りに保つ。
fn draw_game_over_overlay(
    frame: &mut Frame,
    area: Rect,
    selection: GameOverChoice,
    rewind_hint: Option<u8>,
) {
    let overlay_area = centered_rect(40, game_over_overlay_percent_y(rewind_hint.is_some()), area);
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

    let mut lines = vec![
        Line::from(Span::styled("GAME OVER", text_style)),
        Line::from(""),
        choice_line("タイトルへ戻る", selection == GameOverChoice::BackToTitle),
        choice_line("その場から復活", selection == GameOverChoice::Revive),
        Line::from(""),
        Line::from(Span::styled("↑↓で選択 / Enterで決定", text_style)),
    ];
    if let Some(stock) = rewind_hint {
        lines.push(Line::from(Span::styled(
            format!("Backspace/U: 巻き戻す(残り{stock})"),
            text_style,
        )));
    }

    let paragraph = Paragraph::new(lines)
        .block(block)
        .style(Style::default().bg(colors::LETTERBOX_BG))
        .alignment(Alignment::Center);
    frame.render_widget(paragraph, overlay_area);
}

/// GameOverダイアログの縦幅(`centered_rect`のパーセント指定)。巻き戻しヒントの
/// 1行が増えると既定の25%(=8行、枠2行+内容6行)では下端がクリップするため広げる。
fn game_over_overlay_percent_y(with_rewind_hint: bool) -> u16 {
    if with_rewind_hint { 30 } else { 25 }
}

/// 巻き戻したフレーム数を、おおよその秒数へ換算する(TERM独自拡張。#233)。
/// 1フレームを`FRAME_INTERVAL_MS`とみなした目安で、実際のフレーム間隔は負荷によって
/// 前後するため厳密な経過時間ではない(表示も「約N秒前」とする)。
fn rewind_seconds_back(frames_back: u64) -> u64 {
    frames_back * crate::constants::FRAME_INTERVAL_MS / 1000
}

/// 巻き戻し(逆再生)中に重ねるオーバーレイ(TERM独自拡張。#233)。
/// `steps_back`は巻き戻し開始時点から何スナップショットぶん過去を見ているか、
/// `frames_back`は同じく何ゲームフレームぶん過去か(体感時間の目安表示に使う)。
pub fn draw_rewind_overlay(
    frame: &mut Frame,
    field_width: usize,
    steps_back: usize,
    frames_back: u64,
) {
    let area = frame.area();
    if area.width < MIN_TERMINAL_W || area.height < MIN_TERMINAL_H {
        return;
    }
    let plan = compute_layout(area, field_width);
    let overlay_area = bottom_anchored_rect(90, REWIND_OVERLAY_H, plan.game_frame);
    frame.render_widget(Clear, overlay_area);

    let text_style = Style::default()
        .fg(colors::PANEL_TEXT)
        .bg(colors::LETTERBOX_BG);
    let heading_style = Style::default()
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

    let paragraph = Paragraph::new(vec![
        Line::from(Span::styled(
            format!(
                "<< 巻き戻し中  -{steps_back}ステップ (約{}秒前)",
                rewind_seconds_back(frames_back)
            ),
            heading_style,
        )),
        Line::from(Span::styled(
            "Enter/X/Z: ここから再開   ←→: 調整   Esc: やめる",
            text_style,
        )),
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
        // スプライトの幅がCELL_W(4文字)からずれると隣のセルとの描画位置がずれてしまうため回帰確認する。
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
        // ヘルメットの縁(丸括弧)は進行方向側を開けたままにし、ドリルの刃が突き出る側だとわかるようにする。
        assert!(player_sprite(Direction::Left, None)[0].starts_with('<'));
        assert!(player_sprite(Direction::Right, None)[0].ends_with('>'));
    }

    #[test]
    fn star_glyph_toggles_every_sparkle_period_starting_from_visible() {
        // 画面内に入った直後(visible_ms=0)から既にキラキラの切り替えが起きていることを確認する。
        assert_eq!(star_glyph(0), '☆');
        assert_eq!(star_glyph(STAR_SPARKLE_PERIOD_MS - 1), '☆');
        assert_eq!(star_glyph(STAR_SPARKLE_PERIOD_MS), '★');
        assert_eq!(star_glyph(STAR_SPARKLE_PERIOD_MS * 2 - 1), '★');
        assert_eq!(star_glyph(STAR_SPARKLE_PERIOD_MS * 2), '☆');
    }

    #[test]
    fn star_sparkle_content_staggers_the_four_positions_instead_of_flashing_in_unison() {
        // 4マスが同時に切り替わると均一な四角にしか見えないため、位置によって☆/★の
        // 切り替わるタイミングがずれている(=ある瞬間には両方の記号が混在する)ことを確認する。
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
        // AIR・アイテムと同じく、四隅がフィールド背景色まで欠き取られていることを確認する。
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
        // 転がる区間(t=0.0〜1.0)の間に複数回跳ね、設置直前(t=1.0)には
        // 必ず地面に着地している(跳ねていない)ことを確認する。
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
        // グリフ以外がフィールド背景色のまま透過すると輪郭が背景に溶け込むため、
        // セル全体の背景が本体色で塗りつぶされていることを確認する。
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

    /// テスト用: プレイヤー周辺(±`STAR_VISIBLE_RANGE_ROWS`)を全て岩で埋め、指定の1マスだけを
    /// Emptyにしたうえで`debug_place_bomb`を呼び、その1マスへ確実にボムを設置する。
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
        // top_rowより浅い(=まだスクロールインしていない画面外)位置にあるボムの列は、
        // 点滅周期に応じて赤く塗られる。
        let mut game = Game::new(1);
        game.player.row = 500;
        place_bomb_at(&mut game, 490, 3); // top_row(495)より浅い = 画面外
        assert_eq!(
            game.bombs().len(),
            1,
            "テスト前提: ボムが1個設置されていること"
        );

        let inner = Rect::new(0, 0, 20, 20);
        let cam_row_f = 495.0;
        let visible_rows = 10;

        game.player.elapsed_seconds = 0.0;
        let mut buf_on = Buffer::empty(inner);
        draw_off_screen_bomb_warnings(&mut buf_on, inner, cam_row_f, visible_rows, &game);
        assert!(
            buf_on
                .content
                .iter()
                .any(|c| c.bg == colors::BOMB_BODY_DANGER_FG),
            "点滅ON中は画面外ボムの列が赤く塗られるはず"
        );

        game.player.elapsed_seconds = OFF_SCREEN_BOMB_WARNING_ON_MS as f32 / 1000.0;
        let mut buf_off = Buffer::empty(inner);
        draw_off_screen_bomb_warnings(&mut buf_off, inner, cam_row_f, visible_rows, &game);
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
        draw_off_screen_bomb_warnings(&mut buf, inner, 495.0, 10, &game);

        assert!(
            !buf.content
                .iter()
                .any(|c| c.bg == colors::BOMB_BODY_DANGER_FG),
            "画面内のボムでは警告表示しないはず"
        );
    }

    #[test]
    fn draw_bomb_sprite_crackle_alternates_the_spark_glyph_and_position_over_time() {
        // 異なる`crackle_ms`を渡すと、火花の位置(左右どちらのマス)かグリフが変わることを確認する。
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
        // 地底の地面セルは単色の空白ではなく、専用の色(BEDROCK_GROUND_BG/FG)で
        // ハッチング模様に塗りつぶされることを確認する。
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
        // 各チェックポイント(100mごと)通過後の安全地帯(CHECKPOINT_SAFE_ZONE_M行)だけが
        // 対象で、その手前・その先・500mのボーナスフロアは対象外のはず。
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
        // 落下中も静止時と同じグリフ(ダイヤなら◆)で描画されることを確認する。
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
        draw_falling_blocks(&mut buf, inner, 0.0, 10, &game, &moved_map);

        let has_diamond_glyph = buf.content.iter().any(|cell| cell.symbol() == "◆");
        assert!(
            has_diamond_glyph,
            "落下中もダイヤの◆グリフが描画されているはず"
        );
    }

    #[test]
    fn falling_block_that_auto_vanishes_on_the_same_tick_it_lands_still_renders_its_fall() {
        // 着地と同一tickで4連結自動消滅すると盤面は既にEmptyになるが、落下中は
        // 消滅直前の色ブロックの背景色で最後まで描画され続けることを確認する。
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
        // 時間を与える(落下開始後は毎マス揺れ直さず連続で落ち続ける)。
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
        draw_falling_blocks(&mut buf, inner, 0.0, 10, &game, &moved_map);

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
        // 四隅のセルの背景色がフィールド背景色(`FIELD_EMPTY_BG`)まで欠き取られていることを
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
        // AIRと同じく、C/R/Kアイテムも四隅がフィールド背景色まで欠き取られ、
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

    // --- 設定画面のカーソル移動 ---

    #[test]
    fn settings_choice_cycle_back_is_the_exact_reverse_of_cycle() {
        // cycle_back()はcycle()の逆方向であり、どの項目から始めても cycle().cycle_back() で元へ戻る。
        let all = [
            SettingsChoice::Music,
            SettingsChoice::MusicVolume,
            SettingsChoice::Se,
            SettingsChoice::SeVolume,
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
            SettingsChoice::ShakeDuration,
            SettingsChoice::MoveSpeed,
            SettingsChoice::DodgeRecoveryMs,
            SettingsChoice::BombRate,
            SettingsChoice::DebugLogEnabled,
            SettingsChoice::ChainVanishInterval,
            SettingsChoice::RewindStockMax,
        ];
        for choice in all {
            assert_eq!(choice.cycle().cycle_back(), choice);
            assert_eq!(choice.cycle_back().cycle(), choice);
        }
    }

    #[test]
    fn settings_choice_cycle_visits_every_item_exactly_once_before_wrapping() {
        // 項目を追加したときにcycleの鎖から漏れる(到達できない項目が生まれる)のを防ぐ。
        // #233で巻き戻しストック上限を追加した際の回帰確認。
        let mut seen = vec![SettingsChoice::Music];
        let mut choice = SettingsChoice::Music;
        loop {
            choice = choice.cycle();
            if choice == SettingsChoice::Music {
                break;
            }
            assert!(
                !seen.contains(&choice),
                "{choice:?}を2回通っている(cycleの鎖が閉じていない)"
            );
            seen.push(choice);
            assert!(seen.len() < 100, "cycleがMusicへ戻ってこない");
        }
        assert!(
            seen.contains(&SettingsChoice::RewindStockMax),
            "#233で追加した巻き戻しストック上限へカーソルが到達できない"
        );
    }

    #[test]
    fn rewind_overlay_box_is_tall_enough_for_its_two_content_lines() {
        // #233: 巻き戻し中の案内は「-Nステップ(約N秒前)」と操作説明の2行。
        // 行を増やしたらREWIND_OVERLAY_Hも増やすこと。
        const REQUIRED_CONTENT_LINES: u16 = 2;
        let area = Rect::new(0, 0, 200, 60);
        let plan = compute_layout(area, crate::constants::FIELD_WIDTH_DEFAULT);
        let overlay_area = bottom_anchored_rect(90, REWIND_OVERLAY_H, plan.game_frame);
        assert!(
            overlay_area.height >= REQUIRED_CONTENT_LINES + 2,
            "巻き戻しオーバーレイの枠が狭すぎる(高さ={})",
            overlay_area.height
        );
    }

    #[test]
    fn rewind_overlay_sits_below_the_game_over_dialog_instead_of_overlapping_it() {
        // #233: GameOver中にも巻き戻せるため、両方のオーバーレイが同時に出る場面がある。
        // 巻き戻しの案内は下端に寄せ、中央のダイアログと重ならないようにする。
        let area = Rect::new(0, 0, 200, 60);
        let plan = compute_layout(area, crate::constants::FIELD_WIDTH_DEFAULT);

        let dialog = centered_rect(40, game_over_overlay_percent_y(true), plan.game_frame);
        let rewind = bottom_anchored_rect(90, REWIND_OVERLAY_H, plan.game_frame);

        assert!(
            rewind.y >= dialog.y + dialog.height,
            "巻き戻し案内(y={}..{})がGameOverダイアログ(y={}..{})と重なっている",
            rewind.y,
            rewind.y + rewind.height,
            dialog.y,
            dialog.y + dialog.height
        );
        assert_eq!(
            rewind.y + rewind.height,
            plan.game_frame.y + plan.game_frame.height,
            "ゲーム画面の下端に接しているはず"
        );
    }

    #[test]
    fn bottom_anchored_rect_clamps_a_height_taller_than_the_area() {
        let area = Rect::new(0, 0, 40, 3);
        let rect = bottom_anchored_rect(90, 10, area);
        assert_eq!(rect.height, 3, "areaより高くはならないはず");
        assert_eq!(rect.y, 0);
    }

    #[test]
    fn rewind_seconds_back_converts_frames_with_the_frame_interval() {
        // 1フレーム=FRAME_INTERVAL_MS(33ms)換算の目安。スナップショット間隔
        // (100フレーム)ぶん戻れば約3秒前になる。
        assert_eq!(rewind_seconds_back(0), 0);
        assert_eq!(
            rewind_seconds_back(crate::constants::REWIND_SNAPSHOT_INTERVAL_FRAMES as u64),
            3
        );
        assert_eq!(
            rewind_seconds_back(crate::constants::REWIND_SNAPSHOT_INTERVAL_FRAMES as u64 * 10),
            33,
            "履歴が満杯(10個)なら約33秒前まで戻れる"
        );
    }

    #[test]
    fn game_over_overlay_is_tall_enough_for_the_rewind_hint_line() {
        // #233: 巻き戻しヒントの1行が増えると、従来の25%(8行=枠2+内容6)では
        // 下端がクリップする。ヒント有りの時だけ枠を広げていることを確認する。
        let area = Rect::new(0, 0, 200, 60);
        let frame_rect = centered_fixed_rect(TOTAL_SCREEN_W, TOTAL_SCREEN_H, area);

        let without_hint = centered_rect(40, game_over_overlay_percent_y(false), frame_rect);
        assert!(
            without_hint.height >= 6 + 2,
            "ヒント無し(内容6行)が収まらない(高さ={})",
            without_hint.height
        );

        let with_hint = centered_rect(40, game_over_overlay_percent_y(true), frame_rect);
        assert!(
            with_hint.height >= 7 + 2,
            "ヒント有り(内容7行)が収まらない(高さ={})",
            with_hint.height
        );
    }

    // --- モードセレクト画面 ---

    #[test]
    fn course_choice_toggle_swaps_between_easy_and_normal() {
        assert_eq!(CourseChoice::Easy.toggle(), CourseChoice::Normal);
        assert_eq!(CourseChoice::Normal.toggle(), CourseChoice::Easy);
    }

    #[test]
    fn course_choice_depth_goal_m_matches_the_documented_course_lengths() {
        // spec.md 1章の確定事実「コースは2種類: 500m(イージー)と1000m(ノーマル)」。
        assert_eq!(CourseChoice::Easy.depth_goal_m(), 500);
        assert_eq!(CourseChoice::Normal.depth_goal_m(), 1000);
    }

    #[test]
    fn course_choice_from_depth_goal_m_round_trips_through_depth_goal_m() {
        for choice in [CourseChoice::Easy, CourseChoice::Normal] {
            assert_eq!(
                CourseChoice::from_depth_goal_m(choice.depth_goal_m()),
                choice
            );
        }
    }

    #[test]
    fn title_screen_text_overlay_fits_within_a_reasonably_sized_terminal() {
        // アートは画面いっぱいに表示するため別途行数を消費せず、確認すべきは
        // 「ロゴ+案内文パネルが画面の縦幅に収まるか」だけ。55行は一般的なターミナルの高さの目安。
        const ASSUMED_COMMON_TERMINAL_H: u16 = 55;

        let mut text_lines = build_title_logo_lines().to_vec();
        text_lines.extend([
            Line::from(""),
            Line::from(""),
            Line::from(""),
            Line::from(""),
            Line::from(""),
        ]);
        let text_rows = text_lines.len() as u16;

        assert!(
            text_rows <= ASSUMED_COMMON_TERMINAL_H,
            "ロゴ+案内文パネルの行数が一般的な端末の高さ({ASSUMED_COMMON_TERMINAL_H}行)を\
             超えている(text_rows={text_rows})"
        );
    }

    #[test]
    fn title_art_lines_fills_the_exact_requested_terminal_size() {
        // アートは画面いっぱいに表示するため、行数・幅とも要求した端末サイズと1:1で一致するはず。
        let lines = title_art_lines(100, 40);
        assert_eq!(lines.len(), 40);
        assert_eq!(lines[0].spans.len(), 100);
    }

    #[test]
    fn title_art_lines_cache_returns_the_same_size_on_repeated_calls() {
        // 同じ端末サイズでの再呼び出しはキャッシュから返されるが、内容(サイズ)は変わらないはず。
        let first = title_art_lines(80, 24);
        let second = title_art_lines(80, 24);
        assert_eq!(first.len(), second.len());
        assert_eq!(first[0].spans.len(), second[0].spans.len());
    }

    #[test]
    fn help_screen_box_is_tall_enough_for_the_jukebox_section() {
        // 枠の高さが実際の内容行数(操作欄+ジュークボックス欄+空行+末尾行)を収められているか
        // 回帰確認する。内容行数が増えたらこの定数も増やすこと。
        // 内訳: 操作見出し1+操作5(#233で巻き戻し1行を追加)+空行1+一時停止見出し1+
        // 一時停止2+空行1+デバッグ見出し1+デバッグ7+空行1+ジュークボックス見出し1+
        // 曲4+空行1+末尾1=27行。
        const REQUIRED_CONTENT_LINES: u16 = 27;
        let area = Rect::new(0, 0, 200, 60);
        let frame_rect = centered_fixed_rect(TOTAL_SCREEN_W, TOTAL_SCREEN_H, area);
        let help_area = centered_rect(90, HELP_OVERLAY_PERCENT_Y, frame_rect);
        assert!(
            help_area.height >= REQUIRED_CONTENT_LINES + 2,
            "ヘルプ画面の枠が{}行分の内容を収めるには狭すぎる(高さ={})",
            REQUIRED_CONTENT_LINES,
            help_area.height
        );
    }

    #[test]
    fn settings_screen_box_is_tall_enough_for_all_content_lines() {
        // 枠の高さが実際の内容行数を収められているか回帰確認する(足りないと下部の行が
        // クリップして見えなくなる)。見出し1+空行1+設定項目23(#224でMUSIC音量・SE音量の
        // 2項目、#233で巻き戻しストック上限、#243で揺れ時間(落下待ち)を追加)+空行1+
        // 案内2行=28行、枠(上下)2行込みで30行必要。設定を追加したらこの定数も増やすこと。
        const REQUIRED_CONTENT_LINES: u16 = 28;
        let area = Rect::new(0, 0, 200, 60);
        let frame_rect = centered_fixed_rect(TOTAL_SCREEN_W, TOTAL_SCREEN_H, area);
        let settings_area = centered_rect(60, SETTINGS_OVERLAY_PERCENT_Y, frame_rect);
        assert!(
            settings_area.height >= REQUIRED_CONTENT_LINES + 2,
            "設定画面の枠が{}行分の内容を収めるには狭すぎる(高さ={})",
            REQUIRED_CONTENT_LINES,
            settings_area.height
        );
    }

    // --- フィールド幅(列数)可変レイアウト ---

    #[test]
    fn field_pane_w_and_total_screen_w_scale_with_field_width() {
        // 列数が増えればフィールドペイン・フレーム全体の幅も広くなることを確認する。
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
        // プレイ中の配分率再抽選(reroll_spawn_rates_from)は`player.row + SPAWN_RATE_REROLL_SAFE_MARGIN_ROWS`
        // より先だけを書き換える前提なので、縮退表示(9.8)で可視行数がこのマージンを上回ると画面内の
        // 未掘削ブロックまで書き換わる。現実的な端末サイズ・全field_widthで超えないことを回帰確認する。
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

    // --- 揺れ(ぐらぐら)アニメーションのジッター ---

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
        // 上下が非接続の横1行だけの連結では、継ぎ目の角(x=3, x=4)は内部fill(空白)にならず
        // どちらも'─'になる(spec.md 9.3: 内部fillは縦横両方向とも接続している場合のみ)。
        // 継ぎ目に縦線'│'が入って区切られず、1本の途切れないボーダーに見えることを確認する。
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
        // 岩ブロック・色ブロックと同じく、隣接するダイヤブロック同士は境界を消して
        // 1つの塊に見えるようにする。継ぎ目に縦線'│'が入って区切られないことを確認する。
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
        // 隣がダイヤブロックでなければ角の丸みが残ることを確認する。
        let mut board = board_with(3);
        board.rows[1][0] = BoardCell::Diamond;
        board.rows[1][1] = BoardCell::Rock { hits: 0 }; // ダイヤではないので接続しない
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 2));

        draw_diamond_block(&mut buf, 0, 0, &board, 1, 0);

        assert_eq!(buf.cell(Position::new(3, 0)).unwrap().symbol(), "╮");
        assert_eq!(buf.cell(Position::new(3, 1)).unwrap().symbol(), "╯");
    }

    // -----------------------------------------------------------------------
    // 落下tick間隔を遅くした際の「落下→消滅」演出(#234)
    // -----------------------------------------------------------------------

    /// テスト用ヘルパー: 最深行(row2)の列1〜3に赤ブロックが横一列で並ぶだけの3行の盤面。
    /// 最深行なので常に支持され、これらのブロック自身は落下しない。
    fn three_red_cells_in_a_row_game() -> Game {
        let mut game = Game::new(1);
        game.board.rows.truncate(3);
        for row in game.board.rows.iter_mut() {
            for cell in row.iter_mut() {
                *cell = BoardCell::Empty;
            }
        }
        game.player.row = 0;
        game.player.col = 5;
        for col in 1..=3 {
            game.board.rows[2][col] = BoardCell::Color(ColorKind::Red);
        }
        game
    }

    /// テスト用ヘルパー: バッファの各行を、そのまま1つの文字列として取り出す。
    fn buffer_symbol_rows(buf: &Buffer) -> Vec<String> {
        let area = *buf.area();
        (area.y..area.y + area.height)
            .map(|y| {
                (area.x..area.x + area.width)
                    .map(|x| buf.cell(Position::new(x, y)).unwrap().symbol())
                    .collect()
            })
            .collect()
    }

    /// テスト用ヘルパー: 「(0,0)の赤ブロックが2マス落下し、着地先(2,0)で(2,1)(2,2)(2,3)と
    /// 4連結して消滅する」3行の盤面を、指定した落下tick間隔で作り、着地・消滅した直後の
    /// フレームまで1フレーム(33ms)ずつ進める。
    fn landed_and_vanished_game(block_fall_tick_ms: u64) -> Game {
        let mut game = three_red_cells_in_a_row_game();
        game.set_block_fall_tick_ms(block_fall_tick_ms);
        // 列0: 落下してくる赤ブロック(row0から最深行row2まで2マス落下し、着地先(2,0)で
        // (2,1)(2,2)(2,3)と4連結して消滅する)。
        game.board.rows[0][0] = BoardCell::Color(ColorKind::Red);

        let frame = std::time::Duration::from_millis(crate::constants::FRAME_INTERVAL_MS);
        for _ in 0..400 {
            game.update(frame);
            let landed = game
                .recently_moved_blocks()
                .iter()
                .any(|&(to, _)| to == (2, 0));
            if landed && game.board.cell(2, 0) == BoardCell::Empty {
                return game;
            }
        }
        panic!("着地と同一tickでの4連結消滅が起きなかった");
    }

    #[test]
    fn falling_block_that_auto_vanishes_on_landing_still_renders_its_fall_at_a_slow_tick_rate() {
        // #234。tick=600msでは、消滅フラッシュ(従来200ms固定)が落下補間(600ms)より先に
        // 終わってしまい、落下中のブロックが道のりの4割弱で空中から消えていた。
        // 補間が半分ほど進んだ時点でも、まだ赤ブロックとして描かれ続けることを確認する。
        let mut game = landed_and_vanished_game(600);

        let frame = std::time::Duration::from_millis(crate::constants::FRAME_INTERVAL_MS);
        while game.block_fall_progress() < 0.5 {
            game.update(frame);
        }
        let moved_map: HashMap<Pos, Pos> = game.recently_moved_blocks().iter().copied().collect();
        assert!(
            moved_map.contains_key(&(2, 0)),
            "テスト前提: まだ着地セルの落下補間が続いていること"
        );

        let inner = Rect::new(0, 0, 20, 10);
        let mut buf = Buffer::empty(inner);
        draw_falling_blocks(&mut buf, inner, 0.0, 10, &game, &moved_map);

        let red_bg = colors::fill_color(ColorKind::Red);
        assert!(
            buf.content
                .iter()
                .any(|cell| cell.bg == red_bg && cell.symbol() != " "),
            "tick=600msでも落下中は赤ブロックとして描画され続けるはず"
        );
    }

    #[test]
    fn static_cell_of_a_pending_vanish_keeps_its_look_until_the_flash_begins() {
        // #234。一緒に消える静止セル(着地セル自身ではない側)も、落下ブロックが到着する
        // までは消滅前の見た目のままで、到着してからフラッシュへ移ることを確認する。
        let mut game = landed_and_vanished_game(600);
        let moved_map: HashMap<Pos, Pos> = game.recently_moved_blocks().iter().copied().collect();
        assert!(
            !moved_map.contains_key(&(2, 1)),
            "テスト前提: (2,1)は落下していない静止セルであること"
        );

        // 1セルぶんだけのバッファにして、描画結果をセル全体で検証できるようにする。
        let inner = Rect::new(0, 0, CELL_W, CELL_H);
        let red_bg = colors::fill_color(ColorKind::Red);

        let mut buf = Buffer::empty(inner);
        draw_static_cell(&mut buf, 0, 0, &game, (2, 1), BoardCell::Empty, &moved_map);
        assert!(
            buf.content.iter().any(|cell| cell.bg == red_bg),
            "落下ブロックの到着待ちの間は、消滅前の赤ブロックのまま描かれるはず"
        );

        let frame = std::time::Duration::from_millis(crate::constants::FRAME_INTERVAL_MS);
        for _ in 0..40 {
            if game.vanish_flash_progress((2, 1)).is_some() {
                break;
            }
            game.update(frame);
        }
        let t = game
            .vanish_flash_progress((2, 1))
            .expect("到着後はフラッシュに入っているはず");
        let moved_map: HashMap<Pos, Pos> = game.recently_moved_blocks().iter().copied().collect();
        let mut buf = Buffer::empty(inner);
        draw_static_cell(&mut buf, 0, 0, &game, (2, 1), BoardCell::Empty, &moved_map);
        let flash_bg = colors::vanish_flash_bg(t);
        assert!(
            buf.content.iter().all(|cell| cell.bg == flash_bg),
            "到着後は消滅フラッシュの背景色で塗られるはず"
        );
    }

    // -----------------------------------------------------------------------
    // 消滅待機中の連結罫線(#242 修正1)
    // -----------------------------------------------------------------------

    #[test]
    fn pending_vanish_cells_keep_their_connected_border_until_the_flash_begins() {
        // #242。消滅待機中の接続罫線を現在の盤面(既にEmpty)だけで判定すると、隣も自分も
        // Emptyと見なされ、1つの塊だったはずの3マスがバラバラの単独ブロックに見えていた。
        // 待機中も消滅直前と同じ1つの塊として描かれることを確認する。
        let game = landed_and_vanished_game(600);
        let moved_map: HashMap<Pos, Pos> = game.recently_moved_blocks().iter().copied().collect();
        assert!(
            moved_map.contains_key(&(2, 0)),
            "テスト前提: (2,0)はまだ落下補間中(到着していない)であること"
        );
        for col in 1..=3 {
            assert!(
                game.pending_vanish_kind((2, col)).is_some(),
                "テスト前提: (2,{col})がフラッシュ開始待ちであること"
            );
        }

        let area = Rect::new(0, 0, CELL_W * 3, CELL_H);
        let mut pending_buf = Buffer::empty(area);
        for col in 1..=3u16 {
            let x = (col - 1) * CELL_W;
            draw_static_cell(
                &mut pending_buf,
                x,
                0,
                &game,
                (2, col as usize),
                BoardCell::Empty,
                &moved_map,
            );
        }

        // 消滅直前(まだ3マスが盤面上に並んでいた頃)の見た目。
        let before = three_red_cells_in_a_row_game();
        let no_moves: HashMap<Pos, Pos> = HashMap::new();
        let mut before_buf = Buffer::empty(area);
        for col in 1..=3u16 {
            let x = (col - 1) * CELL_W;
            draw_static_cell(
                &mut before_buf,
                x,
                0,
                &before,
                (2, col as usize),
                before.board.cell(2, col as usize),
                &no_moves,
            );
        }

        assert_eq!(
            buffer_symbol_rows(&pending_buf),
            buffer_symbol_rows(&before_buf),
            "消滅待機中も消滅直前と同じ罫線で描かれるはず"
        );
        assert_eq!(
            buffer_symbol_rows(&pending_buf),
            vec!["╭──────────╮".to_string(), "╰──────────╯".to_string()],
            "3マスが継ぎ目のない1つの塊として描かれるはず"
        );
    }

    #[test]
    fn pending_vanish_cells_switch_to_the_flash_color_once_the_flash_begins() {
        // 落下ブロックが到着してフラッシュが始まったら、連結罫線ではなくフラッシュ色に
        // 切り替わることを確認する(待機中の見た目を残し続けない)。
        let mut game = landed_and_vanished_game(600);
        let frame = std::time::Duration::from_millis(crate::constants::FRAME_INTERVAL_MS);
        for _ in 0..40 {
            if game.vanish_flash_progress((2, 1)).is_some() {
                break;
            }
            game.update(frame);
        }
        let t = game
            .vanish_flash_progress((2, 1))
            .expect("到着後はフラッシュに入っているはず");
        let moved_map: HashMap<Pos, Pos> = game.recently_moved_blocks().iter().copied().collect();

        let area = Rect::new(0, 0, CELL_W * 3, CELL_H);
        let mut buf = Buffer::empty(area);
        for col in 1..=3u16 {
            draw_static_cell(
                &mut buf,
                (col - 1) * CELL_W,
                0,
                &game,
                (2, col as usize),
                BoardCell::Empty,
                &moved_map,
            );
        }
        let flash_bg = colors::vanish_flash_bg(t);
        assert!(
            buf.content.iter().all(|cell| cell.bg == flash_bg),
            "フラッシュ開始後は3マスともフラッシュ色で塗られるはず"
        );
    }

    // -----------------------------------------------------------------------
    // カメラの小数スクロール(#242 修正2)
    // -----------------------------------------------------------------------

    /// テスト用ヘルパー: プレイヤーが(11,2)から連続で自由落下する、全Emptyの深い盤面。
    /// `extra`で落下の様子を観測するためのブロックを追加で置ける。
    fn free_falling_game() -> Game {
        let mut game = Game::new(1);
        for row in game.board.rows.iter_mut() {
            for cell in row.iter_mut() {
                *cell = BoardCell::Empty;
            }
        }
        game.player.row = 11;
        game.player.col = 2;
        game
    }

    /// テスト用ヘルパー: 自由落下の描画検証で使う可視領域(枠線の内側を模して原点をずらす)。
    fn field_inner_rect(visible_rows: usize) -> Rect {
        Rect::new(
            1,
            1,
            FIELD_WIDTH as u16 * CELL_W,
            visible_rows as u16 * CELL_H,
        )
    }

    /// テスト用ヘルパー: バッファ内で`pred`を満たすセルを含む最初の行(端末行)。
    fn first_row_matching(
        buf: &Buffer,
        pred: impl Fn(&ratatui::buffer::Cell) -> bool,
    ) -> Option<u16> {
        let area = *buf.area();
        (area.y..area.y + area.height).find(|&y| {
            (area.x..area.x + area.width).any(|x| pred(buf.cell(Position::new(x, y)).unwrap()))
        })
    }

    #[test]
    fn player_sprite_stays_on_its_fixed_screen_row_while_free_falling() {
        // #242。カメラをプレイヤーの補間後の位置から求めるようにしても、プレイヤー自身は
        // 常に画面内の固定行に留まること(spec.md 9.2)を、落下補間の全進捗で確認する。
        let mut game = free_falling_game();
        let visible_rows = 14;
        let psr = player_screen_row(visible_rows);
        let inner = field_inner_rect(visible_rows);
        let frame = std::time::Duration::from_millis(crate::constants::FRAME_INTERVAL_MS);

        let mut seen_progress = Vec::new();
        for _ in 0..40 {
            game.update(frame);
            if game.render_prev_position().0 == game.player.row {
                continue; // 落下補間中のフレームだけを見る
            }
            let cam = field_camera(&game, psr);
            let mut buf = Buffer::empty(inner);
            draw_player(&mut buf, inner, cam.row_f, &game);
            let sprite_row = first_row_matching(&buf, |cell| cell.fg == colors::PLAYER_FG)
                .expect("プレイヤースプライトが描かれているはず");
            assert_eq!(
                sprite_row,
                inner.y + psr as u16 * CELL_H,
                "落下補間の進捗{:.2}でもプレイヤーは固定行に留まるはず",
                game.move_anim_progress()
            );
            seen_progress.push(game.move_anim_progress());
        }
        assert!(
            seen_progress.iter().any(|&t| (0.4..0.6).contains(&t)),
            "テスト前提: 補間が半分ほど進んだフレームも観測できていること: {seen_progress:?}"
        );
    }

    #[test]
    fn field_scrolls_by_one_terminal_row_at_the_half_cell_camera_position() {
        // #242。カメラが論理行の途中(進捗0.5)にいるとき、静止しているブロックは端末で
        // 1行ぶん上へずれて描かれる(=セル単位でスナップせず滑らかにスクロールする)。
        let mut game = free_falling_game();
        game.board.rows.truncate(20);
        let rock_row = 19; // 最深行なので落下せず、その場に留まる
        game.board.rows[rock_row][5] = BoardCell::Rock { hits: 0 };

        let visible_rows = 14;
        let psr = player_screen_row(visible_rows);
        let inner = field_inner_rect(visible_rows);
        let frame = std::time::Duration::from_millis(crate::constants::FRAME_INTERVAL_MS);
        let no_moves: HashMap<Pos, Pos> = HashMap::new();

        // (top_row, dy) ごとの岩ブロックの描画行を集める。
        let mut samples: Vec<(usize, u16, u16)> = Vec::new();
        for _ in 0..40 {
            game.update(frame);
            let cam = field_camera(&game, psr);
            let mut buf = Buffer::empty(inner);
            draw_static_field(&mut buf, inner, &cam, visible_rows, &game, &no_moves);
            if let Some(row) = first_row_matching(&buf, |cell| cell.fg == colors::ROCK_X_FG) {
                samples.push((cam.top_row, cam.dy, row));
            }
        }

        let base = samples
            .iter()
            .find(|&&(_, dy, _)| dy == 0)
            .copied()
            .expect("カメラがちょうど論理行に乗るフレームがあるはず");
        let half = samples
            .iter()
            .find(|&&(top, dy, _)| top == base.0 && dy == 1)
            .copied()
            .expect("同じ論理行のままセルの半分だけスクロールしたフレームがあるはず");
        assert_eq!(
            half.2 + 1,
            base.2,
            "進捗が半分のフレームでは静止ブロックが端末1行ぶん上にずれているはず: {samples:?}"
        );
    }

    #[test]
    fn falling_blocks_never_jump_upward_across_a_player_fall_tick() {
        // #242。修正前はプレイヤーが1マス自由落下するたびにカメラが端末2行ぶん一気に
        // スナップし、落下中の他のブロックが1フレームで2行上へ飛んで(逆走して)見えた。
        // カメラは1フレームあたり最大1行ずつしか進まず、落下ブロックの描画行も
        // 1フレームで2行以上動かないことを確認する。
        let mut game = free_falling_game();
        game.board.rows[16][5] = BoardCell::Diamond; // プレイヤーの下方で一緒に落ちるブロック

        let visible_rows = 14;
        let psr = player_screen_row(visible_rows);
        let inner = field_inner_rect(visible_rows);
        let frame = std::time::Duration::from_millis(crate::constants::FRAME_INTERVAL_MS);

        let mut cam_offsets: Vec<i32> = Vec::new();
        let mut diamond_rows: Vec<i32> = Vec::new();
        let mut player_fall_ticks = 0;
        let mut prev_player_row = game.player.row;
        for _ in 0..60 {
            game.update(frame);
            if game.player.row != prev_player_row {
                player_fall_ticks += 1;
                prev_player_row = game.player.row;
            }
            let cam = field_camera(&game, psr);
            cam_offsets.push(cam.top_row as i32 * CELL_H as i32 + cam.dy as i32);

            let moved_map: HashMap<Pos, Pos> =
                game.recently_moved_blocks().iter().copied().collect();
            let mut buf = Buffer::empty(inner);
            draw_falling_blocks(&mut buf, inner, cam.row_f, visible_rows, &game, &moved_map);
            if let Some(row) = first_row_matching(&buf, |cell| cell.symbol() == "◆") {
                diamond_rows.push(row as i32);
            }
        }

        assert!(
            player_fall_ticks >= 3,
            "テスト前提: プレイヤーの自由落下tickを何度かまたいでいること({player_fall_ticks}回)"
        );
        for pair in cam_offsets.windows(2) {
            let step = pair[1] - pair[0];
            assert!(
                (0..=1).contains(&step),
                "カメラは逆走せず端末1行ずつ進むはず: {cam_offsets:?}"
            );
        }
        assert!(
            cam_offsets.last() > cam_offsets.first(),
            "テスト前提: 落下に伴いカメラが実際に進んでいること: {cam_offsets:?}"
        );
        assert!(
            diamond_rows.len() >= 10,
            "テスト前提: 落下中のブロックを十分な数のフレームで観測できていること"
        );
        for pair in diamond_rows.windows(2) {
            assert!(
                (pair[1] - pair[0]).abs() <= 1,
                "落下中のブロックが1フレームで2行以上飛ぶ(逆走して見える)ことはないはず: {diamond_rows:?}"
            );
        }
    }

    #[test]
    fn camera_near_the_surface_never_underflows() {
        // #242。プレイヤーが画面上の固定行より浅い位置(地表付近)にいる間は、カメラが
        // 負へ回り込まず先頭行で止まること。描画側でパニックしないことも合わせて確認する。
        let visible_rows = 14;
        let psr = player_screen_row(visible_rows);
        let inner = field_inner_rect(visible_rows);
        let no_moves: HashMap<Pos, Pos> = HashMap::new();

        for player_row in 0..=(psr + 2) {
            let mut game = free_falling_game();
            game.player.row = player_row;
            let cam = field_camera(&game, psr);
            if player_row <= psr {
                assert_eq!(cam.top_row, 0, "row={player_row}では先頭行で止まるはず");
                assert_eq!(
                    cam.dy, 0,
                    "row={player_row}では半端なスクロールも起きないはず"
                );
            }
            assert!(
                cam.row_f >= 0.0,
                "row={player_row}でカメラが負にならないはず"
            );

            let mut buf = Buffer::empty(inner);
            draw_static_field(&mut buf, inner, &cam, visible_rows, &game, &no_moves);
            draw_falling_blocks(&mut buf, inner, cam.row_f, visible_rows, &game, &no_moves);
            draw_bombs(&mut buf, inner, cam.row_f, visible_rows, &game);
            draw_player(&mut buf, inner, cam.row_f, &game);
            draw_off_screen_bomb_warnings(&mut buf, inner, cam.row_f, visible_rows, &game);
        }
    }
}

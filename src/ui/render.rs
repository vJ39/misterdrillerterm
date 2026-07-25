//! ratatui描画(spec.md 9章 TUI仕様)。
//!
//! 1論理セルを横4文字×縦2ターミナル行の大型ブロックとして描画する(9.2)。
//! 旧版のhalf-block方式(1論理セルを1文字に圧縮)は完全に廃止した。

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction as LayoutDirection, Layout, Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::constants::{FIELD_WIDTH, OXYGEN_MAX};
use crate::game::board::{Board, Cell as BoardCell, ColorKind};
use crate::game::player::Direction;
use crate::game::{Game, GameStatus};
use crate::ui::colors;

use super::intro;

// ---------------------------------------------------------------------------
// 9.1・9.2・9.8 画面サイズ関連の定数
// ---------------------------------------------------------------------------

/// 固定フレームの目安サイズ(9.1)。
const TOTAL_SCREEN_W: u16 = 74;
const TOTAL_SCREEN_H: u16 = 32;

/// フィールドペイン幅 = 12列×4文字+左右ボーダー2文字(9.2)。
const FIELD_PANE_W: u16 = 50;
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

fn compute_layout(area: Rect) -> LayoutPlan {
    if area.width >= TOTAL_SCREEN_W && area.height >= TOTAL_SCREEN_H {
        let frame_rect = centered_fixed_rect(TOTAL_SCREEN_W, TOTAL_SCREEN_H, area);

        let cols = Layout::default()
            .direction(LayoutDirection::Horizontal)
            .constraints([Constraint::Length(FIELD_PANE_W), Constraint::Length(HUD_PANE_W)])
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
        let field_width = FIELD_PANE_W.min(area.width);
        let field_rect = Rect {
            x: area.x,
            y: area.y,
            width: field_width,
            height: area.height,
        };
        let hud_width = area.width.saturating_sub(field_width).max(HUD_PANE_W_MIN);
        let hud_rect = Rect {
            x: area.x + field_width,
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

pub fn draw(frame: &mut Frame, game: &Game, sound_enabled: bool) {
    let area = frame.area();

    // 9.6実装上の注意: まずフレーム全体を明示的な背景色で塗りつぶしてから、その上に
    // ゲーム画面を重ねる(ターミナルのデフォルト背景色が縁に残ることを防ぐ)。
    frame
        .buffer_mut()
        .set_style(area, Style::default().fg(colors::LETTERBOX_BG).bg(colors::LETTERBOX_BG));

    if area.width < MIN_TERMINAL_W || area.height < MIN_TERMINAL_H {
        draw_size_warning(frame, area);
        return;
    }

    let plan = compute_layout(area);
    draw_field(frame, plan.field_rect, plan.visible_rows, game);
    draw_status(frame, plan.hud_rect, game);

    match game.status {
        GameStatus::Paused => draw_overlay(
            frame,
            plan.game_frame,
            "PAUSED",
            &format!(
                "Pキーで再開 / Qキーでタイトルへ / Sキーでサウンド{}",
                sound_label(sound_enabled)
            ),
        ),
        // 押し潰されてのミスは、GameOverオーバーレイを出す前に一呼吸「潰れた」演出
        // (draw_field内のdraw_player)を見せる(spec.md 5章・9章TERM独自拡張)。
        GameStatus::GameOver if !game.crush_flash_active() => {
            draw_overlay(frame, plan.game_frame, "GAME OVER", "Qキーでタイトルへ")
        }
        GameStatus::GameOver => {}
        GameStatus::Cleared => draw_overlay(frame, plan.game_frame, "CLEAR !", "Qキーでタイトルへ"),
        GameStatus::Playing => {}
    }
}

/// サウンドON/OFF状態を短い日本語ラベルにする(TERM独自拡張、10章)。
fn sound_label(sound_enabled: bool) -> &'static str {
    if sound_enabled { "ON" } else { "OFF" }
}

// ---------------------------------------------------------------------------
// タイトル画面(spec.md 1章「Qキーはタイトルへ戻る」の受け皿)
// ---------------------------------------------------------------------------

/// タイトル画面を描画する(ゲーム名+スタート案内のみのシンプルな画面)。
/// このタイトル画面上でのみ、Qキーがアプリ終了として扱われる(main.rsの画面遷移)。
pub fn draw_title(frame: &mut Frame, sound_enabled: bool) {
    let area = frame.area();

    frame
        .buffer_mut()
        .set_style(area, Style::default().fg(colors::LETTERBOX_BG).bg(colors::LETTERBOX_BG));

    let frame_rect = centered_fixed_rect(TOTAL_SCREEN_W, TOTAL_SCREEN_H, area);
    let title_area = centered_rect(70, 40, frame_rect);
    frame.render_widget(Clear, title_area);

    let title_style = Style::default().fg(colors::PANEL_TEXT).bg(colors::LETTERBOX_BG);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(colors::PANEL_BORDER).bg(colors::LETTERBOX_BG))
        .style(Style::default().bg(colors::LETTERBOX_BG));

    let paragraph = Paragraph::new(vec![
        Line::from(Span::styled("ミスタードリラーTERM", title_style)),
        Line::from(Span::styled("MISTER DRILLER TERM", title_style)),
        Line::from(""),
        Line::from(Span::styled("何かキーを押してスタート", title_style)),
        Line::from(Span::styled("(Qキーで終了)", title_style)),
        Line::from(Span::styled(
            format!("Sキーでサウンド切替(現在: {})", sound_label(sound_enabled)),
            title_style,
        )),
    ])
    .block(block)
    .style(Style::default().bg(colors::LETTERBOX_BG))
    .alignment(Alignment::Center);
    frame.render_widget(paragraph, title_area);
}

/// 起動時のスプラッシュ画面(AAピクセルアート)。`Game`を経由しない独立した画面
/// なので`GameStatus`には含めず、`crate::main`から起動直後に一度だけ直接呼ぶ
/// (詳細は`ui::intro`参照)。
pub fn draw_intro(frame: &mut Frame) {
    let area = frame.area();

    frame
        .buffer_mut()
        .set_style(area, Style::default().fg(colors::LETTERBOX_BG).bg(colors::LETTERBOX_BG));

    let canvas = intro::build_canvas();
    let art_lines = canvas.to_lines(1.0);
    let art_cols = art_lines.first().map(|line| line.spans.len()).unwrap_or(0) as u16;
    let art_rows = art_lines.len() as u16;

    let art_area = centered_fixed_rect(art_cols, art_rows, area);
    frame.render_widget(
        Paragraph::new(Text::from(art_lines))
            .style(Style::default().bg(colors::LETTERBOX_BG))
            .alignment(Alignment::Center),
        art_area,
    );

    let hint_area = Rect {
        x: area.x,
        y: art_area.y.saturating_add(art_area.height),
        width: area.width,
        height: 1,
    };
    if hint_area.y < area.y + area.height {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "何かキーを押して開始",
                Style::default().fg(colors::PANEL_TEXT).bg(colors::LETTERBOX_BG),
            )))
            .alignment(Alignment::Center),
            hint_area,
        );
    }
}

fn draw_size_warning(frame: &mut Frame, area: Rect) {
    let message = format!(
        "ターミナルサイズが不足しています(現在 {}x{} / 最小 {}x{} / 推奨 {}x{})。ウィンドウを広げてください",
        area.width, area.height, MIN_TERMINAL_W, MIN_TERMINAL_H, TOTAL_SCREEN_W, TOTAL_SCREEN_H
    );
    let style = Style::default().fg(colors::PANEL_TEXT).bg(colors::LETTERBOX_BG);
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
        .border_style(Style::default().fg(colors::PANEL_BORDER).bg(colors::LETTERBOX_BG))
        .style(Style::default().bg(colors::FIELD_EMPTY_BG));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 || visible_rows == 0 {
        return;
    }

    let player_screen_row = (visible_rows * PLAYER_SCREEN_ROW_RATIO_NUM / PLAYER_SCREEN_ROW_RATIO_DEN)
        .min(visible_rows.saturating_sub(1));
    let top_row = game.player.row.saturating_sub(player_screen_row);

    let buf = frame.buffer_mut();

    for screen_row in 0..visible_rows {
        let y = inner.y + screen_row as u16 * CELL_H;
        if y + CELL_H > inner.y + inner.height {
            break; // 縮退表示でinner.heightが可視行数ぶんに満たない場合の防御
        }

        let board_row = top_row + screen_row;
        for col in 0..FIELD_WIDTH {
            let x = inner.x + col as u16 * CELL_W;
            if x + CELL_W > inner.x + inner.width {
                break;
            }

            let cell = if board_row < game.board.depth_rows() {
                game.board.cell(board_row, col)
            } else {
                BoardCell::Empty
            };
            // プレイヤーがいる論理セルも含め、常にそのマス本来の内容を描画する
            // (プレイヤーは掘削・移動でEmptyになったマスにしか進入できないため、
            // 通常はここでEmpty背景が描かれるだけになる)。プレイヤー自身のスプライトは
            // このループの外側で、見た目補間アニメーション込みで別途重ねて描画する
            // (spec.md 9.5・9章TERM独自拡張)。
            draw_logical_cell(buf, x, y, &game.board, board_row, col, cell);
        }
    }

    draw_player(buf, inner, top_row, game);
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

    let px = inner.x as f32 + interp_col * CELL_W as f32;
    let py = inner.y as f32 + screen_row * CELL_H as f32;
    if px < 0.0 || py < 0.0 {
        return;
    }
    let x = px.round() as u16;
    let y = py.round() as u16;
    if x + CELL_W > inner.x + inner.width || y + CELL_H > inner.y + inner.height {
        return; // 補間の一時的なはみ出しは描画をスキップする(クラッシュ防止)
    }

    let cur_cell = if cur_row < game.board.depth_rows() {
        game.board.cell(cur_row, cur_col)
    } else {
        BoardCell::Empty
    };
    let bg = natural_cell_bg(&game.board, cur_row, cur_col, cur_cell);

    if game.crush_flash_active() {
        draw_crushed_sprite(buf, x, y, bg);
    } else {
        draw_player_sprite(buf, x, y, game.player.facing, bg);
    }
}

/// 1論理セルぶん(4文字×2行)を描画する。
fn draw_logical_cell(buf: &mut Buffer, x: u16, y: u16, board: &Board, row: usize, col: usize, cell: BoardCell) {
    match cell {
        BoardCell::Empty => fill_block(buf, x, y, colors::FIELD_EMPTY_BG),
        BoardCell::Color(kind) => draw_color_block(buf, x, y, board, row, col, kind),
        BoardCell::Rock { hits } => draw_rock_block(buf, x, y, board, row, col, hits),
        BoardCell::Oxygen => draw_fixed_unit(buf, x, y, [['○', '○'], ['○', '○']], colors::OXYGEN_FG, colors::OXYGEN_BG),
        BoardCell::Diamond => draw_fixed_unit(buf, x, y, [['◆', '◆'], ['◆', '◆']], colors::DIAMOND_FG, colors::DIAMOND_BG),
    }
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
fn conn_mask_by(board: &Board, row: usize, col: usize, same: impl Fn(BoardCell) -> bool) -> ConnMask {
    let check = |r: isize, c: isize| -> bool {
        r >= 0 && (r as usize) < board.depth_rows() && c >= 0 && (c as usize) < FIELD_WIDTH && same(board.cell(r as usize, c as usize))
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
    conn_mask_by(board, row, col, |cell| matches!(cell, BoardCell::Rock { .. }))
}


fn draw_color_block(buf: &mut Buffer, x: u16, y: u16, board: &Board, row: usize, col: usize, kind: ColorKind) {
    let mask = conn_mask(board, row, col, kind);
    let bg = colors::shaded_color(kind, colors::shade(mask.up, mask.down));
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
fn put_corner(buf: &mut Buffer, x: u16, y: u16, a_conn: bool, b_conn: bool, none_glyph: char, fg: Color, bg: Color) {
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

/// 岩・酸素・ダイヤ共通: 常に4方向とも非接続(=フルに縁取られた独立ユニット)として描く。
/// 角は丸罫線、辺の位置(中央2列×2行)には種類ごとの記号を敷き詰める(spec.md 9.4)。
fn draw_fixed_unit(buf: &mut Buffer, x: u16, y: u16, content: [[char; 2]; 2], fg: Color, bg: Color) {
    put(buf, x, y, '╭', fg, bg);
    put(buf, x + 3, y, '╮', fg, bg);
    put(buf, x, y + 1, '╰', fg, bg);
    put(buf, x + 3, y + 1, '╯', fg, bg);

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
fn draw_rock_block(buf: &mut Buffer, x: u16, y: u16, board: &Board, row: usize, col: usize, hits: u8) {
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

// --- 9.5 プレイヤースプライト ---

/// プレイヤーが立っているマス本来の背景色(9.5「そのマスの本来の背景色をそのまま使う」)。
fn natural_cell_bg(board: &Board, row: usize, col: usize, cell: BoardCell) -> Color {
    match cell {
        BoardCell::Empty => colors::FIELD_EMPTY_BG,
        BoardCell::Color(kind) => {
            let mask = conn_mask(board, row, col, kind);
            colors::shaded_color(kind, colors::shade(mask.up, mask.down))
        }
        BoardCell::Rock { hits } => colors::rock_bg(hits),
        BoardCell::Oxygen => colors::OXYGEN_BG,
        BoardCell::Diamond => colors::DIAMOND_BG,
    }
}

fn draw_player_sprite(buf: &mut Buffer, x: u16, y: u16, facing: Direction, bg: Color) {
    for (dy, line) in player_sprite(facing).iter().enumerate() {
        for (dx, ch) in line.chars().enumerate() {
            put(buf, x + dx as u16, y + dy as u16, ch, colors::PLAYER_FG, bg);
        }
    }
}

fn player_sprite(facing: Direction) -> [&'static str; 2] {
    match facing {
        Direction::Down => [" oo ", " \\/ "],
        Direction::Up => [" /\\ ", " oo "],
        Direction::Left => ["<oo ", "<== "],
        Direction::Right => [" oo>", " ==>"],
    }
}

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
        .border_style(Style::default().fg(colors::PANEL_BORDER).bg(colors::LETTERBOX_BG))
        .style(Style::default().bg(colors::LETTERBOX_BG));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let buf = frame.buffer_mut();
    let label_style = Style::default().fg(colors::PANEL_TEXT).bg(colors::LETTERBOX_BG);
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
    let air_style = Style::default().fg(colors::oxygen_bar_color(ratio)).bg(colors::LETTERBOX_BG);
    let gauge = air_gauge_string(ratio, game.player.oxygen_display());
    let air_text = if ratio < 0.3 {
        format!("  {gauge} \u{2620}") // ☠ 骸骨アイコン(spec.md 9.7・6章)
    } else {
        format!("  {gauge}")
    };
    write_line(buf, inner, &mut row, &air_text, air_style);
    write_line(buf, inner, &mut row, "", label_style);

    write_line(buf, inner, &mut row, "LIVES", label_style);
    write_line(buf, inner, &mut row, &format!("  \u{2665} \u{d7}{}", game.player.lives), label_style);
    write_line(buf, inner, &mut row, "", label_style);

    write_line(buf, inner, &mut row, "TIME", label_style);
    let elapsed = game.player.elapsed_seconds as u32;
    write_line(buf, inner, &mut row, &format!("  {:02}:{:02}", elapsed / 60, elapsed % 60), label_style);
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
    format!("[{}{}] {}%", "#".repeat(filled), "\u{2591}".repeat(empty), percent)
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

fn draw_overlay(frame: &mut Frame, area: Rect, title: &str, hint: &str) {
    let overlay_area = centered_rect(40, 20, area);
    frame.render_widget(Clear, overlay_area);

    let text_style = Style::default().fg(colors::PANEL_TEXT).bg(colors::LETTERBOX_BG);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(colors::PANEL_BORDER).bg(colors::LETTERBOX_BG))
        .style(Style::default().bg(colors::LETTERBOX_BG));

    let paragraph = Paragraph::new(vec![
        Line::from(Span::styled(title, text_style)),
        Line::from(Span::styled(hint, text_style)),
    ])
    .block(block)
    .style(Style::default().bg(colors::LETTERBOX_BG))
    .alignment(Alignment::Center);
    frame.render_widget(paragraph, overlay_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board_with(rows: usize) -> Board {
        Board {
            rows: vec![[BoardCell::Empty; FIELD_WIDTH]; rows],
        }
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
                assert_eq!(symbol, "─", "継ぎ目(x={x},y={y})は縦線で区切られず、横線で繋がっているはず");
            }
        }

        // 継ぎ目をまたぐ左右の背景色も一致し、色ムラなく1つの塊に見える。
        let left_bg = buf.cell(Position::new(3, 0)).unwrap().bg;
        let right_bg = buf.cell(Position::new(4, 0)).unwrap().bg;
        assert_eq!(left_bg, right_bg, "継ぎ目の左右で背景色(シェーディング)が食い違ってはいけない");
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
}

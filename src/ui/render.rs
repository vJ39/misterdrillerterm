//! ratatui描画(spec.md 9章 TUI仕様)。

use ratatui::layout::{Alignment, Constraint, Direction as LayoutDirection, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, Paragraph};
use ratatui::Frame;

use crate::constants::{DEPTH_SCORE_MULTIPLIER, FIELD_WIDTH, OXYGEN_MAX, OXYGEN_WARNING_THRESHOLD};
use crate::game::{Game, GameStatus};
use crate::ui::colors;

/// プレイヤーを画面内の何行目(可視行数に対する比率)に固定表示するか。
/// spec.md 9章「プレイヤーは常に画面内の固定行(例: 上から1/3の位置)に表示」に対応。
const PLAYER_SCREEN_ROW_RATIO_NUM: usize = 1;
const PLAYER_SCREEN_ROW_RATIO_DEN: usize = 3;

/// フィールド幅 = 12列×2文字+左右のボーダー分(spec.md 9章)。
fn field_pane_width() -> u16 {
    (FIELD_WIDTH as u16) * 2 + 2
}

pub fn draw(frame: &mut Frame, game: &Game) {
    let area = frame.area();

    let chunks = Layout::default()
        .direction(LayoutDirection::Horizontal)
        .constraints([Constraint::Length(field_pane_width()), Constraint::Min(20)])
        .split(area);

    draw_field(frame, chunks[0], game);
    draw_status(frame, chunks[1], game);

    match game.status {
        GameStatus::Paused => draw_overlay(frame, area, "PAUSED", "Pキーで再開 / Qキーで終了"),
        GameStatus::GameOver => draw_overlay(frame, area, "GAME OVER", "Qキーで終了"),
        GameStatus::Cleared => draw_overlay(frame, area, "CLEAR !", "Qキーで終了"),
        GameStatus::Playing => {}
    }
}

fn draw_field(frame: &mut Frame, area: Rect, game: &Game) {
    let block = Block::default().borders(Borders::ALL).title("フィールド");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let visible_rows = inner.height as usize;
    if visible_rows == 0 {
        return;
    }

    let player_screen_row = (visible_rows * PLAYER_SCREEN_ROW_RATIO_NUM / PLAYER_SCREEN_ROW_RATIO_DEN)
        .min(visible_rows.saturating_sub(1));
    let top_row = game.player.row.saturating_sub(player_screen_row);

    let mut lines: Vec<Line> = Vec::with_capacity(visible_rows);
    for screen_row in 0..visible_rows {
        let board_row = top_row + screen_row;
        if board_row >= game.board.depth_rows() {
            lines.push(Line::from(""));
            continue;
        }

        let mut spans: Vec<Span> = Vec::with_capacity(FIELD_WIDTH);
        for col in 0..FIELD_WIDTH {
            let cell = game.board.cell(board_row, col);
            let bg = colors::cell_bg(cell);
            let is_player = board_row == game.player.row && col == game.player.col;
            if is_player {
                spans.push(Span::styled("@ ", Style::default().fg(colors::PLAYER_FG).bg(bg)));
            } else {
                spans.push(Span::styled(colors::cell_glyph(cell), Style::default().bg(bg)));
            }
        }
        lines.push(Line::from(spans));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_status(frame: &mut Frame, area: Rect, game: &Game) {
    let block = Block::default().borders(Borders::ALL).title("ステータス");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(LayoutDirection::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(inner);

    frame.render_widget(
        Paragraph::new(format!("深度      : {} m", game.player.depth_m())),
        rows[0],
    );

    let oxygen_ratio = (game.player.oxygen / OXYGEN_MAX).clamp(0.0, 1.0) as f64;
    let oxygen_gauge = Gauge::default()
        .gauge_style(Style::default().fg(oxygen_color(game.player.oxygen)))
        .ratio(oxygen_ratio)
        .label(format!("酸素 {}", game.player.oxygen_display()));
    frame.render_widget(oxygen_gauge, rows[1]);

    frame.render_widget(
        Paragraph::new(format!(
            "スコア    : {}",
            game.player.total_score(DEPTH_SCORE_MULTIPLIER)
        )),
        rows[2],
    );

    let elapsed = game.player.elapsed_seconds as u32;
    frame.render_widget(
        Paragraph::new(format!("経過タイム: {:02}:{:02}", elapsed / 60, elapsed % 60)),
        rows[3],
    );

    frame.render_widget(
        Paragraph::new(format!("次CPまで  : {} m", game.distance_to_next_checkpoint_m())),
        rows[4],
    );
}

fn oxygen_color(oxygen: f32) -> Color {
    if oxygen <= OXYGEN_WARNING_THRESHOLD {
        Color::Rgb(255, 90, 90)
    } else {
        colors::OXYGEN_BG
    }
}

fn draw_overlay(frame: &mut Frame, area: Rect, title: &str, hint: &str) {
    let overlay_area = centered_rect(40, 20, area);
    frame.render_widget(Clear, overlay_area);

    let paragraph = Paragraph::new(vec![Line::from(title), Line::from(hint)])
        .block(Block::default().borders(Borders::ALL))
        .alignment(Alignment::Center);
    frame.render_widget(paragraph, overlay_area);
}

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

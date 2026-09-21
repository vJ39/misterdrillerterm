//! ratatui描画(spec.md 9章 TUI仕様)。
//! 1論理セルを横4文字×縦2ターミナル行の大型ブロックとして描画する(9.2)。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{
    Alignment, Constraint, Direction as LayoutDirection, Layout, Position, Rect,
};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::battle::BattleOutcome;
use crate::constants::{
    BOMB_DANGER_MS, BOMB_ROLL_MS, BONUS_FLOOR_DEPTH_M, CHECKPOINT_SAFE_ZONE_M, CHECKPOINT_STEP_M,
    CHECKPOINT_ZONE_GAP_M, DEBUG_INCOMING_ATTACK_POWER, INCOMING_ROCK_WARNING_MS, OXYGEN_MAX,
    STAR_MELT_DURATION_MS, STAR_SPARKLE_PERIOD_MS, STAR_VISIBLE_GRACE_MS,
};
use crate::game::board::{Board, Cell as BoardCell, ColorKind, ItemEffect, Pos};
use crate::game::player::Direction;
use crate::game::{BombPhase, Game, GameOverChoice, GameStatus};
use crate::lobby::{LobbyPhase, LobbyState};
use crate::text_edit::TextEditState;
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

/// ヘルプ画面のオーバーレイ枠の高さ(`centered_rect`のパーセント指定)。
/// 内容行数が増えて枠に収まらなくなったら上げる(収まっているかは
/// `help_screen_box_is_tall_enough_...`で確認する)。
const HELP_OVERLAY_PERCENT_Y: u16 = 95;

/// 設定画面のオーバーレイ枠の大きさ。横幅はゲームフレームに対する割合、高さは行数で指定する。
/// #304で項目が29個になり、フレーム(32行)に対する割合では内容が収まらなくなったため、
/// 高さだけは端末の高さを使う固定行数に切り替えた(収まっているかは
/// `settings_screen_box_is_tall_enough_...`で確認する)。
const SETTINGS_BOX_PERCENT_X: u16 = 60;
const SETTINGS_BOX_H: u16 = 34;

/// 対戦ロビー画面(#256)の枠の高さ(`centered_rect`のパーセント指定)。候補リストが
/// 伸びても収まるよう、ヘルプ画面と同程度に取る。
const LOBBY_OVERLAY_PERCENT_Y: u16 = 60;

/// 表示名入力画面(#270)の枠の大きさ。内容は見出し・空行・入力行・空行・操作案内2行の
/// 6行で固定のため、パーセントではなく行数・桁数で取る(上下ボーダー2行を含む)。
const PLAYER_NAME_INPUT_BOX_W: u16 = 52;
const PLAYER_NAME_INPUT_BOX_H: u16 = 8;

/// 巻き戻し中オーバーレイ(#233)の枠の高さ(行数)。内容2行+上下ボーダー2行。
/// 中央ではなく画面下端に寄せるため、割合ではなく固定行数で指定する(GameOver
/// ダイアログ等、中央に出る他のオーバーレイと重ならないようにするため)。
const REWIND_OVERLAY_H: u16 = 4;

/// 対戦の待機中オーバーレイ(#302)の横幅(`centered_rect`のパーセント指定)。覆い隠す対象の
/// GameOverダイアログ(幅40%)より広く、かつ相手パネル(#290)を余計に隠さない程度に留める。
/// 縦幅はダイアログ側の`game_over_overlay_percent_y`から引くため、ここには持たない。
const BATTLE_WAITING_OVERLAY_PERCENT_X: u16 = 50;

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

/// 設定画面の枠(9.10)。横幅は他のオーバーレイと揃えてゲームフレームの割合で取り、
/// 高さは項目数ぶんの固定行数を端末の高さまで使って確保する(#304)。
fn settings_box_rect(area: Rect, frame_rect: Rect) -> Rect {
    let height = SETTINGS_BOX_H.min(area.height);
    Rect {
        y: area.y + (area.height - height) / 2,
        height,
        ..centered_rect(SETTINGS_BOX_PERCENT_X, 100, frame_rect)
    }
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
        // ミスは死因・ライフの残りを問わず、GameOverオーバーレイを出す前に「天に召される」
        // 演出(draw_field内のdraw_player)を見せ切る(spec.md 5章・9.5)。
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

/// 対戦画面(#252)の1フレーム。自分の盤面は通常プレイと同じ`draw`で描き、他の参加者の
/// 盤面はフル描画せず深度・ライフ・進捗バーの3値だけをパネルに重ねる(spec.md 12.3)。
///
/// `other_games`・`opponent_names`は自分以外の全参加者(N人対戦では最大3人)で、同じindexで
/// 対応する。パネルは全員ぶんを縦に積み(#290)、あわせて各参加者の現在位置を自分の盤面へ
/// ゴーストとして重ねる(#301)。
/// `outcome`が`Some`なら決着しているので、結果を中央に重ねる(#256)。
pub fn draw_battle(
    frame: &mut Frame,
    game_local: &Game,
    other_games: &[Game],
    opponent_names: &[String],
    music_enabled: bool,
    se_enabled: bool,
    outcome: Option<BattleOutcome>,
) {
    // オートプレイは対戦では使わないため常にfalseを渡す。
    draw(frame, game_local, music_enabled, se_enabled, false);

    let area = frame.area();
    if area.width < MIN_TERMINAL_W || area.height < MIN_TERMINAL_H {
        return;
    }

    let plan = compute_layout(area, game_local.board.width());

    // 相手パネル(#290)が画面下端を占める行数。ゴースト(#301)の画面外矢印は本来
    // 盤面の下端に出るが、それだとパネルの裏に隠れて見えなくなる(実機で発見)。
    // パネルの開始位置より下には矢印を描かせないことで、常に見える位置へ収める。
    let panel_area = bottom_anchored_rect(
        90,
        battle_opponent_panel_h(other_games.len()),
        plan.game_frame,
    );
    draw_opponent_ghosts(
        frame.buffer_mut(),
        plan.field_rect,
        plan.visible_rows,
        game_local,
        other_games,
        panel_area.y,
    );
    draw_battle_opponent_panel(frame, panel_area, other_games, opponent_names);

    // 自分が力尽きても、対戦は全員の結果がそろうまで決着しない(#289)。その間は通常プレイの
    // GameOverダイアログ(タイトルへ戻る/その場から復活)の操作を`tick_battle`が受け付け
    // ないため、押しても何も起きないダイアログを待機中の案内で覆い隠す(#302)。
    if battle_local_is_waiting_for_others(game_local, outcome) {
        draw_overlay_sized(
            frame,
            plan.game_frame,
            BATTLE_WAITING_OVERLAY_PERCENT_X,
            // ダイアログは巻き戻しヒントの有無で高さが変わるので、高い方に合わせる。
            game_over_overlay_percent_y(true),
            "GAME OVER",
            &["対戦終了までお待ちください", "決着まで操作できません"],
        );
    }

    // 決着していれば結果を中央に重ねる(#256)。盤面・相手パネルはそのまま残し、
    // 最後の状態を見ながら結果を確認できるようにする。
    if let Some(outcome) = outcome {
        draw_overlay(
            frame,
            plan.game_frame,
            &battle_outcome_message(outcome),
            &["Enter/Escキーでタイトルへ"],
        );
    }
}

/// 相手パネルの高さ(行数)。自分以外の参加者を1人1行で縦に積み、上下ボーダー2行を
/// 足す(#290/#305)。巻き戻し中オーバーレイと同じく画面下端に寄せる。
fn battle_opponent_panel_h(opponent_count: usize) -> u16 {
    opponent_count as u16 + 2
}

/// 相手パネル(#252)。自分以外の全参加者を1人1行(名前・深度・ライフ・進捗率)に圧縮して
/// 縦に積む(#290/#305。3人以上だと3行×人数が画面を占有しすぎたため、1行化した)。
/// 見出しの色と番号は自分の盤面のゴースト(#301)と揃えて、どの行がどのゴーストなのかを
/// 対応づけられるようにする。
fn draw_battle_opponent_panel(
    frame: &mut Frame,
    panel_area: Rect,
    other_games: &[Game],
    opponent_names: &[String],
) {
    if other_games.is_empty() {
        return;
    }

    frame.render_widget(Clear, panel_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(colors::PANEL_BORDER)
                .bg(colors::LETTERBOX_BG),
        )
        .style(Style::default().bg(colors::LETTERBOX_BG));

    let mut lines = Vec::with_capacity(other_games.len());
    for (index, game) in other_games.iter().enumerate() {
        // `other_games`と`opponent_names`は同じindexで対応する。
        let name = opponent_names.get(index).map_or("", String::as_str);
        let heading_style = Style::default()
            .fg(colors::battle_ghost_fg(index))
            .bg(colors::LETTERBOX_BG);
        let depth_m = game.player.depth_m();
        let ratio = battle_progress_ratio(depth_m, game.depth_goal_m());
        let percent = (ratio * 100.0).round() as u32;

        lines.push(Line::from(Span::styled(
            format!(
                "[{}] {name}  {depth_m}m \u{2665}\u{d7}{}  {percent}%",
                ghost_marker_glyph(index),
                game.player.lives
            ),
            heading_style,
        )));
    }

    let paragraph = Paragraph::new(lines)
        .block(block)
        .style(Style::default().bg(colors::LETTERBOX_BG))
        .alignment(Alignment::Center);
    frame.render_widget(paragraph, panel_area);
}

/// 自分が力尽きた後、他の参加者の決着を待っている状態か(#302)。`draw`がGameOver
/// ダイアログを出すのと同じ条件(「天に召される」演出を見せ切った後)で切り替える。
fn battle_local_is_waiting_for_others(game_local: &Game, outcome: Option<BattleOutcome>) -> bool {
    outcome.is_none()
        && game_local.status == GameStatus::GameOver
        && !game_local.crush_flash_active()
}

/// 相手の進捗(深度÷ゴール深度)を0.0〜1.0で返す。ゴール深度0の盤面は存在しないが、
/// 0除算を避けるため0.0として扱う。
fn battle_progress_ratio(depth_m: usize, depth_goal_m: usize) -> f32 {
    if depth_goal_m == 0 {
        return 0.0;
    }
    (depth_m as f32 / depth_goal_m as f32).clamp(0.0, 1.0)
}

/// 決着の表示文字列(#256)。順位方式(#273)になったため自分の順位をそのまま出す。
fn battle_outcome_message(outcome: BattleOutcome) -> String {
    match outcome {
        BattleOutcome::Ranked(rank) => format!("RANK {rank}"),
    }
}

// ---------------------------------------------------------------------------
// 対戦ロビー画面(#256。spec.md 12.1)
// ---------------------------------------------------------------------------

/// ロビー画面(`Screen::NetworkLobby`)を描画する。候補リスト・招待ダイアログ・接続中・
/// 通知をフェーズごとに出し分ける。
pub fn draw_network_lobby(frame: &mut Frame, lobby: &LobbyState) {
    let area = frame.area();

    frame.buffer_mut().set_style(
        area,
        Style::default()
            .fg(colors::LETTERBOX_BG)
            .bg(colors::LETTERBOX_BG),
    );

    let frame_rect = centered_fixed_rect(TOTAL_SCREEN_W, TOTAL_SCREEN_H, area);
    let lobby_area = centered_rect(90, LOBBY_OVERLAY_PERCENT_Y, frame_rect);
    frame.render_widget(Clear, lobby_area);

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

    let line = |text: String| Line::from(Span::styled(text, text_style));
    let heading = |text: &str| Line::from(Span::styled(text.to_string(), heading_style));

    let mut lines = vec![
        heading("== 対戦相手をさがす =="),
        line(format!("自分: {}", lobby.my_name())),
        Line::from(""),
    ];

    match lobby.phase() {
        LobbyPhase::Discovering { .. } => {
            lines.push(heading("== 見つかった相手 =="));
            if lobby.peers().is_empty() {
                lines.push(line("さがしています...".to_string()));
            } else {
                for (index, peer) in lobby.peers().iter().enumerate() {
                    let selected = index == lobby.selection();
                    let marker = if selected { "> " } else { "  " };
                    let style = if selected { selected_style } else { text_style };
                    lines.push(Line::from(Span::styled(
                        format!("{marker}{} ({})", peer.player_name, peer.addr),
                        style,
                    )));
                }
            }
            // ルームを開いている(1人以上迎え入れた)場合は参加者と開始操作を出す(#276)。
            let guests = lobby.hosted_guests();
            if !guests.is_empty() {
                lines.push(Line::from(""));
                lines.push(heading("== ルームの参加者 =="));
                lines.push(line(format!("  {} (自分)", lobby.my_name())));
                for guest in guests {
                    lines.push(line(format!("  {}", guest.name())));
                }
            }
            lines.push(Line::from(""));
            lines.push(line(
                "↑↓: 選択 / Enter: 対戦を申し込む / Esc: タイトルへ".to_string(),
            ));
            if !guests.is_empty() {
                lines.push(line(format!(
                    "Tab: この{}人で対戦をはじめる",
                    guests.len() + 1
                )));
            }
            // 相手が見つからなくても遊べる入口(#296)。通信は使わない。
            lines.push(line("V: AIと対戦".to_string()));
            // 人間の参加者にAIを混ぜる枠(#300)。1つ上のV(通信を使わないAI対戦)とは
            // 別物なので、「このルームに」「通信あり」と書いて取り違えを防ぐ。
            lines.push(line(format!(
                "I/D: このルームにAIを追加(通信あり) 今{}人",
                lobby.room_ai_count()
            )));
        }
        LobbyPhase::SelectingAiOpponentCount { ai_count } => {
            lines.push(heading("== AIと対戦 =="));
            lines.push(line(format!(
                "AIの人数: {ai_count} (合計{}人)",
                ai_count + 1
            )));
            lines.push(Line::from(""));
            lines.push(line(
                "↑↓: 人数を変更 / Enter: 開始 / Esc: やめる".to_string(),
            ));
        }
        LobbyPhase::AwaitingInviteResponse { target, .. } => {
            lines.push(line(format!(
                "「{}」に対戦を申し込みました。返事を待っています...",
                target.player_name
            )));
            lines.push(Line::from(""));
            lines.push(line("Esc: 取り消す".to_string()));
        }
        LobbyPhase::IncomingInvite { from, .. } => {
            lines.push(line(format!(
                "「{}」から対戦を申し込まれました",
                from.player_name
            )));
            lines.push(Line::from(""));
            lines.push(line("Enter: 受ける / Esc: ことわる".to_string()));
        }
        LobbyPhase::AcceptingGuestConnection { guest_name, .. } => {
            lines.push(line(format!("「{guest_name}」の接続を待っています...")));
        }
        LobbyPhase::ConnectingToHost { host_name, .. } => {
            lines.push(line(format!("「{host_name}」へ接続しています...")));
        }
        LobbyPhase::WaitingForRoomStart { .. } => {
            lines.push(line(
                "ルームに参加しました。開始を待っています...".to_string(),
            ));
            lines.push(Line::from(""));
            // 開始はゲストからも出せる(#293)。
            lines.push(line("Tab: 自分から対戦をはじめる".to_string()));
        }
        LobbyPhase::Notice { message, .. } => {
            lines.push(line(message.clone()));
        }
    }

    let paragraph = Paragraph::new(lines)
        .block(block)
        .style(Style::default().bg(colors::LETTERBOX_BG))
        .alignment(Alignment::Left);
    frame.render_widget(paragraph, lobby_area);
}

// ---------------------------------------------------------------------------
// 表示名の入力画面(#270)
// ---------------------------------------------------------------------------

/// 編集中の1行を、カーソル位置と選択範囲が見える形のSpan列へ組む。
///
/// 選択範囲は背景色で塗り、カーソル位置は反転表示にする。カーソルが選択範囲の内側にある
/// (後ろから前へ選択した)場合は選択の塗りをそのまま優先し、カーソルの反転は出さない
/// (選択の端がカーソルなので、範囲が見えていれば位置も分かる)。カーソルが末尾にある
/// ときは、文字が無いので空白1つを反転表示してそこに置く。
fn player_name_input_spans(state: &TextEditState, text_style: Style) -> Vec<Span<'static>> {
    let cursor_style = text_style.add_modifier(Modifier::REVERSED);
    let selection_style = text_style.bg(Color::DarkGray);
    let selection = state.selection_range();

    let mut spans: Vec<Span<'static>> = Vec::new();
    for (index, c) in state.chars().iter().enumerate() {
        let selected = selection.is_some_and(|(start, end)| index >= start && index < end);
        let style = if selected {
            selection_style
        } else if index == state.cursor() {
            cursor_style
        } else {
            text_style
        };
        spans.push(Span::styled(c.to_string(), style));
    }
    if state.cursor() >= state.chars().len() {
        spans.push(Span::styled(" ".to_string(), cursor_style));
    }
    spans
}

/// 表示名の入力画面(`Screen::PlayerNameInput`)を描画する(#270)。ロビー画面と同じ
/// 中央の枠付きボックスに、見出し・編集中の1行・操作案内を出す。
pub fn draw_player_name_input(frame: &mut Frame, state: &TextEditState) {
    let area = frame.area();

    frame.buffer_mut().set_style(
        area,
        Style::default()
            .fg(colors::LETTERBOX_BG)
            .bg(colors::LETTERBOX_BG),
    );

    let frame_rect = centered_fixed_rect(TOTAL_SCREEN_W, TOTAL_SCREEN_H, area);
    let box_area =
        centered_fixed_rect(PLAYER_NAME_INPUT_BOX_W, PLAYER_NAME_INPUT_BOX_H, frame_rect);
    frame.render_widget(Clear, box_area);

    let text_style = Style::default()
        .fg(colors::PANEL_TEXT)
        .bg(colors::LETTERBOX_BG);
    let heading_style = Style::default()
        .fg(colors::PANEL_BORDER)
        .bg(colors::LETTERBOX_BG);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(colors::PANEL_BORDER)
                .bg(colors::LETTERBOX_BG),
        )
        .style(Style::default().bg(colors::LETTERBOX_BG));

    let lines = vec![
        Line::from(Span::styled("== 名前を入力 ==".to_string(), heading_style)),
        Line::from(""),
        Line::from(player_name_input_spans(state, text_style)),
        Line::from(""),
        Line::from(Span::styled(
            "Enter: 決定 / Esc: タイトルへ".to_string(),
            text_style,
        )),
        Line::from(Span::styled(
            "Ctrl+A: 全選択 / Shift+←→: 範囲選択".to_string(),
            text_style,
        )),
    ];

    let paragraph = Paragraph::new(lines)
        .block(block)
        .style(Style::default().bg(colors::LETTERBOX_BG))
        .alignment(Alignment::Left);
    frame.render_widget(paragraph, box_area);
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
        // 対戦(#256)はNキー。押せることが分からないと入口として機能しないため、
        // 設定・ヘルプと同じ行に並べる。
        Line::from(Span::styled(
            "(Sキーで設定 / Hキーでヘルプ / Nキーで対戦)",
            text_style,
        )),
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
        line(&format!(
            "O: 相手から攻撃力{DEBUG_INCOMING_ATTACK_POWER}を受け取る(自分の溜め分と相殺し、残りが降る)"
        )),
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
    /// ボム設置(Ticking開始)から爆発までの時間(ms)。
    BombFuse,
    /// 対戦の妨害ルール(#247)で、岩1個を降らせるのに必要な攻撃力。
    AttackBlocksPerRock,
    /// 対戦の妨害ルール(#247)で、1回に降らせる岩の個数上限。
    AttackRocksPerWaveMax,
    /// 対戦の妨害ルール(#304)で、ボム1個を降らせるのに必要な攻撃力。
    AttackBlocksPerBomb,
    /// 対戦の妨害ルール(#304)で、1回に降らせるボムの個数上限。
    AttackBombsPerWaveMax,
    /// 対戦の妨害ルール(#304)で、攻撃力のうちボムとして送る割合(%、0=岩のみ)。
    AttackBombRatioPercent,
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
            SettingsChoice::BombRate => SettingsChoice::BombFuse,
            SettingsChoice::BombFuse => SettingsChoice::AttackBlocksPerRock,
            SettingsChoice::AttackBlocksPerRock => SettingsChoice::AttackRocksPerWaveMax,
            SettingsChoice::AttackRocksPerWaveMax => SettingsChoice::AttackBlocksPerBomb,
            SettingsChoice::AttackBlocksPerBomb => SettingsChoice::AttackBombsPerWaveMax,
            SettingsChoice::AttackBombsPerWaveMax => SettingsChoice::AttackBombRatioPercent,
            SettingsChoice::AttackBombRatioPercent => SettingsChoice::DebugLogEnabled,
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
            SettingsChoice::DebugLogEnabled => SettingsChoice::AttackBombRatioPercent,
            SettingsChoice::AttackBombRatioPercent => SettingsChoice::AttackBombsPerWaveMax,
            SettingsChoice::AttackBombsPerWaveMax => SettingsChoice::AttackBlocksPerBomb,
            SettingsChoice::AttackBlocksPerBomb => SettingsChoice::AttackRocksPerWaveMax,
            SettingsChoice::AttackRocksPerWaveMax => SettingsChoice::AttackBlocksPerRock,
            SettingsChoice::AttackBlocksPerRock => SettingsChoice::BombFuse,
            SettingsChoice::BombFuse => SettingsChoice::BombRate,
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
    bomb_fuse_ms: u32,
    attack_blocks_per_rock: u32,
    attack_rocks_per_wave_max: u32,
    attack_blocks_per_bomb: u32,
    attack_bombs_per_wave_max: u32,
    attack_bomb_ratio_percent: u32,
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
    // #246で項目が23個になりさらに1行増えたため97%(31行)へ広げた。
    // #247で対戦の2項目が加わって26個になったが、97%より高くはできないため、
    // 見出しの下と案内の上にあった空行2行を削って収めている。
    // #304で対戦のボム3項目が加わって29個になり、フレーム基準の割合では足りないため、
    // 端末の高さを使う固定行数(`SETTINGS_BOX_H`)へ切り替えた。
    let settings_area = settings_box_rect(area, frame_rect);
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
    let count_line = |label: &str, count: u32, is_selected: bool| {
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
        count_line(
            "色数",
            u32::from(color_count),
            selection == SettingsChoice::ColorCount,
        ),
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
        ms_line(
            "ボム爆発までの時間",
            u64::from(bomb_fuse_ms),
            selection == SettingsChoice::BombFuse,
        ),
        count_line(
            "対戦: 岩1個に必要な攻撃力",
            attack_blocks_per_rock,
            selection == SettingsChoice::AttackBlocksPerRock,
        ),
        count_line(
            "対戦: 一度に降る岩の上限",
            attack_rocks_per_wave_max,
            selection == SettingsChoice::AttackRocksPerWaveMax,
        ),
        count_line(
            "対戦: ボム1個に必要な攻撃力",
            attack_blocks_per_bomb,
            selection == SettingsChoice::AttackBlocksPerBomb,
        ),
        count_line(
            "対戦: 一度に降るボムの上限",
            attack_bombs_per_wave_max,
            selection == SettingsChoice::AttackBombsPerWaveMax,
        ),
        rate_line(
            "対戦: 攻撃力のボム化比率",
            attack_bomb_ratio_percent,
            selection == SettingsChoice::AttackBombRatioPercent,
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
            u32::from(rewind_stock_max),
            selection == SettingsChoice::RewindStockMax,
        ),
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
    draw_incoming_rock_warnings(buf, inner, cam.row_f, visible_rows, game);
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
            } else if is_unrevealed_future_zone(board_row, game.last_checkpoint_reported()) {
                // まだ掘り抜いていないチェックポイントのギャップより先(次の100mゾーン)は、
                // そこに何が生成されていても見せない(TERM独自拡張。#197/#281)。
                BoardCell::Empty
            } else if board_row < game.board.depth_rows() {
                game.board.cell(board_row, col)
            } else {
                // 盤面外(ゴールより深い行)。draw_static_cellが地面として描く。
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
/// 優先順: 盤面の底(フィールドより深い、実データの無い行)はクリア前後を問わず地底の
/// 地面 > 爆風直後のセルは炎色で一瞬覆う > フラッシュ中のセルはフラッシュしてから
/// 背景色へ消える > 消滅は確定したが落下ブロックの到着待ちのセルは消滅前の見た目の
/// まま(#234) > チェックポイント安全地帯のEmptyは地面ビジュアル > 通常描画。
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
    if board_row >= game.board.depth_rows() {
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

/// 相手の攻撃で降ってくる岩(#247)の予告を描く。点滅・赤色・炎は一切使わず、
///
/// - 落下経路(出現予定マスから下方向にEmptyが続く区間)の背景を`INCOMING_ROCK_PATH_BG`へ
/// - 出現予定マス自体は、残り時間に応じてフィールド背景色から岩の地色へ近づく背景へ
///
/// 変えるだけにとどめる。画面外(カメラより上)にある予告についても、ボムのような赤い
/// 警告ラインは出さない(spec.md 12.8「控えめな予告」)。
fn draw_incoming_rock_warnings(
    buf: &mut Buffer,
    inner: Rect,
    cam_row_f: f32,
    visible_rows: usize,
    game: &Game,
) {
    if game.incoming_rocks().is_empty() {
        return;
    }
    let player_pos = game.player.position();
    // 設置済み(Settling/Ticking)のボムはCellグリッド外のオーバーレイなので、盤面のセル
    // だけを見ると落下経路が通り抜けているように見えてしまう。
    let blocked_by_bomb = |pos: Pos| {
        game.bombs()
            .iter()
            .any(|b| b.pos == pos && matches!(b.phase, BombPhase::Settling | BombPhase::Ticking))
    };
    for rock in game.incoming_rocks() {
        let (spawn_row, col) = rock.pos;

        // 落下経路: 出現予定マスの1つ下から、Empty(かつプレイヤー・設置済みボム以外)が
        // 続く間だけを塗る。塞がっているマスに当たったらそこで止める。
        let mut row = spawn_row + 1;
        while row < game.board.depth_rows()
            && game.board.cell(row, col) == BoardCell::Empty
            && (row, col) != player_pos
            && !blocked_by_bomb((row, col))
        {
            if let Some((x, y)) = cell_screen_pos(inner, cam_row_f, visible_rows, row, col) {
                fill_block(buf, x, y, colors::INCOMING_ROCK_PATH_BG);
            }
            row += 1;
        }

        // 出現予定マス(ゴースト岩)。残り時間が減るほど岩の地色へ近づける。
        let progress = 1.0
            - (rock.remaining_ms as f32 / INCOMING_ROCK_WARNING_MS.max(1) as f32).clamp(0.0, 1.0);
        if let Some((x, y)) = cell_screen_pos(inner, cam_row_f, visible_rows, spawn_row, col) {
            fill_block(buf, x, y, colors::incoming_rock_ghost_bg(progress));
        }
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

/// 対戦中、他の参加者の現在位置を自分の盤面へ「ゴースト」として重ねる(#301)。相手の盤面は
/// フル描画しない(spec.md 12.3)ので、代わりに相手の(行, 列)を自分の盤面の同じ座標として
/// 示し、どのあたりを誰が掘っているかが分かるようにする。可視範囲内なら輪郭だけのスプライト、
/// 範囲外(自分より浅い/深い)なら画面の上端・下端に矢印を出す(画面外ボム警告と同じ考え方)。
fn draw_opponent_ghosts(
    buf: &mut Buffer,
    field_rect: Rect,
    visible_rows: usize,
    game_local: &Game,
    other_games: &[Game],
    panel_top_y: u16,
) {
    if other_games.is_empty() || visible_rows == 0 {
        return;
    }
    // `draw_field`が盤面を描くのと同じ、罫線の内側の領域を求める。
    let inner = Block::default().borders(Borders::ALL).inner(field_rect);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let cam_row_f = field_camera(game_local, player_screen_row(visible_rows)).row_f;

    for (index, game) in other_games.iter().enumerate() {
        let (row, col) = game.player.position();
        let fg = colors::battle_ghost_fg(index);
        let marker = ghost_marker_glyph(index);
        match cell_screen_pos(inner, cam_row_f, visible_rows, row, col) {
            Some((x, y)) => draw_ghost_sprite(buf, x, y, marker, fg),
            None => {
                // 自分より浅い位置なら上端、深い位置なら下端に矢印を出す。
                let above = (row as f32) < cam_row_f;
                draw_off_screen_ghost_marker(
                    buf,
                    inner,
                    visible_rows,
                    col,
                    (marker, fg),
                    above,
                    panel_top_y,
                );
            }
        }
    }
}

/// 相手ゴースト(#301)の輪郭。自分のスプライト(`player_sprite`)と混ざらないよう罫線の箱で
/// 囲み、中に参加者番号を入れる。背景色は書き換えず(`put_fg`)、下の盤面が透けて見える
/// 半透明のような見た目にする。
const BATTLE_GHOST_OUTLINE: [[char; CELL_W as usize]; CELL_H as usize] =
    [['┌', '─', '─', '┐'], ['└', ' ', ' ', '┘']];

/// 相手ゴーストのスプライト。`BATTLE_GHOST_OUTLINE`の空白部分に参加者番号を埋める。
fn draw_ghost_sprite(buf: &mut Buffer, x: u16, y: u16, marker: char, fg: Color) {
    for (dy, row) in BATTLE_GHOST_OUTLINE.iter().enumerate() {
        for (dx, &ch) in row.iter().enumerate() {
            let ch = if ch == ' ' { marker } else { ch };
            put_fg(buf, x + dx as u16, y + dy as u16, ch, fg);
        }
    }
}

/// 可視範囲の外にいる相手を、その列の画面上端(`above`)または下端に矢印で示す(#301)。
/// 下向き矢印は相手パネル(#290)より上(`panel_top_y`未満)に収める。パネルは盤面の下端に
/// 重なるように置かれるため、そのままだと矢印がパネルの裏に隠れて見えなくなる
/// (実機で発見)。
fn draw_off_screen_ghost_marker(
    buf: &mut Buffer,
    inner: Rect,
    visible_rows: usize,
    col: usize,
    glyph: (char, Color),
    above: bool,
    panel_top_y: u16,
) {
    let (marker, fg) = glyph;
    let x = inner.x + col as u16 * CELL_W;
    if x + CELL_W > inner.x + inner.width {
        return;
    }
    // 罫線の内側でも可視セルグリッドに収まる範囲にだけ描く(縮退表示では余りが出る)。
    let grid_h = (visible_rows as u16 * CELL_H).min(inner.height);
    if grid_h == 0 {
        return;
    }
    let y = if above {
        inner.y
    } else {
        (inner.y + grid_h - 1).min(panel_top_y.saturating_sub(1))
    };
    let arrow = if above { '\u{2191}' } else { '\u{2193}' };
    for (dx, ch) in [arrow, marker, marker, arrow].into_iter().enumerate() {
        put_fg(buf, x + dx as u16, y, ch, fg);
    }
}

/// 参加者index(自分を除いた0始まり)に対応するゴーストの番号グリフ(#301)。相手パネル(#290)の
/// 見出しと同じ番号を使い、盤面のゴーストとパネルの行を対応づけられるようにする。
/// 対戦人数の上限は4人なので、自分以外は必ず1桁に収まる。
fn ghost_marker_glyph(index: usize) -> char {
    char::from_digit(index as u32 + 1, 10).unwrap_or('?')
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

/// バッファ1マスへ文字・前景色だけを設定し、背景色はそのまま残す(範囲外は無視)。
/// 下に描かれている盤面が透けて見えるため、重ねる印(対戦の相手ゴースト#301)を半透明のように
/// 見せられる。
fn put_fg(buf: &mut Buffer, x: u16, y: u16, ch: char, fg: Color) {
    if let Some(cell) = buf.cell_mut(Position::new(x, y)) {
        cell.set_char(ch).set_fg(fg);
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

/// 盤面の底(ゴールより深い行。到達前から近づくと見える)やチェックポイント安全地帯に
/// 見せる地底の地面。単色でなく岩肌のようなハッチング模様にして「この先は掘り進めない
/// 底がある」ことを見た目でも伝える。
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

/// `board_row`が、まだ掘り抜いていないチェックポイントのギャップより先(次の
/// 100mゾーン)に含まれるかどうか(TERM独自拡張。#197/#281。ユーザー指摘: 「この
/// 地面の上を掘ったら次の100mゾーンに進めるようにしたい。それまで次の100mゾーン
/// はブロック配置しない。進んだら配置する」)。盤面自体はゲーム開始時に事前生成
/// 済みのままだが、描画側でこの判定がtrueの行は中身によらずEmpty扱いにして隠す。
/// チェックポイントの地面・ギャップ帯自体(`is_checkpoint_safe_zone_row`が担当)は
/// 掘る対象として見えている必要があるため対象外(ギャップの先だけを隠す)。
fn is_unrevealed_future_zone(board_row: usize, last_checkpoint_reported: usize) -> bool {
    if board_row < CHECKPOINT_STEP_M {
        return false;
    }
    let checkpoint = board_row / CHECKPOINT_STEP_M;
    if checkpoint <= last_checkpoint_reported {
        return false;
    }
    let gap_end = checkpoint * CHECKPOINT_STEP_M + CHECKPOINT_SAFE_ZONE_M + CHECKPOINT_ZONE_GAP_M;
    board_row >= gap_end
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
    draw_overlay_sized(frame, area, 40, 20, title, hints);
}

/// `draw_overlay`の箱の大きさを呼び出し側から指定できる版。対戦の待機中オーバーレイ(#302)は
/// 通常プレイのGameOverダイアログを完全に覆い隠す必要があり、既定の大きさでは足りない。
fn draw_overlay_sized(
    frame: &mut Frame,
    area: Rect,
    percent_x: u16,
    percent_y: u16,
    title: &str,
    hints: &[&str],
) {
    let overlay_area = centered_rect(percent_x, percent_y, area);
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
    use crate::discovery::DiscoveredPeer;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn every_battle_outcome_has_its_own_result_message() {
        // 対戦の結末は順位(#273)だけになった(#299で盤面の突き合わせによる打ち切りを廃止)。
        assert_eq!(battle_outcome_message(BattleOutcome::Ranked(1)), "RANK 1");
        assert_eq!(battle_outcome_message(BattleOutcome::Ranked(4)), "RANK 4");
    }

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
    fn is_unrevealed_future_zone_hides_only_the_gap_and_beyond_of_a_not_yet_reached_checkpoint() {
        // #197/#281: ユーザー指摘: 「この地面の上を掘ったら次の100mゾーンに進める
        // ようにしたい。それまで次の100mゾーンはブロック配置しない」。まだ到達
        // していない(last_checkpoint_reported未満の)チェックポイントについて、
        // 地面・ギャップ帯自体(掘る対象として見える必要がある)は隠さず、
        // ギャップの先(次の100mゾーン)だけを隠す。
        let gap_end = 100 + CHECKPOINT_SAFE_ZONE_M + CHECKPOINT_ZONE_GAP_M;
        assert!(
            !is_unrevealed_future_zone(100, 0),
            "地面帯自体は隠さないはず"
        );
        assert!(
            !is_unrevealed_future_zone(gap_end - 1, 0),
            "ギャップの最後の行までは隠さないはず"
        );
        assert!(
            is_unrevealed_future_zone(gap_end, 0),
            "ギャップの直後(次の100mゾーン)は隠すはず"
        );
        assert!(
            is_unrevealed_future_zone(199, 0),
            "次の100mゾーンの奥まで隠すはず"
        );
        // 既にそのチェックポイントに到達済み(last_checkpoint_reported>=1)なら隠さない。
        assert!(
            !is_unrevealed_future_zone(gap_end, 1),
            "到達済みチェックポイントの先は隠さないはず"
        );
        // 最初のチェックポイント(100m)より手前は常に表示。
        assert!(!is_unrevealed_future_zone(0, 0));
        assert!(!is_unrevealed_future_zone(99, 0));
    }

    #[test]
    fn rows_below_the_board_bottom_are_drawn_as_bedrock_ground_before_clearing() {
        // #261の回帰防止。盤面外(ゴールより深い行)は、ゲーム状態がPlayingのままでも
        // クリア前から地底の地面ビジュアルで表示されるはず。到達した瞬間に突然
        // 出現するのがバグだった(ゴール直前まで地面が見えず、着地すると急に現れる)。
        let mut game = free_falling_game();
        game.board.rows.truncate(20);
        assert_eq!(
            game.status,
            GameStatus::Playing,
            "テスト前提: クリア前であること"
        );
        let no_moves: HashMap<Pos, Pos> = HashMap::new();
        let area = Rect::new(0, 0, CELL_W, CELL_H);

        // 盤面外(depth_rows=20を超えた行)。
        let mut below_buf = Buffer::empty(area);
        draw_static_cell(
            &mut below_buf,
            0,
            0,
            &game,
            (20, 0),
            BoardCell::Empty,
            &no_moves,
        );
        assert!(
            below_buf
                .content
                .iter()
                .all(|c| c.bg == colors::BEDROCK_GROUND_BG),
            "盤面外の行はクリア前でも地底の地面のはず"
        );

        // 対照: 盤面内・チェックポイント帯でもない普通のEmpty行は素の背景のまま。
        let mut inside_buf = Buffer::empty(area);
        draw_static_cell(
            &mut inside_buf,
            0,
            0,
            &game,
            (19, 0),
            BoardCell::Empty,
            &no_moves,
        );
        assert!(
            inside_buf
                .content
                .iter()
                .all(|c| c.bg == colors::FIELD_EMPTY_BG),
            "盤面内の通常のEmpty行は地底の地面にならないはず"
        );
    }

    #[test]
    fn rows_below_the_board_bottom_stay_bedrock_ground_after_clearing() {
        // #182の回帰防止。クリア後も盤面外の行は同じく地底の地面のままのはず。
        let mut game = free_falling_game();
        game.board.rows.truncate(20);
        game.status = GameStatus::Cleared;
        let no_moves: HashMap<Pos, Pos> = HashMap::new();

        let area = Rect::new(0, 0, CELL_W, CELL_H);
        let mut buf = Buffer::empty(area);
        draw_static_cell(&mut buf, 0, 0, &game, (20, 0), BoardCell::Empty, &no_moves);
        assert!(
            buf.content
                .iter()
                .all(|c| c.bg == colors::BEDROCK_GROUND_BG),
            "クリア後も盤面外の行は地底の地面のままのはず"
        );
    }

    #[test]
    fn bottom_of_a_bonus_floor_depth_course_is_ground_before_clearing() {
        // ユーザー報告の再現(500mコース)。500はチェックポイント安全地帯の判定からは
        // 除外される(ボーナスフロアのため)が、盤面外判定が先に効くのでクリア前でも
        // 地底の地面になるはず。
        assert!(
            !is_checkpoint_safe_zone_row(BONUS_FLOOR_DEPTH_M),
            "テスト前提: 500mはチェックポイント安全地帯の判定からは対象外のはず"
        );
        let mut game = free_falling_game();
        game.board.rows.truncate(BONUS_FLOOR_DEPTH_M);
        assert_eq!(
            game.status,
            GameStatus::Playing,
            "テスト前提: クリア前であること"
        );
        let no_moves: HashMap<Pos, Pos> = HashMap::new();

        let area = Rect::new(0, 0, CELL_W, CELL_H);
        let mut buf = Buffer::empty(area);
        draw_static_cell(
            &mut buf,
            0,
            0,
            &game,
            (BONUS_FLOOR_DEPTH_M, 0),
            BoardCell::Empty,
            &no_moves,
        );
        assert!(
            buf.content
                .iter()
                .all(|c| c.bg == colors::BEDROCK_GROUND_BG),
            "500mゴールの盤面外行はクリア前でも地底の地面のはず"
        );
    }

    #[test]
    fn static_field_shows_the_ground_at_the_bottom_while_the_player_approaches_the_goal() {
        // #261の統合テスト。draw_static_field経由でも、盤面外の行が画面内に入れば
        // クリア前から地底の地面が描かれ、盤面内のEmpty行とは見た目が区別できるはず。
        let mut game = free_falling_game();
        game.board.rows.truncate(20);
        game.player.row = 15;
        game.player.col = 2;

        // top_row=10・visible_rows=10で盤面内の行は board_row 10〜19の10行ぶん。
        // dy=1(半端スクロール量、CELL_H=2の半分)にすると、`inner`の最終端末行
        // (row=inner.height-1)には次に見えてくる盤面外の行(board_row=20)の
        // 上側ピクセル行がせり上がって見える(#242のオフスクリーン転写の仕組み)。
        let visible_rows = 10;
        let cam = FieldCamera {
            row_f: 10.5,
            top_row: 10,
            dy: 1,
        };
        let inner = field_inner_rect(visible_rows);
        let no_moves: HashMap<Pos, Pos> = HashMap::new();
        let mut buf = Buffer::empty(inner);

        draw_static_field(&mut buf, inner, &cam, visible_rows, &game, &no_moves);

        // inner先頭の端末行(board_row=10、盤面内の通常のEmpty行)。
        let inside_y = inner.y;
        // inner最終端末行(board_row=20、盤面外)。
        let below_y = inner.y + inner.height - 1;

        let row_bg = |y: u16| {
            (inner.x..inner.x + inner.width)
                .map(|x| buf.cell(Position::new(x, y)).unwrap().bg)
                .collect::<Vec<_>>()
        };
        assert!(
            row_bg(inside_y)
                .iter()
                .all(|&bg| bg == colors::FIELD_EMPTY_BG),
            "盤面内のEmpty行は素の背景のままのはず"
        );
        assert!(
            row_bg(below_y)
                .iter()
                .all(|&bg| bg == colors::BEDROCK_GROUND_BG),
            "ゴールに近づいている(クリア前)時点で盤面外の行が地底の地面として見えるはず"
        );
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
            SettingsChoice::BombFuse,
            SettingsChoice::AttackBlocksPerRock,
            SettingsChoice::AttackRocksPerWaveMax,
            SettingsChoice::AttackBlocksPerBomb,
            SettingsChoice::AttackBombsPerWaveMax,
            SettingsChoice::AttackBombRatioPercent,
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
        assert!(
            seen.contains(&SettingsChoice::AttackBlocksPerRock),
            "#247で追加した「岩1個に必要な攻撃力」へカーソルが到達できない"
        );
        assert!(
            seen.contains(&SettingsChoice::AttackRocksPerWaveMax),
            "#247で追加した「一度に降る岩の上限」へカーソルが到達できない"
        );
        assert!(
            seen.contains(&SettingsChoice::AttackBlocksPerBomb),
            "#304で追加した「ボム1個に必要な攻撃力」へカーソルが到達できない"
        );
        assert!(
            seen.contains(&SettingsChoice::AttackBombsPerWaveMax),
            "#304で追加した「一度に降るボムの上限」へカーソルが到達できない"
        );
        assert!(
            seen.contains(&SettingsChoice::AttackBombRatioPercent),
            "#304で追加した「攻撃力のボム化比率」へカーソルが到達できない"
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
        // 一時停止2+空行1+デバッグ見出し1+デバッグ8(#247でOキーを追加)+空行1+
        // ジュークボックス見出し1+曲4+空行1+末尾1=28行。
        const REQUIRED_CONTENT_LINES: u16 = 28;
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
        // クリップして見えなくなる)。見出し1+設定項目29(#224でMUSIC音量・SE音量の2項目、
        // #233で巻き戻しストック上限、#243で揺れ時間(落下待ち)、#246でボム爆発までの
        // 時間、#247で対戦の2項目、#304で対戦のボム3項目を追加)+案内2行=32行、
        // 枠(上下)2行込みで34行必要。設定を追加したらこの定数も増やすこと(#247で項目を
        // 2つ増やした際、見出し前後の空行2行を削って収めている)。
        const REQUIRED_CONTENT_LINES: u16 = 32;
        let area = Rect::new(0, 0, 200, 60);
        let frame_rect = centered_fixed_rect(TOTAL_SCREEN_W, TOTAL_SCREEN_H, area);
        let settings_area = settings_box_rect(area, frame_rect);
        assert!(
            settings_area.height >= REQUIRED_CONTENT_LINES + 2,
            "設定画面の枠が{}行分の内容を収めるには狭すぎる(高さ={})",
            REQUIRED_CONTENT_LINES,
            settings_area.height
        );
    }

    /// TestBackendへ実際に描画し、画面に見えている文字を行ごとに連結して返す。
    /// 行数の算術チェックでは拾えない「枠からはみ出して見えない」を確認する用途。
    fn rendered_screen_text(draw: impl FnOnce(&mut Frame)) -> String {
        let backend = ratatui::backend::TestBackend::new(200, 60);
        let mut terminal = ratatui::Terminal::new(backend).expect("TestBackendを初期化できるはず");
        terminal.draw(draw).expect("描画できるはず");
        let buf = terminal.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| {
                        buf.cell(Position::new(x, y))
                            .map(|c| c.symbol())
                            .unwrap_or(" ")
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// `rendered_screen_text`の結果に`needle`が出ているか。全角文字は1セル目に本体・
    /// 2セル目に詰め物が入るため、空白を落としてから突き合わせる。
    fn screen_shows(text: &str, needle: &str) -> bool {
        text.replace(' ', "").contains(&needle.replace(' ', ""))
    }

    /// テスト用に`draw_settings`を既定値で描画する(選択項目だけを変える)。
    fn render_settings_screen(selection: SettingsChoice) -> String {
        rendered_screen_text(|frame| {
            draw_settings(
                frame,
                selection,
                true,
                true,
                100,
                100,
                100,
                100,
                100,
                100,
                100,
                100,
                100,
                4,
                100,
                FIELD_WIDTH,
                150,
                150,
                450,
                80,
                800,
                100,
                5000,
                10,
                4,
                20,
                2,
                20,
                true,
                0,
                3,
                false,
            );
        })
    }

    #[test]
    fn settings_screen_actually_shows_the_attack_rule_rows() {
        // #247で追加した2項目が、値つきで画面に出ている(枠からクリップされていない)ことを
        // 実描画で確認する。
        let text = render_settings_screen(SettingsChoice::AttackBlocksPerRock);
        assert!(
            screen_shows(&text, "対戦: 岩1個に必要な攻撃力: 10"),
            "「岩1個に必要な攻撃力」の行が画面に出ていない:\n{text}"
        );
        assert!(
            screen_shows(&text, "対戦: 一度に降る岩の上限: 4"),
            "「一度に降る岩の上限」の行が画面に出ていない:\n{text}"
        );
        assert!(
            screen_shows(&text, "> 対戦: 岩1個に必要な攻撃力"),
            "選択中の項目にカーソル(>)が付いていない:\n{text}"
        );
    }

    #[test]
    fn settings_screen_actually_shows_the_bomb_attack_rule_rows() {
        // #304で追加した3項目が、値つきで画面に出ている(枠からクリップされていない)ことを
        // 実描画で確認する。末尾に近い項目のため、枠の高さ不足はここで表に出る。
        let text = render_settings_screen(SettingsChoice::AttackBombRatioPercent);
        assert!(
            screen_shows(&text, "対戦: ボム1個に必要な攻撃力: 20"),
            "「ボム1個に必要な攻撃力」の行が画面に出ていない:\n{text}"
        );
        assert!(
            screen_shows(&text, "対戦: 一度に降るボムの上限: 2"),
            "「一度に降るボムの上限」の行が画面に出ていない:\n{text}"
        );
        assert!(
            screen_shows(&text, "> 対戦: 攻撃力のボム化比率: 20%"),
            "選択中の「攻撃力のボム化比率」の行が画面に出ていない:\n{text}"
        );
    }

    // --- 対戦ロビー画面(#256) ---

    /// ロビー画面を実描画して、画面に見えている文字を返す。
    fn render_network_lobby(lobby: &LobbyState) -> String {
        rendered_screen_text(|frame| draw_network_lobby(frame, lobby))
    }

    #[test]
    fn the_lobby_shows_the_candidates_and_the_key_hints_while_discovering() {
        // 候補リストの行と操作案内が枠に収まって見えることを実描画で確認する。
        let mut lobby = LobbyState::new_on_loopback("me".to_string())
            .expect("ループバックのソケットは確保できるはず");
        let text = render_network_lobby(&lobby);
        assert!(
            screen_shows(&text, "さがしています"),
            "候補が0件のときの案内が出ていない:\n{text}"
        );

        lobby.add_peer(DiscoveredPeer::for_test(
            "Player-1a2b",
            IpAddr::from(Ipv4Addr::new(192, 168, 0, 2)),
            39394,
        ));
        let text = render_network_lobby(&lobby);
        assert!(
            screen_shows(&text, "> Player-1a2b (192.168.0.2)"),
            "候補の行とカーソルが出ていない:\n{text}"
        );
        assert!(
            screen_shows(&text, "Enter: 対戦を申し込む"),
            "操作案内がクリップされている:\n{text}"
        );
        assert!(
            screen_shows(&text, "自分: me"),
            "自分の表示名が出ていない:\n{text}"
        );
    }

    #[test]
    fn the_lobby_shows_the_room_members_and_how_to_start_once_a_guest_joined() {
        // #276: ゲストを迎え入れたら参加者一覧と開始操作(Tab)が出る。
        let mut lobby = LobbyState::new_on_loopback("me".to_string())
            .expect("ループバックのソケットは確保できるはず");
        lobby.add_peer(DiscoveredPeer::for_test(
            "Player-1a2b",
            IpAddr::from(Ipv4Addr::new(192, 168, 0, 2)),
            39394,
        ));
        lobby.set_phase(LobbyPhase::Discovering {
            guests: vec![
                crate::lobby::HostedGuest::for_test("Player-3c4d"),
                crate::lobby::HostedGuest::for_test("Player-5e6f"),
            ],
        });

        let text = render_network_lobby(&lobby);

        assert!(
            screen_shows(&text, "== ルームの参加者 =="),
            "参加者一覧の見出しが出ていない:\n{text}"
        );
        assert!(
            screen_shows(&text, "me (自分)"),
            "参加者一覧に自分が出ていない:\n{text}"
        );
        assert!(
            screen_shows(&text, "Player-3c4d") && screen_shows(&text, "Player-5e6f"),
            "迎え入れたゲストが出ていない:\n{text}"
        );
        assert!(
            screen_shows(&text, "Tab: この3人で対戦をはじめる"),
            "開始操作の案内が出ていない:\n{text}"
        );
    }

    #[test]
    fn the_lobby_offers_an_ai_battle_and_shows_how_many_opponents_are_selected() {
        // #296: 相手が見つからなくてもAIと対戦できる入口(V)と、人数選択の表示。
        let mut lobby = LobbyState::new_on_loopback("me".to_string())
            .expect("ループバックのソケットは確保できるはず");

        let text = render_network_lobby(&lobby);
        assert!(
            screen_shows(&text, "V: AIと対戦"),
            "AI対戦の入口の案内が出ていない:\n{text}"
        );

        lobby.set_phase(LobbyPhase::SelectingAiOpponentCount { ai_count: 2 });
        let text = render_network_lobby(&lobby);
        assert!(
            screen_shows(&text, "== AIと対戦 =="),
            "人数選択の見出しが出ていない:\n{text}"
        );
        assert!(
            screen_shows(&text, "AIの人数: 2 (合計3人)"),
            "選んでいる人数と合計人数が出ていない:\n{text}"
        );
        assert!(
            screen_shows(&text, "Enter: 開始"),
            "操作案内がクリップされている:\n{text}"
        );
    }

    #[test]
    fn the_lobby_tells_a_guest_that_it_is_waiting_for_the_host_to_start() {
        // #276: ゲスト側はホストの開始操作を待つ間、何を待っているか分かるようにする。
        // #293: ゲスト自身も開始できるため、その案内も出す。
        let mut lobby = LobbyState::new_on_loopback("me".to_string())
            .expect("ループバックのソケットは確保できるはず");
        let (_result_tx, result_rx) = std::sync::mpsc::channel();
        lobby.set_phase(LobbyPhase::WaitingForRoomStart {
            result_rx,
            host_peer: DiscoveredPeer::for_test(
                "Player-1a2b",
                IpAddr::from(Ipv4Addr::LOCALHOST),
                39394,
            ),
        });

        let text = render_network_lobby(&lobby);

        assert!(
            screen_shows(&text, "ルームに参加しました。開始を待っています..."),
            "開始待ちの文面が出ていない:\n{text}"
        );
        assert!(
            screen_shows(&text, "Tab: 自分から対戦をはじめる"),
            "ゲスト側の開始操作の案内が出ていない:\n{text}"
        );
    }

    #[test]
    fn the_lobby_shows_what_to_press_for_an_incoming_invite() {
        let mut lobby = LobbyState::new_on_loopback("me".to_string())
            .expect("ループバックのソケットは確保できるはず");
        lobby.set_phase(LobbyPhase::IncomingInvite {
            from: DiscoveredPeer::for_test("Player-1a2b", IpAddr::from(Ipv4Addr::LOCALHOST), 39394),
            pending: Vec::new(),
            guests: Vec::new(),
        });

        let text = render_network_lobby(&lobby);

        assert!(
            screen_shows(&text, "「Player-1a2b」から対戦を申し込まれました"),
            "招待の文面が出ていない:\n{text}"
        );
        assert!(
            screen_shows(&text, "Enter: 受ける / Esc: ことわる"),
            "承諾/拒否の案内が出ていない:\n{text}"
        );
    }

    #[test]
    fn the_lobby_shows_the_notice_message_as_it_is() {
        let mut lobby = LobbyState::new_on_loopback("me".to_string())
            .expect("ループバックのソケットは確保できるはず");
        lobby.set_phase(LobbyPhase::Notice {
            message: "相手に断られました".to_string(),
            shown_at: std::time::Instant::now(),
            guests: Vec::new(),
            pending: Vec::new(),
        });

        assert!(screen_shows(
            &render_network_lobby(&lobby),
            "相手に断られました"
        ));
    }

    // --- 表示名の入力画面(#270) ---

    /// 表示名入力画面を実描画して、画面に見えている文字を返す。
    fn render_player_name_input(state: &TextEditState) -> String {
        rendered_screen_text(|frame| draw_player_name_input(frame, state))
    }

    #[test]
    fn the_name_input_shows_the_edited_text_and_the_key_hints() {
        // 編集中の内容と操作案内が枠に収まって見えることを実描画で確認する。
        let state = TextEditState::new("Player-1a2b");
        let text = render_player_name_input(&state);

        assert!(
            screen_shows(&text, "== 名前を入力 =="),
            "見出しが出ていない:\n{text}"
        );
        assert!(
            screen_shows(&text, "Player-1a2b"),
            "編集中の名前が出ていない:\n{text}"
        );
        assert!(
            screen_shows(&text, "Enter: 決定 / Esc: タイトルへ"),
            "決定/取り消しの案内がクリップされている:\n{text}"
        );
        assert!(
            screen_shows(&text, "Ctrl+A: 全選択"),
            "全選択の案内がクリップされている:\n{text}"
        );
    }

    #[test]
    fn the_name_input_shows_multibyte_text_as_it_is() {
        // 日本語の名前でも、入力した文字がそのまま出る。
        let state = TextEditState::new("よっち");
        assert!(screen_shows(&render_player_name_input(&state), "よっち"));
    }

    #[test]
    fn the_name_input_marks_the_cursor_position() {
        // カーソル位置は反転表示にする。末尾にある場合は空白1つぶんを反転させる。
        let base = Style::default();
        let mut state = TextEditState::new("ab");
        let spans = player_name_input_spans(&state, base);
        assert_eq!(spans.len(), 3, "文字2つ+末尾カーソルの空白1つ");
        assert_eq!(spans[2].content, " ");
        assert!(
            spans[2].style.add_modifier.contains(Modifier::REVERSED),
            "末尾のカーソルが反転表示になっていない"
        );
        assert!(!spans[0].style.add_modifier.contains(Modifier::REVERSED));

        // 文字の上にある場合はその文字を反転し、末尾の空白は足さない。
        state.move_left(false);
        let spans = player_name_input_spans(&state, base);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[1].content, "b");
        assert!(
            spans[1].style.add_modifier.contains(Modifier::REVERSED),
            "カーソル位置の文字が反転表示になっていない"
        );
    }

    #[test]
    fn the_name_input_highlights_the_selected_range() {
        // 選択範囲は背景色で塗る(選択の外の文字は塗らない)。
        let base = Style::default();
        let mut state = TextEditState::new("abc");
        state.move_to_start(false);
        state.move_right(true);
        state.move_right(true);
        assert_eq!(state.selection_range(), Some((0, 2)));

        let spans = player_name_input_spans(&state, base);
        assert_eq!(spans.len(), 3, "選択中は末尾カーソルの空白を足さない");
        assert_eq!(spans[0].style.bg, Some(Color::DarkGray));
        assert_eq!(spans[1].style.bg, Some(Color::DarkGray));
        assert_eq!(spans[2].style.bg, None, "選択の外は塗らない");
    }

    #[test]
    fn the_name_input_shows_a_cursor_even_when_the_text_is_empty() {
        // 全消しした状態でもカーソルが見えるようにする。
        let spans = player_name_input_spans(&TextEditState::new(""), Style::default());
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, " ");
        assert!(spans[0].style.add_modifier.contains(Modifier::REVERSED));
    }

    /// 対戦画面テスト用に、相手1人ぶんのゲームと名前を用意する。
    fn single_opponent(name: &str) -> (Vec<Game>, Vec<String>) {
        (
            vec![Game::new_with_width(1, FIELD_WIDTH, 100)],
            vec![name.to_string()],
        )
    }

    #[test]
    fn the_battle_screen_shows_the_result_overlay_once_the_outcome_is_decided() {
        // 決着後は結果と抜け方が盤面の上に重なって見える(#256)。順位は1位から最下位まで
        // どれでも同じように出す(対戦人数の上限は4人=ROOM_MAX_PLAYERS)。
        let game = Game::new_with_width(1, FIELD_WIDTH, 100);
        let (others, names) = single_opponent("opponent");
        for rank in 1..=4u8 {
            let text = rendered_screen_text(|frame| {
                draw_battle(
                    frame,
                    &game,
                    &others,
                    &names,
                    true,
                    true,
                    Some(BattleOutcome::Ranked(rank)),
                )
            });

            assert!(
                screen_shows(&text, &format!("RANK {rank}")),
                "{rank}位の結果が出ていない:\n{text}"
            );
            assert!(
                screen_shows(&text, "Enter/Escキーでタイトルへ"),
                "{rank}位: 抜け方の案内が出ていない:\n{text}"
            );
            assert!(
                screen_shows(&text, "opponent"),
                "{rank}位: 相手パネルは結果表示中も残るはず:\n{text}"
            );
        }
    }

    #[test]
    fn the_battle_screen_shows_no_result_overlay_before_the_outcome() {
        let game = Game::new_with_width(1, FIELD_WIDTH, 100);
        let (others, names) = single_opponent("opponent");
        let text = rendered_screen_text(|frame| {
            draw_battle(frame, &game, &others, &names, true, true, None)
        });

        assert!(
            !screen_shows(&text, "Enter/Escキーでタイトルへ"),
            "決着前に結果オーバーレイが出てしまっている:\n{text}"
        );
    }

    // --- N人対戦の相手パネル(#290) ---

    #[test]
    fn the_battle_opponent_panel_lists_every_other_player() {
        // N人対戦では自分以外の全員(上限4人=ROOM_MAX_PLAYERSなので最大3人)の名前・深度・
        // ライフがパネルに並ぶ(#290)。以前は先頭の1人しか出ていなかった。
        let game = Game::new_with_width(1, FIELD_WIDTH, 100);
        let names: Vec<String> = ["alpha", "bravo", "charlie"]
            .iter()
            .map(|n| n.to_string())
            .collect();
        let mut others = Vec::new();
        for (index, depth_row) in [9usize, 24, 49].into_iter().enumerate() {
            let mut other = Game::new_with_width(2, FIELD_WIDTH, 100);
            other.player.row = depth_row;
            other.player.lives = index as u8 + 1;
            others.push(other);
        }

        let text = rendered_screen_text(|frame| {
            draw_battle(frame, &game, &others, &names, true, true, None)
        });

        for (index, name) in names.iter().enumerate() {
            assert!(
                screen_shows(&text, name),
                "{name}の名前がパネルに出ていない:\n{text}"
            );
            let depth_m = others[index].player.depth_m();
            assert!(
                screen_shows(&text, &format!("{depth_m}m")),
                "{name}の深度({depth_m}m)がパネルに出ていない:\n{text}"
            );
            assert!(
                screen_shows(&text, &format!("\u{2665}\u{d7}{}", others[index].player.lives)),
                "{name}のライフがパネルに出ていない:\n{text}"
            );
        }
    }

    #[test]
    fn the_battle_opponent_panel_grows_with_the_number_of_players() {
        // パネルの高さは人数ぶん(1人1行、#305)+上下ボーダー2行。
        assert_eq!(battle_opponent_panel_h(1), 3);
        assert_eq!(battle_opponent_panel_h(2), 4);
        assert_eq!(battle_opponent_panel_h(3), 5);
    }

    #[test]
    fn the_battle_screen_draws_no_opponent_panel_without_other_players() {
        // 相手がいない(全員抜けた等)場合はパネルを出さない。
        let game = Game::new_with_width(1, FIELD_WIDTH, 100);
        let text =
            rendered_screen_text(|frame| draw_battle(frame, &game, &[], &[], true, true, None));
        assert!(
            !screen_shows(&text, "OPPONENT:"),
            "相手がいないのにパネルが出ている:\n{text}"
        );
    }

    // --- 相手の位置のゴースト表示(#301) ---

    /// ゴーストのテスト用に、プレイヤーを指定の行・列に置いたゲームを作る。
    /// 生成直後のGameは移動アニメーションが完了済みなので、`interp_player_row`は
    /// この行そのものになる。
    fn game_at(row: usize, col: usize) -> Game {
        let mut game = Game::new(7);
        game.player.row = row;
        game.player.col = col;
        game
    }

    #[test]
    fn opponent_ghosts_are_drawn_on_the_own_field_when_they_are_in_view() {
        // 可視範囲にいる相手は、自分の盤面の同じ座標へゴーストとして重なる(#301)。
        let visible_rows = 10;
        let local = game_at(100, 0);
        let cam_row_f = field_camera(&local, player_screen_row(visible_rows)).row_f;
        // 自分の行はplayer_screen_row分だけ画面上端から下がるため、カメラより深い行にいる。
        let others = vec![game_at(100 + 1, 3), game_at(100 + 2, 5)];

        let field_rect = Rect::new(0, 0, 60, 30);
        let mut buf = Buffer::empty(field_rect);
        draw_opponent_ghosts(
            &mut buf,
            field_rect,
            visible_rows,
            &local,
            &others,
            // 相手パネルを置かない場合と同じく、盤面の下端まで矢印を出せる位置にする。
            field_rect.bottom(),
        );

        let inner = Block::default().borders(Borders::ALL).inner(field_rect);
        for (index, other) in others.iter().enumerate() {
            let (row, col) = other.player.position();
            let (x, y) = cell_screen_pos(inner, cam_row_f, visible_rows, row, col)
                .expect("可視範囲内のはず");
            let cell = buf.cell(Position::new(x, y)).expect("盤面内のはず");
            assert_eq!(cell.symbol(), "\u{250c}", "ゴーストの輪郭が描かれていない");
            assert_eq!(
                cell.fg,
                colors::battle_ghost_fg(index),
                "参加者ごとに色を変えるはず"
            );
            // 番号は輪郭の内側(2行目)に入る。
            let number = buf
                .cell(Position::new(x + 1, y + 1))
                .expect("盤面内のはず")
                .symbol()
                .to_string();
            assert_eq!(
                number,
                ghost_marker_glyph(index).to_string(),
                "ゴーストに参加者番号が入っていない"
            );
        }
    }

    #[test]
    fn opponent_ghosts_keep_the_background_of_the_cell_below_them() {
        // ゴーストは前景色だけを書き換え、下の盤面の背景色を残す(半透明のような見た目)。
        let visible_rows = 10;
        let local = game_at(100, 0);
        let others = vec![game_at(101, 3)];
        let field_rect = Rect::new(0, 0, 60, 30);

        let mut buf = Buffer::empty(field_rect);
        let inner = Block::default().borders(Borders::ALL).inner(field_rect);
        let cam_row_f = field_camera(&local, player_screen_row(visible_rows)).row_f;
        let (x, y) =
            cell_screen_pos(inner, cam_row_f, visible_rows, 101, 3).expect("可視範囲内のはず");
        fill_block(&mut buf, x, y, colors::ROCK_BG_INTACT);

        draw_opponent_ghosts(
            &mut buf,
            field_rect,
            visible_rows,
            &local,
            &others,
            // 相手パネルを置かない場合と同じく、盤面の下端まで矢印を出せる位置にする。
            field_rect.bottom(),
        );

        assert_eq!(
            buf.cell(Position::new(x, y)).unwrap().bg,
            colors::ROCK_BG_INTACT,
            "ゴーストが下のマスの背景色を塗り潰している"
        );
    }

    #[test]
    fn opponents_out_of_view_are_shown_as_arrows_at_the_screen_edges() {
        // 可視範囲の外にいる相手は、その列の上端(浅い)・下端(深い)に矢印で示す(#301)。
        let visible_rows = 10;
        let local = game_at(100, 0);
        let shallow_col = 2;
        let deep_col = 4;
        let others = vec![game_at(10, shallow_col), game_at(400, deep_col)];

        let field_rect = Rect::new(0, 0, 60, 30);
        let mut buf = Buffer::empty(field_rect);
        draw_opponent_ghosts(
            &mut buf,
            field_rect,
            visible_rows,
            &local,
            &others,
            // 相手パネルを置かない場合と同じく、盤面の下端まで矢印を出せる位置にする。
            field_rect.bottom(),
        );

        let inner = Block::default().borders(Borders::ALL).inner(field_rect);
        let symbol_at = |buf: &Buffer, x: u16, y: u16| {
            buf.cell(Position::new(x, y))
                .expect("盤面内のはず")
                .symbol()
                .to_string()
        };

        let shallow_x = inner.x + shallow_col as u16 * CELL_W;
        assert_eq!(
            symbol_at(&buf, shallow_x, inner.y),
            "\u{2191}",
            "自分より浅い相手は上端に上向き矢印で出るはず"
        );
        assert_eq!(
            symbol_at(&buf, shallow_x + 1, inner.y),
            ghost_marker_glyph(0).to_string(),
            "矢印に参加者番号が添えられていない"
        );

        let deep_x = inner.x + deep_col as u16 * CELL_W;
        let bottom_y = inner.y + (visible_rows as u16 * CELL_H).min(inner.height) - 1;
        assert_eq!(
            symbol_at(&buf, deep_x, bottom_y),
            "\u{2193}",
            "自分より深い相手は下端に下向き矢印で出るはず"
        );
        assert_eq!(
            symbol_at(&buf, deep_x + 1, bottom_y),
            ghost_marker_glyph(1).to_string(),
            "矢印に参加者番号が添えられていない"
        );
    }

    #[test]
    fn ghost_marker_glyphs_are_numbered_from_one() {
        // パネルの見出し(#290)と盤面のゴースト(#301)で同じ番号を使う。
        assert_eq!(ghost_marker_glyph(0), '1');
        assert_eq!(ghost_marker_glyph(1), '2');
        assert_eq!(ghost_marker_glyph(2), '3');
    }

    // --- 自分がGameOverになった後の待機表示(#302) ---

    #[test]
    fn the_battle_screen_shows_a_waiting_notice_instead_of_the_game_over_dialog() {
        // 自分が力尽きても対戦は全員の結果がそろうまで終わらない。`tick_battle`は
        // GameOverダイアログの選択操作を受け付けないため、押しても何も起きない
        // ダイアログではなく待機中の案内を出す(#302)。
        let mut local = Game::new_with_width(1, FIELD_WIDTH, 100);
        local.status = GameStatus::GameOver;
        let (others, names) = single_opponent("opponent");

        let text = rendered_screen_text(|frame| {
            draw_battle(frame, &local, &others, &names, true, true, None)
        });

        assert!(
            screen_shows(&text, "対戦終了までお待ちください"),
            "待機中の案内が出ていない:\n{text}"
        );
        assert!(
            screen_shows(&text, "決着まで操作できません"),
            "操作できない旨の案内が出ていない:\n{text}"
        );
        assert!(
            !screen_shows(&text, "タイトルへ戻る"),
            "押しても効かないGameOverダイアログの選択肢が残っている:\n{text}"
        );
        assert!(
            !screen_shows(&text, "その場から復活"),
            "押しても効かないGameOverダイアログの選択肢が残っている:\n{text}"
        );
        assert!(
            !screen_shows(&text, "↑↓で選択 / Enterで決定"),
            "押しても効かないGameOverダイアログの操作案内が残っている:\n{text}"
        );
    }

    #[test]
    fn the_battle_waiting_notice_gives_way_to_the_result_once_the_outcome_is_decided() {
        // 決着したら待機表示は引っ込め、結果と抜け方を出す(#256の挙動は変えない)。
        let mut local = Game::new_with_width(1, FIELD_WIDTH, 100);
        local.status = GameStatus::GameOver;
        let (others, names) = single_opponent("opponent");

        let text = rendered_screen_text(|frame| {
            draw_battle(
                frame,
                &local,
                &others,
                &names,
                true,
                true,
                Some(BattleOutcome::Ranked(2)),
            )
        });

        assert!(
            screen_shows(&text, "Enter/Escキーでタイトルへ"),
            "決着後は抜け方の案内が出るはず:\n{text}"
        );
        assert!(
            !screen_shows(&text, "対戦終了までお待ちください"),
            "決着後に待機中の案内が残っている:\n{text}"
        );
    }

    #[test]
    fn the_battle_waiting_notice_is_not_shown_while_still_playing() {
        // プレイ中は当然出さない。
        let local = Game::new_with_width(1, FIELD_WIDTH, 100);
        assert_eq!(local.status, GameStatus::Playing);
        let (others, names) = single_opponent("opponent");

        let text = rendered_screen_text(|frame| {
            draw_battle(frame, &local, &others, &names, true, true, None)
        });
        assert!(
            !screen_shows(&text, "対戦終了までお待ちください"),
            "プレイ中に待機中の案内が出ている:\n{text}"
        );
    }

    #[test]
    fn battle_waiting_state_needs_both_game_over_and_an_undecided_outcome() {
        let playing = Game::new_with_width(1, FIELD_WIDTH, 100);
        let mut over = Game::new_with_width(1, FIELD_WIDTH, 100);
        over.status = GameStatus::GameOver;

        assert!(battle_local_is_waiting_for_others(&over, None));
        assert!(
            !battle_local_is_waiting_for_others(&over, Some(BattleOutcome::Ranked(1))),
            "決着後は待機ではなく結果表示"
        );
        assert!(
            !battle_local_is_waiting_for_others(&playing, None),
            "プレイ中は待機ではない"
        );
    }

    #[test]
    fn the_battle_waiting_overlay_fully_covers_the_game_over_dialog() {
        // 待機中の案内はGameOverダイアログを覆い隠して消す方式なので、ダイアログの箱が
        // はみ出さないことを確認する(はみ出すと効かない選択肢が見えたままになる)。
        let area = Rect::new(0, 0, 200, 60);
        let game_frame = centered_fixed_rect(TOTAL_SCREEN_W, TOTAL_SCREEN_H, area);
        let waiting = centered_rect(
            BATTLE_WAITING_OVERLAY_PERCENT_X,
            game_over_overlay_percent_y(true),
            game_frame,
        );
        for with_rewind_hint in [false, true] {
            let dialog = centered_rect(
                40,
                game_over_overlay_percent_y(with_rewind_hint),
                game_frame,
            );
            assert!(
                waiting.x <= dialog.x
                    && waiting.y <= dialog.y
                    && waiting.x + waiting.width >= dialog.x + dialog.width
                    && waiting.y + waiting.height >= dialog.y + dialog.height,
                "待機中オーバーレイ({waiting:?})がGameOverダイアログ({dialog:?})を覆いきれていない"
            );
        }
    }

    #[test]
    fn settings_screen_still_shows_its_last_line_after_the_attack_rows_were_added() {
        // #247で項目を2つ増やした結果、枠の高さに対して内容行がぴったりになった。
        // 最下段(操作案内の2行目)が切れていないことを実描画で確認する。
        // #304でさらに3項目増え、枠の高さは`SETTINGS_BOX_H`の固定行数指定へ移した。
        let text = render_settings_screen(SettingsChoice::Music);
        assert!(
            screen_shows(&text, "Escで閉じる"),
            "最下段の案内行がクリップされている:\n{text}"
        );
        assert!(
            screen_shows(&text, "SETTINGS"),
            "先頭の見出しがクリップされている:\n{text}"
        );
    }

    // --- 相手の攻撃で降ってくる岩の予告(#247) ---

    /// 予告テスト用に、指定列の`spawn_row`から`empty_rows`行ぶんを空にしたゲームを作る。
    fn game_with_incoming_rock(col: usize, spawn_row: usize, empty_rows: usize) -> Game {
        let mut game = Game::new(7);
        game.player.row = 500;
        game.player.col = 0;
        for r in spawn_row..(spawn_row + empty_rows) {
            game.board.rows[r][col] = BoardCell::Empty;
        }
        game.board.rows[spawn_row + empty_rows][col] = BoardCell::Rock { hits: 0 };
        game.incoming_rocks_mut().push(crate::game::IncomingRock {
            pos: (spawn_row, col),
            remaining_ms: INCOMING_ROCK_WARNING_MS,
        });
        game
    }

    #[test]
    fn incoming_rock_warning_paints_the_ghost_cell_and_the_empty_fall_path_below_it() {
        // 出現予定マスはゴースト岩、その下の連続するEmptyは落下経路として塗る。
        let col = 3;
        let spawn_row = 495;
        let game = game_with_incoming_rock(col, spawn_row, 3);

        let inner = Rect::new(0, 0, 20, 20);
        let mut buf = Buffer::empty(inner);
        draw_incoming_rock_warnings(&mut buf, inner, spawn_row as f32, 10, &game);

        let bg_at = |buf: &Buffer, row: usize| {
            let (x, y) = cell_screen_pos(inner, spawn_row as f32, 10, row, col)
                .expect("画面内に収まっているはず");
            buf.cell(Position::new(x, y)).unwrap().bg
        };
        assert_eq!(
            bg_at(&buf, spawn_row),
            colors::incoming_rock_ghost_bg(0.0),
            "出現予定マスは予告開始直後のゴースト色になるはず"
        );
        for row in (spawn_row + 1)..(spawn_row + 3) {
            assert_eq!(
                bg_at(&buf, row),
                colors::INCOMING_ROCK_PATH_BG,
                "{row}行目の落下経路が塗られていない"
            );
        }
        assert_ne!(
            bg_at(&buf, spawn_row + 3),
            colors::INCOMING_ROCK_PATH_BG,
            "経路はEmptyでないマスに当たったところで止まるはず"
        );
    }

    #[test]
    fn incoming_rock_ghost_cell_approaches_the_rock_color_as_the_warning_runs_out() {
        // 予告は点滅させず、残り時間に応じて岩の地色へ寄せるだけにする。
        let col = 3;
        let spawn_row = 495;
        let mut game = game_with_incoming_rock(col, spawn_row, 3);
        let inner = Rect::new(0, 0, 20, 20);
        let (x, y) =
            cell_screen_pos(inner, spawn_row as f32, 10, spawn_row, col).expect("画面内のはず");

        let mut buf_start = Buffer::empty(inner);
        draw_incoming_rock_warnings(&mut buf_start, inner, spawn_row as f32, 10, &game);
        let at_start = buf_start.cell(Position::new(x, y)).unwrap().bg;

        game.incoming_rocks_mut()[0].remaining_ms = 0;
        let mut buf_end = Buffer::empty(inner);
        draw_incoming_rock_warnings(&mut buf_end, inner, spawn_row as f32, 10, &game);
        let at_end = buf_end.cell(Position::new(x, y)).unwrap().bg;

        assert_ne!(at_start, at_end, "予告の進行に応じて色が変わるはず");
        assert_eq!(
            at_end,
            colors::ROCK_BG_INTACT,
            "出現直前は岩の地色と同じになるはず"
        );
    }

    #[test]
    fn incoming_rock_warning_path_stops_at_the_player_cell() {
        // プレイヤーが立っているマスは落下経路として塗らない(自分の姿が隠れないように)。
        let col = 3;
        let spawn_row = 495;
        let mut game = game_with_incoming_rock(col, spawn_row, 3);
        game.player.col = col;
        game.player.row = spawn_row + 1;

        let inner = Rect::new(0, 0, 20, 20);
        let mut buf = Buffer::empty(inner);
        draw_incoming_rock_warnings(&mut buf, inner, spawn_row as f32, 10, &game);

        let (x, y) =
            cell_screen_pos(inner, spawn_row as f32, 10, spawn_row + 1, col).expect("画面内のはず");
        assert_ne!(
            buf.cell(Position::new(x, y)).unwrap().bg,
            colors::INCOMING_ROCK_PATH_BG,
            "プレイヤーのマスで経路が止まるはず"
        );
    }

    #[test]
    fn no_incoming_rock_warning_is_drawn_without_any_incoming_rock() {
        // 予告が1つも無ければ何も描かない(通常プレイの見た目を変えない)。
        let mut game = Game::new(7);
        game.player.row = 500;
        assert!(game.incoming_rocks().is_empty(), "前提: 予告が無いこと");

        let inner = Rect::new(0, 0, 20, 20);
        let mut buf = Buffer::empty(inner);
        draw_incoming_rock_warnings(&mut buf, inner, 495.0, 10, &game);

        assert!(
            !buf.content
                .iter()
                .any(|c| c.bg == colors::INCOMING_ROCK_PATH_BG
                    || c.bg == colors::incoming_rock_ghost_bg(0.0)),
            "予告が無いのに塗られているセルがある"
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

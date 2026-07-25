//! crosstermキー入力処理(spec.md 1章・9章)。
//!
//! `event::poll`+`event::read`でノンブロッキングに取得し、ゲームが扱う
//! `InputAction`へ変換する。

use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};

use crate::game::physics::Direction;

/// 1回の入力から得られるゲーム側のアクション。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    Move(Direction),
    TogglePause,
    Quit,
}

/// `poll_ms`だけ待って入力の有無を確認し、対象キーであれば`InputAction`に変換する。
/// 対象外のキー・イベントの場合はNoneを返す(呼び出し側は何もしなければよい)。
pub fn poll_input(poll_ms: u64) -> std::io::Result<Option<InputAction>> {
    if !event::poll(Duration::from_millis(poll_ms))? {
        return Ok(None);
    }

    let Event::Key(key) = event::read()? else {
        return Ok(None);
    };

    // 一部端末/OSはキー入力のPress/Releaseの両方をイベントとして送ってくる。
    // Releaseまで拾うと1回の押下で2回入力処理してしまうため、Pressのみ受理する。
    if key.kind != KeyEventKind::Press {
        return Ok(None);
    }

    let action = match key.code {
        KeyCode::Left => InputAction::Move(Direction::Left),
        KeyCode::Right => InputAction::Move(Direction::Right),
        KeyCode::Down => InputAction::Move(Direction::Down),
        KeyCode::Char('p') | KeyCode::Char('P') => InputAction::TogglePause,
        KeyCode::Char('q') | KeyCode::Char('Q') => InputAction::Quit,
        _ => return Ok(None),
    };

    Ok(Some(action))
}

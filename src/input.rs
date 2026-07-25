//! crosstermキー入力処理(spec.md 1章・9.9)。
//!
//! `event::poll`+`event::read`でノンブロッキングに取得し、ゲームが扱う
//! `game::InputAction`へ変換する。

use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};

use crate::game::InputAction;

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
        KeyCode::Left => InputAction::MoveLeft,
        KeyCode::Right => InputAction::MoveRight,
        KeyCode::Up => InputAction::FaceUp,
        KeyCode::Down => InputAction::FaceDown,
        KeyCode::Char(' ') => InputAction::Drill,
        KeyCode::Char('p') | KeyCode::Char('P') => InputAction::TogglePause,
        KeyCode::Char('q') | KeyCode::Char('Q') => InputAction::Quit,
        KeyCode::Char('s') | KeyCode::Char('S') => InputAction::ToggleSound,
        _ => return Ok(None),
    };

    Ok(Some(action))
}

/// スプラッシュ/タイトル画面の「何かキーを押して進む」用。`poll_input`が対応する
/// キー(矢印・スペース・p/q/s)以外の入力(Enter等)も含め、Pressイベント全般を
/// 「進む」とみなす。Qキーのみ区別して返す(タイトル画面ではアプリ終了、
/// スプラッシュ画面でもQは終了として扱うため)。
pub fn poll_any_key(poll_ms: u64) -> std::io::Result<Option<AnyKeyAction>> {
    if !event::poll(Duration::from_millis(poll_ms))? {
        return Ok(None);
    }

    let Event::Key(key) = event::read()? else {
        return Ok(None);
    };

    if key.kind != KeyEventKind::Press {
        return Ok(None);
    }

    Ok(Some(match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') => AnyKeyAction::Quit,
        KeyCode::Char('s') | KeyCode::Char('S') => AnyKeyAction::ToggleSound,
        _ => AnyKeyAction::Advance,
    }))
}

/// `poll_any_key`の戻り値。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnyKeyAction {
    /// Q以外の任意のキー(Enter含む)。「進む」トリガーとして扱う。
    Advance,
    /// Qキー。呼び出し側でアプリ/スプラッシュの終了として扱う。
    Quit,
    /// Sキー。タイトル画面でのサウンド切り替えとして扱う(進むトリガーにはしない)。
    ToggleSound,
}

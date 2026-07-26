//! crosstermキー入力処理(spec.md 1章・9.9)。
//!
//! `event::poll`+`event::read`でノンブロッキングに取得し、ゲームが扱う
//! `game::InputAction`へ変換する。

use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};

use crate::game::InputAction;

/// キーコードを`InputAction`へ変換する。既知のショートカットに割り当てられていない
/// キーは`InputAction::UnboundKey`を返す(ユーザー指摘: 「ポーズ解除は、Pだけじゃなく、
/// ショートカット設定されていない任意のキー入力でも解除されるように」。一時停止中に
/// 限りmain.rs側で再開トリガーとして扱う)。`poll_input_batch`の実装本体。
fn action_from_key_code(code: KeyCode) -> InputAction {
    match code {
        KeyCode::Left => InputAction::MoveLeft,
        KeyCode::Right => InputAction::MoveRight,
        KeyCode::Up => InputAction::FaceUp,
        KeyCode::Down => InputAction::FaceDown,
        // 掘削キー(TERM独自拡張。ユーザー指摘: 「掘るボタンはXとZキー(どちらも
        // 掘れる)」)。どちらのキーでも同じ掘削として扱う。
        KeyCode::Char('x') | KeyCode::Char('X') | KeyCode::Char('z') | KeyCode::Char('Z') => InputAction::Drill,
        // 一時停止(TERM独自拡張。ユーザー指摘: 「スペースはポーズ」)。既存のPキーも
        // 引き続き有効(併用)。
        KeyCode::Char(' ') => InputAction::TogglePause,
        KeyCode::Char('p') | KeyCode::Char('P') => InputAction::TogglePause,
        KeyCode::Char('q') | KeyCode::Char('Q') => InputAction::Quit,
        // 一時停止中のみ意味を持つ、MUSIC/SE個別トグル(TERM独自拡張。ユーザー指摘:
        // 「サウンドON/OFFではなくMUSIC/SEをそれぞれトグルできるように」)。
        KeyCode::Char('m') | KeyCode::Char('M') => InputAction::ToggleMusic,
        KeyCode::Char('e') | KeyCode::Char('E') => InputAction::ToggleSe,
        // 一時停止中のみ意味を持つ、設定画面/ヘルプ画面のオーバーレイ表示(TERM独自拡張)。
        KeyCode::Char('s') | KeyCode::Char('S') => InputAction::OpenSettings,
        KeyCode::Char('h') | KeyCode::Char('H') => InputAction::OpenHelp,
        // デバッグショートカット(TERM独自拡張、動作確認用)。
        KeyCode::Char('c') | KeyCode::Char('C') => InputAction::DebugUnifyNearbyColors,
        KeyCode::Char('l') | KeyCode::Char('L') => InputAction::DebugAddLife,
        // 元はXキーだったが、掘削キー(X/Z)と衝突するためRキーへ変更した。
        KeyCode::Char('r') | KeyCode::Char('R') => InputAction::DebugClearAbovePlayer,
        KeyCode::Char('[') => InputAction::DebugBlockFallSlower,
        KeyCode::Char(']') => InputAction::DebugBlockFallFaster,
        KeyCode::Char('-') => InputAction::DebugPlayerFallSlower,
        KeyCode::Char('=') => InputAction::DebugPlayerFallFaster,
        KeyCode::Char(',') => InputAction::DebugShakeDurationLonger,
        KeyCode::Char('.') => InputAction::DebugShakeDurationShorter,
        _ => InputAction::UnboundKey,
    }
}

/// `poll_ms`だけ待って入力を確認し、その時点でキューされている全キーイベントを
/// `InputAction`へ変換してまとめて返す(TERM独自拡張)。矢印キー(向き変更・移動)と
/// スペースキー(掘削)をほぼ同時に押した場合でも、同一フレーム内に届いた各キーの
/// イベントを取りこぼさず両方とも処理できるようにするための、複数キー対応版。
pub fn poll_input_batch(poll_ms: u64) -> std::io::Result<Vec<InputAction>> {
    let mut actions = Vec::new();

    if !event::poll(Duration::from_millis(poll_ms))? {
        return Ok(actions);
    }

    loop {
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            actions.push(action_from_key_code(key.code));
        }

        if !event::poll(Duration::ZERO)? {
            break;
        }
    }

    Ok(actions)
}

/// スプラッシュ/タイトル画面の「何かキーを押して進む」用。`poll_input`が対応する
/// キー(矢印・スペース・p/q/s)以外の入力(Enter等)も含め、Pressイベント全般を
/// 「進む」とみなす。Qキー・Sキーのみ区別して返す(タイトル画面ではQがアプリ終了、
/// スプラッシュ画面でもQは終了として扱うため。Sキーは設定画面を開く)。
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
        KeyCode::Char('s') | KeyCode::Char('S') => AnyKeyAction::OpenSettings,
        KeyCode::Char('h') | KeyCode::Char('H') => AnyKeyAction::OpenHelp,
        _ => AnyKeyAction::Advance,
    }))
}

/// `poll_any_key`の戻り値。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnyKeyAction {
    /// Q・S・H以外の任意のキー(Enter含む)。「進む」トリガーとして扱う。
    Advance,
    /// Qキー。呼び出し側でアプリ/スプラッシュの終了として扱う。
    Quit,
    /// Sキー。タイトル画面での設定画面オープンとして扱う(TERM独自拡張。
    /// ユーザー指摘: 「設定画面つくって、カーソルで選んでスペースでトグル」)。
    OpenSettings,
    /// Hキー。タイトル画面でのショートカット一覧ヘルプ画面オープンとして扱う
    /// (TERM独自拡張。ユーザー指摘: 「ショートカットのヘルプページも必要」)。
    OpenHelp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_from_key_code_maps_known_shortcuts() {
        assert_eq!(action_from_key_code(KeyCode::Left), InputAction::MoveLeft);
        assert_eq!(action_from_key_code(KeyCode::Char('p')), InputAction::TogglePause);
        // ユーザー指摘: 「掘るボタンはXとZキー(どちらも掘れる)」「スペースはポーズ」。
        assert_eq!(action_from_key_code(KeyCode::Char('x')), InputAction::Drill);
        assert_eq!(action_from_key_code(KeyCode::Char('X')), InputAction::Drill);
        assert_eq!(action_from_key_code(KeyCode::Char('z')), InputAction::Drill);
        assert_eq!(action_from_key_code(KeyCode::Char('Z')), InputAction::Drill);
        assert_eq!(action_from_key_code(KeyCode::Char(' ')), InputAction::TogglePause);
        assert_eq!(action_from_key_code(KeyCode::Char('r')), InputAction::DebugClearAbovePlayer);
    }

    #[test]
    fn action_from_key_code_maps_unassigned_keys_to_unbound_key() {
        // ユーザー指摘: 「ポーズ解除は、Pだけじゃなく、ショートカット設定されていない
        // 任意のキー入力でも解除されるように」。既知のショートカットに割り当てられて
        // いないキーはUnboundKeyになる(main.rs側で一時停止中の再開トリガーに使う)。
        assert_eq!(action_from_key_code(KeyCode::Enter), InputAction::UnboundKey);
        assert_eq!(action_from_key_code(KeyCode::Tab), InputAction::UnboundKey);
        assert_eq!(action_from_key_code(KeyCode::Char('y')), InputAction::UnboundKey);
    }
}

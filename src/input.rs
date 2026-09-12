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
        KeyCode::Char('x') | KeyCode::Char('X') | KeyCode::Char('z') | KeyCode::Char('Z') => {
            InputAction::Drill
        }
        // 一時停止(TERM独自拡張。ユーザー指摘: 「スペースはポーズ」)。既存のPキーも
        // 引き続き有効(併用)。
        KeyCode::Char(' ') => InputAction::TogglePause,
        KeyCode::Char('p') | KeyCode::Char('P') => InputAction::TogglePause,
        // 終了/タイトルへ戻る(TERM独自拡張。ユーザー指摘: 「すべてのQキーをESCに変更」)。
        KeyCode::Esc => InputAction::Quit,
        // GameOverダイアログの選択確定・タイトル画面からの開始はEnterキーのみで行う
        // (TERM独自拡張。ユーザー指摘: 「メニューから進むのEnter」「他のボタンで
        // 進んではいけない」)。
        KeyCode::Enter => InputAction::Confirm,
        // フレーム巻き戻し(TERM独自拡張。#233)。押しやすい位置のBackspaceと、
        // Undoを連想できるUキーの両方に割り当てる(どちらも他のショートカットと衝突しない)。
        KeyCode::Backspace | KeyCode::Char('u') | KeyCode::Char('U') => InputAction::Rewind,
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
        KeyCode::Char('a') | KeyCode::Char('A') => InputAction::DebugFillAir,
        // 元はXキーだったが、掘削キー(X/Z)と衝突するためRキーへ変更した。
        KeyCode::Char('r') | KeyCode::Char('R') => InputAction::DebugClearAbovePlayer,
        // スターのキラキラ演出(#64)にちなんだキー。
        KeyCode::Char('k') | KeyCode::Char('K') => InputAction::DebugStarifyVisibleScreen,
        // ボム(Bomb)の頭文字。#96。ユーザー指摘: 「ショートカットキーもくれ」。
        KeyCode::Char('b') | KeyCode::Char('B') => InputAction::DebugPlaceBomb,
        // オートプレイ(#218)。T=auTopilot、G=God mode(無敵)。
        // TはONにすると無敵も同時にONになり、Gは無敵だけを単独で切り替える。
        KeyCode::Char('t') | KeyCode::Char('T') => InputAction::DebugToggleAutopilot,
        KeyCode::Char('g') | KeyCode::Char('G') => InputAction::DebugToggleInvincible,
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

/// タイトル画面用。Esc/S/HキーはそれぞれQuit/OpenSettings/OpenHelpとして区別し、
/// 「進む」(`Advance`)はEnterキーのみで発動する(TERM独自拡張。ユーザー指摘:
/// 「メニューから進むのEnter」「他のボタンで進んではいけない」。以前はEsc/S/H以外の
/// 任意のキーで進めたが、誤操作防止のためEnter専用にした。終了キーは元Qだったが
/// 「すべてのQキーをESCに変更」の指摘でEscへ変更)。それ以外のキーは
/// それ以外のキーは`Ignored`を返す(画面遷移は起こさないが「キーが押された」ことは
/// 呼び出し側へ伝える。アトラクトモードのアイドルタイマーをどのキーでもリセット
/// できるようにするため。TERM独自拡張。#218)。キー入力自体が無ければ`None`。
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
        KeyCode::Esc => AnyKeyAction::Quit,
        KeyCode::Char('s') | KeyCode::Char('S') => AnyKeyAction::OpenSettings,
        KeyCode::Char('h') | KeyCode::Char('H') => AnyKeyAction::OpenHelp,
        KeyCode::Enter => AnyKeyAction::Advance,
        _ => AnyKeyAction::Ignored,
    }))
}

/// `poll_any_key`の戻り値。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnyKeyAction {
    /// Enterキー。「進む」トリガーとして扱う。
    Advance,
    /// Escキー。呼び出し側でアプリ/スプラッシュの終了として扱う。
    Quit,
    /// Sキー。タイトル画面での設定画面オープンとして扱う(TERM独自拡張。
    /// ユーザー指摘: 「設定画面つくって、カーソルで選んでスペースでトグル」)。
    OpenSettings,
    /// Hキー。タイトル画面でのショートカット一覧ヘルプ画面オープンとして扱う
    /// (TERM独自拡張。ユーザー指摘: 「ショートカットのヘルプページも必要」)。
    OpenHelp,
    /// 上記いずれにも当てはまらないキー(TERM独自拡張。#218)。画面遷移は起こさないが、
    /// アトラクトモードのアイドルタイマーはリセットする。
    Ignored,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_from_key_code_maps_known_shortcuts() {
        assert_eq!(action_from_key_code(KeyCode::Left), InputAction::MoveLeft);
        assert_eq!(
            action_from_key_code(KeyCode::Char('p')),
            InputAction::TogglePause
        );
        // ユーザー指摘: 「掘るボタンはXとZキー(どちらも掘れる)」「スペースはポーズ」。
        assert_eq!(action_from_key_code(KeyCode::Char('x')), InputAction::Drill);
        assert_eq!(action_from_key_code(KeyCode::Char('X')), InputAction::Drill);
        assert_eq!(action_from_key_code(KeyCode::Char('z')), InputAction::Drill);
        assert_eq!(action_from_key_code(KeyCode::Char('Z')), InputAction::Drill);
        assert_eq!(
            action_from_key_code(KeyCode::Char(' ')),
            InputAction::TogglePause
        );
        assert_eq!(
            action_from_key_code(KeyCode::Char('r')),
            InputAction::DebugClearAbovePlayer
        );
        assert_eq!(
            action_from_key_code(KeyCode::Char('a')),
            InputAction::DebugFillAir
        );
        assert_eq!(
            action_from_key_code(KeyCode::Char('A')),
            InputAction::DebugFillAir
        );
        assert_eq!(
            action_from_key_code(KeyCode::Char('k')),
            InputAction::DebugStarifyVisibleScreen
        );
        assert_eq!(
            action_from_key_code(KeyCode::Char('K')),
            InputAction::DebugStarifyVisibleScreen
        );
        // ユーザー指摘: 「メニューから進むのEnter」「他のボタンで進んではいけない」。
        assert_eq!(action_from_key_code(KeyCode::Enter), InputAction::Confirm);
    }

    #[test]
    fn action_from_key_code_maps_the_autoplay_and_invincible_shortcuts() {
        // #218: T=オートプレイ(無敵も同時にON)、G=無敵単独。
        assert_eq!(
            action_from_key_code(KeyCode::Char('t')),
            InputAction::DebugToggleAutopilot
        );
        assert_eq!(
            action_from_key_code(KeyCode::Char('T')),
            InputAction::DebugToggleAutopilot
        );
        assert_eq!(
            action_from_key_code(KeyCode::Char('g')),
            InputAction::DebugToggleInvincible
        );
        assert_eq!(
            action_from_key_code(KeyCode::Char('G')),
            InputAction::DebugToggleInvincible
        );
    }

    #[test]
    fn action_from_key_code_maps_the_rewind_keys() {
        // #233: Backspace・U(大文字小文字とも)のいずれでも巻き戻しを起動できる。
        assert_eq!(
            action_from_key_code(KeyCode::Backspace),
            InputAction::Rewind
        );
        assert_eq!(
            action_from_key_code(KeyCode::Char('u')),
            InputAction::Rewind
        );
        assert_eq!(
            action_from_key_code(KeyCode::Char('U')),
            InputAction::Rewind
        );
    }

    #[test]
    fn the_rewind_keys_do_not_collide_with_any_other_shortcut() {
        // 巻き戻しキーを追加する際、既存のショートカットを奪っていないことを確認する。
        // 既存の全割り当てキーを列挙し、巻き戻し以外のアクションへ割り当てられたキーの中に
        // Backspace/u/Uが含まれていないことを見る。
        let existing_keys = [
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Esc,
            KeyCode::Enter,
            KeyCode::Char(' '),
            KeyCode::Char('x'),
            KeyCode::Char('z'),
            KeyCode::Char('p'),
            KeyCode::Char('m'),
            KeyCode::Char('e'),
            KeyCode::Char('s'),
            KeyCode::Char('h'),
            KeyCode::Char('c'),
            KeyCode::Char('l'),
            KeyCode::Char('a'),
            KeyCode::Char('r'),
            KeyCode::Char('k'),
            KeyCode::Char('b'),
            KeyCode::Char('t'),
            KeyCode::Char('g'),
            KeyCode::Char('['),
            KeyCode::Char(']'),
            KeyCode::Char('-'),
            KeyCode::Char('='),
            KeyCode::Char(','),
            KeyCode::Char('.'),
        ];
        for key in existing_keys {
            assert_ne!(
                action_from_key_code(key),
                InputAction::Rewind,
                "{key:?}が巻き戻しに奪われている"
            );
        }
    }

    #[test]
    fn action_from_key_code_maps_unassigned_keys_to_unbound_key() {
        // ユーザー指摘: 「ポーズ解除は、Pだけじゃなく、ショートカット設定されていない
        // 任意のキー入力でも解除されるように」。既知のショートカットに割り当てられて
        // いないキーはUnboundKeyになる(main.rs側で一時停止中の再開トリガーに使う)。
        assert_eq!(action_from_key_code(KeyCode::Tab), InputAction::UnboundKey);
        assert_eq!(
            action_from_key_code(KeyCode::Char('y')),
            InputAction::UnboundKey
        );
    }
}

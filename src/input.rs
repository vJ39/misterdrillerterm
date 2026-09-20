//! crosstermキー入力処理(spec.md 1章・9.9)。
//!
//! `event::poll`+`event::read`でノンブロッキングに取得し、ゲームが扱う
//! `game::InputAction`へ変換する。

use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

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
        // N人対戦のルーム開始(#276)。Enterは「選択中の候補へ招待」のままにして、
        // 誤操作で開始してしまわないよう別のキーに分ける(設計書3節)。
        KeyCode::Tab => InputAction::StartRoom,
        // AI対戦モードの開始(#296)。V=vs AI。
        KeyCode::Char('v') | KeyCode::Char('V') => InputAction::StartAiBattle,
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
        // 対戦の妨害ルール(#247)。O=Opponent(相手の攻撃を受け取る)。
        KeyCode::Char('o') | KeyCode::Char('O') => InputAction::DebugReceiveOpponentAttack,
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

/// テキスト編集画面(プレイヤー名入力、#270)専用の入力アクション。ゲームプレイ用の
/// `InputAction`とは意味が異なるキー割り当てのため独立して持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEditAction {
    Char(char),
    Backspace,
    Delete,
    MoveLeft {
        extend: bool,
    },
    MoveRight {
        extend: bool,
    },
    MoveToStart {
        extend: bool,
    },
    MoveToEnd {
        extend: bool,
    },
    SelectAll,
    /// Enter。確定してロビーへ進む。
    Confirm,
    /// Esc。タイトルへ戻る。
    Cancel,
}

/// キーイベントを`TextEditAction`へ変換する(`poll_text_edit_input`の実装本体)。
/// 修飾キーで意味が変わる(Shiftで選択拡張・Ctrl/Cmd+Aで全選択)ため、`KeyCode`だけでなく
/// `KeyEvent`全体を受け取る。どのアクションにも当てはまらないキーは`None`(無視)。
fn text_edit_action_from_key(key: KeyEvent) -> Option<TextEditAction> {
    // macOSのCmdはSUPERとして届くため、Ctrl+AとCmd+Aの両方を全選択として扱う。
    let command = key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.contains(KeyModifiers::SUPER);
    let extend = key.modifiers.contains(KeyModifiers::SHIFT);

    match key.code {
        KeyCode::Char('a') | KeyCode::Char('A') if command => Some(TextEditAction::SelectAll),
        // Ctrl/Cmd付きの文字キーはテキスト入力ではない(他のショートカットに使われうる)
        // ため、文字としては受け取らない。Shiftのみ・無修飾はそのまま文字にする
        // (crosstermは大文字/小文字を`c`自体に反映して届ける)。
        KeyCode::Char(_) if command => None,
        KeyCode::Char(c) => Some(TextEditAction::Char(c)),
        KeyCode::Backspace => Some(TextEditAction::Backspace),
        KeyCode::Delete => Some(TextEditAction::Delete),
        KeyCode::Left => Some(TextEditAction::MoveLeft { extend }),
        KeyCode::Right => Some(TextEditAction::MoveRight { extend }),
        KeyCode::Home => Some(TextEditAction::MoveToStart { extend }),
        KeyCode::End => Some(TextEditAction::MoveToEnd { extend }),
        KeyCode::Enter => Some(TextEditAction::Confirm),
        KeyCode::Esc => Some(TextEditAction::Cancel),
        _ => None,
    }
}

/// `poll_ms`だけ待って入力を確認し、その時点でキューされている全キーイベントを
/// `TextEditAction`へ変換してまとめて返す(#270)。文字を速く打った場合でも
/// 1フレームに届いた入力を取りこぼさないよう、`poll_input_batch`と同じ吐き出し方をする。
pub fn poll_text_edit_input(poll_ms: u64) -> std::io::Result<Vec<TextEditAction>> {
    let mut actions = Vec::new();

    if !event::poll(Duration::from_millis(poll_ms))? {
        return Ok(actions);
    }

    loop {
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && let Some(action) = text_edit_action_from_key(key)
        {
            actions.push(action);
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

    Ok(Some(any_key_action_from_key_code(key.code)))
}

/// キーコードを`AnyKeyAction`へ変換する(`poll_any_key`の実装本体)。キーイベントの
/// 取得はターミナルが要るためテストできないが、この対応表だけは単体で確認できるよう
/// 分けている。
fn any_key_action_from_key_code(code: KeyCode) -> AnyKeyAction {
    match code {
        KeyCode::Esc => AnyKeyAction::Quit,
        KeyCode::Char('s') | KeyCode::Char('S') => AnyKeyAction::OpenSettings,
        KeyCode::Char('h') | KeyCode::Char('H') => AnyKeyAction::OpenHelp,
        // N=Network。タイトルから対戦相手を探すロビーへ入る(#256)。
        KeyCode::Char('n') | KeyCode::Char('N') => AnyKeyAction::OpenNetworkLobby,
        KeyCode::Enter => AnyKeyAction::Advance,
        _ => AnyKeyAction::Ignored,
    }
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
    /// Nキー。タイトル画面から対戦相手を探すロビーを開く(#256。spec.md 12.1)。
    OpenNetworkLobby,
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
    fn action_from_key_code_maps_the_incoming_attack_key() {
        // #247: O(大文字小文字とも)で相手の攻撃を受け取るデバッグショートカット。
        assert_eq!(
            action_from_key_code(KeyCode::Char('o')),
            InputAction::DebugReceiveOpponentAttack
        );
        assert_eq!(
            action_from_key_code(KeyCode::Char('O')),
            InputAction::DebugReceiveOpponentAttack
        );
    }

    #[test]
    fn the_incoming_attack_key_does_not_collide_with_any_other_shortcut() {
        // #247でOキーを追加する際、既存のショートカットを奪っていないことを確認する。
        let existing_keys = [
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Esc,
            KeyCode::Enter,
            KeyCode::Backspace,
            KeyCode::Char(' '),
            KeyCode::Char('x'),
            KeyCode::Char('z'),
            KeyCode::Char('p'),
            KeyCode::Char('u'),
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
            KeyCode::Char('v'),
        ];
        for key in existing_keys {
            assert_ne!(
                action_from_key_code(key),
                InputAction::DebugReceiveOpponentAttack,
                "{key:?}がOキーのアクションに奪われている"
            );
        }
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
            KeyCode::Char('o'),
            KeyCode::Char('t'),
            KeyCode::Char('g'),
            KeyCode::Char('['),
            KeyCode::Char(']'),
            KeyCode::Char('-'),
            KeyCode::Char('='),
            KeyCode::Char(','),
            KeyCode::Char('.'),
            KeyCode::Char('v'),
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
    fn the_title_screen_maps_n_to_the_network_lobby() {
        // #256: タイトルからNキーで対戦相手を探すロビーへ入る。
        assert_eq!(
            any_key_action_from_key_code(KeyCode::Char('n')),
            AnyKeyAction::OpenNetworkLobby
        );
        assert_eq!(
            any_key_action_from_key_code(KeyCode::Char('N')),
            AnyKeyAction::OpenNetworkLobby
        );
    }

    #[test]
    fn the_network_lobby_key_does_not_take_over_the_other_title_screen_keys() {
        // Nキーの追加で、タイトル画面の既存のキー割り当てを奪っていないことを確認する。
        assert_eq!(
            any_key_action_from_key_code(KeyCode::Enter),
            AnyKeyAction::Advance
        );
        assert_eq!(
            any_key_action_from_key_code(KeyCode::Esc),
            AnyKeyAction::Quit
        );
        assert_eq!(
            any_key_action_from_key_code(KeyCode::Char('s')),
            AnyKeyAction::OpenSettings
        );
        assert_eq!(
            any_key_action_from_key_code(KeyCode::Char('h')),
            AnyKeyAction::OpenHelp
        );
        assert_eq!(
            any_key_action_from_key_code(KeyCode::Char('y')),
            AnyKeyAction::Ignored
        );
    }

    #[test]
    fn action_from_key_code_maps_unassigned_keys_to_unbound_key() {
        // ユーザー指摘: 「ポーズ解除は、Pだけじゃなく、ショートカット設定されていない
        // 任意のキー入力でも解除されるように」。既知のショートカットに割り当てられて
        // いないキーはUnboundKeyになる(main.rs側で一時停止中の再開トリガーに使う)。
        // Tabは#276でルーム開始に割り当てたため、ここでは別の未割り当てキーで確かめる。
        assert_eq!(
            action_from_key_code(KeyCode::Insert),
            InputAction::UnboundKey
        );
        assert_eq!(
            action_from_key_code(KeyCode::Char('y')),
            InputAction::UnboundKey
        );
    }

    #[test]
    fn action_from_key_code_maps_tab_to_starting_a_room() {
        // #276: ロビーで集めた参加者と対戦を開始する操作。
        assert_eq!(action_from_key_code(KeyCode::Tab), InputAction::StartRoom);
    }

    #[test]
    fn action_from_key_code_maps_v_to_starting_an_ai_battle() {
        // #296: ロビーからAIと対戦を始める操作(大文字小文字とも)。
        assert_eq!(
            action_from_key_code(KeyCode::Char('v')),
            InputAction::StartAiBattle
        );
        assert_eq!(
            action_from_key_code(KeyCode::Char('V')),
            InputAction::StartAiBattle
        );
    }

    #[test]
    fn the_ai_battle_key_does_not_collide_with_any_other_shortcut() {
        // #296でVキーを追加する際、既存のショートカットを奪っていないことを確認する。
        // 特にロビーで同時に使うEnter(申し込む)・Esc(戻る)・矢印(選択)・Tab(ルーム開始)と
        // 別物であること。
        let existing_keys = [
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Esc,
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::Backspace,
            KeyCode::Char(' '),
            KeyCode::Char('x'),
            KeyCode::Char('z'),
            KeyCode::Char('p'),
            KeyCode::Char('u'),
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
            KeyCode::Char('o'),
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
                InputAction::StartAiBattle,
                "{key:?}がAI対戦の開始に奪われている"
            );
        }
    }

    /// 修飾キーなしのキーイベントを組む(テスト用)。
    fn plain(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn text_edit_maps_the_editing_keys() {
        // #270: 文字入力・削除・確定・キャンセル。
        assert_eq!(
            text_edit_action_from_key(plain(KeyCode::Char('a'))),
            Some(TextEditAction::Char('a'))
        );
        assert_eq!(
            text_edit_action_from_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT)),
            Some(TextEditAction::Char('A')),
            "Shiftのみの文字キーはそのまま大文字として入力する"
        );
        assert_eq!(
            text_edit_action_from_key(plain(KeyCode::Backspace)),
            Some(TextEditAction::Backspace)
        );
        assert_eq!(
            text_edit_action_from_key(plain(KeyCode::Delete)),
            Some(TextEditAction::Delete)
        );
        assert_eq!(
            text_edit_action_from_key(plain(KeyCode::Enter)),
            Some(TextEditAction::Confirm)
        );
        assert_eq!(
            text_edit_action_from_key(plain(KeyCode::Esc)),
            Some(TextEditAction::Cancel)
        );
    }

    #[test]
    fn text_edit_extends_the_selection_only_while_shift_is_held() {
        // #270: 矢印・Home/EndはShiftの有無で選択拡張が切り替わる。
        for (code, without, with) in [
            (
                KeyCode::Left,
                TextEditAction::MoveLeft { extend: false },
                TextEditAction::MoveLeft { extend: true },
            ),
            (
                KeyCode::Right,
                TextEditAction::MoveRight { extend: false },
                TextEditAction::MoveRight { extend: true },
            ),
            (
                KeyCode::Home,
                TextEditAction::MoveToStart { extend: false },
                TextEditAction::MoveToStart { extend: true },
            ),
            (
                KeyCode::End,
                TextEditAction::MoveToEnd { extend: false },
                TextEditAction::MoveToEnd { extend: true },
            ),
        ] {
            assert_eq!(
                text_edit_action_from_key(plain(code)),
                Some(without),
                "{code:?}(修飾なし)は選択を拡張しない"
            );
            assert_eq!(
                text_edit_action_from_key(KeyEvent::new(code, KeyModifiers::SHIFT)),
                Some(with),
                "{code:?}(Shift)は選択を拡張する"
            );
        }
    }

    #[test]
    fn text_edit_maps_both_ctrl_a_and_cmd_a_to_select_all() {
        // #270: macOSのCmdはSUPERとして届くため、CONTROL/SUPERの両方を全選択にする。
        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::SUPER] {
            for code in [KeyCode::Char('a'), KeyCode::Char('A')] {
                assert_eq!(
                    text_edit_action_from_key(KeyEvent::new(code, modifiers)),
                    Some(TextEditAction::SelectAll),
                    "{code:?}+{modifiers:?}は全選択のはず"
                );
            }
        }
        // 修飾なしのaは普通の文字入力(全選択に奪われない)。
        assert_eq!(
            text_edit_action_from_key(plain(KeyCode::Char('a'))),
            Some(TextEditAction::Char('a'))
        );
    }

    #[test]
    fn text_edit_ignores_unrelated_keys() {
        // #270: 割り当ての無いキーと、Ctrl/Cmd付きの文字キー(テキスト入力ではない)は無視する。
        assert_eq!(text_edit_action_from_key(plain(KeyCode::Tab)), None);
        assert_eq!(text_edit_action_from_key(plain(KeyCode::Up)), None);
        assert_eq!(text_edit_action_from_key(plain(KeyCode::F(1))), None);
        assert_eq!(
            text_edit_action_from_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            None,
            "Ctrl+Xは文字として入力しない"
        );
        assert_eq!(
            text_edit_action_from_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER)),
            None,
            "Cmd+Cは文字として入力しない"
        );
    }

    #[test]
    fn the_start_room_key_does_not_collide_with_any_other_shortcut() {
        // ルーム開始(Tab)は他のどのキーにも割り当てられていないこと。特にロビーで
        // 同時に使うEnter(招待)・Esc(戻る)・矢印(選択)と別物であること。
        for code in [
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Backspace,
            KeyCode::Char(' '),
            KeyCode::Char('x'),
            KeyCode::Char('z'),
            KeyCode::Char('p'),
            KeyCode::Char('u'),
            KeyCode::Char('m'),
            KeyCode::Char('e'),
            KeyCode::Char('s'),
            KeyCode::Char('h'),
            KeyCode::Char('n'),
            KeyCode::Char('v'),
        ] {
            assert_ne!(
                action_from_key_code(code),
                InputAction::StartRoom,
                "{code:?}がルーム開始と衝突している"
            );
        }
    }
}

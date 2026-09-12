//! フレーム巻き戻し(TERM独自拡張。#233)。
//!
//! 一定フレームごとに`Game`丸ごとのスナップショットをリングバッファへ残しておき、
//! Backspace/Uキーで過去の状態へ戻ってやり直せるようにする、人間プレイ向けの救済機能。
//!
//! 入力の再生(同じ操作列をもう一度流し込む)方式ではなく、状態そのものを複製して
//! 持つ方式を採る。盤面・乱数・演出タイマーまで含めて丸ごと戻せるため、巻き戻し後の
//! 挙動が巻き戻さなかった場合と完全に一致することを構造的に保証できる。
//!
//! `Game`本体は履歴を持たない(`Game`は「今」だけを表す)。履歴の保持と逆再生セッションの
//! 進行はこのモジュールが受け持ち、両者の接続はmain.rsのメインループが行う。

use std::collections::VecDeque;
use std::time::Duration;

use crate::constants::{REWIND_MAX_SNAPSHOTS, REWIND_SNAPSHOT_INTERVAL_FRAMES};
use crate::game::{Game, InputAction};

/// 逆再生中、自動で1スナップショットぶん過去へ進むまでの間隔(ms)。
///
/// 短すぎると一瞬で最古まで飛んでどこで止めるか選べず、長すぎると止まって見える。
/// 実測の体感で300msを選んだ(スナップショット間隔は約3.3秒なので、約11倍速の巻き戻しに
/// 相当する)。
const REWIND_PLAYBACK_MS_PER_STEP: u64 = 300;

/// 履歴に残した1時点の状態。
pub struct Snapshot {
    /// 記録した時点のフレーム番号(`Game::debug_frame`)。デバッグ表示用。
    pub frame_at_capture: u64,
    /// 記録した時点の`Game`丸ごとの複製。
    pub game: Game,
}

/// スナップショットのリングバッファ。新しいものほど末尾(`back`)に積まれる。
pub struct RewindHistory {
    snapshots: VecDeque<Snapshot>,
    /// 最後に記録してから、記録可能な状態で何フレーム経過したか。
    frames_since_last_capture: u32,
}

impl Default for RewindHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl RewindHistory {
    pub fn new() -> Self {
        RewindHistory {
            snapshots: VecDeque::new(),
            frames_since_last_capture: 0,
        }
    }

    /// 履歴を全て捨てる。盤面の前提が変わる操作(設定変更・デバッグ速度変更・復活・
    /// タイトルへ戻る等)の後に、戻れてはいけない過去が残らないようにするために呼ぶ。
    pub fn clear(&mut self) {
        self.snapshots.clear();
        self.frames_since_last_capture = 0;
    }

    /// メインループから毎フレーム(`Game::update`の直後に)呼ぶ。
    ///
    /// 記録してよい状態(`Game::is_rewind_capturable`)でなければフレーム数も数えない。
    /// 一時停止中・昇天演出中・GameOver中に時間だけ進んでも、記録間隔は据え置かれる。
    pub fn maybe_capture(&mut self, game: &Game) {
        if !game.is_rewind_capturable() {
            return;
        }
        self.frames_since_last_capture += 1;
        if self.frames_since_last_capture >= REWIND_SNAPSHOT_INTERVAL_FRAMES {
            self.frames_since_last_capture = 0;
            self.capture_now(game);
        }
    }

    /// 間隔に関わらず、今の状態を1つ記録する。上限(`REWIND_MAX_SNAPSHOTS`)を
    /// 超えたぶんは最古から捨てる。
    pub fn capture_now(&mut self, game: &Game) {
        self.snapshots.push_back(Snapshot {
            frame_at_capture: game.debug_frame(),
            game: game.clone(),
        });
        while self.snapshots.len() > REWIND_MAX_SNAPSHOTS {
            self.snapshots.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    /// `cursor`番目のスナップショット(0=最新 … `len()-1`=最古)。
    ///
    /// # Panics
    /// `cursor >= len()`の場合。呼び出し側(`RewindSession`)はカーソルを常に
    /// `len()-1`以下に保つ責任を持つ。
    pub fn snapshot_at(&self, cursor: usize) -> &Snapshot {
        &self.snapshots[self.snapshots.len() - 1 - cursor]
    }

    /// `cursor`より新しい(未来側の)スナップショットを削除する。`cursor`自身と、
    /// それより過去は残す。巻き戻しを確定した直後に呼び、もう起こらなかったことに
    /// なった未来へ「進み直せて」しまわないようにする。
    ///
    /// 復元直後は現在の状態が`cursor`のスナップショットそのものなので、次の記録までの
    /// カウンタも0に戻す(ほぼ同一内容のスナップショットが直後に積まれるのを防ぐ)。
    pub fn discard_newer_than(&mut self, cursor: usize) {
        for _ in 0..cursor.min(self.snapshots.len()) {
            self.snapshots.pop_back();
        }
        self.frames_since_last_capture = 0;
    }
}

/// 1回の巻き戻し操作(逆再生セッション)の状態。
pub struct RewindSession {
    /// 現在見ているスナップショット(0=巻き戻し開始時点の「現在」 … 大きいほど過去)。
    cursor: usize,
    /// 自動逆再生中かどうか。プレイヤーが←/→で手動調整した時点でfalseになる。
    auto_reverse: bool,
    /// 自動逆再生の、次の1ステップまでの経過時間。
    elapsed_since_step: Duration,
}

/// `RewindSession::handle`の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewindOutcome {
    /// セッション継続(まだ巻き戻し中)。
    Continue,
    /// この`cursor`の状態で確定して再開する。`0`(=現在そのもの)は呼び出し側で
    /// キャンセル扱いとし、ストックを消費しない。
    Confirm(usize),
    /// 巻き戻しをやめる(現在の状態のまま再開する)。
    Cancel,
}

impl RewindSession {
    /// 逆再生を開始する。
    ///
    /// 開始時点の「現在」も履歴へ積んでからcursor=0で始めるので、→キーで現在まで
    /// 戻ってこられる(戻し過ぎたときのやり直しができる)。
    pub fn start(history: &mut RewindHistory, game: &Game) -> Self {
        history.capture_now(game);
        RewindSession {
            cursor: 0,
            auto_reverse: true,
            elapsed_since_step: Duration::ZERO,
        }
    }

    /// 巻き戻し中の毎フレーム呼ぶ。自動逆再生中なら一定間隔ごとに1つ過去へ進める。
    /// 最古のスナップショットに達したら自動逆再生を止める(自動では確定しない)。
    pub fn tick(&mut self, delta: Duration, history_len: usize) {
        if !self.auto_reverse || history_len == 0 {
            return;
        }
        let step = Duration::from_millis(REWIND_PLAYBACK_MS_PER_STEP);
        self.elapsed_since_step += delta;
        while self.elapsed_since_step >= step {
            if self.cursor + 1 >= history_len {
                // これ以上は戻れない。止まったことがわかるよう自動逆再生を解除する。
                self.auto_reverse = false;
                self.elapsed_since_step = Duration::ZERO;
                return;
            }
            self.elapsed_since_step -= step;
            self.cursor += 1;
        }
    }

    /// 巻き戻し中の入力を処理する。←/→での手動調整は自動逆再生を止める。
    pub fn handle(&mut self, action: InputAction, history_len: usize) -> RewindOutcome {
        match action {
            InputAction::MoveLeft => {
                self.auto_reverse = false;
                if self.cursor + 1 < history_len {
                    self.cursor += 1;
                }
                RewindOutcome::Continue
            }
            InputAction::MoveRight => {
                self.auto_reverse = false;
                self.cursor = self.cursor.saturating_sub(1);
                RewindOutcome::Continue
            }
            // Enter(Confirm)とX/Z(Drill)のどちらでも確定できる。
            InputAction::Confirm | InputAction::Drill => RewindOutcome::Confirm(self.cursor),
            InputAction::Quit => RewindOutcome::Cancel,
            _ => RewindOutcome::Continue,
        }
    }

    /// 現在見ているスナップショットの位置(0=現在 … 大きいほど過去)。
    pub fn cursor(&self) -> usize {
        self.cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::GameStatus;

    fn playing_game() -> Game {
        Game::new(1)
    }

    #[test]
    fn maybe_capture_records_one_snapshot_per_interval_counted_in_frames() {
        // 記録間隔は実時間(Duration)ではなく呼び出し回数=ゲームフレーム数で数える。
        let game = playing_game();
        let mut history = RewindHistory::new();

        for _ in 0..(REWIND_SNAPSHOT_INTERVAL_FRAMES - 1) {
            history.maybe_capture(&game);
        }
        assert_eq!(history.len(), 0, "間隔に達する前は記録されないはず");

        history.maybe_capture(&game);
        assert_eq!(history.len(), 1);

        for _ in 0..REWIND_SNAPSHOT_INTERVAL_FRAMES {
            history.maybe_capture(&game);
        }
        assert_eq!(history.len(), 2, "さらに1間隔ぶんで1つ増えるはず");
    }

    #[test]
    fn maybe_capture_drops_the_oldest_snapshot_beyond_the_maximum() {
        let mut game = playing_game();
        let mut history = RewindHistory::new();

        // 記録されるたびに区別できるよう、スコアを変えながら上限+2個ぶん回す。
        for i in 0..(REWIND_MAX_SNAPSHOTS + 2) {
            game.player.score = i as u64;
            for _ in 0..REWIND_SNAPSHOT_INTERVAL_FRAMES {
                history.maybe_capture(&game);
            }
        }

        assert_eq!(history.len(), REWIND_MAX_SNAPSHOTS, "上限を超えないはず");
        assert_eq!(
            history.snapshot_at(0).game.player.score,
            (REWIND_MAX_SNAPSHOTS + 1) as u64,
            "cursor=0は最新のはず"
        );
        assert_eq!(
            history
                .snapshot_at(REWIND_MAX_SNAPSHOTS - 1)
                .game
                .player
                .score,
            2,
            "最古の2個(score=0,1)が捨てられているはず"
        );
    }

    #[test]
    fn maybe_capture_does_not_advance_while_the_state_is_not_capturable() {
        // 一時停止中・GameOver中・昇天演出中は記録しないだけでなく、経過フレーム数も
        // 数えない(再開後に間隔が短縮されないようにする)。
        let mut game = playing_game();
        let mut history = RewindHistory::new();

        game.status = GameStatus::Paused;
        for _ in 0..(REWIND_SNAPSHOT_INTERVAL_FRAMES * 3) {
            history.maybe_capture(&game);
        }
        assert_eq!(history.len(), 0, "一時停止中は蓄積しないはず");

        game.status = GameStatus::GameOver;
        for _ in 0..(REWIND_SNAPSHOT_INTERVAL_FRAMES * 3) {
            history.maybe_capture(&game);
        }
        assert_eq!(history.len(), 0, "GameOver中も蓄積しないはず");

        game.status = GameStatus::Playing;
        for _ in 0..(REWIND_SNAPSHOT_INTERVAL_FRAMES - 1) {
            history.maybe_capture(&game);
        }
        assert_eq!(
            history.len(),
            0,
            "再開後は間隔いっぱい数え直すはず(停止中の経過は繰り越さない)"
        );
        history.maybe_capture(&game);
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn clear_discards_every_snapshot_and_the_pending_interval() {
        let game = playing_game();
        let mut history = RewindHistory::new();
        history.capture_now(&game);
        for _ in 0..(REWIND_SNAPSHOT_INTERVAL_FRAMES - 1) {
            history.maybe_capture(&game);
        }

        history.clear();

        assert!(history.is_empty());
        // 溜まりかけていたカウンタも0に戻っているので、1フレームでは記録されない。
        history.maybe_capture(&game);
        assert_eq!(history.len(), 0);
    }

    #[test]
    fn discard_newer_than_keeps_the_confirmed_snapshot_and_everything_older() {
        let mut game = playing_game();
        let mut history = RewindHistory::new();
        for i in 0..5u64 {
            game.player.score = i;
            history.capture_now(&game);
        }
        // cursor: 0=score4(最新) 1=score3 2=score2 3=score1 4=score0(最古)

        history.discard_newer_than(2);

        assert_eq!(history.len(), 3, "未来側の2つ(score4,3)が消えるはず");
        assert_eq!(
            history.snapshot_at(0).game.player.score,
            2,
            "確定した時点が新しい最新になるはず"
        );
        assert_eq!(history.snapshot_at(2).game.player.score, 0);
    }

    #[test]
    fn discard_newer_than_zero_keeps_everything() {
        let mut game = playing_game();
        let mut history = RewindHistory::new();
        for i in 0..3u64 {
            game.player.score = i;
            history.capture_now(&game);
        }

        history.discard_newer_than(0);

        assert_eq!(history.len(), 3);
        assert_eq!(history.snapshot_at(0).game.player.score, 2);
    }

    #[test]
    fn capture_now_records_the_current_frame_number() {
        let game = playing_game();
        let mut history = RewindHistory::new();
        history.capture_now(&game);
        assert_eq!(history.snapshot_at(0).frame_at_capture, game.debug_frame());
    }

    #[test]
    fn start_captures_the_current_state_so_the_cursor_can_return_to_it() {
        let mut game = playing_game();
        let mut history = RewindHistory::new();
        game.player.score = 10;
        history.capture_now(&game);
        game.player.score = 20;

        let session = RewindSession::start(&mut history, &game);

        assert_eq!(session.cursor(), 0);
        assert_eq!(history.len(), 2, "開始時点の現在も履歴に積まれるはず");
        assert_eq!(
            history.snapshot_at(0).game.player.score,
            20,
            "cursor=0は巻き戻し開始時点の現在そのもの"
        );
    }

    #[test]
    fn tick_auto_reverses_one_step_per_playback_interval() {
        let game = playing_game();
        let mut history = RewindHistory::new();
        for _ in 0..4 {
            history.capture_now(&game);
        }
        let mut session = RewindSession::start(&mut history, &game);
        let len = history.len();

        session.tick(Duration::from_millis(REWIND_PLAYBACK_MS_PER_STEP - 1), len);
        assert_eq!(session.cursor(), 0, "間隔に満たなければ進まない");

        session.tick(Duration::from_millis(1), len);
        assert_eq!(session.cursor(), 1);

        session.tick(Duration::from_millis(REWIND_PLAYBACK_MS_PER_STEP * 2), len);
        assert_eq!(
            session.cursor(),
            3,
            "まとめて経過したぶんは複数ステップ進む"
        );
    }

    #[test]
    fn tick_stops_the_auto_reverse_at_the_oldest_snapshot() {
        let game = playing_game();
        let mut history = RewindHistory::new();
        history.capture_now(&game);
        history.capture_now(&game);
        let mut session = RewindSession::start(&mut history, &game);
        let len = history.len(); // 3

        session.tick(
            Duration::from_millis(REWIND_PLAYBACK_MS_PER_STEP * 100),
            len,
        );

        assert_eq!(session.cursor(), len - 1, "最古で止まるはず");
        assert!(!session.auto_reverse, "最古に達したら自動逆再生は停止する");
    }

    #[test]
    fn handle_moves_the_cursor_and_stops_at_both_ends() {
        let game = playing_game();
        let mut history = RewindHistory::new();
        history.capture_now(&game);
        history.capture_now(&game);
        let mut session = RewindSession::start(&mut history, &game);
        let len = history.len(); // 3

        assert_eq!(
            session.handle(InputAction::MoveLeft, len),
            RewindOutcome::Continue
        );
        assert_eq!(session.cursor(), 1);
        assert!(!session.auto_reverse, "手動調整で自動逆再生は止まる");

        session.handle(InputAction::MoveLeft, len);
        assert_eq!(session.cursor(), 2);
        session.handle(InputAction::MoveLeft, len);
        assert_eq!(session.cursor(), 2, "最古より過去へは進まない");

        session.handle(InputAction::MoveRight, len);
        assert_eq!(session.cursor(), 1);
        session.handle(InputAction::MoveRight, len);
        assert_eq!(session.cursor(), 0);
        session.handle(InputAction::MoveRight, len);
        assert_eq!(session.cursor(), 0, "現在より未来へは進まない");
    }

    #[test]
    fn handle_confirms_with_enter_or_drill_and_cancels_with_esc() {
        let game = playing_game();
        let mut history = RewindHistory::new();
        history.capture_now(&game);
        let mut session = RewindSession::start(&mut history, &game);
        let len = history.len();
        session.handle(InputAction::MoveLeft, len);

        assert_eq!(
            session.handle(InputAction::Confirm, len),
            RewindOutcome::Confirm(1)
        );
        assert_eq!(
            session.handle(InputAction::Drill, len),
            RewindOutcome::Confirm(1),
            "X/Zでも確定できる"
        );
        assert_eq!(
            session.handle(InputAction::Quit, len),
            RewindOutcome::Cancel
        );
    }

    #[test]
    fn handle_confirm_at_cursor_zero_means_no_change() {
        // Confirm(0)は「巻き戻し開始時点の現在そのもの」なので、呼び出し側(main.rs)は
        // 復元もストック消費も行わずキャンセルと同じ扱いにする。ここではセッション側が
        // 確かにcursor=0のままConfirmを返すことだけを確認する。
        let game = playing_game();
        let mut history = RewindHistory::new();
        history.capture_now(&game);
        let mut session = RewindSession::start(&mut history, &game);

        assert_eq!(
            session.handle(InputAction::Confirm, history.len()),
            RewindOutcome::Confirm(0)
        );
    }

    #[test]
    fn handle_ignores_unrelated_actions() {
        let game = playing_game();
        let mut history = RewindHistory::new();
        history.capture_now(&game);
        let mut session = RewindSession::start(&mut history, &game);
        let len = history.len();

        for action in [
            InputAction::FaceUp,
            InputAction::FaceDown,
            InputAction::TogglePause,
            InputAction::UnboundKey,
            InputAction::Rewind,
        ] {
            assert_eq!(session.handle(action, len), RewindOutcome::Continue);
            assert_eq!(session.cursor(), 0);
        }
    }
}

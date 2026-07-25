//! ゲーム全体のオーケストレーション(盤面+プレイヤー+タイマー類)。
//!
//! board/player/physics は副作用のない純粋なロジックだが、この`Game`はそれらを
//! 「1フレーム進める」「1回入力を処理する」という時間軸に沿ってまとめ、UI/audio層が
//! 反応すべき`GameEvent`列を返す薄いオーケストレーション層。

pub mod board;
pub mod physics;
pub mod player;

use std::time::Duration;

use crate::constants::{FALL_TICK_MS, FIELD_DEPTH_M, INPUT_COOLDOWN_MS, OXYGEN_WARNING_THRESHOLD};
use board::Board;
use physics::{CheckpointEvent, DigOutcome, Direction};
use player::Player;

/// ゲーム全体の進行状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameStatus {
    Playing,
    Paused,
    GameOver,
    Cleared,
}

/// 1回のupdate/入力処理で発生したイベント。UIの効果音再生・演出判断に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameEvent {
    /// 掘削入力を受け付けた瞬間(ブロックの有無に関わらず常に発生)
    Dig,
    /// 岩ブロックへ入力し、掘削に失敗した(任意の専用SE対象)
    DigFailRock,
    /// ブロックが破壊され消滅した
    BlockDestroyed,
    /// 酸素カプセルを取得した
    OxygenCollected,
    /// ダイヤブロックを取得した
    DiamondCollected,
    /// チェックポイントへ到達した(1000m到達時はis_clear=true)
    Checkpoint(CheckpointEvent),
    /// 酸素残量が警告閾値以下の間、1秒間隔で発生
    OxygenWarningTick,
    /// 落下ブロックに押し潰されてゲームオーバーになった
    Crushed,
    /// 酸素切れでゲームオーバーになった
    OutOfOxygen,
    /// 深度1000m到達でゲームクリアした
    Cleared,
}

/// ノーマルコース シングルプレイのゲーム状態一式。
pub struct Game {
    pub board: Board,
    pub player: Player,
    pub status: GameStatus,
    fall_tick_accum: Duration,
    input_cooldown_remaining: Duration,
    oxygen_warning_accum: Duration,
}

impl Game {
    /// 指定シードで新しいゲームを開始する。
    pub fn new(seed: u64) -> Self {
        Game {
            board: Board::generate(seed, FIELD_DEPTH_M),
            player: Player::new(),
            status: GameStatus::Playing,
            fall_tick_accum: Duration::ZERO,
            input_cooldown_remaining: Duration::ZERO,
            oxygen_warning_accum: Duration::ZERO,
        }
    }

    /// 次のチェックポイントまでの残り距離(m)。ステータスパネル表示用(spec.md 9章)。
    pub fn distance_to_next_checkpoint_m(&self) -> usize {
        let depth = self.player.depth_m();
        crate::constants::CHECKPOINTS
            .iter()
            .find(|cp| cp.depth_m > depth)
            .map(|cp| cp.depth_m - depth)
            .unwrap_or(0)
    }

    /// P キー: 一時停止/再開のトグル。GameOver/Cleared中は無効。
    pub fn toggle_pause(&mut self) {
        self.status = match self.status {
            GameStatus::Playing => GameStatus::Paused,
            GameStatus::Paused => GameStatus::Playing,
            other => other,
        };
    }

    /// 移動入力(←/→/↓)を1回処理する。INPUT_COOLDOWN_MS未経過なら無視する。
    pub fn try_input_move(&mut self, dir: Direction) -> Vec<GameEvent> {
        let mut events = Vec::new();

        if self.status != GameStatus::Playing {
            return events;
        }
        if self.input_cooldown_remaining > Duration::ZERO {
            return events;
        }
        self.input_cooldown_remaining = Duration::from_millis(INPUT_COOLDOWN_MS);

        // 掘削入力を受け付けた瞬間(ブロックの有無に関わらず)常に発生
        events.push(GameEvent::Dig);

        let outcome = physics::attempt_move(&mut self.board, &mut self.player, dir);
        match outcome {
            DigOutcome::BlockedByBoundary => {}
            DigOutcome::BlockedByRock => events.push(GameEvent::DigFailRock),
            DigOutcome::MovedIntoEmpty => {}
            DigOutcome::DestroyedColor => events.push(GameEvent::BlockDestroyed),
            DigOutcome::CollectedOxygen => events.push(GameEvent::OxygenCollected),
            DigOutcome::CollectedDiamond => events.push(GameEvent::DiamondCollected),
        }

        if let Some(cp) = physics::check_checkpoint(&mut self.player) {
            let is_clear = cp.is_clear;
            events.push(GameEvent::Checkpoint(cp));
            if is_clear {
                self.status = GameStatus::Cleared;
                events.push(GameEvent::Cleared);
            }
        }

        events
    }

    /// メインループから毎フレーム呼ぶ。deltaぶんの時間経過(酸素減少・落下tick)を反映する。
    pub fn update(&mut self, delta: Duration) -> Vec<GameEvent> {
        let mut events = Vec::new();

        if self.status != GameStatus::Playing {
            return events;
        }

        self.player.elapsed_seconds += delta.as_secs_f32();

        if self.input_cooldown_remaining > Duration::ZERO {
            self.input_cooldown_remaining = self.input_cooldown_remaining.saturating_sub(delta);
        }

        physics::apply_oxygen_decay(&mut self.player, delta.as_secs_f32());

        if self.player.alive && self.player.oxygen <= OXYGEN_WARNING_THRESHOLD {
            self.oxygen_warning_accum += delta;
            if self.oxygen_warning_accum >= Duration::from_secs(1) {
                self.oxygen_warning_accum -= Duration::from_secs(1);
                events.push(GameEvent::OxygenWarningTick);
            }
        } else {
            self.oxygen_warning_accum = Duration::ZERO;
        }

        if !self.player.alive {
            self.status = GameStatus::GameOver;
            events.push(GameEvent::OutOfOxygen);
            return events;
        }

        self.fall_tick_accum += delta;
        let tick = Duration::from_millis(FALL_TICK_MS);
        while self.fall_tick_accum >= tick {
            self.fall_tick_accum -= tick;
            physics::process_gravity_tick(&mut self.board, &mut self.player);
            if !self.player.alive {
                self.status = GameStatus::GameOver;
                events.push(GameEvent::Crushed);
                break;
            }
        }

        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use board::Cell;

    #[test]
    fn reaching_final_checkpoint_via_move_clears_the_game() {
        let mut game = Game::new(1);
        // 最終手前まで進めておき、最後の1マスだけ実際の入力で踏ませる
        game.player.row = FIELD_DEPTH_M - 2;
        game.player.checkpoints_reached = vec![200, 400, 600, 800];
        let last_row = FIELD_DEPTH_M - 1;
        game.board.rows[last_row][game.player.col] = Cell::Empty;

        let events = game.try_input_move(Direction::Down);

        assert_eq!(game.status, GameStatus::Cleared);
        assert!(events.iter().any(|e| matches!(e, GameEvent::Cleared)));
    }

    #[test]
    fn oxygen_running_out_during_update_ends_the_game() {
        let mut game = Game::new(2);
        game.player.oxygen = 1.0;

        let events = game.update(Duration::from_secs(1));

        assert_eq!(game.status, GameStatus::GameOver);
        assert!(events.iter().any(|e| matches!(e, GameEvent::OutOfOxygen)));
        assert!(!game.player.alive);
    }
}

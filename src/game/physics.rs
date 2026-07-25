//! 掘削処理・押し潰し判定・チェックポイント到達判定(spec.md 1章・5〜7章)。
//!
//! board.rs が提供する純粋な盤面操作(連結判定・重力落下)を、プレイヤー状態と
//! 組み合わせてゲームルールとして解釈する層。ratatui/crossterm/rodio の副作用は持たない。

use crate::constants::{CHECKPOINTS, FIELD_DEPTH_M, FIELD_WIDTH, OXYGEN_DECAY_PER_SEC};
use crate::game::board::{apply_gravity_tick, Board, Cell};
use crate::game::player::Player;

/// 移動入力の方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Down,
}

/// 1回の移動入力の結果。UI側の効果音再生・演出の判断に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigOutcome {
    /// 岩ブロックへ入力し、掘削できず移動もしなかった
    BlockedByRock,
    /// フィールド境界外への入力で移動できなかった(岩ではないので専用の失敗音対象外)
    BlockedByBoundary,
    /// 何もない(Empty)マスへ移動した
    MovedIntoEmpty,
    /// 色ブロックを破壊して移動した
    DestroyedColor,
    /// 酸素カプセルを取得して移動した
    CollectedOxygen,
    /// ダイヤブロックを取得して移動した
    CollectedDiamond,
}

/// 移動入力を1回処理する(spec.md 1章 操作一覧)。
///
/// - 掘削入力を受け付けた瞬間(ブロックの有無に関わらず)呼ばれる想定。呼び出し側で
///   「掘削音は常に鳴らす」処理をこの戻り値に関わらず行うこと。
/// - 岩ブロック、またはフィールド境界外への移動は移動不可を表す`DigOutcome`を返し、
///   プレイヤーの位置・状態は一切変更しない。
pub fn attempt_move(board: &mut Board, player: &mut Player, dir: Direction) -> DigOutcome {
    let (dr, dc): (isize, isize) = match dir {
        Direction::Left => (0, -1),
        Direction::Right => (0, 1),
        Direction::Down => (1, 0),
    };

    let nr = player.row as isize + dr;
    let nc = player.col as isize + dc;

    if nr < 0 || nc < 0 || nc as usize >= FIELD_WIDTH || nr as usize >= board.depth_rows() {
        return DigOutcome::BlockedByBoundary;
    }
    let nr = nr as usize;
    let nc = nc as usize;

    let outcome = match board.cell(nr, nc) {
        Cell::Rock => return DigOutcome::BlockedByRock,
        Cell::Empty => DigOutcome::MovedIntoEmpty,
        Cell::Color(_) => DigOutcome::DestroyedColor,
        Cell::Oxygen => DigOutcome::CollectedOxygen,
        Cell::Diamond => DigOutcome::CollectedDiamond,
    };

    // 破壊/取得を伴うセルは掘削によりEmpty化する
    if !matches!(outcome, DigOutcome::MovedIntoEmpty) {
        board.rows[nr][nc] = Cell::Empty;
    }

    player.row = nr;
    player.col = nc;

    match outcome {
        DigOutcome::CollectedOxygen => {
            player.add_oxygen(crate::constants::OXYGEN_CAPSULE_RESTORE);
        }
        DigOutcome::CollectedDiamond => {
            player.diamonds_collected += 1;
            player.diamond_score += crate::constants::DIAMOND_SCORE;
        }
        _ => {}
    }

    outcome
}

/// 酸素の自然減少を適用し、尽きていれば生存フラグを落とす(spec.md 6章・8章)。
pub fn apply_oxygen_decay(player: &mut Player, delta_seconds: f32) {
    if !player.alive || player.cleared {
        return;
    }
    player.decay_oxygen(OXYGEN_DECAY_PER_SEC, delta_seconds);
    if player.is_out_of_oxygen() {
        player.alive = false;
    }
}

/// 重力落下の論理ティックを1回実行し、押し潰し判定結果をプレイヤー状態へ反映する
/// (spec.md 4章・5章)。
pub fn process_gravity_tick(board: &mut Board, player: &mut Player) {
    if !player.alive || player.cleared {
        return;
    }
    let outcome = apply_gravity_tick(board, player.position());
    if outcome.crushed {
        player.alive = false;
    }
}

/// チェックポイント到達イベント。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointEvent {
    pub depth_m: usize,
    pub bonus: u64,
    pub is_clear: bool,
}

/// タイムボーナス = 基礎ボーナス + max(0, 基準タイム - 到達時点の経過秒数) × 10
fn time_bonus(checkpoint: &crate::constants::Checkpoint, elapsed_seconds: f32) -> u64 {
    let diff = (checkpoint.base_time_sec - elapsed_seconds).max(0.0);
    checkpoint.base_bonus + (diff * 10.0) as u64
}

/// プレイヤーの現在深度がチェックポイントに到達していれば、酸素全回復・タイムボーナス
/// 加算・(1000m到達ならクリア確定)を行う(spec.md 7章)。
///
/// 深度はDownの移動でのみ1mずつ増加するため、チェックポイント深度をちょうど跨がずに
/// 通過することはない。
pub fn check_checkpoint(player: &mut Player) -> Option<CheckpointEvent> {
    let depth = player.depth_m();

    for cp in CHECKPOINTS.iter() {
        if depth >= cp.depth_m && !player.checkpoints_reached.contains(&cp.depth_m) {
            player.checkpoints_reached.push(cp.depth_m);
            player.oxygen = crate::constants::OXYGEN_MAX;
            let bonus = time_bonus(cp, player.elapsed_seconds);
            player.time_bonus_total += bonus;

            let is_clear = cp.depth_m == FIELD_DEPTH_M;
            if is_clear {
                player.cleared = true;
            }

            return Some(CheckpointEvent {
                depth_m: cp.depth_m,
                bonus,
                is_clear,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::OXYGEN_MAX;
    use crate::game::board::ColorKind;

    fn empty_board(rows: usize) -> Board {
        Board {
            rows: vec![[Cell::Empty; FIELD_WIDTH]; rows],
        }
    }

    // --- エッジケース: 岩ブロックは掘削できない ---

    #[test]
    fn attempt_move_into_rock_is_blocked_and_leaves_player_and_board_unchanged() {
        let mut board = empty_board(3);
        board.rows[0][7] = Cell::Rock; // Player::new()の初期位置(row0,col6)の右隣
        let mut player = Player::new();

        let outcome = attempt_move(&mut board, &mut player, Direction::Right);

        assert_eq!(outcome, DigOutcome::BlockedByRock);
        assert_eq!(player.position(), (0, 6));
        assert_eq!(board.cell(0, 7), Cell::Rock); // 破壊されず残る
    }

    // --- エッジケース: フィールド端での移動制限 ---

    #[test]
    fn attempt_move_left_at_left_boundary_is_blocked() {
        let mut board = empty_board(3);
        let mut player = Player::new();
        player.col = 0;

        let outcome = attempt_move(&mut board, &mut player, Direction::Left);

        assert_eq!(outcome, DigOutcome::BlockedByBoundary);
        assert_eq!(player.position(), (0, 0));
    }

    #[test]
    fn attempt_move_right_at_right_boundary_is_blocked() {
        let mut board = empty_board(3);
        let mut player = Player::new();
        player.col = FIELD_WIDTH - 1;

        let outcome = attempt_move(&mut board, &mut player, Direction::Right);

        assert_eq!(outcome, DigOutcome::BlockedByBoundary);
        assert_eq!(player.position(), (0, FIELD_WIDTH - 1));
    }

    #[test]
    fn attempt_move_down_at_field_bottom_boundary_is_blocked() {
        let mut board = empty_board(3);
        let mut player = Player::new();
        player.row = 2; // board.depth_rows() - 1

        let outcome = attempt_move(&mut board, &mut player, Direction::Down);

        assert_eq!(outcome, DigOutcome::BlockedByBoundary);
        assert_eq!(player.position(), (2, 6));
    }

    // --- 正常系: 酸素カプセル取得で酸素が回復する ---

    #[test]
    fn collecting_oxygen_capsule_restores_oxygen() {
        let mut board = empty_board(3);
        board.rows[1][6] = Cell::Oxygen; // Player::new()の真下
        let mut player = Player::new();
        player.oxygen = 40.0;

        let outcome = attempt_move(&mut board, &mut player, Direction::Down);

        assert_eq!(outcome, DigOutcome::CollectedOxygen);
        assert_eq!(player.oxygen, 90.0); // 40 + OXYGEN_CAPSULE_RESTORE(50)
        assert_eq!(board.cell(1, 6), Cell::Empty); // カプセルは消費される
    }

    #[test]
    fn collecting_oxygen_capsule_clamps_at_max() {
        let mut board = empty_board(3);
        board.rows[1][6] = Cell::Oxygen;
        let mut player = Player::new();
        player.oxygen = 80.0;

        attempt_move(&mut board, &mut player, Direction::Down);

        assert_eq!(player.oxygen, OXYGEN_MAX);
    }

    // --- 異常系: 酸素が0になった場合にミス判定 ---

    #[test]
    fn oxygen_decay_to_zero_marks_player_dead() {
        let mut player = Player::new();
        player.oxygen = 1.0;

        apply_oxygen_decay(&mut player, 1.0); // 2.0/sec * 1s = 2.0 > 1.0

        assert!(player.is_out_of_oxygen());
        assert!(!player.alive);
    }

    #[test]
    fn oxygen_decay_above_zero_keeps_player_alive() {
        let mut player = Player::new();

        apply_oxygen_decay(&mut player, 1.0);

        assert!(player.alive);
        assert_eq!(player.oxygen, OXYGEN_MAX - crate::constants::OXYGEN_DECAY_PER_SEC);
    }

    #[test]
    fn oxygen_decay_is_ignored_once_player_is_already_dead() {
        let mut player = Player::new();
        player.alive = false;
        player.oxygen = 50.0;

        apply_oxygen_decay(&mut player, 100.0);

        assert_eq!(player.oxygen, 50.0); // 死亡後は減衰処理自体が走らない
    }

    // --- 正常系: チェックポイント到達で酸素全回復 ---

    #[test]
    fn checkpoint_reach_restores_oxygen_and_awards_time_bonus() {
        let mut player = Player::new();
        player.row = 199; // depth_m = 200 (最初のチェックポイント)
        player.oxygen = 10.0;
        player.elapsed_seconds = 10.0;

        let event = check_checkpoint(&mut player).expect("チェックポイントに到達しているはず");

        assert_eq!(event.depth_m, 200);
        assert!(!event.is_clear);
        assert_eq!(player.oxygen, OXYGEN_MAX);
        assert_eq!(event.bonus, 1300); // base 1000 + (40.0-10.0)*10
        assert_eq!(player.time_bonus_total, 1300);
        assert!(player.checkpoints_reached.contains(&200));
    }

    #[test]
    fn checkpoint_is_not_awarded_twice() {
        let mut player = Player::new();
        player.row = 199;
        check_checkpoint(&mut player);

        player.oxygen = 5.0; // 再到達なしで酸素が減った状態を模擬

        let second = check_checkpoint(&mut player);

        assert!(second.is_none());
        assert_eq!(player.oxygen, 5.0); // 二重に回復しない
    }

    #[test]
    fn final_checkpoint_marks_player_cleared() {
        let mut player = Player::new();
        player.row = FIELD_DEPTH_M - 1; // depth_m = 1000
        player.checkpoints_reached = vec![200, 400, 600, 800];

        let event = check_checkpoint(&mut player).expect("最終チェックポイントに到達しているはず");

        assert_eq!(event.depth_m, 1000);
        assert!(event.is_clear);
        assert!(player.cleared);
    }

    // --- 異常系: 落下ブロックがプレイヤー位置に到達した場合にミス判定 ---

    #[test]
    fn process_gravity_tick_sets_player_dead_when_crushed() {
        let mut board = empty_board(3);
        board.rows[0][6] = Cell::Color(ColorKind::Red);
        board.rows[0][7] = Cell::Color(ColorKind::Red);
        board.rows[0][8] = Cell::Color(ColorKind::Red);
        let mut player = Player::new();
        player.row = 1;
        player.col = 7; // 落下後グループの中央列の直下

        process_gravity_tick(&mut board, &mut player);

        assert!(!player.alive);
    }

    #[test]
    fn process_gravity_tick_keeps_player_alive_when_not_crushed() {
        let mut board = empty_board(3);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        board.rows[0][1] = Cell::Color(ColorKind::Red);
        board.rows[0][2] = Cell::Color(ColorKind::Red);
        let mut player = Player::new(); // col=6、落下グループ(col0-2)から離れている

        process_gravity_tick(&mut board, &mut player);

        assert!(player.alive);
    }
}

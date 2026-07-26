//! 掘削処理・押し潰し判定・重力ティックのゲームルール解釈(spec.md 1章・4〜6章)。
//!
//! board.rs が提供する純粋な盤面操作(連結判定・重力落下)を、プレイヤー状態と
//! 組み合わせてゲームルールとして解釈する層。ratatui/crossterm/rodio の副作用は持たない。

use crate::constants::OXYGEN_DECAY_PER_SEC;
use crate::game::board::{
    apply_gravity_tick, connected_rock_group, connected_same_color, drill_color_block, hit_rock, is_group_supported,
    is_supported, Board, BlockMove, Cell, GravityState, RockHitResult,
};
use crate::game::player::{Direction, Player};

/// 1回の掘削(移動を伴う/伴わない問わず)入力の結果。UI側の効果音再生・演出の判断に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrillOutcome {
    /// フィールド境界外への入力で何も起きなかった
    OutOfBounds,
    /// 対象セルが何もない(Empty)ため、掘削としての効果は発生しなかった
    NoEffect,
    /// 岩ブロックへヒットしたが、5回未満でまだ破壊に至らない
    RockHitIntact,
    /// 岩ブロックが累積5回目のヒットで破壊された(酸素-20%はこの呼び出し内で適用済み)。
    /// `blocks`はこのヒットで連結して一緒に消滅した岩ブロックの総数(spec.md 4.9)
    RockDestroyed { blocks: usize },
    /// 色ブロックを直接掘削し、連結している同色グループごと消滅した(サイズ問わず、spec.md 4.6)
    ColorDestroyed { blocks: usize },
    /// facing方向が酸素カプセルだったため、掘削としては何も起きなかった(TERM独自拡張)。
    /// AIRは「掘る」対象ではなく、横移動・自由落下で「触れる」ことでのみ取得できる
    /// (ユーザー指摘: 「AIRは掘っても取得できない、そもそも掘る操作じゃなくてタッチして
    /// 取得するイメージ」)。掘削では消滅も進入もしない
    OxygenUntouchedByDrill,
    /// ダイヤブロックを取得した(スコア+500はこの呼び出し内で適用済み)
    CollectedDiamond,
    /// facingがUpで、頭上のブロックがまだ支持されていない(または揺れている)不安定な
    /// 状態だったため、掘削できずに押し潰された(TERM独自拡張。ユーザー指摘:
    /// 「落下中のブロックは掘れない(つぶされる)。結合して止まってるブロックが上に
    /// あるときは掘れる」)。呼び出し側(Game)がこれを見てミス判定を行う。
    CrushedByUnstableOverhead,
    /// スターブロックを掘削で破壊した(スコア+10はこの呼び出し内で適用済み、TERM独自
    /// 拡張)。放置しても画面内に入れば自然に溶けて消えるが、掘削でも即座に壊せる。
    StarDestroyed,
}

/// 指定セルに対して掘削を1回実行し、盤面・プレイヤーのスコア/酸素へ反映する
/// (移動は行わない。呼び出し側がoutcomeを見て移動の要否を判断する)。
fn drill_cell(board: &mut Board, player: &mut Player, target: (usize, usize)) -> DrillOutcome {
    match board.cell(target.0, target.1) {
        Cell::Empty => DrillOutcome::NoEffect,
        Cell::Color(_) => {
            let blocks = drill_color_block(board, target);
            player.award_drill_score(blocks);
            DrillOutcome::ColorDestroyed { blocks }
        }
        Cell::Rock { .. } => match hit_rock(board, target).expect("直前のmatchでRockと確認済み") {
            RockHitResult::StillIntact => DrillOutcome::RockHitIntact,
            RockHitResult::Destroyed { blocks } => {
                // 連結して巻き込まれた分も含め、酸素ペナルティは実際に掘削した1回分のみ
                // 適用する(spec.md 2章・4.9・6章)。
                player.apply_rock_break_penalty();
                DrillOutcome::RockDestroyed { blocks }
            }
        },
        Cell::Oxygen => DrillOutcome::OxygenUntouchedByDrill,
        Cell::Diamond => {
            board.set(target.0, target.1, Cell::Empty);
            player.collect_diamond();
            DrillOutcome::CollectedDiamond
        }
        Cell::Star { .. } => {
            board.set(target.0, target.1, Cell::Empty);
            player.award_drill_score(1);
            DrillOutcome::StarDestroyed
        }
    }
}

/// ←/→ (MoveLeft/MoveRight)の結果(spec.md 1章)。掘削は一切発生しないため、UI/audio層が
/// 反応すべき`GameEvent`は無い……が、酸素カプセルへの移動だけは例外(下記参照、TERM独自拡張)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LateralOutcome {
    /// フィールド境界外への入力で何も起きなかった(facingの変更も含め完全なno-op)
    OutOfBounds,
    /// 隣接する同じ高さのマスがEmptyだったため、そのマスへ1マス移動した
    MovedLevel,
    /// 隣接する同じ高さのマスが酸素カプセルだったため、掘削を伴わずそのマスへ移動しつつ
    /// 取得した(酸素+50・スコア加算はこの呼び出し内で適用済み。TERM独自拡張、下記参照)
    MovedLevelAndCollectedOxygen,
    /// 直前に同じ方向へぶつかって停止していた状態で、再度同じ方向キーが入力され、
    /// かつその1段上(row-1)のマスがEmptyだったため、1段登って斜め上のマスへ移動した
    ClimbedStep,
    /// 上記と同様に1段登ったが、登った先(row-1)が酸素カプセルだったため取得も行った
    /// (TERM独自拡張、下記参照)
    ClimbedStepAndCollectedOxygen,
    /// 隣接マスが塞がっていたため、その場に留まった(facingの変更のみ反映され、
    /// ブロックは一切破壊されない)。1段上が空いていても、まだ「同じ方向への
    /// 2回目の入力」でなければ登らない(下記move_lateralの2ステップ仕様を参照)
    Blocked,
}

/// ← / → (MoveLeft/MoveRight)入力を1回処理する(spec.md 1章)。
///
/// **掘削は一切行わない**、地形追従の移動のみ。facingをその方向へ変更したうえで、
/// 隣接する同じ高さのマスがEmptyならそこへ移動する(この場合は1回の入力でそのまま
/// 移動する。従来通り)。
///
/// **例外(TERM独自拡張)**: 隣接する同じ高さのマスが酸素カプセルの場合、掘削を伴わずに
/// そのまま移動しつつ自動的に取得する(酸素+50・スコア加算は既存の`collect_oxygen_capsule`
/// をそのまま使う)。色ブロック・岩ブロック・ダイヤブロックは従来通りただの障害物のままで、
/// 取得/破壊にはSpace(Drill)が必要。
///
/// 隣接マスが塞がっている場合は、1段上(row-1)が空いていても**即座には登らない**、
/// 2ステップの段差登りになっている(ユーザー指摘による仕様変更)。
/// - 1回目にその方向へ入力した際は、facingをその方向に変えるだけでその場に停止し、
///   「その方向にぶつかって止まっている」ことを`player.bumped_direction`に記憶する
/// - 直前のフレーム/入力で同じ方向にぶつかって停止していた状態で、再度同じ方向キーが
///   入力され、かつ1段上のマスが空いていれば、そこで初めて1段登って斜め上へ移動する
///   (1段上が酸素カプセルの場合も同様に登りながら取得する)
/// - 方向を変えずに同じ方向へ2回連続で入力しないと登れない。別の方向キーを挟んだ場合は
///   `bumped_direction`が新しい方向で上書きされ、また1回目からやり直しになる
/// - **キャラ自身の真上(player.row-1, player.col)がブロックで塞がっている場合は、
///   登り先(1段上・隣の列)が空いていても一切登れない**(TERM独自拡張。ユーザー指摘:
///   「キャラの上にブロックがある場合は1段登ることはできないものとする」。頭上が
///   塞がっている状態で斜めに登り抜けるのは不自然なため)
///
/// どちらのマスも塞がっていれば、何度入力してもその場に留まる(ブロックは一切破壊されず、
/// そのまま残る)。フィールド端(列0の左、列11の右)へ向けた入力は何も起きない
/// (facingの変更も含め、spec.md 1章末尾の「何も起きない」という明記に基づく解釈)。
pub fn move_lateral(board: &mut Board, player: &mut Player, dir: Direction) -> LateralOutcome {
    debug_assert!(matches!(dir, Direction::Left | Direction::Right), "move_lateralはLeft/Right専用");

    let (_, dc) = dir.delta();
    let nc = player.col as isize + dc;
    if nc < 0 || nc as usize >= crate::constants::FIELD_WIDTH {
        return LateralOutcome::OutOfBounds;
    }
    let nc = nc as usize;

    // 「直前に同じ方向へぶつかって停止していたか」を、facingを更新する前に判定しておく。
    let was_bumped_same_dir = player.bumped_direction == Some(dir);
    player.facing = dir;

    match board.cell(player.row, nc) {
        Cell::Empty => {
            player.col = nc;
            player.bumped_direction = None;
            return LateralOutcome::MovedLevel;
        }
        Cell::Oxygen => {
            board.set(player.row, nc, Cell::Empty);
            player.collect_oxygen_capsule();
            player.col = nc;
            player.bumped_direction = None;
            return LateralOutcome::MovedLevelAndCollectedOxygen;
        }
        _ => {}
    }

    if was_bumped_same_dir && player.row > 0 && board.cell(player.row - 1, player.col) == Cell::Empty {
        match board.cell(player.row - 1, nc) {
            Cell::Empty => {
                player.row -= 1;
                player.col = nc;
                player.bumped_direction = None;
                return LateralOutcome::ClimbedStep;
            }
            Cell::Oxygen => {
                board.set(player.row - 1, nc, Cell::Empty);
                player.collect_oxygen_capsule();
                player.row -= 1;
                player.col = nc;
                player.bumped_direction = None;
                return LateralOutcome::ClimbedStepAndCollectedOxygen;
            }
            _ => {}
        }
    }

    // 1回目のぶつかり、または2回目でも1段上が空いていない場合は、その場に停止する。
    // 「この方向にぶつかって止まっている」ことを記憶し、次の同方向入力で登れるようにする。
    player.bumped_direction = Some(dir);
    LateralOutcome::Blocked
}

/// facingがUpの掘削対象セル`target`が「不安定」(支えを失い、かつ揺れの猶予期間も
/// 明けて実際に落下している最中)かどうかを判定する(TERM独自拡張)。色ブロック・
/// 岩ブロックは連結している塊全体で、酸素カプセル・ダイヤ・スターは単独セルで判定する
/// (spec.md 4章「同色ブロックが隣接したら必ず結合する」と同じ考え方を、上向き掘削の
/// 安定判定にも適用する)。
///
/// 揺れている最中(まだ静止していて、これから落下する予告状態)は押し潰しの対象に
/// **しない**(ユーザー指摘: 「結合して引っかかったブロックに対しては上向き掘ったら、
/// ちゃんと掘れる(つぶされない)」「ぐらぐら中は、上に掘ったら掘れる」)。揺れの
/// 猶予期間はプレイヤーへの警告演出であり、その間に掘り出せば安全に処理できる。
fn is_overhead_unstable(board: &Board, gravity: &GravityState, target: (usize, usize), player_pos: (usize, usize)) -> bool {
    match board.cell(target.0, target.1) {
        Cell::Empty => false,
        Cell::Color(color) => {
            let group = connected_same_color(board, target, color);
            !is_group_supported(board, &group, player_pos) && !group.iter().any(|&p| gravity.is_shaking(p))
        }
        Cell::Rock { .. } => {
            let group = connected_rock_group(board, target);
            !is_group_supported(board, &group, player_pos) && !group.iter().any(|&p| gravity.is_shaking(p))
        }
        // AIR(酸素カプセル)は押し潰しの脅威にはならない(ユーザー指摘: 「AIRだったら、
        // 掘れはしないけどちゃんと取れてほしい。AIRに対しては掘っても無効化しておけば
        // いいだけ」)。不安定でも上向き掘削はdrill_cellのOxygenUntouchedByDrillへ
        // そのまま流れ、押し潰しにはならない。取得は歩み寄り・自由落下・重力ティックでの
        // 自動取得を通じて行われる。
        Cell::Oxygen => false,
        Cell::Diamond | Cell::Star { .. } => {
            !is_supported(board, target, player_pos) && !gravity.is_shaking(target)
        }
    }
}

/// Space(Drill)入力を1回処理する(spec.md 1章)。
///
/// facing方向の1マス先を、**移動を伴わずに**掘削する。掘った結果そのマスが空いても、
/// プレイヤーはその場に留まる(TERM独自拡張。ユーザー指摘: 「掘ったからと言って、
/// プレイヤー落下速度が上がってはいけない」)。掘って開けたマスへ実際に進むのは、
/// 既存の自由落下(`apply_player_free_fall`、`player_fall_tick_ms`ごとの論理ティック)
/// に委ねる。これにより、掘削の連打で通常の落下ペースを追い越すことができなくなる
/// (ブロックの重力や揺れ演出と同じ拍に、プレイヤー自身の下降も揃う)。
///
/// facingがUpの場合のみ追加のチェックがある: 頭上のブロックがまだ支持されて
/// おらず、かつ揺れの猶予も明けて実際に落下中の不安定な状態なら、掘削せずにその場で
/// 押し潰される(TERM独自拡張。ユーザー指摘: 「落下中のブロックは掘れない(つぶされる)。
/// 結合して止まってるブロックが上にあるときは掘れる」「ぐらぐら中は、上に掘ったら掘れる」)。
///
/// 掘削キーを挟んだ場合は、Left/Rightの2ステップ段差登り(move_lateral)における
/// 「ぶつかって停止中」の状態をリセットする(spec.md 1章、TERM独自拡張)。
pub fn drill_facing(board: &mut Board, player: &mut Player, gravity: &GravityState) -> DrillOutcome {
    player.bumped_direction = None;

    let (dr, dc) = player.facing.delta();
    let nr = player.row as isize + dr;
    let nc = player.col as isize + dc;
    if nr < 0 || nc < 0 || nr as usize >= board.depth_rows() || nc as usize >= crate::constants::FIELD_WIDTH {
        return DrillOutcome::OutOfBounds;
    }

    let target = (nr as usize, nc as usize);

    if player.facing == Direction::Up && is_overhead_unstable(board, gravity, target, player.position()) {
        return DrillOutcome::CrushedByUnstableOverhead;
    }

    drill_cell(board, player, target)
}

/// 酸素の自然減少を適用する(spec.md 6章)。生死判定は呼び出し側(Game)が行う。
pub fn apply_oxygen_decay(player: &mut Player, delta_seconds: f32) {
    player.decay_oxygen(OXYGEN_DECAY_PER_SEC, delta_seconds);
}

/// 重力ティック1回ぶんの、ゲームルールとしての結果(spec.md 4〜5章)。
#[derive(Debug, Clone, Default)]
pub struct GravityTickResult {
    /// このティックで実際に1マス落下した各セルの(移動後の位置, 移動前の位置)
    /// (TERM独自拡張。ブロック落下のピクセル単位補間描画に使う。個数は`.len()`で取れる
    /// ため、落下セル数を別フィールドに重複して持たない)。
    pub moved_cells: Vec<BlockMove>,
    /// 自動消滅(spec.md 4.5)で消滅した色ブロック数(スコア加算はこの呼び出し内で適用済み)。
    pub auto_vanished_blocks: usize,
    /// 自動消滅(spec.md 4.9)で消滅した岩ブロック数。色ブロックと異なり得点は発生しない
    /// (2章・7章)が、破壊音等のイベント発火判断に呼び出し側が使う。
    pub auto_vanished_rock_blocks: usize,
    /// 落下ブロックに押し潰され、かつ無敵時間中でなかった(=呼び出し側がライフ処理を
    /// 行うべき)かどうか。無敵時間中の押し潰しはブロックの消滅のみ発生しライフは失わない
    /// (spec.md 5章末尾、TERM独自拡張)。
    pub life_lost_to_crush: bool,
    /// 落下してきた酸素カプセルがプレイヤーに触れて取得された回数(TERM独自拡張)。
    /// 呼び出し側(Game)がこの回数ぶん`Player::collect_oxygen_capsule`を呼ぶ。
    pub oxygen_collected: usize,
}

/// 重力落下の論理ティックを1回実行し、自動消滅のスコア加算・押し潰し判定・
/// 落下してきた酸素カプセルの取得を行う(spec.md 4章・5章)。
///
/// `shake_ticks`: 支えを失ってから実際に落下し始めるまでの揺れティック数
/// (呼び出し側が揺れ時間設定(ms)とブロック落下tick間隔から都度換算して渡す。
/// デバッグショートカットで実行時調整可能・TERM独自拡張)。
pub fn process_gravity_tick(
    board: &mut Board,
    player: &mut Player,
    gravity: &mut GravityState,
    invulnerable: bool,
    shake_ticks: u8,
) -> GravityTickResult {
    let outcome = apply_gravity_tick(board, player.position(), gravity, shake_ticks);

    if outcome.auto_vanished_blocks > 0 {
        player.award_auto_vanish_score(outcome.auto_vanished_blocks);
    }
    // 岩ブロックの自動消滅(spec.md 4.9)は得点対象外なのでスコア加算はしない。

    for _ in 0..outcome.oxygen_collected {
        player.collect_oxygen_capsule();
    }

    GravityTickResult {
        moved_cells: outcome.moved_cells,
        auto_vanished_blocks: outcome.auto_vanished_blocks,
        auto_vanished_rock_blocks: outcome.auto_vanished_rock_blocks,
        life_lost_to_crush: outcome.crushed && !invulnerable,
        oxygen_collected: outcome.oxygen_collected,
    }
}

/// `apply_player_free_fall`の結果(TERM独自拡張)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreeFallOutcome {
    /// 落下しなかった(支持されている、または既に最深行)
    DidNotFall,
    /// 空きマスへ1マス落下した
    Fell,
    /// 酸素カプセルのマスへ1マス落下すると同時に取得した。
    /// 「歩くだけで取得できる」(spec.md公式マニュアル "just walk into it")仕様により、
    /// 掘削しなくても自由落下だけでAIRの上に乗ればその場で取得される
    /// (ユーザー指摘反映: 「AIRがキャラの下にあるとき掘らないと行けないのはバグ」)。
    FellAndCollectedOxygen,
}

/// プレイヤー自身の自由落下(spec.md 1章・4章、TERM独自拡張)。
///
/// 色ブロック等と同じ`FALL_TICK_MS`ごとの論理ティックで、入力の有無や掘削とは無関係に
/// 毎回判定する。直下のマスがフィールド内かつEmptyであれば1マス落下する
/// (揺れは挟まず即座に落ちる。プレイヤーは常に自分の意思で移動するため、支えを失った
/// ブロックのような`SHAKE_TICKS`ぶんの猶予は無い)。直下が酸素カプセルの場合も同様に
/// 通過でき、その場で取得する(掘削は不要)。それ以外の非Empty、または既に最深行に
/// 到達している場合は何も起きない。
pub fn apply_player_free_fall(board: &mut Board, player: &mut Player) -> FreeFallOutcome {
    let below = player.row + 1;
    if below >= board.depth_rows() {
        return FreeFallOutcome::DidNotFall;
    }
    match board.cell(below, player.col) {
        Cell::Empty => {
            player.row = below;
            FreeFallOutcome::Fell
        }
        Cell::Oxygen => {
            board.set(below, player.col, Cell::Empty);
            player.row = below;
            player.collect_oxygen_capsule();
            FreeFallOutcome::FellAndCollectedOxygen
        }
        _ => FreeFallOutcome::DidNotFall,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{FIELD_WIDTH, OXYGEN_MAX, ROCK_HITS_TO_BREAK, SHAKE_TICKS};
    use crate::game::board::ColorKind;

    fn empty_board(rows: usize) -> Board {
        Board {
            rows: vec![[Cell::Empty; FIELD_WIDTH]; rows],
        }
    }

    /// `SHAKE_TICKS`ぶんの揺れティックを消化してから、実際に落下するティックを1回実行する。
    fn shake_out_then_process_tick(
        board: &mut Board,
        player: &mut Player,
        gravity: &mut GravityState,
        invulnerable: bool,
    ) -> GravityTickResult {
        for _ in 0..SHAKE_TICKS {
            process_gravity_tick(board, player, gravity, invulnerable, SHAKE_TICKS);
        }
        process_gravity_tick(board, player, gravity, invulnerable, SHAKE_TICKS)
    }

    // --- MoveLeft/MoveRight: 掘削なしの地形追従移動(facing変更・フィールド端・移動/1段登り/停止) ---

    #[test]
    fn move_lateral_at_left_boundary_is_a_complete_no_op() {
        let mut board = empty_board(3);
        let mut player = Player::new();
        player.col = 0;
        player.facing = Direction::Up;

        let outcome = move_lateral(&mut board, &mut player, Direction::Left);

        assert_eq!(outcome, LateralOutcome::OutOfBounds);
        assert_eq!(player.col, 0);
        assert_eq!(player.facing, Direction::Up); // facingも変わらない(spec.md 1章末尾)
    }

    #[test]
    fn move_lateral_into_empty_moves_and_sets_facing() {
        let mut board = empty_board(3);
        let mut player = Player::new();

        let outcome = move_lateral(&mut board, &mut player, Direction::Right);

        assert_eq!(outcome, LateralOutcome::MovedLevel);
        assert_eq!(player.col, FIELD_WIDTH / 2 + 1);
        assert_eq!(player.row, 0); // 高さは変わらない
        assert_eq!(player.facing, Direction::Right);
    }

    #[test]
    fn move_lateral_first_bump_stops_without_climbing_even_when_row_above_is_empty() {
        // 隣接マスが色ブロックで塞がっていても、掘削は一切行わない。ユーザー指摘による
        // 2ステップ仕様: 1段上(row-1)が空いていても、1回目の入力では即座には登らず、
        // facingの変更だけでその場に停止する(spec.md 1章)。
        let mut board = empty_board(3);
        let mut player = Player::new();
        player.row = 1;
        let target_col = player.col + 1;
        board.rows[player.row][target_col] = Cell::Color(ColorKind::Red);
        // row-1側は空けておく(既定でEmpty)

        let outcome = move_lateral(&mut board, &mut player, Direction::Right);

        assert_eq!(outcome, LateralOutcome::Blocked);
        assert_eq!(player.row, 1); // 登っていない
        assert_eq!(player.col, target_col - 1); // 移動していない
        assert_eq!(player.facing, Direction::Right);
        assert_eq!(player.bumped_direction, Some(Direction::Right)); // ぶつかった方向を記憶する
        // ブロックは破壊されずそのまま残る
        assert_eq!(board.cell(1, target_col), Cell::Color(ColorKind::Red));
        assert_eq!(player.score, 0);
    }

    #[test]
    fn move_lateral_second_press_same_direction_climbs_the_step() {
        // 直前に同じ方向へぶつかって停止していた状態で、再度同じ方向キーが入力され、
        // かつ1段上が空いていれば、そこで初めて1段登る(2ステップ仕様の核心)。
        let mut board = empty_board(3);
        let mut player = Player::new();
        player.row = 1;
        let target_col = player.col + 1;
        board.rows[player.row][target_col] = Cell::Color(ColorKind::Red);

        let first = move_lateral(&mut board, &mut player, Direction::Right); // 1回目: ぶつかって停止
        let second = move_lateral(&mut board, &mut player, Direction::Right); // 2回目: 登る

        assert_eq!(first, LateralOutcome::Blocked);
        assert_eq!(second, LateralOutcome::ClimbedStep);
        assert_eq!(player.row, 0); // 1段登った
        assert_eq!(player.col, target_col);
        assert_eq!(player.facing, Direction::Right);
        assert_eq!(player.bumped_direction, None); // 登ったのでリセットされる
        // ブロックは破壊されずそのまま残る
        assert_eq!(board.cell(1, target_col), Cell::Color(ColorKind::Red));
        assert_eq!(player.score, 0);
    }

    #[test]
    fn move_lateral_into_unbroken_rock_climbs_on_second_press_same_direction() {
        let mut board = empty_board(3);
        let mut player = Player::new();
        player.row = 1;
        let target_col = player.col + 1;
        board.rows[player.row][target_col] = Cell::Rock { hits: 0 };

        move_lateral(&mut board, &mut player, Direction::Right); // 1回目: ぶつかって停止
        let outcome = move_lateral(&mut board, &mut player, Direction::Right); // 2回目: 登る

        assert_eq!(outcome, LateralOutcome::ClimbedStep);
        assert_eq!(player.row, 0);
        assert_eq!(player.col, target_col);
        // 岩ブロックはヒットを受けず、そのまま残る
        assert!(matches!(board.cell(1, target_col), Cell::Rock { hits: 0 }));
    }

    #[test]
    fn move_lateral_does_not_climb_when_a_block_is_directly_above_the_player() {
        // ユーザー指摘: 「キャラの上にブロックがある場合は1段登ることはできないものと
        // する」。登り先(1段上・隣の列)が空いていても、キャラ自身の真上が塞がって
        // いれば登れない。
        let mut board = empty_board(3);
        let mut player = Player::new();
        player.row = 1;
        let target_col = player.col + 1;
        board.rows[player.row][target_col] = Cell::Color(ColorKind::Red);
        board.rows[player.row - 1][player.col] = Cell::Rock { hits: 0 }; // キャラの真上が塞がっている

        move_lateral(&mut board, &mut player, Direction::Right); // 1回目: ぶつかって停止
        let outcome = move_lateral(&mut board, &mut player, Direction::Right); // 2回目でも登れない

        assert_eq!(outcome, LateralOutcome::Blocked);
        assert_eq!(player.row, 1, "頭上が塞がっているので登れない");
        assert_eq!(player.col, target_col - 1, "移動していない");
        assert_eq!(board.cell(1, target_col), Cell::Color(ColorKind::Red)); // ブロックは残る
    }

    #[test]
    fn move_lateral_switching_direction_resets_the_bump() {
        // 別の方向キーを挟むと「ぶつかって停止中」の状態はリセットされる。
        // 切り替え後の1回目の入力だけでは、まだ登れない。
        let mut board = empty_board(3);
        let mut player = Player::new();
        player.row = 1;
        let left_col = player.col - 1;
        let right_col = player.col + 1;
        board.rows[player.row][left_col] = Cell::Color(ColorKind::Red);
        board.rows[player.row][right_col] = Cell::Color(ColorKind::Blue);

        move_lateral(&mut board, &mut player, Direction::Left); // Leftへぶつかる
        assert_eq!(player.bumped_direction, Some(Direction::Left));

        let outcome = move_lateral(&mut board, &mut player, Direction::Right); // Rightへ切り替え

        assert_eq!(outcome, LateralOutcome::Blocked); // 切り替え直後は登れない
        assert_eq!(player.row, 1);
        assert_eq!(player.bumped_direction, Some(Direction::Right));
    }

    #[test]
    fn move_lateral_is_blocked_when_adjacent_and_row_above_both_occupied() {
        let mut board = empty_board(3);
        let mut player = Player::new();
        player.row = 1;
        let target_col = player.col + 1;
        board.rows[player.row][target_col] = Cell::Rock {
            hits: ROCK_HITS_TO_BREAK - 1,
        };
        board.rows[player.row - 1][target_col] = Cell::Color(ColorKind::Blue); // 1段上も塞がっている

        move_lateral(&mut board, &mut player, Direction::Right); // 1回目
        let outcome = move_lateral(&mut board, &mut player, Direction::Right); // 2回目でも1段上が塞がっているので登れない

        assert_eq!(outcome, LateralOutcome::Blocked);
        assert_eq!(player.col, target_col - 1); // 移動していない
        assert_eq!(player.row, 1);
        assert_eq!(player.facing, Direction::Right); // facingは反映される
        // どちらのブロックも破壊されずそのまま残る
        assert!(matches!(board.cell(1, target_col), Cell::Rock { hits } if hits == ROCK_HITS_TO_BREAK - 1));
        assert_eq!(board.cell(0, target_col), Cell::Color(ColorKind::Blue));
        assert_eq!(player.oxygen, OXYGEN_MAX); // 岩に触れても酸素は減らない(掘削していないため)
    }

    #[test]
    fn move_lateral_into_oxygen_capsule_collects_it_without_drilling() {
        // task2(ユーザー指摘): AIRカプセルは掘削不要で、隣接マスへの移動だけで
        // 自動的に取得できる(TERM独自拡張、spec.md 1章)。
        let mut board = empty_board(3);
        let mut player = Player::new();
        player.oxygen = 40.0;
        let target_col = player.col + 1;
        board.rows[player.row][target_col] = Cell::Oxygen;

        let outcome = move_lateral(&mut board, &mut player, Direction::Right);

        assert_eq!(outcome, LateralOutcome::MovedLevelAndCollectedOxygen);
        assert_eq!(player.col, target_col); // 実際にそのマスへ移動している
        assert_eq!(player.oxygen, 40.0 + crate::constants::OXYGEN_CAPSULE_RESTORE);
        assert_eq!(player.score, 100); // 1個目の取得スコア(spec.md 7章)
        assert_eq!(board.cell(0, target_col), Cell::Empty); // カプセルは消費された
    }

    #[test]
    fn move_lateral_climbing_a_step_into_oxygen_capsule_collects_it() {
        // 1段登った先(row-1)が酸素カプセルの場合も、登りながら取得する(task2)。
        let mut board = empty_board(3);
        let mut player = Player::new();
        player.row = 1;
        let target_col = player.col + 1;
        board.rows[player.row][target_col] = Cell::Rock { hits: 0 }; // 隣は塞がっている
        board.rows[player.row - 1][target_col] = Cell::Oxygen; // 1段上が酸素カプセル

        move_lateral(&mut board, &mut player, Direction::Right); // 1回目: ぶつかって停止
        let outcome = move_lateral(&mut board, &mut player, Direction::Right); // 2回目: 登りながら取得

        assert_eq!(outcome, LateralOutcome::ClimbedStepAndCollectedOxygen);
        assert_eq!(player.row, 0);
        assert_eq!(player.col, target_col);
        assert_eq!(player.oxygen, crate::constants::OXYGEN_MAX); // 既に満タンなのでクランプされる
        assert_eq!(player.score, 100);
        assert_eq!(board.cell(0, target_col), Cell::Empty); // カプセルは消費された
        // 隣の岩ブロックはヒットを受けず、そのまま残る
        assert!(matches!(board.cell(1, target_col), Cell::Rock { hits: 0 }));
    }

    #[test]
    fn move_lateral_into_oxygen_capsule_via_left_direction_also_collects_it() {
        // task2の左右対称性を確認する: Rightだけでなく、Left方向への隣接移動でも
        // 掘削を伴わずAIRカプセルを自動取得できる。
        let mut board = empty_board(3);
        let mut player = Player::new();
        player.oxygen = 40.0;
        let target_col = player.col - 1;
        board.rows[player.row][target_col] = Cell::Oxygen;

        let outcome = move_lateral(&mut board, &mut player, Direction::Left);

        assert_eq!(outcome, LateralOutcome::MovedLevelAndCollectedOxygen);
        assert_eq!(player.col, target_col);
        assert_eq!(player.oxygen, 40.0 + crate::constants::OXYGEN_CAPSULE_RESTORE);
        assert_eq!(player.score, 100);
        assert_eq!(board.cell(0, target_col), Cell::Empty);
    }

    // --- Drill(Space): 移動せず掘削、facing=Downの時だけ降下 ---

    #[test]
    fn drill_facing_resets_the_bumped_direction() {
        // 掘削キーを挟んだ場合は、Left/Rightの「ぶつかって停止中」の状態をリセットして
        // よい(実装者判断、spec.md 1章のTERM独自拡張)。
        let mut board = empty_board(3);
        let mut player = Player::new();
        player.bumped_direction = Some(Direction::Right);

        drill_facing(&mut board, &mut player, &GravityState::new());

        assert_eq!(player.bumped_direction, None);
    }

    #[test]
    fn drill_facing_left_does_not_move_player() {
        let mut board = empty_board(3);
        let mut player = Player::new();
        player.facing = Direction::Left;
        let target_col = player.col - 1;
        board.rows[player.row][target_col] = Cell::Color(ColorKind::Blue);

        let outcome = drill_facing(&mut board, &mut player, &GravityState::new());

        assert_eq!(outcome, DrillOutcome::ColorDestroyed { blocks: 1 });
        assert_eq!(player.col, target_col + 1); // 動いていない(元の位置のまま)
    }

    #[test]
    fn drill_facing_down_clears_the_cell_but_does_not_move_the_player() {
        // ユーザー指摘: 「掘ったからと言って、プレイヤー落下速度が上がってはいけない」。
        // 下方向への掘削はマスを空けるだけで、実際にそこへ進むのは既存の自由落下
        // (apply_player_free_fall、player_fall_tick_msごとの論理ティック)に委ねる。
        let mut board = empty_board(3);
        let mut player = Player::new();
        player.facing = Direction::Down;
        board.rows[player.row + 1][player.col] = Cell::Color(ColorKind::Green);

        let outcome = drill_facing(&mut board, &mut player, &GravityState::new());

        assert_eq!(outcome, DrillOutcome::ColorDestroyed { blocks: 1 });
        assert_eq!(player.row, 0, "掘っただけでは移動しない");
        assert_eq!(board.cell(1, player.col), Cell::Empty);

        // 掘って開けたマスへは、自由落下ティックで自然に進む。
        let fall_outcome = apply_player_free_fall(&mut board, &mut player);
        assert_eq!(fall_outcome, FreeFallOutcome::Fell);
        assert_eq!(player.row, 1);
    }

    #[test]
    fn drill_facing_down_does_not_descend_when_rock_still_intact() {
        let mut board = empty_board(3);
        let mut player = Player::new();
        player.facing = Direction::Down;
        board.rows[player.row + 1][player.col] = Cell::Rock { hits: 0 };

        let outcome = drill_facing(&mut board, &mut player, &GravityState::new());

        assert_eq!(outcome, DrillOutcome::RockHitIntact);
        assert_eq!(player.row, 0); // 降下しない
    }

    #[test]
    fn drill_facing_up_into_empty_has_no_effect() {
        let mut board = empty_board(3);
        let mut player = Player::new();
        player.row = 1;
        player.facing = Direction::Up;

        let outcome = drill_facing(&mut board, &mut player, &GravityState::new());

        assert_eq!(outcome, DrillOutcome::NoEffect);
        assert_eq!(player.row, 1); // Upは移動しない(spec.md 1章)
    }

    // --- ダイヤブロック ---

    #[test]
    fn is_overhead_unstable_crushes_when_diamond_above_is_unsupported() {
        // ダイヤブロックは支えを失って落下中なら上向き掘削で押し潰される。
        let mut board = empty_board(3);
        let mut player = Player::new();
        player.row = 1;
        player.facing = Direction::Up;
        board.rows[0][player.col] = Cell::Diamond; // 直下(row0)を含め周囲は空=未支持

        let outcome = drill_facing(&mut board, &mut player, &GravityState::new());

        assert_eq!(outcome, DrillOutcome::CrushedByUnstableOverhead);
    }

    // --- 酸素自然減少 ---

    #[test]
    fn oxygen_decay_reduces_by_rate_times_delta() {
        let mut player = Player::new();

        apply_oxygen_decay(&mut player, 1.0);

        assert_eq!(player.oxygen, OXYGEN_MAX - OXYGEN_DECAY_PER_SEC);
    }

    // --- 重力ティック: 自動消滅スコア加算・押し潰しの無敵判定 ---

    #[test]
    fn process_gravity_tick_awards_auto_vanish_score() {
        // depth_rows=2: row1が最深行(常に支持)。row0の3個が落下し、あらかじめ最深行に
        // 置いた1個(col3)へ連結して合計4個になり自動消滅する。
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        board.rows[0][1] = Cell::Color(ColorKind::Red);
        board.rows[0][2] = Cell::Color(ColorKind::Red);
        board.rows[1][3] = Cell::Color(ColorKind::Red);
        let mut player = Player::new();
        player.row = 99;
        player.col = 99; // 落下グループから十分離す
        let mut gravity = GravityState::new();

        let result = shake_out_then_process_tick(&mut board, &mut player, &mut gravity, false); // 落下+着地+自動消滅

        assert_eq!(result.auto_vanished_blocks, 4);
        assert_eq!(player.score, 4 * 30);
    }

    #[test]
    fn process_gravity_tick_merges_falling_group_with_supported_same_color_block_directly_below() {
        // 確定事実2(spec.md 4章冒頭)「落下中のブロックは支持されている同色ブロックに
        // 接触すると連結する」を、Game層のAPI(process_gravity_tick)経由で検証する。
        // depth_rows=3: row2に既に連結済みの支持グループ(2個)、row0に連結した落下グループ
        // (2個)を置く。1回の落下ティックで接触・連結し、合計4個で自動消滅・得点加算される。
        let mut board = empty_board(3);
        board.rows[2][0] = Cell::Color(ColorKind::Red);
        board.rows[2][1] = Cell::Color(ColorKind::Red);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        board.rows[0][1] = Cell::Color(ColorKind::Red);
        let mut player = Player::new();
        player.row = 99;
        player.col = 99; // 落下グループから十分離す
        let mut gravity = GravityState::new();

        let result = shake_out_then_process_tick(&mut board, &mut player, &mut gravity, false);

        assert_eq!(result.auto_vanished_blocks, 4);
        assert_eq!(player.score, 4 * 30);
        assert_eq!(board.cell(1, 0), Cell::Empty);
        assert_eq!(board.cell(2, 0), Cell::Empty);
    }

    #[test]
    fn process_gravity_tick_rock_auto_vanishes_without_awarding_score() {
        // ユーザー指摘: 「4個以上結合したらちゃんと消えないといけない」。岩ブロックも
        // 連結・落下・着地すれば自動消滅する(Game層(process_gravity_tick)を通しても
        // 同様)が、得点は加算されない。
        let mut board = empty_board(3);
        board.rows[2][0] = Cell::Rock { hits: 1 };
        board.rows[2][1] = Cell::Rock { hits: 3 };
        board.rows[0][0] = Cell::Rock { hits: 4 }; // あと1発で破壊されるはずだった岩
        board.rows[0][1] = Cell::Rock { hits: 0 };
        let mut player = Player::new();
        player.row = 99;
        player.col = 99;
        let mut gravity = GravityState::new();

        let result = shake_out_then_process_tick(&mut board, &mut player, &mut gravity, false);

        assert_eq!(result.auto_vanished_rock_blocks, 4);
        assert_eq!(result.auto_vanished_blocks, 0);
        assert_eq!(player.score, 0, "岩ブロックの自動消滅はスコア対象外");
        assert_eq!(board.cell(1, 0), Cell::Empty);
        assert_eq!(board.cell(2, 0), Cell::Empty);
    }

    #[test]
    fn process_gravity_tick_crush_is_suppressed_while_invulnerable() {
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        let mut player = Player::new();
        player.row = 1;
        player.col = 0;
        let mut gravity = GravityState::new();

        let result = shake_out_then_process_tick(&mut board, &mut player, &mut gravity, true); // 落下→押し潰しだが無敵中

        assert!(!result.life_lost_to_crush);
        // ブロック自体はその場に残って見える(TERM独自拡張。ユーザー指摘:
        // 「潰れる直前で消えてしまう」)。無敵中でもこの挙動は変わらない。
        assert_eq!(board.cell(1, 0), Cell::Color(ColorKind::Red));
    }

    #[test]
    fn process_gravity_tick_crush_triggers_life_loss_when_not_invulnerable() {
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        let mut player = Player::new();
        player.row = 1;
        player.col = 0;
        let mut gravity = GravityState::new();

        let result = shake_out_then_process_tick(&mut board, &mut player, &mut gravity, false); // 落下→押し潰し

        assert!(result.life_lost_to_crush);
    }

    // --- 支えを失った直後は即座に落下せず、震え時間を経てから落下する(task24) ---

    #[test]
    fn drilling_away_the_support_does_not_cause_an_immediate_fall_it_shakes_first() {
        // 実プレイでの発生源(プレイヤーがdrill_facingで足場を掘削で消す)から検証する。
        // ゲーム開始時点から未支持だったセルの揺れ(board.rsのunsupported_cell_shakes_...)
        // とは別に、「直前まで支持されていたセルが、プレイヤーの掘削によって支えを失った
        // 直後」も同じくSHAKE_TICKSぶん震えてから落下することを確認する。
        let mut board = empty_board(3);
        board.rows[1][0] = Cell::Color(ColorKind::Blue); // これから掘削で消える支え
        board.rows[0][0] = Cell::Color(ColorKind::Red); // 支えの上に乗っているブロック
        let mut player = Player::new();
        player.row = 1;
        player.col = 1;
        player.facing = Direction::Left; // (1,0)を移動せずに掘削する
        let mut gravity = GravityState::new();

        let drill_outcome = drill_facing(&mut board, &mut player, &gravity);
        assert_eq!(drill_outcome, DrillOutcome::ColorDestroyed { blocks: 1 });
        assert_eq!(board.cell(1, 0), Cell::Empty, "支えの掘削は完了している");
        assert_eq!(player.col, 1, "Left方向のDrillはその場から動かない");

        // 支えを失った直後、SHAKE_TICKSぶんはまだ落下しない。
        for _ in 0..SHAKE_TICKS {
            let tick = process_gravity_tick(&mut board, &mut player, &mut gravity, false, SHAKE_TICKS);
            assert_eq!(tick.moved_cells.len(), 0);
        }
        assert_eq!(board.cell(0, 0), Cell::Color(ColorKind::Red), "揺れている間はまだ落下しない");

        // 揺れが明けた次のティックで初めて1マス落下する。
        let tick = process_gravity_tick(&mut board, &mut player, &mut gravity, false, SHAKE_TICKS);
        assert_eq!(tick.moved_cells.len(), 1);
        assert_eq!(board.cell(0, 0), Cell::Empty);
        assert_eq!(board.cell(1, 0), Cell::Color(ColorKind::Red));
    }

    // --- プレイヤー自身の自由落下(TERM独自拡張) ---

    #[test]
    fn apply_player_free_fall_drops_one_row_when_below_is_empty() {
        let mut board = empty_board(5);
        let mut player = Player::new();
        player.row = 2;

        let outcome = apply_player_free_fall(&mut board, &mut player);

        assert_eq!(outcome, FreeFallOutcome::Fell);
        assert_eq!(player.row, 3);
    }

    #[test]
    fn apply_player_free_fall_does_nothing_when_supported() {
        let mut board = empty_board(5);
        let mut player = Player::new();
        player.row = 2;
        board.rows[3][player.col] = Cell::Color(ColorKind::Red);

        let outcome = apply_player_free_fall(&mut board, &mut player);

        assert_eq!(outcome, FreeFallOutcome::DidNotFall);
        assert_eq!(player.row, 2);
    }

    #[test]
    fn apply_player_free_fall_does_nothing_at_the_deepest_row() {
        let mut board = empty_board(3);
        let mut player = Player::new();
        player.row = 2; // 最深行(depth_rows=3 -> row0..2)

        let outcome = apply_player_free_fall(&mut board, &mut player);

        assert_eq!(outcome, FreeFallOutcome::DidNotFall);
        assert_eq!(player.row, 2);
    }

    #[test]
    fn apply_player_free_fall_onto_oxygen_capsule_collects_it_and_restores_oxygen() {
        // ユーザー指摘「AIRがキャラの下にあるとき掘らないと行けないのはバグ」の修正確認。
        // 公式マニュアル "just walk into it" のとおり、掘削しなくても自由落下だけで
        // 酸素カプセルの上に乗れば取得できる。
        let mut board = empty_board(5);
        let mut player = Player::new();
        board.rows[3][player.col] = Cell::Oxygen;
        player.row = 2;
        player.oxygen = 10.0;

        let outcome = apply_player_free_fall(&mut board, &mut player);

        assert_eq!(outcome, FreeFallOutcome::FellAndCollectedOxygen);
        assert_eq!(player.row, 3);
        assert_eq!(board.cell(3, player.col), Cell::Empty, "取得済みの酸素カプセルは消滅する");
        assert_eq!(player.oxygen, 10.0 + crate::constants::OXYGEN_CAPSULE_RESTORE);
    }
}

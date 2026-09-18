//! 連結判定・掘削(spec.md 4章・4.6)。同色の連結グループ・岩ブロックの連結グループの
//! 探索、直接掘削による消滅処理。

use super::*;

/// 盤面上の`start`を起点に、4方向で`same_kind`を満たすセルに連結している全セルを求める
/// 汎用BFS(spec.md 4章)。色ブロックの同色連結・岩ブロックの連結(hitsに関わらず全て
/// 同種とみなす)の両方がこの1つの実装を共有する。
fn connected_group(
    board: &Board,
    start: (usize, usize),
    same_kind: impl Fn(Cell) -> bool,
) -> Vec<(usize, usize)> {
    let depth_rows = board.depth_rows();
    let mut visited: HashSet<(usize, usize)> = HashSet::new();
    let mut stack = vec![start];
    visited.insert(start);
    let mut group = Vec::new();

    while let Some((r, c)) = stack.pop() {
        group.push((r, c));

        let neighbors = [
            (r.wrapping_sub(1), c),
            (r + 1, c),
            (r, c.wrapping_sub(1)),
            (r, c + 1),
        ];
        for (nr, nc) in neighbors {
            if nr >= depth_rows || nc >= board.width() {
                continue;
            }
            if visited.contains(&(nr, nc)) {
                continue;
            }
            if same_kind(board.cell(nr, nc)) {
                visited.insert((nr, nc));
                stack.push((nr, nc));
            }
        }
    }

    group
}

/// 盤面上の`start`を起点に、4方向で`color`に連結している全セルを求める(spec.md 4章)。
/// サイズに関わらず(1個の孤立ブロックでも)全て列挙する。呼び出し側が
/// 「即時消滅(4.6、サイズ問わず)」「自動消滅(4.5、サイズ4以上のみ)」を使い分ける。
pub fn connected_same_color(
    board: &Board,
    start: (usize, usize),
    color: ColorKind,
) -> Vec<(usize, usize)> {
    connected_group(board, start, |cell| cell == Cell::Color(color))
}

/// 盤面上の`start`を起点に、4方向で連結している岩ブロック(Xブロック)を全て求める
/// (spec.md 4.1・4.9)。個々のセルの`hits`値に関わらず、岩ブロックであれば全て同種として
/// 連結対象になる(色ブロックの「同色」に相当する条件が岩ブロックでは「岩であること」)。
pub fn connected_rock_group(board: &Board, start: (usize, usize)) -> Vec<(usize, usize)> {
    connected_group(board, start, |cell| matches!(cell, Cell::Rock { .. }))
}

/// プレイヤーが色ブロックを直接掘削した際の即時消滅処理(spec.md 4.6)。掘削セルを起点に
/// 4方向連結の同色グループをサイズに関わらず全体消滅させる。色ブロック以外なら0を返す。
/// 戻り値は消滅させたブロック数(呼び出し側が「消滅数 × 10点」を加算する。spec.md 7章)。
pub fn drill_color_block(board: &mut Board, target: (usize, usize)) -> usize {
    let Cell::Color(color) = board.cell(target.0, target.1) else {
        return 0;
    };
    let group = connected_same_color(board, target, color);
    for &(r, c) in &group {
        board.set(r, c, Cell::Empty);
    }
    group.len()
}

/// 岩ブロックへの1ヒットの結果(spec.md 2章・4章・6章)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RockHitResult {
    /// 5回未満のヒットで、まだ破壊に至らない(セルの内容はそのまま、ヒット数だけ進む)
    StillIntact,
    /// 累積5回目のヒットで破壊された。連結していても消えるのはヒットした1ブロックのみ。
    /// `blocks`は消滅した総数(現仕様では常に1)
    Destroyed { blocks: usize },
}

/// 岩ブロックへ1ヒット加える。`target`が岩ブロックでない場合はNoneを返す。
/// 5回目のヒットで破壊されるのはヒットしたそのセル1個のみ(連結した岩は巻き込まれない)。
/// 酸素ペナルティは呼び出し側が1回だけ適用する。
pub fn hit_rock(board: &mut Board, target: (usize, usize)) -> Option<RockHitResult> {
    let Cell::Rock { hits } = board.cell(target.0, target.1) else {
        return None;
    };
    let hits = hits + 1;
    if hits >= ROCK_HITS_TO_BREAK {
        // 岩ブロックは色ブロックと異なり、連結していても消えるのはヒットした1ブロックのみ。
        board.set(target.0, target.1, Cell::Empty);
        Some(RockHitResult::Destroyed { blocks: 1 })
    } else {
        board.set(target.0, target.1, Cell::Rock { hits });
        Some(RockHitResult::StillIntact)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::empty_board;
    use super::*;

    // --- 直接掘削による即時消滅(4.6、サイズ問わず) ---

    #[test]
    fn drill_color_block_removes_whole_connected_group_regardless_of_size() {
        let mut board = empty_board(3);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        board.rows[0][1] = Cell::Color(ColorKind::Red);
        board.rows[1][0] = Cell::Color(ColorKind::Red);
        board.rows[0][2] = Cell::Color(ColorKind::Blue); // 別グループ

        let removed = drill_color_block(&mut board, (0, 0));

        assert_eq!(removed, 3);
        assert_eq!(board.cell(0, 0), Cell::Empty);
        assert_eq!(board.cell(0, 1), Cell::Empty);
        assert_eq!(board.cell(1, 0), Cell::Empty);
        assert_eq!(board.cell(0, 2), Cell::Color(ColorKind::Blue)); // 別グループは影響なし
    }

    #[test]
    fn drill_color_block_removes_a_single_isolated_block_alone() {
        // 孤立ブロック(4方向に同色隣接なし)を掘削すると、自分1個だけが消える(spec.md 4.6)。
        let mut board = empty_board(3);
        board.rows[1][1] = Cell::Color(ColorKind::Red); // 孤立
        board.rows[0][1] = Cell::Color(ColorKind::Blue);
        board.rows[2][1] = Cell::Color(ColorKind::Green);
        board.rows[1][0] = Cell::Color(ColorKind::Yellow);
        // rows[1][2] はEmptyのまま

        let removed = drill_color_block(&mut board, (1, 1));

        assert_eq!(removed, 1);
        assert_eq!(board.cell(1, 1), Cell::Empty);
        // 別色の隣接ブロックは影響を受けない
        assert_eq!(board.cell(0, 1), Cell::Color(ColorKind::Blue));
        assert_eq!(board.cell(2, 1), Cell::Color(ColorKind::Green));
        assert_eq!(board.cell(1, 0), Cell::Color(ColorKind::Yellow));
    }

    #[test]
    fn drill_color_block_on_non_color_cell_does_nothing() {
        let mut board = empty_board(3);
        board.rows[0][0] = Cell::Rock { hits: 0 };

        let removed = drill_color_block(&mut board, (0, 0));

        assert_eq!(removed, 0);
        assert_eq!(board.cell(0, 0), Cell::Rock { hits: 0 });
    }

    // --- 岩ブロック: 5回目のヒットで破壊(spec.md 2章・4.9) ---

    #[test]
    fn rock_breaks_on_fifth_hit() {
        let mut board = empty_board(1);
        board.rows[0][0] = Cell::Rock { hits: 0 };

        for _ in 0..4 {
            let result = hit_rock(&mut board, (0, 0)).unwrap();
            assert_eq!(result, RockHitResult::StillIntact);
        }
        assert!(matches!(board.cell(0, 0), Cell::Rock { hits: 4 }));

        let result = hit_rock(&mut board, (0, 0)).unwrap();
        assert_eq!(result, RockHitResult::Destroyed { blocks: 1 }); // 単独なので1個だけ消える
        assert_eq!(board.cell(0, 0), Cell::Empty);
    }

    #[test]
    fn rock_break_on_fifth_hit_vanishes_only_the_hit_block() {
        // ユーザー指摘: 「Xブロックは結合してても全体が消えるのではなく1ブロックしか
        // 消せないものとする」。連結している他の岩ブロックは、hitsに関わらず影響を
        // 受けずそのまま残る(色ブロックとは違うルール)。
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Rock {
            hits: ROCK_HITS_TO_BREAK - 1,
        }; // あと1発で破壊
        board.rows[0][1] = Cell::Rock { hits: 0 }; // 連結していても巻き込まれない
        board.rows[1][0] = Cell::Rock { hits: 2 }; // 同上
        board.rows[0][2] = Cell::Color(ColorKind::Red); // 別種、巻き込まれない

        let result = hit_rock(&mut board, (0, 0)).unwrap();

        assert_eq!(result, RockHitResult::Destroyed { blocks: 1 });
        assert_eq!(board.cell(0, 0), Cell::Empty);
        assert_eq!(
            board.cell(0, 1),
            Cell::Rock { hits: 0 },
            "連結していた岩は影響を受けない"
        );
        assert_eq!(
            board.cell(1, 0),
            Cell::Rock { hits: 2 },
            "連結していた岩は影響を受けない"
        );
        assert_eq!(board.cell(0, 2), Cell::Color(ColorKind::Red)); // 色ブロックは無関係
    }

    #[test]
    fn connected_rock_group_ignores_hit_count_differences() {
        let mut board = empty_board(1);
        board.rows[0][0] = Cell::Rock { hits: 0 };
        board.rows[0][1] = Cell::Rock { hits: 3 };
        board.rows[0][2] = Cell::Color(ColorKind::Blue); // ここで途切れる

        let group = connected_rock_group(&board, (0, 0));

        assert_eq!(group.len(), 2);
        assert!(group.contains(&(0, 0)));
        assert!(group.contains(&(0, 1)));
    }
}

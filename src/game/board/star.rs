//! スターブロックの実時間溶解(spec.md 2章・4章、TERM独自拡張)。

use super::*;

/// 画面内(行±`STAR_VISIBLE_RANGE_ROWS`)のスターブロックの表示経過時間を実時間`delta_ms`
/// ぶん進める。`STAR_VISIBLE_GRACE_MS`までは無傷、以後`STAR_MELT_DURATION_MS`かけて溶けて
/// 消える。画面外は進まない。戻り値は消滅したスターの座標と直前のセル内容(デバッグログ用)。
pub fn tick_star_melting(board: &mut Board, player_row: usize, delta_ms: u32) -> Vec<(Pos, Cell)> {
    let range = crate::constants::STAR_VISIBLE_RANGE_ROWS;
    let row_start = player_row.saturating_sub(range);
    let row_end = (player_row + range).min(board.depth_rows().saturating_sub(1));
    let mut melted = Vec::new();
    let vanish_at_ms = STAR_VISIBLE_GRACE_MS + STAR_MELT_DURATION_MS;

    let width = board.width();
    for r in row_start..=row_end {
        for c in 0..width {
            if let Cell::Star { visible_ms } = board.cell(r, c) {
                let updated = visible_ms.saturating_add(delta_ms);
                if updated >= vanish_at_ms {
                    board.set(r, c, Cell::Empty);
                    melted.push((
                        (r, c),
                        Cell::Star {
                            visible_ms: updated,
                        },
                    ));
                } else {
                    board.set(
                        r,
                        c,
                        Cell::Star {
                            visible_ms: updated,
                        },
                    );
                }
            }
        }
    }

    melted
}

#[cfg(test)]
mod tests {
    use super::super::tests::empty_board;
    use super::*;

    // --- スターブロックの実時間溶解 ---

    #[test]
    fn tick_star_melting_leaves_the_star_intact_within_the_grace_period() {
        // 猶予時間(STAR_VISIBLE_GRACE_MS)未満しか経過していなければ、画面内であっても
        // 溶解が始まらない(セルが残る)ことを確認する。
        let mut board = empty_board(1);
        board.rows[0][0] = Cell::Star { visible_ms: 0 };

        let melted = tick_star_melting(&mut board, 0, STAR_VISIBLE_GRACE_MS - 1);

        assert_eq!(melted.len(), 0, "猶予時間未満では消滅しないはず");
        assert!(
            matches!(board.cell(0, 0), Cell::Star { .. }),
            "猶予時間未満ではまだスターのままのはず"
        );
    }

    #[test]
    fn tick_star_melting_vanishes_after_grace_period_plus_melt_duration_elapses() {
        let mut board = empty_board(1);
        board.rows[0][0] = Cell::Star { visible_ms: 0 };

        let melted =
            tick_star_melting(&mut board, 0, STAR_VISIBLE_GRACE_MS + STAR_MELT_DURATION_MS);

        assert_eq!(
            melted,
            vec![(
                (0, 0),
                Cell::Star {
                    visible_ms: STAR_VISIBLE_GRACE_MS + STAR_MELT_DURATION_MS
                }
            )],
            "猶予時間+溶解時間が経過すれば1個消えるはず"
        );
        assert_eq!(
            board.cell(0, 0),
            Cell::Empty,
            "溶け切ったスターは消えているはず"
        );
    }

    #[test]
    fn tick_star_melting_ignores_stars_outside_the_visible_range() {
        // プレイヤーの画面外(行±STAR_VISIBLE_RANGE_ROWS)にあるスターブロックは
        // 経過時間が進まないことを確認する。
        let range = crate::constants::STAR_VISIBLE_RANGE_ROWS;
        let far_row = range + 10;
        let mut board = empty_board(far_row + 1);
        board.rows[far_row][0] = Cell::Star { visible_ms: 0 };

        let melted = tick_star_melting(
            &mut board,
            0,
            STAR_VISIBLE_GRACE_MS + STAR_MELT_DURATION_MS + 1000,
        );

        assert_eq!(melted.len(), 0, "画面外のスターは溶解が進まないはず");
        assert_eq!(
            board.cell(far_row, 0),
            Cell::Star { visible_ms: 0 },
            "経過時間が進んでいないはず"
        );
    }
}

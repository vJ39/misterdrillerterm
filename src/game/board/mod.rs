//! フィールド生成・ブロック配置・連結判定・重力落下ロジック(spec.md 2〜4章)。
//!
//! ratatui/crossterm/rodio の副作用を一切持たない純粋なデータ構造・関数のみで構成する。

use std::collections::HashMap;
use std::collections::HashSet;

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;

use crate::constants::{
    COLOR_CLUSTER_DEPTH_START_PROB, ROCK_CLUSTER_DEPTH_MAX_BONUS, ROCK_HITS_TO_BREAK,
    STAR_MELT_DURATION_MS, STAR_VISIBLE_GRACE_MS, depth_fraction,
};

mod connect;
mod generate;
mod gravity;
mod star;

pub use connect::*;
pub use gravity::*;
pub use star::*;

/// フィールド1マスの内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    Empty,
    Color(ColorKind),
    /// 岩ブロック(Xブロック)。hitsは累積ヒット数(0〜4)で、5回目のヒットで破壊される。
    /// 落下・着地してもhitsは保持されたまま移動する(spec.md 2章・4章・4.8)。
    Rock {
        hits: u8,
    },
    Oxygen,
    /// ダイヤブロック。
    Diamond,
    /// スターブロック。可視範囲に入ると`STAR_VISIBLE_GRACE_MS`は無傷のまま、その後
    /// `STAR_MELT_DURATION_MS`かけて溶けて消える。`visible_ms`は画面内に入ってからの経過
    /// 時間(ms)。落下tick間隔は深度で変わるため、tick数でなく実時間で数える。連結対象外。
    Star {
        visible_ms: u32,
    },
    /// アイテムブロック。ドリルで取得すると対応するショートカットと同じ効果が即発動する。
    /// ダイヤ・スター同様、連結せず常に単独の塊として落下する。
    Item(ItemEffect),
}

/// アイテムブロック取得時に発動する効果の種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemEffect {
    /// ショートカットRと同じ: プレイヤーより上のブロックを全削除する(AIRは残す)
    ClearAbove,
    /// ショートカットCと同じ: プレイヤー付近の色ブロックをランダムな2色に統一する
    UnifyColors,
    /// ショートカットKと同じ: 画面内のXブロック・ダイヤブロックを100%スター化する
    StarifyScreen,
}

/// 「触れるだけで取得できる」セル(AIR・アイテムブロック)を実際に取得した内容
/// (TERM独自拡張)。段差登りのように1回の入力で複数マスを通過しうる経路では、
/// どのマスで何を取得したかを取りまとめて呼び出し側へ返すために使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pickup {
    /// 酸素カプセル(酸素+50・スコア加算は取得時点で適用済み)
    Oxygen,
    /// アイテムブロック。効果の発動自体は`Game`(呼び出し側)が行う
    Item(ItemEffect),
}

/// 色ブロックの色種別。初代は4色(赤・青・緑・黄)。紫は存在しない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorKind {
    Red,
    Blue,
    Green,
    Yellow,
}

impl ColorKind {
    /// 全色種の一覧(生成時の走査・UIパレット参照に使う)。
    pub const ALL: [ColorKind; 4] = [
        ColorKind::Red,
        ColorKind::Blue,
        ColorKind::Green,
        ColorKind::Yellow,
    ];
}

/// ゲームフィールド全体(1000行×`width`列)。`width`(列数)は設定で変更可能で、新規ゲーム
/// 開始時に決まり以後そのゲームの間は固定(`rows`各要素の長さと必ず一致する)。
#[derive(Debug, Clone)]
pub struct Board {
    pub rows: Vec<Vec<Cell>>,
    pub width: usize,
}

impl Board {
    pub fn depth_rows(&self) -> usize {
        self.rows.len()
    }

    /// フィールド幅(列数)。新規ゲーム開始時に決まり、以後そのゲームの間は固定。
    pub fn width(&self) -> usize {
        self.width
    }

    pub fn cell(&self, row: usize, col: usize) -> Cell {
        self.rows[row][col]
    }

    /// `cell`の、盤面外なら`None`を返す版(TERM独自拡張。#218)。オートプレイが
    /// 最深行の直下や盤面端の外側を見る際、毎回境界チェックを書かずに済ませる。
    pub fn cell_or_none(&self, row: usize, col: usize) -> Option<Cell> {
        (row < self.depth_rows() && col < self.width()).then(|| self.cell(row, col))
    }

    pub fn set(&mut self, row: usize, col: usize, cell: Cell) {
        self.rows[row][col] = cell;
    }

    /// 盤面全体(全行)に存在する、指定した効果のアイテムブロックの個数。
    /// 出現数の上限判定に使う。
    fn count_item(&self, effect: ItemEffect) -> usize {
        self.rows
            .iter()
            .flatten()
            .filter(|c| matches!(c, Cell::Item(e) if *e == effect))
            .count()
    }

    /// `from_row..to_row`(to_row自体は含まない)の範囲に存在する、指定した効果の
    /// アイテムブロックの個数。窓単位の上限判定(`top_up_items`)専用。
    fn count_item_in_range(&self, effect: ItemEffect, from_row: usize, to_row: usize) -> usize {
        self.rows[from_row..to_row.min(self.rows.len())]
            .iter()
            .flatten()
            .filter(|c| matches!(c, Cell::Item(e) if *e == effect))
            .count()
    }
}

/// ボムの爆風が届くセル(原点を含む)を計算する。上下へ`row_range`・左右へ`col_range`マス
/// ずつ伸ばす(軸ごとに画面内全域の大きさが異なるため距離を分離)。盤面外へは伸びない。
/// 途中の岩・ダイヤで遮蔽されず、range・盤面境界まで届く(ショートカットKと同じ挙動)。
pub fn bomb_blast_cells(
    board: &Board,
    origin: Pos,
    row_range: usize,
    col_range: usize,
) -> Vec<Pos> {
    let mut cells = vec![origin];
    let deltas: [(isize, isize, usize); 4] = [
        (-1, 0, row_range),
        (1, 0, row_range),
        (0, -1, col_range),
        (0, 1, col_range),
    ];
    for (dr, dc, range) in deltas {
        let mut r = origin.0 as isize;
        let mut c = origin.1 as isize;
        for _ in 0..range {
            r += dr;
            c += dc;
            if r < 0 || c < 0 || r as usize >= board.depth_rows() || c as usize >= board.width() {
                break;
            }
            cells.push((r as usize, c as usize));
        }
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::FIELD_WIDTH_DEFAULT as FIELD_WIDTH;

    pub(super) fn empty_board(rows: usize) -> Board {
        Board {
            rows: vec![vec![Cell::Empty; FIELD_WIDTH]; rows],
            width: FIELD_WIDTH,
        }
    }

    // --- ボム爆発の爆風範囲 ---

    #[test]
    fn bomb_blast_cells_reaches_full_range_through_empty_cells() {
        let board = empty_board(10);
        let cells = bomb_blast_cells(&board, (5, 5), 2, 2);
        // 原点 + 上下左右2マスずつ = 9セル、全てEmptyなので途中で止まらないはず。
        assert_eq!(cells.len(), 9);
        for pos in [
            (3, 5),
            (4, 5),
            (5, 5),
            (6, 5),
            (7, 5),
            (5, 3),
            (5, 4),
            (5, 6),
            (5, 7),
        ] {
            assert!(cells.contains(&pos), "{pos:?}が爆風範囲に含まれるはず");
        }
    }

    #[test]
    fn bomb_blast_cells_passes_through_rock_without_stopping() {
        // ショートカットK/StarifyScreenアイテムと同じく、途中の岩で打ち切らず
        // 指定範囲の端まで届くはず。
        let mut board = empty_board(10);
        board.rows[5][7] = Cell::Rock { hits: 0 }; // 原点(5,5)から右へ2マス目
        let cells = bomb_blast_cells(&board, (5, 5), 2, 2);
        assert!(cells.contains(&(5, 6)), "岩の手前のマスは含まれるはず");
        assert!(cells.contains(&(5, 7)), "岩自体のマスは含まれるはず");
        assert!(
            !cells.contains(&(5, 8)),
            "range(2)を超えた先は含まれないはず(岩に遮られたからではない)"
        );
    }

    #[test]
    fn bomb_blast_cells_passes_through_diamond_the_same_way_as_rock() {
        let mut board = empty_board(10);
        board.rows[4][5] = Cell::Diamond; // 原点(5,5)から上へ1マス目
        let cells = bomb_blast_cells(&board, (5, 5), 2, 2);
        assert!(cells.contains(&(4, 5)), "ダイヤ自体のマスは含まれるはず");
        assert!(
            cells.contains(&(3, 5)),
            "ダイヤの先(range内)へも爆風が伸びるはず"
        );
    }

    #[test]
    fn bomb_blast_cells_reaches_the_screen_edge_through_a_solid_wall_of_rock_and_diamond() {
        // 連続した岩・ダイヤの壁があっても、遮蔽されずrangeの端(画面端)まで届くはず。
        let mut board = empty_board(10);
        for col in 6..=9 {
            board.rows[5][col] = if col % 2 == 0 {
                Cell::Rock { hits: 0 }
            } else {
                Cell::Diamond
            };
        }
        let cells = bomb_blast_cells(&board, (5, 5), 0, 4);
        for col in 6..=9 {
            assert!(
                cells.contains(&(5, col)),
                "岩・ダイヤの壁の途中セル({col})も爆風に含まれるはず"
            );
        }
    }

    #[test]
    fn bomb_blast_cells_does_not_go_out_of_bounds() {
        let board = empty_board(3);
        let cells = bomb_blast_cells(&board, (0, 0), 2, 2);
        for &(r, c) in &cells {
            assert!(
                r < board.depth_rows() && c < board.width(),
                "盤面外セル{:?}が含まれている",
                (r, c)
            );
        }
    }

    #[test]
    fn bomb_blast_cells_applies_row_range_and_col_range_independently() {
        // 縦(row_range)と横(col_range)で異なる距離がそれぞれ独立に適用されるはず。
        // 盤面境界には届かない距離にして、範囲自体が効いていることを確認する。
        let board = empty_board(20);
        let cells = bomb_blast_cells(&board, (10, 5), 3, 2);
        assert!(cells.contains(&(7, 5)), "row_range=3の上端は含まれるはず");
        assert!(
            !cells.contains(&(6, 5)),
            "row_range=3を超えた先は含まれないはず"
        );
        assert!(cells.contains(&(10, 7)), "col_range=2の右端は含まれるはず");
        assert!(
            !cells.contains(&(10, 8)),
            "col_range=2を超えた先は含まれないはず"
        );
    }
}

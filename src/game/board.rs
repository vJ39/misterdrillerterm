//! フィールド生成・ブロック配置・連結判定・重力落下ロジック(spec.md 2〜4章)。
//!
//! ratatui/crossterm/rodio の副作用を一切持たない純粋なデータ構造・関数のみで構成する。
//! 次フェーズのユニットテスト対象。

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;

use crate::constants::FIELD_WIDTH;

/// フィールド1マスの内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    Empty,
    Color(ColorKind),
    Rock,
    Oxygen,
    Diamond,
}

/// 色ブロックの色種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorKind {
    Red,
    Blue,
    Green,
    Yellow,
    Purple,
}

impl ColorKind {
    /// 全色種の一覧(生成時の走査・UIパレット参照に使う)。
    pub const ALL: [ColorKind; 5] = [
        ColorKind::Red,
        ColorKind::Blue,
        ColorKind::Green,
        ColorKind::Yellow,
        ColorKind::Purple,
    ];
}

/// 深度帯ごとの出現確率テーブル(spec.md 3章)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandTable {
    pub rock: f32,
    pub color_each: f32,
    pub oxygen: f32,
    pub diamond: f32,
}

/// 行インデックス(=深度[m]、spec.md 2章)から出現確率テーブルを引く。
///
/// 注意: ここでの「行インデックス=深度」は spec.md 2章のフィールド定義に基づく値で、
/// `Player::depth_m()`(= row + 1、クリア条件が深度1000mちょうどになるよう+1している)
/// とは意味が異なる。本関数の引数は常に「行インデックスそのもの」を渡すこと。
pub fn band_table(row: usize) -> BandTable {
    match row {
        0..=199 => BandTable {
            rock: 0.05,
            color_each: 0.18,
            oxygen: 0.03,
            diamond: 0.02,
        },
        200..=399 => BandTable {
            rock: 0.08,
            color_each: 0.17,
            oxygen: 0.04,
            diamond: 0.03,
        },
        400..=599 => BandTable {
            rock: 0.12,
            color_each: 0.16,
            oxygen: 0.04,
            diamond: 0.04,
        },
        600..=799 => BandTable {
            rock: 0.16,
            color_each: 0.15,
            oxygen: 0.04,
            diamond: 0.05,
        },
        _ => BandTable {
            rock: 0.20,
            color_each: 0.14,
            oxygen: 0.04,
            diamond: 0.06,
        },
    }
}

/// 累積分布に従い、乱数値rからセル種別を1つ選ぶ。
fn pick_cell(r: f32, t: &BandTable) -> Cell {
    let mut acc = 0.0f32;
    acc += t.rock;
    if r < acc {
        return Cell::Rock;
    }
    for color in ColorKind::ALL {
        acc += t.color_each;
        if r < acc {
            return Cell::Color(color);
        }
    }
    acc += t.oxygen;
    if r < acc {
        return Cell::Oxygen;
    }
    // 残り(浮動小数の誤差でaccが1.0未満に留まる場合の受け皿も兼ねる)はダイヤ
    Cell::Diamond
}

/// 行インデックスrowの行を1行分生成する。
///
/// row < 2 (先頭2行、開始地点の安全確保)は常にEmpty固定。
pub fn generate_row(rng: &mut ChaCha8Rng, row: usize) -> [Cell; FIELD_WIDTH] {
    if row < 2 {
        return [Cell::Empty; FIELD_WIDTH];
    }
    let t = band_table(row);
    let mut row = [Cell::Empty; FIELD_WIDTH];
    for cell in row.iter_mut() {
        let r: f32 = rng.random_range(0.0..1.0);
        *cell = pick_cell(r, &t);
    }
    row
}

/// ゲームフィールド全体(1000行×12列)。
#[derive(Debug, Clone)]
pub struct Board {
    pub rows: Vec<[Cell; FIELD_WIDTH]>,
}

impl Board {
    /// 乱数シードから深さ FIELD_DEPTH_M 行ぶんのフィールドを事前生成する。
    pub fn generate(seed: u64, depth_rows: usize) -> Self {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let rows = (0..depth_rows).map(|depth| generate_row(&mut rng, depth)).collect();
        Board { rows }
    }

    pub fn depth_rows(&self) -> usize {
        self.rows.len()
    }

    pub fn cell(&self, row: usize, col: usize) -> Cell {
        self.rows[row][col]
    }
}

/// 1回の重力ティックの結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FallTickOutcome {
    /// このティックで実際に1マス落下したグループの数。
    pub groups_moved: usize,
    /// 落下してきたグループがプレイヤー位置に重なった(=押し潰された)かどうか。
    pub crushed: bool,
}

/// 盤面上の同色4方向連結成分(3個以上)を全て求める。
///
/// 毎ティック全マス走査するシンプルな実装(12列×depth_rows行、計12,000セル規模なら
/// 150ms間隔のティックで十分に軽量)。spec.md 4章の「変化のあった行の周辺のみに限定する」
/// という最適化は将来必要になった場合の拡張余地として残す。
fn find_color_groups(board: &Board) -> Vec<Vec<(usize, usize)>> {
    let rows = board.rows.len();
    let mut visited = vec![[false; FIELD_WIDTH]; rows];
    let mut groups = Vec::new();

    for r in 0..rows {
        for c in 0..FIELD_WIDTH {
            if visited[r][c] {
                continue;
            }
            let Cell::Color(color) = board.rows[r][c] else {
                visited[r][c] = true;
                continue;
            };

            let mut stack = vec![(r, c)];
            visited[r][c] = true;
            let mut group = Vec::new();

            while let Some((cr, cc)) = stack.pop() {
                group.push((cr, cc));

                let neighbors = [
                    (cr.wrapping_sub(1), cc),
                    (cr + 1, cc),
                    (cr, cc.wrapping_sub(1)),
                    (cr, cc + 1),
                ];
                for (nr, nc) in neighbors {
                    if nr >= rows || nc >= FIELD_WIDTH {
                        continue;
                    }
                    if visited[nr][nc] {
                        continue;
                    }
                    if board.rows[nr][nc] == Cell::Color(color) {
                        visited[nr][nc] = true;
                        stack.push((nr, nc));
                    }
                }
            }

            if group.len() >= 3 {
                groups.push(group);
            }
        }
    }

    groups
}

/// グループが1マス下へ落下可能か(=グループの全セルについて、直下がEmptyまたは
/// 同一グループ内のセルであるか)を判定する。
///
/// 4方向連結は列内で非連続な形状(U字・S字・「コ」の字等)を許すため、
/// 「各列の最下段マスだけ」を見ると、列の途中(非最下段)にあるセルの直下チェックが
/// 漏れる。漏れたセルの直下に岩・別グループ等の障害物があっても検出できず、
/// `move_group_down` がそれを無条件に上書きしてしまう(=障害物が静かに消滅する)。
/// これを避けるため、グループの全セルについて直下マスを検証する。
///
/// プレイヤーの現在位置は常にEmpty(掘削済みの空洞)であるため、
/// 「プレイヤー位置は空洞として扱う」という仕様(spec.md 4章手順4)はここでの
/// Cell::Empty判定に自然に含まれる(プレイヤーは非Empty状態のマスにはいられないため)。
fn can_fall(board: &Board, group: &[(usize, usize)]) -> bool {
    let depth_rows = board.rows.len();
    let group_set: std::collections::HashSet<(usize, usize)> = group.iter().copied().collect();

    for &(r, c) in group {
        let nr = r + 1;
        if nr >= depth_rows {
            // フィールド最深部を超える=着地済み
            return false;
        }
        if group_set.contains(&(nr, c)) {
            // 直下は同一グループ内のセル。グループごと一緒に落下するので支持とはみなさない。
            continue;
        }
        if board.rows[nr][c] != Cell::Empty {
            return false;
        }
    }
    true
}

/// グループを1マス下へ移動させる。移動後の占有マスにplayer_posが含まれていれば
/// 押し潰し(crushed)と判定する。
fn move_group_down(board: &mut Board, group: &[(usize, usize)], player_pos: (usize, usize)) -> bool {
    let color = match board.rows[group[0].0][group[0].1] {
        Cell::Color(c) => c,
        _ => unreachable!("find_color_groups only returns Color cell groups"),
    };

    // 先に全ての旧位置をEmptyにしてから新位置へ書き込む(グループ内の縦連結時の
    // 上書き事故を避けるため)。
    for &(r, c) in group {
        board.rows[r][c] = Cell::Empty;
    }

    let mut crushed = false;
    for &(r, c) in group {
        let nr = r + 1;
        board.rows[nr][c] = Cell::Color(color);
        if (nr, c) == player_pos {
            crushed = true;
        }
    }
    crushed
}

/// 論理ティック1回ぶんの重力落下処理(spec.md 4章・5章)を実行する。
///
/// - 対象グループの識別(連結成分の抽出)はティック開始時点の盤面から1回だけ行う
/// - 処理順序は浅い深度(行番号が小さい)側から。これにより下段グループが先に動いて
///   隙間を作り、その結果上段グループも同ティックで連鎖的に動いてしまう事故を防ぐ
///   (spec.md 4章 手順6)
/// - 落下先がプレイヤー位置と重なった場合、その瞬間に押し潰し(crushed)を確定する
pub fn apply_gravity_tick(board: &mut Board, player_pos: (usize, usize)) -> FallTickOutcome {
    let mut groups = find_color_groups(board);
    groups.sort_by_key(|g| g.iter().map(|&(r, _)| r).min().unwrap_or(0));

    let mut outcome = FallTickOutcome::default();

    for group in &groups {
        if can_fall(board, group) {
            let crushed = move_group_down(board, group, player_pos);
            outcome.groups_moved += 1;
            if crushed {
                outcome.crushed = true;
                // 押し潰し確定=即ミスなので、以降のグループ処理を続ける実益はない。
                break;
            }
        }
    }

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_rows(n: usize) -> Vec<[Cell; FIELD_WIDTH]> {
        vec![[Cell::Empty; FIELD_WIDTH]; n]
    }

    // --- 連結判定 ---

    #[test]
    fn find_color_groups_requires_four_directional_adjacency_not_diagonal() {
        let mut rows = empty_rows(2);
        rows[0][0] = Cell::Color(ColorKind::Purple);
        rows[1][1] = Cell::Color(ColorKind::Purple); // 斜め隣接のみ
        let board = Board { rows };
        let groups = find_color_groups(&board);
        assert!(
            groups.is_empty(),
            "斜め隣接は連結とみなさず、孤立セルは3個未満なのでグループ扱いされない"
        );
    }

    #[test]
    fn find_color_groups_includes_groups_with_exactly_three_cells() {
        let mut rows = empty_rows(2);
        rows[0][0] = Cell::Color(ColorKind::Purple);
        rows[0][1] = Cell::Color(ColorKind::Purple);
        rows[1][0] = Cell::Color(ColorKind::Purple);
        let board = Board { rows };
        let groups = find_color_groups(&board);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 3);
    }

    #[test]
    fn find_color_groups_excludes_groups_with_fewer_than_three_cells() {
        let mut rows = empty_rows(2);
        rows[0][0] = Cell::Color(ColorKind::Red);
        rows[0][1] = Cell::Color(ColorKind::Red);
        let board = Board { rows };
        let groups = find_color_groups(&board);
        assert!(groups.is_empty());
    }

    // --- 正常系: 3個以上連結した同色ブロックの落下 ---

    #[test]
    fn connected_group_of_three_or_more_falls_into_hole_below() {
        let mut rows = empty_rows(3);
        rows[0][0] = Cell::Color(ColorKind::Red);
        rows[0][1] = Cell::Color(ColorKind::Red);
        rows[0][2] = Cell::Color(ColorKind::Red);
        let mut board = Board { rows };

        let outcome = apply_gravity_tick(&mut board, (2, 11));

        assert_eq!(outcome.groups_moved, 1);
        assert!(!outcome.crushed);
        assert_eq!(board.rows[0][0], Cell::Empty);
        assert_eq!(board.rows[0][1], Cell::Empty);
        assert_eq!(board.rows[0][2], Cell::Empty);
        assert_eq!(board.rows[1][0], Cell::Color(ColorKind::Red));
        assert_eq!(board.rows[1][1], Cell::Color(ColorKind::Red));
        assert_eq!(board.rows[1][2], Cell::Color(ColorKind::Red));
    }

    // --- エッジケース: 連結が2つ以下(3未満)の場合は落下しない ---

    #[test]
    fn group_of_two_does_not_fall_even_with_hole_below() {
        let mut rows = empty_rows(3);
        rows[0][0] = Cell::Color(ColorKind::Blue);
        rows[0][1] = Cell::Color(ColorKind::Blue);
        let mut board = Board { rows };

        let outcome = apply_gravity_tick(&mut board, (2, 11));

        assert_eq!(outcome.groups_moved, 0);
        assert_eq!(board.rows[0][0], Cell::Color(ColorKind::Blue));
        assert_eq!(board.rows[0][1], Cell::Color(ColorKind::Blue));
        assert_eq!(board.rows[1][0], Cell::Empty);
        assert_eq!(board.rows[1][1], Cell::Empty);
    }

    #[test]
    fn single_isolated_color_cell_does_not_fall() {
        let mut rows = empty_rows(3);
        rows[0][5] = Cell::Color(ColorKind::Green);
        let mut board = Board { rows };

        let outcome = apply_gravity_tick(&mut board, (2, 11));

        assert_eq!(outcome.groups_moved, 0);
        assert_eq!(board.rows[0][5], Cell::Color(ColorKind::Green));
    }

    // --- エッジケース: 岩ブロックは連結落下ロジックの対象外(固定される) ---

    #[test]
    fn rock_blocks_never_form_groups_and_never_fall() {
        let mut rows = empty_rows(3);
        rows[0][0] = Cell::Rock;
        let mut board = Board { rows };

        let outcome = apply_gravity_tick(&mut board, (2, 11));

        assert_eq!(outcome.groups_moved, 0);
        assert_eq!(board.rows[0][0], Cell::Rock);
        assert_eq!(board.rows[1][0], Cell::Empty);
    }

    #[test]
    fn group_blocked_by_rock_below_does_not_fall() {
        let mut rows = empty_rows(3);
        rows[0][0] = Cell::Color(ColorKind::Yellow);
        rows[0][1] = Cell::Color(ColorKind::Yellow);
        rows[0][2] = Cell::Color(ColorKind::Yellow);
        rows[1][1] = Cell::Rock; // 中央列の直下を岩でふさぐ
        let mut board = Board { rows };

        let outcome = apply_gravity_tick(&mut board, (2, 11));

        assert_eq!(outcome.groups_moved, 0);
        assert_eq!(board.rows[0][0], Cell::Color(ColorKind::Yellow));
        assert_eq!(board.rows[0][1], Cell::Color(ColorKind::Yellow));
        assert_eq!(board.rows[0][2], Cell::Color(ColorKind::Yellow));
    }

    #[test]
    fn u_shaped_group_does_not_overwrite_rock_hidden_inside_its_pocket() {
        // 列0: row0=Red, row1=Red, row2=Rock, row3=Red
        // 列1: row1=Red, row2=Red, row3=Red
        // 列1経由で連結する「コ」の字型グループ(列0内では非連続=row2に岩を挟む)。
        let mut rows = empty_rows(5);
        rows[0][0] = Cell::Color(ColorKind::Red);
        rows[1][0] = Cell::Color(ColorKind::Red);
        rows[2][0] = Cell::Rock;
        rows[3][0] = Cell::Color(ColorKind::Red);
        rows[1][1] = Cell::Color(ColorKind::Red);
        rows[2][1] = Cell::Color(ColorKind::Red);
        rows[3][1] = Cell::Color(ColorKind::Red);
        let mut board = Board { rows };

        apply_gravity_tick(&mut board, (4, 11));

        assert_eq!(
            board.cell(2, 0),
            Cell::Rock,
            "列の非最下段にある岩が、グループの落下で上書き消滅してはいけない"
        );
    }

    #[test]
    fn group_already_at_field_bottom_does_not_fall_further() {
        let mut rows = empty_rows(1);
        rows[0][0] = Cell::Color(ColorKind::Red);
        rows[0][1] = Cell::Color(ColorKind::Red);
        rows[0][2] = Cell::Color(ColorKind::Red);
        let mut board = Board { rows };

        let outcome = apply_gravity_tick(&mut board, (0, 11));

        assert_eq!(outcome.groups_moved, 0);
        assert_eq!(board.rows[0][0], Cell::Color(ColorKind::Red));
    }

    // --- 異常系: 落下ブロックがプレイヤー位置に到達した場合は押し潰し(ミス)判定 ---

    #[test]
    fn falling_group_that_lands_on_player_position_is_crushed() {
        let mut rows = empty_rows(3);
        rows[0][0] = Cell::Color(ColorKind::Green);
        rows[0][1] = Cell::Color(ColorKind::Green);
        rows[0][2] = Cell::Color(ColorKind::Green);
        let mut board = Board { rows };

        // プレイヤーはグループ落下後の位置(row1, col1)にいる
        let outcome = apply_gravity_tick(&mut board, (1, 1));

        assert!(outcome.crushed);
        assert_eq!(board.rows[1][1], Cell::Color(ColorKind::Green));
    }

    // --- 正常系: 同一ティック内の処理順序(浅い側から。spec.md 4章手順6) ---

    #[test]
    fn shallower_group_does_not_chain_fall_in_the_same_tick_as_the_group_below_it() {
        // 上段グループ(row0)は下段グループ(row1)に直接乗っている(支持あり)。
        // 下段グループ(row1)の直下(row2)は空いているので、このティックで単独で落下できる。
        // 浅い側(上段)から処理するため、上段は「下段がまだ動いていない盤面」で判定され
        // 落下不可のまま。下段だけがこのティックで1マス落ちる。
        // (もし深い側から処理してしまうと、下段が先に動いて空いたrow1を上段がそのまま
        // 同一ティックで消費してしまい、2グループ分の連鎖落下が1ティックで起きてしまう)
        let mut rows = empty_rows(4);
        rows[0][0] = Cell::Color(ColorKind::Red);
        rows[0][1] = Cell::Color(ColorKind::Red);
        rows[0][2] = Cell::Color(ColorKind::Red);
        rows[1][0] = Cell::Color(ColorKind::Blue);
        rows[1][1] = Cell::Color(ColorKind::Blue);
        rows[1][2] = Cell::Color(ColorKind::Blue);
        let mut board = Board { rows };

        let outcome = apply_gravity_tick(&mut board, (3, 11));

        assert_eq!(outcome.groups_moved, 1, "このティックで動くのは下段グループのみ");
        assert_eq!(board.rows[0][0], Cell::Color(ColorKind::Red), "上段グループはこのティックでは未落下");
        assert_eq!(board.rows[1][0], Cell::Empty, "下段グループが抜けた後の穴");
        assert_eq!(board.rows[2][0], Cell::Color(ColorKind::Blue), "下段グループは1マス落下済み");
    }

    #[test]
    fn falling_group_that_does_not_reach_player_position_is_not_crushed() {
        let mut rows = empty_rows(3);
        rows[0][0] = Cell::Color(ColorKind::Green);
        rows[0][1] = Cell::Color(ColorKind::Green);
        rows[0][2] = Cell::Color(ColorKind::Green);
        let mut board = Board { rows };

        // プレイヤーは落下先グループと重ならない位置にいる
        let outcome = apply_gravity_tick(&mut board, (2, 11));

        assert!(!outcome.crushed);
    }
}

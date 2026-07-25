//! フィールド生成・ブロック配置・連結判定・重力落下ロジック(spec.md 2〜4章)。
//!
//! ratatui/crossterm/rodio の副作用を一切持たない純粋なデータ構造・関数のみで構成する。

use std::collections::HashMap;
use std::collections::HashSet;

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;

use crate::constants::{FIELD_WIDTH, ROCK_HITS_TO_BREAK, SHAKE_TICKS};

/// フィールド1マスの内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    Empty,
    Color(ColorKind),
    /// 岩ブロック(Xブロック)。破壊されるまでの累積ヒット数(0〜4、spec.md 2章・4章)。
    /// 5回目のヒットで破壊される(確定事実: "require five strikes before they break")。
    /// 落下・着地してもこの値は保持されたまま移動する(spec.md 4.8)。
    Rock { hits: u8 },
    Oxygen,
    /// ダイヤブロック。TERM独自拡張(初代の確定事実には存在しない要素)。
    Diamond,
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
    pub const ALL: [ColorKind; 4] = [ColorKind::Red, ColorKind::Blue, ColorKind::Green, ColorKind::Yellow];
}

/// 深度帯ごとの岩・酸素・ダイヤの出現確率テーブル(spec.md 3.1)。
///
/// 色ブロックの内訳(4色均等)はこのテーブルでは扱わない。3.2〜3.4の近傍依存生成は
/// 深度に関わらず常に4色均等抽選のため、`color_each`のような個別値は不要(3.1末尾参照)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandTable {
    pub rock: f32,
    pub oxygen: f32,
    pub diamond: f32,
}

/// 行インデックス(=深度[m]、spec.md 2章)から出現確率テーブルを引く(spec.md 3.1)。
///
/// 注意: ここでの「行インデックス=深度」は spec.md 2章のフィールド定義に基づく値で、
/// `Player::depth_m()`(= row + 1、クリア条件が深度1000mちょうどになるよう+1している)
/// とは意味が異なる。本関数の引数は常に「行インデックスそのもの」を渡すこと。
pub fn band_table(row: usize) -> BandTable {
    match row {
        0..=199 => BandTable {
            rock: 0.05,
            oxygen: 0.03,
            diamond: 0.02,
        },
        200..=399 => BandTable {
            rock: 0.08,
            oxygen: 0.04,
            diamond: 0.03,
        },
        400..=599 => BandTable {
            rock: 0.12,
            oxygen: 0.04,
            diamond: 0.04,
        },
        600..=799 => BandTable {
            rock: 0.16,
            oxygen: 0.04,
            diamond: 0.05,
        },
        _ => BandTable {
            rock: 0.20,
            oxygen: 0.04,
            diamond: 0.06,
        },
    }
}

// ---------------------------------------------------------------------------
// 3.2 色ブロックの下地生成(近傍依存)
// ---------------------------------------------------------------------------

/// 左隣を同色にする確率(spec.md 3.2)。横方向のまとまりの強さを直接左右する値。
///
/// 当初値0.55だったが、統計検証(横方向の同色ランの平均長)の結果、横方向の
/// まとまりをもっとはっきり見せてほしいというユーザー指摘を受けて0.65へ引き上げた
/// (TERM独自の調整。3.1の深度帯別出現確率とは無関係で、色ブロック同士の内訳にのみ影響する)。
const LEFT_INHERIT_PROB: f32 = 0.65;

/// 左隣が不採用の場合に上隣を同色にする確率の上限(累積、spec.md 3.2)。
/// `LEFT_INHERIT_PROB`引き上げに合わせて0.85から0.90へ調整し、縦方向のまとまりの
/// 強さ自体はほぼ維持しつつ、完全ランダム抽選の比率を15%から10%へ下げた。
const TOP_INHERIT_PROB_CEIL: f32 = 0.90;

/// 左隣・上隣の色をもとに候補色を1つ選ぶ(spec.md 3.2)。
///
/// - `r < LEFT_INHERIT_PROB`(0.65) かつ左隣が色ブロックなら左隣と同色
/// - それ以外で `r < TOP_INHERIT_PROB_CEIL`(0.90) かつ上隣が色ブロックなら上隣と同色
/// - どちらにも該当しなければ4色から均等ランダム
fn pick_base_color(rng: &mut ChaCha8Rng, left: Option<ColorKind>, top: Option<ColorKind>) -> ColorKind {
    let r: f32 = rng.random_range(0.0..1.0);
    if r < LEFT_INHERIT_PROB
        && let Some(c) = left {
            return c;
        }
    if r < TOP_INHERIT_PROB_CEIL
        && let Some(c) = top {
            return c;
        }
    ColorKind::ALL[rng.random_range(0..4)]
}

/// 同色連続の上限(横4・縦3、spec.md 3.3)を超える場合に候補色を差し替える。
///
/// 差し替え先は追加の乱数を消費せず、`ColorKind::ALL`の固定順で最初に見つかった
/// 制約に抵触しない色を採用する。
fn resolve_run_limits(candidate: ColorKind, left3: [Option<ColorKind>; 3], top2: [Option<ColorKind>; 2]) -> ColorKind {
    let breaks_horizontal = |c: ColorKind| left3.iter().all(|n| *n == Some(c));
    let breaks_vertical = |c: ColorKind| top2.iter().all(|n| *n == Some(c));

    if !breaks_horizontal(candidate) && !breaks_vertical(candidate) {
        return candidate;
    }
    ColorKind::ALL
        .into_iter()
        .find(|&c| !breaks_horizontal(c) && !breaks_vertical(c))
        .expect("4色中2色以上は必ず両条件を満たす")
}

/// コース全行の色下地を生成する(spec.md 3.2〜3.3)。
///
/// 戻り値は行×列の`Option<ColorKind>`。`None`は「その位置に色ブロックが存在しない」
/// ことを表し、深度0〜1m(先頭2行、spec.md 2章の安全地帯)は生成パス自体を適用せず
/// 常に`None`のままにする。これにより3.2の「上隣が存在し色ブロックである」という
/// 条件判定が、安全地帯を挟んでも自然にfalseになる(安全地帯にダミー色を置いて
/// しまうと、実際には存在しない色ブロックから3行目以降の生成が影響を受けてしまう)。
fn generate_base_colors(rng: &mut ChaCha8Rng, depth_rows: usize) -> Vec<[Option<ColorKind>; FIELD_WIDTH]> {
    let mut base: Vec<[Option<ColorKind>; FIELD_WIDTH]> = vec![[None; FIELD_WIDTH]; depth_rows];

    for row in 2..depth_rows {
        for col in 0..FIELD_WIDTH {
            let left = if col == 0 { None } else { base[row][col - 1] };
            let top = base[row - 1][col];
            let candidate = pick_base_color(rng, left, top);

            let left3 = [
                if col >= 1 { base[row][col - 1] } else { None },
                if col >= 2 { base[row][col - 2] } else { None },
                if col >= 3 { base[row][col - 3] } else { None },
            ];
            let top2 = [base[row - 1][col], base[row - 2][col]];

            base[row][col] = Some(resolve_run_limits(candidate, left3, top2));
        }
    }

    base
}

/// マス`(row, col)`の上下左右のうち、盤内かつ色ブロックであるものの色一覧(spec.md 3.4)。
fn same_color_neighbor_candidates(base: &[[Option<ColorKind>; FIELD_WIDTH]], row: usize, col: usize) -> Vec<ColorKind> {
    let rows = base.len();
    let mut neighbors = Vec::with_capacity(4);
    if row > 0
        && let Some(c) = base[row - 1][col] {
            neighbors.push(c);
        }
    if row + 1 < rows
        && let Some(c) = base[row + 1][col] {
            neighbors.push(c);
        }
    if col > 0
        && let Some(c) = base[row][col - 1] {
            neighbors.push(c);
        }
    if col + 1 < FIELD_WIDTH
        && let Some(c) = base[row][col + 1] {
            neighbors.push(c);
        }
    neighbors
}

/// 最も出現数が多い色を返す。同数の場合は`ColorKind::ALL`の順(Red,Blue,Green,Yellow)で
/// 先に来る色を採用する(spec.md 3.4)。
fn most_common_color(neighbors: &[ColorKind]) -> ColorKind {
    let mut best = ColorKind::ALL[0];
    let mut best_count = -1i32;
    for c in ColorKind::ALL {
        let count = neighbors.iter().filter(|&&n| n == c).count() as i32;
        if count > best_count {
            best_count = count;
            best = c;
        }
    }
    best
}

/// 孤立セルの解消(生成後の後処理、spec.md 3.4)。
///
/// 盤面全体に対して1回だけ、行→列の順に走査しながら**その場で**書き換える
/// (spec.mdのpseudocode通り、スナップショットを取らず逐次反映する。既に置換済みの
/// 隣接セルの新しい色を後続の判定が参照することも許容する)。
fn fix_isolated_cells(base: &mut [[Option<ColorKind>; FIELD_WIDTH]]) {
    let rows = base.len();
    for row in 0..rows {
        for col in 0..FIELD_WIDTH {
            let Some(me) = base[row][col] else { continue };
            let neighbors = same_color_neighbor_candidates(base, row, col);
            let is_isolated = !neighbors.contains(&me);
            if is_isolated && !neighbors.is_empty() {
                base[row][col] = Some(most_common_color(&neighbors));
            }
        }
    }
}

/// 岩・酸素・ダイヤの上書き配置(spec.md 3.5)。マスごとに独立抽選する。
fn overlay_rock_oxygen_diamond(rng: &mut ChaCha8Rng, base_color: ColorKind, row: usize) -> Cell {
    let t = band_table(row);
    let r: f32 = rng.random_range(0.0..1.0);
    if r < t.rock {
        Cell::Rock { hits: 0 }
    } else if r < t.rock + t.oxygen {
        Cell::Oxygen
    } else if r < t.rock + t.oxygen + t.diamond {
        Cell::Diamond
    } else {
        Cell::Color(base_color)
    }
}

/// ゲームフィールド全体(1000行×12列)。
#[derive(Debug, Clone)]
pub struct Board {
    pub rows: Vec<[Cell; FIELD_WIDTH]>,
}

impl Board {
    /// 乱数シードから深さ depth_rows 行ぶんのフィールドを事前生成する(spec.md 3.6)。
    ///
    /// 手順: 3.2〜3.3の下地生成を全行分行い、3.4の孤立セル解消を盤面全体に1回、
    /// 最後に3.5の上書きを全マスに適用する。この順序で1回だけ行い、生成し直しはしない。
    pub fn generate(seed: u64, depth_rows: usize) -> Self {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);

        let mut base = generate_base_colors(&mut rng, depth_rows);
        fix_isolated_cells(&mut base);

        let rows = (0..depth_rows)
            .map(|row| {
                let mut cells = [Cell::Empty; FIELD_WIDTH];
                for (col, cell) in cells.iter_mut().enumerate() {
                    *cell = match base[row][col] {
                        None => Cell::Empty, // 安全地帯(深度0〜1m)
                        Some(color) => overlay_rock_oxygen_diamond(&mut rng, color, row),
                    };
                }
                cells
            })
            .collect();

        Board { rows }
    }

    pub fn depth_rows(&self) -> usize {
        self.rows.len()
    }

    pub fn cell(&self, row: usize, col: usize) -> Cell {
        self.rows[row][col]
    }

    pub fn set(&mut self, row: usize, col: usize, cell: Cell) {
        self.rows[row][col] = cell;
    }
}

/// 盤面上の`start`を起点に、4方向で`same_kind`を満たすセルに連結している全セルを求める
/// 汎用BFS(spec.md 4章)。色ブロックの同色連結・岩ブロックの連結(hitsに関わらず全て
/// 同種とみなす)の両方がこの1つの実装を共有する。
fn connected_group(board: &Board, start: (usize, usize), same_kind: impl Fn(Cell) -> bool) -> Vec<(usize, usize)> {
    let depth_rows = board.depth_rows();
    let mut visited: HashSet<(usize, usize)> = HashSet::new();
    let mut stack = vec![start];
    visited.insert(start);
    let mut group = Vec::new();

    while let Some((r, c)) = stack.pop() {
        group.push((r, c));

        let neighbors = [(r.wrapping_sub(1), c), (r + 1, c), (r, c.wrapping_sub(1)), (r, c + 1)];
        for (nr, nc) in neighbors {
            if nr >= depth_rows || nc >= FIELD_WIDTH {
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
///
/// サイズに関わらず(1個の孤立ブロックでも)全て列挙する。呼び出し側が
/// 「即時消滅(4.6、サイズ問わず)」「自動消滅(4.5、サイズ4以上のみ)」を使い分ける。
pub fn connected_same_color(board: &Board, start: (usize, usize), color: ColorKind) -> Vec<(usize, usize)> {
    connected_group(board, start, |cell| cell == Cell::Color(color))
}

/// 盤面上の`start`を起点に、4方向で連結している岩ブロック(Xブロック)を全て求める
/// (spec.md 4.1・4.9)。個々のセルの`hits`値に関わらず、岩ブロックであれば全て同種として
/// 連結対象になる(色ブロックの「同色」に相当する条件が岩ブロックでは「岩であること」)。
pub fn connected_rock_group(board: &Board, start: (usize, usize)) -> Vec<(usize, usize)> {
    connected_group(board, start, |cell| matches!(cell, Cell::Rock { .. }))
}

/// プレイヤーが色ブロックを直接掘削した際の即時消滅処理(spec.md 4.6)。
///
/// 掘削したセルを起点に4方向連結の同色グループを求め、**サイズに関わらず**
/// (1個の孤立ブロックでも、既に4個以上連結した静的な塊でも)グループ全体を即座に消滅させる。
/// `target`が色ブロックでない場合は何もせず0を返す。
///
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
    /// 累積5回目のヒットで破壊された(このセルを含め、4方向で連結している岩ブロック
    /// 全てが色ブロックと同様に消滅する。spec.md 4.9)。`blocks`は消滅した総数
    Destroyed { blocks: usize },
}

/// 岩ブロックへ1ヒット加える。`target`が岩ブロックでない場合はNoneを返す。
///
/// 5回目のヒットで破壊に至った場合、そのセル単独ではなく、4方向で連結している岩ブロックを
/// hits値に関わらず全てBFSで求め(spec.md 4.9)、まとめて消滅させる(色ブロックの直接掘削
/// 消滅4.6と同様の扱い)。酸素ペナルティは呼び出し側が1回だけ適用する(実際に掘削した
/// 1ブロック分。連結して巻き込まれた分は追加ペナルティなし)。
pub fn hit_rock(board: &mut Board, target: (usize, usize)) -> Option<RockHitResult> {
    let Cell::Rock { hits } = board.cell(target.0, target.1) else {
        return None;
    };
    let hits = hits + 1;
    if hits >= ROCK_HITS_TO_BREAK {
        let group = connected_rock_group(board, target);
        for &(r, c) in &group {
            board.set(r, c, Cell::Empty);
        }
        Some(RockHitResult::Destroyed { blocks: group.len() })
    } else {
        board.set(target.0, target.1, Cell::Rock { hits });
        Some(RockHitResult::StillIntact)
    }
}

// ---------------------------------------------------------------------------
// 4章 落下・連結・消滅ロジック
// ---------------------------------------------------------------------------

/// 揺れ(spec.md 4.3)の状態を保持する。物理演算自体は副作用のない純粋関数のまま保ち、
/// この状態はGame(呼び出し側)が明示的に持ち回す。
///
/// マップの値は「そのセルが連続して未支持と判定されたティック数」。エントリが無い
/// (=0扱い)セルは支持されている、または直近まで支持されていたセル。
#[derive(Debug, Clone, Default)]
pub struct GravityState {
    unsupported_ticks: HashMap<(usize, usize), u8>,
}

impl GravityState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 指定セルが現在「震えている」状態(未支持と判定されたが、まだ`SHAKE_TICKS`ぶんの
    /// 猶予が明けておらず実際には落下していない)かどうか(spec.md 4.3)。
    ///
    /// 物理演算自体は副作用の無い純粋関数のままだが、この状態は`GravityState`が保持して
    /// いるため、描画側(次フェーズ以降のシェイク演出)はこのメソッド経由で参照できる。
    /// シェイク演出自体は今回のイテレーションのスコープ外(spec.md 9.11)のため、本体側の
    /// 呼び出し元はまだ無い(単体テストでのみ使用)。
    #[allow(dead_code, reason = "次フェーズの描画層(シェイク演出)向けに公開しておくデータフラグ")]
    pub fn is_shaking(&self, pos: (usize, usize)) -> bool {
        self.unsupported_ticks.contains_key(&pos)
    }
}

/// 1回の重力ティックの結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FallTickOutcome {
    /// このティックで実際に1マス落下したセルの数(揺れ中のセルは含まない)。
    pub cells_moved: usize,
    /// 落下してきたセルがプレイヤー位置に重なった(=押し潰された)かどうか。
    pub crushed: bool,
    /// 落下・着地した結果、同色4連結により自動消滅した色ブロック数
    /// (spec.md 4.5。呼び出し側が「消滅数 × 30点」を加算する。7章)。
    pub auto_vanished_blocks: usize,
    /// 落下・着地した結果、4連結以上により自動消滅した岩ブロック数
    /// (spec.md 4.9。色ブロックと異なり得点対象外だが、破壊音等のイベント発火には使う)。
    pub auto_vanished_rock_blocks: usize,
}

/// あるセル`pos`が「支持されている」か(spec.md 4.2)。
///
/// - `row`がコース最深行であり、直下の行が存在しない場合は真
/// - 直下のセルが空(Empty)でない、かつプレイヤーが直下にいない場合は真
///
/// プレイヤーの現在位置は常に「空洞」として扱う(プレイヤーが支えの代わりになることはない)。
fn is_supported(board: &Board, pos: (usize, usize), player_pos: (usize, usize)) -> bool {
    let (row, col) = pos;
    let depth_rows = board.depth_rows();
    if row + 1 >= depth_rows {
        return true;
    }
    let below = (row + 1, col);
    board.cell(below.0, below.1) != Cell::Empty && below != player_pos
}

/// 論理ティック1回ぶんの重力落下処理(spec.md 4章・5章)を実行する。
///
/// - 支持判定(4.2)はティック開始時点のスナップショットを基準に、全セル同時に行う(4.4)。
/// - 支持を失ったセルは即座には落下せず、まず`SHAKE_TICKS`ぶん揺れてから落下を開始する
///   (4.3)。揺れが明けている未支持セルだけがこのティックで1マス下へ移動する。
/// - 移動先がプレイヤー位置と重なった場合、その瞬間に押し潰し(crushed)を確定し、
///   押し潰した側のブロックはその場で消滅する(spec.md 5章)
/// - 移動した結果、直下が非Empty(=着地)になった色ブロックについてのみ、4方向連結の
///   同色グループを判定し、4個以上なら自動消滅させる(4.5)。岩・酸素・ダイヤは
///   このグループ判定の対象外で、着地したらそのまま固定される(4.1)
pub fn apply_gravity_tick(board: &mut Board, player_pos: (usize, usize), gravity: &mut GravityState) -> FallTickOutcome {
    let snapshot = board.clone();
    let depth_rows = board.depth_rows();
    let mut moves: Vec<((usize, usize), (usize, usize))> = Vec::new();
    let mut next_unsupported_ticks: HashMap<(usize, usize), u8> = HashMap::new();

    for row in 0..depth_rows {
        for col in 0..FIELD_WIDTH {
            let pos = (row, col);
            if snapshot.cell(row, col) == Cell::Empty {
                continue;
            }
            if is_supported(&snapshot, pos, player_pos) {
                continue; // 支持されている = 揺れ状態も解除(next_unsupported_ticksに載せない)
            }

            let ticks_unsupported = gravity.unsupported_ticks.get(&pos).copied().unwrap_or(0) + 1;
            if ticks_unsupported as u32 > SHAKE_TICKS as u32 {
                // 揺れが明けた(またはSHAKE_TICKS=0で即座に) -> このティックで1マス落下する
                moves.push((pos, (row + 1, col)));
            } else {
                // まだ揺れている最中 -> 移動しない
                next_unsupported_ticks.insert(pos, ticks_unsupported);
            }
        }
    }

    gravity.unsupported_ticks = next_unsupported_ticks;

    let mut outcome = FallTickOutcome::default();
    if moves.is_empty() {
        return outcome;
    }
    outcome.cells_moved = moves.len();

    for &(from, to) in &moves {
        let cell = snapshot.cell(from.0, from.1);
        board.set(from.0, from.1, Cell::Empty);
        if to == player_pos {
            // 押し潰した側のブロックはその場で消滅する(spec.md 5章。得点は発生しない)。
            board.set(to.0, to.1, Cell::Empty);
            // 酸素カプセル(AIR)だけは例外で、落下して当たっても押し潰し判定にしない
            // (TERM独自拡張・ユーザー指摘反映)。取得扱い(酸素回復)にはせず消滅のみとする。
            if cell != Cell::Oxygen {
                outcome.crushed = true;
            }
        } else {
            board.set(to.0, to.1, cell);
        }
    }

    // 着地(=移動先で直下が支持状態になった)色ブロック・岩ブロックについて連結・自動消滅を
    // 判定する(spec.md 4.5・4.9)。岩ブロックも色ブロックと同様に4連結以上で自動消滅するが、
    // 得点は発生しない(2章・7章)。
    for &(_, to) in &moves {
        if !is_supported(board, to, player_pos) {
            continue; // まだ落下中(次ティック以降に改めて着地判定する)
        }
        match board.cell(to.0, to.1) {
            Cell::Color(color) => {
                let group = connected_same_color(board, to, color);
                if group.len() >= 4 {
                    for &(r, c) in &group {
                        board.set(r, c, Cell::Empty);
                    }
                    outcome.auto_vanished_blocks += group.len();
                }
            }
            Cell::Rock { .. } => {
                let group = connected_rock_group(board, to);
                if group.len() >= 4 {
                    for &(r, c) in &group {
                        board.set(r, c, Cell::Empty);
                    }
                    outcome.auto_vanished_rock_blocks += group.len();
                }
            }
            _ => {}
        }
    }

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_board(rows: usize) -> Board {
        Board {
            rows: vec![[Cell::Empty; FIELD_WIDTH]; rows],
        }
    }

    // --- 生成: 安全地帯(先頭2行)は常にEmpty ---

    #[test]
    fn generate_keeps_first_two_rows_empty() {
        let board = Board::generate(1, 50);
        for col in 0..FIELD_WIDTH {
            assert_eq!(board.cell(0, col), Cell::Empty);
            assert_eq!(board.cell(1, col), Cell::Empty);
        }
    }

    #[test]
    fn generate_produces_deterministic_output_for_same_seed() {
        let a = Board::generate(42, 100);
        let b = Board::generate(42, 100);
        for row in 0..100 {
            assert_eq!(a.rows[row], b.rows[row]);
        }
    }

    // 3.2〜3.3の下地生成(孤立セル解消より前)は横4・縦3の連続数上限を厳密に守る。
    // 3.4の孤立セル解消は上限を再チェックしない仕様(spec.md 3.4末尾)のため、
    // その後処理を経た最終盤面ではごく稀に上限を超える可能性を許容する
    // (`resolve_run_limits_*`の単体テストで境界条件自体は個別に検証する)。
    fn assert_run_limits_hold(base: &[[Option<ColorKind>; FIELD_WIDTH]], seed: u64) {
        for (row, cells) in base.iter().enumerate() {
            let mut run_color = None;
            let mut run_len = 0usize;
            for (col, &c) in cells.iter().enumerate() {
                if c.is_some() && c == run_color {
                    run_len += 1;
                } else {
                    run_color = c;
                    run_len = if c.is_some() { 1 } else { 0 };
                }
                assert!(run_len <= 4, "seed={seed}: 横方向の同色連続が4を超えた row={row} col={col}");
            }
        }
        for col in 0..FIELD_WIDTH {
            let mut run_color = None;
            let mut run_len = 0usize;
            for (row, cells) in base.iter().enumerate() {
                let c = cells[col];
                if c.is_some() && c == run_color {
                    run_len += 1;
                } else {
                    run_color = c;
                    run_len = if c.is_some() { 1 } else { 0 };
                }
                assert!(run_len <= 3, "seed={seed}: 縦方向の同色連続が3を超えた row={row} col={col}");
            }
        }
    }

    #[test]
    fn base_color_generation_respects_run_limits_before_isolated_cell_fix() {
        let mut rng = ChaCha8Rng::seed_from_u64(7);
        let base = generate_base_colors(&mut rng, 500);
        assert_run_limits_hold(&base, 7);
    }

    // 統計的検証(spec.md 3.3): アルゴリズムの保証自体は決定的だが、多数のシード・
    // 盤面サイズにわたって上限が破られないことを横断的に確認する。
    #[test]
    fn base_color_generation_respects_run_limits_across_many_seeds() {
        for seed in 0..20u64 {
            let mut rng = ChaCha8Rng::seed_from_u64(seed);
            let base = generate_base_colors(&mut rng, 300);
            assert_run_limits_hold(&base, seed);
        }
    }

    /// 完成した盤面(岩・酸素・ダイヤ上書き後)における、あるセルの色ブロックとしての色
    /// (色ブロックでなければNone)。
    fn color_of(board: &Board, row: usize, col: usize) -> Option<ColorKind> {
        match board.cell(row, col) {
            Cell::Color(c) => Some(c),
            _ => None,
        }
    }

    // 統計的検証(spec.md 3.4末尾): 孤立セル解消は連続数上限を再チェックしないため、
    // 最終盤面ではごく稀に横5連続・縦4連続が発生し得ることを仕様上許容している。
    // ここでは「稀」であることを、大量セルに対する超過発生率が無視できる水準
    // (全セル数の1%未満)に収まっているかで統計的に検証する。
    #[test]
    fn final_generated_board_run_limit_violations_are_rare() {
        let mut total_cells = 0usize;
        let mut horizontal_violations = 0usize;
        let mut vertical_violations = 0usize;

        for seed in 0..10u64 {
            let board = Board::generate(seed, 300);

            for row in 0..board.depth_rows() {
                let mut run_color = None;
                let mut run_len = 0usize;
                for col in 0..FIELD_WIDTH {
                    let c = color_of(&board, row, col);
                    total_cells += 1;
                    if c.is_some() && c == run_color {
                        run_len += 1;
                    } else {
                        run_color = c;
                        run_len = if c.is_some() { 1 } else { 0 };
                    }
                    if run_len == 5 {
                        horizontal_violations += 1;
                    }
                }
            }

            for col in 0..FIELD_WIDTH {
                let mut run_color = None;
                let mut run_len = 0usize;
                for row in 0..board.depth_rows() {
                    let c = color_of(&board, row, col);
                    if c.is_some() && c == run_color {
                        run_len += 1;
                    } else {
                        run_color = c;
                        run_len = if c.is_some() { 1 } else { 0 };
                    }
                    if run_len == 4 {
                        vertical_violations += 1;
                    }
                }
            }
        }

        let max_allowed = total_cells / 100;
        assert!(
            horizontal_violations <= max_allowed,
            "横方向の上限超過が多すぎる: {horizontal_violations} / {total_cells}セル(許容 {max_allowed})"
        );
        assert!(
            vertical_violations <= max_allowed,
            "縦方向の上限超過が多すぎる: {vertical_violations} / {total_cells}セル(許容 {max_allowed})"
        );
    }

    // --- 孤立セルの解消(spec.md 3.4) ---

    #[test]
    fn fix_isolated_cells_replaces_lone_cell_with_majority_neighbor_color() {
        // fix_isolated_cellsは盤面を行→列の順で走査しながらその場で書き換える(spec.md 3.4、
        // 既に置換済みの隣接セルの新しい色を後続の判定が参照することも許容する)。そのため
        // target=(2,2)より前に処理される上(1,2)・左(2,1)は、それ自身が孤立と判定されず
        // 安定してRed/Blueのまま残るよう(0,0)や(2,0)で「支え」を用意しておく。
        // targetより後に処理される下(3,2)・右(2,3)は素の値のまま参照されるため、そのまま置く。
        let mut base: Vec<[Option<ColorKind>; FIELD_WIDTH]> = vec![[None; FIELD_WIDTH]; 4];
        base[1][1] = Some(ColorKind::Red); // (1,2)の左隣、先に処理されRedのまま安定する支え
        base[1][2] = Some(ColorKind::Red); // 上隣。(1,1)がRedで支えられ孤立判定されない
        base[2][0] = Some(ColorKind::Blue); // (2,1)の左隣、先に処理されBlueのまま安定する支え
        base[2][1] = Some(ColorKind::Blue); // 左隣。(2,0)がBlueで支えられ孤立判定されない
        base[2][2] = Some(ColorKind::Yellow); // target: 孤立セル自身
        base[2][3] = Some(ColorKind::Green); // 右隣。targetより後に処理されるため素の値のまま
        base[3][2] = Some(ColorKind::Red); // 下隣。targetより後に処理されるため素の値のまま

        fix_isolated_cells(&mut base);

        // targetの隣接色内訳はRed(上),Blue(左),Green(右),Red(下) = Red2・Blue1・Green1
        // → 最多のRedに置換される。
        assert_eq!(base[2][2], Some(ColorKind::Red));
    }

    #[test]
    fn fix_isolated_cells_breaks_ties_by_all_order() {
        // target=(1,1)の隣接はRed(上、既に処理済みで安定)とBlue(右、targetより後に処理
        // されるため素の値)の1個ずつでタイ。ColorKind::ALLの順(Red,Blue,Green,Yellow)で
        // 先に来るRedが採用される。
        let mut base: Vec<[Option<ColorKind>; FIELD_WIDTH]> = vec![[None; FIELD_WIDTH]; 3];
        base[0][0] = Some(ColorKind::Red); // (0,1)の左隣、先に処理されRedのまま安定する支え
        base[0][1] = Some(ColorKind::Red); // 上隣。(0,0)がRedで支えられ孤立判定されない
        base[1][1] = Some(ColorKind::Green); // target: 孤立セル自身
        base[1][2] = Some(ColorKind::Blue); // 右隣。targetより後に処理されるため素の値のまま

        fix_isolated_cells(&mut base);

        assert_eq!(base[1][1], Some(ColorKind::Red));
    }

    #[test]
    fn fix_isolated_cells_leaves_cell_untouched_when_no_color_neighbors_exist() {
        // 四方全てNone(安全地帯/盤外相当)の場合は置換しない(spec.md 3.4)。
        let mut base: Vec<[Option<ColorKind>; FIELD_WIDTH]> = vec![[None; FIELD_WIDTH]; 3];
        base[1][1] = Some(ColorKind::Red);

        fix_isolated_cells(&mut base);

        assert_eq!(base[1][1], Some(ColorKind::Red));
    }

    #[test]
    fn resolve_run_limits_avoids_fifth_horizontal_same_color() {
        let left3 = [Some(ColorKind::Red), Some(ColorKind::Red), Some(ColorKind::Red)];
        let resolved = resolve_run_limits(ColorKind::Red, left3, [None, None]);
        assert_ne!(resolved, ColorKind::Red);
    }

    #[test]
    fn resolve_run_limits_avoids_fourth_vertical_same_color() {
        let top2 = [Some(ColorKind::Blue), Some(ColorKind::Blue)];
        let resolved = resolve_run_limits(ColorKind::Blue, [None, None, None], top2);
        assert_ne!(resolved, ColorKind::Blue);
    }

    #[test]
    fn resolve_run_limits_keeps_candidate_when_no_limit_hit() {
        let left3 = [Some(ColorKind::Red), None, None];
        let resolved = resolve_run_limits(ColorKind::Red, left3, [None, None]);
        assert_eq!(resolved, ColorKind::Red);
    }

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
    fn rock_break_on_fifth_hit_vanishes_the_whole_connected_rock_group() {
        // ユーザー指摘(task4): 岩ブロックも色ブロックと同様に4方向連結の対象になる。
        // ヒットしたのは(0,0)だけでも、連結している岩ブロック全部が消滅する(spec.md 4.9)。
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Rock { hits: ROCK_HITS_TO_BREAK - 1 }; // あと1発で破壊
        board.rows[0][1] = Cell::Rock { hits: 0 }; // hitsが違っても岩ブロックなら連結対象
        board.rows[1][0] = Cell::Rock { hits: 2 };
        board.rows[0][2] = Cell::Color(ColorKind::Red); // 別種、巻き込まれない

        let result = hit_rock(&mut board, (0, 0)).unwrap();

        assert_eq!(result, RockHitResult::Destroyed { blocks: 3 });
        assert_eq!(board.cell(0, 0), Cell::Empty);
        assert_eq!(board.cell(0, 1), Cell::Empty);
        assert_eq!(board.cell(1, 0), Cell::Empty);
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

    // --- 重力: 揺れてから落下する(4.3) ---

    #[test]
    fn unsupported_cell_shakes_for_shake_ticks_before_falling() {
        let mut board = empty_board(3);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        let mut gravity = GravityState::new();

        // SHAKE_TICKS ぶんは揺れるだけで移動しない。この間`is_shaking`はtrueを返し、
        // 描画側がシェイク演出に使えるデータとして残る(spec.md 4.3)。
        for _ in 0..SHAKE_TICKS {
            let outcome = apply_gravity_tick(&mut board, (99, 99), &mut gravity);
            assert_eq!(outcome.cells_moved, 0);
            assert!(gravity.is_shaking((0, 0)));
        }
        assert_eq!(board.cell(0, 0), Cell::Color(ColorKind::Red));

        // SHAKE_TICKS+1ティック目で実際に1マス落下する
        let outcome = apply_gravity_tick(&mut board, (99, 99), &mut gravity);
        assert_eq!(outcome.cells_moved, 1);
        assert_eq!(board.cell(0, 0), Cell::Empty);
        assert_eq!(board.cell(1, 0), Cell::Color(ColorKind::Red));
        assert!(!gravity.is_shaking((1, 0)), "着地して支持されればもう揺れていない");
    }

    #[test]
    fn is_shaking_is_false_for_a_supported_cell() {
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        board.rows[1][0] = Cell::Color(ColorKind::Blue); // 支えあり
        let mut gravity = GravityState::new();

        apply_gravity_tick(&mut board, (99, 99), &mut gravity);

        assert!(!gravity.is_shaking((0, 0)));
    }

    /// テスト用ヘルパー: `SHAKE_TICKS`ぶん揺れティックを消化してから、実際に落下する
    /// ティックを1回実行する(4.3のテストで繰り返し使う定型パターン)。
    fn shake_out_then_tick(board: &mut Board, player_pos: (usize, usize), gravity: &mut GravityState) -> FallTickOutcome {
        for _ in 0..SHAKE_TICKS {
            apply_gravity_tick(board, player_pos, gravity);
        }
        apply_gravity_tick(board, player_pos, gravity)
    }

    #[test]
    fn stacked_blocks_with_gap_fall_one_at_a_time_per_tick() {
        // 3段重なったブロックの下に1マスの隙間がある場合、最下段だけが先に落ちる(4.4)。
        let mut board = empty_board(5);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        board.rows[1][0] = Cell::Color(ColorKind::Red);
        board.rows[2][0] = Cell::Color(ColorKind::Red);
        // row3 col0 is Empty(隙間)、row4 col0 is Empty(床は無いので最下行はrow4)
        let mut gravity = GravityState::new();

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity); // 最下段のみ落下

        assert_eq!(outcome.cells_moved, 1);
        assert_eq!(board.cell(2, 0), Cell::Empty);
        assert_eq!(board.cell(3, 0), Cell::Color(ColorKind::Red));
        // 中段・上段はまだ落ちていない
        assert_eq!(board.cell(0, 0), Cell::Color(ColorKind::Red));
        assert_eq!(board.cell(1, 0), Cell::Color(ColorKind::Red));
    }

    // --- 重力: 押し潰し判定(5章) ---

    #[test]
    fn falling_block_onto_player_crushes_and_vanishes() {
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        let mut gravity = GravityState::new();
        let player_pos = (1, 0);

        let outcome = shake_out_then_tick(&mut board, player_pos, &mut gravity); // 落下→押し潰し

        assert!(outcome.crushed);
        assert_eq!(board.cell(1, 0), Cell::Empty); // 押し潰した側も消滅・得点なし
    }

    #[test]
    fn falling_oxygen_capsule_onto_player_does_not_crush_but_still_vanishes() {
        // AIR(酸素カプセル)は他のブロックと同様に落下する(2章)が、ユーザー指摘により
        // TERM独自拡張として、プレイヤーに当たっても押し潰し判定にはしない。
        // 取得扱い(酸素回復)にもせず、単に消滅するのみとする。
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Oxygen;
        let mut gravity = GravityState::new();
        let player_pos = (1, 0);

        let outcome = shake_out_then_tick(&mut board, player_pos, &mut gravity);

        assert!(!outcome.crushed, "酸素カプセルは押し潰し判定にならないはず");
        assert_eq!(board.cell(1, 0), Cell::Empty);
    }

    // --- 重力: 着地時の自動消滅(4.5、4個以上のみ) ---

    #[test]
    fn landing_group_of_four_or_more_auto_vanishes() {
        // depth_rows=2: row1が最深行(常に支持される)。row0の3個が落下してrow1へ着地し、
        // あらかじめ最深行に置いた1個(col3)へ連結して合計4個になる。
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        board.rows[0][1] = Cell::Color(ColorKind::Red);
        board.rows[0][2] = Cell::Color(ColorKind::Red);
        board.rows[1][3] = Cell::Color(ColorKind::Red); // 既に着底(最深行=常に支持)
        let mut gravity = GravityState::new();

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity); // 落下+着地+自動消滅

        assert_eq!(outcome.auto_vanished_blocks, 4);
        assert_eq!(board.cell(1, 0), Cell::Empty);
        assert_eq!(board.cell(1, 1), Cell::Empty);
        assert_eq!(board.cell(1, 2), Cell::Empty);
        assert_eq!(board.cell(1, 3), Cell::Empty);
    }

    #[test]
    fn landing_group_of_three_or_fewer_stays() {
        // depth_rows=2: row0の2個が落下し、最深行に既にある1個(col2)へ連結して合計3個。
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        board.rows[0][1] = Cell::Color(ColorKind::Red);
        board.rows[1][2] = Cell::Color(ColorKind::Red); // 既に着底(最深行=常に支持)
        let mut gravity = GravityState::new();

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity); // 落下+着地(3個)

        assert_eq!(outcome.auto_vanished_blocks, 0);
        assert_eq!(board.cell(1, 0), Cell::Color(ColorKind::Red));
        assert_eq!(board.cell(1, 1), Cell::Color(ColorKind::Red));
        assert_eq!(board.cell(1, 2), Cell::Color(ColorKind::Red));
    }

    #[test]
    fn falling_group_merges_with_supported_same_color_block_directly_below_and_auto_vanishes() {
        // 確定事実2「支えを失ったブロックは落下し、支持されている同色ブロックに接触すると
        // 停止して連結する」の核心を、実際に「2マス連結した状態で一緒に落下してくるグループ」が
        // 「既に支持されている同色の塊」の真上に接触する形で検証する(spec.md 4章冒頭)。
        //
        // depth_rows=3: row2(最深行)に既に連結済みの支持グループ(2個)、row0に連結した
        // 落下グループ(2個)を置く。接触前はこの2グループが繋がっていないことを確認したうえで、
        // 1回の落下ティックで接触・連結し、合計4個になって自動消滅することを確認する。
        let mut board = empty_board(3);
        board.rows[2][0] = Cell::Color(ColorKind::Red);
        board.rows[2][1] = Cell::Color(ColorKind::Red);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        board.rows[0][1] = Cell::Color(ColorKind::Red);
        let mut gravity = GravityState::new();

        // 接触前: 支持グループは2個だけで、落下グループとはまだ繋がっていない。
        assert_eq!(connected_same_color(&board, (2, 0), ColorKind::Red).len(), 2);

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity); // 落下→接触→連結→自動消滅

        assert_eq!(outcome.cells_moved, 2, "落下グループの2個が同時に1マス落ちる");
        assert_eq!(outcome.auto_vanished_blocks, 4, "接触した結果、合計4個で自動消滅する");
        assert_eq!(board.cell(1, 0), Cell::Empty);
        assert_eq!(board.cell(1, 1), Cell::Empty);
        assert_eq!(board.cell(2, 0), Cell::Empty);
        assert_eq!(board.cell(2, 1), Cell::Empty);
    }

    #[test]
    fn landing_rock_group_of_four_or_more_auto_vanishes_without_score() {
        // spec.md 4.9(task4): 岩ブロックも色ブロックと同様に、着地して4連結以上になれば
        // 掘削されずに自動消滅する。ただし得点は発生しない(auto_vanished_blocksとは
        // 別カウンタ`auto_vanished_rock_blocks`に計上される)。
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Rock { hits: 0 };
        board.rows[0][1] = Cell::Rock { hits: 1 };
        board.rows[0][2] = Cell::Rock { hits: 2 };
        board.rows[1][3] = Cell::Rock { hits: 3 }; // 既に着底(最深行=常に支持)
        let mut gravity = GravityState::new();

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity);

        assert_eq!(outcome.auto_vanished_rock_blocks, 4);
        assert_eq!(outcome.auto_vanished_blocks, 0, "岩ブロックの自動消滅はスコア対象外");
        assert_eq!(board.cell(1, 0), Cell::Empty);
        assert_eq!(board.cell(1, 1), Cell::Empty);
        assert_eq!(board.cell(1, 2), Cell::Empty);
        assert_eq!(board.cell(1, 3), Cell::Empty);
    }

    #[test]
    fn landing_rock_group_of_three_or_fewer_stays_with_hits_preserved() {
        // 岩ブロックは落下・着地してもhitsを保持したまま移動する(spec.md 4.8)。
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Rock { hits: 3 };
        board.rows[1][1] = Cell::Rock { hits: 1 }; // 既に着底
        let mut gravity = GravityState::new();

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity);

        assert_eq!(outcome.auto_vanished_rock_blocks, 0);
        assert!(matches!(board.cell(1, 0), Cell::Rock { hits: 3 }), "落下してもhitsは保持される");
    }

    #[test]
    fn unsupported_rock_shakes_for_shake_ticks_before_falling() {
        // ユーザー指摘(task24)により追加された「震えてから落ちる」挙動(4.3)は色ブロックに
        // 限らず岩ブロックにも同様に適用される(4.9で岩ブロックも同じ重力の枠組みに乗ることが
        // 明記されている)。色ブロック版(unsupported_cell_shakes_for_shake_ticks_before_falling)
        // と同じ形の検証を岩ブロックで行う。
        let mut board = empty_board(3);
        board.rows[0][0] = Cell::Rock { hits: 2 };
        let mut gravity = GravityState::new();

        for _ in 0..SHAKE_TICKS {
            let outcome = apply_gravity_tick(&mut board, (99, 99), &mut gravity);
            assert_eq!(outcome.cells_moved, 0);
            assert!(gravity.is_shaking((0, 0)));
        }
        assert!(matches!(board.cell(0, 0), Cell::Rock { hits: 2 }), "揺れている間はまだ落下しない");

        let outcome = apply_gravity_tick(&mut board, (99, 99), &mut gravity);
        assert_eq!(outcome.cells_moved, 1);
        assert_eq!(board.cell(0, 0), Cell::Empty);
        assert!(matches!(board.cell(1, 0), Cell::Rock { hits: 2 }), "落下してもhitsは保持される");
        assert!(!gravity.is_shaking((1, 0)), "着地して支持されればもう揺れていない");
    }

    #[test]
    fn falling_rock_group_merges_with_supported_rock_group_below_and_auto_vanishes_ignoring_hit_counts() {
        // task25: Xブロックも色ブロックと同様に「支えを失う→震える→落下→支持されている
        // 岩ブロックに接触して連結→4個以上で自動消滅」する(4.9)。この連結・自動消滅は
        // 個々のhits値に関わらず成立し、かつ掘削の5回耐久ルール(2章)とは独立に働く
        // ことを示す: hits=4(あと1発で破壊)の岩ブロックが、5回目のヒットを一度も
        // 受けないまま、連結消滅ルールだけで消える。
        let mut board = empty_board(3);
        board.rows[2][0] = Cell::Rock { hits: 1 };
        board.rows[2][1] = Cell::Rock { hits: 3 };
        board.rows[0][0] = Cell::Rock { hits: 4 }; // あと1発で破壊されるはずだった岩
        board.rows[0][1] = Cell::Rock { hits: 0 };
        let mut gravity = GravityState::new();

        assert_eq!(connected_rock_group(&board, (2, 0)).len(), 2, "接触前は支持グループのみ2個");

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity);

        assert_eq!(outcome.auto_vanished_rock_blocks, 4, "hitsがバラバラでも岩ブロックとして連結・消滅する");
        assert_eq!(outcome.auto_vanished_blocks, 0, "岩ブロックの自動消滅はスコア対象外");
        assert_eq!(board.cell(1, 0), Cell::Empty);
        assert_eq!(board.cell(1, 1), Cell::Empty);
        assert_eq!(board.cell(2, 0), Cell::Empty);
        assert_eq!(board.cell(2, 1), Cell::Empty);
    }

    #[test]
    fn statically_generated_group_of_four_does_not_auto_vanish() {
        // ランダム生成時点でたまたま4個以上連結していても、実際に落下→着地するまでは
        // 自動消滅しない(spec.md 4.5末尾)。
        let mut board = empty_board(3);
        board.rows[2][0] = Cell::Color(ColorKind::Red);
        board.rows[2][1] = Cell::Color(ColorKind::Red);
        board.rows[2][2] = Cell::Color(ColorKind::Red);
        board.rows[2][3] = Cell::Color(ColorKind::Red);
        let mut gravity = GravityState::new();

        let outcome = apply_gravity_tick(&mut board, (99, 99), &mut gravity);

        assert_eq!(outcome.auto_vanished_blocks, 0);
        assert_eq!(board.cell(2, 0), Cell::Color(ColorKind::Red));
    }

    // --- 酸素カプセルは連結落下ロジックの対象外(spec.md 2章・4.1) ---

    #[test]
    fn connected_same_color_does_not_traverse_through_an_oxygen_capsule() {
        // 酸素カプセルは同色連結の対象外(4.1)。同色2マスの間に挟まっていると、
        // その間は繋がっているとみなされず、別グループとして扱われる。
        let mut board = empty_board(1);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        board.rows[0][1] = Cell::Oxygen;
        board.rows[0][2] = Cell::Color(ColorKind::Red);

        let group = connected_same_color(&board, (0, 0), ColorKind::Red);

        assert_eq!(group, vec![(0, 0)]); // 酸素カプセルに遮られ右側のRedへは繋がらない
    }

    #[test]
    fn falling_color_block_is_supported_by_an_oxygen_capsule_below_it_without_overwriting_it() {
        // 酸素カプセルも重力の対象(支えを失えば落下する)だが、他の色ブロックの支えにも
        // なる(spec.md 2章「常に連結グループの対象外」「重力の対象にはなる」)。
        // 落下判定は必ずEmptyマスへの移動としてのみ確定するため(4.4)、
        // 酸素カプセルが誤って上書き・消滅することは無い。
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        board.rows[1][0] = Cell::Oxygen;
        let mut gravity = GravityState::new();

        apply_gravity_tick(&mut board, (99, 99), &mut gravity); // 揺れ
        let outcome = apply_gravity_tick(&mut board, (99, 99), &mut gravity); // 支持判定

        assert_eq!(outcome.cells_moved, 0, "酸素カプセルは非Emptyなので上のRedは支持され落下しない");
        assert_eq!(board.cell(0, 0), Cell::Color(ColorKind::Red));
        assert_eq!(board.cell(1, 0), Cell::Oxygen, "酸素カプセルは上書き・消滅していない");
    }

    #[test]
    fn oxygen_capsule_itself_falls_through_empty_space_like_other_blocks() {
        // 酸素カプセルも重力の対象(spec.md 2章)。支えを失えば他のブロックと同様に落下する。
        let mut board = empty_board(3);
        board.rows[0][0] = Cell::Oxygen;
        let mut gravity = GravityState::new();

        shake_out_then_tick(&mut board, (99, 99), &mut gravity); // 揺れ+落下

        assert_eq!(board.cell(0, 0), Cell::Empty);
        assert_eq!(board.cell(1, 0), Cell::Oxygen);
    }

    // --- 横方向の同色クラスタ生成(spec.md 3.2、統計検証) ---

    /// 完成盤面(岩・酸素・ダイヤ上書き後)における横方向の同色ランの平均長を求める。
    fn avg_horizontal_color_run_length(depth_rows: usize, seeds: std::ops::Range<u64>) -> f64 {
        let mut total_len = 0u64;
        let mut total_runs = 0u64;
        for seed in seeds {
            let board = Board::generate(seed, depth_rows);
            for row in 0..board.depth_rows() {
                let mut run_color: Option<ColorKind> = None;
                let mut run_len = 0u64;
                for col in 0..FIELD_WIDTH {
                    let c = match board.cell(row, col) {
                        Cell::Color(k) => Some(k),
                        _ => None,
                    };
                    if c.is_some() && c == run_color {
                        run_len += 1;
                    } else {
                        if run_color.is_some() {
                            total_len += run_len;
                            total_runs += 1;
                        }
                        run_color = c;
                        run_len = if c.is_some() { 1 } else { 0 };
                    }
                }
                if run_color.is_some() {
                    total_len += run_len;
                    total_runs += 1;
                }
            }
        }
        total_len as f64 / total_runs as f64
    }

    #[test]
    fn horizontal_color_runs_are_noticeably_clustered_not_speckled() {
        // ユーザー指摘(「同じ色のブロックがくっついて見えない」)を受けてLEFT_INHERIT_PROBを
        // 0.55→0.65へ引き上げた。独立抽選(0.25)なら平均ラン長は1.33程度になるはずで、
        // 近傍依存生成が機能していれば明確にそれを上回る(実測: 旧値0.55で約2.06、
        // 新値0.65で約2.2〜2.3)。将来の劣化を検知できるよう、健全な下限を固定する。
        let avg = avg_horizontal_color_run_length(300, 0..200);
        assert!(
            avg >= 2.0,
            "横方向の同色ランの平均長が想定より短い(clustered生成が機能していない疑い、\
             完全独立抽選なら1.33程度になるはず): {avg}"
        );
    }
}

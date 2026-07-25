//! フィールド生成・ブロック配置・連結判定・重力落下ロジック(spec.md 2〜4章)。
//!
//! ratatui/crossterm/rodio の副作用を一切持たない純粋なデータ構造・関数のみで構成する。

use std::collections::HashMap;
use std::collections::HashSet;

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;

use crate::constants::{FIELD_WIDTH, ROCK_HITS_TO_BREAK, STAR_MELT_TICKS};

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
    /// スターブロック(TERM独自拡張。ユーザー指摘: 「画面内にきたら、溶けて自然と
    /// 消えるスターブロックも欲しい」)。プレイヤーの可視範囲内に入ると自然に溶けて
    /// 消える。`melting`は0(まだ画面外/入った直後)〜`STAR_MELT_TICKS`(消滅)の
    /// カウンタで、掘削・連結落下の対象外(常に単独・固定、酸素カプセル等と同様)。
    Star { melting: u8 },
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
    /// スターブロックの出現率(TERM独自拡張。深度に関わらず`STAR_SPAWN_PROB`で一定)。
    pub star: f32,
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
            star: crate::constants::STAR_SPAWN_PROB,
        },
        200..=399 => BandTable {
            rock: 0.08,
            oxygen: 0.04,
            diamond: 0.03,
            star: crate::constants::STAR_SPAWN_PROB,
        },
        400..=599 => BandTable {
            rock: 0.12,
            oxygen: 0.04,
            diamond: 0.04,
            star: crate::constants::STAR_SPAWN_PROB,
        },
        600..=799 => BandTable {
            rock: 0.16,
            oxygen: 0.04,
            diamond: 0.05,
            star: crate::constants::STAR_SPAWN_PROB,
        },
        _ => BandTable {
            rock: 0.20,
            oxygen: 0.04,
            diamond: 0.06,
            star: crate::constants::STAR_SPAWN_PROB,
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
    } else if r < t.rock + t.oxygen + t.diamond + t.star {
        Cell::Star { melting: 0 }
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
        // 岩ブロックは色ブロックと異なり、連結していても消えるのはヒットした
        // 1ブロックのみ(TERM独自拡張。ユーザー指摘: 「Xブロックは結合してても
        // 全体が消えるのではなく1ブロックしか消せないものとする」)。
        board.set(target.0, target.1, Cell::Empty);
        Some(RockHitResult::Destroyed { blocks: 1 })
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
    /// 現在「震えている」塊に属する全セル(TERM独自拡張、描画用)。`unsupported_ticks`は
    /// 揺れティック数を数えるため塊の代表座標だけをキーにしているが、描画側は塊の
    /// どのセルについても「揺れているか」を知りたい(ユーザー指摘: 「落下開始までの
    /// アニメーションぐらぐらしてほしい(各種ブロック)」)ため、塊の全メンバーを別途保持する。
    shaking_cells: HashSet<(usize, usize)>,
}

impl GravityState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 指定セルが現在「震えている」状態(未支持と判定されたが、まだ揺れティック数ぶんの
    /// 猶予が明けておらず実際には落下していない)かどうか(spec.md 4.3)。塊(連結
    /// グループ)に属するセルであれば、代表座標以外のどのセルについてもtrueを返す。
    ///
    /// 上向き掘削時の「不安定なブロックは掘れず押し潰される」判定(physics::drill_facing)
    /// と、揺れ演出の描画(ui::render)の両方で使う。支持されていない
    /// (=`is_supported`がfalse)だけでは「揺れ始めた直後でまだ見た目には静止している」
    /// ケースを区別できないため、この状態も合わせて見る。
    pub fn is_shaking(&self, pos: (usize, usize)) -> bool {
        self.shaking_cells.contains(&pos)
    }

    /// 揺れ状態を全てクリアする(TERM独自拡張)。デバッグショートカット等で盤面の
    /// 色配置を重力ティックの外から直接書き換えた直後に呼ぶ。塊の境界が変わると
    /// 揺れ状態が指していた代表座標の意味も変わってしまうため、次の重力ティックで
    /// 結合関係(塊)を作り直して支持判定からやり直させる(ユーザー指摘: 「ショートカット
    /// Cを10画面分に適用し、ちゃんと結合関係を再計算するように」)。
    pub fn reset(&mut self) {
        self.unsupported_ticks.clear();
        self.shaking_cells.clear();
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
    /// 落下してきた酸素カプセルがプレイヤー位置に重なり、取得された回数
    /// (TERM独自拡張。「歩くだけで取得できる」仕様と同様、上から落ちてきたAIRに
    /// 触れた場合も押し潰されず酸素回復扱いにする。ユーザー指摘「上から降ってきた
    /// AIRで回復してないバグ」の修正)。呼び出し側が酸素回復・スコア加算を行う。
    pub oxygen_collected: usize,
}

/// あるセル`pos`が「支持されている」か(spec.md 4.2)。
///
/// - `row`がコース最深行であり、直下の行が存在しない場合は真
/// - 直下のセルが空(Empty)でない、かつプレイヤーが直下にいない場合は真
///
/// プレイヤーの現在位置は常に「空洞」として扱う(プレイヤーが支えの代わりになることはない)。
/// 単独セル(酸素・ダイヤ)の判定にはそのまま使えるが、連結グループの判定には
/// `is_group_supported`を使うこと(このセル単独の直下だけを見ると、連結している
/// 仲間セルの存在を支えと誤認したり、逆に仲間越しの本当の支えを見落としたりする)。
pub(crate) fn is_supported(board: &Board, pos: (usize, usize), player_pos: (usize, usize)) -> bool {
    let (row, col) = pos;
    let depth_rows = board.depth_rows();
    if row + 1 >= depth_rows {
        return true;
    }
    let below = (row + 1, col);
    board.cell(below.0, below.1) != Cell::Empty && below != player_pos
}

/// 盤面全体を「重力の単位」ごとに分割する(spec.md 4.1・4.7、ユーザー指摘対応)。
///
/// 色ブロックは同色4方向連結グループ、岩ブロックは連結グループ(hits問わず)を、
/// それぞれ「1つの塊」として1つのVecにまとめる。同色・岩ブロックが上下左右に
/// 隣接している限り必ず同じ塊に含まれるため、支持判定・移動を後続の処理でセル単位に
/// バラして行うことはなく、「ちぎれて落ちる」ことが起きない。酸素カプセル・ダイヤは
/// 連結対象外(spec.md 2章)なので、常にサイズ1の塊として扱う。
fn collect_fall_groups(board: &Board) -> Vec<Vec<(usize, usize)>> {
    let depth_rows = board.depth_rows();
    let mut visited: HashSet<(usize, usize)> = HashSet::new();
    let mut groups = Vec::new();

    for row in 0..depth_rows {
        for col in 0..FIELD_WIDTH {
            let pos = (row, col);
            if visited.contains(&pos) {
                continue;
            }
            match board.cell(row, col) {
                Cell::Empty => {
                    visited.insert(pos);
                }
                Cell::Color(color) => {
                    let group = connected_same_color(board, pos, color);
                    visited.extend(group.iter().copied());
                    groups.push(group);
                }
                Cell::Rock { .. } => {
                    let group = connected_rock_group(board, pos);
                    visited.extend(group.iter().copied());
                    groups.push(group);
                }
                Cell::Oxygen | Cell::Diamond | Cell::Star { .. } => {
                    visited.insert(pos);
                    groups.push(vec![pos]);
                }
            }
        }
    }

    groups
}

/// `group`(1つの塊。連結グループまたは単独セル)が全体として支持されているか。
///
/// グループ内のどれか1つのセルでも「直下がグループ外の非Emptyセル」または
/// 「コース最深行」であれば、塊全体が支持されているとみなす。直下がグループ内の
/// 仲間セルである場合は、そのセル自身は支えにならない(仲間越しに、さらにその下の
/// 本当の支えを探す必要がある)。
pub(crate) fn is_group_supported(board: &Board, group: &[(usize, usize)], player_pos: (usize, usize)) -> bool {
    let depth_rows = board.depth_rows();
    let group_set: HashSet<(usize, usize)> = group.iter().copied().collect();

    group.iter().any(|&(r, c)| {
        if r + 1 >= depth_rows {
            return true;
        }
        let below = (r + 1, c);
        if group_set.contains(&below) {
            return false; // 仲間は支えにならない。他のセルの判定に委ねる
        }
        board.cell(below.0, below.1) != Cell::Empty && below != player_pos
    })
}

/// `group`が「真に安定した支え」を持つかどうか(`is_group_supported`の連鎖判定版)。
///
/// `is_group_supported`と同様に直下の非Emptyセルを支えの根拠として探すが、
/// その根拠となるセルが属する塊(`cell_to_group`で引く)が`supported`上で
/// 既に未支持と判定されている場合、その支えは「このティックで一緒に落ちてしまう
/// 不安定な支え」であり、真の支えとしては数えない。呼び出し側が収束するまで
/// 繰り返し呼ぶことで、支えの連鎖(支えの支えの支え…)を正しく伝播させる
/// (ユーザー指摘対応: 「右1列でひっかかっても2:1でちぎれて分離されることがある」)。
fn has_stable_support(
    board: &Board,
    group: &[(usize, usize)],
    cell_to_group: &HashMap<(usize, usize), usize>,
    supported: &[bool],
    player_pos: (usize, usize),
) -> bool {
    let depth_rows = board.depth_rows();
    let group_set: HashSet<(usize, usize)> = group.iter().copied().collect();

    group.iter().any(|&(r, c)| {
        if r + 1 >= depth_rows {
            return true; // 最深行は常に安定した支え
        }
        let below = (r + 1, c);
        if group_set.contains(&below) {
            return false; // 仲間は支えにならない。他のセルの判定に委ねる
        }
        if below == player_pos || board.cell(below.0, below.1) == Cell::Empty {
            return false;
        }
        match cell_to_group.get(&below) {
            Some(&group_index) => supported[group_index],
            None => true, // グループ管理外(通常は起きない)は安全側でtrue扱い
        }
    })
}

/// 論理ティック1回ぶんの重力落下処理(spec.md 4章・5章)を実行する。
///
/// - まず盤面全体を`collect_fall_groups`で「塊」(連結グループ・単独セル)に分割する。
///   同色・岩ブロックが上下左右に隣接していれば必ず同じ塊としてまとめて扱われ、
///   支持判定・移動もその塊単位で行うため、一部のセルだけがちぎれて落ちることはない
///   (ユーザー指摘対応)。
/// - 支持判定(4.2)はティック開始時点のスナップショットを基準に、全ての塊同時に行う(4.4)。
/// - 支持を失った塊は即座には落下せず、まず`shake_ticks`ぶん揺れてから落下を開始する
///   (4.3)。揺れが明けている未支持の塊だけがこのティックで1マス下へ移動する。
///   `shake_ticks`は呼び出し側(Game)が揺れ時間設定(ms、デバッグショートカットで実行時
///   調整可能・TERM独自拡張)とブロック落下tick間隔から都度換算して渡す
/// - 移動先がプレイヤー位置と重なった場合、その瞬間に押し潰し(crushed)を確定し、
///   押し潰した側の塊はその場で消滅する(spec.md 5章)。酸素カプセルだけは例外で、
///   押し潰しにはせず取得(酸素回復)扱いにする(TERM独自拡張)
/// - 移動した結果、直下が非Empty(=着地)になった色ブロック・岩ブロックの塊についてのみ、
///   4個以上なら自動消滅させる(4.5・4.9)
pub fn apply_gravity_tick(board: &mut Board, player_pos: (usize, usize), gravity: &mut GravityState, shake_ticks: u8) -> FallTickOutcome {
    let snapshot = board.clone();
    let groups = collect_fall_groups(&snapshot);

    // 各セルがどの塊(groupsのインデックス)に属するかの逆引きマップ。
    let mut cell_to_group: HashMap<(usize, usize), usize> = HashMap::new();
    for (i, group) in groups.iter().enumerate() {
        for &pos in group {
            cell_to_group.insert(pos, i);
        }
    }

    // まず素朴な支持判定(直下が非Emptyかどうか)で初期化する。
    let mut supported: Vec<bool> = groups.iter().map(|g| is_group_supported(&snapshot, g, player_pos)).collect();

    // 連鎖的な再判定(ユーザー指摘対応: 「右1列でひっかかっても2:1でちぎれて分離
    // されることがある」)。支えの根拠となっているセルが属する塊自体が、このティックで
    // 未支持(=これから落下する)と判定されているなら、その支えは実際には不安定であり、
    // 支えられている側の塊も連動して未支持にする。1段の連鎖では済まない場合があるため
    // (支えの支えの支え…)、変化が無くなるまで繰り返す。
    loop {
        let mut changed = false;
        for i in 0..groups.len() {
            if !supported[i] {
                continue;
            }
            if !has_stable_support(&snapshot, &groups[i], &cell_to_group, &supported, player_pos) {
                supported[i] = false;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut next_unsupported_ticks: HashMap<(usize, usize), u8> = HashMap::new();
    let mut next_shaking_cells: HashSet<(usize, usize)> = HashSet::new();
    let mut falling_groups: Vec<Vec<(usize, usize)>> = Vec::new();

    for (i, group) in groups.into_iter().enumerate() {
        if supported[i] {
            continue; // 支持されている = 揺れ状態も解除(next_unsupported_ticksに載せない)
        }

        // 塊の代表座標(先頭要素。`collect_fall_groups`の探索順序上、同じ塊なら常に
        // 「最小の(row, col)」で安定する)で揺れティック数を管理する。
        let representative = group[0];
        let ticks_unsupported = gravity.unsupported_ticks.get(&representative).copied().unwrap_or(0) + 1;
        if ticks_unsupported as u32 > shake_ticks as u32 {
            // 揺れが明けた(またはshake_ticks=0で即座に) -> このティックで1マス落下する
            falling_groups.push(group);
        } else {
            // まだ揺れている最中 -> 移動しない。描画用に塊の全セルを揺れ中として記録する。
            next_shaking_cells.extend(group.iter().copied());
            next_unsupported_ticks.insert(representative, ticks_unsupported);
        }
    }

    gravity.unsupported_ticks = next_unsupported_ticks;
    gravity.shaking_cells = next_shaking_cells;

    let mut outcome = FallTickOutcome::default();
    if falling_groups.is_empty() {
        return outcome;
    }

    // 浅い深度(行番号が小さい)側の塊から処理する。これにより下段の塊が先に動いて
    // 隙間を作り、その結果上段の塊も同ティックで連動して動いてしまう事故を防ぐ
    // (spec.md 4章手順6のグループ版)。
    falling_groups.sort_by_key(|g| g.iter().map(|&(r, _)| r).min().unwrap_or(0));

    // 先に全ての塊の旧位置をまとめてEmptyにしてから、新位置への書き込みへ進む。
    // 複数の塊が縦に連なって同時に落下する場合(例: 塊Aの新位置が塊Bの旧位置と
    // 一致するケース)、塊ごとに「旧位置クリア→新位置書き込み」を順番に行うと、
    // 後から処理する塊のクリアが先に書き込んだ別の塊を消してしまう事故が起きる
    // (ユーザー指摘「右1列でひっかかっても2:1でちぎれて分離される」の連鎖判定を
    // 追加した際に顕在化したバグ)。spec.md 4章手順5の「先に旧位置を全てEmptyに」
    // という配慮を、単一の塊内だけでなく全ての塊を跨いで徹底する。
    for group in &falling_groups {
        for &(r, c) in group {
            board.set(r, c, Cell::Empty);
        }
    }

    for group in &falling_groups {
        outcome.cells_moved += group.len();

        // 各セルの内容(色ブロックのColorKind・岩ブロックのhits)は`snapshot`から
        // セルごとに個別取得する。グループ代表セル1つの内容を全セルへ使い回すと、
        // 岩ブロックのようにセルごとに異なる付随データ(hits)を持つ塊で、着地後に
        // 全セルが代表セルのhitsで上書きされてしまうバグになる
        // (発見: 岩ブロック非自動消滅化のテストで顕在化)。
        let mut crushed_in_group = false;
        for &(r, c) in group {
            let cell = snapshot.cell(r, c);
            let to = (r + 1, c);
            if to == player_pos {
                // 落下してきたセルはその場で消滅する(spec.md 5章)。
                if cell == Cell::Oxygen {
                    // 酸素カプセル(AIR)だけは例外で、掘削・自由落下時の「歩くだけで取得」と
                    // 同様に押し潰し判定にせず取得(酸素回復)扱いにする(TERM独自拡張。
                    // ユーザー指摘「上から降ってきたAIRで回復してないバグ」の修正)。
                    outcome.oxygen_collected += 1;
                } else {
                    // 押し潰した側のセルが消滅する(得点は発生しない)。
                    crushed_in_group = true;
                }
            } else {
                board.set(to.0, to.1, cell);
            }
        }

        if crushed_in_group {
            outcome.crushed = true;
            // 押し潰し確定=即ミスなので、以降の塊の処理を続ける実益はない。
            break;
        }
    }

    if outcome.crushed {
        return outcome;
    }

    // 着地(=移動先で直下が支持状態になった)色ブロック・岩ブロックについて連結・自動消滅を
    // 判定する(spec.md 4.5・4.9)。岩ブロックも4連結以上になれば自動消滅するが得点は
    // 発生しない。ただし「掘削(hit_rock)で1回ヒットした際に消えるのはそのセル1個だけ」
    // という別ルール(ユーザー指摘: 「Xブロックは結合してても全体が消えるのではなく
    // 1ブロックしか消せない」)とは独立していて、こちらは「支えを失って落下し、着地して
    // 4個以上連結した場合」にのみ働く自動消滅(ユーザー指摘: 「4個以上結合したら
    // ちゃんと消えないといけない」)。
    for group in &falling_groups {
        let moved_group: Vec<(usize, usize)> = group.iter().map(|&(r, c)| (r + 1, c)).collect();
        let Some(&to) = moved_group.first() else { continue };
        if board.cell(to.0, to.1) == Cell::Empty {
            continue; // 押し潰しでこの塊自体が消滅済み
        }
        if !is_group_supported(board, &moved_group, player_pos) {
            continue; // まだ落下中(次ティック以降に改めて着地判定する)
        }
        match board.cell(to.0, to.1) {
            Cell::Color(color) => {
                let vanish_group = connected_same_color(board, to, color);
                if vanish_group.len() >= 4 {
                    for &(vr, vc) in &vanish_group {
                        board.set(vr, vc, Cell::Empty);
                    }
                    outcome.auto_vanished_blocks += vanish_group.len();
                }
            }
            Cell::Rock { .. } => {
                let vanish_group = connected_rock_group(board, to);
                if vanish_group.len() >= 4 {
                    for &(vr, vc) in &vanish_group {
                        board.set(vr, vc, Cell::Empty);
                    }
                    outcome.auto_vanished_rock_blocks += vanish_group.len();
                }
            }
            _ => {}
        }
    }

    outcome
}

/// プレイヤーの画面内(行±`STAR_VISIBLE_RANGE_ROWS`)にあるスターブロックを1ティック分
/// 溶かし、`STAR_MELT_TICKS`に達したものは消す(TERM独自拡張。ユーザー指摘:
/// 「画面内にきたら、溶けて自然と消えるスターブロックも欲しい」)。画面外のスター
/// ブロックは溶解が進行しない。戻り値は消滅したスターブロックの個数。
pub fn tick_star_melting(board: &mut Board, player_row: usize) -> usize {
    let range = crate::constants::STAR_VISIBLE_RANGE_ROWS;
    let row_start = player_row.saturating_sub(range);
    let row_end = (player_row + range).min(board.depth_rows().saturating_sub(1));
    let mut melted = 0;

    for r in row_start..=row_end {
        for c in 0..FIELD_WIDTH {
            if let Cell::Star { melting } = board.cell(r, c) {
                if melting + 1 >= STAR_MELT_TICKS {
                    board.set(r, c, Cell::Empty);
                    melted += 1;
                } else {
                    board.set(r, c, Cell::Star { melting: melting + 1 });
                }
            }
        }
    }

    melted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::SHAKE_TICKS;

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
    fn rock_break_on_fifth_hit_vanishes_only_the_hit_block() {
        // ユーザー指摘: 「Xブロックは結合してても全体が消えるのではなく1ブロックしか
        // 消せないものとする」。連結している他の岩ブロックは、hitsに関わらず影響を
        // 受けずそのまま残る(色ブロックとは違うルール)。
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Rock { hits: ROCK_HITS_TO_BREAK - 1 }; // あと1発で破壊
        board.rows[0][1] = Cell::Rock { hits: 0 }; // 連結していても巻き込まれない
        board.rows[1][0] = Cell::Rock { hits: 2 }; // 同上
        board.rows[0][2] = Cell::Color(ColorKind::Red); // 別種、巻き込まれない

        let result = hit_rock(&mut board, (0, 0)).unwrap();

        assert_eq!(result, RockHitResult::Destroyed { blocks: 1 });
        assert_eq!(board.cell(0, 0), Cell::Empty);
        assert_eq!(board.cell(0, 1), Cell::Rock { hits: 0 }, "連結していた岩は影響を受けない");
        assert_eq!(board.cell(1, 0), Cell::Rock { hits: 2 }, "連結していた岩は影響を受けない");
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
            let outcome = apply_gravity_tick(&mut board, (99, 99), &mut gravity, SHAKE_TICKS);
            assert_eq!(outcome.cells_moved, 0);
            assert!(gravity.is_shaking((0, 0)));
        }
        assert_eq!(board.cell(0, 0), Cell::Color(ColorKind::Red));

        // SHAKE_TICKS+1ティック目で実際に1マス落下する
        let outcome = apply_gravity_tick(&mut board, (99, 99), &mut gravity, SHAKE_TICKS);
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

        apply_gravity_tick(&mut board, (99, 99), &mut gravity, SHAKE_TICKS);

        assert!(!gravity.is_shaking((0, 0)));
    }

    /// テスト用ヘルパー: `SHAKE_TICKS`ぶん揺れティックを消化してから、実際に落下する
    /// ティックを1回実行する(4.3のテストで繰り返し使う定型パターン)。
    fn shake_out_then_tick(board: &mut Board, player_pos: (usize, usize), gravity: &mut GravityState) -> FallTickOutcome {
        for _ in 0..SHAKE_TICKS {
            apply_gravity_tick(board, player_pos, gravity, SHAKE_TICKS);
        }
        apply_gravity_tick(board, player_pos, gravity, SHAKE_TICKS)
    }

    // --- 支えの連鎖判定(ユーザー指摘対応) ---

    #[test]
    fn group_supported_only_by_a_currently_unsupported_group_falls_together_in_the_same_tick() {
        // 上段(Blue,col0)は下段(Red,col0)に直接乗っている。下段自体もその下(row2)が
        // 空洞で未支持。支えの根拠(下段)がこのティックで一緒に落下対象になっている
        // 場合、その支えは「不安定」であり、上段も連鎖的に未支持と判定され、両方とも
        // ちぎれずに同じティックで1マス落下する(ユーザー指摘:「右1列でひっかかっても
        // 2:1でちぎれて分離されることがある」の直接的な回帰防止テスト)。
        let mut board = empty_board(4);
        board.rows[0][0] = Cell::Color(ColorKind::Blue);
        board.rows[1][0] = Cell::Color(ColorKind::Red);
        let mut gravity = GravityState::new();

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity);

        assert_eq!(outcome.cells_moved, 2, "上段・下段とも同じティックで一緒に落下するはず");
        assert_eq!(board.cell(0, 0), Cell::Empty);
        assert_eq!(board.cell(1, 0), Cell::Color(ColorKind::Blue));
        assert_eq!(board.cell(2, 0), Cell::Color(ColorKind::Red));
    }

    #[test]
    fn group_supported_by_a_truly_stable_group_does_not_fall() {
        // 対比: 下段(Red,col0)が最深行にあり本当に安定している場合、上段(Blue)も
        // 支持され、どちらも落ちない。
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Color(ColorKind::Blue);
        board.rows[1][0] = Cell::Color(ColorKind::Red);
        let mut gravity = GravityState::new();

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity);

        assert_eq!(outcome.cells_moved, 0);
        assert_eq!(board.cell(0, 0), Cell::Color(ColorKind::Blue));
        assert_eq!(board.cell(1, 0), Cell::Color(ColorKind::Red));
    }

    #[test]
    fn support_chain_of_three_unsupported_groups_all_fall_together() {
        // 3段連鎖: 上(Green)→中(Blue)→下(Red)の順に乗っており、下段の直下(row3)が
        // 空洞。3つとも支えを辿ると最終的に不安定なので、全部同じティックで
        // 1マス落下する(支えの連鎖が1段では止まらないケースの確認)。
        let mut board = empty_board(4);
        board.rows[0][0] = Cell::Color(ColorKind::Green);
        board.rows[1][0] = Cell::Color(ColorKind::Blue);
        board.rows[2][0] = Cell::Color(ColorKind::Red);
        let mut gravity = GravityState::new();

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity);

        assert_eq!(outcome.cells_moved, 3);
        assert_eq!(board.cell(1, 0), Cell::Color(ColorKind::Green));
        assert_eq!(board.cell(2, 0), Cell::Color(ColorKind::Blue));
        assert_eq!(board.cell(3, 0), Cell::Color(ColorKind::Red));
    }

    #[test]
    fn isolated_single_color_block_falls_when_unsupported() {
        // 孤立した(周囲に同色が無い)単独ブロックでも、支えを失えば普通に落下する
        // (4個以上でないと自動消滅しないだけで、落下自体はグループサイズを問わない)。
        // ユーザー報告「キャラの右上の赤ブロックが落ちないのはおかしい」の再現確認。
        let mut board = empty_board(3);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        let mut gravity = GravityState::new();

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity);

        assert_eq!(outcome.cells_moved, 1, "孤立していても支えが無ければ落ちるはず");
        assert_eq!(board.cell(0, 0), Cell::Empty);
        assert_eq!(board.cell(1, 0), Cell::Color(ColorKind::Red));
    }

    #[test]
    fn isolated_block_falls_even_when_flanked_by_stable_different_color_groups() {
        // 孤立した赤ブロックの左右に、最深行まで届いていて安定している別色(青)の
        // グループがあっても、赤ブロック自身の直下がEmptyなら独立して落下するはず。
        // ユーザー報告「キャラの右上の赤ブロックが落ちないのはおかしい」の再現確認
        // (周囲が別グループで安定しているケース)。
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Color(ColorKind::Blue);
        board.rows[0][1] = Cell::Color(ColorKind::Red);
        board.rows[0][2] = Cell::Color(ColorKind::Blue);
        board.rows[1][0] = Cell::Color(ColorKind::Blue); // 最深行、左側の支え
        board.rows[1][2] = Cell::Color(ColorKind::Blue); // 最深行、右側の支え
        let mut gravity = GravityState::new();

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity);

        assert_eq!(outcome.cells_moved, 1, "赤ブロックだけが落ちるはず");
        assert_eq!(board.cell(0, 1), Cell::Empty);
        assert_eq!(board.cell(1, 1), Cell::Color(ColorKind::Red));
        // 青グループは安定したまま動かない
        assert_eq!(board.cell(0, 0), Cell::Color(ColorKind::Blue));
        assert_eq!(board.cell(0, 2), Cell::Color(ColorKind::Blue));
    }

    #[test]
    fn stacked_same_color_blocks_form_one_group_and_fall_together() {
        // 縦に連結した同色ブロックは1つの塊として扱われ、支えを失うと全体が
        // ちぎれずに一緒に1マス落下する(ユーザー指摘対応: 「落下中に同じ色の
        // ブロックの結合がきれて、ちぎれて落ちることはない。ちゃんと同じ色ブロックが
        // 上下左右に隣接したら必ず結合する」)。
        let mut board = empty_board(5);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        board.rows[1][0] = Cell::Color(ColorKind::Red);
        board.rows[2][0] = Cell::Color(ColorKind::Red);
        let mut gravity = GravityState::new();

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity); // 3セル全体が一緒に落下

        assert_eq!(outcome.cells_moved, 3);
        assert_eq!(board.cell(0, 0), Cell::Empty);
        assert_eq!(board.cell(1, 0), Cell::Color(ColorKind::Red));
        assert_eq!(board.cell(2, 0), Cell::Color(ColorKind::Red));
        assert_eq!(board.cell(3, 0), Cell::Color(ColorKind::Red), "3つ目もちぎれずに一緒に1マス落下しているはず");
    }

    #[test]
    fn horizontal_group_of_three_supported_only_under_rightmost_cell_falls_together_without_tearing() {
        // 横3列(col0,1,2)の同色グループ。col2の直下(row1)だけがEmptyで、その
        // さらに1つ下(row2,col2)に支え(岩)がある。col0・col1の直下(row1)は
        // Emptyのまま。グループ全体で見ればcol2経由でまだ支持されていないので、
        // 3つとも一緒に1マス落下するはず(ユーザー指摘: 「右1列でひっかかっても
        // 2:1でちぎれて分離されることがある」の再現・回帰防止)。
        // 支え(岩)は最深行に置き、支え自身が未支持で一緒に落ちてしまわないようにする。
        let mut board = empty_board(3);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        board.rows[0][1] = Cell::Color(ColorKind::Red);
        board.rows[0][2] = Cell::Color(ColorKind::Red);
        board.rows[2][2] = Cell::Rock { hits: 0 }; // col2は2マス下(最深行)にしか支えが無い(1マス下row1は空洞)
        let mut gravity = GravityState::new();

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity);

        assert_eq!(outcome.cells_moved, 3, "3つとも一緒に1マス落下するはず(ちぎれない)");
        assert_eq!(board.cell(0, 0), Cell::Empty);
        assert_eq!(board.cell(0, 1), Cell::Empty);
        assert_eq!(board.cell(0, 2), Cell::Empty);
        assert_eq!(board.cell(1, 0), Cell::Color(ColorKind::Red));
        assert_eq!(board.cell(1, 1), Cell::Color(ColorKind::Red));
        assert_eq!(board.cell(1, 2), Cell::Color(ColorKind::Red));
    }

    #[test]
    fn horizontal_group_of_three_supported_directly_under_rightmost_cell_does_not_fall() {
        // 上のテストとの対比: col2の直下(row1、最深行)に直接支え(岩)がある場合は、
        // グループ全体が支持されているので誰も落ちない。
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        board.rows[0][1] = Cell::Color(ColorKind::Red);
        board.rows[0][2] = Cell::Color(ColorKind::Red);
        board.rows[1][2] = Cell::Rock { hits: 0 };
        let mut gravity = GravityState::new();

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity);

        assert_eq!(outcome.cells_moved, 0);
        assert_eq!(board.cell(0, 0), Cell::Color(ColorKind::Red));
        assert_eq!(board.cell(0, 1), Cell::Color(ColorKind::Red));
        assert_eq!(board.cell(0, 2), Cell::Color(ColorKind::Red));
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
        // ユーザー指摘: 「4個以上結合したらちゃんと消えないといけない」。岩ブロックも
        // 色ブロックと同様、着地して4連結以上になれば自動消滅する(ただし得点は無し)。
        // これは「掘削(hit_rock)で消えるのは1ブロックのみ」という別ルールとは独立している。
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
            let outcome = apply_gravity_tick(&mut board, (99, 99), &mut gravity, SHAKE_TICKS);
            assert_eq!(outcome.cells_moved, 0);
            assert!(gravity.is_shaking((0, 0)));
        }
        assert!(matches!(board.cell(0, 0), Cell::Rock { hits: 2 }), "揺れている間はまだ落下しない");

        let outcome = apply_gravity_tick(&mut board, (99, 99), &mut gravity, SHAKE_TICKS);
        assert_eq!(outcome.cells_moved, 1);
        assert_eq!(board.cell(0, 0), Cell::Empty);
        assert!(matches!(board.cell(1, 0), Cell::Rock { hits: 2 }), "落下してもhitsは保持される");
        assert!(!gravity.is_shaking((1, 0)), "着地して支持されればもう揺れていない");
    }

    #[test]
    fn falling_rock_group_merges_with_supported_rock_group_below_and_auto_vanishes() {
        // ユーザー指摘: 「4個以上結合したらちゃんと消えないといけない」。支えを失った
        // 岩が震え→落下→支持されている岩ブロックに接触して連結し、合計4個以上に
        // なった時点で自動消滅する(得点は無し。「掘削(hit_rock)で消えるのは1ブロック
        // のみ」とは独立したルール)。
        let mut board = empty_board(3);
        board.rows[2][0] = Cell::Rock { hits: 1 };
        board.rows[2][1] = Cell::Rock { hits: 3 };
        board.rows[0][0] = Cell::Rock { hits: 4 }; // あと1発で破壊されるはずだった岩
        board.rows[0][1] = Cell::Rock { hits: 0 };
        let mut gravity = GravityState::new();

        assert_eq!(connected_rock_group(&board, (2, 0)).len(), 2, "接触前は支持グループのみ2個");

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity);

        assert_eq!(outcome.auto_vanished_rock_blocks, 4);
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

        let outcome = apply_gravity_tick(&mut board, (99, 99), &mut gravity, SHAKE_TICKS);

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

        apply_gravity_tick(&mut board, (99, 99), &mut gravity, SHAKE_TICKS); // 揺れ
        let outcome = apply_gravity_tick(&mut board, (99, 99), &mut gravity, SHAKE_TICKS); // 支持判定

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
    fn no_group_remains_unsupported_forever_on_random_boards() {
        // ユーザー報告「孤立した赤ブロックが落ちてこない」の統計的な回帰検証。
        // ランダム生成した盤面を十分な回数ティックさせ、最終的に全ての塊が
        // 「支持されている(=これ以上は落ちるはずがない)」状態に収束することを
        // 確認する。収束せず未支持のまま残る塊があれば、揺れ/連鎖判定のどこかで
        // 永続的に浮いたままになるバグがあることを意味する。
        for seed in 0..20u64 {
            let mut board = Board::generate(seed, 60);
            let mut gravity = GravityState::new();
            let player_pos = (usize::MAX, usize::MAX); // 影響しない盤外位置

            for _ in 0..(SHAKE_TICKS as usize + 1) * 60 {
                apply_gravity_tick(&mut board, player_pos, &mut gravity, SHAKE_TICKS);
            }

            for group in collect_fall_groups(&board) {
                assert!(
                    is_group_supported(&board, &group, player_pos),
                    "seed={seed}: 十分な時間が経っても未支持のまま残っている塊がある: {group:?}"
                );
            }
        }
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

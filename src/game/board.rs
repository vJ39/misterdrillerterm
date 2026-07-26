//! フィールド生成・ブロック配置・連結判定・重力落下ロジック(spec.md 2〜4章)。
//!
//! ratatui/crossterm/rodio の副作用を一切持たない純粋なデータ構造・関数のみで構成する。

use std::collections::HashMap;
use std::collections::HashSet;

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;

use crate::constants::{
    depth_fraction, COLOR_CLUSTER_DEPTH_START_PROB, ROCK_CLUSTER_DEPTH_MAX_BONUS, ROCK_HITS_TO_BREAK,
    STAR_MELT_DURATION_MS, STAR_VISIBLE_GRACE_MS,
};

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
    /// 消えるスターブロックも欲しい」)。プレイヤーの可視範囲内に入ると、`STAR_VISIBLE_
    /// GRACE_MS`(既定5秒、ユーザー指摘: 「画面内に見えてから5秒たったら消えはじめる
    /// こと」)は無傷のまま、その後`STAR_MELT_DURATION_MS`かけて溶けて消える。
    /// `visible_ms`は画面内に入ってからの経過時間(ms)。ブロック落下tickの間隔(深度
    /// によって変わる、TERM独自拡張)とは独立した実時間で管理するため、tick数ではなく
    /// 経過ミリ秒で持つ。掘削・連結落下の対象外(常に単独・固定、酸素カプセル等と同様)。
    Star { visible_ms: u32 },
    /// アイテムブロック(TERM独自拡張。ユーザー指摘: 「ショートカットRと同じ効果の
    /// あるアイテムつくろ」「ショートカットC効果のアイテムも作って」)。ドリルで
    /// 取得すると即座に対応するデバッグショートカットと同じ効果が発動する。ダイヤ・
    /// スター同様、連結せず常に単独の塊として落下する。
    Item(ItemEffect),
}

/// アイテムブロック取得時に発動する効果の種別(TERM独自拡張)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemEffect {
    /// ショートカットRと同じ: プレイヤーより上のブロックを全削除する(AIRは残す)
    ClearAbove,
    /// ショートカットCと同じ: プレイヤー付近の色ブロックをランダムな2色に統一する
    UnifyColors,
    /// ショートカットKと同じ: 画面内のXブロック・ダイヤブロックを100%スター化する
    StarifyScreen,
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
fn generate_base_colors(rng: &mut ChaCha8Rng, depth_rows: usize, width: usize) -> Vec<Vec<Option<ColorKind>>> {
    let mut base: Vec<Vec<Option<ColorKind>>> = vec![vec![None; width]; depth_rows];

    for row in 2..depth_rows {
        for col in 0..width {
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
fn same_color_neighbor_candidates(base: &[Vec<Option<ColorKind>>], row: usize, col: usize) -> Vec<ColorKind> {
    let rows = base.len();
    let width = base[row].len();
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
    if col + 1 < width
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
fn fix_isolated_cells(base: &mut [Vec<Option<ColorKind>>]) {
    let rows = base.len();
    for row in 0..rows {
        let width = base[row].len();
        for col in 0..width {
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
fn overlay_rock_oxygen_diamond(
    rng: &mut ChaCha8Rng,
    base_color: ColorKind,
    row: usize,
    item_caps: &mut ItemSpawnCaps,
) -> Cell {
    overlay_rock_oxygen_diamond_with_rates(
        rng,
        base_color,
        row,
        crate::constants::SPAWN_RATE_PERCENT_DEFAULT,
        crate::constants::SPAWN_RATE_PERCENT_DEFAULT,
        crate::constants::SPAWN_RATE_PERCENT_DEFAULT,
        crate::constants::SPAWN_RATE_PERCENT_DEFAULT,
        crate::constants::SPAWN_RATE_PERCENT_DEFAULT,
        crate::constants::SPAWN_RATE_PERCENT_DEFAULT,
        crate::constants::SPAWN_RATE_PERCENT_DEFAULT,
        0.0,
        true, // 初期生成の候補は常に「まだ色ブロック」なのでスター抽選の対象
        item_caps,
    )
}

/// アイテムブロック3種の「盤面全体であと何個まで出現させてよいか」の残数
/// (TERM独自拡張。ユーザー指摘: 「各種アイテムのキャラより上の位置には最大それぞれ
/// 10個までとする」)。呼び出し側(`Board::generate`・`reroll_overlays_from_row`)が
/// 処理開始前に盤面全体の既存個数を数えて`ITEM_MAX_COUNT_ON_BOARD`との差分で初期化し、
/// 新たに1個出現させるたびに対応するフィールドを1減らす。0になったらその種類は
/// それ以上出現しなくなる。
struct ItemSpawnCaps {
    clear_above_remaining: usize,
    unify_colors_remaining: usize,
    starify_screen_remaining: usize,
}

impl ItemSpawnCaps {
    fn from_existing_counts(board: &Board) -> Self {
        let max = crate::constants::ITEM_MAX_COUNT_ON_BOARD;
        ItemSpawnCaps {
            clear_above_remaining: max.saturating_sub(board.count_item(ItemEffect::ClearAbove)),
            unify_colors_remaining: max.saturating_sub(board.count_item(ItemEffect::UnifyColors)),
            starify_screen_remaining: max.saturating_sub(board.count_item(ItemEffect::StarifyScreen)),
        }
    }

    /// まだ盤面に1個も無い状態からの新規生成(`Board::generate`)用。既存個数は常に0。
    fn fresh() -> Self {
        let max = crate::constants::ITEM_MAX_COUNT_ON_BOARD;
        ItemSpawnCaps {
            clear_above_remaining: max,
            unify_colors_remaining: max,
            starify_screen_remaining: max,
        }
    }
}

/// `overlay_rock_oxygen_diamond`の、岩/AIR/スター/ダイヤ・アイテム3種の出現率を
/// 設定値(%、100=通常のまま)で調整できる版(TERM独自拡張。ユーザー指摘: 「設定で
/// Xブロックの配分量・AIRの配分量をいじれるようにしたい」「スターブロック比率0〜」
/// 「ダイヤブロック0%設定」「各種アイテムの出現頻度の設定項目増やして」)。
///
/// `rock_cluster_bonus`(0.0〜)は岩の出現確率へ加算するボーナス(TERM独自拡張。
/// ユーザー指摘: 「Xブロックが結合で大量にあったりするように」)。呼び出し側が
/// 隣接セルが岩かどうか・深度に応じて算出する。`Board::generate`(初期生成、後で
/// `reroll_overlays_from_row`により上書きされる)では常に0.0を渡し無効化する。
///
/// `allow_star`がfalseの場合、スターへの抽選そのものを無効化する(TERM独自拡張。
/// ユーザー指摘: 「スターブロックに変わる対象ブロックは色ブロックのみで、Xブロック、
/// ダイヤブロック、AIRは対象外とする」「ぐらぐら/落下中のブロックはスターブロックへの
/// 変化対象外とする」)。呼び出し側(`reroll_overlays_from_row`)が、再抽選対象セルが
/// 元々色ブロック(またはスター自身)だったか、揺れ中/落下中でないかを判定して渡す。
#[allow(clippy::too_many_arguments)]
fn overlay_rock_oxygen_diamond_with_rates(
    rng: &mut ChaCha8Rng,
    base_color: ColorKind,
    row: usize,
    rock_rate_percent: u32,
    air_rate_percent: u32,
    star_rate_percent: u32,
    diamond_rate_percent: u32,
    item_clear_above_rate_percent: u32,
    item_unify_colors_rate_percent: u32,
    item_starify_screen_rate_percent: u32,
    rock_cluster_bonus: f32,
    allow_star: bool,
    item_caps: &mut ItemSpawnCaps,
) -> Cell {
    let mut t = band_table(row);
    t.rock = (t.rock * rock_rate_percent as f32 / 100.0 + rock_cluster_bonus).clamp(0.0, 0.9);
    t.oxygen = (t.oxygen * air_rate_percent as f32 / 100.0).clamp(0.0, 0.9);
    t.star = if allow_star {
        (t.star * star_rate_percent as f32 / 100.0).clamp(0.0, 0.9)
    } else {
        0.0
    };
    t.diamond = (t.diamond * diamond_rate_percent as f32 / 100.0).clamp(0.0, 0.9);
    // アイテムブロック3種(#98/#101/#107)は岩/AIR/スター/ダイヤの配分率設定とは独立した
    // ごく低確率の値だが、他ブロック同様に設定画面から個別調整できる(TERM独自拡張。
    // ユーザー指摘: 「各種アイテムの出現頻度の設定項目増やして」)。盤面全体で
    // `ITEM_MAX_COUNT_ON_BOARD`個に達した種類は、それ以上抽選対象にしない(TERM独自
    // 拡張。ユーザー指摘: 「各種アイテムのキャラより上の位置には最大それぞれ10個
    // までとする」)。
    let item_clear_above = if item_caps.clear_above_remaining > 0 {
        crate::constants::ITEM_CLEAR_ABOVE_SPAWN_PROB * item_clear_above_rate_percent as f32 / 100.0
    } else {
        0.0
    };
    let item_unify_colors = if item_caps.unify_colors_remaining > 0 {
        crate::constants::ITEM_UNIFY_COLORS_SPAWN_PROB * item_unify_colors_rate_percent as f32 / 100.0
    } else {
        0.0
    };
    let item_starify_screen = if item_caps.starify_screen_remaining > 0 {
        crate::constants::ITEM_STARIFY_SCREEN_SPAWN_PROB * item_starify_screen_rate_percent as f32 / 100.0
    } else {
        0.0
    };

    // ルーレット式抽選(TERM独自拡張)。各候補の確率ぶんを順に積み上げ、rが最初に
    // 収まった区間を採用する。
    let r: f32 = rng.random_range(0.0..1.0);
    let mut threshold = t.rock;
    if r < threshold {
        return Cell::Rock { hits: 0 };
    }
    threshold += t.oxygen;
    if r < threshold {
        return Cell::Oxygen;
    }
    threshold += t.diamond;
    if r < threshold {
        return Cell::Diamond;
    }
    threshold += t.star;
    if r < threshold {
        return Cell::Star { visible_ms: 0 };
    }
    threshold += item_clear_above;
    if r < threshold {
        item_caps.clear_above_remaining -= 1;
        return Cell::Item(ItemEffect::ClearAbove);
    }
    threshold += item_unify_colors;
    if r < threshold {
        item_caps.unify_colors_remaining -= 1;
        return Cell::Item(ItemEffect::UnifyColors);
    }
    threshold += item_starify_screen;
    if r < threshold {
        item_caps.starify_screen_remaining -= 1;
        return Cell::Item(ItemEffect::StarifyScreen);
    }
    Cell::Color(base_color)
}

/// 行内の全マスが岩ブロックになっている場合、少なくとも1マスを色ブロックへ
/// 差し替える(TERM独自拡張。ユーザー指摘: 「Xブロック配置のとき横一列全部埋まる
/// 配置にはならないように」)。岩ブロックだけで完全にふさがった横一列は掘削しないと
/// 絶対に通過できない壁になってしまうため、必ず逃げ道を1マス残す。
fn ensure_row_is_not_fully_blocked_by_rock(row_cells: &mut [Cell], rng: &mut ChaCha8Rng, color_count: usize) {
    if row_cells.iter().all(|c| matches!(c, Cell::Rock { .. })) {
        let escape_col = rng.random_range(0..row_cells.len());
        let escape_color = ColorKind::ALL[rng.random_range(0..color_count)];
        row_cells[escape_col] = Cell::Color(escape_color);
    }
}

/// ゲームフィールド全体(1000行×`width`列)。`width`(列数)はTERM独自拡張で設定
/// 可能(ユーザー指摘: 「設定値に列の数を変更できるようにして」)。新規ゲーム開始時に
/// 決まり、以後そのゲームの間は固定(既存の`rows`各要素の長さと必ず一致する)。
#[derive(Debug, Clone)]
pub struct Board {
    pub rows: Vec<Vec<Cell>>,
    pub width: usize,
}

impl Board {
    /// 乱数シードから深さ depth_rows 行×width列ぶんのフィールドを事前生成する
    /// (spec.md 3.6)。
    ///
    /// 手順: 3.2〜3.3の下地生成を全行分行い、3.4の孤立セル解消を盤面全体に1回、
    /// 最後に3.5の上書きを全マスに適用する。この順序で1回だけ行い、生成し直しはしない。
    pub fn generate(seed: u64, depth_rows: usize, width: usize) -> Self {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);

        let mut base = generate_base_colors(&mut rng, depth_rows, width);
        fix_isolated_cells(&mut base);

        // 新規生成の盤面はまだアイテムを1つも含まないため、常に上限いっぱいから始める
        // (TERM独自拡張。ユーザー指摘: 「各種アイテムのキャラより上の位置には最大
        // それぞれ10個までとする」)。
        let mut item_caps = ItemSpawnCaps::fresh();
        let rows = (0..depth_rows)
            .map(|row| {
                let mut cells = vec![Cell::Empty; width];
                for (col, cell) in cells.iter_mut().enumerate() {
                    *cell = match base[row][col] {
                        None => Cell::Empty, // 安全地帯(深度0〜1m)
                        Some(color) => overlay_rock_oxygen_diamond(&mut rng, color, row, &mut item_caps),
                    };
                }
                ensure_row_is_not_fully_blocked_by_rock(&mut cells, &mut rng, ColorKind::ALL.len());
                cells
            })
            .collect();

        Board { rows, width }
    }

    pub fn depth_rows(&self) -> usize {
        self.rows.len()
    }

    /// フィールド幅(列数)。新規ゲーム開始時に決まり、以後そのゲームの間は固定
    /// (TERM独自拡張。ユーザー指摘: 「設定値に列の数を変更できるようにして」)。
    pub fn width(&self) -> usize {
        self.width
    }

    pub fn cell(&self, row: usize, col: usize) -> Cell {
        self.rows[row][col]
    }

    pub fn set(&mut self, row: usize, col: usize, cell: Cell) {
        self.rows[row][col] = cell;
    }

    /// 盤面全体(全行)に存在する、指定した効果のアイテムブロックの個数(TERM独自拡張。
    /// ユーザー指摘: 「各種アイテムのキャラより上の位置には最大それぞれ10個までとする」)。
    /// 出現数の上限を判定するために使う。
    fn count_item(&self, effect: ItemEffect) -> usize {
        self.rows.iter().flatten().filter(|c| matches!(c, Cell::Item(e) if *e == effect)).count()
    }

    /// `from_row`以降の未掘削マス(`Cell::Empty`以外の全セル)について、色・岩(X)・
    /// AIR・スター・ダイヤの内訳を指定の配分率(%、100=通常のまま)で丸ごと
    /// 再抽選する(TERM独自拡張。ユーザー指摘: 「設定でXブロックの配分量・AIRの配分量を
    /// いじれるようにしたい。プレイ中でもその数値をいじれるようにしたい」「ダイヤブロック
    /// 0%設定」)。
    ///
    /// 最初の`Board::generate`は常に既定率(100%)で岩/AIR/スター/ダイヤを配置するため、
    /// 「まだ`Cell::Color`のセルだけ」を対象にすると、既にRock/Oxygen/Star/Diamond
    /// として確定していたセルは設定を0%にしても永久に残ってしまう(事故: 「0%に設定
    /// したのに、存在するとか」)。この関数は未掘削マスであれば元の内容を問わず
    /// 新しい乱数色を割り当てた上で再抽選するため、既に確定していたセルも含めて
    /// 正しく反映される。`Cell::Empty`(既に掘削済み・落下等で空になったマス)だけは、
    /// 実際にプレイヤーが見た/触れた状態を壊さないよう対象外にする。再現性は求めない
    /// ため、呼び出しごとに新しい乱数系列を使う。
    ///
    /// `color_count`(1〜4)は新しい色ブロックの抽選を`ColorKind::ALL`の先頭からこの数
    /// だけに制限する(TERM独自拡張。ユーザー指摘: 「出現する色ブロックの色数を設定で
    /// 選べるようにしたい(1〜4)」)。範囲外の値は1〜4にクランプする。
    ///
    /// 深度が進むほど、色ブロックの初期配置はまとまりが弱くなり(左隣の色を継承する
    /// 確率が下がる)、岩ブロックは逆にまとまりやすくなる(隣接岩ブロックがあると
    /// 出現確率にボーナスが乗る)(TERM独自拡張の難易度カーブ。ユーザー指摘: 「初期
    /// 配置されるブロックがあまり結合状態になく、個別でばらばらであり、Xブロックが
    /// 結合で大量にあったりするように」)。
    ///
    /// `color_cluster_rate_percent`(%、100=通常のまま)は色ブロックの結合しやすさ
    /// (深度カーブの起点値`COLOR_CLUSTER_DEPTH_START_PROB`)に乗算する係数(TERM独自
    /// 拡張。ユーザー指摘: 「ブロック配置の結合関係の割合を設定できるようにして」)。
    /// 0%なら深度に関わらず常に完全ランダム抽選(結合ゼロ)になる。
    #[allow(clippy::too_many_arguments)]
    pub fn reroll_overlays_from_row(
        &mut self,
        from_row: usize,
        rock_rate_percent: u32,
        air_rate_percent: u32,
        star_rate_percent: u32,
        diamond_rate_percent: u32,
        item_clear_above_rate_percent: u32,
        item_unify_colors_rate_percent: u32,
        item_starify_screen_rate_percent: u32,
        color_count: u8,
        color_cluster_rate_percent: u32,
        gravity: &GravityState,
    ) {
        use rand::RngExt;
        let seed: u64 = rand::rng().random();
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let color_count = (color_count as usize).clamp(1, ColorKind::ALL.len());
        // 盤面全体(from_rowより前の既存アイテムも含む)の既存個数を先に数えてから
        // 上限を計算する(TERM独自拡張。ユーザー指摘: 「各種アイテムのキャラより上の
        // 位置には最大それぞれ10個までとする」)。
        let mut item_caps = ItemSpawnCaps::from_existing_counts(self);

        for row in from_row..self.rows.len() {
            let fraction = depth_fraction(row);
            let color_cluster_prob = (COLOR_CLUSTER_DEPTH_START_PROB
                * (1.0 - fraction)
                * (color_cluster_rate_percent as f32 / 100.0))
                .clamp(0.0, 1.0);
            let rock_cluster_bonus_if_adjacent = ROCK_CLUSTER_DEPTH_MAX_BONUS * fraction;

            for col in 0..self.width {
                let current = self.rows[row][col];
                // アイテムブロックは配分率再抽選の対象外にする(TERM独自拡張。#109調査で
                // 判明: 岩/AIR/スター/ダイヤの配分率設定を変更すると、既に盤面に配置済み
                // (場合によっては落下中)のアイテムブロックまで無条件に再抽選され上書き
                // されて消えてしまっていた)。既に置かれているアイテムはAIRと同様、
                // このセルはEmpty同様「既に確定した内容」として扱い、そのまま残す。
                if current == Cell::Empty || matches!(current, Cell::Item(_)) {
                    continue;
                }

                let left = if col > 0 { Some(self.rows[row][col - 1]) } else { None };
                let top = if row > 0 { Some(self.rows[row - 1][col]) } else { None };

                let fresh_color = match left {
                    Some(Cell::Color(c)) if rng.random_range(0.0..1.0) < color_cluster_prob => c,
                    _ => ColorKind::ALL[rng.random_range(0..color_count)],
                };
                let adjacent_is_rock =
                    matches!(left, Some(Cell::Rock { .. })) || matches!(top, Some(Cell::Rock { .. }));
                let rock_cluster_bonus = if adjacent_is_rock { rock_cluster_bonus_if_adjacent } else { 0.0 };

                // スターへの再抽選は、元がXブロックまたはダイヤブロックだったセルに限る
                // (TERM独自拡張。ユーザー指摘: 「スターブロックに変わるのはXブロックと
                // ダイヤブロックだけという前提にする」。以前は逆に色ブロックのみが対象
                // だったが、この指示により反転した)。揺れ中/落下中のセルは対象外にする
                // (ユーザー指摘: 「ぐらぐら/落下中のブロックはスターブロックへの変化
                // 対象外とする」)。
                let was_rock_or_diamond = matches!(current, Cell::Rock { .. } | Cell::Diamond);
                let allow_star = was_rock_or_diamond && !gravity.is_shaking((row, col));

                self.rows[row][col] = overlay_rock_oxygen_diamond_with_rates(
                    &mut rng,
                    fresh_color,
                    row,
                    rock_rate_percent,
                    air_rate_percent,
                    star_rate_percent,
                    diamond_rate_percent,
                    item_clear_above_rate_percent,
                    item_unify_colors_rate_percent,
                    item_starify_screen_rate_percent,
                    rock_cluster_bonus,
                    allow_star,
                    &mut item_caps,
                );
            }

            ensure_row_is_not_fully_blocked_by_rock(&mut self.rows[row], &mut rng, color_count);
        }
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

    /// 揺れ猶予中(まだ落下し始めていない)の状態だけをクリアする(TERM独自拡張)。
    /// デバッグショートカット等で盤面の色配置を重力ティックの外から直接書き換えた直後に
    /// 呼ぶ。塊の境界が変わると揺れ状態が指していた代表座標の意味も変わってしまうため、
    /// 次の重力ティックで結合関係(塊)を作り直して支持判定からやり直させる(ユーザー指摘:
    /// 「ショートカットCを10画面分に適用し、ちゃんと結合関係を再計算するように」)。
    ///
    /// 揺れ猶予が明けて既に連続落下中の塊(値が`current_shake_ticks`を超えている
    /// エントリ、`apply_gravity_tick`が「揺れ直さず落下し続ける」ために使う印)は
    /// クリア対象から除外する。ここを無条件に全クリアしてしまうと、連続落下中の
    /// ブロックがショートカットCを押した瞬間に不必要へ揺れ直しを始めてしまい、
    /// 「フリーズしたように見える」(ユーザー指摘: 「ショートカット:Cにした瞬間これで
    /// 落ちずにフリーズしてるように見える」「グラグラさせたら、ちゃんと落下処理しないと」)。
    pub fn reset_shake_progress(&mut self, current_shake_ticks: u8) {
        self.unsupported_ticks
            .retain(|_, ticks| *ticks as u32 > current_shake_ticks as u32);
        self.shaking_cells.clear();
    }
}

/// 盤面上の座標(行, 列)。
pub type Pos = (usize, usize);

/// 1回の重力ティックで実際に1マス落下したセル1つぶんの(移動後の位置, 移動前の位置)
/// (TERM独自拡張。ブロック落下のピクセル単位補間描画に使う)。
pub type BlockMove = (Pos, Pos);

/// 1回の重力ティックの結果。
#[derive(Debug, Clone, Default)]
pub struct FallTickOutcome {
    /// このティックで実際に1マス落下した各セルの(移動後の位置, 移動前の位置)
    /// (TERM独自拡張。ユーザー指摘: 「ブロックの落ち方をコマ送りでなくピクセル単位で
    /// 滑らかにしてほしい」)。描画側(render.rs)がこれを使って、移動後の位置から
    /// 移動前の位置へ向けて補間描画する。押し潰しで消滅したセルは含まない
    /// (揺れ中のセルも含まない。落下セル数は`.len()`で取れるため別フィールドに
    /// 重複して持たない)。
    pub moved_cells: Vec<BlockMove>,
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
    /// 落下してきたアイテムブロックがプレイヤー位置に重なり、取得された効果の一覧
    /// (TERM独自拡張。ユーザー指摘: 「アイテムはAIRと同じ用に…上から振ってきても
    /// 死なないように」)。AIRと同様、押し潰されず取得扱いになる。呼び出し側(Game)が
    /// この効果を実際に発動する
    pub items_collected: Vec<ItemEffect>,
    /// このティックで自動消滅(4連結以上の色ブロック・岩ブロック)により消えたセルの
    /// 座標(TERM独自拡張。ユーザー指摘: 「ブロックが消える瞬間に消える演出してほしい」)。
    /// 描画側(render.rs)がこの座標に一瞬フラッシュ演出を出す。
    pub vanished_cells: Vec<Pos>,
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
/// バラして行うことはなく、「ちぎれて落ちる」ことが起きない。酸素カプセル・ダイヤ・
/// スター・白ブロック・アイテムブロックは連結対象外(spec.md 2章、白ブロック・
/// アイテムブロックはTERM独自拡張)なので、常にサイズ1の塊として扱う。
fn collect_fall_groups(board: &Board) -> Vec<Vec<(usize, usize)>> {
    let depth_rows = board.depth_rows();
    let mut visited: HashSet<(usize, usize)> = HashSet::new();
    let mut groups = Vec::new();

    let width = board.width();
    for row in 0..depth_rows {
        for col in 0..width {
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
                Cell::Oxygen | Cell::Diamond | Cell::Star { .. } | Cell::Item(_) => {
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
            // 揺れが明けた(またはshake_ticks=0で即座に) -> このティックで1マス落下する。
            // 移動後もなお未支持なら、次のティック以降は揺れ直さずそのまま落下し続ける
            // (ユーザー指摘: 「落下開始したら、ぐらぐらしなくてもいい」)。移動先の
            // 代表座標(行+1・列は変わらない)へ、既に揺れが明けたことを示す印を
            // 引き継いでおく。値を`shake_ticks+1`に固定するのは、落下し続ける間に
            // ここへ延々と加算してu8オーバーフローするのを避けるため。
            let unlocked_representative = (representative.0 + 1, representative.1);
            next_unsupported_ticks.insert(unlocked_representative, shake_ticks.saturating_add(1));
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
                if cell == Cell::Oxygen {
                    // 酸素カプセル(AIR)だけは例外で、掘削・自由落下時の「歩くだけで取得」と
                    // 同様に押し潰し判定にせず取得(酸素回復)扱いにする(TERM独自拡張。
                    // ユーザー指摘「上から降ってきたAIRで回復してないバグ」の修正)。その場で消滅する。
                    outcome.oxygen_collected += 1;
                } else if let Cell::Item(effect) = cell {
                    // アイテムブロックもAIRと同じ扱いにする(TERM独自拡張。ユーザー指摘:
                    // 「アイテムはAIRと同じ用に…上から振ってきても死なないように」)。
                    outcome.items_collected.push(effect);
                } else {
                    // 押し潰した側のセルは即座には消滅させず、プレイヤーの位置にそのまま
                    // 残す(TERM独自拡張。ユーザー指摘: 「潰れる直前で消えてしまう」
                    // 「潰した様子が認識できるように」)。得点は発生しない(spec.md 5章)。
                    // 「天に召される」演出が完了して復活する際にGame側で消去される
                    // (Game::tick_ascending)。
                    board.set(to.0, to.1, cell);
                    outcome.moved_cells.push((to, (r, c)));
                    crushed_in_group = true;
                }
            } else {
                board.set(to.0, to.1, cell);
                outcome.moved_cells.push((to, (r, c)));
            }
        }

        if crushed_in_group {
            outcome.crushed = true;
            // ここで`break`すると、直前のループで旧位置を既にEmptyにしてしまった
            // 「他の(無関係な)落下中の塊」が新位置への書き込みだけスキップされ、
            // 盤面から消滅してしまう(発見: 同一tickに複数の塊が同時に落下していて、
            // うち1つが押し潰しを起こすケース)。押し潰しミス自体はこのtickで確定する
            // が、他の塊の移動は最後まで反映する必要がある。
            continue;
        }
    }

    // 押し潰しが確定していても、以降の自動消滅判定はスキップしない。押し潰しは
    // 「特定の1つの塊がプレイヤー位置へ移動した」という個別の事象であり、同じtickで
    // 着地した他の(無関係な)塊の4連結自動消滅とは独立している。ここで早期returnすると、
    // 押し潰しが起きたtickに限って、本来なら着地・自動消滅するはずの塊(押し潰しとは
    // 無関係な塊も含め全て)が消滅せずそのまま盤面に残ってしまう
    // (発見: 「4個以上結合したのに消えない」報告の一因)。押し潰された塊自体は、
    // 直後のEmptyチェックで自然にスキップされるため、ここで特別扱いする必要はない。

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
        match board.cell(to.0, to.1) {
            Cell::Color(color) => {
                let vanish_group = connected_same_color(board, to, color);
                // 支持判定は「落下してきた塊自身のセルだけ」ではなく、新たに接触した
                // 既存の塊も含めた現在の連結グループ全体で行う。既存の静的な塊の
                // 「真横」に接触して結合した場合、落下塊自身のセルの直下は空のままの
                // ことがあり(例: 縦棒の隣に落ちてきた1個)、moved_groupだけを見ると
                // 未支持=まだ落下中と判定されてこのtickの自動消滅チェックをスキップ
                // してしまう。ところが次tickにはcollect_fall_groupsが両者を1つの
                // 塊として合体させ、既存側の支え(最深行等)によって「支持済み」に
                // 分類されるため、以後二度と自動消滅チェックの対象にならず、
                // 4連結以上のまま永久に残ってしまう(発見: 「結合したのに消えない」
                // 報告群の実体)。着地判定と自動消滅判定を同じ「現在の連結グループ」
                // 基準に揃えることで、この食い違いを無くす。
                if !is_group_supported(board, &vanish_group, player_pos) {
                    continue; // まだ落下中(次ティック以降に改めて着地判定する)
                }
                if vanish_group.len() >= 4 {
                    for &(vr, vc) in &vanish_group {
                        board.set(vr, vc, Cell::Empty);
                    }
                    outcome.auto_vanished_blocks += vanish_group.len();
                    outcome.vanished_cells.extend(vanish_group);
                }
            }
            Cell::Rock { .. } => {
                let vanish_group = connected_rock_group(board, to);
                if !is_group_supported(board, &vanish_group, player_pos) {
                    continue;
                }
                if vanish_group.len() >= 4 {
                    for &(vr, vc) in &vanish_group {
                        board.set(vr, vc, Cell::Empty);
                    }
                    outcome.auto_vanished_rock_blocks += vanish_group.len();
                    outcome.vanished_cells.extend(vanish_group);
                }
            }
            _ => {}
        }
    }

    outcome
}

/// プレイヤーの画面内(行±`STAR_VISIBLE_RANGE_ROWS`)にあるスターブロックの表示経過
/// 時間を実時間`delta_ms`ぶん進める。`STAR_VISIBLE_GRACE_MS`に達するまでは無傷のまま、
/// その後`STAR_MELT_DURATION_MS`かけて溶けて消える(TERM独自拡張。ユーザー指摘:
/// 「画面内にきたら、溶けて自然と消えるスターブロックも欲しい」「スターブロックは
/// 画面内に見えてから5秒たったら消えはじめること」)。画面外のスターブロックは
/// 経過時間が進まない(画面内に戻ってきたら残りの猶予から再開する)。戻り値は消滅した
/// スターブロックの個数。
pub fn tick_star_melting(board: &mut Board, player_row: usize, delta_ms: u32) -> Vec<Pos> {
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
                    melted.push((r, c));
                } else {
                    board.set(r, c, Cell::Star { visible_ms: updated });
                }
            }
        }
    }

    melted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::FIELD_WIDTH_DEFAULT as FIELD_WIDTH;
    use crate::constants::SHAKE_TICKS;

    fn empty_board(rows: usize) -> Board {
        Board {
            rows: vec![vec![Cell::Empty; FIELD_WIDTH]; rows],
            width: FIELD_WIDTH,
        }
    }

    // --- 生成: 安全地帯(先頭2行)は常にEmpty ---

    #[test]
    fn generate_keeps_first_two_rows_empty() {
        let board = Board::generate(1, 50, FIELD_WIDTH);
        for col in 0..FIELD_WIDTH {
            assert_eq!(board.cell(0, col), Cell::Empty);
            assert_eq!(board.cell(1, col), Cell::Empty);
        }
    }

    // --- プレイ中の配分率(岩/AIR)変更(TERM独自拡張) ---

    #[test]
    fn reroll_overlays_from_row_leaves_rows_before_from_row_untouched() {
        let mut board = empty_board(5);
        for col in 0..FIELD_WIDTH {
            board.rows[0][col] = Cell::Color(ColorKind::Red); // from_rowより手前
        }

        board.reroll_overlays_from_row(1, 0, 0, 0, 0, 0, 0, 0, 4, 100, &GravityState::new()); // 岩/AIR/スター/ダイヤの確率を0に

        for col in 0..FIELD_WIDTH {
            assert_eq!(board.cell(0, col), Cell::Color(ColorKind::Red), "from_rowより手前は変わらない");
        }
    }

    #[test]
    fn reroll_overlays_from_row_also_rerolls_cells_already_committed_to_an_overlay() {
        // ユーザー指摘: 「0%に設定したのに、存在するとか」。最初のBoard::generateは
        // 常に既定率(100%)で岩/AIR/スター/ダイヤを配置するため、「まだCell::Colorの
        // セルだけ」を対象にすると、既に確定していたセルは配分率を0%にしても永久に
        // 残ってしまう。Empty以外なら元の種類を問わず再抽選対象になることを確認する。
        let mut board = empty_board(1);
        board.rows[0][0] = Cell::Rock { hits: 3 };
        board.rows[0][1] = Cell::Oxygen;
        board.rows[0][2] = Cell::Star { visible_ms: 2000 };
        board.rows[0][3] = Cell::Diamond;
        board.rows[0][4] = Cell::Empty; // 既に掘削済み・対象外

        // 岩/AIR/スター/ダイヤの配分率を全て0にすれば、Empty以外の全セルは必ず
        // Color(通常の色ブロック)へ再抽選される。
        board.reroll_overlays_from_row(0, 0, 0, 0, 0, 0, 0, 0, 4, 100, &GravityState::new());

        for col in 0..4 {
            assert!(
                matches!(board.cell(0, col), Cell::Color(_)),
                "配分率0%なら既存の確定セルも含めて色ブロックへ再抽選されるはず: col={col} -> {:?}",
                board.cell(0, col)
            );
        }
        assert_eq!(board.cell(0, 4), Cell::Empty, "既に掘削済みのセルは対象外のまま");
    }

    #[test]
    fn reroll_overlays_from_row_never_overwrites_existing_item_blocks() {
        // #109調査で判明: 岩/AIR/スター/ダイヤの配分率を再抽選すると、既に盤面に
        // 配置済みのアイテムブロックまで無条件に上書きされて消えてしまっていた。
        // アイテムはAIRと同様、既に確定した内容として再抽選の対象外にする。
        let mut board = empty_board(1);
        board.rows[0][0] = Cell::Item(ItemEffect::ClearAbove);
        board.rows[0][1] = Cell::Item(ItemEffect::UnifyColors);
        board.rows[0][2] = Cell::Item(ItemEffect::StarifyScreen);

        board.reroll_overlays_from_row(0, 300, 300, 300, 300, 300, 300, 300, 4, 100, &GravityState::new());

        assert_eq!(board.cell(0, 0), Cell::Item(ItemEffect::ClearAbove), "Rアイテムは再抽選で上書きされないはず");
        assert_eq!(board.cell(0, 1), Cell::Item(ItemEffect::UnifyColors), "Cアイテムは再抽選で上書きされないはず");
        assert_eq!(board.cell(0, 2), Cell::Item(ItemEffect::StarifyScreen), "Kアイテムは再抽選で上書きされないはず");
    }

    #[test]
    fn reroll_overlays_from_row_higher_rock_rate_yields_more_rock_cells_on_average() {
        fn all_color_board(rows: usize) -> Board {
            let mut b = empty_board(rows);
            for row in 0..rows {
                for col in 0..FIELD_WIDTH {
                    b.rows[row][col] = Cell::Color(ColorKind::Red);
                }
            }
            b
        }
        fn count_rocks(board: &Board) -> usize {
            board.rows.iter().flatten().filter(|c| matches!(c, Cell::Rock { .. })).count()
        }

        let mut low = all_color_board(500);
        low.reroll_overlays_from_row(0, 20, 100, 100, 100, 0, 0, 0, 4, 100, &GravityState::new());
        let mut high = all_color_board(500);
        high.reroll_overlays_from_row(0, 300, 100, 100, 100, 0, 0, 0, 4, 100, &GravityState::new());

        let (low_count, high_count) = (count_rocks(&low), count_rocks(&high));
        assert!(
            high_count > low_count * 2,
            "配分率を上げれば統計的に岩ブロックが明確に増えるはず: low={low_count}, high={high_count}"
        );
    }

    #[test]
    fn reroll_overlays_from_row_star_rate_zero_produces_no_star_cells() {
        // ユーザー指摘: 「スターブロック比率0〜」。0%を指定すれば、通常なら出現するはずの
        // スターブロックが一切生成されないことを確認する。
        let mut board = empty_board(500);
        for row in 0..500 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }

        board.reroll_overlays_from_row(0, 100, 100, 0, 100, 0, 0, 0, 4, 100, &GravityState::new());

        let star_count = board.rows.iter().flatten().filter(|c| matches!(c, Cell::Star { .. })).count();
        assert_eq!(star_count, 0, "スター配分率0%ならスターブロックは一切出現しないはず");
    }

    #[test]
    fn reroll_overlays_from_row_spawns_all_three_kinds_of_item_blocks() {
        // ユーザー指摘: 「ショートカットRと同じ効果のあるアイテムつくろ」「ショートカット
        // C効果のアイテムも作って」「ショートカットKアイテムつくって」。出現率はごく
        // 低確率の値のため、十分な行数で統計的に3種とも出現することを確認する。
        let mut board = empty_board(5000);
        for row in 0..5000 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }

        board.reroll_overlays_from_row(0, 100, 100, 100, 100, 100, 100, 100, 4, 100, &GravityState::new());

        let clear_above_count = board
            .rows
            .iter()
            .flatten()
            .filter(|c| matches!(c, Cell::Item(ItemEffect::ClearAbove)))
            .count();
        let unify_colors_count = board
            .rows
            .iter()
            .flatten()
            .filter(|c| matches!(c, Cell::Item(ItemEffect::UnifyColors)))
            .count();
        let starify_screen_count = board
            .rows
            .iter()
            .flatten()
            .filter(|c| matches!(c, Cell::Item(ItemEffect::StarifyScreen)))
            .count();
        assert!(clear_above_count > 0, "ClearAboveアイテムが1つも出現しないのは不自然");
        assert!(unify_colors_count > 0, "UnifyColorsアイテムが1つも出現しないのは不自然");
        assert!(starify_screen_count > 0, "StarifyScreenアイテムが1つも出現しないのは不自然");
    }

    #[test]
    fn reroll_overlays_from_row_item_rate_percent_controls_each_item_independently() {
        // ユーザー指摘: 「各種アイテムの出現頻度の設定項目増やして」。ClearAboveだけ
        // 0%にすれば出現せず、他の2種は100%のまま出現し続けることを確認する。
        let mut board = empty_board(5000);
        for row in 0..5000 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }

        board.reroll_overlays_from_row(0, 100, 100, 100, 100, 0, 100, 100, 4, 100, &GravityState::new());

        let clear_above_count = board
            .rows
            .iter()
            .flatten()
            .filter(|c| matches!(c, Cell::Item(ItemEffect::ClearAbove)))
            .count();
        let unify_colors_count = board
            .rows
            .iter()
            .flatten()
            .filter(|c| matches!(c, Cell::Item(ItemEffect::UnifyColors)))
            .count();
        let starify_screen_count = board
            .rows
            .iter()
            .flatten()
            .filter(|c| matches!(c, Cell::Item(ItemEffect::StarifyScreen)))
            .count();
        assert_eq!(clear_above_count, 0, "ClearAbove配分率0%なら出現しないはず");
        assert!(unify_colors_count > 0, "UnifyColorsは100%のままなので出現し続けるはず");
        assert!(starify_screen_count > 0, "StarifyScreenは100%のままなので出現し続けるはず");
    }

    #[test]
    fn reroll_overlays_from_row_never_exceeds_the_per_item_type_cap_on_the_board() {
        // ユーザー指摘: 「各種アイテムのキャラより上の位置には最大それぞれ10個までとする」。
        // 出現率を極端に高くしても、盤面全体で種類ごとに上限個数を超えないことを確認する。
        let mut board = empty_board(5000);
        for row in 0..5000 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }

        board.reroll_overlays_from_row(0, 0, 0, 0, 0, 300, 300, 300, 4, 100, &GravityState::new());

        for effect in [ItemEffect::ClearAbove, ItemEffect::UnifyColors, ItemEffect::StarifyScreen] {
            let count = board.count_item(effect);
            assert!(
                count <= crate::constants::ITEM_MAX_COUNT_ON_BOARD,
                "{effect:?}は上限{}個を超えてはいけないはず(実際={count})",
                crate::constants::ITEM_MAX_COUNT_ON_BOARD
            );
        }
    }

    #[test]
    fn board_generate_never_exceeds_the_per_item_type_cap() {
        // 新規生成(`Board::generate`)でも同じ上限が効くことを確認する。
        let board = Board::generate(1, 5000, FIELD_WIDTH);

        for effect in [ItemEffect::ClearAbove, ItemEffect::UnifyColors, ItemEffect::StarifyScreen] {
            let count = board.count_item(effect);
            assert!(
                count <= crate::constants::ITEM_MAX_COUNT_ON_BOARD,
                "{effect:?}は上限{}個を超えてはいけないはず(実際={count})",
                crate::constants::ITEM_MAX_COUNT_ON_BOARD
            );
        }
    }

    #[test]
    fn reroll_overlays_from_row_counts_pre_existing_items_toward_the_cap() {
        // 既に盤面上にアイテムが置かれている場合、その個数も上限に含めて計算し、
        // 残り枠ぶんしか新規出現させないことを確認する(#109調査で判明した「既存
        // アイテムは再抽選対象外」との組み合わせ挙動)。
        let mut board = empty_board(5000);
        for row in 0..5000 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }
        let max = crate::constants::ITEM_MAX_COUNT_ON_BOARD;
        for i in 0..max - 2 {
            board.rows[0][i] = Cell::Item(ItemEffect::ClearAbove);
        }

        board.reroll_overlays_from_row(1, 0, 0, 0, 0, 300, 0, 0, 4, 100, &GravityState::new());

        let count = board.count_item(ItemEffect::ClearAbove);
        assert!(
            count <= max,
            "既存分を含めても上限{max}個を超えてはいけないはず(実際={count})"
        );
    }

    #[test]
    fn reroll_overlays_from_row_never_converts_existing_color_or_oxygen_cells_into_stars() {
        // ユーザー指摘: 「スターブロックに変わるのはXブロックとダイヤブロックだけという
        // 前提にする」(#71の色ブロックのみルールから反転)。既存のColor/Oxygenセルは、
        // スター配分率を上限にしても、スターへは変わらないことを確認する。
        let mut board = empty_board(3);
        for col in 0..FIELD_WIDTH {
            board.rows[0][col] = Cell::Color(ColorKind::Red);
            board.rows[1][col] = Cell::Oxygen;
        }

        board.reroll_overlays_from_row(0, 0, 0, 300, 0, 0, 0, 0, 4, 100, &GravityState::new());

        for row in 0..2 {
            for col in 0..FIELD_WIDTH {
                assert!(
                    !matches!(board.cell(row, col), Cell::Star { .. }),
                    "row={row} col={col}は元がColor/Oxygenなのでスターへ変わらないはず: {:?}",
                    board.cell(row, col)
                );
            }
        }
    }

    #[test]
    fn reroll_overlays_from_row_can_convert_existing_rock_or_diamond_cells_into_stars() {
        // ユーザー指摘: 「スターブロックに変わるのはXブロックとダイヤブロックだけという
        // 前提にする」。既存のRock/Diamondセルは、スター配分率を上限にすれば
        // スターへ変わり得ることを統計的に確認する(0件は不自然)。
        let mut board = empty_board(500);
        for row in 0..500 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = if col % 2 == 0 { Cell::Rock { hits: 0 } } else { Cell::Diamond };
            }
        }

        board.reroll_overlays_from_row(0, 0, 0, 300, 0, 0, 0, 0, 4, 100, &GravityState::new());

        let star_count = board.rows.iter().flatten().filter(|c| matches!(c, Cell::Star { .. })).count();
        assert!(star_count > 0, "Xブロック・ダイヤブロックはスターへ変わり得るはず(0件は不自然)");
    }

    #[test]
    fn reroll_overlays_from_row_never_converts_shaking_cells_into_stars() {
        // ユーザー指摘: 「ぐらぐら/落下中のブロックはスターブロックへの変化対象外とする」。
        // 元がRockなら本来スター化対象だが、揺れ中は除外されることを確認する。
        let mut board = empty_board(1);
        for col in 0..FIELD_WIDTH {
            board.rows[0][col] = Cell::Rock { hits: 0 };
        }
        let mut gravity = GravityState::new();
        for col in 0..FIELD_WIDTH {
            gravity.shaking_cells.insert((0, col));
        }

        board.reroll_overlays_from_row(0, 0, 0, 300, 0, 0, 0, 0, 4, 100, &gravity);

        let star_count = board.rows.iter().flatten().filter(|c| matches!(c, Cell::Star { .. })).count();
        assert_eq!(star_count, 0, "揺れ中のセルはスターへ変わらないはず");
    }

    #[test]
    fn reroll_overlays_from_row_diamond_rate_zero_produces_no_diamond_cells() {
        // ユーザー指摘: 「ダイヤブロック0%設定」。0%を指定すれば、通常なら出現するはずの
        // ダイヤブロックが一切生成されないことを確認する。
        let mut board = empty_board(500);
        for row in 0..500 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }

        board.reroll_overlays_from_row(0, 100, 100, 100, 0, 0, 0, 0, 4, 100, &GravityState::new());

        let diamond_count = board.rows.iter().flatten().filter(|&&c| c == Cell::Diamond).count();
        assert_eq!(diamond_count, 0, "ダイヤ配分率0%ならダイヤブロックは一切出現しないはず");
    }

    // --- スターブロックの実時間溶解(TERM独自拡張) ---

    #[test]
    fn tick_star_melting_leaves_the_star_intact_within_the_grace_period() {
        // ユーザー指摘: 「スターブロックは画面内に見えてから5秒たったら消えはじめる
        // こと」。猶予時間(STAR_VISIBLE_GRACE_MS)未満しか経過していなければ、
        // 画面内であっても溶解が始まらない(セルが残る)ことを確認する。
        let mut board = empty_board(1);
        board.rows[0][0] = Cell::Star { visible_ms: 0 };

        let melted = tick_star_melting(&mut board, 0, STAR_VISIBLE_GRACE_MS - 1);

        assert_eq!(melted.len(), 0, "猶予時間未満では消滅しないはず");
        assert!(matches!(board.cell(0, 0), Cell::Star { .. }), "猶予時間未満ではまだスターのままのはず");
    }

    #[test]
    fn tick_star_melting_vanishes_after_grace_period_plus_melt_duration_elapses() {
        let mut board = empty_board(1);
        board.rows[0][0] = Cell::Star { visible_ms: 0 };

        let melted = tick_star_melting(&mut board, 0, STAR_VISIBLE_GRACE_MS + STAR_MELT_DURATION_MS);

        assert_eq!(melted, vec![(0, 0)], "猶予時間+溶解時間が経過すれば1個消えるはず");
        assert_eq!(board.cell(0, 0), Cell::Empty, "溶け切ったスターは消えているはず");
    }

    #[test]
    fn tick_star_melting_ignores_stars_outside_the_visible_range() {
        // プレイヤーの画面外(行±STAR_VISIBLE_RANGE_ROWS)にあるスターブロックは
        // 経過時間が進まないことを確認する。
        let range = crate::constants::STAR_VISIBLE_RANGE_ROWS;
        let far_row = range + 10;
        let mut board = empty_board(far_row + 1);
        board.rows[far_row][0] = Cell::Star { visible_ms: 0 };

        let melted = tick_star_melting(&mut board, 0, STAR_VISIBLE_GRACE_MS + STAR_MELT_DURATION_MS + 1000);

        assert_eq!(melted.len(), 0, "画面外のスターは溶解が進まないはず");
        assert_eq!(board.cell(far_row, 0), Cell::Star { visible_ms: 0 }, "経過時間が進んでいないはず");
    }

    #[test]
    fn reroll_overlays_from_row_color_count_restricts_the_palette_to_the_first_n_colors() {
        // ユーザー指摘: 「出現する色ブロックの色数を設定で選べるようにしたい(1〜4)」。
        // color_countを指定すると、ColorKind::ALLの先頭からその数だけに色ブロックの
        // 抽選が制限されることを確認する(岩/AIR/スター/ダイヤは0%にして純粋に
        // 色ブロックだけを観測する)。
        let mut board = empty_board(500);
        for row in 0..500 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }

        board.reroll_overlays_from_row(0, 0, 0, 0, 0, 0, 0, 0, 2, 100, &GravityState::new());

        let mut colors_seen: Vec<ColorKind> = Vec::new();
        for cell in board.rows.iter().flatten() {
            if let Cell::Color(k) = cell
                && !colors_seen.contains(k)
            {
                colors_seen.push(*k);
            }
        }
        colors_seen.sort_by_key(|c| ColorKind::ALL.iter().position(|a| a == c).unwrap());
        assert_eq!(
            colors_seen,
            vec![ColorKind::Red, ColorKind::Blue],
            "color_count=2ならRed/Blueの2色のみが出現するはず: {colors_seen:?}"
        );
    }

    #[test]
    fn reroll_overlays_from_row_color_count_one_produces_a_single_color() {
        let mut board = empty_board(200);
        for row in 0..200 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Blue);
            }
        }

        board.reroll_overlays_from_row(0, 0, 0, 0, 0, 0, 0, 0, 1, 100, &GravityState::new());

        for cell in board.rows.iter().flatten() {
            // アイテムブロック(TERM独自拡張)は岩/AIR/スター/ダイヤの配分率とは独立した
            // ごく低確率の抽選のため、rate=0設定でも例外的に出現し得る。この試験の
            // 意図(色数設定が単色に絞られること)には影響しないため許容する。
            assert!(
                matches!(cell, Cell::Color(ColorKind::Red)) || matches!(cell, Cell::Item(_)),
                "color_count=1なら常にColorKind::ALLの先頭色のみ(アイテムブロック化のみ例外): {cell:?}"
            );
        }
    }

    #[test]
    fn reroll_overlays_from_row_color_clustering_weakens_with_depth() {
        // ユーザー指摘: 「階層が進むにつれて…初期配置されるブロックがあまり結合状態に
        // なく、個別でばらばらであり…難易度をあげていってほしい」。深度が浅いほど
        // 左隣の色を継承しやすくまとまりが強く、深いほど独立抽選に近づきバラバラに
        // なることを確認する(TERM独自拡張の難易度カーブ)。
        fn avg_run_length_in_range(board: &Board, rows: std::ops::Range<usize>) -> f64 {
            let mut total_len = 0u64;
            let mut total_runs = 0u64;
            for row in rows {
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
            if total_runs == 0 { 0.0 } else { total_len as f64 / total_runs as f64 }
        }

        let mut board = empty_board(1000);
        for row in 0..1000 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }
        // 岩/AIR/スター/ダイヤは無しにして、純粋に色の連結だけを観測する。
        board.reroll_overlays_from_row(0, 0, 0, 0, 0, 0, 0, 0, 4, 100, &GravityState::new());

        let shallow_avg = avg_run_length_in_range(&board, 2..200);
        let deep_avg = avg_run_length_in_range(&board, 800..1000);

        assert!(
            shallow_avg > deep_avg + 0.1,
            "浅い深度の方が横方向のまとまりが強いはず: shallow={shallow_avg}, deep={deep_avg}"
        );
    }

    #[test]
    fn reroll_overlays_from_row_color_cluster_rate_percent_scales_clustering_strength() {
        // ユーザー指摘: 「ブロック配置の結合関係の割合を設定できるようにして」。
        // 同じ浅い深度帯でも、color_cluster_rate_percentを0%にすると常に均等
        // ランダム抽選になり、100%(既定)時より横方向のまとまりが明確に弱くなる
        // ことを確認する。
        fn avg_run_length_in_range(board: &Board, rows: std::ops::Range<usize>) -> f64 {
            let mut total_len = 0u64;
            let mut total_runs = 0u64;
            for row in rows {
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
            if total_runs == 0 { 0.0 } else { total_len as f64 / total_runs as f64 }
        }

        fn make_board() -> Board {
            let mut board = empty_board(200);
            for row in 0..200 {
                for col in 0..FIELD_WIDTH {
                    board.rows[row][col] = Cell::Color(ColorKind::Red);
                }
            }
            board
        }

        let mut zero_rate = make_board();
        zero_rate.reroll_overlays_from_row(0, 0, 0, 0, 0, 0, 0, 0, 4, 0, &GravityState::new());
        let mut default_rate = make_board();
        default_rate.reroll_overlays_from_row(0, 0, 0, 0, 0, 0, 0, 0, 4, 100, &GravityState::new());

        let zero_avg = avg_run_length_in_range(&zero_rate, 2..200);
        let default_avg = avg_run_length_in_range(&default_rate, 2..200);

        assert!(
            default_avg > zero_avg + 0.1,
            "100%設定の方が0%設定よりまとまりが強いはず: zero={zero_avg}, default={default_avg}"
        );
    }

    #[test]
    fn reroll_overlays_from_row_rock_clustering_strengthens_with_depth() {
        // ユーザー指摘: 「Xブロックが結合で大量にあったりするように」。深度が深いほど
        // 岩ブロックの塊が大きくなりやすいことを確認する(TERM独自拡張の難易度カーブ)。
        fn avg_rock_group_size_in_range(board: &Board, rows: std::ops::Range<usize>) -> f64 {
            let mut visited: HashSet<(usize, usize)> = HashSet::new();
            let mut total = 0u64;
            let mut groups = 0u64;
            for row in rows.clone() {
                for col in 0..FIELD_WIDTH {
                    let pos = (row, col);
                    if visited.contains(&pos) {
                        continue;
                    }
                    if matches!(board.cell(row, col), Cell::Rock { .. }) {
                        let group = connected_rock_group(board, pos);
                        for &p in &group {
                            visited.insert(p);
                        }
                        total += group.len() as u64;
                        groups += 1;
                    } else {
                        visited.insert(pos);
                    }
                }
            }
            if groups == 0 { 0.0 } else { total as f64 / groups as f64 }
        }

        let mut board = empty_board(1000);
        for row in 0..1000 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }
        // 岩の出現率を上限(300%)にして、隣接ボーナスの効果を観測しやすくする。
        board.reroll_overlays_from_row(0, 300, 0, 0, 0, 0, 0, 0, 4, 100, &GravityState::new());

        let shallow_avg = avg_rock_group_size_in_range(&board, 2..200);
        let deep_avg = avg_rock_group_size_in_range(&board, 800..1000);

        assert!(
            deep_avg > shallow_avg + 0.1,
            "深い深度の方が岩ブロックの塊が大きいはず: shallow={shallow_avg}, deep={deep_avg}"
        );
    }

    #[test]
    fn deep_rock_clustering_does_not_fill_the_whole_field_at_default_settings() {
        // ユーザー報告(スクリーンショット2枚、深度620m・831m): 岩の塊化ボーナスが
        // 強すぎて画面のほぼ全域が岩で埋め尽くされ、経路が実質的に塞がれていた
        // (「絶対無理」)。既定設定(rock_rate_percent=100%)の最深帯でも、岩マスの
        // 割合が画面全体を覆ってしまわないことを統計的に確認する。
        //
        // reroll_overlays_from_rowは呼び出しごとに新しい乱数系列を使う(再現性を
        // 求めない設計)ため、1回きりの試行では閾値ぎりぎりでたまたま通ってしまう
        // (またはたまたま落ちる)ことがある。複数回試行した平均で判定し、統計的な
        // ふらつきに左右されない検証にする。
        fn rock_fraction_in_deepest_band() -> f64 {
            let mut board = empty_board(1000);
            for row in 0..1000 {
                for col in 0..FIELD_WIDTH {
                    board.rows[row][col] = Cell::Color(ColorKind::Red);
                }
            }
            board.reroll_overlays_from_row(0, 100, 100, 0, 100, 0, 0, 0, 4, 100, &GravityState::new());

            let mut rock_cells = 0usize;
            let mut total_cells = 0usize;
            for row in 800..1000 {
                for col in 0..FIELD_WIDTH {
                    total_cells += 1;
                    if matches!(board.cell(row, col), Cell::Rock { .. }) {
                        rock_cells += 1;
                    }
                }
            }
            rock_cells as f64 / total_cells as f64
        }

        const TRIALS: usize = 20;
        let avg_fraction: f64 = (0..TRIALS).map(|_| rock_fraction_in_deepest_band()).sum::<f64>() / TRIALS as f64;
        assert!(
            avg_fraction < 0.45,
            "最深帯の岩マス比率(平均)が高すぎて画面全体が岩で埋まっている疑いがある: {avg_fraction:.3}"
        );
    }

    // --- 横一列が岩ブロックで完全に埋まる配置の禁止(TERM独自拡張) ---

    #[test]
    fn ensure_row_is_not_fully_blocked_by_rock_replaces_one_cell_when_the_whole_row_is_rock() {
        // ユーザー指摘: 「Xブロック配置のとき横一列全部埋まる配置にはならないように」。
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let mut row = [Cell::Rock { hits: 0 }; FIELD_WIDTH];

        ensure_row_is_not_fully_blocked_by_rock(&mut row, &mut rng, 4);

        let rock_count = row.iter().filter(|c| matches!(c, Cell::Rock { .. })).count();
        assert_eq!(rock_count, FIELD_WIDTH - 1, "少なくとも1マスは岩ブロック以外に差し替わるはず");
        assert!(row.iter().any(|c| matches!(c, Cell::Color(_))), "差し替え先は色ブロックのはず");
    }

    #[test]
    fn ensure_row_is_not_fully_blocked_by_rock_leaves_a_non_full_row_untouched() {
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let mut row = [Cell::Rock { hits: 0 }; FIELD_WIDTH];
        row[3] = Cell::Empty; // 既に掘削済みの穴が1つあれば「完全に塞がった壁」ではない

        ensure_row_is_not_fully_blocked_by_rock(&mut row, &mut rng, 4);

        let rock_count = row.iter().filter(|c| matches!(c, Cell::Rock { .. })).count();
        assert_eq!(rock_count, FIELD_WIDTH - 1, "既に穴がある行はそのまま変更されないはず");
    }

    #[test]
    fn reroll_overlays_from_row_never_produces_a_row_fully_blocked_by_rock() {
        // 岩の出現率を上限(300%)・最大深度(塊化ボーナス最大)にしても、横一列が
        // 岩ブロックだけで完全に埋まることは無いことを確認する。
        let mut board = empty_board(1000);
        for row in 0..1000 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }
        board.reroll_overlays_from_row(0, 300, 0, 0, 0, 0, 0, 0, 4, 100, &GravityState::new());

        for row in 800..1000 {
            let all_rock = (0..FIELD_WIDTH).all(|col| matches!(board.cell(row, col), Cell::Rock { .. }));
            assert!(!all_rock, "row={row}が岩ブロックだけで完全に埋まっているはず無い");
        }
    }

    #[test]
    fn diamond_blocks_never_merge_even_when_adjacent() {
        // ダイヤブロックは隣接していても連結せず、それぞれ単独の塊として扱われる
        // (酸素・スターと同様、spec.md 2章)。
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Diamond;
        board.rows[0][1] = Cell::Diamond;
        board.rows[0][2] = Cell::Diamond;

        let groups = collect_fall_groups(&board);
        let diamond_groups: Vec<&Vec<(usize, usize)>> =
            groups.iter().filter(|g| g.iter().any(|&(r, c)| board.cell(r, c) == Cell::Diamond)).collect();

        assert_eq!(diamond_groups.len(), 3, "隣接していてもダイヤブロックはそれぞれ単独の塊のはず");
        for group in diamond_groups {
            assert_eq!(group.len(), 1);
        }
    }

    #[test]
    fn generate_produces_deterministic_output_for_same_seed() {
        let a = Board::generate(42, 100, FIELD_WIDTH);
        let b = Board::generate(42, 100, FIELD_WIDTH);
        for row in 0..100 {
            assert_eq!(a.rows[row], b.rows[row]);
        }
    }

    // 3.2〜3.3の下地生成(孤立セル解消より前)は横4・縦3の連続数上限を厳密に守る。
    // 3.4の孤立セル解消は上限を再チェックしない仕様(spec.md 3.4末尾)のため、
    // その後処理を経た最終盤面ではごく稀に上限を超える可能性を許容する
    // (`resolve_run_limits_*`の単体テストで境界条件自体は個別に検証する)。
    fn assert_run_limits_hold(base: &[Vec<Option<ColorKind>>], seed: u64) {
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
        let base = generate_base_colors(&mut rng, 500, FIELD_WIDTH);
        assert_run_limits_hold(&base, 7);
    }

    // 統計的検証(spec.md 3.3): アルゴリズムの保証自体は決定的だが、多数のシード・
    // 盤面サイズにわたって上限が破られないことを横断的に確認する。
    #[test]
    fn base_color_generation_respects_run_limits_across_many_seeds() {
        for seed in 0..20u64 {
            let mut rng = ChaCha8Rng::seed_from_u64(seed);
            let base = generate_base_colors(&mut rng, 300, FIELD_WIDTH);
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
            let board = Board::generate(seed, 300, FIELD_WIDTH);

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
        let mut base: Vec<Vec<Option<ColorKind>>> = vec![vec![None; FIELD_WIDTH]; 4];
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
        let mut base: Vec<Vec<Option<ColorKind>>> = vec![vec![None; FIELD_WIDTH]; 3];
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
        let mut base: Vec<Vec<Option<ColorKind>>> = vec![vec![None; FIELD_WIDTH]; 3];
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
            assert_eq!(outcome.moved_cells.len(), 0);
            assert!(gravity.is_shaking((0, 0)));
        }
        assert_eq!(board.cell(0, 0), Cell::Color(ColorKind::Red));

        // SHAKE_TICKS+1ティック目で実際に1マス落下する
        let outcome = apply_gravity_tick(&mut board, (99, 99), &mut gravity, SHAKE_TICKS);
        assert_eq!(outcome.moved_cells.len(), 1);
        assert_eq!(board.cell(0, 0), Cell::Empty);
        assert_eq!(board.cell(1, 0), Cell::Color(ColorKind::Red));
        assert!(!gravity.is_shaking((1, 0)), "着地して支持されればもう揺れていない");
    }

    #[test]
    fn once_falling_starts_it_continues_every_tick_without_re_shaking() {
        // ユーザー指摘: 「落下開始したら、ぐらぐらしなくてもいい」。開放された縦穴を
        // 連続で落ちる間、1マス落ちるたびに揺れ直すことはない(揺れるのは最初の
        // SHAKE_TICKSぶんだけ)。
        let mut board = empty_board(6);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        let mut gravity = GravityState::new();

        for _ in 0..SHAKE_TICKS {
            apply_gravity_tick(&mut board, (99, 99), &mut gravity, SHAKE_TICKS);
        }

        // SHAKE_TICKS+1回目の呼び出しで揺れが明けて最初の1マスが落ちる。
        let outcome = apply_gravity_tick(&mut board, (99, 99), &mut gravity, SHAKE_TICKS);
        assert_eq!(outcome.moved_cells.len(), 1);
        assert_eq!(board.cell(1, 0), Cell::Color(ColorKind::Red));
        assert!(!gravity.is_shaking((1, 0)));

        // 以降、最深行に着地するまで毎ティック連続で1マスずつ落下し続け、
        // 揺れ状態(is_shaking)には一切戻らない。
        for expected_row in 2..6 {
            let outcome = apply_gravity_tick(&mut board, (99, 99), &mut gravity, SHAKE_TICKS);
            assert_eq!(outcome.moved_cells.len(), 1, "row={expected_row}到達時点で連続落下しているはず");
            assert_eq!(board.cell(expected_row, 0), Cell::Color(ColorKind::Red));
            assert!(!gravity.is_shaking((expected_row, 0)), "落下中は揺れ状態に戻らないはず");
        }
    }

    #[test]
    fn reset_shake_progress_preserves_a_group_already_falling_continuously() {
        // ユーザー指摘: 「ショートカット:Cにした瞬間これで落ちずにフリーズしてるように
        // 見える」「グラグラさせたら、ちゃんと落下処理しないと」。連続落下中(揺れ猶予が
        // 明けた後)の塊は、reset_shake_progressを呼んでも揺れ直しにならず、そのまま
        // 連続で落下し続ける。
        let mut board = empty_board(6);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        let mut gravity = GravityState::new();

        for _ in 0..=SHAKE_TICKS {
            apply_gravity_tick(&mut board, (99, 99), &mut gravity, SHAKE_TICKS);
        }
        assert_eq!(board.cell(1, 0), Cell::Color(ColorKind::Red), "既に1マス落下しているはず");

        // ショートカットC相当の書き換え直後に呼ばれる想定の関数。
        gravity.reset_shake_progress(SHAKE_TICKS);

        let outcome = apply_gravity_tick(&mut board, (99, 99), &mut gravity, SHAKE_TICKS);
        assert_eq!(outcome.moved_cells.len(), 1, "reset_shake_progress後も揺れ直さず連続で落下し続けるはず");
        assert_eq!(board.cell(2, 0), Cell::Color(ColorKind::Red));
    }

    #[test]
    fn reset_shake_progress_clears_a_group_still_within_its_shake_grace_period() {
        // まだ揺れ猶予中(落下し始めていない)の塊は、reset_shake_progressで
        // クリアされ、次のティックからは揺れをやり直す(塊の境界が変わりうる
        // デバッグ書き換え直後に結合関係を作り直すため)。
        let mut board = empty_board(3);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        let mut gravity = GravityState::new();

        apply_gravity_tick(&mut board, (99, 99), &mut gravity, SHAKE_TICKS);
        assert!(gravity.is_shaking((0, 0)), "揺れ猶予中のはず");

        gravity.reset_shake_progress(SHAKE_TICKS);

        assert!(!gravity.is_shaking((0, 0)), "揺れ猶予中の状態はクリアされるはず");
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

    #[test]
    fn diamond_falling_onto_item_does_not_erase_the_item() {
        // ユーザー指摘: 「RアイテムやKアイテムがその上にダイヤブロックなどがあるとき、
        // 一緒に落下する過程で消えてしまう(必ず再現する)」。アイテムの真上にダイヤが
        // あり両方支えを失って一緒に落下しても、アイテムが消えずに最深行まで残ることを
        // 確認する(純粋な重力エンジンレベルの回帰テスト)。
        let mut board = empty_board(5);
        board.rows[0][0] = Cell::Diamond;
        board.rows[1][0] = Cell::Item(ItemEffect::ClearAbove);
        let mut gravity = GravityState::new();
        let player_pos = (999, 999);
        for _ in 0..(SHAKE_TICKS as usize + 6) {
            apply_gravity_tick(&mut board, player_pos, &mut gravity, SHAKE_TICKS);
        }

        assert!(matches!(board.cell(4, 0), Cell::Item(ItemEffect::ClearAbove)), "アイテムは最深行まで落ちて残るはず");
        assert!(matches!(board.cell(3, 0), Cell::Diamond), "ダイヤはアイテムのすぐ上に着地するはず");
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

        assert_eq!(outcome.moved_cells.len(), 2, "上段・下段とも同じティックで一緒に落下するはず");
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

        assert_eq!(outcome.moved_cells.len(), 0);
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

        assert_eq!(outcome.moved_cells.len(), 3);
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

        assert_eq!(outcome.moved_cells.len(), 1, "孤立していても支えが無ければ落ちるはず");
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

        assert_eq!(outcome.moved_cells.len(), 1, "赤ブロックだけが落ちるはず");
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

        assert_eq!(outcome.moved_cells.len(), 3);
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

        assert_eq!(outcome.moved_cells.len(), 3, "3つとも一緒に1マス落下するはず(ちぎれない)");
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

        assert_eq!(outcome.moved_cells.len(), 0);
        assert_eq!(board.cell(0, 0), Cell::Color(ColorKind::Red));
        assert_eq!(board.cell(0, 1), Cell::Color(ColorKind::Red));
        assert_eq!(board.cell(0, 2), Cell::Color(ColorKind::Red));
    }

    // --- 重力: 押し潰し判定(5章) ---

    #[test]
    fn falling_block_onto_player_crushes_and_remains_visible_at_the_impact_point() {
        // ユーザー指摘: 「潰れる直前で消えてしまう(ブロックが)」「潰した様子が
        // 認識できるように」。押し潰した側のセルは即座には消さず、プレイヤーの位置
        // (=着地先)にそのまま残す(得点は発生しない)。実際に消すのはGame側が
        // 「天に召される」演出完了・復活のタイミングで行う。
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        let mut gravity = GravityState::new();
        let player_pos = (1, 0);

        let outcome = shake_out_then_tick(&mut board, player_pos, &mut gravity); // 落下→押し潰し

        assert!(outcome.crushed);
        assert_eq!(board.cell(1, 0), Cell::Color(ColorKind::Red), "潰したブロックはその場に残って見えるはず");
        assert_eq!(outcome.moved_cells, vec![((1, 0), (0, 0))], "落下アニメーション用に着地移動も記録されるはず");
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

    #[test]
    fn falling_item_block_onto_player_does_not_crush_but_is_recorded_as_collected() {
        // アイテムブロックもAIRと同じ扱いにする(TERM独自拡張。ユーザー指摘: 「アイテムは
        // AIRと同じ用に…上から振ってきても死なないように」)。
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Item(ItemEffect::ClearAbove);
        let mut gravity = GravityState::new();
        let player_pos = (1, 0);

        let outcome = shake_out_then_tick(&mut board, player_pos, &mut gravity);

        assert!(!outcome.crushed, "アイテムブロックは押し潰し判定にならないはず");
        assert_eq!(board.cell(1, 0), Cell::Empty);
        assert_eq!(outcome.items_collected, vec![ItemEffect::ClearAbove]);
    }

    #[test]
    fn crush_from_one_falling_group_does_not_erase_another_unrelated_falling_group_in_the_same_tick() {
        // 発見: 同一tickに複数の無関係な塊が同時に落下していて、そのうち1つが
        // プレイヤーを押し潰す場合、旧実装では押し潰し確定時に即座にbreakしていたため、
        // 「他の(無関係な)塊」は旧位置こそ既にEmptyにされているのに新位置への
        // 書き込みだけスキップされ、盤面から消滅してしまっていた(revive()は盤面を
        // そのまま維持するため、このデータ消失は復活後のプレイにも影響する)。
        let mut board = empty_board(4);
        board.rows[0][0] = Cell::Color(ColorKind::Red); // プレイヤーを押し潰す塊
        board.rows[0][5] = Cell::Color(ColorKind::Blue); // 無関係な塊(消えてはいけない)
        let player_pos = (1, 0);
        let mut gravity = GravityState::new();

        let outcome = shake_out_then_tick(&mut board, player_pos, &mut gravity);

        assert!(outcome.crushed, "プレイヤーは押し潰されるはず");
        assert_eq!(
            board.cell(1, 0),
            Cell::Color(ColorKind::Red),
            "押し潰したブロックはその場(着地先)に残って見えるはず"
        );
        assert_eq!(
            board.cell(1, 5),
            Cell::Color(ColorKind::Blue),
            "無関係な塊は消滅せず、ちゃんと1マス落下しているはず"
        );
    }

    #[test]
    fn a_crush_in_one_group_does_not_suppress_auto_vanish_for_another_group_landing_the_same_tick() {
        // 発見: 同一tickに複数の無関係な塊が同時に落下していて、そのうち1つが
        // プレイヤーを押し潰す場合、旧実装ではoutcome.crushed=true時点で自動消滅判定
        // ループごと早期returnしていたため、押し潰しとは無関係な塊が同じtickで着地して
        // 4連結以上になっても自動消滅しなかった(ユーザー報告「緑に1ブロック結合したけど
        // 消えなかった」の一因になり得るバグ)。
        let mut board = empty_board(3);
        board.rows[0][0] = Cell::Color(ColorKind::Red); // プレイヤーを押し潰す塊
        board.rows[0][5] = Cell::Color(ColorKind::Blue); // 無関係に着地・自動消滅するはずの塊
        board.rows[0][6] = Cell::Color(ColorKind::Blue);
        board.rows[2][5] = Cell::Color(ColorKind::Blue); // 既に支持されている静的な塊(最深行)
        board.rows[2][6] = Cell::Color(ColorKind::Blue);
        let player_pos = (1, 0);
        let mut gravity = GravityState::new();

        let outcome = shake_out_then_tick(&mut board, player_pos, &mut gravity);

        assert!(outcome.crushed, "プレイヤーは押し潰されるはず");
        assert_eq!(
            outcome.auto_vanished_blocks, 4,
            "押し潰しと無関係な塊は、着地して4連結以上になったので自動消滅するはず"
        );
        assert_eq!(board.cell(2, 5), Cell::Empty);
        assert_eq!(board.cell(2, 6), Cell::Empty);
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

        assert_eq!(outcome.moved_cells.len(), 2, "落下グループの2個が同時に1マス落ちる");
        assert_eq!(outcome.auto_vanished_blocks, 4, "接触した結果、合計4個で自動消滅する");
        assert_eq!(board.cell(1, 0), Cell::Empty);
        assert_eq!(board.cell(1, 1), Cell::Empty);
        assert_eq!(board.cell(2, 0), Cell::Empty);
        assert_eq!(board.cell(2, 1), Cell::Empty);
    }

    #[test]
    fn l_shaped_falling_group_merges_with_static_group_offset_from_its_own_representative_cell() {
        // ユーザー指摘: 「結合されてるブロックの数が正しく4個以上と判定されてないかも。
        // この計上のとき、あらたに隣接したときに消えないことがある」。落下グループの
        // 代表座標(最小(row,col))自体は接触点に無くても、L字等の複雑な形でBFSが
        // 正しく全体を辿って4個以上を検出できることを確認する。
        //
        // 落下グループ(Red、L字): (0,3)-(1,3)-(1,2) の3セル。代表座標は(0,3)で、
        // 実際に接触するのは(1,2)側。既存の支持グループ(Red、3セル、最深行)は
        // row3のcol0,1,2。
        let mut board = empty_board(4);
        board.rows[3][0] = Cell::Color(ColorKind::Red);
        board.rows[3][1] = Cell::Color(ColorKind::Red);
        board.rows[3][2] = Cell::Color(ColorKind::Red);
        board.rows[0][3] = Cell::Color(ColorKind::Red);
        board.rows[1][2] = Cell::Color(ColorKind::Red);
        board.rows[1][3] = Cell::Color(ColorKind::Red);
        let mut gravity = GravityState::new();

        // 接触前は別グループ。
        assert_eq!(connected_same_color(&board, (3, 0), ColorKind::Red).len(), 3);
        assert_eq!(connected_same_color(&board, (0, 3), ColorKind::Red).len(), 3);

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity);

        assert_eq!(outcome.auto_vanished_blocks, 6, "接触後は合計6個で自動消滅するはず");
        for &(r, c) in &[(2, 3), (3, 3), (3, 2), (3, 1), (3, 0)] {
            assert_eq!(board.cell(r, c), Cell::Empty, "row={r},col={c}が消えていない");
        }
    }

    #[test]
    fn falling_block_that_lands_beside_a_static_column_still_triggers_auto_vanish() {
        // 発見: 落下してきた塊が既存の静的な塊の「真上」ではなく「横」に接触して
        // 結合する場合、着地したそのtickでは is_group_supported が「落下塊自身の
        // セルだけ」を見るため(隣の静的構造は別グループ扱いで支えの根拠にならない)
        // 未支持と判定されて自動消滅チェックがスキップされる。ところが次のtickには
        // collect_fall_groupsが両者を1つの塊として合体させ、静的構造側の支え
        // (最深行)によって「支持済み」と分類されてしまうため、二度と自動消滅
        // チェックの対象にならず、4連結以上のまま永久に残ってしまう。
        let mut board = empty_board(6); // row5が最深行
        board.rows[3][0] = Cell::Color(ColorKind::Red);
        board.rows[4][0] = Cell::Color(ColorKind::Red);
        board.rows[5][0] = Cell::Color(ColorKind::Red); // 静的な3連結(最深行で支持済み)
        board.rows[0][1] = Cell::Color(ColorKind::Red); // 隣の列を落ちてくる1個
        let mut gravity = GravityState::new();
        let player_pos = (usize::MAX, usize::MAX);

        for _ in 0..(SHAKE_TICKS as usize + 1) * 10 {
            apply_gravity_tick(&mut board, player_pos, &mut gravity, SHAKE_TICKS);
        }

        for r in 0..6 {
            assert_eq!(
                board.cell(r, 0),
                Cell::Empty,
                "row={r} col=0: 4連結以上になったので自動消滅しているはず"
            );
        }
        assert_eq!(board.cell(3, 1), Cell::Empty, "落下してきた側も自動消滅しているはず");
    }

    #[test]
    fn falling_block_touching_a_static_t_shaped_group_of_four_triggers_auto_vanish() {
        // ユーザー指摘: 「テトリスのトの字になってる構造に1個結合したら本来きえるべきが、
        // 消えない」。静的に生成されたT字/ト字型の4連結(それ自体は一度も落下していない
        // ため単独では消えない)に、落下してきた1個が新たに接触して合計5個になった
        // 場合、正しく自動消滅することを確認する。
        let mut board = empty_board(4);
        // 静的なT字(4個、最深行に固定=常に支持されている、一度も落下していない)。
        board.rows[3][0] = Cell::Color(ColorKind::Red);
        board.rows[3][1] = Cell::Color(ColorKind::Red);
        board.rows[3][2] = Cell::Color(ColorKind::Red);
        board.rows[2][1] = Cell::Color(ColorKind::Red);
        // 落下してくる1個(T字の縦棒の真上、col1)。
        board.rows[0][1] = Cell::Color(ColorKind::Red);
        let mut gravity = GravityState::new();

        // 接触前はT字(4個)と落下セル(1個)は別グループ。
        assert_eq!(connected_same_color(&board, (3, 0), ColorKind::Red).len(), 4);
        assert_eq!(connected_same_color(&board, (0, 1), ColorKind::Red).len(), 1);

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity);

        assert_eq!(outcome.auto_vanished_blocks, 5, "T字4個+落下1個=5個で自動消滅するはず");
        for &(r, c) in &[(1, 1), (3, 0), (3, 1), (3, 2), (2, 1)] {
            assert_eq!(board.cell(r, c), Cell::Empty, "row={r},col={c}が消えていない");
        }
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
            assert_eq!(outcome.moved_cells.len(), 0);
            assert!(gravity.is_shaking((0, 0)));
        }
        assert!(matches!(board.cell(0, 0), Cell::Rock { hits: 2 }), "揺れている間はまだ落下しない");

        let outcome = apply_gravity_tick(&mut board, (99, 99), &mut gravity, SHAKE_TICKS);
        assert_eq!(outcome.moved_cells.len(), 1);
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

        assert_eq!(outcome.moved_cells.len(), 0, "酸素カプセルは非Emptyなので上のRedは支持され落下しない");
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
            let board = Board::generate(seed, depth_rows, FIELD_WIDTH);
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
    fn a_plus_shaped_group_floating_over_a_wide_open_cavity_eventually_falls() {
        // ユーザー報告(スクリーンショット): 横棒3個+縦棒3個(縦棒が横棒の中央を貫く
        // 「十字/プラス」型)の赤ブロック塊が、直下が広い空洞なのに落ちてこない。
        let mut board = empty_board(30);
        board.rows[0][4] = Cell::Color(ColorKind::Red); // 十字の頭頂部
        board.rows[1][3] = Cell::Color(ColorKind::Red); // 横棒(左)
        board.rows[1][4] = Cell::Color(ColorKind::Red); // 横棒(中央、縦棒と共有)
        board.rows[1][5] = Cell::Color(ColorKind::Red); // 横棒(右)
        board.rows[2][4] = Cell::Color(ColorKind::Red); // 縦棒(下へ続く)
        board.rows[3][4] = Cell::Color(ColorKind::Red); // 縦棒(下へ続く)
        // row4以降、col4は最深行(row29)までずっとEmpty(広い空洞)。
        let mut gravity = GravityState::new();
        let player_pos = (usize::MAX, usize::MAX);

        for _ in 0..(SHAKE_TICKS as usize + 1) * 30 {
            apply_gravity_tick(&mut board, player_pos, &mut gravity, SHAKE_TICKS);
        }

        for group in collect_fall_groups(&board) {
            assert!(
                is_group_supported(&board, &group, player_pos),
                "十分な時間が経っても未支持のまま残っている塊がある: {group:?}"
            );
        }
        // 元の位置(浮いていた場所)からは動いているはず。6個連結なので最深行まで
        // 落ちきった時点で4個以上自動消滅ルールにより消滅する可能性もあるが、
        // 「浮いたまま元の位置に固まって残る」ことさえなければ良い。
        assert_eq!(
            board.cell(0, 4),
            Cell::Empty,
            "十字の頭頂部が元の位置に浮いたまま残っている(落下していない)"
        );
    }

    #[test]
    fn no_group_remains_unsupported_forever_on_random_boards() {
        // ユーザー報告「孤立した赤ブロックが落ちてこない」の統計的な回帰検証。
        // ランダム生成した盤面を十分な回数ティックさせ、最終的に全ての塊が
        // 「支持されている(=これ以上は落ちるはずがない)」状態に収束することを
        // 確認する。収束せず未支持のまま残る塊があれば、揺れ/連鎖判定のどこかで
        // 永続的に浮いたままになるバグがあることを意味する。
        for seed in 0..20u64 {
            let mut board = Board::generate(seed, 60, FIELD_WIDTH);
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
    fn no_group_remains_unsupported_forever_after_reroll_at_realistic_depth() {
        // ユーザー報告(スクリーンショット、深度418m Lv.14付近): 支えを失っているはずの
        // ブロックが崩れず浮いたままになる箇所がある。既存のno_group_remains_unsupported_
        // forever_on_random_boardsはBoard::generate直後(=reroll前)の浅い盤面(60行)しか
        // 検証していなかった。実際のプレイでは開始直後にreroll_overlays_from_rowが適用され、
        // かつ深度が進むほど岩ブロックの塊化ボーナスが強く効く(ROCK_CLUSTER_DEPTH_MAX_BONUS)
        // ため、そのギャップを埋めて深い深度(400〜600m帯)でも同じ不変条件を確認する。
        for seed in 0..2u64 {
            let mut board = Board::generate(seed, 70, FIELD_WIDTH);
            let gravity_for_reroll = GravityState::new();
            board.reroll_overlays_from_row(2, 100, 100, 100, 100, 0, 0, 0, 4, 100, &gravity_for_reroll);

            let mut gravity = GravityState::new();
            let player_pos = (usize::MAX, usize::MAX);

            for _ in 0..(SHAKE_TICKS as usize + 1) * 70 {
                apply_gravity_tick(&mut board, player_pos, &mut gravity, SHAKE_TICKS);
            }

            for group in collect_fall_groups(&board) {
                assert!(
                    is_group_supported(&board, &group, player_pos),
                    "seed={seed}: reroll後・深い深度で十分な時間が経っても未支持のまま残っている塊がある: {group:?}"
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

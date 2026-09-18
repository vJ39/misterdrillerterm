//! フィールド生成ロジック(spec.md 3章)。乱数シードからの下地色生成・岩/酸素/ダイヤ/
//! アイテムブロックの上書き配置・プレイ中の配分率変更(reroll)を扱う。

use super::*;

/// 深度帯ごとの岩・酸素・ダイヤの出現確率テーブル(spec.md 3.1)。
///
/// 色ブロックの内訳(4色均等)はこのテーブルでは扱わない。3.2〜3.4の近傍依存生成は
/// 深度に関わらず常に4色均等抽選のため、`color_each`のような個別値は不要(3.1末尾参照)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandTable {
    pub rock: f32,
    pub oxygen: f32,
    pub diamond: f32,
    /// スターブロックの出現率(深度に関わらず`STAR_SPAWN_PROB`で一定)。
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
/// 色ブロック同士の内訳にのみ影響する(3.1の深度帯別出現確率とは無関係)。
const LEFT_INHERIT_PROB: f32 = 0.65;

/// 左隣が不採用の場合に上隣を同色にする確率の上限(累積、spec.md 3.2)。
/// 残り10%(1.0 - 0.90)が4色均等の完全ランダム抽選になる。
const TOP_INHERIT_PROB_CEIL: f32 = 0.90;

/// 左隣・上隣の色をもとに候補色を1つ選ぶ(spec.md 3.2)。
///
/// - `r < LEFT_INHERIT_PROB`(0.65) かつ左隣が色ブロックなら左隣と同色
/// - それ以外で `r < TOP_INHERIT_PROB_CEIL`(0.90) かつ上隣が色ブロックなら上隣と同色
/// - どちらにも該当しなければ4色から均等ランダム
fn pick_base_color(
    rng: &mut ChaCha8Rng,
    left: Option<ColorKind>,
    top: Option<ColorKind>,
) -> ColorKind {
    let r: f32 = rng.random_range(0.0..1.0);
    if r < LEFT_INHERIT_PROB
        && let Some(c) = left
    {
        return c;
    }
    if r < TOP_INHERIT_PROB_CEIL
        && let Some(c) = top
    {
        return c;
    }
    ColorKind::ALL[rng.random_range(0..4)]
}

/// 同色連続の上限(横4・縦3、spec.md 3.3)を超える場合に候補色を差し替える。差し替え先は
/// 追加の乱数を消費せず、`ColorKind::ALL`の先頭`color_count`色の固定順で最初に制約に
/// 抵触しない色を採用する。該当色が無ければ候補のまま返す(色数1ではランを制限できない)。
fn resolve_run_limits(
    candidate: ColorKind,
    left3: [Option<ColorKind>; 3],
    top2: [Option<ColorKind>; 2],
    color_count: usize,
) -> ColorKind {
    let breaks_horizontal = |c: ColorKind| left3.iter().all(|n| *n == Some(c));
    let breaks_vertical = |c: ColorKind| top2.iter().all(|n| *n == Some(c));

    if !breaks_horizontal(candidate) && !breaks_vertical(candidate) {
        return candidate;
    }
    ColorKind::ALL[..color_count]
        .iter()
        .copied()
        .find(|&c| !breaks_horizontal(c) && !breaks_vertical(c))
        .unwrap_or(candidate)
}

/// セルが色ブロックならその色を返す。ラン判定は「同色ブロックが実際に連結するか」が
/// 基準のため、Rock/Oxygen等が挟まればランは途切れる(色ブロック以外は常に`None`扱い)。
fn color_of(cell: Cell) -> Option<ColorKind> {
    match cell {
        Cell::Color(c) => Some(c),
        _ => None,
    }
}

/// コース全行の色下地を生成する(spec.md 3.2〜3.3)。戻り値は行×列の`Option<ColorKind>`で、
/// `None`は色ブロック無しを表す。安全地帯(先頭2行)は生成パスを適用せず常に`None`のままにし、
/// 3行目以降の近傍依存生成が実在しないダミー色の影響を受けないようにする。
fn generate_base_colors(
    rng: &mut ChaCha8Rng,
    depth_rows: usize,
    width: usize,
) -> Vec<Vec<Option<ColorKind>>> {
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

            base[row][col] = Some(resolve_run_limits(
                candidate,
                left3,
                top2,
                ColorKind::ALL.len(),
            ));
        }
    }

    base
}

/// マス`(row, col)`の上下左右のうち、盤内かつ色ブロックであるものの色一覧(spec.md 3.4)。
fn same_color_neighbor_candidates(
    base: &[Vec<Option<ColorKind>>],
    row: usize,
    col: usize,
) -> Vec<ColorKind> {
    let rows = base.len();
    let width = base[row].len();
    let mut neighbors = Vec::with_capacity(4);
    if row > 0
        && let Some(c) = base[row - 1][col]
    {
        neighbors.push(c);
    }
    if row + 1 < rows
        && let Some(c) = base[row + 1][col]
    {
        neighbors.push(c);
    }
    if col > 0
        && let Some(c) = base[row][col - 1]
    {
        neighbors.push(c);
    }
    if col + 1 < width
        && let Some(c) = base[row][col + 1]
    {
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

/// 孤立セルの解消(生成後の後処理、spec.md 3.4)。盤面全体に1回だけ、行→列の順に走査
/// しながらその場で書き換える(スナップショットを取らず逐次反映し、置換済み隣接セルの
/// 新しい色を後続の判定が参照することも許容する)。
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
/// アイテム3種は常に配分率0(生成しない)で呼ぶ。rerollは既存アイテムを上書きしない仕様の
/// ため、初期生成で置くと直後の設定値reroll(0%指定)でも除去できず残ってしまう。
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
        0,
        0,
        0,
        0.0,
        true, // 初期生成の候補は常に「まだ色ブロック」なのでスター抽選の対象
        item_caps,
    )
}

/// アイテムブロック3種の「あと何個まで出現させてよいか」の残数。呼び出し側が既存個数と
/// `ITEM_MAX_COUNT_ON_BOARD`の差分で初期化し、1個出現させるたびに1減らす。
/// 0になった種類はそれ以上出現しない。
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
            starify_screen_remaining: max
                .saturating_sub(board.count_item(ItemEffect::StarifyScreen)),
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

/// `overlay_rock_oxygen_diamond`の、岩/AIR/スター/ダイヤ・アイテム3種の出現率を設定値
/// (%、100=通常)で調整できる版。`rock_cluster_bonus`は岩の出現確率への加算ボーナス
/// (呼び出し側が隣接岩・深度から算出。初期生成では常に0.0)。`allow_star`=falseはスター
/// 抽選自体を無効化する(スター化対象のセルか・揺れ中/落下中でないかは呼び出し側が判定)。
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
    // アイテム3種は岩/AIR/スター/ダイヤとは独立したごく低確率の抽選で、設定から個別に
    // 調整できる。上限個数に達した種類はそれ以上抽選対象にしない。
    let item_clear_above = if item_caps.clear_above_remaining > 0 {
        crate::constants::ITEM_CLEAR_ABOVE_SPAWN_PROB * item_clear_above_rate_percent as f32 / 100.0
    } else {
        0.0
    };
    let item_unify_colors = if item_caps.unify_colors_remaining > 0 {
        crate::constants::ITEM_UNIFY_COLORS_SPAWN_PROB * item_unify_colors_rate_percent as f32
            / 100.0
    } else {
        0.0
    };
    let item_starify_screen = if item_caps.starify_screen_remaining > 0 {
        crate::constants::ITEM_STARIFY_SCREEN_SPAWN_PROB * item_starify_screen_rate_percent as f32
            / 100.0
    } else {
        0.0
    };

    // ルーレット式抽選。各候補の確率ぶんを順に積み上げ、rが最初に収まった区間を採用する。
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

/// アイテムブロック3種だけの独立したルーレット抽選(`Board::top_up_items`用)。
/// 内容が確定済みのセルに対して、後からアイテムへ変えるかどうかだけを判定する。
/// 選ばれなければ`None`を返し、呼び出し側は元のセル内容をそのまま残す。
fn roll_item_effect_only(
    rng: &mut ChaCha8Rng,
    item_clear_above_rate_percent: u32,
    item_unify_colors_rate_percent: u32,
    item_starify_screen_rate_percent: u32,
    item_caps: &mut ItemSpawnCaps,
) -> Option<ItemEffect> {
    let item_clear_above = if item_caps.clear_above_remaining > 0 {
        crate::constants::ITEM_CLEAR_ABOVE_SPAWN_PROB * item_clear_above_rate_percent as f32 / 100.0
    } else {
        0.0
    };
    let item_unify_colors = if item_caps.unify_colors_remaining > 0 {
        crate::constants::ITEM_UNIFY_COLORS_SPAWN_PROB * item_unify_colors_rate_percent as f32
            / 100.0
    } else {
        0.0
    };
    let item_starify_screen = if item_caps.starify_screen_remaining > 0 {
        crate::constants::ITEM_STARIFY_SCREEN_SPAWN_PROB * item_starify_screen_rate_percent as f32
            / 100.0
    } else {
        0.0
    };

    let r: f32 = rng.random_range(0.0..1.0);
    let mut threshold = item_clear_above;
    if r < threshold {
        item_caps.clear_above_remaining -= 1;
        return Some(ItemEffect::ClearAbove);
    }
    threshold += item_unify_colors;
    if r < threshold {
        item_caps.unify_colors_remaining -= 1;
        return Some(ItemEffect::UnifyColors);
    }
    threshold += item_starify_screen;
    if r < threshold {
        item_caps.starify_screen_remaining -= 1;
        return Some(ItemEffect::StarifyScreen);
    }
    None
}

/// 行内の全マスが岩ブロックになっている場合、少なくとも1マスを色ブロックへ差し替える。
/// 岩だけで完全にふさがった横一列は通過できない壁になるため、必ず逃げ道を1マス残す。
fn ensure_row_is_not_fully_blocked_by_rock(
    row_cells: &mut [Cell],
    rng: &mut ChaCha8Rng,
    color_count: usize,
) {
    if row_cells.iter().all(|c| matches!(c, Cell::Rock { .. })) {
        let escape_col = rng.random_range(0..row_cells.len());
        let escape_color = ColorKind::ALL[rng.random_range(0..color_count)];
        row_cells[escape_col] = Cell::Color(escape_color);
    }
}

impl Board {
    /// 乱数シードから深さdepth_rows行×width列のフィールドを事前生成する(spec.md 3.6)。
    /// 手順: 3.2〜3.3の下地生成→3.4の孤立セル解消を盤面全体に1回→3.5の上書きを全マスに
    /// 適用する。この順序で1回だけ行い、生成し直しはしない。
    pub fn generate(seed: u64, depth_rows: usize, width: usize) -> Self {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);

        let mut base = generate_base_colors(&mut rng, depth_rows, width);
        fix_isolated_cells(&mut base);

        // 新規生成の盤面はまだアイテムを1つも含まないため、常に上限いっぱいから始める。
        let mut item_caps = ItemSpawnCaps::fresh();
        let rows = (0..depth_rows)
            .map(|row| {
                let mut cells = vec![Cell::Empty; width];
                for (col, cell) in cells.iter_mut().enumerate() {
                    *cell = match base[row][col] {
                        None => Cell::Empty, // 安全地帯(深度0〜1m)
                        Some(color) => {
                            overlay_rock_oxygen_diamond(&mut rng, color, row, &mut item_caps)
                        }
                    };
                }
                ensure_row_is_not_fully_blocked_by_rock(&mut cells, &mut rng, ColorKind::ALL.len());
                cells
            })
            .collect();

        Board { rows, width }
    }

    /// アイテム3種を、プレイヤー前方の窓内で常に`ITEM_MAX_COUNT_ON_BOARD`個になるよう
    /// 補充する。窓内(`window_start_row`〜`frontier_row`)の既存個数を数え、不足分だけ
    /// 未抽選領域`[frontier_row, target_row)`へ追加抽選する(Empty・Itemセルは対象外)。
    /// 窓が進むと補充余地が生まれ「盤面全体で生涯N個」でなく「常に前方に最大N個」になる。
    ///
    /// `rng`は呼び出し元(`Game`)がゲーム開始時のシードから作って持ち回す乱数源。
    /// OS乱数から都度作り直すと同じシードでも盤面が再現されないため、必ず共有の系列を
    /// 消費する(#221。ソークテストで失敗したシードを再現するために必須)。
    #[allow(clippy::too_many_arguments)]
    pub fn top_up_items(
        &mut self,
        rng: &mut ChaCha8Rng,
        window_start_row: usize,
        frontier_row: usize,
        target_row: usize,
        item_clear_above_rate_percent: u32,
        item_unify_colors_rate_percent: u32,
        item_starify_screen_rate_percent: u32,
    ) {
        if target_row <= frontier_row {
            return;
        }

        // window_start_rowがfrontier_rowより先に進んでいる場合、確定済みの窓内は
        // 実質空なので、カウント対象の開始行はfrontier_rowを超えないようにする。
        let count_start_row = window_start_row.min(frontier_row);
        let max = crate::constants::ITEM_MAX_COUNT_ON_BOARD;
        let mut caps = ItemSpawnCaps {
            clear_above_remaining: max.saturating_sub(self.count_item_in_range(
                ItemEffect::ClearAbove,
                count_start_row,
                frontier_row,
            )),
            unify_colors_remaining: max.saturating_sub(self.count_item_in_range(
                ItemEffect::UnifyColors,
                count_start_row,
                frontier_row,
            )),
            starify_screen_remaining: max.saturating_sub(self.count_item_in_range(
                ItemEffect::StarifyScreen,
                count_start_row,
                frontier_row,
            )),
        };

        for row in frontier_row..target_row.min(self.rows.len()) {
            for col in 0..self.width {
                let current = self.rows[row][col];
                if current == Cell::Empty || matches!(current, Cell::Item(_)) {
                    continue;
                }
                if let Some(effect) = roll_item_effect_only(
                    rng,
                    item_clear_above_rate_percent,
                    item_unify_colors_rate_percent,
                    item_starify_screen_rate_percent,
                    &mut caps,
                ) {
                    self.rows[row][col] = Cell::Item(effect);
                }
            }
        }
    }

    /// `from_row`以降の未掘削マス(Empty以外)の色・岩・AIR・スター・ダイヤ内訳を配分率
    /// (%、100=通常)で丸ごと再抽選する。元の内容を問わず対象にするため、初期生成で既定率
    /// のまま確定していたセルにも設定値が正しく反映される(掘削済みEmptyのみ、プレイヤーが
    /// 見た/触れた状態を壊さないよう対象外)。
    ///
    /// `rng`は呼び出し元(`Game`)が持ち回す共有の乱数源(`top_up_items`と同じ理由。#221)。
    ///
    /// `color_count`(1〜4)は色抽選を`ColorKind::ALL`の先頭N色に制限する(範囲外はクランプ)。
    /// 深度が進むほど色ブロックはばらけ、岩は隣接ボーナスで固まる難易度カーブを持つ。
    /// `color_cluster_rate_percent`は色の結合しやすさへの乗算係数(0%で常に完全ランダム抽選)。
    #[allow(clippy::too_many_arguments)]
    pub fn reroll_overlays_from_row(
        &mut self,
        rng: &mut ChaCha8Rng,
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
        let to_row = self.rows.len();
        self.reroll_overlays_in_row_range(
            rng,
            from_row,
            to_row,
            rock_rate_percent,
            air_rate_percent,
            star_rate_percent,
            diamond_rate_percent,
            item_clear_above_rate_percent,
            item_unify_colors_rate_percent,
            item_starify_screen_rate_percent,
            color_count,
            color_cluster_rate_percent,
            gravity,
        );
    }

    /// `reroll_overlays_from_row`と同じ再抽選を、`from_row..to_row`(to_row自体は含まない)
    /// の範囲だけに限定して行う。特定の深度帯だけに特別な配分率を適用したい場合
    /// (ボーナスフロア等)に使う。
    #[allow(clippy::too_many_arguments)]
    pub fn reroll_overlays_in_row_range(
        &mut self,
        rng: &mut ChaCha8Rng,
        from_row: usize,
        to_row: usize,
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
        let color_count = (color_count as usize).clamp(1, ColorKind::ALL.len());
        // 盤面全体(from_rowより前の既存アイテムも含む)の既存個数を先に数えてから
        // 上限を計算する。
        let mut item_caps = ItemSpawnCaps::from_existing_counts(self);

        for row in from_row..to_row.min(self.rows.len()) {
            let fraction = depth_fraction(row);
            let color_cluster_prob = (COLOR_CLUSTER_DEPTH_START_PROB
                * (1.0 - fraction)
                * (color_cluster_rate_percent as f32 / 100.0))
                .clamp(0.0, 1.0);
            let rock_cluster_bonus_if_adjacent = ROCK_CLUSTER_DEPTH_MAX_BONUS * fraction;

            for col in 0..self.width {
                let current = self.rows[row][col];
                // 既に置かれたアイテムブロックは配分率再抽選の対象外。Emptyと同様
                // 「既に確定した内容」として扱い、そのまま残す(配置済み・落下中の保護)。
                if current == Cell::Empty || matches!(current, Cell::Item(_)) {
                    continue;
                }

                let left = if col > 0 {
                    Some(self.rows[row][col - 1])
                } else {
                    None
                };
                let top = if row > 0 {
                    Some(self.rows[row - 1][col])
                } else {
                    None
                };

                let fresh_color = match left {
                    Some(Cell::Color(c)) if rng.random_range(0.0..1.0) < color_cluster_prob => c,
                    _ => ColorKind::ALL[rng.random_range(0..color_count)],
                };
                // 同色ランが横4・縦3(spec.md 3.3)を超えないよう調整する。初期生成と
                // 同じ上限を課さないと、rerollで巨大な同色塊が生成されうる。
                let left3 = [
                    if col >= 1 {
                        color_of(self.rows[row][col - 1])
                    } else {
                        None
                    },
                    if col >= 2 {
                        color_of(self.rows[row][col - 2])
                    } else {
                        None
                    },
                    if col >= 3 {
                        color_of(self.rows[row][col - 3])
                    } else {
                        None
                    },
                ];
                let top2 = [
                    if row >= 1 {
                        color_of(self.rows[row - 1][col])
                    } else {
                        None
                    },
                    if row >= 2 {
                        color_of(self.rows[row - 2][col])
                    } else {
                        None
                    },
                ];
                let fresh_color = resolve_run_limits(fresh_color, left3, top2, color_count);
                let adjacent_is_rock = matches!(left, Some(Cell::Rock { .. }))
                    || matches!(top, Some(Cell::Rock { .. }));
                let rock_cluster_bonus = if adjacent_is_rock {
                    rock_cluster_bonus_if_adjacent
                } else {
                    0.0
                };

                // スターへの再抽選は、元がXブロックまたはダイヤブロックだったセルに限る。
                // 揺れ中/落下中のセルは対象外にする。
                let was_rock_or_diamond = matches!(current, Cell::Rock { .. } | Cell::Diamond);
                let allow_star = was_rock_or_diamond && !gravity.is_shaking((row, col));

                self.rows[row][col] = overlay_rock_oxygen_diamond_with_rates(
                    rng,
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

            ensure_row_is_not_fully_blocked_by_rock(&mut self.rows[row], rng, color_count);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::empty_board;
    use super::*;
    use crate::constants::FIELD_WIDTH_DEFAULT as FIELD_WIDTH;

    // --- 生成: 安全地帯(先頭2行)は常にEmpty ---

    #[test]
    fn generate_keeps_first_two_rows_empty() {
        let board = Board::generate(1, 50, FIELD_WIDTH);
        for col in 0..FIELD_WIDTH {
            assert_eq!(board.cell(0, col), Cell::Empty);
            assert_eq!(board.cell(1, col), Cell::Empty);
        }
    }

    // --- プレイ中の配分率(岩/AIR)変更 ---

    #[test]
    fn reroll_overlays_from_row_leaves_rows_before_from_row_untouched() {
        let mut rng = ChaCha8Rng::seed_from_u64(25);
        let mut board = empty_board(5);
        for col in 0..FIELD_WIDTH {
            board.rows[0][col] = Cell::Color(ColorKind::Red); // from_rowより手前
        }

        board.reroll_overlays_from_row(
            &mut rng,
            1,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            4,
            100,
            &GravityState::new(),
        ); // 岩/AIR/スター/ダイヤの確率を0に

        for col in 0..FIELD_WIDTH {
            assert_eq!(
                board.cell(0, col),
                Cell::Color(ColorKind::Red),
                "from_rowより手前は変わらない"
            );
        }
    }

    #[test]
    fn reroll_overlays_from_row_also_rerolls_cells_already_committed_to_an_overlay() {
        let mut rng = ChaCha8Rng::seed_from_u64(24);
        // 初期生成で既に岩/AIR/スター/ダイヤとして確定していたセルも、Empty以外なら
        // 元の種類を問わず再抽選対象になることを確認する。
        let mut board = empty_board(1);
        board.rows[0][0] = Cell::Rock { hits: 3 };
        board.rows[0][1] = Cell::Oxygen;
        board.rows[0][2] = Cell::Star { visible_ms: 2000 };
        board.rows[0][3] = Cell::Diamond;
        board.rows[0][4] = Cell::Empty; // 既に掘削済み・対象外

        // 岩/AIR/スター/ダイヤの配分率を全て0にすれば、Empty以外の全セルは必ず
        // Color(通常の色ブロック)へ再抽選される。
        board.reroll_overlays_from_row(
            &mut rng,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            4,
            100,
            &GravityState::new(),
        );

        for col in 0..4 {
            assert!(
                matches!(board.cell(0, col), Cell::Color(_)),
                "配分率0%なら既存の確定セルも含めて色ブロックへ再抽選されるはず: col={col} -> {:?}",
                board.cell(0, col)
            );
        }
        assert_eq!(
            board.cell(0, 4),
            Cell::Empty,
            "既に掘削済みのセルは対象外のまま"
        );
    }

    #[test]
    fn reroll_overlays_from_row_never_overwrites_existing_item_blocks() {
        let mut rng = ChaCha8Rng::seed_from_u64(23);
        // 既に配置済みのアイテムブロックは、配分率の再抽選で上書きされず
        // 「確定した内容」としてそのまま残ることを確認する。
        let mut board = empty_board(1);
        board.rows[0][0] = Cell::Item(ItemEffect::ClearAbove);
        board.rows[0][1] = Cell::Item(ItemEffect::UnifyColors);
        board.rows[0][2] = Cell::Item(ItemEffect::StarifyScreen);

        board.reroll_overlays_from_row(
            &mut rng,
            0,
            300,
            300,
            300,
            300,
            300,
            300,
            300,
            4,
            100,
            &GravityState::new(),
        );

        assert_eq!(
            board.cell(0, 0),
            Cell::Item(ItemEffect::ClearAbove),
            "Rアイテムは再抽選で上書きされないはず"
        );
        assert_eq!(
            board.cell(0, 1),
            Cell::Item(ItemEffect::UnifyColors),
            "Cアイテムは再抽選で上書きされないはず"
        );
        assert_eq!(
            board.cell(0, 2),
            Cell::Item(ItemEffect::StarifyScreen),
            "Kアイテムは再抽選で上書きされないはず"
        );
    }

    #[test]
    fn reroll_overlays_from_row_higher_rock_rate_yields_more_rock_cells_on_average() {
        let mut rng = ChaCha8Rng::seed_from_u64(22);
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
            board
                .rows
                .iter()
                .flatten()
                .filter(|c| matches!(c, Cell::Rock { .. }))
                .count()
        }

        let mut low = all_color_board(500);
        low.reroll_overlays_from_row(
            &mut rng,
            0,
            20,
            100,
            100,
            100,
            0,
            0,
            0,
            4,
            100,
            &GravityState::new(),
        );
        let mut high = all_color_board(500);
        high.reroll_overlays_from_row(
            &mut rng,
            0,
            300,
            100,
            100,
            100,
            0,
            0,
            0,
            4,
            100,
            &GravityState::new(),
        );

        let (low_count, high_count) = (count_rocks(&low), count_rocks(&high));
        assert!(
            high_count > low_count * 2,
            "配分率を上げれば統計的に岩ブロックが明確に増えるはず: low={low_count}, high={high_count}"
        );
    }

    #[test]
    fn reroll_overlays_from_row_star_rate_zero_produces_no_star_cells() {
        let mut rng = ChaCha8Rng::seed_from_u64(21);
        // スター配分率0%なら、通常なら出現するはずのスターブロックが一切生成されない
        // ことを確認する。
        let mut board = empty_board(500);
        for row in 0..500 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }

        board.reroll_overlays_from_row(
            &mut rng,
            0,
            100,
            100,
            0,
            100,
            0,
            0,
            0,
            4,
            100,
            &GravityState::new(),
        );

        let star_count = board
            .rows
            .iter()
            .flatten()
            .filter(|c| matches!(c, Cell::Star { .. }))
            .count();
        assert_eq!(
            star_count, 0,
            "スター配分率0%ならスターブロックは一切出現しないはず"
        );
    }

    #[test]
    fn reroll_overlays_from_row_spawns_all_three_kinds_of_item_blocks() {
        let mut rng = ChaCha8Rng::seed_from_u64(20);
        // 出現率はごく低確率の値のため、十分な行数で統計的にアイテム3種とも
        // 出現することを確認する。
        let mut board = empty_board(5000);
        for row in 0..5000 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }

        board.reroll_overlays_from_row(
            &mut rng,
            0,
            100,
            100,
            100,
            100,
            100,
            100,
            100,
            4,
            100,
            &GravityState::new(),
        );

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
        assert!(
            clear_above_count > 0,
            "ClearAboveアイテムが1つも出現しないのは不自然"
        );
        assert!(
            unify_colors_count > 0,
            "UnifyColorsアイテムが1つも出現しないのは不自然"
        );
        assert!(
            starify_screen_count > 0,
            "StarifyScreenアイテムが1つも出現しないのは不自然"
        );
    }

    #[test]
    fn reroll_overlays_from_row_item_rate_percent_controls_each_item_independently() {
        let mut rng = ChaCha8Rng::seed_from_u64(19);
        // アイテムごとに配分率が独立していること: ClearAboveだけ0%にすれば出現せず、
        // 他の2種は100%のまま出現し続けることを確認する。
        let mut board = empty_board(5000);
        for row in 0..5000 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }

        board.reroll_overlays_from_row(
            &mut rng,
            0,
            100,
            100,
            100,
            100,
            0,
            100,
            100,
            4,
            100,
            &GravityState::new(),
        );

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
        assert!(
            unify_colors_count > 0,
            "UnifyColorsは100%のままなので出現し続けるはず"
        );
        assert!(
            starify_screen_count > 0,
            "StarifyScreenは100%のままなので出現し続けるはず"
        );
    }

    #[test]
    fn reroll_overlays_from_row_never_exceeds_the_per_item_type_cap_on_the_board() {
        let mut rng = ChaCha8Rng::seed_from_u64(18);
        // 出現率を極端に高くしても、盤面全体で種類ごとに上限個数を超えないことを確認する。
        let mut board = empty_board(5000);
        for row in 0..5000 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }

        board.reroll_overlays_from_row(
            &mut rng,
            0,
            0,
            0,
            0,
            0,
            300,
            300,
            300,
            4,
            100,
            &GravityState::new(),
        );

        for effect in [
            ItemEffect::ClearAbove,
            ItemEffect::UnifyColors,
            ItemEffect::StarifyScreen,
        ] {
            let count = board.count_item(effect);
            assert!(
                count <= crate::constants::ITEM_MAX_COUNT_ON_BOARD,
                "{effect:?}は上限{}個を超えてはいけないはず(実際={count})",
                crate::constants::ITEM_MAX_COUNT_ON_BOARD
            );
        }
    }

    #[test]
    fn board_generate_never_places_any_item_blocks() {
        // rerollは既存アイテムを上書きしないため、Board::generateの時点で置くと0%指定
        // でも除去できず残る。generateはアイテムを一切置かないのが正しい仕様
        // (実際の配分率は必ず直後のrerollで反映される)。
        let board = Board::generate(1, 5000, FIELD_WIDTH);

        for effect in [
            ItemEffect::ClearAbove,
            ItemEffect::UnifyColors,
            ItemEffect::StarifyScreen,
        ] {
            let count = board.count_item(effect);
            assert_eq!(count, 0, "{effect:?}はBoard::generateの時点で0個のはず");
        }
    }

    #[test]
    fn item_rate_zero_at_new_game_start_removes_items_generated_by_board_generate() {
        let mut rng = ChaCha8Rng::seed_from_u64(17);
        // Board::generate直後に新規ゲーム開始時と同じreroll(item rate=0)を適用すれば、
        // アイテムブロックが1つも残らないことを確認する。
        let mut board = Board::generate(1, 2000, FIELD_WIDTH);
        board.reroll_overlays_from_row(
            &mut rng,
            2,
            100,
            100,
            100,
            100,
            0,
            0,
            0,
            4,
            100,
            &GravityState::new(),
        );

        for effect in [
            ItemEffect::ClearAbove,
            ItemEffect::UnifyColors,
            ItemEffect::StarifyScreen,
        ] {
            let count = board.count_item(effect);
            assert_eq!(
                count, 0,
                "{effect:?}は配分0%でのreroll後は0個のはず(実際={count})"
            );
        }
    }

    #[test]
    fn reroll_overlays_from_row_counts_pre_existing_items_toward_the_cap() {
        let mut rng = ChaCha8Rng::seed_from_u64(16);
        // 既に盤面上にあるアイテムの個数も上限に含めて計算し、残り枠ぶんしか
        // 新規出現させないことを確認する。
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

        board.reroll_overlays_from_row(
            &mut rng,
            1,
            0,
            0,
            0,
            0,
            300,
            0,
            0,
            4,
            100,
            &GravityState::new(),
        );

        let count = board.count_item(ItemEffect::ClearAbove);
        assert!(
            count <= max,
            "既存分を含めても上限{max}個を超えてはいけないはず(実際={count})"
        );
    }

    #[test]
    fn reroll_overlays_from_row_never_converts_existing_color_or_oxygen_cells_into_stars() {
        let mut rng = ChaCha8Rng::seed_from_u64(15);
        // スターへ変わるのは岩とダイヤのみ。既存のColor/Oxygenセルはスター配分率を
        // 上限にしてもスターへ変わらないことを確認する。
        let mut board = empty_board(3);
        for col in 0..FIELD_WIDTH {
            board.rows[0][col] = Cell::Color(ColorKind::Red);
            board.rows[1][col] = Cell::Oxygen;
        }

        board.reroll_overlays_from_row(
            &mut rng,
            0,
            0,
            0,
            300,
            0,
            0,
            0,
            0,
            4,
            100,
            &GravityState::new(),
        );

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
        let mut rng = ChaCha8Rng::seed_from_u64(14);
        // 既存のRock/Diamondセルは、スター配分率を上限にすればスターへ変わり得る
        // ことを統計的に確認する(0件は不自然)。
        let mut board = empty_board(500);
        for row in 0..500 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = if col % 2 == 0 {
                    Cell::Rock { hits: 0 }
                } else {
                    Cell::Diamond
                };
            }
        }

        board.reroll_overlays_from_row(
            &mut rng,
            0,
            0,
            0,
            300,
            0,
            0,
            0,
            0,
            4,
            100,
            &GravityState::new(),
        );

        let star_count = board
            .rows
            .iter()
            .flatten()
            .filter(|c| matches!(c, Cell::Star { .. }))
            .count();
        assert!(
            star_count > 0,
            "Xブロック・ダイヤブロックはスターへ変わり得るはず(0件は不自然)"
        );
    }

    #[test]
    fn reroll_overlays_from_row_never_converts_shaking_cells_into_stars() {
        let mut rng = ChaCha8Rng::seed_from_u64(13);
        // 元がRockなら本来スター化対象だが、揺れ中のセルは除外されることを確認する。
        let mut board = empty_board(1);
        for col in 0..FIELD_WIDTH {
            board.rows[0][col] = Cell::Rock { hits: 0 };
        }
        let mut gravity = GravityState::new();
        for col in 0..FIELD_WIDTH {
            gravity.shaking_cells.insert((0, col));
        }

        board.reroll_overlays_from_row(&mut rng, 0, 0, 0, 300, 0, 0, 0, 0, 4, 100, &gravity);

        let star_count = board
            .rows
            .iter()
            .flatten()
            .filter(|c| matches!(c, Cell::Star { .. }))
            .count();
        assert_eq!(star_count, 0, "揺れ中のセルはスターへ変わらないはず");
    }

    #[test]
    fn reroll_overlays_from_row_diamond_rate_zero_produces_no_diamond_cells() {
        let mut rng = ChaCha8Rng::seed_from_u64(12);
        // ダイヤ配分率0%なら、通常なら出現するはずのダイヤブロックが一切生成されない
        // ことを確認する。
        let mut board = empty_board(500);
        for row in 0..500 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }

        board.reroll_overlays_from_row(
            &mut rng,
            0,
            100,
            100,
            100,
            0,
            0,
            0,
            0,
            4,
            100,
            &GravityState::new(),
        );

        let diamond_count = board
            .rows
            .iter()
            .flatten()
            .filter(|&&c| c == Cell::Diamond)
            .count();
        assert_eq!(
            diamond_count, 0,
            "ダイヤ配分率0%ならダイヤブロックは一切出現しないはず"
        );
    }

    #[test]
    fn reroll_overlays_from_row_color_count_restricts_the_palette_to_the_first_n_colors() {
        let mut rng = ChaCha8Rng::seed_from_u64(11);
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

        board.reroll_overlays_from_row(
            &mut rng,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            2,
            100,
            &GravityState::new(),
        );

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
        let mut rng = ChaCha8Rng::seed_from_u64(10);
        let mut board = empty_board(200);
        for row in 0..200 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Blue);
            }
        }

        board.reroll_overlays_from_row(
            &mut rng,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            1,
            100,
            &GravityState::new(),
        );

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
        let mut rng = ChaCha8Rng::seed_from_u64(9);
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
            if total_runs == 0 {
                0.0
            } else {
                total_len as f64 / total_runs as f64
            }
        }

        let mut board = empty_board(1000);
        for row in 0..1000 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }
        // 岩/AIR/スター/ダイヤは無しにして、純粋に色の連結だけを観測する。
        board.reroll_overlays_from_row(
            &mut rng,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            4,
            100,
            &GravityState::new(),
        );

        let shallow_avg = avg_run_length_in_range(&board, 2..200);
        let deep_avg = avg_run_length_in_range(&board, 800..1000);

        assert!(
            shallow_avg > deep_avg + 0.1,
            "浅い深度の方が横方向のまとまりが強いはず: shallow={shallow_avg}, deep={deep_avg}"
        );
    }

    #[test]
    fn reroll_overlays_from_row_color_cluster_rate_percent_scales_clustering_strength() {
        let mut rng = ChaCha8Rng::seed_from_u64(8);
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
            if total_runs == 0 {
                0.0
            } else {
                total_len as f64 / total_runs as f64
            }
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
        zero_rate.reroll_overlays_from_row(
            &mut rng,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            4,
            0,
            &GravityState::new(),
        );
        let mut default_rate = make_board();
        default_rate.reroll_overlays_from_row(
            &mut rng,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            4,
            100,
            &GravityState::new(),
        );

        let zero_avg = avg_run_length_in_range(&zero_rate, 2..200);
        let default_avg = avg_run_length_in_range(&default_rate, 2..200);

        assert!(
            default_avg > zero_avg + 0.1,
            "100%設定の方が0%設定よりまとまりが強いはず: zero={zero_avg}, default={default_avg}"
        );
    }

    #[test]
    fn reroll_overlays_from_row_never_produces_a_same_color_run_beyond_the_spec_limit() {
        let mut rng = ChaCha8Rng::seed_from_u64(7);
        // ユーザー報告(#114): 「8000フレーム付近で縦に大量消失してった」の根本原因調査で、
        // reroll_overlays_from_row(新規ゲーム開始時に必ず盤面全体へ適用される)が
        // 初期生成(generate_base_colors)の同色ラン上限(横4・縦3、spec.md 3.3)を
        // 継承しておらず、無制限に同色が連続しうることが判明した(#118でresolve_run_limits
        // を移植)。結合が最も強くなる浅い深度・rate=100%の条件で、横4・縦3を超える
        // 同色の直線ランが生成されないことを確認する。
        let mut board = empty_board(500);
        for row in 0..500 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }

        board.reroll_overlays_from_row(
            &mut rng,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            4,
            100,
            &GravityState::new(),
        );

        for row in 2..500 {
            let mut run_color: Option<ColorKind> = None;
            let mut run_len = 0u32;
            for col in 0..FIELD_WIDTH {
                let c = match board.cell(row, col) {
                    Cell::Color(k) => Some(k),
                    _ => None,
                };
                if c.is_some() && c == run_color {
                    run_len += 1;
                } else {
                    run_color = c;
                    run_len = if c.is_some() { 1 } else { 0 };
                }
                assert!(
                    run_len <= 4,
                    "row={row} col={col}で横方向の同色ランが4を超えている: {run_len}"
                );
            }
        }

        for col in 0..FIELD_WIDTH {
            let mut run_color: Option<ColorKind> = None;
            let mut run_len = 0u32;
            for row in 2..500 {
                let c = match board.cell(row, col) {
                    Cell::Color(k) => Some(k),
                    _ => None,
                };
                if c.is_some() && c == run_color {
                    run_len += 1;
                } else {
                    run_color = c;
                    run_len = if c.is_some() { 1 } else { 0 };
                }
                assert!(
                    run_len <= 3,
                    "col={col} row={row}で縦方向の同色ランが3を超えている: {run_len}"
                );
            }
        }
    }

    #[test]
    fn reroll_overlays_from_row_keeps_same_color_connected_groups_reasonably_small() {
        let mut rng = ChaCha8Rng::seed_from_u64(6);
        // #118の実測検証: ラン上限移植前は実プレイで86セルの巨大な同色塊(#114、
        // frame636)が観測されていた。横4・縦3のラン上限を課すことで、最も結合が
        // 強くなる条件(浅い深度・全配分率100%)でも連結グループが現実的な大きさに
        // 収まることを統計的に確認する(実測: 移植後は概ね10〜13セル程度)。
        let mut board = empty_board(5000);
        for row in 0..5000 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }
        board.reroll_overlays_from_row(
            &mut rng,
            0,
            100,
            100,
            100,
            100,
            0,
            0,
            0,
            4,
            100,
            &GravityState::new(),
        );

        let mut visited: HashSet<(usize, usize)> = HashSet::new();
        let mut max_size = 0usize;
        for row in 0..5000 {
            for col in 0..FIELD_WIDTH {
                if visited.contains(&(row, col)) {
                    continue;
                }
                if let Cell::Color(color) = board.cell(row, col) {
                    let group = connected_same_color(&board, (row, col), color);
                    visited.extend(group.iter().copied());
                    max_size = max_size.max(group.len());
                } else {
                    visited.insert((row, col));
                }
            }
        }
        assert!(
            max_size < 30,
            "同色連結グループが不自然に巨大(#114で観測した86セルに近い規模)になっている: {max_size}"
        );
    }

    #[test]
    fn reroll_overlays_from_row_rock_clustering_strengthens_with_depth() {
        let mut rng = ChaCha8Rng::seed_from_u64(5);
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
            if groups == 0 {
                0.0
            } else {
                total as f64 / groups as f64
            }
        }

        let mut board = empty_board(1000);
        for row in 0..1000 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }
        // 岩の出現率を上限(300%)にして、隣接ボーナスの効果を観測しやすくする。
        board.reroll_overlays_from_row(
            &mut rng,
            0,
            300,
            0,
            0,
            0,
            0,
            0,
            0,
            4,
            100,
            &GravityState::new(),
        );

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
        // 1回きりの試行では閾値ぎりぎりでたまたま通ってしまう(またはたまたま落ちる)
        // ことがあるため、シードを変えた複数回の平均で判定し、統計的なふらつきに
        // 左右されない検証にする。
        fn rock_fraction_in_deepest_band(seed: u64) -> f64 {
            let mut rng = ChaCha8Rng::seed_from_u64(seed);
            let mut board = empty_board(1000);
            for row in 0..1000 {
                for col in 0..FIELD_WIDTH {
                    board.rows[row][col] = Cell::Color(ColorKind::Red);
                }
            }
            board.reroll_overlays_from_row(
                &mut rng,
                0,
                100,
                100,
                0,
                100,
                0,
                0,
                0,
                4,
                100,
                &GravityState::new(),
            );

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
        let avg_fraction: f64 = (0..TRIALS)
            .map(|trial| rock_fraction_in_deepest_band(trial as u64))
            .sum::<f64>()
            / TRIALS as f64;
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

        let rock_count = row
            .iter()
            .filter(|c| matches!(c, Cell::Rock { .. }))
            .count();
        assert_eq!(
            rock_count,
            FIELD_WIDTH - 1,
            "少なくとも1マスは岩ブロック以外に差し替わるはず"
        );
        assert!(
            row.iter().any(|c| matches!(c, Cell::Color(_))),
            "差し替え先は色ブロックのはず"
        );
    }

    #[test]
    fn ensure_row_is_not_fully_blocked_by_rock_leaves_a_non_full_row_untouched() {
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let mut row = [Cell::Rock { hits: 0 }; FIELD_WIDTH];
        row[3] = Cell::Empty; // 既に掘削済みの穴が1つあれば「完全に塞がった壁」ではない

        ensure_row_is_not_fully_blocked_by_rock(&mut row, &mut rng, 4);

        let rock_count = row
            .iter()
            .filter(|c| matches!(c, Cell::Rock { .. }))
            .count();
        assert_eq!(
            rock_count,
            FIELD_WIDTH - 1,
            "既に穴がある行はそのまま変更されないはず"
        );
    }

    #[test]
    fn reroll_overlays_from_row_never_produces_a_row_fully_blocked_by_rock() {
        let mut rng = ChaCha8Rng::seed_from_u64(3);
        // 岩の出現率を上限(300%)・最大深度(塊化ボーナス最大)にしても、横一列が
        // 岩ブロックだけで完全に埋まることは無いことを確認する。
        let mut board = empty_board(1000);
        for row in 0..1000 {
            for col in 0..FIELD_WIDTH {
                board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }
        board.reroll_overlays_from_row(
            &mut rng,
            0,
            300,
            0,
            0,
            0,
            0,
            0,
            0,
            4,
            100,
            &GravityState::new(),
        );

        for row in 800..1000 {
            let all_rock =
                (0..FIELD_WIDTH).all(|col| matches!(board.cell(row, col), Cell::Rock { .. }));
            assert!(
                !all_rock,
                "row={row}が岩ブロックだけで完全に埋まっているはず無い"
            );
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
        let diamond_groups: Vec<&Vec<(usize, usize)>> = groups
            .iter()
            .filter(|g| g.iter().any(|&(r, c)| board.cell(r, c) == Cell::Diamond))
            .collect();

        assert_eq!(
            diamond_groups.len(),
            3,
            "隣接していてもダイヤブロックはそれぞれ単独の塊のはず"
        );
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
                assert!(
                    run_len <= 4,
                    "seed={seed}: 横方向の同色連続が4を超えた row={row} col={col}"
                );
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
                assert!(
                    run_len <= 3,
                    "seed={seed}: 縦方向の同色連続が3を超えた row={row} col={col}"
                );
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
        let left3 = [
            Some(ColorKind::Red),
            Some(ColorKind::Red),
            Some(ColorKind::Red),
        ];
        let resolved =
            resolve_run_limits(ColorKind::Red, left3, [None, None], ColorKind::ALL.len());
        assert_ne!(resolved, ColorKind::Red);
    }

    #[test]
    fn resolve_run_limits_avoids_fourth_vertical_same_color() {
        let top2 = [Some(ColorKind::Blue), Some(ColorKind::Blue)];
        let resolved = resolve_run_limits(
            ColorKind::Blue,
            [None, None, None],
            top2,
            ColorKind::ALL.len(),
        );
        assert_ne!(resolved, ColorKind::Blue);
    }

    #[test]
    fn resolve_run_limits_keeps_candidate_when_no_limit_hit() {
        let left3 = [Some(ColorKind::Red), None, None];
        let resolved =
            resolve_run_limits(ColorKind::Red, left3, [None, None], ColorKind::ALL.len());
        assert_eq!(resolved, ColorKind::Red);
    }

    #[test]
    fn resolve_run_limits_returns_candidate_unchanged_when_color_count_is_one() {
        // 色数設定(color_count)が1の場合、常に同色になるのは仕様通りであり、
        // ランを制限できる代替色が存在しないため候補をそのまま返すことを確認する
        // (TERM独自拡張。#118: reroll_overlays_from_rowへのラン上限移植)。
        let left3 = [
            Some(ColorKind::Red),
            Some(ColorKind::Red),
            Some(ColorKind::Red),
        ];
        let resolved = resolve_run_limits(ColorKind::Red, left3, [None, None], 1);
        assert_eq!(
            resolved,
            ColorKind::Red,
            "色数1では代替色が無いため候補のまま返るはず"
        );
    }
}

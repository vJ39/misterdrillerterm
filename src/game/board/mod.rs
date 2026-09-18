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
mod star;

pub use connect::*;
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

// ---------------------------------------------------------------------------
// 4章 落下・連結・消滅ロジック
// ---------------------------------------------------------------------------

/// 揺れ(spec.md 4.3)の状態。物理演算は純粋関数のまま保ち、Game(呼び出し側)が明示的に
/// 持ち回す。`unsupported_ticks`の値は「連続して未支持と判定されたティック数」で、
/// エントリが無い(=0扱い)セルは支持されている、または直近まで支持されていたセル。
#[derive(Debug, Clone, Default)]
pub struct GravityState {
    unsupported_ticks: HashMap<(usize, usize), u8>,
    /// 現在「震えている」塊に属する全セル(描画用)。`unsupported_ticks`は塊の代表座標だけを
    /// キーにするが、描画側は塊のどのセルについても揺れ中か知りたいため全メンバーを別途保持する。
    shaking_cells: HashSet<(usize, usize)>,
}

impl GravityState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 指定セルが「震えている」(未支持だが揺れ猶予中でまだ落下していない)かどうか
    /// (spec.md 4.3)。塊に属するセルなら代表座標以外でもtrueを返す。上向き掘削の
    /// 不安定判定(physics::drill_facing)と揺れ演出の描画(ui::render)の両方で使う。
    pub fn is_shaking(&self, pos: (usize, usize)) -> bool {
        self.shaking_cells.contains(&pos)
    }

    /// 揺れ猶予中(まだ落下していない)の状態だけをクリアする。盤面を重力ティック外から直接
    /// 書き換えた直後に呼び、次ティックで塊を作り直して支持判定からやり直させる(塊の境界が
    /// 変わると代表座標の意味も変わるため)。既に連続落下中の塊(値が`current_shake_ticks`超)
    /// は除外する(全クリアすると落下中の塊が揺れ直し、フリーズしたように見えてしまう)。
    pub fn reset_shake_progress(&mut self, current_shake_ticks: u8) {
        self.unsupported_ticks
            .retain(|_, ticks| *ticks as u32 > current_shake_ticks as u32);
        self.shaking_cells.clear();
    }
}

/// 盤面上の座標(行, 列)。
pub type Pos = (usize, usize);

/// 1回の重力ティックで実際に1マス落下したセル1つぶんの(移動後の位置, 移動前の位置)。
/// ブロック落下のピクセル単位補間描画に使う。
pub type BlockMove = (Pos, Pos);

/// `apply_gravity_tick`内で「揺れが明けて今ティックで落下する候補」を表す
/// (代表座標, 塊の全セル)。
type FallCandidate = (Pos, Vec<Pos>);

/// 1回の重力ティックの結果。
#[derive(Debug, Clone, Default)]
pub struct FallTickOutcome {
    /// このティックで実際に1マス落下した各セルの(移動後の位置, 移動前の位置)。
    /// 描画側(render.rs)が補間描画に使う。押し潰しで消滅したセル・揺れ中のセルは含まない。
    pub moved_cells: Vec<BlockMove>,
    /// 落下してきたセルがプレイヤー位置に重なった(=押し潰された)かどうか。
    pub crushed: bool,
    /// 落下・着地した結果、同色4連結により自動消滅した色ブロック数
    /// (spec.md 4.5。呼び出し側が「消滅数 × 30点」を加算する。7章)。
    pub auto_vanished_blocks: usize,
    /// 落下・着地した結果、4連結以上により自動消滅した岩ブロック数
    /// (spec.md 4.9。色ブロックと異なり得点対象外だが、破壊音等のイベント発火には使う)。
    pub auto_vanished_rock_blocks: usize,
    /// 落下してきた酸素カプセルがプレイヤー位置に重なり、取得された回数。押し潰しではなく
    /// 酸素回復扱いにする。呼び出し側が酸素回復・スコア加算を行う。
    pub oxygen_collected: usize,
    /// 落下してきたアイテムブロックがプレイヤー位置に重なり、取得された効果の一覧。
    /// AIRと同様、押し潰されず取得扱いになる。呼び出し側(Game)が効果を実際に発動する
    pub items_collected: Vec<ItemEffect>,
    /// このティックで自動消滅(4連結以上の色/岩ブロック)により消えたセルの座標と消える
    /// 直前のセル内容。描画側がフラッシュ演出に、デバッグログがセル内容の記録に使う。
    pub vanished_cells: Vec<(Pos, Cell)>,
}

/// あるセル`pos`が「支持されている」か(spec.md 4.2)。最深行、または直下が非Emptyかつ
/// プレイヤー不在なら真(プレイヤー位置は常に空洞扱いで支えにならない)。単独セル専用で、
/// 連結グループの判定には`is_group_supported`を使うこと(仲間セルを支えと誤認するため)。
///
/// `solid`はCellグリッド外のオーバーレイ(設置済みボム)が占めるマスの一覧。グリッド上は
/// Emptyだが物理的には塞がっているため、非Emptyセルと同じ「支え」として扱う。
pub(crate) fn is_supported(
    board: &Board,
    pos: (usize, usize),
    player_pos: (usize, usize),
    solid: &[Pos],
) -> bool {
    let (row, col) = pos;
    let depth_rows = board.depth_rows();
    if row + 1 >= depth_rows {
        return true;
    }
    let below = (row + 1, col);
    if solid.contains(&below) {
        return true;
    }
    board.cell(below.0, below.1) != Cell::Empty && below != player_pos
}

/// 盤面全体を「重力の単位」ごとに分割する(spec.md 4.1・4.7)。色は同色4方向連結、岩は
/// hits問わず連結で1つの塊にまとめ、支持判定・移動を塊単位で行うことで「ちぎれて落ちる」
/// ことを防ぐ。酸素・ダイヤ・スター・アイテムは連結対象外で、常にサイズ1の塊として扱う。
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

/// `group`(1つの塊)が全体として支持されているか。どれか1つのセルでも「直下がグループ外の
/// 非Emptyセル」または「最深行」なら塊全体が支持されているとみなす。直下が仲間セルの場合、
/// そのセル自身は支えにならない(仲間越しに本当の支えを探す)。
///
/// `solid`(設置済みボム等のCellグリッド外オーバーレイ)は`is_supported`と同じく支えとして
/// 扱う。
pub(crate) fn is_group_supported(
    board: &Board,
    group: &[(usize, usize)],
    player_pos: (usize, usize),
    solid: &[Pos],
) -> bool {
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
        if solid.contains(&below) {
            return true;
        }
        board.cell(below.0, below.1) != Cell::Empty && below != player_pos
    })
}

/// `group`が「真に安定した支え」を持つか(`is_group_supported`の連鎖判定版)。支えの根拠
/// セルが属する塊が既に未支持と判定済みなら、その支えは一緒に落ちる不安定な支えとして
/// 数えない。呼び出し側が収束するまで繰り返し呼び、支えの連鎖を正しく伝播させる。
fn has_stable_support(
    board: &Board,
    group: &[(usize, usize)],
    cell_to_group: &HashMap<(usize, usize), usize>,
    supported: &[bool],
    player_pos: (usize, usize),
    solid: &[Pos],
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
        if solid.contains(&below) {
            // 設置済みボムは重力の対象外で、このティックで立ち退くことがないため
            // 常に安定した支えとして扱う。
            return true;
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

/// 論理ティック1回ぶんの重力落下処理(spec.md 4章・5章)。盤面を塊に分割し、ティック開始時
/// のスナップショット基準で全塊同時に支持判定→未支持の塊は`shake_ticks`ぶん揺れてから
/// 1マスずつ落下する(4.2〜4.4)。移動先がプレイヤーなら押し潰し確定(酸素・アイテムは例外で
/// 取得扱い、5章)。着地した色/岩ブロックの塊は4個以上なら自動消滅する(4.5・4.9)。
///
/// `solid`はCellグリッド外のオーバーレイ(設置済みボム)が占めるマスの一覧。盤面上はEmpty
/// だが物理的には塞がっているため、支持判定・着地先判定・自動消滅判定のすべてで非Emptyセル
/// と同じ「支え/障害物」として扱う。これを渡さないと、ボムの真上で支えを失ったブロックが
/// ボムのマスへ落下して重なって見える。
pub fn apply_gravity_tick(
    board: &mut Board,
    player_pos: (usize, usize),
    solid: &[Pos],
    gravity: &mut GravityState,
    shake_ticks: u8,
) -> FallTickOutcome {
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
    let mut supported: Vec<bool> = groups
        .iter()
        .map(|g| is_group_supported(&snapshot, g, player_pos, solid))
        .collect();

    // 連鎖的な再判定。支えの根拠セルが属する塊自体がこのティックで未支持なら、支えられて
    // いる側も連動して未支持にする(塊がちぎれて分離するのを防ぐ)。1段の連鎖では済まない
    // 場合があるため(支えの支えの支え…)、変化が無くなるまで繰り返す。
    loop {
        let mut changed = false;
        for i in 0..groups.len() {
            if !supported[i] {
                continue;
            }
            if !has_stable_support(
                &snapshot,
                &groups[i],
                &cell_to_group,
                &supported,
                player_pos,
                solid,
            ) {
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

    // 揺れが明けて「今ティックで落下する」候補の塊。この時点ではまだ確定ではない
    // (直後の収束ループで、着地先がまだ物理的に塞がっている候補を除外する)。
    let mut candidates: Vec<FallCandidate> = Vec::new();

    for (i, group) in groups.into_iter().enumerate() {
        if supported[i] {
            continue; // 支持されている = 揺れ状態も解除(next_unsupported_ticksに載せない)
        }

        // 塊の代表座標(先頭要素。`collect_fall_groups`の探索順序上、同じ塊なら常に
        // 「最小の(row, col)」で安定する)で揺れティック数を管理する。
        let representative = group[0];
        let ticks_unsupported = gravity
            .unsupported_ticks
            .get(&representative)
            .copied()
            .unwrap_or(0)
            + 1;
        if ticks_unsupported as u32 > shake_ticks as u32 {
            // 揺れが明けた(またはshake_ticks=0で即座に) -> このティックで1マス落下する
            // 「候補」にする。確定は後段の収束ループの後。
            candidates.push((representative, group));
        } else {
            // まだ揺れている最中 -> 移動しない。描画用に塊の全セルを揺れ中として記録する。
            next_shaking_cells.extend(group.iter().copied());
            next_unsupported_ticks.insert(representative, ticks_unsupported);
        }
    }

    // 着地先(snapshot時点)が非Empty(または設置済みボムが占有)で「今ティックで立ち退く
    // 別候補」でもない候補は落下を見送り揺れ中に戻す。揺れ猶予の消化は塊ごとに独立して進む
    // ため、まだ揺れ猶予中で物理的に残っている支えを、先に揺れ明けした側が無警告で上書き
    // してしまうのを防ぐ。
    // 1候補の見送りで別候補の立ち退き先が失われうるため、変化が無くなるまで繰り返す。
    loop {
        let vacating: HashSet<(usize, usize)> = candidates
            .iter()
            .flat_map(|(_, group)| group.iter().copied())
            .collect();
        let blocked_at = candidates.iter().position(|(_, group)| {
            group.iter().any(|&(r, c)| {
                let to = (r + 1, c);
                to != player_pos
                    && (snapshot.cell(to.0, to.1) != Cell::Empty || solid.contains(&to))
                    && !vacating.contains(&to)
            })
        });
        let Some(idx) = blocked_at else { break };
        let (representative, group) = candidates.remove(idx);
        next_shaking_cells.extend(group.iter().copied());
        // 揺れ猶予をゼロからやり直させず、次ティックで即座に再挑戦できるよう
        // shake_ticksちょうど(次ティック+1でshake_ticksを超える)を積んでおく。
        next_unsupported_ticks.insert(representative, shake_ticks);
    }

    let mut falling_groups: Vec<Vec<(usize, usize)>> = Vec::new();
    for (representative, group) in candidates {
        // 落下開始後は揺れ直さず落下し続けるよう、移動先の代表座標(行+1)へ揺れ明け済みの
        // 印を引き継ぐ。値を`shake_ticks+1`に固定するのはu8オーバーフロー回避のため。
        let unlocked_representative = (representative.0 + 1, representative.1);
        next_unsupported_ticks.insert(unlocked_representative, shake_ticks.saturating_add(1));
        falling_groups.push(group);
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

    // 先に全ての塊の旧位置をまとめてEmptyにしてから新位置へ書き込む。塊ごとに
    // 「クリア→書き込み」を順に行うと、縦に連なって同時落下する塊で後続のクリアが先に
    // 書き込んだ別の塊を消してしまう(spec.md 4章手順5を全塊を跨いで徹底する)。
    for group in &falling_groups {
        for &(r, c) in group {
            board.set(r, c, Cell::Empty);
        }
    }

    for group in &falling_groups {
        // 各セルの内容は`snapshot`からセルごとに個別取得する。代表セルの内容を全セルへ
        // 使い回すと、岩のhitsのようにセルごとに異なる付随データが上書きされてしまう。
        let mut crushed_in_group = false;
        for &(r, c) in group {
            let cell = snapshot.cell(r, c);
            let to = (r + 1, c);
            if to == player_pos {
                if cell == Cell::Oxygen {
                    // 酸素カプセル(AIR)だけは例外で、押し潰し判定にせず取得(酸素回復)扱いに
                    // してその場で消滅する。
                    outcome.oxygen_collected += 1;
                } else if let Cell::Item(effect) = cell {
                    // アイテムブロックもAIRと同様、押し潰しにせず取得扱いにする。
                    outcome.items_collected.push(effect);
                } else {
                    // 押し潰した側のセルは即座に消滅させず、潰した様子が見えるようプレイヤー
                    // 位置に残す(得点なし、spec.md 5章)。復活時にGame::tick_ascendingで消去される。
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
            // ここでbreakすると、旧位置を既にEmptyにした他の無関係な塊の新位置書き込みが
            // スキップされ盤面から消滅してしまう。他の塊の移動は最後まで反映する。
            continue;
        }
    }

    // 押し潰しが確定していても自動消滅判定はスキップしない。同じtickで着地した他の
    // 無関係な塊の4連結自動消滅は独立した事象で、早期returnすると消え残ってしまう。

    // 着地した色/岩ブロックの連結・自動消滅を判定する(spec.md 4.5・4.9)。岩も4連結以上で
    // 自動消滅するが得点なし。掘削(hit_rock)の「消えるのは1個だけ」ルールとは独立し、
    // こちらは落下・着地して4個以上連結した場合にのみ働く。
    for group in &falling_groups {
        let moved_group: Vec<(usize, usize)> = group.iter().map(|&(r, c)| (r + 1, c)).collect();
        let Some(&to) = moved_group.first() else {
            continue;
        };
        if board.cell(to.0, to.1) == Cell::Empty {
            continue; // 別の塊による押し潰し等で既にこの位置が変化している
        }
        match board.cell(to.0, to.1) {
            Cell::Color(color) => {
                // プレイヤー位置は連結グループの計算から除外する。押し潰しで表示用に残る
                // 特殊セルが連結に含まれると、無関係な静的ブロックまで巻き込んで自動消滅
                // させてしまう。通常時のプレイヤー位置は常にEmptyのためこの除外は無害。
                let vanish_group: Vec<(usize, usize)> = connected_same_color(board, to, color)
                    .into_iter()
                    .filter(|&pos| pos != player_pos)
                    .collect();
                // 支持判定は落下塊自身だけでなく、接触した既存の塊も含む現在の連結グループ
                // 全体で行う。真横への接触だと落下塊単独では未支持と誤判定され、次tickで
                // 合体・支持済み扱いになり、以後二度と自動消滅チェックされず永久に残るため。
                // 同じ理由で`solid`(設置済みボム)も必ず渡す。ボムの上に着地した塊を未支持と
                // 誤判定すると、この自動消滅チェックがスキップされたまま永久に残ってしまう。
                if !is_group_supported(board, &vanish_group, player_pos, solid) {
                    continue; // まだ落下中(次ティック以降に改めて着地判定する)
                }
                if vanish_group.len() >= 4 {
                    for &(vr, vc) in &vanish_group {
                        let kind = board.cell(vr, vc);
                        board.set(vr, vc, Cell::Empty);
                        outcome.vanished_cells.push(((vr, vc), kind));
                    }
                    outcome.auto_vanished_blocks += vanish_group.len();
                }
            }
            Cell::Rock { .. } => {
                // 色ブロックと同じ理由でプレイヤー位置を除外する。
                let vanish_group: Vec<(usize, usize)> = connected_rock_group(board, to)
                    .into_iter()
                    .filter(|&pos| pos != player_pos)
                    .collect();
                if !is_group_supported(board, &vanish_group, player_pos, solid) {
                    continue;
                }
                if vanish_group.len() >= 4 {
                    for &(vr, vc) in &vanish_group {
                        let kind = board.cell(vr, vc);
                        board.set(vr, vc, Cell::Empty);
                        outcome.vanished_cells.push(((vr, vc), kind));
                    }
                    outcome.auto_vanished_rock_blocks += vanish_group.len();
                }
            }
            _ => {}
        }
    }

    outcome
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
    use crate::constants::SHAKE_TICKS;

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

    // --- 重力: 揺れてから落下する(4.3) ---

    #[test]
    fn unsupported_cell_shakes_for_shake_ticks_before_falling() {
        let mut board = empty_board(3);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        let mut gravity = GravityState::new();

        // SHAKE_TICKS ぶんは揺れるだけで移動しない。この間`is_shaking`はtrueを返し、
        // 描画側がシェイク演出に使えるデータとして残る(spec.md 4.3)。
        for _ in 0..SHAKE_TICKS {
            let outcome = apply_gravity_tick(&mut board, (99, 99), &[], &mut gravity, SHAKE_TICKS);
            assert_eq!(outcome.moved_cells.len(), 0);
            assert!(gravity.is_shaking((0, 0)));
        }
        assert_eq!(board.cell(0, 0), Cell::Color(ColorKind::Red));

        // SHAKE_TICKS+1ティック目で実際に1マス落下する
        let outcome = apply_gravity_tick(&mut board, (99, 99), &[], &mut gravity, SHAKE_TICKS);
        assert_eq!(outcome.moved_cells.len(), 1);
        assert_eq!(board.cell(0, 0), Cell::Empty);
        assert_eq!(board.cell(1, 0), Cell::Color(ColorKind::Red));
        assert!(
            !gravity.is_shaking((1, 0)),
            "着地して支持されればもう揺れていない"
        );
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
            apply_gravity_tick(&mut board, (99, 99), &[], &mut gravity, SHAKE_TICKS);
        }

        // SHAKE_TICKS+1回目の呼び出しで揺れが明けて最初の1マスが落ちる。
        let outcome = apply_gravity_tick(&mut board, (99, 99), &[], &mut gravity, SHAKE_TICKS);
        assert_eq!(outcome.moved_cells.len(), 1);
        assert_eq!(board.cell(1, 0), Cell::Color(ColorKind::Red));
        assert!(!gravity.is_shaking((1, 0)));

        // 以降、最深行に着地するまで毎ティック連続で1マスずつ落下し続け、
        // 揺れ状態(is_shaking)には一切戻らない。
        for expected_row in 2..6 {
            let outcome = apply_gravity_tick(&mut board, (99, 99), &[], &mut gravity, SHAKE_TICKS);
            assert_eq!(
                outcome.moved_cells.len(),
                1,
                "row={expected_row}到達時点で連続落下しているはず"
            );
            assert_eq!(board.cell(expected_row, 0), Cell::Color(ColorKind::Red));
            assert!(
                !gravity.is_shaking((expected_row, 0)),
                "落下中は揺れ状態に戻らないはず"
            );
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
            apply_gravity_tick(&mut board, (99, 99), &[], &mut gravity, SHAKE_TICKS);
        }
        assert_eq!(
            board.cell(1, 0),
            Cell::Color(ColorKind::Red),
            "既に1マス落下しているはず"
        );

        // ショートカットC相当の書き換え直後に呼ばれる想定の関数。
        gravity.reset_shake_progress(SHAKE_TICKS);

        let outcome = apply_gravity_tick(&mut board, (99, 99), &[], &mut gravity, SHAKE_TICKS);
        assert_eq!(
            outcome.moved_cells.len(),
            1,
            "reset_shake_progress後も揺れ直さず連続で落下し続けるはず"
        );
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

        apply_gravity_tick(&mut board, (99, 99), &[], &mut gravity, SHAKE_TICKS);
        assert!(gravity.is_shaking((0, 0)), "揺れ猶予中のはず");

        gravity.reset_shake_progress(SHAKE_TICKS);

        assert!(
            !gravity.is_shaking((0, 0)),
            "揺れ猶予中の状態はクリアされるはず"
        );
    }

    #[test]
    fn is_shaking_is_false_for_a_supported_cell() {
        let mut board = empty_board(2);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        board.rows[1][0] = Cell::Color(ColorKind::Blue); // 支えあり
        let mut gravity = GravityState::new();

        apply_gravity_tick(&mut board, (99, 99), &[], &mut gravity, SHAKE_TICKS);

        assert!(!gravity.is_shaking((0, 0)));
    }

    /// テスト用ヘルパー: `SHAKE_TICKS`ぶん揺れティックを消化してから、実際に落下する
    /// ティックを1回実行する(4.3のテストで繰り返し使う定型パターン)。
    fn shake_out_then_tick(
        board: &mut Board,
        player_pos: (usize, usize),
        gravity: &mut GravityState,
    ) -> FallTickOutcome {
        for _ in 0..SHAKE_TICKS {
            apply_gravity_tick(board, player_pos, &[], gravity, SHAKE_TICKS);
        }
        apply_gravity_tick(board, player_pos, &[], gravity, SHAKE_TICKS)
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
            apply_gravity_tick(&mut board, player_pos, &[], &mut gravity, SHAKE_TICKS);
        }

        assert!(
            matches!(board.cell(4, 0), Cell::Item(ItemEffect::ClearAbove)),
            "アイテムは最深行まで落ちて残るはず"
        );
        assert!(
            matches!(board.cell(3, 0), Cell::Diamond),
            "ダイヤはアイテムのすぐ上に着地するはず"
        );
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

        assert_eq!(
            outcome.moved_cells.len(),
            2,
            "上段・下段とも同じティックで一緒に落下するはず"
        );
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

        assert_eq!(
            outcome.moved_cells.len(),
            1,
            "孤立していても支えが無ければ落ちるはず"
        );
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
        assert_eq!(
            board.cell(3, 0),
            Cell::Color(ColorKind::Red),
            "3つ目もちぎれずに一緒に1マス落下しているはず"
        );
    }

    #[test]
    fn horizontal_group_of_three_supported_only_under_rightmost_cell_falls_together_without_tearing()
     {
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

        assert_eq!(
            outcome.moved_cells.len(),
            3,
            "3つとも一緒に1マス落下するはず(ちぎれない)"
        );
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
        assert_eq!(
            board.cell(1, 0),
            Cell::Color(ColorKind::Red),
            "潰したブロックはその場に残って見えるはず"
        );
        assert_eq!(
            outcome.moved_cells,
            vec![((1, 0), (0, 0))],
            "落下アニメーション用に着地移動も記録されるはず"
        );
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

        assert!(
            !outcome.crushed,
            "アイテムブロックは押し潰し判定にならないはず"
        );
        assert_eq!(board.cell(1, 0), Cell::Empty);
        assert_eq!(outcome.items_collected, vec![ItemEffect::ClearAbove]);
    }

    #[test]
    fn crush_from_one_falling_group_does_not_erase_another_unrelated_falling_group_in_the_same_tick()
     {
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
    fn crushing_the_player_does_not_auto_vanish_static_same_color_neighbors_via_the_crush_cell() {
        // ユーザー指摘: 「キャラと同じ位置に来た隣接ブロックがどうも消えるっぽい」(#170)。
        // 押し潰したブロックはプレイヤー位置にそのまま残る(表示用の特殊セル、
        // falling_block_onto_player_crushes_and_remains_visible_at_the_impact_point参照)が、
        // このセルを起点に通常の自動消滅判定(4連結以上)を回してしまうと、たまたま
        // 同色で隣接していただけの、落下とは無関係な既存の静的ブロックまで巻き込んで
        // 消えてしまっていた。押し潰した塊は自動消滅判定の対象から除外されるべき。
        let mut board = empty_board(3);
        board.rows[0][0] = Cell::Color(ColorKind::Red); // プレイヤーを押し潰す塊
        board.rows[1][1] = Cell::Color(ColorKind::Red); // 静的な隣接ブロック(消えてはいけない)
        board.rows[1][2] = Cell::Color(ColorKind::Red);
        board.rows[1][3] = Cell::Color(ColorKind::Red);
        board.rows[2][1] = Cell::Rock { hits: 0 }; // 隣接ブロックを支える床
        board.rows[2][2] = Cell::Rock { hits: 0 };
        board.rows[2][3] = Cell::Rock { hits: 0 };
        let player_pos = (1, 0);
        let mut gravity = GravityState::new();

        let outcome = shake_out_then_tick(&mut board, player_pos, &mut gravity);

        assert!(outcome.crushed, "プレイヤーは押し潰されるはず");
        assert_eq!(
            board.cell(1, 0),
            Cell::Color(ColorKind::Red),
            "押し潰したブロックはその場に残って見えるはず"
        );
        assert_eq!(
            board.cell(1, 1),
            Cell::Color(ColorKind::Red),
            "静的な隣接ブロックは押し潰しに巻き込まれて消えてはいけないはず"
        );
        assert_eq!(board.cell(1, 2), Cell::Color(ColorKind::Red));
        assert_eq!(board.cell(1, 3), Cell::Color(ColorKind::Red));
    }

    #[test]
    fn crushing_the_player_does_not_auto_vanish_via_a_separate_group_landing_adjacent_to_the_crush_cell()
     {
        // #170のフォローアップ: 押し潰した塊自身の着地判定だけでなく、同じtickに
        // 着地した別の(無関係な)塊がたまたま押し潰しセルへ隣接するケースでも、
        // プレイヤー位置を経由して静的な隣接ブロックまで巻き込んで自動消滅させて
        // はいけない(グループ単位の除外だけでは防げず、プレイヤー位置そのものを
        // 連結グループの計算から除外する必要があった)。
        let mut board = empty_board(3);
        board.rows[0][0] = Cell::Color(ColorKind::Red); // プレイヤーを押し潰す塊
        board.rows[1][1] = Cell::Color(ColorKind::Red); // 静的な隣接ブロック(消えてはいけない)
        board.rows[0][2] = Cell::Color(ColorKind::Red); // 別の(無関係な)落下中の塊
        board.rows[1][3] = Cell::Color(ColorKind::Red); // 静的な隣接ブロック(消えてはいけない)
        board.rows[2][1] = Cell::Rock { hits: 0 };
        board.rows[2][2] = Cell::Rock { hits: 0 };
        board.rows[2][3] = Cell::Rock { hits: 0 };
        let player_pos = (1, 0);
        let mut gravity = GravityState::new();

        let outcome = shake_out_then_tick(&mut board, player_pos, &mut gravity);

        assert!(outcome.crushed, "プレイヤーは押し潰されるはず");
        assert_eq!(
            board.cell(1, 0),
            Cell::Color(ColorKind::Red),
            "押し潰したブロックはその場に残って見えるはず"
        );
        assert_eq!(
            board.cell(1, 1),
            Cell::Color(ColorKind::Red),
            "静的な隣接ブロックは押し潰しに巻き込まれて消えてはいけないはず"
        );
        assert_eq!(
            board.cell(1, 2),
            Cell::Color(ColorKind::Red),
            "無関係に落下した塊自体も消えてはいけないはず"
        );
        assert_eq!(
            board.cell(1, 3),
            Cell::Color(ColorKind::Red),
            "静的な隣接ブロックは押し潰しに巻き込まれて消えてはいけないはず"
        );
    }

    #[test]
    fn a_crush_in_one_group_does_not_suppress_auto_vanish_for_another_group_landing_the_same_tick()
    {
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
        assert_eq!(
            connected_same_color(&board, (2, 0), ColorKind::Red).len(),
            2
        );

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity); // 落下→接触→連結→自動消滅

        assert_eq!(
            outcome.moved_cells.len(),
            2,
            "落下グループの2個が同時に1マス落ちる"
        );
        assert_eq!(
            outcome.auto_vanished_blocks, 4,
            "接触した結果、合計4個で自動消滅する"
        );
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
        assert_eq!(
            connected_same_color(&board, (3, 0), ColorKind::Red).len(),
            3
        );
        assert_eq!(
            connected_same_color(&board, (0, 3), ColorKind::Red).len(),
            3
        );

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity);

        assert_eq!(
            outcome.auto_vanished_blocks, 6,
            "接触後は合計6個で自動消滅するはず"
        );
        for &(r, c) in &[(2, 3), (3, 3), (3, 2), (3, 1), (3, 0)] {
            assert_eq!(
                board.cell(r, c),
                Cell::Empty,
                "row={r},col={c}が消えていない"
            );
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
            apply_gravity_tick(&mut board, player_pos, &[], &mut gravity, SHAKE_TICKS);
        }

        for r in 0..6 {
            assert_eq!(
                board.cell(r, 0),
                Cell::Empty,
                "row={r} col=0: 4連結以上になったので自動消滅しているはず"
            );
        }
        assert_eq!(
            board.cell(3, 1),
            Cell::Empty,
            "落下してきた側も自動消滅しているはず"
        );
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
        assert_eq!(
            connected_same_color(&board, (3, 0), ColorKind::Red).len(),
            4
        );
        assert_eq!(
            connected_same_color(&board, (0, 1), ColorKind::Red).len(),
            1
        );

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity);

        assert_eq!(
            outcome.auto_vanished_blocks, 5,
            "T字4個+落下1個=5個で自動消滅するはず"
        );
        for &(r, c) in &[(1, 1), (3, 0), (3, 1), (3, 2), (2, 1)] {
            assert_eq!(
                board.cell(r, c),
                Cell::Empty,
                "row={r},col={c}が消えていない"
            );
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
        assert_eq!(
            outcome.auto_vanished_blocks, 0,
            "岩ブロックの自動消滅はスコア対象外"
        );
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
        assert!(
            matches!(board.cell(1, 0), Cell::Rock { hits: 3 }),
            "落下してもhitsは保持される"
        );
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
            let outcome = apply_gravity_tick(&mut board, (99, 99), &[], &mut gravity, SHAKE_TICKS);
            assert_eq!(outcome.moved_cells.len(), 0);
            assert!(gravity.is_shaking((0, 0)));
        }
        assert!(
            matches!(board.cell(0, 0), Cell::Rock { hits: 2 }),
            "揺れている間はまだ落下しない"
        );

        let outcome = apply_gravity_tick(&mut board, (99, 99), &[], &mut gravity, SHAKE_TICKS);
        assert_eq!(outcome.moved_cells.len(), 1);
        assert_eq!(board.cell(0, 0), Cell::Empty);
        assert!(
            matches!(board.cell(1, 0), Cell::Rock { hits: 2 }),
            "落下してもhitsは保持される"
        );
        assert!(
            !gravity.is_shaking((1, 0)),
            "着地して支持されればもう揺れていない"
        );
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

        assert_eq!(
            connected_rock_group(&board, (2, 0)).len(),
            2,
            "接触前は支持グループのみ2個"
        );

        let outcome = shake_out_then_tick(&mut board, (99, 99), &mut gravity);

        assert_eq!(outcome.auto_vanished_rock_blocks, 4);
        assert_eq!(
            outcome.auto_vanished_blocks, 0,
            "岩ブロックの自動消滅はスコア対象外"
        );
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

        let outcome = apply_gravity_tick(&mut board, (99, 99), &[], &mut gravity, SHAKE_TICKS);

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

        apply_gravity_tick(&mut board, (99, 99), &[], &mut gravity, SHAKE_TICKS); // 揺れ
        let outcome = apply_gravity_tick(&mut board, (99, 99), &[], &mut gravity, SHAKE_TICKS); // 支持判定

        assert_eq!(
            outcome.moved_cells.len(),
            0,
            "酸素カプセルは非Emptyなので上のRedは支持され落下しない"
        );
        assert_eq!(board.cell(0, 0), Cell::Color(ColorKind::Red));
        assert_eq!(
            board.cell(1, 0),
            Cell::Oxygen,
            "酸素カプセルは上書き・消滅していない"
        );
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
            apply_gravity_tick(&mut board, player_pos, &[], &mut gravity, SHAKE_TICKS);
        }

        for group in collect_fall_groups(&board) {
            assert!(
                is_group_supported(&board, &group, player_pos, &[]),
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
                apply_gravity_tick(&mut board, player_pos, &[], &mut gravity, SHAKE_TICKS);
            }

            for group in collect_fall_groups(&board) {
                assert!(
                    is_group_supported(&board, &group, player_pos, &[]),
                    "seed={seed}: 十分な時間が経っても未支持のまま残っている塊がある: {group:?}"
                );
            }
        }
    }

    #[test]
    fn no_group_remains_unsupported_forever_after_reroll_at_realistic_depth() {
        let mut rng = ChaCha8Rng::seed_from_u64(2);
        // ユーザー報告(スクリーンショット、深度418m Lv.14付近): 支えを失っているはずの
        // ブロックが崩れず浮いたままになる箇所がある。既存のno_group_remains_unsupported_
        // forever_on_random_boardsはBoard::generate直後(=reroll前)の浅い盤面(60行)しか
        // 検証していなかった。実際のプレイでは開始直後にreroll_overlays_from_rowが適用され、
        // かつ深度が進むほど岩ブロックの塊化ボーナスが強く効く(ROCK_CLUSTER_DEPTH_MAX_BONUS)
        // ため、そのギャップを埋めて深い深度(400〜600m帯)でも同じ不変条件を確認する。
        for seed in 0..2u64 {
            let mut board = Board::generate(seed, 70, FIELD_WIDTH);
            let gravity_for_reroll = GravityState::new();
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
                &gravity_for_reroll,
            );

            let mut gravity = GravityState::new();
            let player_pos = (usize::MAX, usize::MAX);

            for _ in 0..(SHAKE_TICKS as usize + 1) * 70 {
                apply_gravity_tick(&mut board, player_pos, &[], &mut gravity, SHAKE_TICKS);
            }

            for group in collect_fall_groups(&board) {
                assert!(
                    is_group_supported(&board, &group, player_pos, &[]),
                    "seed={seed}: reroll後・深い深度で十分な時間が経っても未支持のまま残っている塊がある: {group:?}"
                );
            }
        }
    }

    #[test]
    fn a_falling_rock_stops_on_top_of_a_stationary_diamond_directly_in_its_path() {
        // #85再調査(2026/07/27): 実プレイのフレーム単位キャプチャで、静止したダイヤ
        // ブロックの真上を、別に落下中の岩ブロックが1マスずつ近づいてくる状況を確認した。
        // 本来、岩がダイヤの1マス上に来た時点で「直下(ダイヤ)が非Empty」により支持され、
        // そこで止まってダイヤの上に乗るはずである。ところが実際のログでは、岩が
        // ダイヤの座標へ何の抵抗もなく通過していた(ダイヤ側にはmove/vanishどちらの
        // 記録も一切残らないまま消えていた)。この再現テストは、その状況(静止した
        // 単独セルの真上を、別カラムからではなく同一カラムで落下中の塊が1マスずつ
        // 接近する)を直接構築し、岩が正しくダイヤの上で止まる(=ダイヤを通過しない)
        // ことを確認する。
        let mut board = empty_board(10);
        board.rows[8][3] = Cell::Diamond;
        board.rows[9][3] = Cell::Rock { hits: 0 }; // ダイヤ自身の支え(最深行そのものでもよい)
        board.rows[0][3] = Cell::Rock { hits: 0 }; // 同じ列を落ちてくる、無関係な岩
        let mut gravity = GravityState::new();
        let player_pos = (usize::MAX, usize::MAX);

        for tick in 0..((SHAKE_TICKS as usize + 1) * 10) {
            let outcome =
                apply_gravity_tick(&mut board, player_pos, &[], &mut gravity, SHAKE_TICKS);
            if board.cell(8, 3) != Cell::Diamond {
                let logged = outcome
                    .vanished_cells
                    .iter()
                    .any(|&(pos, kind)| pos == (8, 3) && kind == Cell::Diamond);
                assert!(
                    logged,
                    "tick={tick}: (8,3)のダイヤがログに残らず盤面から消えた(自動消滅対象ではないはず)"
                );
            }
        }

        assert_eq!(
            board.cell(8, 3),
            Cell::Diamond,
            "ダイヤは(押し潰し等の正規経路がない限り)最後まで(8,3)に残っているはず"
        );
        assert_eq!(
            board.cell(7, 3),
            Cell::Rock { hits: 0 },
            "落ちてきた岩はダイヤの1マス上(7,3)で止まって乗っているはず(ダイヤを素通りしていないか)"
        );
    }

    #[test]
    fn a_stationary_diamond_survives_a_wide_simultaneous_multi_column_cascade_passing_beside_it() {
        // #85再調査: 単純な「岩1個がダイヤ1個の真上に近づく」だけの再現テスト
        // (a_falling_rock_stops_on_top_of_a_stationary_diamond_directly_in_its_path)では
        // 再現しなかった。実プレイのログでは、静止したダイヤの列(col5)だけでなく
        // 隣接列(col6・col7)でも同時に別の塊(ダイヤ・色ブロック混在)が同じペースで
        // 落下しており、しかもダイヤが消えたのと同じtickで隣列の塊が真横(同じ行)へ
        // 着地していた。この、より実プレイに近い「複数列が同時に連鎖落下し、静止した
        // 単独セルの真横にも別の塊が同じ行へ着地する」状況を再現し、ダイヤが
        // 通過されず・ログにも残らず消えないことを確認する。
        let mut board = empty_board(20);
        // col3: 静止しているはずのダイヤ(row19=最深行、常に支持される)。
        board.rows[19][3] = Cell::Diamond;
        // col2・col3・col4(ダイヤの列の両隣に直接隣接)に、遠く上から落ちてくる
        // 単独セルを1個ずつ置く(実ログ同様、何tickもかけて1マスずつ近づく状況を
        // 模す)。col2・col4は色ブロック(岩と結合しないので隣接していても4連結には
        // ならない)、col3はダイヤと同じ列の岩(実ログのcol5と同じ関係)。
        board.rows[0][2] = Cell::Color(ColorKind::Blue);
        board.rows[0][3] = Cell::Rock { hits: 0 };
        board.rows[0][4] = Cell::Color(ColorKind::Green);

        let mut gravity = GravityState::new();
        let player_pos = (usize::MAX, usize::MAX);

        for tick in 0..((SHAKE_TICKS as usize + 1) * 20) {
            let outcome =
                apply_gravity_tick(&mut board, player_pos, &[], &mut gravity, SHAKE_TICKS);
            if board.cell(19, 3) != Cell::Diamond {
                let logged = outcome
                    .vanished_cells
                    .iter()
                    .any(|&(pos, kind)| pos == (19, 3) && kind == Cell::Diamond);
                assert!(
                    logged,
                    "tick={tick}: (19,3)のダイヤがログに残らず盤面から消えた(自動消滅対象ではないはず): moved={:?} vanished={:?}",
                    outcome.moved_cells, outcome.vanished_cells
                );
            }
        }

        assert_eq!(
            board.cell(19, 3),
            Cell::Diamond,
            "同一列を単独セルの岩が素通りするなどして、ダイヤは(19,3)に残っているはず"
        );
    }

    #[test]
    fn a_group_that_already_finished_shaking_does_not_overwrite_a_neighbor_still_within_its_own_shake_grace_period()
     {
        // #85根本原因の再現(2026/07/27): 実プレイのデバッグログを`board_snapshot`と
        // `block_events`の全件突合で解析した結果、静止していたダイヤ(col1,row33)が
        // 一度もmove/vanishログに現れないまま、その真上を落ちてきた岩の着地ログ
        // (`move Rock (33,1) <- (32,1)`)にちょうど上書きされる形で消えていたことが
        // 確定した。原因はapply_gravity_tick内の`has_stable_support`連鎖判定にある:
        // 支えとなる塊(ダイヤ)が論理的に支持を失った(=`supported[]`がfalseになった)
        // ことは、支えられる側(岩)へ即座に伝播するが、実際に「揺れ猶予(shake_ticks)
        // が明けて物理的に動き出す」タイミングは塊ごとに完全に独立したカウンタ
        // (`unsupported_ticks`、代表座標キー)で管理されている。そのため、岩が既に
        // (別の経緯で)揺れ明け・連続落下中で、ダイヤの方はこのtickで初めて支持を
        // 失ったばかり(揺れ猶予の1ティック目)、という組み合わせが起こり得る。この
        // 状態で書き込みフェーズ(旧: 着地先の非Emptyチェックが無かった)が実行される
        // と、岩がまだそこに物理的に残っているダイヤへ無警告で上書きしてしまっていた。
        let mut board = empty_board(6);
        board.rows[0][0] = Cell::Rock { hits: 0 };
        board.rows[1][0] = Cell::Diamond;
        // row2は空。ダイヤはこのtickで初めて支持を失う(直下が最深行でも岩でもない)。

        let mut gravity = GravityState::new();
        // 岩(代表座標(0,0))は、既に別の経緯で揺れ明け・連続落下中だったことを模す
        // (実プレイでは、この岩は隣接列と連結した塊としてもっと手前のtickから
        // 揺れ明けしていた)。
        gravity.unsupported_ticks.insert((0, 0), SHAKE_TICKS);

        let outcome = apply_gravity_tick(
            &mut board,
            (usize::MAX, usize::MAX),
            &[],
            &mut gravity,
            SHAKE_TICKS,
        );

        assert_eq!(
            board.cell(1, 0),
            Cell::Diamond,
            "ダイヤはまだ揺れ猶予中(このtickで支持を失ったばかり)なので、\
             揺れ明け済みの岩に上書きされて消えてはいけない"
        );
        assert_eq!(
            board.cell(0, 0),
            Cell::Rock { hits: 0 },
            "ダイヤがまだそこに物理的に残っている以上、岩はこのtickでは1マス落下できないはず"
        );
        assert!(
            outcome.moved_cells.is_empty(),
            "着地先がまだ塞がっているので、このtickでは何も移動しないはず: {:?}",
            outcome.moved_cells
        );

        // その後、ダイヤ自身の揺れ猶予が明けて実際に落下を始めれば、岩も連動して
        // 正しく後を追って落下し続けられる(=恒久的にフリーズしたままにはならない)。
        for _ in 0..(SHAKE_TICKS as usize + 4) {
            apply_gravity_tick(
                &mut board,
                (usize::MAX, usize::MAX),
                &[],
                &mut gravity,
                SHAKE_TICKS,
            );
        }
        assert_eq!(
            board.cell(5, 0),
            Cell::Diamond,
            "ダイヤは最終的に最深行まで正しく落下しているはず(消えてはいない)"
        );
        assert_eq!(
            board.cell(4, 0),
            Cell::Rock { hits: 0 },
            "岩もダイヤを追って正しく最深行の1つ上まで落下しているはず"
        );
    }

    #[test]
    fn diamond_count_is_conserved_across_many_gravity_ticks_on_random_boards() {
        // #85再調査(2026/07/27): プレイ中のスクリーンショットで、単独のダイヤブロックが
        // 移動(move)・消滅(vanish)どちらのログにも一度も現れないまま盤面から消えている
        // ことが2回連続で確認された。ダイヤは`collect_fall_groups`で常にサイズ1の塊として
        // 扱われ、色/岩ブロックのような4連結自動消滅の対象にはならない(spec.md 2章)ため、
        // `apply_gravity_tick`(プレイヤーへの押し潰し以外)を通しては本来一切消えないはず。
        // ランダム生成した盤面を十分な回数ティックさせても、盤面全体のダイヤ総数が
        // 変わらないことを確認する不変条件テスト(プレイヤー位置は影響しない盤外に置き、
        // 押し潰しによる正規の消滅経路も除外する)。
        for seed in 0..20u64 {
            let mut board = Board::generate(seed, 60, FIELD_WIDTH);
            let mut gravity = GravityState::new();
            let player_pos = (usize::MAX, usize::MAX);

            let count_diamonds = |b: &Board| -> usize {
                let mut n = 0;
                for row in 0..b.depth_rows() {
                    for col in 0..b.width() {
                        if b.cell(row, col) == Cell::Diamond {
                            n += 1;
                        }
                    }
                }
                n
            };

            let before = count_diamonds(&board);

            for _ in 0..(SHAKE_TICKS as usize + 1) * 60 {
                let outcome =
                    apply_gravity_tick(&mut board, player_pos, &[], &mut gravity, SHAKE_TICKS);
                assert!(
                    !outcome
                        .vanished_cells
                        .iter()
                        .any(|&(_, kind)| kind == Cell::Diamond),
                    "seed={seed}: ダイヤが自動消滅の対象になっている(本来あり得ない)"
                );
            }

            let after = count_diamonds(&board);
            assert_eq!(
                before, after,
                "seed={seed}: ダイヤの総数が変化した(移動/消滅どちらのログにも残らず盤面から消えた疑い): 前={before} 後={after}"
            );
        }
    }

    #[test]
    fn diamond_count_is_conserved_across_many_gravity_ticks_after_reroll_at_realistic_depth() {
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        // #85再調査: 上のテスト(reroll前の浅い盤面)では再現しなかったため、実プレイに近い
        // 条件(reroll_overlays_from_row適用後、ダイヤ出現率を上げた深い深度相当)でも
        // 同じ不変条件(ダイヤ総数の保存)を確認する。
        for seed in 0..10u64 {
            let mut board = Board::generate(seed, 70, FIELD_WIDTH);
            let gravity_for_reroll = GravityState::new();
            board.reroll_overlays_from_row(
                &mut rng,
                2,
                100,
                100,
                100,
                200,
                0,
                0,
                0,
                4,
                100,
                &gravity_for_reroll,
            );

            let mut gravity = GravityState::new();
            let player_pos = (usize::MAX, usize::MAX);

            let count_diamonds = |b: &Board| -> usize {
                let mut n = 0;
                for row in 0..b.depth_rows() {
                    for col in 0..b.width() {
                        if b.cell(row, col) == Cell::Diamond {
                            n += 1;
                        }
                    }
                }
                n
            };

            let before = count_diamonds(&board);

            for _ in 0..(SHAKE_TICKS as usize + 1) * 70 {
                let outcome =
                    apply_gravity_tick(&mut board, player_pos, &[], &mut gravity, SHAKE_TICKS);
                assert!(
                    !outcome
                        .vanished_cells
                        .iter()
                        .any(|&(_, kind)| kind == Cell::Diamond),
                    "seed={seed}: ダイヤが自動消滅の対象になっている(本来あり得ない)"
                );
            }

            let after = count_diamonds(&board);
            assert_eq!(
                before, after,
                "seed={seed}: reroll後・深い深度でダイヤの総数が変化した: 前={before} 後={after}"
            );
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

    // --- 固体オーバーレイ(設置済みボム)を支え/障害物として扱う(#240) ---

    /// 固体オーバーレイのテストで共通に使う盤外のプレイヤー位置。
    const OFF_BOARD_PLAYER: (usize, usize) = (usize::MAX, usize::MAX);

    /// 固体オーバーレイのテストで扱う全セル種別。
    fn overlay_test_cell_kinds() -> [Cell; 6] {
        [
            Cell::Color(ColorKind::Red),
            Cell::Rock { hits: 0 },
            Cell::Diamond,
            Cell::Star { visible_ms: 0 },
            Cell::Item(ItemEffect::ClearAbove),
            Cell::Oxygen,
        ]
    }

    #[test]
    fn every_cell_kind_rests_on_top_of_a_solid_overlay() {
        // 設置済みボムはCellグリッド外のオーバーレイのため、盤面だけを見る重力解決では
        // 支えとして数えられず、真上のブロックがボムのマスへ落ちて重なって見えていた
        // (#240)。全種別が固体オーバーレイの真上で静止することを確認する。
        for kind in overlay_test_cell_kinds() {
            let mut board = empty_board(5);
            board.rows[1][0] = kind; // オーバーレイの真上。盤面上の支えは無い
            let solid = [(2usize, 0usize)];
            let mut gravity = GravityState::new();

            for _ in 0..(SHAKE_TICKS as usize + 2) {
                let outcome = apply_gravity_tick(
                    &mut board,
                    OFF_BOARD_PLAYER,
                    &solid,
                    &mut gravity,
                    SHAKE_TICKS,
                );
                assert!(
                    outcome.moved_cells.is_empty(),
                    "{kind:?}: 固体オーバーレイの上では落下しないはず"
                );
            }

            assert_eq!(board.cell(1, 0), kind, "{kind:?}: 元の位置に残るはず");
            assert_eq!(
                board.cell(2, 0),
                Cell::Empty,
                "{kind:?}: オーバーレイのマスへ入って重なってはいけない"
            );
            assert!(
                !gravity.is_shaking((1, 0)),
                "{kind:?}: 支持されているので揺れもしないはず"
            );
        }
    }

    #[test]
    fn without_a_solid_overlay_every_cell_kind_still_falls_through_that_cell() {
        // 上のテストの対比。`solid`が空スライスなら従来通りの挙動(素通りして落下)で
        // あることを確認し、支えが増えたのはオーバーレイを渡した場合だけだと固定する。
        for kind in overlay_test_cell_kinds() {
            let mut board = empty_board(5);
            board.rows[1][0] = kind;
            let mut gravity = GravityState::new();

            for _ in 0..(SHAKE_TICKS as usize + 1) * 5 {
                apply_gravity_tick(&mut board, OFF_BOARD_PLAYER, &[], &mut gravity, SHAKE_TICKS);
            }

            assert_eq!(
                board.cell(4, 0),
                kind,
                "{kind:?}: オーバーレイが無ければ最深行まで落ちるはず"
            );
        }
    }

    #[test]
    fn a_block_falling_from_above_stops_one_row_above_the_solid_overlay() {
        // 既に落下を始めている塊(揺れ明け済み)も、オーバーレイの1マス上で止まるはず。
        let mut board = empty_board(6);
        board.rows[0][0] = Cell::Color(ColorKind::Red);
        let solid = [(3usize, 0usize)];
        let mut gravity = GravityState::new();

        for _ in 0..(SHAKE_TICKS as usize + 1) * 6 {
            apply_gravity_tick(
                &mut board,
                OFF_BOARD_PLAYER,
                &solid,
                &mut gravity,
                SHAKE_TICKS,
            );
        }

        assert_eq!(
            board.cell(2, 0),
            Cell::Color(ColorKind::Red),
            "オーバーレイの1マス上で止まるはず"
        );
        assert_eq!(
            board.cell(3, 0),
            Cell::Empty,
            "オーバーレイのマスは空のまま"
        );
        assert!(
            !gravity.is_shaking((2, 0)),
            "支持されて止まったのだから、揺れ続けてもいけない"
        );
    }

    #[test]
    fn a_two_wide_group_is_supported_when_only_one_side_rests_on_the_solid_overlay() {
        // 塊単位の支持判定。片側だけがオーバーレイに乗っていれば塊全体が支持される
        // (ちぎれて片側だけ落ちることもない)。
        let mut board = empty_board(5);
        board.rows[1][0] = Cell::Color(ColorKind::Red);
        board.rows[1][1] = Cell::Color(ColorKind::Red);
        let solid = [(2usize, 0usize)]; // col1の直下は空洞のまま
        let mut gravity = GravityState::new();

        for _ in 0..(SHAKE_TICKS as usize + 2) {
            apply_gravity_tick(
                &mut board,
                OFF_BOARD_PLAYER,
                &solid,
                &mut gravity,
                SHAKE_TICKS,
            );
        }

        assert_eq!(board.cell(1, 0), Cell::Color(ColorKind::Red));
        assert_eq!(
            board.cell(1, 1),
            Cell::Color(ColorKind::Red),
            "片側だけがオーバーレイに乗っていても塊全体が支持されるはず"
        );
        assert!(
            !gravity.is_shaking((1, 0)) && !gravity.is_shaking((1, 1)),
            "支持されているのだから揺れ続けてもいけない"
        );
    }

    #[test]
    fn stacked_groups_are_both_supported_through_the_solid_overlay() {
        // 異色2段積み(上段Blue on 下段Red on オーバーレイ)。`has_stable_support`の連鎖
        // 判定でもオーバーレイを安定した支えとして扱わないと、下段が未支持と判定されて
        // 上段まで巻き込まれ、2段まとめて落ちてしまう。
        let mut board = empty_board(5);
        board.rows[1][0] = Cell::Color(ColorKind::Blue);
        board.rows[2][0] = Cell::Color(ColorKind::Red);
        let solid = [(3usize, 0usize)];
        let mut gravity = GravityState::new();

        for _ in 0..(SHAKE_TICKS as usize + 2) {
            apply_gravity_tick(
                &mut board,
                OFF_BOARD_PLAYER,
                &solid,
                &mut gravity,
                SHAKE_TICKS,
            );
        }

        assert_eq!(board.cell(1, 0), Cell::Color(ColorKind::Blue));
        assert_eq!(board.cell(2, 0), Cell::Color(ColorKind::Red));
        assert_eq!(board.cell(3, 0), Cell::Empty);
        assert!(
            !gravity.is_shaking((1, 0)) && !gravity.is_shaking((2, 0)),
            "連鎖判定でも支えとして数えられるので、どちらの段も揺れ続けないはず"
        );
    }

    #[test]
    fn a_four_color_row_landing_on_a_single_solid_overlay_point_auto_vanishes() {
        // 着地後の自動消滅判定が呼ぶ`is_group_supported`にもオーバーレイを渡さないと、
        // 「まだ落下中」と誤判定されて消滅がスキップされる。次tick以降は支持済みなので
        // 二度と判定されず、4連結が永久に残ってしまう。
        let mut board = empty_board(6);
        for col in 0..4 {
            board.rows[1][col] = Cell::Color(ColorKind::Red);
        }
        let solid = [(3usize, 0usize)]; // col0の1マス下だけがオーバーレイ
        let mut gravity = GravityState::new();

        let mut vanished = 0usize;
        for _ in 0..(SHAKE_TICKS as usize + 2) {
            let outcome = apply_gravity_tick(
                &mut board,
                OFF_BOARD_PLAYER,
                &solid,
                &mut gravity,
                SHAKE_TICKS,
            );
            vanished += outcome.auto_vanished_blocks;
        }

        assert_eq!(
            vanished, 4,
            "オーバーレイ1点に支えられて着地した4連結は自動消滅するはず"
        );
        for col in 0..4 {
            assert_eq!(board.cell(2, col), Cell::Empty, "col={col}が消え残っている");
        }
    }

    #[test]
    fn a_four_rock_row_landing_on_a_single_solid_overlay_point_auto_vanishes() {
        // 岩ブロック版(得点は発生しないが自動消滅はする、4.9)。
        let mut board = empty_board(6);
        for col in 0..4 {
            board.rows[1][col] = Cell::Rock { hits: 0 };
        }
        let solid = [(3usize, 0usize)];
        let mut gravity = GravityState::new();

        let mut vanished = 0usize;
        for _ in 0..(SHAKE_TICKS as usize + 2) {
            let outcome = apply_gravity_tick(
                &mut board,
                OFF_BOARD_PLAYER,
                &solid,
                &mut gravity,
                SHAKE_TICKS,
            );
            vanished += outcome.auto_vanished_rock_blocks;
        }

        assert_eq!(
            vanished, 4,
            "オーバーレイ1点に支えられて着地した岩4連結も自動消滅するはず"
        );
        for col in 0..4 {
            assert_eq!(board.cell(2, col), Cell::Empty, "col={col}が消え残っている");
        }
    }
}

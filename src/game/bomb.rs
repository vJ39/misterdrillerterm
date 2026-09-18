//! ボム(白ボンが投げ込む爆発物)関連ロジック(#241/#242等。spec.md該当章)。
//! 出現・登場演出・転がり・静止・起爆カウントダウン・爆風・プレイヤー/ブロックとの
//! 押し出し・段差登り判定をまとめる。

use super::*;

/// ボムの演出段階。白ボンが画面外から登場し、ボムを投げ、転がって静止し、爆発する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BombPhase {
    /// 白ボンが画面端(`Bomb::origin`)に登場し、ボムを投げる直前までの間。この間`pos`は
    /// 毎フレーム再検証され、塞がれれば同じ列の上方向へずらされる(#241)。
    Entering,
    /// 投げられたボムが`origin`から`pos`(最終設置マス)まで転がっている間。この間も`pos`は
    /// 毎フレーム再検証され、塞がれれば同じ列の上方向へずらされる(#241)。
    Rolling,
    /// 転がり終えた直後、支えを失っていれば落下しつつ、左右に跳ねながら落ち着き先を
    /// 探している間。
    Settling,
    /// 静止し、点滅しながら起爆までカウントダウンしている間。`remaining_ms`はこの段階で
    /// のみ減り、支えを失って落下している間は減少を止める(空中で起爆させないため)。
    Ticking,
}

/// `push_bomb_in_the_way`の結果。
pub(super) enum BombInTheWay {
    /// ボムが無い、または押し出しに成功した。通常の物理判定(physics::move_lateral)へ進む。
    ClearToMove,
    /// 押し出せず、段差登り判定を自分で行った(呼び出し側は通常の物理判定を呼ばない)。
    /// 登れた場合、道中(自分の真上→登り先の順)で取得したAIR・アイテムを伴う。
    HandledAsClimb([Option<Pickup>; 2]),
}

/// 白ボンがランダムに投げ込むボム。移動する敵キャラは持たず、盤面上に設置された
/// ボム自体だけを管理する。ブロックとは別レイヤーなので`Cell`列挙体には追加しない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bomb {
    /// 現在位置(落下・跳ねで初期位置から動くことがある)。
    pub pos: board::Pos,
    /// 白ボンが登場する画面端の位置(同じ行、列0か列`width-1`)。
    pub origin: board::Pos,
    pub phase: BombPhase,
    /// 現在の`phase`に入ってからの経過時間(ms)。
    pub phase_elapsed_ms: u32,
    /// 起爆までの残り時間(ms)。`BombPhase::Ticking`に入って初めて減り始める。
    pub remaining_ms: u32,
    /// `BombPhase::Settling`中に左右へ跳ねる方向(+1=右、-1=左)。
    pub settle_bounce_dir: i8,
}

impl Game {
    /// 移動先セルに静止中(Settling/Ticking)のボムがあれば、進行方向へさらに1マス押し
    /// 出し、Settling(左右バウンド中)へ遷移させて以後の重力・バウンド判定に委ねる。
    /// 押し出し先が盤面外・ブロック・他のボムで塞がっていれば、岩ブロックと同じ段差登り
    /// 判定を自分で行う。ボムが無い、または登場・投擲演出中(Entering/Rolling)であれば
    /// 何もせず`ClearToMove`を返す(通常の物理判定に委ねる)。
    pub(super) fn push_bomb_in_the_way(&mut self, dir: Direction) -> BombInTheWay {
        let (_, dc) = dir.delta();
        let nc = self.player.col as isize + dc;
        if nc < 0 || nc as usize >= self.board.width() {
            return BombInTheWay::ClearToMove; // 盤面外は既存の境界チェックに任せる。
        }
        let nc = nc as usize;
        let row = self.player.row;

        let Some(bomb_index) = self.bombs.iter().position(|b| {
            b.pos == (row, nc) && matches!(b.phase, BombPhase::Settling | BombPhase::Ticking)
        }) else {
            return BombInTheWay::ClearToMove;
        };

        let push_c = nc as isize + dc;
        let push_pos = if push_c < 0 || push_c as usize >= self.board.width() {
            None
        } else {
            let candidate = (row, push_c as usize);
            let occupied_by_other_bomb = self
                .bombs
                .iter()
                .enumerate()
                .any(|(i, b)| i != bomb_index && b.pos == candidate);
            (self.board.cell(candidate.0, candidate.1) == Cell::Empty && !occupied_by_other_bomb)
                .then_some(candidate)
        };

        let Some(push_pos) = push_pos else {
            return BombInTheWay::HandledAsClimb(self.climb_over_unpushable_bomb(dir, nc));
        };

        let bomb = &mut self.bombs[bomb_index];
        bomb.pos = push_pos;
        bomb.phase = BombPhase::Settling;
        bomb.phase_elapsed_ms = 0;
        bomb.settle_bounce_dir = if dc > 0 { 1 } else { -1 };
        BombInTheWay::ClearToMove
    }

    /// 押し出せない静止中のボムに対し、岩ブロックと同じ「ぶつかって停止→同方向2回目で
    /// 1段登る」段差登り判定を行う。`physics::move_lateral`と同じ判定式を踏襲しつつ、ボムは
    /// Cellグリッド外のオーバーレイのため、判定対象をボムの有無に置き換える。
    ///
    /// 戻り値は道中(自分の真上→登り先の順)で取得したAIR・アイテムで、登れなかった場合は
    /// どちらも`None`になる。`move_lateral`と同様、頭上・登り先の両方が通過可能だと
    /// 確かめてから取得する(登れないのに頭上だけ取得してしまうのを防ぐ)。
    fn climb_over_unpushable_bomb(&mut self, dir: Direction, nc: usize) -> [Option<Pickup>; 2] {
        let was_bumped_same_dir = self.player.bumped_direction == Some(dir);
        self.player.facing = dir;

        if was_bumped_same_dir && self.player.row > 0 {
            let overhead = (self.player.row - 1, self.player.col);
            let landing = (self.player.row - 1, nc);
            if physics::is_climb_passable(self.board.cell(overhead.0, overhead.1))
                && !self.settled_bomb_at(overhead.0, overhead.1)
                && physics::is_climb_passable(self.board.cell(landing.0, landing.1))
                && !self.settled_bomb_at(landing.0, landing.1)
            {
                let overhead_pickup =
                    physics::take_climb_pickup(&mut self.board, &mut self.player, overhead);
                let landing_pickup =
                    physics::take_climb_pickup(&mut self.board, &mut self.player, landing);
                self.player.row -= 1;
                self.player.col = nc;
                self.player.bumped_direction = None;
                return [overhead_pickup, landing_pickup];
            }
        }

        self.player.bumped_direction = Some(dir);
        [None, None]
    }

    /// 指定セルに静止中(Settling/Ticking)のボムがあるかどうか。
    pub(super) fn settled_bomb_at(&self, row: usize, col: usize) -> bool {
        self.bombs.iter().any(|b| {
            b.pos == (row, col) && matches!(b.phase, BombPhase::Settling | BombPhase::Ticking)
        })
    }

    /// 静止中(Settling/Ticking)のボムが占めるマスの一覧。ボムはCellグリッド外のオーバー
    /// レイなので、重力・支持判定系(`physics::process_gravity_tick`等)へ「固体オーバーレイ」
    /// として明示的に渡す必要がある。渡さないとボムの真上のブロックがボムのマスへ落下して
    /// 重なって見える。まだ登場・投擲演出中(Entering/Rolling)のボムは実体を持たないため
    /// 含めない(`settled_bomb_at`と同じ基準)。盤面上のボムは`BOMB_MAX_COUNT_ON_BOARD`個
    /// までなので`Vec`の線形探索で十分。
    pub(super) fn settled_bomb_positions(&self) -> Vec<Pos> {
        self.bombs
            .iter()
            .filter(|b| matches!(b.phase, BombPhase::Settling | BombPhase::Ticking))
            .map(|b| b.pos)
            .collect()
    }

    /// facing方向に静止中(Settling/Ticking)のボムがあれば掘削で除去する。爆発は誘発
    /// しない(単純に取り除くだけ)。除去した場合は`true`を返し、呼び出し側は通常の掘削
    /// 処理をスキップする。まだ登場・投擲演出中(Entering/Rolling)のボムは対象外。
    pub(super) fn destroy_bomb_facing(&mut self) -> bool {
        let (dr, dc) = self.player.facing.delta();
        let nr = self.player.row as isize + dr;
        let nc = self.player.col as isize + dc;
        if nr < 0
            || nc < 0
            || nr as usize >= self.board.depth_rows()
            || nc as usize >= self.board.width()
        {
            return false;
        }
        let target = (nr as usize, nc as usize);
        let Some(index) = self.bombs.iter().position(|b| {
            b.pos == target && matches!(b.phase, BombPhase::Settling | BombPhase::Ticking)
        }) else {
            return false;
        };
        self.bombs.remove(index);
        true
    }

    /// ボム出現頻度を直接指定する(起動時、Settingsから読み込んだ値を適用する用途)。範囲外
    /// の値は`BOMB_SPAWN_RATE_PERCENT_MIN`〜`MAX`にクランプする。
    pub fn set_bomb_spawn_rate_percent(&mut self, percent: u32) {
        self.bomb_spawn_rate_percent = percent.clamp(
            crate::constants::BOMB_SPAWN_RATE_PERCENT_MIN,
            crate::constants::BOMB_SPAWN_RATE_PERCENT_MAX,
        );
    }

    /// ボム爆発までの時間を直接指定する(起動時、Settingsから読み込んだ値を適用する用途)。
    /// 範囲外の値は`BOMB_FUSE_MS_MIN`〜`MAX`にクランプする。新規に出現するボムから
    /// 反映され、設置済みボムの残り時間には影響しない。
    pub fn set_bomb_fuse_ms(&mut self, ms: u32) {
        self.bomb_fuse_ms = ms.clamp(
            crate::constants::BOMB_FUSE_MS_MIN,
            crate::constants::BOMB_FUSE_MS_MAX,
        );
    }

    /// 現在盤面上にあるボムの一覧。描画側(render.rs)が参照する。
    pub fn bombs(&self) -> &[Bomb] {
        &self.bombs
    }

    /// テスト専用: ボムを直接配置するための可変参照。`debug_place_bomb`は配置先がランダム
    /// なため、「この座標にボムがある盤面」を組み立てたい場合に使う。
    #[cfg(test)]
    pub(crate) fn bombs_mut(&mut self) -> &mut Vec<Bomb> {
        &mut self.bombs
    }

    /// 1個のボムの爆風を盤面へ適用する(炎フラッシュ・岩/ダイヤのスター化・色ブロックの
    /// 一色統一・アイテムブロックの破壊・新たに4連結以上になったグループの自動消滅)。
    /// プレイヤーが爆風に巻き込まれたかどうかを返すのみで、ミス処理自体は呼び出し側の
    /// 責務とする(通常の起爆完了時と、死亡時の即時全爆発の両方から共通で使うため)。
    fn apply_bomb_blast(&mut self, bomb: &Bomb, events: &mut Vec<GameEvent>) -> bool {
        let blast_cells = bomb_blast_cells(
            &self.board,
            bomb.pos,
            BOMB_BLAST_ROW_RANGE,
            BOMB_BLAST_COL_RANGE,
        );
        let mut hit_player = false;
        let flash = Duration::from_millis(BOMB_EXPLOSION_FLASH_MS);
        // 爆風が届いた色ブロックは一色に統一する。爆発ごとに1色をランダムに選び、その
        // 爆発の範囲内にある色ブロック全てを同じ色に揃える。ショートカットC/UnifyColors
        // アイテムと違い、ボム爆発では統一後に4連結以上になったグループを着地時の自動
        // 消滅と同じ判定でこの場で消す。
        use rand::RngExt;
        let all_colors = ColorKind::ALL;
        let unify_color = all_colors[self.rng.random_range(0..all_colors.len())];
        let mut unified_positions = Vec::new();
        // 爆風で破壊したアイテムブロック(消滅ログ用にループ後まとめて記録する)。
        let mut destroyed_items: Vec<(board::Pos, Cell)> = Vec::new();
        for &(row, col) in &blast_cells {
            if (row, col) == self.player.position() {
                hit_player = true;
            }
            // 爆心地(ボム設置マス)から遠いほど炎の色調を外側寄り(中心ほど白熱、外側
            // ほど赤黒い)にする。爆風は上下左右の直線上にしか届かないため、マンハッタン
            // 距離がそのまま「軸方向に何マス離れているか」と一致する。
            let tier = row.abs_diff(bomb.pos.0) + col.abs_diff(bomb.pos.1);
            let tier = tier.min(u8::MAX as usize) as u8;
            // 炎フラッシュは着弾したセルの中身を問わず、爆風が通過した全マスに表示する。
            // Rock/Diamond/Colorを変化させた場合だけにすると、爆風がEmpty(既に掘削済み
            // の空間)を貫通する区間で炎が全く見えなくなる。
            self.recently_exploded.push(((row, col), flash, tier));
            if matches!(self.board.cell(row, col), Cell::Rock { .. } | Cell::Diamond) {
                self.board.set(row, col, Cell::Star { visible_ms: 0 });
            } else if matches!(self.board.cell(row, col), Cell::Color(_)) {
                self.board.set(row, col, Cell::Color(unify_color));
                unified_positions.push((row, col));
            } else if matches!(self.board.cell(row, col), Cell::Item(_)) {
                // アイテムブロック(C/R/K)は爆風で破壊する。頭上一括クリア系の効果では
                // アイテムをAIRと同じ保護対象にしているが、その保護はボムの爆風には
                // 及ばせない(AIR自体は爆風でも消さない)。効果は発動させず、スターも
                // 生まずにただ消す。取得(触れる)と破壊(爆風)は別物として扱う。
                let old = self.board.cell(row, col);
                self.board.set(row, col, Cell::Empty);
                destroyed_items.push(((row, col), old));
            }
        }

        // 消滅ログ・消滅フラッシュの記録は、下の色ブロック4連結消滅と同じように
        // ループ後にまとめて1回で行う。
        if !destroyed_items.is_empty() {
            // 爆風による破壊は落下とは無関係にその場で消えるため、待たずに光り始める。
            self.note_vanished_cells(destroyed_items, Duration::ZERO);
        }

        // 一色に統一した結果、新たに4連結以上になったグループはこの場で消滅させる。同じ
        // グループに属する複数の位置を二重に処理しないよう、判定済みは`checked`で除外する。
        let mut checked: Vec<board::Pos> = Vec::new();
        for &pos in &unified_positions {
            if checked.contains(&pos) {
                continue;
            }
            let group = connected_same_color(&self.board, pos, unify_color);
            checked.extend(group.iter().copied());
            if group.len() >= 4 {
                let vanished: Vec<(board::Pos, Cell)> = group
                    .iter()
                    .map(|&g| (g, self.board.cell(g.0, g.1)))
                    .collect();
                for &(r, c) in &group {
                    self.board.set(r, c, Cell::Empty);
                }
                events.push(GameEvent::BlockDestroyed {
                    blocks: group.len(),
                });
                self.note_vanished_cells(vanished, Duration::ZERO);
            }
        }

        events.push(GameEvent::BombExploded);
        hit_player
    }

    /// `initial`のボムをまとめて爆発させる。爆風が他の(まだ`self.bombs`に残っている)ボムを
    /// 巻き込んだら、そのボムも連鎖してこの場で爆発させる(誘爆)。連鎖が全て終わるまで
    /// ミス処理は行わず、`trigger_miss_on_hit`がtrueの場合のみ、連鎖全体でプレイヤーが一度
    /// でも巻き込まれていればその場でミス処理する(死亡処理の途中から呼ぶ場合はfalse)。
    fn detonate_bombs(
        &mut self,
        initial: Vec<Bomb>,
        events: &mut Vec<GameEvent>,
        trigger_miss_on_hit: bool,
    ) {
        let mut queue = initial;
        let mut any_hit_player = false;
        while let Some(bomb) = queue.pop() {
            any_hit_player |= self.apply_bomb_blast(&bomb, events);

            let blast_cells = bomb_blast_cells(
                &self.board,
                bomb.pos,
                BOMB_BLAST_ROW_RANGE,
                BOMB_BLAST_COL_RANGE,
            );
            let mut caught_indices: Vec<usize> = self
                .bombs
                .iter()
                .enumerate()
                .filter(|(_, b)| blast_cells.contains(&b.pos))
                .map(|(i, _)| i)
                .collect();
            caught_indices.sort_unstable_by(|a, b| b.cmp(a)); // 後ろから取り出す
            for i in caught_indices {
                queue.push(self.bombs.remove(i));
            }
        }
        // 同一フレームで複数のボムが爆発し、どちらもプレイヤーを巻き込んだ場合に二重で
        // ミス処理しないよう、既にミス処理済み(is_dying/GameOver)でないことを確認する。
        if trigger_miss_on_hit
            && any_hit_player
            && !self.is_dying()
            && self.status == GameStatus::Playing
        {
            self.apply_miss(MissCause::BombBlast, events);
        }
    }

    /// 盤面上の全てのボムを、画面内外を問わずこの場で即座に爆発させる(誘爆の連鎖込み)。
    /// 死亡処理の途中(`resolve_death_board_effects`)から呼ぶため、爆風がプレイヤーを
    /// 巻き込んでもこれ以上のミス処理の連鎖はしない。
    pub(super) fn detonate_all_bombs_immediately(&mut self, events: &mut Vec<GameEvent>) {
        let bombs = std::mem::take(&mut self.bombs);
        self.detonate_bombs(bombs, events, false);
    }

    /// ボム出現を1回判定する。盤面全体のボム数が上限未満で、深度・設定に応じた確率の
    /// 抽選に当たれば、画面内のランダムなEmptyマスへボムを1個設置する。
    fn maybe_spawn_bomb(&mut self) {
        if self.bombs.len() >= BOMB_MAX_COUNT_ON_BOARD {
            return;
        }
        let prob = (BOMB_SPAWN_BASE_PROB
            + BOMB_SPAWN_DEPTH_MAX_BONUS * depth_fraction(self.player.depth_m()))
            * (self.bomb_spawn_rate_percent as f32 / 100.0);
        if self.rng.random_range(0.0..1.0) >= prob {
            return;
        }
        self.spawn_bomb_at_random_empty_cell();
    }

    /// 画面内(プレイヤー位置から上下`STAR_VISIBLE_RANGE_ROWS`行)のEmptyマスを1つランダムに
    /// 選び、ボムを設置する。候補が無ければ何もしない。他のボムが既に占めているマスは
    /// 候補から除外する(ボム同士を重ねないため)。プレイヤーが現在いるマスも候補から
    /// 除外する(#241。含めてしまうとEntering/Rollingの再検証で毎回上へ逃げる不自然な
    /// 演出になるため、そもそも狙わせない)。
    fn spawn_bomb_at_random_empty_cell(&mut self) {
        let start_row = self.player.row.saturating_sub(STAR_VISIBLE_RANGE_ROWS);
        let end_row = (self.player.row + STAR_VISIBLE_RANGE_ROWS)
            .min(self.board.depth_rows().saturating_sub(1));
        let width = self.board.width();
        let occupied: Vec<board::Pos> = self.bombs.iter().map(|b| b.pos).collect();
        let player_pos = self.player.position();
        let candidates: Vec<board::Pos> = (start_row..=end_row)
            .flat_map(|row| (0..width).map(move |col| (row, col)))
            .filter(|&(row, col)| {
                self.board.cell(row, col) == Cell::Empty
                    && !occupied.contains(&(row, col))
                    && (row, col) != player_pos
            })
            .collect();
        if candidates.is_empty() {
            return;
        }
        let idx = self.rng.random_range(0..candidates.len());
        let pos = candidates[idx];
        // 白ボンは画面の左端・右端のどちらかから登場する。同じ行の端から登場させることで、
        // 必ず盤面内を横切って転がってくる形になる。
        let edge_col = if self.rng.random_range(0..2) == 0 {
            0
        } else {
            width.saturating_sub(1)
        };
        self.bombs.push(Bomb {
            pos,
            origin: (pos.0, edge_col),
            phase: BombPhase::Entering,
            phase_elapsed_ms: 0,
            remaining_ms: self.bomb_fuse_ms,
            settle_bounce_dir: 1,
        });
    }

    /// デバッグ: ボムを1個、画面内のランダムなEmptyマスへ即座に設置する。盤面全体のボム数
    /// が上限に達している、または出現先が無ければ何もしない。Playing中のみ有効。
    pub fn debug_place_bomb(&mut self) {
        if self.status != GameStatus::Playing || self.bombs.len() >= BOMB_MAX_COUNT_ON_BOARD {
            return;
        }
        self.spawn_bomb_at_random_empty_cell();
    }

    /// ボムの進行(登場→転がり→静止→起爆カウントダウン→誘爆、および
    /// `BOMB_SPAWN_CHECK_INTERVAL_MS`ごとの新規出現判定)を1フレームぶん進める。
    /// `was_dying`は`update()`呼び出し時点で「天に召される」演出中だったかどうかで、
    /// 演出中は位置の食い違いを避けるため他のプレイヤー関連処理と同様に何もしない。
    /// 戻り値は`update()`がこの後の処理を続けてよいかどうか(falseなら`update`全体を
    /// 打ち切ってその場で`events`を返す)。
    pub(super) fn tick_bombs(
        &mut self,
        delta: Duration,
        was_dying: bool,
        events: &mut Vec<GameEvent>,
    ) -> bool {
        if was_dying {
            return true;
        }
        let delta_ms = delta.as_millis() as u32;
        let mut exploded = Vec::new();
        // 他のボムの現在位置のスナップショット。ボムはCellグリッドとは別のオーバー
        // レイ(`Vec<Bomb>`)のため、盤面のセルだけを見て重力・バウンドを判定すると
        // 他のボムへ重なってしまう。このフレーム開始時点の位置で判定するので、複数の
        // ボムがほぼ同時に同じマスへ動く稀なケースでは1フレームだけずれる(次で解消)。
        let bomb_positions: Vec<board::Pos> = self.bombs.iter().map(|b| b.pos).collect();
        for (i, bomb) in self.bombs.iter_mut().enumerate() {
            match bomb.phase {
                BombPhase::Entering => {
                    if let Some(p) = bomb_landing_pos_adjusted_upward(
                        &self.board,
                        bomb.pos,
                        i,
                        &bomb_positions,
                        self.player.position(),
                    ) {
                        bomb.pos = p;
                        bomb.origin.0 = p.0; // 「originとposは常に同じ行」の不変条件を維持するため
                    }
                    bomb.phase_elapsed_ms = bomb.phase_elapsed_ms.saturating_add(delta_ms);
                    if bomb.phase_elapsed_ms >= BOMB_ENTER_MS {
                        bomb.phase = BombPhase::Rolling;
                        bomb.phase_elapsed_ms = 0;
                    }
                }
                BombPhase::Rolling => {
                    if let Some(p) = bomb_landing_pos_adjusted_upward(
                        &self.board,
                        bomb.pos,
                        i,
                        &bomb_positions,
                        self.player.position(),
                    ) {
                        bomb.pos = p;
                        bomb.origin.0 = p.0; // 「originとposは常に同じ行」の不変条件を維持するため
                    }
                    bomb.phase_elapsed_ms = bomb.phase_elapsed_ms.saturating_add(delta_ms);
                    if bomb.phase_elapsed_ms >= BOMB_ROLL_MS {
                        bomb.phase = BombPhase::Settling;
                        bomb.phase_elapsed_ms = 0;
                        bomb.settle_bounce_dir = if self.rng.random_bool(0.5) { 1 } else { -1 };
                    }
                }
                BombPhase::Settling => {
                    // 支えを失っていれば落下し、支持されていれば左右に跳ねて落ち着き
                    // 先を探す。`BOMB_SETTLE_TICK_MS`ごとに1歩ぶん進める。
                    let prev_ticks = bomb.phase_elapsed_ms / BOMB_SETTLE_TICK_MS;
                    bomb.phase_elapsed_ms = bomb.phase_elapsed_ms.saturating_add(delta_ms);
                    let new_ticks = bomb.phase_elapsed_ms / BOMB_SETTLE_TICK_MS;
                    // 1フレームのdeltaが複数tickぶんまたぐ場合(低フレームレート等)でも
                    // 歩数が実時間ぶん進むよう、またいだ回数ぶん繰り返す。
                    for _ in 0..(new_ticks - prev_ticks) {
                        bomb_settle_step(
                            &self.board,
                            &mut bomb.pos,
                            &mut bomb.settle_bounce_dir,
                            &bomb_positions,
                            self.player.position(),
                        );
                    }
                    if bomb.phase_elapsed_ms >= BOMB_SETTLE_MS {
                        bomb.phase = BombPhase::Ticking;
                        bomb.phase_elapsed_ms = 0;
                    }
                }
                BombPhase::Ticking => {
                    let below = (bomb.pos.0 + 1, bomb.pos.1);
                    if bomb_positions.contains(&below) || below == self.player.position() {
                        // 他のボムの真上、またはプレイヤーの頭上に来た場合は、地面に
                        // 着地した時と違いそこで静止せず、Settling同様に左右へバウンド
                        // しながら転がり続ける。その間は起爆カウントダウンも進めない。
                        let prev_ticks = bomb.phase_elapsed_ms / BOMB_SETTLE_TICK_MS;
                        bomb.phase_elapsed_ms = bomb.phase_elapsed_ms.saturating_add(delta_ms);
                        let new_ticks = bomb.phase_elapsed_ms / BOMB_SETTLE_TICK_MS;
                        for _ in 0..(new_ticks - prev_ticks) {
                            bomb_settle_step(
                                &self.board,
                                &mut bomb.pos,
                                &mut bomb.settle_bounce_dir,
                                &bomb_positions,
                                self.player.position(),
                            );
                        }
                    } else if below.0 < self.board.depth_rows()
                        && self.board.cell(below.0, below.1) == Cell::Empty
                    {
                        // 起爆カウントダウン中も支えを失っていれば落下を続ける。
                        // 落下中は`remaining_ms`を減らさない(空中で起爆させないため)。
                        bomb.pos = below;
                        bomb.phase_elapsed_ms = 0;
                    } else {
                        bomb.phase_elapsed_ms = 0;
                        let remaining_before = bomb.remaining_ms;
                        bomb.remaining_ms = bomb.remaining_ms.saturating_sub(delta_ms);
                        // 残り時間が`BOMB_DANGER_MS`を初めて下回った瞬間(本体が赤く
                        // 点滅し始めるのと同じタイミング)に1回だけ導火線SEを鳴らす。
                        if remaining_before > BOMB_DANGER_MS && bomb.remaining_ms <= BOMB_DANGER_MS
                        {
                            events.push(GameEvent::BombFuseWarning);
                        }
                        // 危険域に入っている間、`BOMB_FUSE_TICK_INTERVAL_MS`ごとの
                        // 境界を跨いだ瞬間に繰り返し「チッ」を鳴らす。爆発する瞬間
                        // (remaining_ms==0)は爆発音と重ならないよう対象外にする。
                        if bomb.remaining_ms > 0
                            && bomb.remaining_ms <= BOMB_DANGER_MS
                            && remaining_before / BOMB_FUSE_TICK_INTERVAL_MS
                                != bomb.remaining_ms / BOMB_FUSE_TICK_INTERVAL_MS
                        {
                            events.push(GameEvent::BombFuseTick);
                        }
                        if bomb.remaining_ms == 0 {
                            exploded.push(i);
                        }
                    }
                }
            }
        }
        if !exploded.is_empty() {
            // 先に起爆確定分を全てまとめて取り出してから渡す。同じtickで複数のボムが
            // 同時に起爆完了した場合、1個ずつ`self.bombs.remove(i)`していくと、後続の
            // 誘爆が既に取り出したインデックスとぶつかって壊れるため。
            let caught: Vec<Bomb> = exploded
                .iter()
                .rev()
                .map(|&i| self.bombs.remove(i))
                .collect();
            self.detonate_bombs(caught, events, true);
            if self.status != GameStatus::Playing {
                return false;
            }
        }

        self.bomb_spawn_check_accum_ms += delta.as_millis() as u64;
        while self.bomb_spawn_check_accum_ms >= BOMB_SPAWN_CHECK_INTERVAL_MS {
            self.bomb_spawn_check_accum_ms -= BOMB_SPAWN_CHECK_INTERVAL_MS;
            self.maybe_spawn_bomb();
        }
        true
    }
}

/// `BombPhase::Settling`中の1歩ぶんの移動。支えを失っていれば1マス落下し、支持されて
/// いれば現在の`bounce_dir`(+1=右、-1=左)方向へ1マス移動を試みる。移動先が壁・既存
/// ブロック・他のボム・プレイヤーの現在地で塞がっていれば方向を反転する。
/// `other_bomb_positions`と`player_pos`はいずれもCellグリッドとは別のオーバーレイ/
/// エンティティで、盤面のセルだけを見ていると重なって落下・移動してしまうため渡す。
fn bomb_settle_step(
    board: &Board,
    pos: &mut board::Pos,
    bounce_dir: &mut i8,
    other_bomb_positions: &[board::Pos],
    player_pos: board::Pos,
) {
    let below = (pos.0 + 1, pos.1);
    if below.0 < board.depth_rows()
        && board.cell(below.0, below.1) == Cell::Empty
        && !other_bomb_positions.contains(&below)
        && below != player_pos
    {
        *pos = below;
        return;
    }

    let next_col = pos.1 as isize + *bounce_dir as isize;
    if next_col >= 0
        && (next_col as usize) < board.width()
        && board.cell(pos.0, next_col as usize) == Cell::Empty
        && !other_bomb_positions.contains(&(pos.0, next_col as usize))
        && (pos.0, next_col as usize) != player_pos
    {
        pos.1 = next_col as usize;
    } else {
        *bounce_dir = -*bounce_dir;
    }
}

/// 着地予定マス`pos`が塞がっていれば(非Emptyセル/プレイヤー/他のボム)、同じ列を上方向に
/// 走査して最初の空きマスを返す。空いていれば`None`(変更不要)。列の最上段まで空きが
/// 見つからなければ`None`(その場合は現状の挙動のまま何もしない)。
///
/// Entering/Rolling中のボムは、まだCellグリッド上に何の予約も残さない(#240の`solid`
/// オーバーレイの対象外)。そのためspawn時点では空いていた`pos`へ、演出中に別のブロックが
/// 落下してきたり、プレイヤーが歩いてきたりすると重なって見える(#241)。この関数を
/// Entering/Rollingの各フレームで呼び、塞がれた瞬間に上へ逃がすことで重なりを防ぐ。
fn bomb_landing_pos_adjusted_upward(
    board: &Board,
    pos: board::Pos,
    self_index: usize,
    bomb_positions: &[board::Pos],
    player_pos: board::Pos,
) -> Option<board::Pos> {
    let is_free = |p: board::Pos| {
        board.cell(p.0, p.1) == Cell::Empty
            && p != player_pos
            && !bomb_positions
                .iter()
                .enumerate()
                .any(|(j, q)| j != self_index && *q == p)
    };
    if is_free(pos) {
        return None;
    }
    (0..pos.0).rev().map(|r| (r, pos.1)).find(|&p| is_free(p))
}

#[cfg(test)]
mod tests {
    use super::super::tests::clear_board;
    use super::*;
    use crate::constants::{FRAME_INTERVAL_MS, SHAKE_TICKS};

    #[test]
    fn pushing_into_a_resting_bomb_rolls_it_further_in_the_move_direction() {
        // 静止中のボムは、プレイヤーが押した方向へ1マス転がる。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 足場(横移動には支持が必要)
        game.bombs.push(Bomb {
            pos: (500, 6),
            origin: (500, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        game.try_move_right();

        assert_eq!(
            game.player.col, 6,
            "ボムが押し出された先(元のボムの位置)へ移動できるはず"
        );
        assert_eq!(
            game.bombs[0].pos,
            (500, 7),
            "ボムはさらに進行方向へ1マス押し出されるはず"
        );
        assert_eq!(
            game.bombs[0].phase,
            BombPhase::Settling,
            "押し出されたボムはSettling(左右バウンド中)へ遷移するはず"
        );
        assert_eq!(
            game.bombs[0].settle_bounce_dir, 1,
            "押した方向(右)へバウンドする向きになっているはず"
        );
    }

    #[test]
    fn player_is_grounded_returns_true_when_a_settled_bomb_rests_below() {
        // ボムはCellグリッド外のオーバーレイなので、盤面だけ見るとEmptyのまま=支持なし
        // に見えてしまう。設置済み(Settling/Ticking)のボムは支えとして扱う。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.bombs.push(Bomb {
            pos: (501, 5),
            origin: (501, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        assert!(
            game.player_is_grounded(),
            "直下に設置済みのボムがあれば支持されているとみなすはず"
        );
    }

    #[test]
    fn free_fall_does_not_drop_the_player_onto_a_settled_bomb() {
        // 自由落下がボムの存在を無視して直下のEmptyマスへ落ち、プレイヤーとボムが
        // 同じマスに重なって見えるバグの回帰テスト。
        let mut game = Game::new(1);
        clear_board(&mut game);
        let bottom = game.board.depth_rows() - 1;
        game.player.row = bottom - 2;
        game.player.col = 5;
        // ボム自身の足場は盤面最深行に置く(それ以外の行に浮かせたRockは支えが
        // 無く自重力で落下してしまい、テストの前提が崩れるため)。
        game.board.rows[bottom][5] = Cell::Rock { hits: 0 };
        game.bombs.push(Bomb {
            pos: (bottom - 1, 5),
            origin: (bottom - 1, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        for _ in 0..20 {
            game.update(Duration::from_millis(FRAME_INTERVAL_MS));
        }

        assert_eq!(
            game.player.row,
            bottom - 2,
            "設置済みのボムがあるマスへ自由落下してはいけない(重なって見えるバグ)"
        );
    }

    #[test]
    fn free_fall_still_falls_through_a_bomb_that_has_not_settled_yet() {
        // Entering/Rolling段階のボムはまだ登場・投擲演出中で実体を持たないため、支えには
        // ならず通過できるはず(支え判定がSettling/Ticking以外まで及んでいないことの確認)。
        let mut game = Game::new(1);
        clear_board(&mut game);
        let bottom = game.board.depth_rows() - 1;
        game.player.row = bottom - 2;
        game.player.col = 5;
        // ボム自身の足場は盤面最深行に置く(それ以外の行に浮かせたRockは支えが
        // 無く自重力で落下してしまい、テストの前提が崩れるため)。
        game.board.rows[bottom][5] = Cell::Rock { hits: 0 };
        game.bombs.push(Bomb {
            pos: (bottom - 1, 5),
            origin: (bottom - 1, 0),
            phase: BombPhase::Entering,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        for _ in 0..20 {
            game.update(Duration::from_millis(FRAME_INTERVAL_MS));
        }

        assert_eq!(
            game.player.row,
            bottom - 1,
            "登場演出中のボムは支えにならず、プレイヤーはそのマスへ落下できるはず"
        );
    }

    #[test]
    fn falling_blocks_do_not_land_on_a_settled_bomb() {
        // 落下ブロックがボムの存在を無視して直下のEmptyマスへ落ち、ブロックとボムが
        // 同じマスに重なって見えるバグの回帰テスト(#240)。プレイヤー版(#238)と同じ
        // 構造の別インスタンスで、全種別で再現していた。
        let kinds = [
            Cell::Color(ColorKind::Red),
            Cell::Rock { hits: 0 },
            Cell::Diamond,
            Cell::Star { visible_ms: 0 },
            Cell::Item(ItemEffect::ClearAbove),
            Cell::Oxygen,
        ];

        for kind in kinds {
            let mut game = Game::new(1);
            clear_board(&mut game);
            let bottom = game.board.depth_rows() - 1;
            // ボム自身の足場は盤面最深行に置く(それ以外の行に浮かせたRockは支えが
            // 無く自重力で落下してしまい、テストの前提が崩れるため)。
            game.board.rows[bottom][5] = Cell::Rock { hits: 0 };
            game.bombs.push(Bomb {
                pos: (bottom - 1, 5),
                origin: (bottom - 1, 0),
                phase: BombPhase::Ticking,
                phase_elapsed_ms: 0,
                remaining_ms: BOMB_FUSE_MS,
                settle_bounce_dir: 1,
            });
            game.board.rows[bottom - 2][5] = kind; // ボムの真上、盤面上の支えは無い

            for _ in 0..90 {
                game.update(Duration::from_millis(FRAME_INTERVAL_MS));
            }

            assert_eq!(
                game.board.cell(bottom - 1, 5),
                Cell::Empty,
                "{kind:?}: ボムのマスへブロックが落下して重なっている"
            );
            assert_eq!(
                game.board.cell(bottom - 2, 5),
                kind,
                "{kind:?}: 設置済みのボムに支えられて元の位置に残るはず"
            );
        }
    }

    #[test]
    fn falling_blocks_still_fall_through_a_bomb_that_has_not_settled_yet() {
        // Entering/Rolling段階のボムはまだ登場・投擲演出中で実体を持たないため、
        // ブロックの支えにもならず通過できるはず(プレイヤー版の対称形)。
        for phase in [BombPhase::Entering, BombPhase::Rolling] {
            let mut game = Game::new(1);
            clear_board(&mut game);
            let bottom = game.board.depth_rows() - 1;
            game.board.rows[bottom][5] = Cell::Rock { hits: 0 };
            game.bombs.push(Bomb {
                pos: (bottom - 1, 5),
                origin: (bottom - 1, 0),
                phase,
                phase_elapsed_ms: 0,
                remaining_ms: BOMB_FUSE_MS,
                settle_bounce_dir: 1,
            });
            game.board.rows[bottom - 2][5] = Cell::Color(ColorKind::Red);

            for _ in 0..90 {
                game.update(Duration::from_millis(FRAME_INTERVAL_MS));
            }

            assert_eq!(
                game.board.cell(bottom - 2, 5),
                Cell::Empty,
                "{phase:?}: 演出中のボムは支えにならず、ブロックは落下するはず"
            );
        }
    }

    #[test]
    fn settled_bomb_positions_lists_only_settling_and_ticking_bombs() {
        // 固体オーバーレイの対象phaseが`settled_bomb_at`と揃っていることの単体確認。
        let mut game = Game::new(1);
        clear_board(&mut game);
        let phases = [
            BombPhase::Entering,
            BombPhase::Rolling,
            BombPhase::Settling,
            BombPhase::Ticking,
        ];
        for (i, phase) in phases.into_iter().enumerate() {
            game.bombs.push(Bomb {
                pos: (10, i),
                origin: (10, 0),
                phase,
                phase_elapsed_ms: 0,
                remaining_ms: BOMB_FUSE_MS,
                settle_bounce_dir: 1,
            });
        }

        let solid = game.settled_bomb_positions();

        assert_eq!(
            solid,
            vec![(10, 2), (10, 3)],
            "Settling/Tickingのボムだけを固体として扱うはず(Entering/Rollingは含めない)"
        );
    }

    #[test]
    fn a_block_falling_into_a_rolling_bombs_cell_pushes_the_landing_cell_up() {
        // Entering/Rolling段階のボムはまだ着地予定マスへ何の予約も残さないため、演出中に
        // 真上から落下してきたブロックと重なって見えるバグの回帰テスト(#241)。
        let mut game = Game::new(1);
        clear_board(&mut game);
        let bottom = game.board.depth_rows() - 1;
        // 床と両脇の壁を作り、ボムが横に逃げられない部屋にする。
        game.board.rows[bottom][4] = Cell::Rock { hits: 0 };
        game.board.rows[bottom][5] = Cell::Rock { hits: 0 };
        game.board.rows[bottom][6] = Cell::Rock { hits: 0 };
        game.board.rows[bottom - 1][4] = Cell::Rock { hits: 0 };
        game.board.rows[bottom - 1][6] = Cell::Rock { hits: 0 };
        game.bombs.push(Bomb {
            pos: (bottom - 1, 5),
            origin: (bottom - 1, 0),
            phase: BombPhase::Entering,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });
        game.board.rows[bottom - 2][5] = Cell::Color(ColorKind::Red); // ボムの真上、未支持

        for frame in 0..90 {
            game.update(Duration::from_millis(FRAME_INTERVAL_MS));
            let bomb_pos = game.bombs[0].pos;
            assert_eq!(
                game.board.cell(bomb_pos.0, bomb_pos.1),
                Cell::Empty,
                "frame {frame}: ボムの着地予定マスへブロックが落下して重なっている"
            );
        }
    }

    #[test]
    fn a_bomb_never_settles_on_the_players_cell() {
        // Entering中のボムがプレイヤーの現在マスと同じ位置に出現した場合でも、重ならず
        // 上へ逃げ続けるはず(#241)。
        let mut game = Game::new(1);
        clear_board(&mut game);
        let row = 10;
        let col = 5;
        game.player.row = row;
        game.player.col = col;
        game.board.rows[row + 1][col] = Cell::Rock { hits: 0 }; // プレイヤーの足場
        game.bombs.push(Bomb {
            pos: (row, col),
            origin: (row, 0),
            phase: BombPhase::Entering,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        for frame in 0..90 {
            game.update(Duration::from_millis(FRAME_INTERVAL_MS));
            assert_ne!(
                game.bombs[0].pos,
                game.player.position(),
                "frame {frame}: ボムがプレイヤーのマスに重なっている"
            );
        }
    }

    #[test]
    fn bomb_landing_pos_adjusted_upward_returns_none_when_pos_itself_is_free() {
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 0;
        game.player.col = 0; // posとは無関係な位置に置く
        let pos = (5, 5);

        let result =
            bomb_landing_pos_adjusted_upward(&game.board, pos, 0, &[pos], game.player.position());

        assert_eq!(result, None, "何にも塞がれていなければ変更不要のはず");
    }

    #[test]
    fn bomb_landing_pos_adjusted_upward_moves_up_when_pos_matches_the_player() {
        let mut game = Game::new(1);
        clear_board(&mut game);
        let pos = (5, 5);
        game.player.row = pos.0;
        game.player.col = pos.1;

        let result =
            bomb_landing_pos_adjusted_upward(&game.board, pos, 0, &[pos], game.player.position());

        assert_eq!(
            result,
            Some((4, 5)),
            "プレイヤーと重なっていれば直上の空きマスへずらすはず"
        );
    }

    #[test]
    fn bomb_landing_pos_adjusted_upward_moves_up_when_pos_matches_another_bomb() {
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 0;
        game.player.col = 0; // posとは無関係な位置に置く
        let pos = (5, 5);
        // self_index=0がpos自身、index1に同じマスの別ボムがいる状態を模す。
        let bomb_positions = [pos, pos];

        let result = bomb_landing_pos_adjusted_upward(
            &game.board,
            pos,
            0,
            &bomb_positions,
            game.player.position(),
        );

        assert_eq!(
            result,
            Some((4, 5)),
            "他のボムと重なっていれば直上の空きマスへずらすはず"
        );
    }

    #[test]
    fn bomb_landing_pos_adjusted_upward_ignores_self_index_when_checking_bomb_overlap() {
        // self_indexで指定した自分自身の現在位置を、他ボムとして塞がっていると
        // 誤認してはいけない。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 0;
        game.player.col = 0; // posとは無関係な位置に置く
        let pos = (5, 5);
        let bomb_positions = [(1, 1), (2, 2), pos]; // self_index=2がpos自身

        let result = bomb_landing_pos_adjusted_upward(
            &game.board,
            pos,
            2,
            &bomb_positions,
            game.player.position(),
        );

        assert_eq!(
            result, None,
            "自分自身の現在位置を他ボムとして誤検知してはいけない"
        );
    }

    #[test]
    fn bomb_landing_pos_adjusted_upward_returns_none_when_the_whole_column_above_is_blocked() {
        let mut game = Game::new(1);
        clear_board(&mut game);
        let pos = (5, 5);
        game.player.row = pos.0;
        game.player.col = pos.1; // posを塞ぐ
        for r in 0..pos.0 {
            game.board.rows[r][5] = Cell::Rock { hits: 0 }; // 列の上方向を全て塞ぐ
        }

        let result =
            bomb_landing_pos_adjusted_upward(&game.board, pos, 0, &[pos], game.player.position());

        assert_eq!(
            result, None,
            "列の最上段まで塞がっていれば変更不要(現状維持)のはず"
        );
    }

    #[test]
    fn pushing_a_bomb_against_a_wall_blocks_the_move() {
        // 押し出し先が塞がっていれば、壁にぶつかった時と同じくその場に留まるはず。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 足場
        game.board.rows[500][7] = Cell::Rock { hits: 0 }; // 押し出し先を塞ぐ壁
        game.bombs.push(Bomb {
            pos: (500, 6),
            origin: (500, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        game.try_move_right();

        assert_eq!(game.player.col, 5, "押し出せないので移動しないはず");
        assert_eq!(game.bombs[0].pos, (500, 6), "ボムも動かないはず");
        assert_eq!(
            game.bombs[0].phase,
            BombPhase::Ticking,
            "押し出せなかったボムの段階は変わらないはず"
        );
    }

    #[test]
    fn pushing_a_bomb_into_another_bomb_blocks_the_move() {
        // 押し出し先に既に他のボムが居座っていれば、同じく移動を妨げるはず
        // (ボム同士は重ならないため)。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 足場
        game.bombs.push(Bomb {
            pos: (500, 6),
            origin: (500, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });
        game.bombs.push(Bomb {
            pos: (500, 7),
            origin: (500, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        game.try_move_right();

        assert_eq!(game.player.col, 5, "押し出せないので移動しないはず");
        assert_eq!(game.bombs[0].pos, (500, 6), "手前のボムも動かないはず");
    }

    #[test]
    fn walking_toward_a_bomb_still_entering_does_not_push_it() {
        // 登場・投擲演出中(Entering/Rolling)のボムはまだ「静止」していないため、
        // 押し出しの対象外。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 足場
        game.bombs.push(Bomb {
            pos: (500, 6),
            origin: (500, 0),
            phase: BombPhase::Entering,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        game.try_move_right();

        assert_eq!(
            game.player.col, 6,
            "Enteringのボムは押し出し判定の対象外で、通常通り移動できるはず"
        );
        assert_eq!(
            game.bombs[0].pos,
            (500, 6),
            "Enteringのボムの位置は変わらないはず"
        );
    }

    #[test]
    fn pressing_toward_an_unpushable_bomb_twice_climbs_over_it_on_the_second_press() {
        // 押し出せないボムは岩ブロックと同じく、1回目はぶつかって停止するだけで、
        // 同じ方向への2回目の入力で初めて1段登る。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 足場
        game.board.rows[500][7] = Cell::Rock { hits: 0 }; // 押し出し先を塞ぐ壁
        game.bombs.push(Bomb {
            pos: (500, 6),
            origin: (500, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        let first_events = game.try_move_right(); // 1回目: ぶつかって停止

        assert_eq!(game.player.row, 500, "まだ登っていない");
        assert_eq!(game.player.col, 5, "まだ移動していない");
        assert_eq!(game.player.facing, Direction::Right);
        assert!(first_events.is_empty());

        game.move_cooldown_accum = Duration::from_millis(INPUT_COOLDOWN_MS);
        let second_events = game.try_move_right(); // 2回目: 同じ方向への再入力で登る

        assert_eq!(game.player.row, 499, "1段登った");
        assert_eq!(game.player.col, 6);
        assert_eq!(game.player.facing, Direction::Right);
        assert!(
            second_events.is_empty(),
            "掘削・破壊イベントは一切発生しない"
        );
        assert_eq!(
            game.bombs[0].pos,
            (500, 6),
            "ボムは押し出されず、その場に残ったまま"
        );
    }

    #[test]
    fn climbing_over_a_bomb_fails_when_the_players_own_head_is_blocked() {
        // 頭上(自分の真上)が塞がっていれば、岩ブロックと同じくボムでも登れないはず。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 足場
        game.board.rows[500][7] = Cell::Rock { hits: 0 }; // 押し出し先を塞ぐ壁
        game.board.rows[499][5] = Cell::Rock { hits: 0 }; // 自分の頭上を塞ぐ
        game.bombs.push(Bomb {
            pos: (500, 6),
            origin: (500, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        game.try_move_right();
        game.move_cooldown_accum = Duration::from_millis(INPUT_COOLDOWN_MS);
        game.try_move_right();

        assert_eq!(game.player.row, 500, "頭上が塞がっているので登れないはず");
        assert_eq!(game.player.col, 5, "移動しないはず");
        assert_eq!(game.bombs[0].pos, (500, 6), "ボムも動かないはず");
    }

    #[test]
    fn climbing_over_a_bomb_fails_when_the_landing_cell_has_another_bomb() {
        // 登った先(1段上)に他のボムが居座っていれば、Cellのブロックで塞がっている
        // 場合と同じく登れないはず。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 足場
        game.board.rows[500][7] = Cell::Rock { hits: 0 }; // 押し出し先を塞ぐ壁
        game.bombs.push(Bomb {
            pos: (500, 6),
            origin: (500, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });
        game.bombs.push(Bomb {
            pos: (499, 6), // 登った先を塞ぐボム
            origin: (499, 0),
            phase: BombPhase::Settling,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        game.try_move_right();
        game.move_cooldown_accum = Duration::from_millis(INPUT_COOLDOWN_MS);
        game.try_move_right();

        assert_eq!(
            game.player.row, 500,
            "登った先がボムで塞がっているので登れないはず"
        );
        assert_eq!(game.player.col, 5, "移動しないはず");
    }

    #[test]
    fn climbing_over_a_bomb_collects_an_overhead_item_and_a_capsule_on_the_landing_cell() {
        // ボム越えの登りも`move_lateral`と同じく、頭上・登り先のAIR・アイテムに
        // 妨げられず、通過しながら取得する(#244)。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.player.oxygen = 40.0;
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 足場
        game.board.rows[500][7] = Cell::Rock { hits: 0 }; // 押し出し先を塞ぐ壁
        game.board.rows[499][5] = Cell::Item(ItemEffect::ClearAbove); // 自分の真上
        game.board.rows[499][6] = Cell::Oxygen; // 登り先
        game.bombs.push(Bomb {
            pos: (500, 6),
            origin: (500, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        game.try_move_right(); // 1回目: ぶつかって停止
        game.move_cooldown_accum = Duration::from_millis(INPUT_COOLDOWN_MS);
        let events = game.try_move_right(); // 2回目: 取得しながら登る

        assert_eq!(game.player.row, 499, "1段登った");
        assert_eq!(game.player.col, 6);
        assert_eq!(game.board.cell(499, 5), Cell::Empty); // アイテムは消費された
        assert_eq!(game.board.cell(499, 6), Cell::Empty); // カプセルも消費された
        assert_eq!(
            game.player.oxygen,
            40.0 + crate::constants::OXYGEN_CAPSULE_RESTORE
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::OxygenCollected))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::ItemCollected(ItemEffect::ClearAbove)))
        );
        assert_eq!(game.bombs[0].pos, (500, 6), "ボムは押し出されず残ったまま");
    }

    #[test]
    fn climbing_over_a_bomb_leaves_the_overhead_item_when_the_landing_cell_is_blocked() {
        // 登り先が塞がっていて登れない場合、頭上のアイテムも取得せず残る(#244)。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 足場
        game.board.rows[500][7] = Cell::Rock { hits: 0 }; // 押し出し先を塞ぐ壁
        game.board.rows[499][5] = Cell::Item(ItemEffect::ClearAbove); // 自分の真上
        game.board.rows[499][6] = Cell::Rock { hits: 0 }; // 登り先を塞ぐ
        game.bombs.push(Bomb {
            pos: (500, 6),
            origin: (500, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        game.try_move_right(); // 1回目
        game.move_cooldown_accum = Duration::from_millis(INPUT_COOLDOWN_MS);
        let events = game.try_move_right(); // 2回目でも登れない

        assert_eq!(game.player.row, 500, "登れていないはず");
        assert_eq!(game.player.col, 5, "移動しないはず");
        assert_eq!(
            game.board.cell(499, 5),
            Cell::Item(ItemEffect::ClearAbove),
            "登れていないので頭上のアイテムは取得されない"
        );
        assert!(events.is_empty());
    }

    #[test]
    fn drilling_a_settled_bomb_removes_it_without_triggering_an_explosion() {
        // 静止中(Settling/Ticking)のボムは掘削で除去できる。爆発は誘発せず、
        // 通常のブロック消滅と同じ`BlockDestroyed`のみ発生する。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.player.facing = Direction::Right;
        game.bombs.push(Bomb {
            pos: (500, 6),
            origin: (500, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        let events = game.try_drill();

        assert!(game.bombs.is_empty(), "掘削したボムは除去されるはず");
        assert!(
            events.contains(&GameEvent::BlockDestroyed { blocks: 1 }),
            "通常のブロック消滅と同じイベントが発生するはず: {events:?}"
        );
        assert!(
            !events.contains(&GameEvent::BombExploded),
            "掘削除去では爆発は誘発しないはず: {events:?}"
        );
    }

    #[test]
    fn drilling_toward_a_bomb_still_entering_does_not_destroy_it() {
        // まだ登場・投擲演出中(Entering/Rolling)のボムは掘削の対象外
        // (「静止していないと干渉しない」ルールは押し出しと同じ)。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.player.facing = Direction::Right;
        game.bombs.push(Bomb {
            pos: (500, 6),
            origin: (500, 0),
            phase: BombPhase::Entering,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        let events = game.try_drill();

        assert_eq!(
            game.bombs.len(),
            1,
            "Enteringのボムは掘削の対象外で残るはず"
        );
        assert_eq!(game.bombs[0].pos, (500, 6));
        assert!(
            !events.contains(&GameEvent::BlockDestroyed { blocks: 1 }),
            "掘削は素通りの空振りになるはず: {events:?}"
        );
    }

    #[test]
    fn debug_place_bomb_spawns_at_the_only_empty_cell_within_visible_range() {
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        // 画面内(±STAR_VISIBLE_RANGE_ROWS)を全て岩で埋め、1マスだけEmptyにすることで
        // デバッグ配置先を一意に絞り込む。
        let range = crate::constants::STAR_VISIBLE_RANGE_ROWS;
        for row in (game.player.row - range)..=(game.player.row + range) {
            for col in 0..game.board.width() {
                game.board.rows[row][col] = Cell::Rock { hits: 0 };
            }
        }
        game.board.rows[510][7] = Cell::Empty;

        game.debug_place_bomb();

        assert_eq!(
            game.bombs.len(),
            1,
            "候補が1マスしかないのでボムが1個設置されるはず"
        );
        assert_eq!(game.bombs[0].pos, (510, 7));
        assert_eq!(game.bombs[0].remaining_ms, BOMB_FUSE_MS);
        assert_eq!(
            game.bombs[0].phase,
            BombPhase::Entering,
            "白ボンが登場する段階から始まるはず"
        );
        assert_eq!(
            game.bombs[0].origin.0, 510,
            "登場位置は最終設置マスと同じ行のはず"
        );
        assert!(
            game.bombs[0].origin.1 == 0 || game.bombs[0].origin.1 == game.board.width() - 1,
            "登場位置は画面の左端か右端のはず: {:?}",
            game.bombs[0].origin
        );
    }

    #[test]
    fn debug_place_bomb_uses_configured_bomb_fuse_ms() {
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        // 画面内(±STAR_VISIBLE_RANGE_ROWS)を全て岩で埋め、1マスだけEmptyにすることで
        // デバッグ配置先を一意に絞り込む。
        let range = crate::constants::STAR_VISIBLE_RANGE_ROWS;
        for row in (game.player.row - range)..=(game.player.row + range) {
            for col in 0..game.board.width() {
                game.board.rows[row][col] = Cell::Rock { hits: 0 };
            }
        }
        game.board.rows[510][7] = Cell::Empty;

        game.set_bomb_fuse_ms(2000);
        game.debug_place_bomb();

        assert_eq!(
            game.bombs.len(),
            1,
            "候補が1マスしかないのでボムが1個設置されるはず"
        );
        assert_eq!(
            game.bombs[0].remaining_ms, 2000,
            "set_bomb_fuse_msで指定した値が新規ボムの残り時間に反映されるはず"
        );
    }

    #[test]
    fn bomb_fuse_ms_constants_are_sane() {
        // 定数同士の比較のみでコンパイル時に値が確定するため、clippyの提案通り
        // `const`ブロックで包み`assertions_on_constants`を回避する。
        const {
            assert!(
                crate::constants::BOMB_FUSE_MS_MIN > BOMB_DANGER_MS,
                "爆発までの時間の下限は危険域突入の閾値(BOMB_DANGER_MS)より大きくなければならない"
            );
            assert!(
                crate::constants::BOMB_FUSE_MS_MIN <= BOMB_FUSE_MS
                    && BOMB_FUSE_MS <= crate::constants::BOMB_FUSE_MS_MAX,
                "既定値は設定可能範囲内でなければならない"
            );
            assert!(
                (BOMB_FUSE_MS - crate::constants::BOMB_FUSE_MS_MIN)
                    .is_multiple_of(crate::constants::BOMB_FUSE_MS_STEP),
                "既定値は下限からSTEP刻みの倍数になっているはず"
            );
        }
    }

    #[test]
    fn bomb_advances_through_entering_and_rolling_before_ticking_down_the_fuse() {
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        // ボムを盤面の最深行に置く(ボムにも落下判定があるため支えが必要)。Rockで床を
        // 作ると、このテストの経過時間では床自体の揺れ猶予が明けて落下してしまうため、
        // それ自体が常に支持される最深行を使う。
        let bomb_row = FIELD_DEPTH_M - 1;
        game.bombs.push(Bomb {
            pos: (bomb_row, 5),
            origin: (bomb_row, 0),
            phase: BombPhase::Entering,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        // Entering段階の途中では、まだRollingへ進まないはず。
        game.update(Duration::from_millis(BOMB_ENTER_MS as u64 / 2));
        assert_eq!(game.bombs[0].phase, BombPhase::Entering);
        assert_eq!(
            game.bombs[0].remaining_ms, BOMB_FUSE_MS,
            "Entering中は起爆カウントダウンが始まらないはず"
        );

        // Enteringを終えるとRollingへ進む。
        game.update(Duration::from_millis(BOMB_ENTER_MS as u64));
        assert_eq!(game.bombs[0].phase, BombPhase::Rolling);
        assert_eq!(
            game.bombs[0].remaining_ms, BOMB_FUSE_MS,
            "Rolling中も起爆カウントダウンが始まらないはず"
        );

        // Rollingを終えるとSettling(左右に跳ねて落ち着き先を探す段階)へ進む。
        game.update(Duration::from_millis(BOMB_ROLL_MS as u64));
        assert_eq!(game.bombs[0].phase, BombPhase::Settling);
        assert_eq!(
            game.bombs[0].remaining_ms, BOMB_FUSE_MS,
            "Settling中も起爆カウントダウンが始まらないはず"
        );

        // Settlingを終えるとTickingへ進み、そこで初めて起爆カウントダウンが始まる。
        game.update(Duration::from_millis(BOMB_SETTLE_MS as u64));
        assert_eq!(game.bombs[0].phase, BombPhase::Ticking);
        game.update(Duration::from_millis(100));
        assert_eq!(game.bombs[0].remaining_ms, BOMB_FUSE_MS - 100);
    }

    #[test]
    fn bomb_fuse_warning_fires_exactly_once_when_remaining_time_crosses_the_danger_threshold() {
        // 本体が激しく赤く点滅し始める瞬間(残り時間がBOMB_DANGER_MSを初めて下回った
        // 瞬間)にBombFuseWarningを1回だけ発火する。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        let bomb_row = FIELD_DEPTH_M - 1;
        game.bombs.push(Bomb {
            pos: (bomb_row, 5),
            origin: (bomb_row, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_DANGER_MS + 50,
            settle_bounce_dir: 1,
        });

        // まだ閾値を上回っている間は発火しないはず。
        let events = game.update(Duration::from_millis(20));
        assert!(!events.contains(&GameEvent::BombFuseWarning));

        // 閾値をまたぐ1ティックで1回だけ発火するはず。
        let events = game.update(Duration::from_millis(40));
        assert_eq!(
            events
                .iter()
                .filter(|e| **e == GameEvent::BombFuseWarning)
                .count(),
            1,
            "閾値をまたいだ瞬間に1回だけ発火するはず: {events:?}"
        );

        // 閾値を下回ったまま経過しても再発火しないはず。
        let events = game.update(Duration::from_millis(100));
        assert!(
            !events.contains(&GameEvent::BombFuseWarning),
            "既に閾値未満なら再発火しないはず: {events:?}"
        );
    }

    #[test]
    fn bomb_fuse_tick_fires_repeatedly_at_fixed_intervals_while_in_the_danger_zone() {
        // 危険域(残りBOMB_DANGER_MS以下)に入っている間、BOMB_FUSE_TICK_INTERVAL_MS
        // ごとに繰り返しBombFuseTickが発生するはず。
        let mut game = Game::new(2);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        let bomb_row = FIELD_DEPTH_M - 1;
        game.bombs.push(Bomb {
            pos: (bomb_row, 5),
            origin: (bomb_row, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_DANGER_MS + 50,
            settle_bounce_dir: 1,
        });

        // 危険域に入るまではまだ発火しないはず。
        let events = game.update(Duration::from_millis(20));
        assert!(!events.contains(&GameEvent::BombFuseTick));

        // 危険域に入った後は、爆発するまでの間にBOMB_FUSE_TICK_INTERVAL_MSごとに
        // 繰り返し発火するはず(単発のBombFuseWarningとは異なり複数回鳴る)。
        let mut tick_count = 0;
        while !game.bombs.is_empty() {
            let events = game.update(Duration::from_millis(20));
            tick_count += events
                .iter()
                .filter(|e| **e == GameEvent::BombFuseTick)
                .count();
        }
        assert!(
            tick_count >= 2,
            "危険域の間にチッが複数回繰り返し鳴るはず: {tick_count}回"
        );
    }

    #[test]
    fn bomb_fuse_tick_does_not_fire_on_the_same_tick_the_bomb_explodes() {
        // 爆発音(play_bomb_explosion)と重ならないよう、remaining_msがちょうど0になる
        // 瞬間(=爆発する瞬間)はBombFuseTickの対象外にするはず。
        let mut game = Game::new(3);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        let bomb_row = FIELD_DEPTH_M - 1;
        game.bombs.push(Bomb {
            pos: (bomb_row, 5),
            origin: (bomb_row, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 30,
            settle_bounce_dir: 1,
        });

        let events = game.update(Duration::from_millis(30));
        assert!(events.contains(&GameEvent::BombExploded));
        assert!(
            !events.contains(&GameEvent::BombFuseTick),
            "爆発した瞬間はBombFuseTickを鳴らさないはず: {events:?}"
        );
    }

    #[test]
    fn debug_place_bomb_does_nothing_while_not_playing() {
        let mut game = Game::new(1);
        game.status = GameStatus::Paused;

        game.debug_place_bomb();

        assert!(
            game.bombs.is_empty(),
            "Playing中以外ではボムを設置しないはず"
        );
    }

    #[test]
    fn debug_place_bomb_respects_the_board_wide_cap() {
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;

        for _ in 0..(BOMB_MAX_COUNT_ON_BOARD + 5) {
            game.debug_place_bomb();
        }

        assert_eq!(
            game.bombs.len(),
            BOMB_MAX_COUNT_ON_BOARD,
            "上限を超えてボムが設置されてはいけない"
        );
        // 値が戻ってしまう回帰を防ぐため、定数への参照だけでなく実際の値も固定する。
        assert_eq!(BOMB_MAX_COUNT_ON_BOARD, 10);
    }

    #[test]
    fn bomb_explosion_converts_rock_and_diamond_within_blast_range_to_star_and_destroys_items_but_leaves_air_untouched()
     {
        // 頭上一括クリア系の効果ではアイテムをAIRと同じ保護対象にしているが、その保護は
        // ボムの爆風には及ばない。AIR自体は爆風の影響を受けない。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5; // 爆風範囲外の位置
        game.bombs.push(Bomb {
            pos: (520, 5),
            origin: (520, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 50,
            settle_bounce_dir: 1,
        });
        game.board.rows[520][6] = Cell::Rock { hits: 0 };
        game.board.rows[521][5] = Cell::Diamond;
        game.board.rows[520][4] = Cell::Oxygen;
        game.board.rows[519][5] = Cell::Item(ItemEffect::ClearAbove);
        game.board.rows[520][8] = Cell::Item(ItemEffect::UnifyColors);
        game.board.rows[520][9] = Cell::Item(ItemEffect::StarifyScreen);

        let events = game.update(Duration::from_millis(60));

        assert!(game.bombs.is_empty(), "爆発したボムはリストから消えるはず");
        assert!(
            matches!(game.board.cell(520, 6), Cell::Star { .. }),
            "爆風内の岩はスターに変わるはず"
        );
        assert!(
            matches!(game.board.cell(521, 5), Cell::Star { .. }),
            "爆風内のダイヤはスターに変わるはず"
        );
        assert_eq!(
            game.board.cell(520, 4),
            Cell::Oxygen,
            "AIRは爆風の影響を受けないはず"
        );
        assert_eq!(
            game.board.cell(519, 5),
            Cell::Empty,
            "爆風内のRアイテムは破壊されて空になるはず"
        );
        assert_eq!(
            game.board.cell(520, 8),
            Cell::Empty,
            "爆風内のCアイテムは破壊されて空になるはず"
        );
        assert_eq!(
            game.board.cell(520, 9),
            Cell::Empty,
            "爆風内のKアイテムは破壊されて空になるはず"
        );
        assert!(events.contains(&GameEvent::BombExploded));
    }

    #[test]
    fn bomb_destroying_an_item_does_not_trigger_the_item_effect() {
        // 爆風でのアイテム破壊は「取得」ではないため、C/R/Kいずれの効果も発動させない。
        // 爆風範囲外に置いた各効果の痕跡確認用セルが元のままであることで確認する。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5; // 爆風範囲外の位置
        game.bombs.push(Bomb {
            pos: (520, 5),
            origin: (520, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 50,
            settle_bounce_dir: 1,
        });
        game.board.rows[521][5] = Cell::Rock { hits: 0 }; // ボムの支え(ボムにも落下判定があるため必要)
        // 爆風(ボムの行全体+ボムの列の上下)に入るアイテム3種
        game.board.rows[520][2] = Cell::Item(ItemEffect::ClearAbove);
        game.board.rows[520][8] = Cell::Item(ItemEffect::UnifyColors);
        game.board.rows[520][9] = Cell::Item(ItemEffect::StarifyScreen);
        // Rアイテム(頭上クリア)が発動したら消える位置の岩。同時に
        // Kアイテム(画面内スター化)が発動したらスターに変わる位置でもある。
        game.board.rows[490][8] = Cell::Rock { hits: 0 };
        // Kアイテム(画面内スター化)が発動したらスターに変わる位置のダイヤ
        game.board.rows[505][9] = Cell::Diamond;
        // Cアイテム(近傍色統一)が発動したら2色に塗り替えられる4色の色ブロック
        game.board.rows[495][10] = Cell::Color(ColorKind::Red);
        game.board.rows[495][11] = Cell::Color(ColorKind::Blue);
        game.board.rows[496][10] = Cell::Color(ColorKind::Green);
        game.board.rows[496][11] = Cell::Color(ColorKind::Yellow);

        let events = game.update(Duration::from_millis(60));

        assert_eq!(game.board.cell(520, 2), Cell::Empty);
        assert_eq!(game.board.cell(520, 8), Cell::Empty);
        assert_eq!(game.board.cell(520, 9), Cell::Empty);
        assert_eq!(
            game.board.cell(490, 8),
            Cell::Rock { hits: 0 },
            "Rアイテムの頭上クリアもKアイテムのスター化も発動していないはず"
        );
        assert_eq!(
            game.board.cell(505, 9),
            Cell::Diamond,
            "Kアイテムの画面内スター化は発動していないはず"
        );
        assert_eq!(game.board.cell(495, 10), Cell::Color(ColorKind::Red));
        assert_eq!(game.board.cell(495, 11), Cell::Color(ColorKind::Blue));
        assert_eq!(game.board.cell(496, 10), Cell::Color(ColorKind::Green));
        assert_eq!(
            game.board.cell(496, 11),
            Cell::Color(ColorKind::Yellow),
            "Cアイテムの色統一は発動していないはず(発動していれば4色は2色に減る)"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::ItemCollected(_))),
            "破壊は取得ではないのでItemCollectedは発生しないはず"
        );
    }

    #[test]
    fn bomb_destroying_an_item_does_not_change_the_score() {
        // ボムは現状スコアを一切生まない(岩のスター化・色統一・その後の4連結自動消滅
        // すら加点しない)ため、アイテム破壊も加点・減点しない。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5; // 爆風範囲外の位置
        game.bombs.push(Bomb {
            pos: (520, 5),
            origin: (520, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 50,
            settle_bounce_dir: 1,
        });
        game.board.rows[521][5] = Cell::Rock { hits: 0 }; // ボムの支え(ボムにも落下判定があるため必要)
        game.board.rows[520][2] = Cell::Item(ItemEffect::ClearAbove);
        game.board.rows[520][8] = Cell::Item(ItemEffect::UnifyColors);
        game.board.rows[520][9] = Cell::Item(ItemEffect::StarifyScreen);
        let score_before = game.player.score;

        game.update(Duration::from_millis(60));

        assert_eq!(game.board.cell(520, 2), Cell::Empty);
        assert_eq!(
            game.player.score, score_before,
            "アイテムの破壊はスコア対象外"
        );
    }

    #[test]
    fn bomb_explosion_leaves_item_blocks_outside_the_blast_untouched() {
        // 破壊されるのは爆風の届いたマスのアイテムだけで、範囲外のアイテムは無傷で残る。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5; // 爆風範囲外の位置
        game.bombs.push(Bomb {
            pos: (520, 5),
            origin: (520, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 50,
            settle_bounce_dir: 1,
        });
        game.board.rows[521][5] = Cell::Rock { hits: 0 }; // ボムの支え(ボムにも落下判定があるため必要)
        game.board.rows[520][8] = Cell::Item(ItemEffect::ClearAbove); // 爆風内
        game.board.rows[540][8] = Cell::Item(ItemEffect::UnifyColors); // 行も列も外れる
        game.board.rows[560][5] = Cell::Item(ItemEffect::StarifyScreen); // 同じ列だが縦の射程外

        game.update(Duration::from_millis(60));

        assert_eq!(
            game.board.cell(520, 8),
            Cell::Empty,
            "爆風内のアイテムは破壊されるはず"
        );
        assert_eq!(
            game.board.cell(540, 8),
            Cell::Item(ItemEffect::UnifyColors),
            "爆風範囲外のアイテムは無傷のはず"
        );
        assert_eq!(
            game.board.cell(560, 5),
            Cell::Item(ItemEffect::StarifyScreen),
            "同じ列でも縦の射程外のアイテムは無傷のはず"
        );
    }

    #[test]
    fn blocks_resting_on_an_item_destroyed_by_a_bomb_fall_afterwards() {
        // アイテムがEmptyになることで支えを失った上のブロックは、通常の重力
        // (揺れ→落下)でそのまま落ちてくる。
        let last_row = FIELD_DEPTH_M - 1;
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 950;
        game.player.col = 11; // 爆風範囲外の位置
        game.set_bomb_spawn_rate_percent(0);
        game.bombs.push(Bomb {
            pos: (last_row, 5),
            origin: (last_row, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 50,
            settle_bounce_dir: 1,
        });
        // 最深行のアイテム(常に支持されている)と、その上に乗ったダイヤ
        game.board.rows[last_row][8] = Cell::Item(ItemEffect::ClearAbove);
        game.board.rows[last_row - 1][8] = Cell::Diamond;

        game.update(Duration::from_millis(60));
        assert_eq!(
            game.board.cell(last_row, 8),
            Cell::Empty,
            "爆風内のアイテムは破壊されるはず"
        );
        assert_eq!(
            game.board.cell(last_row - 1, 8),
            Cell::Diamond,
            "この時点ではダイヤはまだ落ちていないはず"
        );

        game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));

        assert_eq!(
            game.board.cell(last_row - 1, 8),
            Cell::Empty,
            "支えを失ったダイヤは元の位置から落ちるはず"
        );
        assert_eq!(
            game.board.cell(last_row, 8),
            Cell::Diamond,
            "アイテムが消えた跡へダイヤが落ちてくるはず"
        );
    }

    #[test]
    fn destroyed_items_free_up_the_window_top_up_capacity() {
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        // アイテム出現数の窓単位補充は現存個数を都度数え直す方式なので、爆風で
        // アイテムがEmptyになれば補充枠も自動的に回復する。
        let mut game = Game::new(1);
        clear_board(&mut game);
        let count_clear_above = |game: &Game, from: usize, to: usize| {
            game.board.rows[from..to]
                .iter()
                .flatten()
                .filter(|c| matches!(c, Cell::Item(ItemEffect::ClearAbove)))
                .count()
        };

        // 窓[100, 200)を上限いっぱいのRアイテムで埋め、その先の未抽選territory
        // (200..260)は抽選対象になる未掘削マスで埋めておく。
        for i in 0..crate::constants::ITEM_MAX_COUNT_ON_BOARD {
            game.board.rows[100 + i][0] = Cell::Item(ItemEffect::ClearAbove);
        }
        for row in 200..260 {
            for col in 0..game.board.width() {
                game.board.rows[row][col] = Cell::Color(ColorKind::Red);
            }
        }

        game.board.top_up_items(&mut rng, 100, 200, 260, 2000, 0, 0);
        assert_eq!(
            count_clear_above(&game, 200, 260),
            0,
            "窓内が上限に達している間は新しいRアイテムを補充しないはず"
        );

        // 爆風で窓内のアイテムが全て破壊された状態にして、同じ条件で再度補充する。
        for i in 0..crate::constants::ITEM_MAX_COUNT_ON_BOARD {
            game.board.rows[100 + i][0] = Cell::Empty;
        }
        game.board.top_up_items(&mut rng, 100, 200, 260, 2000, 0, 0);

        assert!(
            count_clear_above(&game, 200, 260) > 0,
            "破壊されたぶんだけ補充枠が回復し、新しいRアイテムが出現するはず"
        );
    }

    #[test]
    fn bomb_explosion_unifies_color_blocks_within_blast_range_to_a_single_shared_color() {
        // 爆風内の異なる色のブロックが、爆発後は全て同じ1色になっていることを確認する
        // (色はランダムに選ばれるため、色そのものではなく一致しているかを検証する)。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5; // 爆風範囲外の位置
        game.bombs.push(Bomb {
            pos: (520, 5),
            origin: (520, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 50,
            settle_bounce_dir: 1,
        });
        game.board.rows[520][6] = Cell::Color(ColorKind::Red);
        game.board.rows[520][7] = Cell::Color(ColorKind::Blue);
        game.board.rows[521][5] = Cell::Color(ColorKind::Green);
        // 爆風範囲外(縦距離BOMB_BLAST_ROW_RANGE+1、画面外)。横は画面幅全部が範囲なので、
        // 範囲外を示すには縦方向を使う。
        game.board.rows[520 - BOMB_BLAST_ROW_RANGE - 1][5] = Cell::Color(ColorKind::Yellow);

        game.update(Duration::from_millis(60));

        let Cell::Color(unified) = game.board.cell(520, 6) else {
            panic!("爆風内の色ブロックは色ブロックのままのはず");
        };
        assert_eq!(
            game.board.cell(520, 7),
            Cell::Color(unified),
            "爆風内は全て同じ色になるはず"
        );
        assert_eq!(
            game.board.cell(521, 5),
            Cell::Color(unified),
            "爆風内は全て同じ色になるはず"
        );
        assert_eq!(
            game.board.cell(520 - BOMB_BLAST_ROW_RANGE - 1, 5),
            Cell::Color(ColorKind::Yellow),
            "爆風範囲外の色ブロックは変化しないはず"
        );
    }

    #[test]
    fn bomb_explosion_chain_detonates_another_bomb_caught_in_its_blast() {
        // 起爆カウントダウンが完了して爆発したボムの爆風範囲内に別のボム(まだ起爆まで
        // 余裕がある)があれば、そのボムも連鎖してその場で爆発することを確認する。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5; // 爆風範囲外の位置
        // 各ボムの真下に支えを置く(支えが無いと落下扱いになり、ボム自身の起爆
        // カウントダウンが進まなくなるため)。
        game.board.rows[521][5] = Cell::Rock { hits: 0 };
        game.board.rows[521][7] = Cell::Rock { hits: 0 };
        game.bombs.push(Bomb {
            pos: (520, 5),
            origin: (520, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 50, // このtickで起爆する
            settle_bounce_dir: 1,
        });
        game.bombs.push(Bomb {
            pos: (520, 7), // 1個目と同じ行、2列隣(爆風範囲内)
            origin: (520, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 5000, // 単独ではこのtickでは起爆しないはずの残り時間
            settle_bounce_dir: 1,
        });

        let events = game.update(Duration::from_millis(60));

        assert!(
            game.bombs.is_empty(),
            "誘爆で2個目のボムも盤面から消えるはず"
        );
        let exploded_count = events
            .iter()
            .filter(|e| matches!(e, GameEvent::BombExploded))
            .count();
        assert_eq!(
            exploded_count, 2,
            "誘爆した分も含めて2回ぶんBombExplodedが発生するはず: {events:?}"
        );
    }

    #[test]
    fn bomb_explosion_unify_that_forms_a_group_of_four_or_more_vanishes_immediately_like_a_landing()
    {
        // 爆風内の隣接する4マスの色ブロック(元は別々の色)が一色に統一された結果、
        // 4連結以上になった場合はその場で消滅する(着地時の自動消滅と同じ扱い)。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5; // 爆風範囲外の位置
        game.board.rows[521][5] = Cell::Rock { hits: 0 }; // ボムの支え
        game.bombs.push(Bomb {
            pos: (520, 5),
            origin: (520, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 50,
            settle_bounce_dir: 1,
        });
        // 距離1〜4(範囲内)に隣接する4色ブロックを並べる。統一後は同色4連結になる。
        game.board.rows[520][6] = Cell::Color(ColorKind::Red);
        game.board.rows[520][7] = Cell::Color(ColorKind::Blue);
        game.board.rows[520][8] = Cell::Color(ColorKind::Green);
        game.board.rows[520][9] = Cell::Color(ColorKind::Yellow);

        let events = game.update(Duration::from_millis(60));

        for col in 6..=9 {
            assert_eq!(
                game.board.cell(520, col),
                Cell::Empty,
                "4連結以上になった色ブロックはその場で消滅するはず(col={col})"
            );
        }
        assert!(
            events.contains(&GameEvent::BlockDestroyed { blocks: 4 }),
            "4連結の自動消滅イベントが発生するはず: {events:?}"
        );
    }

    #[test]
    fn bomb_falls_while_ticking_if_the_cell_below_becomes_empty() {
        // 起爆カウントダウン中でも、直下が空いていれば1マス落下し、その間はカウント
        // ダウンを進めない(空中で起爆させないため)ことを確認する。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.bombs.push(Bomb {
            pos: (520, 5),
            origin: (520, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 1000,
            settle_bounce_dir: 1,
        });

        game.update(Duration::from_millis(50));

        assert_eq!(
            game.bombs[0].pos,
            (521, 5),
            "直下が空いていれば1マス落下するはず"
        );
        assert_eq!(
            game.bombs[0].remaining_ms, 1000,
            "落下中は起爆カウントダウンを進めないはず"
        );
    }

    #[test]
    fn bomb_in_settling_phase_falls_one_cell_per_settle_tick_when_unsupported() {
        // Settling中も直下が空いていれば`BOMB_SETTLE_TICK_MS`ごとに1マスずつ落下する。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.bombs.push(Bomb {
            pos: (520, 5),
            origin: (520, 0),
            phase: BombPhase::Settling,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        game.update(Duration::from_millis(BOMB_SETTLE_TICK_MS as u64));

        assert_eq!(
            game.bombs[0].pos,
            (521, 5),
            "Settling中も直下が空いていれば1マス落下するはず"
        );
        assert_eq!(
            game.bombs[0].phase,
            BombPhase::Settling,
            "落下してもSettling段階のままのはず"
        );
    }

    #[test]
    fn bomb_in_settling_phase_bounces_sideways_instead_of_falling_onto_another_bomb_below() {
        // 直下が空セルでも、既に他のボムが占めていれば、そこへは落下せず左右へ
        // バウンドするはず(ボム同士は重ならない)。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.bombs.push(Bomb {
            pos: (520, 5),
            origin: (520, 0),
            phase: BombPhase::Settling,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });
        game.bombs.push(Bomb {
            pos: (521, 5), // 1つ目のボムの直下
            origin: (521, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        game.update(Duration::from_millis(BOMB_SETTLE_TICK_MS as u64));

        assert_eq!(
            game.bombs[0].pos,
            (520, 6),
            "他のボムの直下へは落下せず、bounce_dir方向(右)へバウンドするはず"
        );
    }

    #[test]
    fn bomb_ticking_above_another_bomb_stays_put_before_a_full_settle_tick_elapses() {
        // 他のボムの真上に来た直後、まだ1 settle tickぶんの時間が経過していなければ
        // 動かないはず(Settlingと同じペース制御であることの確認)。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.bombs.push(Bomb {
            pos: (520, 5),
            origin: (520, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 1000,
            settle_bounce_dir: 1,
        });
        game.bombs.push(Bomb {
            pos: (521, 5),
            origin: (521, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        game.update(Duration::from_millis(50)); // BOMB_SETTLE_TICK_MS(80)未満

        assert_eq!(
            game.bombs[0].pos,
            (520, 5),
            "1 settle tick未満では他のボムの上でまだ動かないはず"
        );
        assert_eq!(
            game.bombs[0].remaining_ms, 1000,
            "他のボムの上に乗っている間は起爆カウントダウンを進めないはず"
        );
    }

    #[test]
    fn bomb_ticking_above_another_bomb_bounces_sideways_after_a_full_settle_tick() {
        // 起爆カウントダウン中でも、直下に他のボムが居座っていればそこで静止せず、
        // 1 settle tick経過後に左右へバウンドするはず。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.bombs.push(Bomb {
            pos: (520, 5),
            origin: (520, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 1000,
            settle_bounce_dir: 1,
        });
        game.bombs.push(Bomb {
            pos: (521, 5),
            origin: (521, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        game.update(Duration::from_millis(BOMB_SETTLE_TICK_MS as u64));

        assert_eq!(
            game.bombs[0].pos,
            (520, 6),
            "他のボムの上に居座り続けず、右へバウンドして転がるはず"
        );
        assert_eq!(
            game.bombs[0].remaining_ms, 1000,
            "他のボムの上に乗っていた間は起爆カウントダウンを進めないはず"
        );
    }

    #[test]
    fn spawning_a_new_bomb_never_lands_on_a_cell_already_occupied_by_another_bomb() {
        // ボムはCellグリッドとは別のオーバーレイのため、既存ボムの位置も候補から除外
        // されているかを確認する。画面内を岩で埋め、既存ボムが占めるマスと本当に空いて
        // いるマスの2つだけをEmptyにすることで、RNGのseedによらず決定的に検証できる。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        let range = crate::constants::STAR_VISIBLE_RANGE_ROWS;
        for row in (game.player.row - range)..=(game.player.row + range) {
            for col in 0..game.board.width() {
                game.board.rows[row][col] = Cell::Rock { hits: 0 };
            }
        }
        let occupied_by_existing_bomb = (game.player.row, 3);
        let genuinely_free_cell = (game.player.row, 7);
        game.board.rows[occupied_by_existing_bomb.0][occupied_by_existing_bomb.1] = Cell::Empty;
        game.board.rows[genuinely_free_cell.0][genuinely_free_cell.1] = Cell::Empty;
        game.bombs.push(Bomb {
            pos: occupied_by_existing_bomb,
            origin: occupied_by_existing_bomb,
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        game.debug_place_bomb();

        assert_eq!(game.bombs.len(), 2, "新しいボムが1個追加されているはず");
        assert_eq!(
            game.bombs[1].pos, genuinely_free_cell,
            "既存ボムが占めるマスを避け、本当に空いているマスへ設置されるはず"
        );
    }

    #[test]
    fn bomb_in_settling_phase_bounces_sideways_instead_of_falling_onto_the_player() {
        // 直下が空セルでも、プレイヤーがそこに居れば落下せず左右へバウンドするはず。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 521;
        game.player.col = 5; // ボムの直下
        game.bombs.push(Bomb {
            pos: (520, 5),
            origin: (520, 0),
            phase: BombPhase::Settling,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });

        game.update(Duration::from_millis(BOMB_SETTLE_TICK_MS as u64));

        assert_eq!(
            game.bombs[0].pos,
            (520, 6),
            "プレイヤーの頭上へは落下せず、bounce_dir方向(右)へバウンドするはず"
        );
    }

    #[test]
    fn bomb_ticking_above_the_player_bounces_sideways_after_a_full_settle_tick() {
        // 起爆カウントダウン中でも、直下にプレイヤーが居ればそこで静止せず、
        // 1 settle tick経過後に左右へバウンドするはず。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 521;
        game.player.col = 5; // ボムの直下
        game.bombs.push(Bomb {
            pos: (520, 5),
            origin: (520, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 1000,
            settle_bounce_dir: 1,
        });

        game.update(Duration::from_millis(BOMB_SETTLE_TICK_MS as u64));

        assert_eq!(
            game.bombs[0].pos,
            (520, 6),
            "プレイヤーの頭上に居座り続けず、右へバウンドして転がるはず"
        );
        assert_eq!(
            game.bombs[0].remaining_ms, 1000,
            "プレイヤーの頭上に乗っていた間は起爆カウントダウンを進めないはず"
        );
    }

    #[test]
    fn bomb_explosion_shows_a_flame_flash_on_blast_cells_with_distance_based_tier_that_fades_out_after_the_flash_duration()
     {
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5; // 爆風範囲外の位置
        game.bombs.push(Bomb {
            pos: (520, 5),
            origin: (520, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 50,
            settle_bounce_dir: 1,
        });
        game.board.rows[521][5] = Cell::Rock { hits: 0 }; // ボムの支え(ボムにも落下判定があるため必要)
        game.board.rows[520][5] = Cell::Rock { hits: 0 }; // 爆心地(距離0)
        game.board.rows[519][5] = Cell::Rock { hits: 0 }; // 距離1(上方向)
        game.board.rows[520][7] = Cell::Rock { hits: 0 }; // 距離2(右方向、520,6はEmptyのまま)

        game.update(Duration::from_millis(60));

        let (progress0, tier0) = game
            .explosion_flash_progress((520, 5))
            .expect("爆心地は炎演出の対象のはず");
        assert_eq!(tier0, 0, "爆心地は距離0(炎の中心=CORE)のはず");
        assert!(progress0 < 0.5, "爆発直後は演出の進捗がまだ浅いはず");

        let (_, tier1) = game
            .explosion_flash_progress((519, 5))
            .expect("距離1のセルも炎演出の対象のはず");
        assert_eq!(tier1, 1, "距離1はMID相当のはず");

        let (_, tier2) = game
            .explosion_flash_progress((520, 7))
            .expect("距離2のセルも炎演出の対象のはず");
        assert_eq!(tier2, 2, "距離2はOUTER相当のはず");

        assert!(
            game.explosion_flash_progress((500, 5)).is_none(),
            "爆風の届いていないセルは対象にならないはず"
        );

        game.update(Duration::from_millis(BOMB_EXPLOSION_FLASH_MS + 10));
        assert!(
            game.explosion_flash_progress((520, 5)).is_none(),
            "演出時間が経過したら炎フラッシュは終わるはず"
        );
    }

    #[test]
    fn bomb_explosion_shows_a_flame_flash_on_empty_cells_within_the_blast_too() {
        // 爆風はRock/Diamondで止まらず遠くまで貫通するため、経路の大半はEmpty(既に
        // 掘削済みの空間)になる。内容を書き換えないEmpty/Oxygen等のセルでも、炎演出
        // 自体は他のセルと同じように発火するはず(付かないと炎の柱が見えなくなる)。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5; // 爆風範囲外の位置
        game.bombs.push(Bomb {
            pos: (520, 5),
            origin: (520, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 50,
            settle_bounce_dir: 1,
        });
        game.board.rows[521][5] = Cell::Rock { hits: 0 }; // ボムの支え(ボムにも落下判定があるため必要)
        // 爆心地の右方向はすべてEmptyのまま(遮蔽物なしの貫通経路)。

        game.update(Duration::from_millis(60));

        let (_, tier_right1) = game
            .explosion_flash_progress((520, 6))
            .expect("Empty(距離1・右方向)も炎演出の対象のはず");
        assert_eq!(tier_right1, 1);
        assert_eq!(
            game.board.cell(520, 6),
            Cell::Empty,
            "炎演出はEmptyの中身自体を書き換えないはず"
        );

        let (_, tier_right2) = game
            .explosion_flash_progress((520, 7))
            .expect("Empty(距離2・右方向)も炎演出の対象のはず");
        assert_eq!(tier_right2, 2);
    }

    #[test]
    fn bomb_explosion_crushes_the_player_caught_in_the_blast() {
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.bombs.push(Bomb {
            pos: (500, 6),
            origin: (500, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 50,
            settle_bounce_dir: 1,
        }); // プレイヤーの1マス右、爆風範囲内
        game.board.rows[501][6] = Cell::Rock { hits: 0 }; // ボムの支え(ボムにも落下判定があるため必要)

        let events = game.update(Duration::from_millis(60));

        assert!(events.contains(&GameEvent::BombExploded));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::LifeLost { .. })),
            "爆風に巻き込まれたら押し潰し相当のミスになるはず: {events:?}"
        );
    }

    #[test]
    fn bomb_blast_range_now_reaches_across_the_entire_field_width() {
        // 横方向の爆風はフィールド幅全体に届く。遮るものが無ければ、端から端まで
        // プレイヤーを巻き込むことを確認する(BOMB_BLAST_COL_RANGE=FIELD_WIDTH_MAX)。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 520;
        game.player.col = FIELD_WIDTH_DEFAULT - 1; // 爆心地(520,0)から見て反対端
        game.bombs.push(Bomb {
            pos: (520, 0),
            origin: (520, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 50,
            settle_bounce_dir: 1,
        });
        game.board.rows[521][0] = Cell::Rock { hits: 0 }; // ボムの支え(ボムにも落下判定があるため必要)

        let events = game.update(Duration::from_millis(60));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::LifeLost { .. })),
            "フィールド幅の反対端でも爆風に巻き込まれるはず: {events:?}"
        );
    }

    #[test]
    fn bomb_blast_row_range_catches_the_player_within_the_screen_but_not_beyond() {
        // 縦方向の爆風は盤面全体の深度ではなく画面内(BOMB_BLAST_ROW_RANGE)に限定される
        // ため、その距離ちょうどは巻き込むが1マス超えたら巻き込まないことを確認する。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 520 - BOMB_BLAST_ROW_RANGE;
        game.player.col = 5;
        game.bombs.push(Bomb {
            pos: (520, 5),
            origin: (520, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 50,
            settle_bounce_dir: 1,
        });
        game.board.rows[521][5] = Cell::Rock { hits: 0 }; // ボムの支え(ボムにも落下判定があるため必要)

        let events = game.update(Duration::from_millis(60));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::LifeLost { .. })),
            "画面内の距離(BOMB_BLAST_ROW_RANGE)ちょうどは爆風の範囲内で巻き込むはず: {events:?}"
        );
    }

    #[test]
    fn bomb_blast_row_range_does_not_catch_the_player_one_cell_beyond_the_screen() {
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 520 - BOMB_BLAST_ROW_RANGE - 1;
        game.player.col = 5;
        game.bombs.push(Bomb {
            pos: (520, 5),
            origin: (520, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 50,
            settle_bounce_dir: 1,
        });
        game.board.rows[521][5] = Cell::Rock { hits: 0 }; // ボムの支え(ボムにも落下判定があるため必要)

        let events = game.update(Duration::from_millis(60));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::LifeLost { .. })),
            "画面内(BOMB_BLAST_ROW_RANGE)を1マス超えたらプレイヤーを巻き込まないはず: {events:?}"
        );
    }

    #[test]
    fn bomb_stops_blinking_countdown_and_does_not_explode_before_the_fuse_runs_out() {
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.bombs.push(Bomb {
            pos: (520, 5),
            origin: (520, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });
        game.board.rows[521][5] = Cell::Rock { hits: 0 }; // ボムの支え(ボムにも落下判定があるため必要)

        let events = game.update(Duration::from_millis(100));

        assert_eq!(game.bombs.len(), 1, "起爆時間前は消えないはず");
        assert_eq!(game.bombs[0].remaining_ms, BOMB_FUSE_MS - 100);
        assert!(!events.contains(&GameEvent::BombExploded));
    }

    #[test]
    fn invincible_averts_being_caught_in_a_bomb_blast() {
        let mut game = Game::new(1);
        game.set_invincible(true);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        let lives_before = game.player.lives;
        game.bombs.push(Bomb {
            pos: (500, 6),
            origin: (500, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 50,
            settle_bounce_dir: 1,
        });
        game.board.rows[501][6] = Cell::Rock { hits: 0 }; // ボムの支え

        let events = game.update(Duration::from_millis(60));

        assert!(events.contains(&GameEvent::BombExploded));
        assert!(
            events.contains(&GameEvent::MissAverted {
                cause: MissCause::BombBlast
            }),
            "爆風はBombBlastとして回避されるはず: {events:?}"
        );
        assert_eq!(game.player.lives, lives_before);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::LifeLost { .. }))
        );
    }

    #[test]
    fn death_detonates_every_bomb_on_the_board_immediately_regardless_of_its_own_fuse() {
        // 死亡(押し潰し)処理の際、起爆までまだ全く余裕があるボムも含めて、盤面上の
        // 全てのボムがその場で即座に爆発することを確認する。
        let mut game = Game::new_with_lives(74, 2); // ライフ2、押し潰されても即GameOverにならない
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[998][5] = Cell::Color(ColorKind::Red); // プレイヤーの真上、支えなし(押し潰す)
        game.board.rows[521][7] = Cell::Rock { hits: 0 }; // ボムの支え
        game.bombs.push(Bomb {
            pos: (520, 7),
            origin: (520, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 60_000, // まだ全く起爆する気配が無い残り時間
            settle_bounce_dir: 1,
        });

        let mut events = game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));
        assert!(
            !game.bombs.is_empty(),
            "「天に召される」演出が終わるまではまだ爆発しないはず"
        );
        events.extend(game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        )));

        assert!(
            game.bombs.is_empty(),
            "死亡処理の完了時点で残り時間に関わらずボムは爆発しているはず"
        );
        assert!(
            events.contains(&GameEvent::BombExploded),
            "死亡による即時爆発でもBombExplodedイベントが発生するはず: {events:?}"
        );
    }
}

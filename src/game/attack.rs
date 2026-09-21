//! 対戦の妨害ルール(攻撃力→岩・ボム投下。#247/#304。spec.md 12.8)。

use super::*;

/// 相手の攻撃で降ってくる岩1個ぶんの予告状態(#247)。ボム(`Bomb`)と同じくCellグリッド外の
/// オーバーレイとして`Game.incoming_rocks`で管理し、予告が明けた瞬間に`Cell::Rock { hits: 0 }`
/// として盤面へ書き込まれて実体化する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncomingRock {
    /// 出現予定マス。予告開始時点の画面上端の行(`player.row`から`PLAYER_SCREEN_ROWS_ABOVE`
    /// ぶん引いた行)と、抽選した列。
    pub pos: board::Pos,
    /// 予告表示の残り時間(ms)。0になったフレームで盤面へ出現する。
    pub remaining_ms: u32,
}

/// `Game::receive_incoming_attack`の結果(テスト・HUD・将来のデシンク調査用)。岩とボムは
/// 別勘定のため、相殺量・到達量・投下数もそれぞれ独立に持つ(#304)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AttackReceipt {
    /// 自分の未送信攻撃力(岩ぶん)と相殺して消えた量。
    pub rock_absorbed: u32,
    /// 相殺後に実際にこちらへ届いた攻撃力(岩の受信待ちプールへ加算された量)。
    pub rock_delivered: u32,
    /// このタイミングで予告キューへ積まれた岩の個数(上限・列の空き状況でクランプ後)。
    pub rocks_queued: usize,
    /// 自分の未送信攻撃力(ボムぶん)と相殺して消えた量。
    pub bomb_absorbed: u32,
    /// 相殺後に実際にこちらへ届いた攻撃力(ボムの受信待ちプールへ加算された量)。
    pub bomb_delivered: u32,
    /// このタイミングで盤面へ出現させたボムの個数(上限・盤面のボム数でクランプ後)。
    pub bombs_queued: usize,
}

impl Game {
    // --- 対戦の妨害ルール(攻撃力→岩・ボム投下。#247/#304。spec.md 12.8) -------

    /// 自分が消したブロック数を攻撃力へ単純合算する。`attack_rules_enabled`がfalseなら
    /// 何もしない(通常の1人プレイに影響を残さない)。`game`本体(mod.rs)の掘削・自動消滅処理
    /// からも呼ぶため`pub(super)`にする。
    pub(super) fn add_attack_power(&mut self, blocks: usize) {
        if !self.attack_rules_enabled {
            return;
        }
        self.attack_power_pending = self
            .attack_power_pending
            .saturating_add(u32::try_from(blocks).unwrap_or(u32::MAX));
    }

    // 以下4つは通信層(#10本体)とテストのための公開API。現状の1人プレイ経路からは
    // デバッグショートカット(O)しか呼ばないため、バイナリクレートでは未使用と判定される。
    /// 妨害ルールの有効/無効を切り替える。将来の対戦開始処理(#10)から呼ぶ。
    #[allow(dead_code)]
    pub fn set_attack_rules_enabled(&mut self, on: bool) {
        self.attack_rules_enabled = on;
    }

    /// 妨害ルールが有効かどうか。
    #[allow(dead_code)]
    pub fn attack_rules_enabled(&self) -> bool {
        self.attack_rules_enabled
    }

    /// 自分の未送信攻撃力を取り出して0に戻す(=相手へ「送信した」扱い)。
    pub fn take_pending_attack_power(&mut self) -> u32 {
        std::mem::take(&mut self.attack_power_pending)
    }

    /// 自分の未送信攻撃力を取り出し、`attack_bomb_ratio_percent`に従って岩ぶん・ボムぶんへ
    /// 振り分ける(#304)。戻り値は(岩, ボム)。固定比率で割るのではなく1ポイントごとに
    /// `self.rng`で判定するため、降る個数に毎回ばらつきが出る。岩の抽選と同じ乱数
    /// ストリームを使うので、同じシードなら同じ振り分けが再現される。
    pub fn take_pending_attack_power_split(&mut self) -> (u32, u32) {
        let total = self.take_pending_attack_power();
        if self.attack_bomb_ratio_percent == 0 {
            return (total, 0);
        }
        let bomb_prob = self.attack_bomb_ratio_percent as f64 / 100.0;
        let mut bomb = 0u32;
        for _ in 0..total {
            if self.rng.random_range(0.0..1.0) < bomb_prob {
                bomb += 1;
            }
        }
        (total - bomb, bomb)
    }

    /// 自分の未送信攻撃力。
    #[allow(dead_code)]
    pub fn attack_power_pending(&self) -> u32 {
        self.attack_power_pending
    }

    /// 相手から届いたが、まだ岩へ変換していない攻撃力。
    #[allow(dead_code)]
    pub fn incoming_attack_power(&self) -> u32 {
        self.incoming_attack_power
    }

    /// 自分の未送信攻撃力のうちボムぶん(#304)。送信時の振り分けで積まれる。
    #[allow(dead_code)]
    pub fn bomb_power_pending(&self) -> u32 {
        self.bomb_power_pending
    }

    /// 相手から届いたが、まだボムへ変換していない攻撃力(#304)。
    #[allow(dead_code)]
    pub fn incoming_bomb_power(&self) -> u32 {
        self.incoming_bomb_power
    }

    /// 予告中(まだ盤面に出ていない)の岩。描画側(render.rs)が参照する。
    pub fn incoming_rocks(&self) -> &[IncomingRock] {
        &self.incoming_rocks
    }

    /// 岩1個に必要な攻撃力を直接指定する(起動時、Settingsから読み込んだ値を適用する
    /// 用途)。範囲外の値は`ATTACK_BLOCKS_PER_ROCK_MIN`〜`MAX`にクランプする。
    pub fn set_attack_blocks_per_rock(&mut self, blocks: u32) {
        self.attack_blocks_per_rock =
            blocks.clamp(ATTACK_BLOCKS_PER_ROCK_MIN, ATTACK_BLOCKS_PER_ROCK_MAX);
    }

    /// 1ウェーブで降らせる岩の上限を直接指定する(同上)。範囲外の値は
    /// `ATTACK_ROCKS_PER_WAVE_MAX_MIN`〜`MAX`にクランプする。
    pub fn set_attack_rocks_per_wave_max(&mut self, rocks: u32) {
        self.attack_rocks_per_wave_max =
            rocks.clamp(ATTACK_ROCKS_PER_WAVE_MAX_MIN, ATTACK_ROCKS_PER_WAVE_MAX_MAX);
    }

    /// ボム1個に必要な攻撃力を直接指定する(#304。起動時、Settingsから読み込んだ値を適用
    /// する用途)。範囲外の値は`ATTACK_BLOCKS_PER_BOMB_MIN`〜`MAX`にクランプする。
    pub fn set_attack_blocks_per_bomb(&mut self, blocks: u32) {
        self.attack_blocks_per_bomb =
            blocks.clamp(ATTACK_BLOCKS_PER_BOMB_MIN, ATTACK_BLOCKS_PER_BOMB_MAX);
    }

    /// 1ウェーブで降らせるボムの上限を直接指定する(同上)。範囲外の値は
    /// `ATTACK_BOMBS_PER_WAVE_MAX_MIN`〜`MAX`にクランプする。
    pub fn set_attack_bombs_per_wave_max(&mut self, bombs: u32) {
        self.attack_bombs_per_wave_max =
            bombs.clamp(ATTACK_BOMBS_PER_WAVE_MAX_MIN, ATTACK_BOMBS_PER_WAVE_MAX_MAX);
    }

    /// 送信する攻撃力のうちボムへ振り分ける比率(%)を直接指定する(同上)。範囲外の値は
    /// `ATTACK_BOMB_RATIO_PERCENT_MIN`〜`MAX`にクランプする。
    pub fn set_attack_bomb_ratio_percent(&mut self, percent: u32) {
        self.attack_bomb_ratio_percent =
            percent.clamp(ATTACK_BOMB_RATIO_PERCENT_MIN, ATTACK_BOMB_RATIO_PERCENT_MAX);
    }

    /// 相手の攻撃力を受け取る。岩ぶん・ボムぶんそれぞれを自分の未送信攻撃力と相殺し、
    /// 残りを種類ごとの受信待ちプールへ加算した上で、岩の予告キューとボムの投下を開始
    /// する。Playing中以外は何もしない。
    pub fn receive_incoming_attack(&mut self, rock_amount: u32, bomb_amount: u32) -> AttackReceipt {
        if self.status != GameStatus::Playing {
            return AttackReceipt::default();
        }
        let rock_absorbed = self.attack_power_pending.min(rock_amount);
        self.attack_power_pending -= rock_absorbed;
        let rock_delivered = rock_amount - rock_absorbed;
        self.incoming_attack_power = self.incoming_attack_power.saturating_add(rock_delivered);

        let bomb_absorbed = self.bomb_power_pending.min(bomb_amount);
        self.bomb_power_pending -= bomb_absorbed;
        let bomb_delivered = bomb_amount - bomb_absorbed;
        self.incoming_bomb_power = self.incoming_bomb_power.saturating_add(bomb_delivered);

        let rocks_queued = self.try_start_incoming_wave();
        let bombs_queued = self.try_start_incoming_bomb_wave();
        AttackReceipt {
            rock_absorbed,
            rock_delivered,
            rocks_queued,
            bomb_absorbed,
            bomb_delivered,
            bombs_queued,
        }
    }

    /// 受信待ちプールから1ウェーブぶんの岩を予告キューへ積む。戻り値は今回積んだ個数。
    /// 前のウェーブの予告がまだ残っている間は何もしない(1ウェーブずつしか進めない)。
    /// `game`本体(mod.rs)の`update()`からも呼ぶため`pub(super)`にする。
    pub(super) fn try_start_incoming_wave(&mut self) -> usize {
        if !self.incoming_rocks.is_empty() {
            return 0;
        }
        if self.attack_blocks_per_rock == 0 {
            return 0; // 設定のクランプ上あり得ないが、0除算だけは構造的に防いでおく。
        }
        let n_by_power = self.incoming_attack_power / self.attack_blocks_per_rock;
        let n = n_by_power.min(self.attack_rocks_per_wave_max) as usize;
        if n == 0 {
            return 0;
        }

        // 出現行は画面上端(プレイヤーより`PLAYER_SCREEN_ROWS_ABOVE`行浅い位置)。
        // 画面内へいきなり現れるのではなく、上端から降ってくるように見せる。
        let spawn_row = self.player.row.saturating_sub(PLAYER_SCREEN_ROWS_ABOVE);
        let player_pos = self.player.position();
        let mut eligible: Vec<usize> = (0..self.board.width())
            .filter(|&col| {
                self.board.cell(spawn_row, col) == Cell::Empty
                    && (spawn_row, col) != player_pos
                    && !self.settled_bomb_at(spawn_row, col)
            })
            .collect();
        let n = n.min(eligible.len());
        if n == 0 {
            // 空きが無ければプールに残したまま、次のフレームで再試行する。
            return 0;
        }

        // 候補列から非復元で n 列を抽選する(ボム出現位置と同じく`self.rng`を共用し、
        // 同じシードなら同じ降り方が再現される)。
        for _ in 0..n {
            let idx = self.rng.random_range(0..eligible.len());
            let col = eligible.swap_remove(idx);
            self.incoming_rocks.push(IncomingRock {
                pos: (spawn_row, col),
                remaining_ms: INCOMING_ROCK_WARNING_MS,
            });
        }
        self.incoming_attack_power -= self.attack_blocks_per_rock * n as u32;
        n
    }

    /// 受信待ちプールから1ウェーブぶんのボムを盤面へ出現させる(#304)。戻り値は今回出現
    /// させた個数。岩と違い予告の段階を持たず、自然発生ボムと同じ`BombPhase::Entering`
    /// から始める。盤面のボム数上限(`BOMB_MAX_COUNT_ON_BOARD`)は自然発生ぶんと共用し、
    /// 埋まっていればプールに残したまま次のフレームで再試行する。
    /// `game`本体(mod.rs)の`update()`からも呼ぶため`pub(super)`にする。
    pub(super) fn try_start_incoming_bomb_wave(&mut self) -> usize {
        if self.attack_blocks_per_bomb == 0 {
            return 0; // 設定のクランプ上あり得ないが、0除算だけは構造的に防いでおく。
        }
        let n_by_power = self.incoming_bomb_power / self.attack_blocks_per_bomb;
        let n = n_by_power.min(self.attack_bombs_per_wave_max) as usize;
        let n = n.min(BOMB_MAX_COUNT_ON_BOARD.saturating_sub(self.bombs.len()));
        if n == 0 {
            return 0;
        }

        let mut spawned = 0usize;
        for _ in 0..n {
            let before = self.bombs.len();
            self.spawn_bomb_at_random_empty_cell();
            if self.bombs.len() == before {
                // 画面内に空きマスが無い。残りはプールに残して次のフレームで再試行する。
                break;
            }
            spawned += 1;
        }
        self.incoming_bomb_power -= self.attack_blocks_per_bomb * spawned as u32;
        spawned
    }

    /// 予告中の岩の残り時間を進め、0になったものを盤面へ`Cell::Rock { hits: 0 }`として
    /// 出現させる。出現後の落下・揺れ・連結消滅は既存の重力ティックがそのまま処理する
    /// ため、ここでは盤面へ書き込むだけにとどめる(揺れ状態のリセットも行わない)。
    /// `game`本体(mod.rs)の`update()`からも呼ぶため`pub(super)`にする。
    pub(super) fn tick_incoming_rocks(&mut self, delta_ms: u32, events: &mut Vec<GameEvent>) {
        if self.incoming_rocks.is_empty() {
            return;
        }
        for rock in self.incoming_rocks.iter_mut() {
            rock.remaining_ms = rock.remaining_ms.saturating_sub(delta_ms);
        }
        let (ready, waiting): (Vec<IncomingRock>, Vec<IncomingRock>) = self
            .incoming_rocks
            .drain(..)
            .partition(|r| r.remaining_ms == 0);
        self.incoming_rocks = waiting;

        let mut spawned = 0usize;
        let mut returned_power = 0u32;
        for rock in ready {
            let (r, c) = rock.pos;
            let free = self.board.cell(r, c) == Cell::Empty
                && (r, c) != self.player.position()
                && !self.settled_bomb_at(r, c);
            if free {
                self.board.set(r, c, Cell::Rock { hits: 0 });
                spawned += 1;
            } else {
                // 予告中にマスが塞がれた(稀)。攻撃力としてプールへ戻し、次ウェーブで
                // 別の列を再抽選する。
                returned_power = returned_power.saturating_add(self.attack_blocks_per_rock);
            }
        }
        self.incoming_attack_power = self.incoming_attack_power.saturating_add(returned_power);
        if spawned > 0 {
            events.push(GameEvent::IncomingRocksSpawned { rocks: spawned });
        }
    }

    /// デバッグ: 「攻撃交換1ラウンド」を疑似実行する(#247)。相手は居ないので、相殺後に
    /// 残った自分の攻撃力は捨てる(=送信済み扱いにする)。初回押下で妨害ルールを有効化する。
    pub fn debug_receive_opponent_attack(&mut self) {
        if self.status != GameStatus::Playing {
            return;
        }
        self.attack_rules_enabled = true;
        // 受け取る量も送信時と同じ比率で岩ぶん・ボムぶんへ振り分け、両方の経路を試せる
        // ようにする(#304)。
        let bomb = DEBUG_INCOMING_ATTACK_POWER * self.attack_bomb_ratio_percent / 100;
        let rock = DEBUG_INCOMING_ATTACK_POWER - bomb;
        self.receive_incoming_attack(rock, bomb);
        self.take_pending_attack_power();
    }

    /// テスト専用: 予告中の岩を直接置くための可変参照(#247)。出現列は乱数で決まるため、
    /// 「この座標に予告が出ている盤面」を組み立てたい描画テスト等で使う。
    #[cfg(test)]
    pub(crate) fn incoming_rocks_mut(&mut self) -> &mut Vec<IncomingRock> {
        &mut self.incoming_rocks
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{attack_rules_game, clear_board};
    use super::*;
    use crate::constants::{ROCK_HITS_TO_BREAK, SHAKE_TICKS};

    // --- 対戦の妨害ルール(攻撃力→岩・ボム投下。#247/#304。spec.md 12.8) --------

    /// `attack_rules_game`で作った盤面での岩の出現行。
    fn spawn_row_of(game: &Game) -> usize {
        game.player.row - PLAYER_SCREEN_ROWS_ABOVE
    }

    /// 指定行にある岩(Xブロック)の個数。
    fn rocks_in_row(game: &Game, row: usize) -> usize {
        (0..game.board.width())
            .filter(|&col| matches!(game.board.cell(row, col), Cell::Rock { .. }))
            .count()
    }

    #[test]
    fn drilling_color_blocks_adds_their_count_to_the_pending_attack_power() {
        // 直接掘削で消した色ブロックの数がそのまま攻撃力へ加算される(単純合算・倍率無し)。
        let mut game = Game::new(51);
        game.set_attack_rules_enabled(true);
        game.player.facing = Direction::Down;
        let target_row = game.player.row + 1;
        let col = game.player.col;
        // 横に3個つながった同色を1回の掘削で消す(掘削消滅は連結分もまとめて消える)。
        game.board.rows[target_row][col] = Cell::Color(ColorKind::Red);
        game.board.rows[target_row][col + 1] = Cell::Color(ColorKind::Red);
        game.board.rows[target_row][col + 2] = Cell::Color(ColorKind::Red);

        let events = game.try_drill();

        let destroyed = events
            .iter()
            .find_map(|e| match e {
                GameEvent::BlockDestroyed { blocks } => Some(*blocks),
                _ => None,
            })
            .expect("色ブロックの掘削消滅イベントが出るはず");
        assert!(destroyed >= 3, "3個つながった塊がまとめて消えるはず");
        assert_eq!(
            game.attack_power_pending(),
            destroyed as u32,
            "消したブロック数がそのまま攻撃力になる"
        );
    }

    #[test]
    fn drilling_color_blocks_adds_nothing_while_the_attack_rules_are_disabled() {
        // 通常の1人プレイ(既定=無効)では攻撃力を一切数えない。
        let mut game = Game::new(51);
        game.player.facing = Direction::Down;
        let target_row = game.player.row + 1;
        let col = game.player.col;
        game.board.rows[target_row][col] = Cell::Color(ColorKind::Red);

        game.try_drill();

        assert!(!game.attack_rules_enabled(), "既定では妨害ルールは無効");
        assert_eq!(game.attack_power_pending(), 0);
    }

    #[test]
    fn drilling_a_rock_to_its_fifth_hit_adds_one_attack_power_and_keeps_the_oxygen_penalty() {
        // 岩ブロックは連結していても掘削で消えるのは1ブロックのみ(board::hit_rockの仕様)
        // なので、攻撃力への加算も1。酸素ペナルティは従来どおり1回分だけ引かれる。
        let mut game = Game::new(52);
        game.set_attack_rules_enabled(true);
        game.player.facing = Direction::Down;
        let target_row = game.player.row + 1;
        let col = game.player.col;
        game.board.rows[target_row][col] = Cell::Rock {
            hits: ROCK_HITS_TO_BREAK - 1,
        };
        game.board.rows[target_row][col + 1] = Cell::Rock { hits: 0 };
        let oxygen_before = game.player.oxygen;

        let events = game.try_drill();

        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::RockDestroyed { blocks: 1 }))
        );
        assert_eq!(game.attack_power_pending(), 1);
        assert_eq!(
            game.player.oxygen,
            oxygen_before - 20.0,
            "酸素ペナルティは従来どおり1回分"
        );
    }

    #[test]
    fn auto_vanishing_four_connected_blocks_adds_their_count_to_the_attack_power() {
        // 4連結以上の自動消滅も、直接掘削と同じく消えた数をそのまま合算する。
        let mut game = Game::new(11);
        clear_board(&mut game);
        game.set_attack_rules_enabled(true);
        game.player.row = 999;
        game.player.col = 11;

        game.board.rows[998][0] = Cell::Color(ColorKind::Red);
        game.board.rows[998][1] = Cell::Color(ColorKind::Red);
        game.board.rows[998][2] = Cell::Color(ColorKind::Red);
        game.board.rows[999][3] = Cell::Color(ColorKind::Red);

        let events = game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));

        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::BlockDestroyed { blocks: 4 }))
        );
        assert_eq!(game.attack_power_pending(), 4);
    }

    #[test]
    fn auto_vanishing_four_connected_rock_blocks_also_adds_their_count_to_the_attack_power() {
        // 岩ブロックの4連結自動消滅はスコア対象外(4.9)だが、攻撃力としては色ブロックと
        // 同じく「消したブロック数」に数える。
        let mut game = Game::new(41);
        clear_board(&mut game);
        game.set_attack_rules_enabled(true);
        game.player.row = 999;
        game.player.col = 11;

        game.board.rows[998][0] = Cell::Rock { hits: 0 };
        game.board.rows[998][1] = Cell::Rock { hits: 1 };
        game.board.rows[998][2] = Cell::Rock { hits: 2 };
        game.board.rows[999][3] = Cell::Rock { hits: 3 }; // 最深行=常に支持
        let score_before = game.player.score;

        let events = game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));

        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::RockDestroyed { blocks: 4 }))
        );
        assert_eq!(game.attack_power_pending(), 4);
        assert_eq!(
            game.player.score, score_before,
            "岩の自動消滅はスコア対象外のまま(攻撃力とスコアは別勘定)"
        );
    }

    #[test]
    fn drilling_a_star_adds_one_attack_power() {
        // スターは1マスで消えるため、攻撃力への加算も1。
        let mut game = Game::new(53);
        game.set_attack_rules_enabled(true);
        game.player.facing = Direction::Down;
        let target_row = game.player.row + 1;
        let col = game.player.col;
        game.board.rows[target_row][col] = Cell::Star { visible_ms: 0 };

        let events = game.try_drill();

        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::BlockDestroyed { blocks: 1 }))
        );
        assert_eq!(game.attack_power_pending(), 1);
    }

    #[test]
    fn a_bomb_blast_does_not_count_toward_the_attack_power() {
        // 攻撃力に数えるのは「直接掘削」と「4連結以上の自動消滅」だけ(spec.md 12.8)。
        // 爆風でスター化・消滅したぶんは攻撃力にしない。
        let mut game = Game::new(54);
        clear_board(&mut game);
        game.set_attack_rules_enabled(true);
        game.player.row = 500;
        game.player.col = 10; // 爆風範囲外
        game.bombs.push(Bomb {
            pos: (520, 5),
            origin: (520, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 50,
            settle_bounce_dir: 1,
        });
        // ボムは支えが無いと落下して導火線が進まないため、真下に足場を置く。
        game.board.rows[521][5] = Cell::Rock { hits: 0 };
        // 爆風で一色に統一されると4連結になる並び。爆発と同時にこの場で消える。
        game.board.rows[520][1] = Cell::Color(ColorKind::Red);
        game.board.rows[520][2] = Cell::Color(ColorKind::Blue);
        game.board.rows[520][3] = Cell::Color(ColorKind::Red);
        game.board.rows[520][4] = Cell::Color(ColorKind::Yellow);

        let events = game.update(Duration::from_millis(60));

        assert!(
            events.contains(&GameEvent::BombExploded),
            "前提: 爆発まで進んでいること: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::BlockDestroyed { blocks: 4 })),
            "前提: 爆風でブロックが消えていること: {events:?}"
        );
        assert_eq!(
            game.attack_power_pending(),
            0,
            "爆風による破壊は攻撃力に数えない"
        );
    }

    #[test]
    fn the_attack_settings_are_clamped_into_their_allowed_range() {
        // 設定値の直接指定(起動時のSettings反映経路)でも範囲外の値を持ち込ませない。
        let mut game = Game::new(55);

        game.set_attack_blocks_per_rock(0);
        assert_eq!(game.attack_blocks_per_rock, ATTACK_BLOCKS_PER_ROCK_MIN);
        game.set_attack_blocks_per_rock(u32::MAX);
        assert_eq!(game.attack_blocks_per_rock, ATTACK_BLOCKS_PER_ROCK_MAX);

        game.set_attack_rocks_per_wave_max(0);
        assert_eq!(
            game.attack_rocks_per_wave_max,
            ATTACK_ROCKS_PER_WAVE_MAX_MIN
        );
        game.set_attack_rocks_per_wave_max(u32::MAX);
        assert_eq!(
            game.attack_rocks_per_wave_max,
            ATTACK_ROCKS_PER_WAVE_MAX_MAX
        );

        game.set_attack_blocks_per_bomb(0);
        assert_eq!(game.attack_blocks_per_bomb, ATTACK_BLOCKS_PER_BOMB_MIN);
        game.set_attack_blocks_per_bomb(u32::MAX);
        assert_eq!(game.attack_blocks_per_bomb, ATTACK_BLOCKS_PER_BOMB_MAX);

        game.set_attack_bombs_per_wave_max(0);
        assert_eq!(
            game.attack_bombs_per_wave_max,
            ATTACK_BOMBS_PER_WAVE_MAX_MIN
        );
        game.set_attack_bombs_per_wave_max(u32::MAX);
        assert_eq!(
            game.attack_bombs_per_wave_max,
            ATTACK_BOMBS_PER_WAVE_MAX_MAX
        );

        game.set_attack_bomb_ratio_percent(u32::MAX);
        assert_eq!(
            game.attack_bomb_ratio_percent,
            ATTACK_BOMB_RATIO_PERCENT_MAX
        );
        game.set_attack_bomb_ratio_percent(0);
        assert_eq!(
            game.attack_bomb_ratio_percent, ATTACK_BOMB_RATIO_PERCENT_MIN,
            "0は「ボム化しない」ので有効値"
        );
    }

    #[test]
    fn apply_settings_reflects_the_attack_settings_into_the_game() {
        // 新規ゲーム開始時に、永続化された設定が妨害ルールへ反映されること。
        let mut game = Game::new(56);
        let settings = crate::settings::Settings {
            attack_blocks_per_rock: 7,
            attack_rocks_per_wave_max: 2,
            attack_blocks_per_bomb: 13,
            attack_bombs_per_wave_max: 3,
            attack_bomb_ratio_percent: 35,
            ..Default::default()
        };

        game.apply_settings(&settings);

        assert_eq!(game.attack_blocks_per_rock, 7);
        assert_eq!(game.attack_rocks_per_wave_max, 2);
        assert_eq!(game.attack_blocks_per_bomb, 13);
        assert_eq!(game.attack_bombs_per_wave_max, 3);
        assert_eq!(game.attack_bomb_ratio_percent, 35);
    }

    #[test]
    fn receiving_an_attack_smaller_than_the_pending_power_is_fully_absorbed() {
        // 相殺(そうさい)方式: 自分の溜め分が相手の攻撃力以上なら、こちらへは1つも届かない。
        let mut game = attack_rules_game(60);
        game.attack_power_pending = 5;

        let receipt = game.receive_incoming_attack(3, 0);

        assert_eq!(receipt.rock_absorbed, 3);
        assert_eq!(receipt.rock_delivered, 0);
        assert_eq!(receipt.rocks_queued, 0);
        assert_eq!(game.attack_power_pending(), 2, "相殺した分だけ減る");
        assert_eq!(game.incoming_attack_power(), 0);
        assert!(game.incoming_rocks().is_empty());
    }

    #[test]
    fn receiving_a_larger_attack_delivers_the_remainder_and_converts_it_into_rocks() {
        // 相殺後の残りが比率ぶんだけ岩になり、端数はプールに残って次の受信へ繰り越される。
        let mut game = attack_rules_game(61);
        game.set_attack_blocks_per_rock(4);
        game.attack_power_pending = 2;

        let receipt = game.receive_incoming_attack(12, 0);

        assert_eq!(receipt.rock_absorbed, 2);
        assert_eq!(receipt.rock_delivered, 10);
        assert_eq!(receipt.rocks_queued, 2);
        assert_eq!(game.attack_power_pending(), 0);
        assert_eq!(game.incoming_rocks().len(), 2);
        assert_eq!(game.incoming_attack_power(), 2, "端数はプールに残る");
    }

    #[test]
    fn an_incoming_wave_is_capped_and_the_rest_follows_in_later_waves() {
        // 1ウェーブの上限を超える分はプールに残り、予告が明けるたびに次のウェーブが出る。
        let mut game = attack_rules_game(62);
        game.set_attack_blocks_per_rock(1);
        game.set_attack_rocks_per_wave_max(4);
        let spawn_row = spawn_row_of(&game);

        game.receive_incoming_attack(10, 0);
        assert_eq!(game.incoming_rocks().len(), 4, "1ウェーブは上限の4個まで");
        assert_eq!(game.incoming_attack_power(), 6);

        // 1波目の予告が明けて出現し、同じフレームで2波目が積まれる。
        let events = game.update(Duration::from_millis(INCOMING_ROCK_WARNING_MS as u64));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::IncomingRocksSpawned { rocks: 4 })),
            "1ウェーブにつき1回だけ出現イベントが出る"
        );
        assert_eq!(rocks_in_row(&game, spawn_row), 4, "出現行に岩が4個現れる");
        assert_eq!(game.incoming_rocks().len(), 4, "2波目が積まれている");
        assert_eq!(game.incoming_attack_power(), 2);

        // 2波目 → 残り2個の3波目。
        game.update(Duration::from_millis(INCOMING_ROCK_WARNING_MS as u64));
        assert_eq!(rocks_in_row(&game, spawn_row), 4);
        assert_eq!(game.incoming_rocks().len(), 2);
        assert_eq!(game.incoming_attack_power(), 0);

        game.update(Duration::from_millis(INCOMING_ROCK_WARNING_MS as u64));
        assert_eq!(rocks_in_row(&game, spawn_row), 2);
        assert!(game.incoming_rocks().is_empty(), "プールが尽きれば打ち止め");
    }

    #[test]
    fn a_spawned_incoming_rock_falls_through_the_existing_gravity() {
        // 出現した岩に専用の落下処理は持たせず、既存の重力ティック(揺れ→落下)へ委ねる。
        let mut game = attack_rules_game(63);
        game.set_attack_blocks_per_rock(1);
        game.set_attack_rocks_per_wave_max(1);
        let spawn_row = spawn_row_of(&game);

        game.receive_incoming_attack(1, 0);
        game.update(Duration::from_millis(INCOMING_ROCK_WARNING_MS as u64));
        assert_eq!(rocks_in_row(&game, spawn_row), 1, "前提: 出現していること");

        // 揺れ時間+数ティックぶん進めれば、必ず1行以上落ちているはず。
        game.update(Duration::from_millis(
            SHAKE_DURATION_MS + (SHAKE_TICKS as u64 + 2) * FALL_TICK_MS,
        ));

        assert_eq!(
            rocks_in_row(&game, spawn_row),
            0,
            "出現行から落下して抜けているはず"
        );
        assert!(
            (spawn_row + 1..game.board.depth_rows())
                .any(|row| rocks_in_row(&game, row) > 0 || row == game.board.depth_rows() - 1),
            "落下先の行に岩が移っているはず"
        );
    }

    #[test]
    fn no_wave_starts_while_the_spawn_row_is_completely_blocked() {
        // 出現行に空きが1つも無ければ予告を始めず、攻撃力はプールに残したままにする。
        let mut game = attack_rules_game(64);
        game.set_attack_blocks_per_rock(1);
        let spawn_row = spawn_row_of(&game);
        for col in 0..game.board.width() {
            game.board.set(spawn_row, col, Cell::Rock { hits: 0 });
        }

        let receipt = game.receive_incoming_attack(3, 0);

        assert_eq!(receipt.rocks_queued, 0);
        assert!(game.incoming_rocks().is_empty());
        assert_eq!(game.incoming_attack_power(), 3, "プールは維持される");

        // 1列だけ空ければ、次のフレームでその1個だけが積まれる。
        game.board.set(spawn_row, 2, Cell::Empty);
        game.update(Duration::from_millis(1));

        assert_eq!(game.incoming_rocks().len(), 1);
        assert_eq!(game.incoming_rocks()[0].pos, (spawn_row, 2));
        assert_eq!(game.incoming_attack_power(), 2);
    }

    #[test]
    fn a_blocked_spawn_cell_returns_its_power_to_the_pool_instead_of_losing_the_rock() {
        // 予告中にマスが塞がれた場合、岩を消さずに攻撃力として戻し、次ウェーブで再抽選する。
        let mut game = attack_rules_game(65);
        game.set_attack_blocks_per_rock(1);
        game.set_attack_rocks_per_wave_max(1);
        let spawn_row = spawn_row_of(&game);

        game.receive_incoming_attack(1, 0);
        let blocked_col = game.incoming_rocks()[0].pos.1;
        assert_eq!(game.incoming_attack_power(), 0, "前提: プールは空");

        // 予告が明ける直前まで進めてから、出現予定マスを塞ぐ。塞ぐのに使う岩自体も
        // 支えが無く、揺れ(SHAKE_DURATION_MS)を終えれば落ちて行ってしまうため、残り
        // 1msの時点で置いて、揺れている間に予告を明けさせる。
        game.update(Duration::from_millis(INCOMING_ROCK_WARNING_MS as u64 - 1));
        game.board
            .set(spawn_row, blocked_col, Cell::Rock { hits: 0 });
        let events = game.update(Duration::from_millis(1));

        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::IncomingRocksSpawned { .. })),
            "1個も出現していないので出現イベントは出ない"
        );
        // プールへ戻った1個分が、同じフレーム内で別の列の次ウェーブとして積み直される。
        assert_eq!(game.incoming_rocks().len(), 1, "繰り越されているはず");
        assert_ne!(
            game.incoming_rocks()[0].pos.1,
            blocked_col,
            "塞がれた列は候補から外れる"
        );
    }

    #[test]
    fn the_rock_and_bomb_shares_of_an_attack_are_absorbed_in_separate_pools() {
        // #304: 岩ぶん・ボムぶんは別勘定で相殺する。岩の溜め分でボムは消せない。
        let mut game = attack_rules_game(71);
        game.set_attack_blocks_per_rock(1);
        game.set_attack_blocks_per_bomb(1);
        game.set_attack_bombs_per_wave_max(2);
        game.attack_power_pending = 3;
        game.bomb_power_pending = 2;

        let receipt = game.receive_incoming_attack(1, 5);

        assert_eq!(receipt.rock_absorbed, 1);
        assert_eq!(receipt.rock_delivered, 0);
        assert_eq!(receipt.rocks_queued, 0);
        assert_eq!(
            receipt.bomb_absorbed, 2,
            "ボムはボムの溜め分だけで相殺される"
        );
        assert_eq!(receipt.bomb_delivered, 3);
        assert_eq!(receipt.bombs_queued, 2, "1ウェーブの上限まで降る");
        assert_eq!(
            game.attack_power_pending(),
            2,
            "岩の溜め分は相殺した分だけ減る"
        );
        assert_eq!(game.bomb_power_pending(), 0);
        assert_eq!(
            game.incoming_bomb_power(),
            1,
            "上限を超えた分はプールに残る"
        );
    }

    #[test]
    fn the_bomb_pool_turns_into_bombs_on_the_board_without_a_warning_stage() {
        // #304: ボムは岩の「予告→出現」を持たず、自然発生と同じくEnteringから始まる。
        let mut game = attack_rules_game(72);
        game.set_attack_blocks_per_bomb(5);
        game.set_attack_bombs_per_wave_max(2);

        let receipt = game.receive_incoming_attack(0, 12);

        assert_eq!(receipt.bombs_queued, 2);
        assert_eq!(game.bombs.len(), 2, "その場で盤面へ出現する");
        assert!(
            game.bombs
                .iter()
                .all(|bomb| bomb.phase == BombPhase::Entering),
            "自然発生ボムと同じ登場演出から始まる: {:?}",
            game.bombs
        );
        assert!(game.incoming_rocks().is_empty(), "岩の予告は使わない");
        assert_eq!(game.incoming_bomb_power(), 2, "端数はプールに残る");
    }

    #[test]
    fn an_incoming_bomb_wave_is_capped_and_the_rest_follows_in_later_waves() {
        // #304: 1ウェーブの上限を超える分はプールに残り、次のフレームで続きが降る。
        let mut game = attack_rules_game(73);
        game.set_attack_blocks_per_bomb(1);
        game.set_attack_bombs_per_wave_max(2);

        game.receive_incoming_attack(0, 5);
        assert_eq!(game.bombs.len(), 2, "1ウェーブは上限の2個まで");
        assert_eq!(game.incoming_bomb_power(), 3);

        game.update(Duration::from_millis(1));
        assert_eq!(game.bombs.len(), 4);
        assert_eq!(game.incoming_bomb_power(), 1);

        game.update(Duration::from_millis(1));
        assert_eq!(game.bombs.len(), 5);
        assert_eq!(game.incoming_bomb_power(), 0, "プールが尽きれば打ち止め");
    }

    #[test]
    fn no_bomb_wave_starts_while_the_board_already_holds_the_maximum_bombs() {
        // #304: 盤面のボム数上限は自然発生ぶんと共用する。埋まっている間はプールで待つ。
        let mut game = attack_rules_game(74);
        game.set_attack_blocks_per_bomb(1);
        game.set_attack_bombs_per_wave_max(1);
        for col in 0..BOMB_MAX_COUNT_ON_BOARD {
            game.bombs.push(Bomb {
                pos: (game.player.row, col + 1),
                origin: (game.player.row, 0),
                phase: BombPhase::Entering,
                phase_elapsed_ms: 0,
                remaining_ms: game.bomb_fuse_ms,
                settle_bounce_dir: 1,
            });
        }

        let receipt = game.receive_incoming_attack(0, 2);

        assert_eq!(receipt.bombs_queued, 0);
        assert_eq!(game.bombs.len(), BOMB_MAX_COUNT_ON_BOARD);
        assert_eq!(game.incoming_bomb_power(), 2, "プールは維持される");

        // 1個減れば、次のフレームでその1個だけが降る。
        game.bombs.pop();
        game.update(Duration::from_millis(1));

        assert_eq!(game.bombs.len(), BOMB_MAX_COUNT_ON_BOARD);
        assert_eq!(game.incoming_bomb_power(), 1);
    }

    #[test]
    fn splitting_the_pending_power_sends_everything_as_rocks_when_the_ratio_is_zero() {
        // #304: 比率0%は#304を入れる前と同じ挙動(岩だけが降る)。
        let mut game = attack_rules_game(75);
        game.set_attack_bomb_ratio_percent(0);
        game.attack_power_pending = 100;

        assert_eq!(game.take_pending_attack_power_split(), (100, 0));
        assert_eq!(game.attack_power_pending(), 0, "送信済み扱いで0に戻る");
    }

    #[test]
    fn splitting_the_pending_power_sends_everything_as_bombs_when_the_ratio_is_full() {
        // #304: 比率100%ならすべてボムぶんへ振り分かれる。
        let mut game = attack_rules_game(76);
        game.set_attack_bomb_ratio_percent(100);
        game.attack_power_pending = 100;

        assert_eq!(game.take_pending_attack_power_split(), (0, 100));
        assert_eq!(game.attack_power_pending(), 0);
    }

    #[test]
    fn splitting_the_pending_power_mixes_rocks_and_bombs_around_the_configured_ratio() {
        // #304: 1ポイントずつ判定するため個数はばらつくが、総量は必ず保たれる。
        let mut game = attack_rules_game(77);
        game.set_attack_bomb_ratio_percent(50);
        game.attack_power_pending = 1000;

        let (rock, bomb) = game.take_pending_attack_power_split();

        assert_eq!(rock + bomb, 1000, "振り分けで総量が増減しない");
        assert!(
            (350..=650).contains(&bomb),
            "50%指定なら大きく偏らない: rock={rock} bomb={bomb}"
        );
    }

    #[test]
    fn receiving_an_attack_is_a_no_op_outside_playing_and_warnings_do_not_progress() {
        // 一時停止中・ゲームオーバー中は攻撃を受け取らず、予告の残り時間も進まない。
        let mut game = attack_rules_game(66);
        game.set_attack_blocks_per_rock(1);
        game.receive_incoming_attack(1, 0);
        let remaining_before = game.incoming_rocks()[0].remaining_ms;

        game.status = GameStatus::Paused;
        assert_eq!(
            game.receive_incoming_attack(10, 10),
            AttackReceipt::default()
        );
        game.update(Duration::from_millis(INCOMING_ROCK_WARNING_MS as u64));
        assert_eq!(
            game.incoming_rocks()[0].remaining_ms,
            remaining_before,
            "一時停止中は予告が進まない"
        );

        game.status = GameStatus::GameOver;
        assert_eq!(
            game.receive_incoming_attack(10, 10),
            AttackReceipt::default()
        );
        game.update(Duration::from_millis(INCOMING_ROCK_WARNING_MS as u64));
        assert_eq!(
            game.incoming_rocks()[0].remaining_ms,
            remaining_before,
            "ゲームオーバー中も予告が進まない"
        );
    }

    #[test]
    fn rewinding_restores_the_attack_state_but_keeps_the_attack_settings() {
        // 攻撃力・受信待ちプール・予告中の岩は状態値なので巻き戻し、比率・上限は設定値
        // (`rewind_stock_max`と同じ扱い)なので巻き戻さない。
        let mut game = attack_rules_game(67);
        game.set_attack_blocks_per_rock(2);
        game.set_attack_rocks_per_wave_max(2);
        let snapshot = game.clone();

        game.attack_power_pending = 9;
        game.receive_incoming_attack(4, 0);
        assert!(!game.incoming_rocks().is_empty() || game.attack_power_pending() > 0);
        game.attack_power_pending = 9;
        game.incoming_attack_power = 1;
        game.set_attack_blocks_per_rock(7);
        game.set_attack_rocks_per_wave_max(3);

        game.restore_for_rewind(&snapshot);

        assert_eq!(game.attack_power_pending(), 0, "攻撃力は巻き戻る");
        assert_eq!(game.incoming_attack_power(), 0, "受信待ちプールも巻き戻る");
        assert!(game.incoming_rocks().is_empty(), "予告中の岩も巻き戻る");
        assert_eq!(game.attack_blocks_per_rock, 7, "設定値は巻き戻さない");
        assert_eq!(game.attack_rocks_per_wave_max, 3, "設定値は巻き戻さない");
    }

    #[test]
    fn rewinding_restores_the_bomb_attack_state_but_keeps_the_bomb_attack_settings() {
        // #304: ボム用の攻撃力・受信待ちプールも状態値なので巻き戻し、設定値3つは
        // 岩と同じく巻き戻さない。
        let mut game = attack_rules_game(70);
        game.set_attack_blocks_per_bomb(5);
        game.set_attack_bombs_per_wave_max(1);
        game.set_attack_bomb_ratio_percent(10);
        let snapshot = game.clone();

        game.bomb_power_pending = 9;
        game.incoming_bomb_power = 3;
        game.set_attack_blocks_per_bomb(8);
        game.set_attack_bombs_per_wave_max(3);
        game.set_attack_bomb_ratio_percent(50);

        game.restore_for_rewind(&snapshot);

        assert_eq!(game.bomb_power_pending(), 0, "ボムの攻撃力は巻き戻る");
        assert_eq!(game.incoming_bomb_power(), 0, "受信待ちプールも巻き戻る");
        assert_eq!(game.attack_blocks_per_bomb, 8, "設定値は巻き戻さない");
        assert_eq!(game.attack_bombs_per_wave_max, 3, "設定値は巻き戻さない");
        assert_eq!(game.attack_bomb_ratio_percent, 50, "設定値は巻き戻さない");
    }

    #[test]
    fn the_debug_shortcut_enables_the_rules_and_clears_the_pending_power() {
        // O キー: 初回押下で妨害ルールを有効化し、相殺後に残った自分の攻撃力は捨てる。
        let mut game = Game::new(68);
        clear_board(&mut game);
        game.set_bomb_spawn_rate_percent(0);
        game.player.row = game.board.depth_rows() - 1;
        game.player.col = 0;
        game.attack_power_pending = 5;

        game.debug_receive_opponent_attack();

        assert!(game.attack_rules_enabled(), "初回押下で有効化される");
        assert_eq!(game.attack_power_pending(), 0, "残った攻撃力は送信済み扱い");
        assert!(
            !game.incoming_rocks().is_empty(),
            "相殺後の残りが岩として降ってくる"
        );
    }

    #[test]
    fn the_debug_shortcut_is_a_no_op_outside_playing() {
        let mut game = Game::new(69);
        game.status = GameStatus::Paused;

        game.debug_receive_opponent_attack();

        assert!(!game.attack_rules_enabled());
        assert!(game.incoming_rocks().is_empty());
    }
}

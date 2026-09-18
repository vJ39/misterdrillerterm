//! 対戦のデシンク検出用の状態ハッシュ(#250。spec.md 12.3)。FNV-1a(64bit)の自前実装と
//! `Game::state_hash`を提供する。

use super::*;

/// FNV-1a(64bit)。標準ライブラリのSipHash(実行ごとにランダムシード)は決定性検証に
/// 使えないため、固定アルゴリズムを自前で実装する(#250)。対戦のデシンク検出
/// (`Game::state_hash`、spec.md 12.3)専用のヘルパーで、新規クレート依存は追加しない。
/// 通信層(#10本体)実装までは`state_hash`とテストからしか呼ばれないため、
/// `attack_power_pending`等と同じ理由でdead_code警告を抑止する。
#[allow(dead_code)]
struct Fnv1a64(u64);

#[allow(dead_code)]
impl Fnv1a64 {
    const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    fn new() -> Self {
        Self(Self::OFFSET_BASIS)
    }

    fn write_u8(&mut self, byte: u8) {
        self.0 ^= u64::from(byte);
        self.0 = self.0.wrapping_mul(Self::PRIME);
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u8(b);
        }
    }

    fn write_u32(&mut self, value: u32) {
        self.write_bytes(&value.to_le_bytes());
    }

    fn write_u64(&mut self, value: u64) {
        self.write_bytes(&value.to_le_bytes());
    }

    fn write_usize(&mut self, value: usize) {
        self.write_u64(value as u64);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

impl Game {
    /// 対戦のデシンク検出(#250。spec.md 12.3)用の状態ダイジェスト。シミュレーション
    /// 結果を代表する正準値列をFNV-1a(64bit)でハッシュする。演出専用のタイマー・
    /// 補間状態は含めない。同一シード・同一設定・同一の入力列であれば、2つの
    /// `Game`インスタンスは常に同じ値を返す。
    ///
    /// 通信層(#10本体)実装まではテストからしか呼ばれないため、上の`attack_rules_enabled`
    /// 等と同じ理由でdead_code警告を抑止する。
    #[allow(dead_code)]
    pub fn state_hash(&self) -> u64 {
        let mut h = Fnv1a64::new();

        // 盤面全セル(行→列の順)。
        for row in &self.board.rows {
            for cell in row {
                match *cell {
                    Cell::Empty => h.write_u8(0),
                    Cell::Color(color) => {
                        h.write_u8(1);
                        h.write_u8(match color {
                            ColorKind::Red => 0,
                            ColorKind::Blue => 1,
                            ColorKind::Green => 2,
                            ColorKind::Yellow => 3,
                        });
                    }
                    Cell::Rock { hits } => {
                        h.write_u8(2);
                        h.write_u8(hits);
                    }
                    Cell::Oxygen => h.write_u8(3),
                    Cell::Diamond => h.write_u8(4),
                    Cell::Star { visible_ms } => {
                        h.write_u8(5);
                        h.write_u32(visible_ms);
                    }
                    Cell::Item(effect) => {
                        h.write_u8(6);
                        h.write_u8(match effect {
                            ItemEffect::ClearAbove => 0,
                            ItemEffect::UnifyColors => 1,
                            ItemEffect::StarifyScreen => 2,
                        });
                    }
                }
            }
        }

        // プレイヤー: (row, col, facing, lives, depth)。
        h.write_usize(self.player.row);
        h.write_usize(self.player.col);
        h.write_u8(match self.player.facing {
            Direction::Up => 0,
            Direction::Down => 1,
            Direction::Left => 2,
            Direction::Right => 3,
        });
        h.write_u8(self.player.lives);
        h.write_usize(self.player.depth_m());

        // 酸素ゲージのビットパターン。
        h.write_u32(self.player.oxygen.to_bits());

        // スコア。
        h.write_u64(self.player.score);

        // 盤上のボム(位置・残りタイマーのみ。演出専用のphase/phase_elapsed_ms/
        // settle_bounce_dirは含めない)。
        for bomb in &self.bombs {
            h.write_usize(bomb.pos.0);
            h.write_usize(bomb.pos.1);
            h.write_u32(bomb.remaining_ms);
        }

        // アイテム窓補充フロンティア。
        h.write_usize(self.item_top_up_frontier_row);

        // 対戦の妨害ルール(#247)の状態。
        h.write_u32(self.attack_power_pending);
        h.write_u32(self.incoming_attack_power);
        for rock in &self.incoming_rocks {
            h.write_usize(rock.pos.0);
            h.write_usize(rock.pos.1);
            h.write_u32(rock.remaining_ms);
        }

        h.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::attack_rules_game;
    use super::*;

    // --- 状態ハッシュ(state_hash。#250。spec.md 12.3のデシンク検出用) -----------

    /// 決定性検証用の固定入力列。移動・向き変更・掘削を織り交ぜた数十ステップ。
    const STATE_HASH_TEST_INPUTS: &[InputAction] = &[
        InputAction::MoveRight,
        InputAction::Drill,
        InputAction::MoveRight,
        InputAction::MoveLeft,
        InputAction::FaceUp,
        InputAction::Drill,
        InputAction::FaceDown,
        InputAction::MoveLeft,
        InputAction::Drill,
        InputAction::MoveRight,
        InputAction::MoveRight,
        InputAction::FaceDown,
        InputAction::Drill,
        InputAction::MoveLeft,
        InputAction::MoveLeft,
        InputAction::FaceUp,
        InputAction::Drill,
        InputAction::MoveRight,
        InputAction::Drill,
        InputAction::MoveLeft,
        InputAction::FaceDown,
        InputAction::MoveRight,
        InputAction::Drill,
        InputAction::MoveLeft,
        InputAction::MoveRight,
        InputAction::FaceUp,
        InputAction::Drill,
        InputAction::MoveLeft,
        InputAction::MoveRight,
        InputAction::Drill,
    ];

    #[test]
    fn state_hash_matches_for_two_instances_given_the_same_seed_and_input_sequence() {
        // 同一シード・同一の固定入力列・同一の固定デルタ(spec.md 12.3のNET_TICK_MSと
        // 同値の150ms=FALL_TICK_MS)で2インスタンスを進めれば、各ステップ後の
        // state_hash()は常に一致するはず(対戦のデシンク検出が前提とする決定性)。
        let mut a = Game::new(4001);
        let mut b = Game::new(4001);
        assert_eq!(a.state_hash(), b.state_hash(), "初期状態は一致するはず");

        for (i, &action) in STATE_HASH_TEST_INPUTS.iter().enumerate() {
            a.apply_input(action);
            b.apply_input(action);
            a.update(Duration::from_millis(FALL_TICK_MS));
            b.update(Duration::from_millis(FALL_TICK_MS));
            assert_eq!(
                a.state_hash(),
                b.state_hash(),
                "ステップ{i}({action:?})後にstate_hashが一致しない"
            );
        }
    }

    #[test]
    fn state_hash_diverges_when_one_instance_gets_an_extra_input() {
        // 片方だけ余分にDrillを実行すると、以後state_hashが一致しなくなることを確認する
        // (ハッシュ関数が実際に状態変化を検知できることの確認。理論上まれに偶然衝突する
        // 可能性はゼロではないが実用上問題ない)。
        let mut a = Game::new(4002);
        let mut b = Game::new(4002);

        for &action in &STATE_HASH_TEST_INPUTS[..10] {
            a.apply_input(action);
            b.apply_input(action);
            a.update(Duration::from_millis(FALL_TICK_MS));
            b.update(Duration::from_millis(FALL_TICK_MS));
        }
        assert_eq!(a.state_hash(), b.state_hash(), "分岐前は一致しているはず");

        // bだけ余分にDrillを実行する。
        b.apply_input(InputAction::Drill);
        b.update(Duration::from_millis(FALL_TICK_MS));

        assert_ne!(
            a.state_hash(),
            b.state_hash(),
            "片方だけ余分な入力を与えたら一致しないはず"
        );
    }

    #[test]
    fn state_hash_is_pure_and_reproducible_from_a_clone() {
        // 同じGameをclone()して、両方に対し何も操作せずstate_hash()を呼ぶと同じ値になる
        // (関数自体が副作用を持たないことの確認)。同一インスタンスへの複数回呼び出しも
        // 同じ値を返すことを合わせて確認する。
        let mut game = Game::new(4003);
        game.apply_input(InputAction::MoveRight);
        game.update(Duration::from_millis(FALL_TICK_MS));

        let clone = game.clone();

        assert_eq!(game.state_hash(), clone.state_hash());
        assert_eq!(
            game.state_hash(),
            game.state_hash(),
            "同一インスタンスへの再呼び出しは同じ値を返すはず"
        );
    }

    #[test]
    fn state_hash_changes_when_a_bomb_is_present_on_the_board() {
        // 盤上のボム(位置・残りタイマー)がハッシュ計算に組み込まれていることを確認する。
        let base = Game::new(4004);
        let mut with_bomb = base.clone();
        with_bomb.bombs.push(Bomb {
            pos: (0, 0),
            origin: (0, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 1234,
            settle_bounce_dir: 1,
        });

        assert_ne!(
            base.state_hash(),
            with_bomb.state_hash(),
            "ボムの有無でstate_hashが変わるはず"
        );
    }

    #[test]
    fn state_hash_changes_when_attack_power_pending_differs() {
        // 対戦の妨害ルール(#247)の状態(attack_power_pending)がハッシュ計算に組み込まれて
        // いることを確認する。
        let a = attack_rules_game(4005);
        let mut b = a.clone();
        assert_eq!(a.state_hash(), b.state_hash(), "変更前は一致するはず");

        b.attack_power_pending = 3;

        assert_ne!(
            a.state_hash(),
            b.state_hash(),
            "attack_power_pendingの違いでstate_hashが変わるはず"
        );
    }
}

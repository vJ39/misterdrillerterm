//! ローカルlockstepハーネス(#251。spec.md 12.3)。
//!
//! ネットワーク対戦(#10)は、両ホストがそれぞれ「自分の盤面(`game_local`)」
//! 「相手の盤面(`game_remote`)」という視点の異なる2つの`Game`インスタンスを持ち、
//! 毎tick同じ固定順序でシミュレーションを進めることで盤面を同期する。本モジュールは
//! 実際の通信(#10本体)を行わず、この同期手順(`run_tick`)だけをローカルで再現し、
//! 決定性が成立していることをテストで検証するハーネスに留める。UIは持たない。
//!
//! N人対戦(#273)向けには、人数を可変にした`run_tick_n`を用意している(処理順序は
//! `run_tick`と同じ)。こちらは`BattleState`から実際に呼ばれる。

use std::time::Duration;

use crate::constants::NET_TICK_MS;
use crate::game::{Game, GameStatus, InputAction};

/// lockstepの1tickぶんの処理を、spec.md 12.3の固定実行順序で行う。
/// `local_action`/`remote_action`はNoneなら何もしない(1tickにつき高々1アクション)。
///
/// 通信層(#10本体)実装まではテストからしか呼ばれないため、`Game::state_hash`と
/// 同じ理由でdead_code警告を抑止する。
#[allow(dead_code)]
pub fn run_tick(
    game_local: &mut Game,
    game_remote: &mut Game,
    local_action: Option<InputAction>,
    remote_action: Option<InputAction>,
) {
    if let Some(action) = local_action {
        game_local.apply_input(action);
    }
    if let Some(action) = remote_action {
        game_remote.apply_input(action);
    }
    game_local.update(Duration::from_millis(NET_TICK_MS));
    game_remote.update(Duration::from_millis(NET_TICK_MS));

    // 妨害岩(#247/#297)。生存中(Playing)の相手にだけ、そのまま届ける。
    let local_power = game_local.take_pending_attack_power();
    let remote_power = game_remote.take_pending_attack_power();
    if remote_power > 0 && game_local.status == GameStatus::Playing {
        game_local.receive_incoming_attack(remote_power);
    }
    if local_power > 0 && game_remote.status == GameStatus::Playing {
        game_remote.receive_incoming_attack(local_power);
    }
}

/// lockstepの1tickぶんの処理をN人向けに一般化したもの(#273)。処理順序は`run_tick`と
/// 同じ(全員の入力適用 → 全員のupdate)で、人数だけが可変になる。
///
/// `games`と`actions`は同じindexで対応する(index 0が自分)。`actions`の要素がNoneなら
/// その参加者はこのtickで何もしない(1tickにつき高々1アクション。spec.md 12.2)。
pub fn run_tick_n(games: &mut [Game], actions: &[Option<InputAction>]) {
    debug_assert_eq!(
        games.len(),
        actions.len(),
        "参加者の数と入力の数は一致するはず"
    );
    for (game, &action) in games.iter_mut().zip(actions.iter()) {
        if let Some(action) = action {
            game.apply_input(action);
        }
    }
    for game in games.iter_mut() {
        game.update(Duration::from_millis(NET_TICK_MS));
    }

    // 妨害岩(#247/#297)。各自が消したブロック数を、割らずに生存中(Playing)の
    // 他の全員へそのまま届ける(N人時の配分ルールはユーザー確認済み)。
    let pending: Vec<u32> = games
        .iter_mut()
        .map(Game::take_pending_attack_power)
        .collect();
    for (i, &power) in pending.iter().enumerate() {
        if power == 0 {
            continue;
        }
        for (j, game) in games.iter_mut().enumerate() {
            if i != j && game.status == GameStatus::Playing {
                game.receive_incoming_attack(power);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ホストA視点(`a_local`=Aの操作を受ける、`a_remote`=Bの操作を受ける)と
    /// ホストB視点(`b_local`=Bの操作を受ける、`b_remote`=Aの操作を受ける)の
    /// 4インスタンスを同じtick数だけ並行して回し、各tick後に
    /// `a_local`⟷`b_remote`・`a_remote`⟷`b_local`が一致し続けることを確認する
    /// 共通ヘルパー。`actions`は各tickの(Aの入力, Bの入力)のタプル列。
    fn assert_lockstep_matches(seed: u64, actions: &[(Option<InputAction>, Option<InputAction>)]) {
        let mut a_local = Game::new(seed);
        let mut a_remote = Game::new(seed);
        let mut b_local = Game::new(seed);
        let mut b_remote = Game::new(seed);

        assert_eq!(
            a_local.state_hash(),
            b_remote.state_hash(),
            "初期状態(A.local/B.remote)は一致するはず"
        );
        assert_eq!(
            a_remote.state_hash(),
            b_local.state_hash(),
            "初期状態(A.remote/B.local)は一致するはず"
        );

        for (i, &(a_action, b_action)) in actions.iter().enumerate() {
            // A視点: localは自分(A)の入力、remoteは相手(B)の入力を受ける。
            run_tick(&mut a_local, &mut a_remote, a_action, b_action);
            // B視点: localは自分(B)の入力、remoteは相手(A)の入力を受ける。
            run_tick(&mut b_local, &mut b_remote, b_action, a_action);

            assert_eq!(
                a_local.state_hash(),
                b_remote.state_hash(),
                "tick{i}後にA.localとB.remoteが一致しない"
            );
            assert_eq!(
                a_remote.state_hash(),
                b_local.state_hash(),
                "tick{i}後にA.remoteとB.localが一致しない"
            );
        }
    }

    #[test]
    fn lockstep_keeps_both_hosts_views_in_sync_over_many_ticks() {
        // 基本ケース: AとBがそれぞれ異なる操作をしても、40 tickにわたって
        // 毎tick両視点のクロスチェックが一致し続けることを確認する。
        let actions: Vec<(Option<InputAction>, Option<InputAction>)> = (0..40)
            .map(|i| {
                let a = match i % 4 {
                    0 => Some(InputAction::MoveRight),
                    1 => Some(InputAction::Drill),
                    2 => None,
                    _ => Some(InputAction::FaceDown),
                };
                let b = match i % 3 {
                    0 => Some(InputAction::MoveLeft),
                    1 => Some(InputAction::Drill),
                    _ => None,
                };
                (a, b)
            })
            .collect();

        assert_lockstep_matches(9001, &actions);
    }

    #[test]
    fn lockstep_stays_in_sync_with_no_inputs_at_all() {
        // 何も操作しない(全tick None)場合でも、酸素減少・自由落下等のサブタイマー
        // だけで両視点の決定性が保たれることを確認する。
        let actions: Vec<(Option<InputAction>, Option<InputAction>)> = vec![(None, None); 30];
        assert_lockstep_matches(9002, &actions);
    }

    #[test]
    fn lockstep_stays_in_sync_with_drilling_and_movement_inputs() {
        // 掘削・移動・向き変更を織り交ぜた入力列で一致することを確認する。
        const A_INPUTS: &[InputAction] = &[
            InputAction::MoveRight,
            InputAction::Drill,
            InputAction::FaceDown,
            InputAction::Drill,
            InputAction::MoveRight,
            InputAction::MoveLeft,
            InputAction::FaceUp,
            InputAction::Drill,
            InputAction::MoveLeft,
            InputAction::Drill,
            InputAction::MoveRight,
            InputAction::Drill,
        ];
        const B_INPUTS: &[InputAction] = &[
            InputAction::MoveLeft,
            InputAction::FaceDown,
            InputAction::Drill,
            InputAction::MoveLeft,
            InputAction::Drill,
            InputAction::FaceUp,
            InputAction::MoveRight,
            InputAction::Drill,
            InputAction::MoveRight,
            InputAction::Drill,
            InputAction::MoveLeft,
            InputAction::Drill,
        ];
        assert_eq!(A_INPUTS.len(), B_INPUTS.len());

        let actions: Vec<(Option<InputAction>, Option<InputAction>)> = A_INPUTS
            .iter()
            .zip(B_INPUTS.iter())
            .map(|(&a, &b)| (Some(a), Some(b)))
            .collect();

        assert_lockstep_matches(9003, &actions);
    }

    // -----------------------------------------------------------------------
    // N人版(`run_tick_n`。#273)
    // -----------------------------------------------------------------------

    /// ホスト`h`が持つ`Vec<Game>`の並び順。自分を先頭(index 0)に置き、残りを番号順に
    /// 続ける(`BattleState`が「index 0が自分」とするのと同じ規約)。
    fn order_for(n: usize, h: usize) -> Vec<usize> {
        let mut order = vec![h];
        order.extend((0..n).filter(|&p| p != h));
        order
    }

    /// N人版のクロスチェック。参加者`n`人ぶんの「各ホストの視点」を作り、毎tick後に
    /// 同じ参加者のインスタンスが全ホストで一致し続けることを確認する
    /// (2人版`assert_lockstep_matches`のA.local⟷B.remote照合の一般化)。
    /// `actions`は各tickの参加者ごとの入力(長さ`n`)。
    fn assert_lockstep_n_matches(seed: u64, n: usize, actions: &[Vec<Option<InputAction>>]) {
        let mut views: Vec<Vec<Game>> = (0..n)
            .map(|_| (0..n).map(|_| Game::new(seed)).collect())
            .collect();

        // 同じ参加者のインスタンスをホスト0のものと突き合わせる。
        let assert_views_agree = |views: &[Vec<Game>], label: &str| {
            for p in 0..n {
                let hash_of = |h: usize| {
                    let order = order_for(n, h);
                    let index = order
                        .iter()
                        .position(|&q| q == p)
                        .expect("並び順には全参加者が含まれるはず");
                    views[h][index].state_hash()
                };
                let expected = hash_of(0);
                for h in 1..n {
                    assert_eq!(
                        expected,
                        hash_of(h),
                        "{label}: 参加者{p}のインスタンスがホスト0とホスト{h}で一致しない"
                    );
                }
            }
        };

        assert_views_agree(&views, "初期状態");

        for (i, tick_actions) in actions.iter().enumerate() {
            assert_eq!(tick_actions.len(), n, "各tickの入力は参加者数ぶん必要");
            for (h, view) in views.iter_mut().enumerate() {
                let ordered: Vec<Option<InputAction>> =
                    order_for(n, h).iter().map(|&p| tick_actions[p]).collect();
                run_tick_n(view, &ordered);
            }
            assert_views_agree(&views, &format!("tick{i}後"));
        }
    }

    #[test]
    fn run_tick_n_keeps_every_hosts_views_in_sync_over_many_ticks() {
        // 4人がそれぞれ異なる操作をしても、40 tickにわたって全ホストの視点が一致し
        // 続けることを確認する(2人版の基本ケースの一般化)。
        const N: usize = 4;
        let actions: Vec<Vec<Option<InputAction>>> = (0..40)
            .map(|i| {
                (0..N)
                    .map(|p| match (i + p) % 5 {
                        0 => Some(InputAction::MoveRight),
                        1 => Some(InputAction::Drill),
                        2 => None,
                        3 => Some(InputAction::FaceDown),
                        _ => Some(InputAction::MoveLeft),
                    })
                    .collect()
            })
            .collect();

        assert_lockstep_n_matches(9101, N, &actions);
    }

    #[test]
    fn run_tick_n_stays_in_sync_with_no_inputs_at_all() {
        // 3人で誰も操作しない場合でも、酸素減少・自由落下等のサブタイマーだけで
        // 全ホストの決定性が保たれることを確認する。
        const N: usize = 3;
        let actions: Vec<Vec<Option<InputAction>>> = vec![vec![None; N]; 30];

        assert_lockstep_n_matches(9102, N, &actions);
    }

    #[test]
    fn run_tick_n_produces_the_same_result_as_run_tick_for_two_players() {
        // N人版が2人版の正しい一般化であることの裏付け。同じ入力列を両者へ与え、
        // 毎tick後に状態が完全に一致することを確認する。
        let actions: Vec<(Option<InputAction>, Option<InputAction>)> = (0..30)
            .map(|i| {
                let a = match i % 4 {
                    0 => Some(InputAction::MoveRight),
                    1 => Some(InputAction::Drill),
                    2 => None,
                    _ => Some(InputAction::FaceDown),
                };
                let b = match i % 3 {
                    0 => Some(InputAction::MoveLeft),
                    1 => Some(InputAction::Drill),
                    _ => None,
                };
                (a, b)
            })
            .collect();

        const SEED: u64 = 9103;
        let mut pair_local = Game::new(SEED);
        let mut pair_remote = Game::new(SEED);
        let mut games = vec![Game::new(SEED), Game::new(SEED)];

        for (i, &(a_action, b_action)) in actions.iter().enumerate() {
            run_tick(&mut pair_local, &mut pair_remote, a_action, b_action);
            run_tick_n(&mut games, &[a_action, b_action]);

            assert_eq!(
                pair_local.state_hash(),
                games[0].state_hash(),
                "tick{i}後に自分の盤面が2人版と一致しない"
            );
            assert_eq!(
                pair_remote.state_hash(),
                games[1].state_hash(),
                "tick{i}後に相手の盤面が2人版と一致しない"
            );
        }
    }

    #[test]
    fn run_tick_n_delivers_drilled_blocks_as_attack_power_to_the_opponent() {
        // #247/#297: 掘削で消したブロック数が、同じtick内で生存中の相手へ攻撃力として届く。
        use crate::game::board::{Cell, ColorKind};
        use crate::game::player::Direction;

        let mut attacker = Game::new(1);
        attacker.set_attack_rules_enabled(true);
        attacker.player.facing = Direction::Down;
        let target_row = attacker.player.row + 1;
        let col = attacker.player.col;
        // 横に3個つながった同色を1回の掘削で消す(#247の実装と同じ前提)。
        attacker.board.rows[target_row][col] = Cell::Color(ColorKind::Red);
        attacker.board.rows[target_row][col + 1] = Cell::Color(ColorKind::Red);
        attacker.board.rows[target_row][col + 2] = Cell::Color(ColorKind::Red);

        let mut defender = Game::new(2);
        defender.set_attack_rules_enabled(true);

        let mut games = vec![attacker, defender];
        run_tick_n(&mut games, &[Some(InputAction::Drill), None]);

        assert_eq!(
            games[0].attack_power_pending(),
            0,
            "送った側の蓄積はtick内で消費されるはず"
        );
        assert_eq!(
            games[1].incoming_attack_power(),
            3,
            "3個分の攻撃力が生存中の相手へ届くはず"
        );
    }

    #[test]
    fn run_tick_n_does_not_send_attack_power_to_a_player_who_already_finished() {
        // ゴール・脱落済みの相手へは妨害岩を送らない(意味が無いため)。
        use crate::game::board::{Cell, ColorKind};
        use crate::game::player::Direction;

        let mut attacker = Game::new(1);
        attacker.set_attack_rules_enabled(true);
        attacker.player.facing = Direction::Down;
        let target_row = attacker.player.row + 1;
        let col = attacker.player.col;
        attacker.board.rows[target_row][col] = Cell::Color(ColorKind::Red);
        attacker.board.rows[target_row][col + 1] = Cell::Color(ColorKind::Red);

        let mut finished = Game::new(2);
        finished.set_attack_rules_enabled(true);
        finished.status = GameStatus::Cleared;

        let mut games = vec![attacker, finished];
        run_tick_n(&mut games, &[Some(InputAction::Drill), None]);

        assert_eq!(
            games[1].incoming_attack_power(),
            0,
            "ゴール済みの相手には妨害岩を送らないはず"
        );
    }
}

//! ローカルlockstepハーネス(#251。spec.md 12.3)。
//!
//! ネットワーク対戦(#10)は、両ホストがそれぞれ「自分の盤面(`game_local`)」
//! 「相手の盤面(`game_remote`)」という視点の異なる2つの`Game`インスタンスを持ち、
//! 毎tick同じ固定順序でシミュレーションを進めることで盤面を同期する。本モジュールは
//! 実際の通信(#10本体)を行わず、この同期手順(`run_tick`)だけをローカルで再現し、
//! 決定性が成立していることをテストで検証するハーネスに留める。UIは持たない。

use std::time::Duration;

use crate::constants::NET_TICK_MS;
use crate::game::{Game, InputAction};

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
}

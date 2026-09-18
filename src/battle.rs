//! 対戦用の状態(#252。spec.md 12章)。
//!
//! 通信を伴わない「画面・状態」だけを扱う。TCP層(#253)・通信スレッドとのInput交換
//! (#254)・StateHash配線(#255)・UDP探索とロビーUI(#256)はいずれも対象外で、この段階では
//! タイトルから`Screen::Battle`へ到達する入口も無い。ここでは「自分の盤面と相手の盤面を
//! 150ms固定tickでlockstep実行し、決着を確定する」状態遷移だけを持ち、検証はユニット
//! テストで行う。

use std::time::Duration;

use crate::constants::NET_TICK_MS;
use crate::game::{Game, GameStatus, InputAction};
use crate::lockstep;
use crate::net::BattleConfig;

/// 1フレームの実測時間としてtickへ繰り入れる上限(ms)。通常プレイ(`tick_playing`)が
/// `Game::update`へ渡すdeltaに掛けているクランプと同じ値で、ウィンドウ非アクティブ等で
/// 大きく空いたフレームが一度に大量のtickへ化けるのを防ぐ。
const DELTA_CLAMP_MS: u64 = 250;

/// 対戦の決着(spec.md 12.4)。自分視点で表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BattleOutcome {
    Win,
    Lose,
    Draw,
}

/// 対戦画面(`Screen::Battle`)が持つ状態。
///
/// `game_local`/`game_remote`は通常プレイと同じ`Game`で、両者を毎tick同じ固定順序で
/// 進めることで盤面を同期する(12.3「シミュレーション自体は省略してはならない」)。
pub struct BattleState {
    /// 自分の盤面。自分の入力を適用する。
    pub game_local: Game,
    /// 相手の盤面。相手の入力を適用する。
    pub game_remote: Game,
    /// 相手の表示名。対戦画面の相手パネルの見出しに使う。
    pub opponent_name: String,
    /// 実測フレーム時間を`NET_TICK_MS`(150ms)単位へ量子化するための蓄積バッファ。
    /// `lockstep::run_tick`は1回で150ms固定分しか進めないため、フレーム間隔が150msの
    /// 倍数からずれてもtickを取りこぼさないよう繰り越す。
    net_tick_accum: Duration,
    /// 決着。`Some`になった以後はtickを進めず、自分の入力も受け付けない。
    outcome: Option<BattleOutcome>,
}

impl BattleState {
    /// 対戦開始時の状態を作る。`Screen::Battle`への実際の遷移はハンドシェイク完了時
    /// (#254)・ロビーからの入口(#256)で作るため、この段階ではテストからのみ呼ばれる。
    #[allow(dead_code)]
    pub fn new(game_local: Game, game_remote: Game, opponent_name: String) -> Self {
        Self {
            game_local,
            game_remote,
            opponent_name,
            net_tick_accum: Duration::ZERO,
            outcome: None,
        }
    }

    /// 実測の経過時間`delta`を150ms固定tickへ量子化し、溜まったぶんだけlockstepを進める。
    ///
    /// `local_action`はこのフレームで確定した自分の入力(無ければ`None`)。1tickにつき
    /// 高々1アクション(12.2)のため、1フレームで複数tick進む場合も最初のtickだけが消費し、
    /// 残りのtickは`None`で進む。決着後(`outcome`が`Some`)は何もしない。
    pub fn advance(&mut self, delta: Duration, local_action: Option<InputAction>) {
        // 決着後は結果表示に専念し、盤面も入力も進めない(12.4)。
        if self.outcome.is_some() {
            return;
        }

        self.net_tick_accum += delta.min(Duration::from_millis(DELTA_CLAMP_MS));

        let net_tick = Duration::from_millis(NET_TICK_MS);
        let mut local_action = local_action;
        while self.net_tick_accum >= net_tick {
            self.net_tick_accum -= net_tick;
            self.run_net_tick(local_action.take());
            if self.outcome.is_some() {
                break;
            }
        }
    }

    /// lockstepの1tickぶんを進め、その結果から決着を確定する。
    fn run_net_tick(&mut self, local_action: Option<InputAction>) {
        lockstep::run_tick(
            &mut self.game_local,
            &mut self.game_remote,
            local_action,
            receive_remote_action(),
        );

        // 一度確定した決着は上書きしない(決着後は`advance`がtickを呼ばないため、
        // 実際にはまだ未決着のときだけ評価される)。
        if self.outcome.is_none() {
            self.outcome = resolve_outcome(self.game_local.status, self.game_remote.status);
        }
    }
}

/// `BattleConfig`とseedから、通常プレイの開始処理と同一順序でGameを1つ生成する
/// (#253。spec.md 12.2ステップ4)。ホスト・クライアントの双方がこの関数を同じ引数で
/// 呼ぶことで、4インスタンス(各ホスト2つ)の初期盤面が一致する。
///
/// 通常プレイの`start_new_game`(`main.rs`)が`Settings`から行っている反映を、`Settings`
/// ではなく`BattleConfig`から同じ並び(生成 → 速度系setter群 → 配分率の再抽選)で行う。
/// 巻き戻しストック・ブロック状態遷移ログは対戦では設定として共有しない(前者は対戦中
/// 無効、後者はシミュレーションに影響しないローカル専用。spec.md 12.5)ため、
/// `BattleConfig`にも含まれず、ここでも触らない。
///
/// `Screen::Battle`への実際の遷移は#254/#256で作るため、この段階ではテストからのみ
/// 呼ばれる(`BattleState::new`と同じ理由でdead_code警告を抑止する)。
#[allow(dead_code)]
pub fn new_game_from_battle_config(seed: u64, config: &BattleConfig) -> Game {
    let mut game = Game::new_with_width(
        seed,
        config.field_width as usize,
        config.depth_goal_m as usize,
    );
    game.set_block_fall_tick_ms(config.block_fall_tick_ms);
    game.set_player_fall_tick_ms(config.player_fall_tick_ms);
    game.set_shake_duration_ms(config.shake_duration_ms);
    game.set_dodge_recovery_ms(config.dodge_recovery_ms);
    game.set_move_cooldown_ms(config.move_cooldown_ms);
    game.set_bomb_spawn_rate_percent(config.bomb_spawn_rate_percent);
    game.set_bomb_fuse_ms(config.bomb_fuse_ms);
    game.set_chain_vanish_interval_ms(config.chain_vanish_interval_ms);
    game.set_attack_blocks_per_rock(config.attack_blocks_per_rock);
    game.set_attack_rocks_per_wave_max(config.attack_rocks_per_wave_max);
    // Xブロック/AIR/スター/ダイヤの配分率設定を、安全地帯明け(行2)以降の全体へ反映する。
    game.reroll_spawn_rates_from(
        2,
        config.rock_spawn_rate_percent,
        config.air_spawn_rate_percent,
        config.star_spawn_rate_percent,
        config.diamond_spawn_rate_percent,
        config.item_clear_above_rate_percent,
        config.item_unify_colors_rate_percent,
        config.item_starify_screen_rate_percent,
        config.color_count,
        config.color_cluster_rate_percent,
    );
    game
}

/// 相手の入力を1tickぶん受け取る。通信スレッドがまだ無い#252時点では常に`None`。
/// #254で通信スレッドの受信キュー(mpsc)からの取り出しへ差し替える。相手の入力を
/// 取得する箇所をこの関数1つに閉じているため、差し替えはここだけで済む。
fn receive_remote_action() -> Option<InputAction> {
    None
}

/// 自分・相手それぞれのゲーム状態から、自分視点の決着を判定する(12.4)。
///
/// ゴール到達(`Cleared`)は自分なら勝ち・相手なら負け、脱落(`GameOver`)はその逆になる。
/// 脱落時に通常プレイの「その場から復活」ダイアログは経由せず、即座に敗北扱いにする。
/// 同一tickで自分側の判定と相手側の判定が食い違った場合(両者同時ゴール・両者同時脱落)は
/// 引き分けにする。
fn resolve_outcome(local: GameStatus, remote: GameStatus) -> Option<BattleOutcome> {
    let from_local = match local {
        GameStatus::Cleared => Some(BattleOutcome::Win),
        GameStatus::GameOver => Some(BattleOutcome::Lose),
        GameStatus::Playing | GameStatus::Paused => None,
    };
    let from_remote = match remote {
        GameStatus::Cleared => Some(BattleOutcome::Lose),
        GameStatus::GameOver => Some(BattleOutcome::Win),
        GameStatus::Playing | GameStatus::Paused => None,
    };

    match (from_local, from_remote) {
        (Some(by_local), Some(by_remote)) if by_local != by_remote => Some(BattleOutcome::Draw),
        (Some(by_local), _) => Some(by_local),
        (None, by_remote) => by_remote,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::FIELD_WIDTH_DEFAULT;
    use crate::game::board::Cell;

    /// テスト用の短いコース(ゴール20m)。本番のノーマルコース(1000m)より盤面生成が軽く、
    /// ゴール到達も数tickで再現できる。
    const TEST_GOAL_M: usize = 20;

    fn net_tick() -> Duration {
        Duration::from_millis(NET_TICK_MS)
    }

    /// テスト用の`BattleConfig`。ゴールは上の短いコース(20m)に合わせる。
    fn test_battle_config() -> BattleConfig {
        BattleConfig::from_settings(&crate::settings::Settings::default(), TEST_GOAL_M)
    }

    /// テスト用の対戦状態。短いコースの`Game`を2つ持たせる。
    fn battle(seed_local: u64, seed_remote: u64) -> BattleState {
        BattleState::new(
            Game::new_with_width(seed_local, FIELD_WIDTH_DEFAULT, TEST_GOAL_M),
            Game::new_with_width(seed_remote, FIELD_WIDTH_DEFAULT, TEST_GOAL_M),
            "opponent".to_string(),
        )
    }

    /// ゴール(最深行)の1つ手前に立たせ、直下を空けて「次の自由落下でゴールへ着く」状態に
    /// する。
    fn place_just_above_goal(game: &mut Game) {
        let goal_row = TEST_GOAL_M - 1;
        game.player.row = goal_row - 1;
        game.board.rows[goal_row][game.player.col] = Cell::Empty;
    }

    /// 決着がつくまで(または上限`max_ticks`まで)1tickずつ進める。
    fn advance_until_outcome(state: &mut BattleState, max_ticks: usize) {
        for _ in 0..max_ticks {
            state.advance(net_tick(), None);
            if state.outcome.is_some() {
                return;
            }
        }
    }

    #[test]
    fn reaching_the_goal_first_wins() {
        // 自分が先にゴール到達したら勝ち。
        let mut state = battle(1, 2);
        place_just_above_goal(&mut state.game_local);

        advance_until_outcome(&mut state, 10);

        assert_eq!(
            state.game_local.status,
            GameStatus::Cleared,
            "前提: 自分がゴールに到達しているはず"
        );
        assert_eq!(
            state.game_remote.status,
            GameStatus::Playing,
            "前提: 相手はまだ決着していないはず"
        );
        assert_eq!(state.outcome, Some(BattleOutcome::Win));
    }

    #[test]
    fn the_opponent_dropping_out_first_wins() {
        // 相手が先に脱落(酸素切れ→ライフ0)したら自分の勝ち。自分の側が決着要因に
        // 混ざらないよう、自分の盤面は無敵にしておく。
        let mut state = battle(3, 4);
        state.game_local.set_invincible(true);
        state.game_remote.player.lives = 1;
        state.game_remote.player.oxygen = 1.0;

        // 酸素切れの後、「天に召される」演出(CRUSH_ASCEND_MS=3000ms)を経てGameOverに
        // なるため、150msのtickで十分な回数を回す。
        advance_until_outcome(&mut state, 60);

        assert_eq!(
            state.game_remote.status,
            GameStatus::GameOver,
            "前提: 相手が脱落しているはず"
        );
        assert_eq!(state.outcome, Some(BattleOutcome::Win));
    }

    #[test]
    fn both_reaching_the_goal_on_the_same_tick_is_a_draw() {
        // 同じ盤面(同じシード)で両者を同じ位置に置くと同一tickでゴール到達する。
        let mut state = battle(5, 5);
        place_just_above_goal(&mut state.game_local);
        place_just_above_goal(&mut state.game_remote);

        advance_until_outcome(&mut state, 10);

        assert_eq!(state.game_local.status, GameStatus::Cleared);
        assert_eq!(state.game_remote.status, GameStatus::Cleared);
        assert_eq!(state.outcome, Some(BattleOutcome::Draw));
    }

    #[test]
    fn dropping_out_first_loses() {
        // 自分が先に脱落したら負け(通常プレイの復活ダイアログは経由しない)。
        let mut state = battle(6, 7);
        state.game_remote.set_invincible(true);
        state.game_local.player.lives = 1;
        state.game_local.player.oxygen = 1.0;

        advance_until_outcome(&mut state, 60);

        assert_eq!(state.game_local.status, GameStatus::GameOver);
        assert_eq!(state.outcome, Some(BattleOutcome::Lose));
    }

    #[test]
    fn frame_deltas_shorter_than_one_net_tick_are_carried_over() {
        // 150msに満たないフレームではtickが起きず、繰り越した分と合わせて150msを
        // 超えた時点で1tick進む。
        let mut state = battle(8, 9);

        state.advance(Duration::from_millis(100), None);
        assert_eq!(
            state.game_local.debug_frame(),
            0,
            "150msに満たないのでまだtickは起きないはず"
        );
        assert_eq!(state.net_tick_accum, Duration::from_millis(100));

        state.advance(Duration::from_millis(100), None);
        assert_eq!(
            state.game_local.debug_frame(),
            1,
            "繰り越し分と合わせて150msを超えたら1tick進むはず"
        );
        assert_eq!(
            state.net_tick_accum,
            Duration::from_millis(50),
            "使い切らなかった端数は次フレームへ繰り越すはず"
        );
    }

    #[test]
    fn a_long_frame_delta_is_clamped_before_it_is_quantized() {
        // 大きく空いたフレームでも、クランプ(250ms)を超えた分はtickに化けない。
        let mut state = battle(10, 11);

        state.advance(Duration::from_secs(10), None);

        assert_eq!(
            state.game_local.debug_frame(),
            1,
            "250msにクランプされるので1tickぶんしか進まないはず"
        );
        assert_eq!(state.net_tick_accum, Duration::from_millis(100));
    }

    #[test]
    fn only_the_first_tick_of_a_frame_consumes_the_local_action() {
        // 1フレームで2tick進む場合でも、自分の入力が適用されるのは最初の1tickだけ。
        // 同じtick列を1tickずつ手で回したものと状態が一致することで確認する。
        let mut batched = battle(12, 13);
        batched.advance(Duration::from_millis(250), None); // 1tick進み100ms繰り越す
        batched.advance(Duration::from_millis(250), Some(InputAction::MoveRight)); // 2tick進む

        let mut stepwise = battle(12, 13);
        stepwise.run_net_tick(None);
        stepwise.run_net_tick(Some(InputAction::MoveRight));
        stepwise.run_net_tick(None);

        assert_eq!(
            batched.game_local.debug_frame(),
            3,
            "前提: 合計3tick進むはず"
        );
        assert_eq!(
            batched.game_local.state_hash(),
            stepwise.game_local.state_hash(),
            "2tick目以降にも入力が適用されていると状態が食い違う"
        );
    }

    #[test]
    fn no_tick_advances_after_the_outcome_is_decided() {
        // 決着後は入力を受け付けず、盤面も進めない。
        let mut state = battle(14, 15);
        place_just_above_goal(&mut state.game_local);
        advance_until_outcome(&mut state, 10);
        assert_eq!(state.outcome, Some(BattleOutcome::Win), "前提: 決着済み");

        let frames_at_outcome = state.game_local.debug_frame();
        state.advance(Duration::from_millis(250), Some(InputAction::MoveRight));

        assert_eq!(
            state.game_local.debug_frame(),
            frames_at_outcome,
            "決着後はtickが進まないはず"
        );
    }

    #[test]
    fn new_game_from_battle_config_builds_the_same_board_for_the_same_arguments() {
        // 同一シード・同一設定なら、ホスト側とクライアント側で別々に生成しても
        // 初期盤面が完全に一致する(lockstepの前提。spec.md 12.2ステップ4)。
        let config = test_battle_config();

        let host_side = new_game_from_battle_config(4242, &config);
        let client_side = new_game_from_battle_config(4242, &config);

        assert_eq!(host_side.state_hash(), client_side.state_hash());
    }

    #[test]
    fn new_game_from_battle_config_applies_the_field_width_and_goal_depth() {
        let config = BattleConfig {
            field_width: 10,
            ..test_battle_config()
        };

        let game = new_game_from_battle_config(1, &config);

        assert_eq!(game.board.rows[0].len(), 10);
        assert_eq!(game.depth_goal_m(), TEST_GOAL_M);
    }

    #[test]
    fn new_game_from_battle_config_reflects_the_seed_and_the_spawn_rate_settings() {
        // 引数が効いていること(同じ値を返すだけの実装になっていないこと)を、
        // シードと配分率をそれぞれ変えて確認する。
        let config = test_battle_config();
        let base = new_game_from_battle_config(1, &config);

        assert_ne!(
            base.state_hash(),
            new_game_from_battle_config(2, &config).state_hash(),
            "シードが違えば盤面も変わるはず"
        );

        let denser_rocks = BattleConfig {
            rock_spawn_rate_percent: 300,
            ..config
        };
        assert_ne!(
            base.state_hash(),
            new_game_from_battle_config(1, &denser_rocks).state_hash(),
            "配分率の設定が盤面へ反映されているはず"
        );
    }

    #[test]
    fn resolve_outcome_covers_every_combination_of_statuses() {
        use BattleOutcome::{Draw, Lose, Win};
        use GameStatus::{Cleared, GameOver, Paused, Playing};

        assert_eq!(resolve_outcome(Playing, Playing), None);
        assert_eq!(resolve_outcome(Paused, Playing), None);
        assert_eq!(resolve_outcome(Cleared, Playing), Some(Win));
        assert_eq!(resolve_outcome(GameOver, Playing), Some(Lose));
        assert_eq!(resolve_outcome(Playing, Cleared), Some(Lose));
        assert_eq!(resolve_outcome(Playing, GameOver), Some(Win));
        // 同一tickで両者が同じ結末を迎えた場合は引き分け。
        assert_eq!(resolve_outcome(Cleared, Cleared), Some(Draw));
        assert_eq!(resolve_outcome(GameOver, GameOver), Some(Draw));
        // 自分がゴール・相手が脱落なら、どちらの判定でも自分の勝ち。
        assert_eq!(resolve_outcome(Cleared, GameOver), Some(Win));
        assert_eq!(resolve_outcome(GameOver, Cleared), Some(Lose));
    }
}

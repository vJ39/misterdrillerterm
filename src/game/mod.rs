//! ゲーム全体のオーケストレーション(盤面+プレイヤー+タイマー類)。
//!
//! board/player/physics は副作用のない純粋なロジックだが、この`Game`はそれらを
//! 「1フレーム進める」「1回入力を処理する」という時間軸に沿ってまとめ、UI/audio層が
//! 反応すべき`GameEvent`列を返す薄いオーケストレーション層。

pub mod board;
pub mod physics;
pub mod player;

use std::time::Duration;

use crate::constants::{
    CRUSH_FLASH_MS, FALL_TICK_MS, FIELD_DEPTH_M, INPUT_COOLDOWN_MS, INVULNERABILITY_TICKS, LIVES_DEFAULT,
    MOVE_ANIM_DURATION_MS, OXYGEN_WARNING_THRESHOLD,
};
use board::{Board, GravityState};
use physics::{DrillOutcome, LateralOutcome};
use player::{Direction, Player};

/// キー入力から得られるゲーム側のアクション(spec.md 1章)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    /// facingをLeftにし、掘削を伴わない地形追従の移動を試みる(隣が空なら移動、
    /// 塞がっていて1段上が空なら1段登る、どちらも塞がっていればその場に留まる)
    MoveLeft,
    /// facingをRightにし、掘削を伴わない地形追従の移動を試みる(左右対称。詳細はMoveLeftを参照)
    MoveRight,
    /// facingをUpに変更するのみ(移動・掘削は発生しない)
    FaceUp,
    /// facingをDownに変更するのみ(移動・掘削は発生しない)
    FaceDown,
    /// 現在のfacing方向のセルを、移動を伴わずに掘削する
    Drill,
    /// 一時停止/再開のトグル
    TogglePause,
    /// タイトル画面へ戻る(タイトル画面自体で押された場合のみアプリを終了する。
    /// この解釈はGameの外側=main.rsの画面遷移が担う)
    Quit,
    /// サウンド(SE+BGM)のON/OFF切り替え(TERM独自拡張)。タイトル画面・一時停止画面
    /// でのみ意味を持つ。Gameの内部状態には影響しないため、この解釈もGameの外側=main.rsが担う
    ToggleSound,
}

/// ゲーム全体の進行状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameStatus {
    Playing,
    Paused,
    GameOver,
    Cleared,
}

/// 1回のupdate/入力処理で発生したイベント。UIの効果音再生・演出判断に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameEvent {
    /// 掘削入力が実際にブロックへ命中した瞬間(色ブロックの直接掘削、または岩ブロックへの
    /// ヒット。命中しなかった移動・空振りでは発生しない。spec.md 10章「掘削音」)
    DrillImpact,
    /// 岩ブロックへヒットしたが、まだ破壊に至らない(spec.md 10章「岩ブロックヒット音」)
    RockHitIntact,
    /// ブロックが消滅した(直接掘削消滅・自動消滅・岩の5回目破壊のいずれも。
    /// spec.md 10章「破壊音」)。消滅したブロック数を伴う
    BlockDestroyed { blocks: usize },
    /// 酸素カプセルを取得した
    OxygenCollected,
    /// ダイヤブロックを取得した
    DiamondCollected,
    /// 酸素残量が警告閾値以下の間、1秒間隔で発生
    OxygenWarningTick,
    /// レベル(30mごと)が上がった
    LevelUp { level: usize },
    /// ライフを1つ失ったが、まだライフが残っている(その場で酸素全回復して再開)
    LifeLost,
    /// 最後のライフを失い、ゲームオーバーになった
    GameOverMiss,
    /// 深度1000m到達でゲームクリアした
    Cleared,
}

/// ノーマルコース シングルプレイのゲーム状態一式。
pub struct Game {
    pub board: Board,
    pub player: Player,
    pub status: GameStatus,
    gravity_state: GravityState,
    fall_tick_accum: Duration,
    input_cooldown_remaining: Duration,
    oxygen_warning_accum: Duration,
    /// ライフ消費で再開した直後、残り何ティックの間 押し潰し判定を無効化するか
    /// (spec.md 5章末尾、TERM独自拡張)。
    invulnerability_ticks_remaining: u32,
    /// 直近でGameEvent::LevelUpを通知した時点のレベル番号(重複通知防止)。
    last_level_reported: usize,
    /// 押し潰しミス発生時、残りこれだけの間「潰れた」見た目を表示し続ける
    /// (0になったらGameOverオーバーレイの表示を許す。TERM独自拡張、9章)。
    crush_flash_remaining: Duration,
    /// 描画専用: プレイヤーの直前の論理位置(移動の見た目補間アニメーション用、
    /// TERM独自拡張、9章)。ロジック上の当たり判定・掘削・落下判定には一切使わない。
    render_prev_position: (usize, usize),
    /// 直前の論理位置変化からの経過時間(秒)。`MOVE_ANIM_DURATION_MS`に達すると
    /// 補間が完了したものとして扱う。
    render_anim_elapsed: f32,
}

impl Game {
    /// 指定シードで、既定ライフ数の新しいゲームを開始する。
    pub fn new(seed: u64) -> Self {
        Self::new_with_lives(seed, LIVES_DEFAULT)
    }

    /// 指定シード・ライフ数で新しいゲームを開始する(spec.md 8章「1〜5機から選べる」)。
    pub fn new_with_lives(seed: u64, lives: u8) -> Self {
        let player = Player::with_lives(lives);
        let last_level_reported = player.level();
        let start_position = player.position();
        Game {
            board: Board::generate(seed, FIELD_DEPTH_M),
            player,
            status: GameStatus::Playing,
            gravity_state: GravityState::new(),
            fall_tick_accum: Duration::ZERO,
            input_cooldown_remaining: Duration::ZERO,
            oxygen_warning_accum: Duration::ZERO,
            invulnerability_ticks_remaining: 0,
            last_level_reported,
            crush_flash_remaining: Duration::ZERO,
            render_prev_position: start_position,
            // 開始時点では補間の必要が無いため、既に完了した扱いにしておく
            // (さもないと初期表示が(0,0)相当からアニメーションしてしまう)。
            render_anim_elapsed: move_anim_duration_secs(),
        }
    }

    /// P キー: 一時停止/再開のトグル。GameOver/Cleared中は無効。
    pub fn toggle_pause(&mut self) {
        self.status = match self.status {
            GameStatus::Playing => GameStatus::Paused,
            GameStatus::Paused => GameStatus::Playing,
            other => other,
        };
    }

    /// ← キー: facingをLeftにし、掘削を伴わない地形追従の移動を試みる(spec.md 1章)。
    pub fn try_move_left(&mut self) -> Vec<GameEvent> {
        self.try_lateral_move(Direction::Left)
    }

    /// → キー: facingをRightにし、掘削を伴わない地形追従の移動を試みる(spec.md 1章)。
    pub fn try_move_right(&mut self) -> Vec<GameEvent> {
        self.try_lateral_move(Direction::Right)
    }

    /// ←/→ 共通の処理本体。掘削は一切発生しないため、原則としてSE再生等の`GameEvent`は
    /// 生じないが、移動先が酸素カプセルだった場合のみ取得イベントを発火する
    /// (TERM独自拡張、spec.md 1章)。
    fn try_lateral_move(&mut self, dir: Direction) -> Vec<GameEvent> {
        if !self.consume_input_cooldown() {
            return Vec::new();
        }

        let before = self.player.position();
        let outcome = physics::move_lateral(&mut self.board, &mut self.player, dir);
        self.note_possible_move(before);

        match outcome {
            LateralOutcome::MovedLevelAndCollectedOxygen | LateralOutcome::ClimbedStepAndCollectedOxygen => {
                vec![GameEvent::OxygenCollected]
            }
            _ => Vec::new(),
        }
    }

    /// ↑ キー: facingをUpに変更するのみ(移動・掘削は発生しない。spec.md 1章)。
    ///
    /// Left/Rightの2ステップ段差登り(TERM独自拡張)における「ぶつかって停止中」の
    /// 状態もリセットする(方向キーを挟んだ場合の扱い)。
    pub fn face_up(&mut self) {
        if self.status == GameStatus::Playing {
            self.player.facing = Direction::Up;
            self.player.bumped_direction = None;
        }
    }

    /// ↓ キー: facingをDownに変更するのみ(移動・掘削は発生しない。spec.md 1章)。
    ///
    /// Left/Rightの2ステップ段差登り(TERM独自拡張)における「ぶつかって停止中」の
    /// 状態もリセットする(方向キーを挟んだ場合の扱い)。
    pub fn face_down(&mut self) {
        if self.status == GameStatus::Playing {
            self.player.facing = Direction::Down;
            self.player.bumped_direction = None;
        }
    }

    /// Space キー: facing方向のセルを移動を伴わずに掘削する(spec.md 1章)。
    pub fn try_drill(&mut self) -> Vec<GameEvent> {
        let mut events = Vec::new();
        if !self.consume_input_cooldown() {
            return events;
        }

        let before = self.player.position();
        let outcome = physics::drill_facing(&mut self.board, &mut self.player);
        self.push_drill_outcome_events(outcome, &mut events);
        self.note_possible_move(before);

        if self.player.row != before.0 {
            self.check_level_and_clear(&mut events);
        }
        events
    }

    /// 入力クールダウン(spec.md 9.9)が明けているかを確認し、明けていればリセットする。
    /// Playing状態でない場合、またはクールダウン中は`false`を返す。
    fn consume_input_cooldown(&mut self) -> bool {
        if self.status != GameStatus::Playing {
            return false;
        }
        if self.input_cooldown_remaining > Duration::ZERO {
            return false;
        }
        self.input_cooldown_remaining = Duration::from_millis(INPUT_COOLDOWN_MS);
        true
    }

    /// `DrillOutcome`をSE再生用の`GameEvent`列へ変換し、酸素切れが発生していれば
    /// ライフ処理も行う。
    fn push_drill_outcome_events(&mut self, outcome: DrillOutcome, events: &mut Vec<GameEvent>) {
        match outcome {
            DrillOutcome::OutOfBounds | DrillOutcome::NoEffect => {}
            DrillOutcome::RockHitIntact => {
                events.push(GameEvent::DrillImpact);
                events.push(GameEvent::RockHitIntact);
            }
            DrillOutcome::RockDestroyed { blocks } => {
                events.push(GameEvent::DrillImpact);
                events.push(GameEvent::BlockDestroyed { blocks });
                self.check_oxygen_zero(events);
            }
            DrillOutcome::ColorDestroyed { blocks } => {
                events.push(GameEvent::DrillImpact);
                events.push(GameEvent::BlockDestroyed { blocks });
            }
            DrillOutcome::CollectedOxygen => events.push(GameEvent::OxygenCollected),
            DrillOutcome::CollectedDiamond => events.push(GameEvent::DiamondCollected),
        }
    }

    /// 酸素が0になっていればミス処理(ライフ喪失/ゲームオーバー)を行う。
    fn check_oxygen_zero(&mut self, events: &mut Vec<GameEvent>) {
        if self.status != GameStatus::Playing {
            return;
        }
        if self.player.is_out_of_oxygen() {
            self.apply_miss(events, false);
        }
    }

    /// ミス(酸素切れ/押し潰し)を処理する(spec.md 8章)。ライフが残っていれば
    /// 無敵時間を設定して続行、無くなっていればゲームオーバーにする。
    ///
    /// `is_crush`が`true`(=落下ブロックに押し潰された)場合は、GameOverオーバーレイの
    /// 表示前に一呼吸置く「潰れた」演出を開始する(TERM独自拡張、9章)。酸素切れ由来の
    /// ミスではこの演出は行わない。
    fn apply_miss(&mut self, events: &mut Vec<GameEvent>, is_crush: bool) {
        if is_crush {
            self.crush_flash_remaining = Duration::from_millis(CRUSH_FLASH_MS);
        }

        let game_over = self.player.lose_life();
        if game_over {
            self.status = GameStatus::GameOver;
            events.push(GameEvent::GameOverMiss);
        } else {
            self.invulnerability_ticks_remaining = INVULNERABILITY_TICKS;
            events.push(GameEvent::LifeLost);
        }
    }

    /// レベルアップ・ゲームクリアを判定する(spec.md 7.1・8章)。深度(=row)が変化した
    /// 場合にのみ呼ぶ。
    fn check_level_and_clear(&mut self, events: &mut Vec<GameEvent>) {
        let level = self.player.level();
        if level > self.last_level_reported {
            self.last_level_reported = level;
            events.push(GameEvent::LevelUp { level });
        }

        if self.status == GameStatus::Playing && self.player.depth_m() >= FIELD_DEPTH_M {
            self.status = GameStatus::Cleared;
            events.push(GameEvent::Cleared);
        }
    }

    /// メインループから毎フレーム呼ぶ。deltaぶんの時間経過(酸素減少・落下tick)を反映する。
    pub fn update(&mut self, delta: Duration) -> Vec<GameEvent> {
        let mut events = Vec::new();

        // 押し潰し演出・移動補間の経過時間は、GameOverでPlaying状態を抜けた後も
        // 描画側が最後まで追従できるよう、Playingガードより前に進めておく。
        self.crush_flash_remaining = self.crush_flash_remaining.saturating_sub(delta);
        self.render_anim_elapsed += delta.as_secs_f32();

        if self.status != GameStatus::Playing {
            return events;
        }

        self.player.elapsed_seconds += delta.as_secs_f32();

        if self.input_cooldown_remaining > Duration::ZERO {
            self.input_cooldown_remaining = self.input_cooldown_remaining.saturating_sub(delta);
        }

        physics::apply_oxygen_decay(&mut self.player, delta.as_secs_f32());

        if self.player.oxygen > 0.0 && self.player.oxygen <= OXYGEN_WARNING_THRESHOLD {
            self.oxygen_warning_accum += delta;
            if self.oxygen_warning_accum >= Duration::from_secs(1) {
                self.oxygen_warning_accum -= Duration::from_secs(1);
                events.push(GameEvent::OxygenWarningTick);
            }
        } else {
            self.oxygen_warning_accum = Duration::ZERO;
        }

        if self.player.is_out_of_oxygen() {
            self.apply_miss(&mut events, false);
            if self.status != GameStatus::Playing {
                return events;
            }
        }

        self.fall_tick_accum += delta;
        let tick = Duration::from_millis(FALL_TICK_MS);
        while self.fall_tick_accum >= tick {
            self.fall_tick_accum -= tick;

            let invulnerable = self.invulnerability_ticks_remaining > 0;
            let result =
                physics::process_gravity_tick(&mut self.board, &mut self.player, &mut self.gravity_state, invulnerable);
            if invulnerable {
                self.invulnerability_ticks_remaining -= 1;
            }

            if result.auto_vanished_blocks > 0 {
                events.push(GameEvent::BlockDestroyed {
                    blocks: result.auto_vanished_blocks,
                });
            }
            if result.auto_vanished_rock_blocks > 0 {
                // 岩ブロックの自動消滅は得点対象外だが、破壊音は色ブロックと同様に鳴らす
                // (spec.md 4.9・10章)。
                events.push(GameEvent::BlockDestroyed {
                    blocks: result.auto_vanished_rock_blocks,
                });
            }

            if result.life_lost_to_crush {
                self.apply_miss(&mut events, true);
            }

            if self.status != GameStatus::Playing {
                break;
            }

            // プレイヤー自身の自由落下(spec.md 1章、TERM独自拡張)。入力の有無や掘削とは
            // 無関係に、支えを失っていれば(直下がEmptyなら)このティックで1マス落下する。
            let before_fall = self.player.position();
            physics::apply_player_free_fall(&self.board, &mut self.player);
            self.note_possible_move(before_fall);
            if self.player.row != before_fall.0 {
                self.check_level_and_clear(&mut events);
                if self.status != GameStatus::Playing {
                    break;
                }
            }
        }

        events
    }

    /// プレイヤーの位置が`before`から変化していれば、移動の見た目補間アニメーションを
    /// (描画専用の状態として)開始する。ロジック上の位置(row/col)には一切影響しない
    /// (TERM独自拡張、9章)。
    fn note_possible_move(&mut self, before: (usize, usize)) {
        let after = self.player.position();
        if after != before {
            self.render_prev_position = before;
            self.render_anim_elapsed = 0.0;
        }
    }

    /// 描画側が使う、移動補間の進捗(0.0=直前位置にいる, 1.0=現在位置に到達済み)。
    pub fn move_anim_progress(&self) -> f32 {
        (self.render_anim_elapsed / move_anim_duration_secs()).clamp(0.0, 1.0)
    }

    /// 描画側が使う、移動補間の起点(直前の論理位置)。
    pub fn render_prev_position(&self) -> (usize, usize) {
        self.render_prev_position
    }

    /// 押し潰しの「潰れた」演出が表示中かどうか(GameOverオーバーレイの表示可否判定にも使う)。
    pub fn crush_flash_active(&self) -> bool {
        self.crush_flash_remaining > Duration::ZERO
    }
}

/// 移動補間アニメーションの長さ(秒)。
fn move_anim_duration_secs() -> f32 {
    MOVE_ANIM_DURATION_MS as f32 / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use board::{Cell, ColorKind};
    use crate::constants::{ROCK_HITS_TO_BREAK, SHAKE_TICKS};

    #[test]
    fn reaching_goal_depth_via_drill_clears_the_game() {
        let mut game = Game::new(1);
        game.player.row = FIELD_DEPTH_M - 2;
        game.player.facing = Direction::Down;
        let last_row = FIELD_DEPTH_M - 1;
        game.board.rows[last_row][game.player.col] = Cell::Empty;

        let events = game.try_drill();

        assert_eq!(game.status, GameStatus::Cleared);
        assert!(events.iter().any(|e| matches!(e, GameEvent::Cleared)));
    }

    #[test]
    fn oxygen_running_out_during_update_costs_a_life_and_continues() {
        let mut game = Game::new(2);
        game.player.oxygen = 1.0;
        let lives_before = game.player.lives;

        let events = game.update(Duration::from_secs(1));

        assert_eq!(game.status, GameStatus::Playing);
        assert_eq!(game.player.lives, lives_before - 1);
        assert_eq!(game.player.oxygen, crate::constants::OXYGEN_MAX);
        assert!(events.iter().any(|e| matches!(e, GameEvent::LifeLost)));
    }

    #[test]
    fn oxygen_running_out_on_last_life_ends_the_game() {
        let mut game = Game::new_with_lives(2, 1);
        game.player.oxygen = 1.0;

        let events = game.update(Duration::from_secs(1));

        assert_eq!(game.status, GameStatus::GameOver);
        assert!(events.iter().any(|e| matches!(e, GameEvent::GameOverMiss)));
    }

    #[test]
    fn input_cooldown_blocks_rapid_repeated_moves() {
        let mut game = Game::new(3);
        let col_before = game.player.col;

        game.try_move_right();
        let col_after_first = game.player.col;
        game.try_move_right(); // クールダウン中なので無視される

        assert_eq!(game.player.col, col_after_first);
        assert_ne!(col_before, col_after_first);
    }

    #[test]
    fn face_up_resets_the_bumped_direction() {
        // 上下の向き変更を挟んだ場合、Left/Rightの「ぶつかって停止中」の状態はリセット
        // され、次に同じ方向へ入力してもいきなりは登れない(実装者判断、spec.md 1章)。
        let mut game = Game::new(4);
        game.player.bumped_direction = Some(Direction::Right);

        game.face_up();

        assert_eq!(game.player.bumped_direction, None);
    }

    #[test]
    fn face_up_and_face_down_do_not_move_the_player() {
        let mut game = Game::new(4);
        let pos_before = game.player.position();

        game.face_up();
        assert_eq!(game.player.facing, Direction::Up);
        assert_eq!(game.player.position(), pos_before);

        game.face_down();
        assert_eq!(game.player.facing, Direction::Down);
        assert_eq!(game.player.position(), pos_before);
    }

    #[test]
    fn level_up_event_fires_once_when_crossing_a_level_boundary() {
        let mut game = Game::new(5);
        game.player.row = crate::constants::LEVEL_STEP_M - 1; // depth=30, level=1のまま
        game.player.facing = Direction::Down;
        game.board.rows[game.player.row + 1][game.player.col] = Cell::Empty;

        let events = game.try_drill(); // depth=31 -> level 2へ

        assert!(events.iter().any(|e| matches!(e, GameEvent::LevelUp { level: 2 })));
    }

    #[test]
    fn move_right_never_drills_and_climbs_over_a_blocking_color_block_on_second_press() {
        // カーソルキー(MoveLeft/MoveRight)は掘削を一切行わない。隣が塞がっていると、
        // 1回目の入力ではぶつかって停止するだけで登らず、同じ方向への2回目の入力で
        // 初めて1段上(row-1)へ登る(ユーザー指摘による2ステップ仕様)。ブロックは
        // どちらの場合も破壊されない。
        let mut game = Game::new(6);
        game.player.row = 1;
        let target_col = game.player.col + 1;
        game.board.rows[game.player.row][target_col] = Cell::Color(ColorKind::Red);
        // row 0(1段上)は生成上つねにEmpty(安全地帯、spec.md 3.2)

        let first_events = game.try_move_right(); // 1回目: ぶつかって停止

        assert_eq!(game.player.row, 1); // まだ登っていない
        assert_eq!(game.player.col, target_col - 1); // まだ移動していない
        assert_eq!(game.player.facing, Direction::Right);
        assert!(first_events.is_empty());

        game.input_cooldown_remaining = Duration::ZERO; // クールダウンを明ける(本テストの本題ではない)
        let second_events = game.try_move_right(); // 2回目: 同じ方向への再入力で登る

        assert_eq!(game.player.row, 0); // 1段登った
        assert_eq!(game.player.col, target_col);
        assert_eq!(game.player.facing, Direction::Right);
        assert_eq!(game.player.score, 0); // 掘削していないので加点なし
        assert!(second_events.is_empty()); // 掘削・破壊イベントは一切発生しない
        assert_eq!(game.board.cell(1, target_col), Cell::Color(ColorKind::Red)); // ブロックは残る
    }

    #[test]
    fn move_left_never_drills_and_climbs_over_a_blocking_color_block_on_second_press() {
        // move_right版と左右対称の確認(Gameの公開API try_move_leftを経由した統合テスト)。
        let mut game = Game::new(60);
        game.player.row = 1;
        let target_col = game.player.col - 1;
        game.board.rows[game.player.row][target_col] = Cell::Color(ColorKind::Green);
        // row 0(1段上)は生成上つねにEmpty(安全地帯、spec.md 3.2)

        let first_events = game.try_move_left(); // 1回目: ぶつかって停止

        assert_eq!(game.player.row, 1); // まだ登っていない
        assert_eq!(game.player.col, target_col + 1); // まだ移動していない
        assert_eq!(game.player.facing, Direction::Left);
        assert!(first_events.is_empty());

        game.input_cooldown_remaining = Duration::ZERO; // クールダウンを明ける(本テストの本題ではない)
        let second_events = game.try_move_left(); // 2回目: 同じ方向への再入力で登る

        assert_eq!(game.player.row, 0); // 1段登った
        assert_eq!(game.player.col, target_col);
        assert_eq!(game.player.facing, Direction::Left);
        assert_eq!(game.player.score, 0); // 掘削していないので加点なし
        assert!(second_events.is_empty()); // 掘削・破壊イベントは一切発生しない
        assert_eq!(game.board.cell(1, target_col), Cell::Color(ColorKind::Green)); // ブロックは残る
    }

    #[test]
    fn try_move_right_into_oxygen_capsule_collects_it_and_emits_event() {
        // task2(ユーザー指摘): AIRカプセルは掘削不要で、Gameの公開API(try_move_right)を
        // 通した隣接移動だけでも自動的に取得でき、SE再生用のGameEventも発火する。
        let mut game = Game::new(8);
        let target_col = game.player.col + 1;
        game.board.rows[game.player.row][target_col] = Cell::Oxygen;
        game.player.oxygen = 40.0;

        let events = game.try_move_right();

        assert_eq!(game.player.col, target_col);
        assert_eq!(game.player.oxygen, 40.0 + crate::constants::OXYGEN_CAPSULE_RESTORE);
        assert_eq!(game.player.score, 100);
        assert_eq!(game.board.cell(game.player.row, target_col), Cell::Empty);
        assert!(events.iter().any(|e| matches!(e, GameEvent::OxygenCollected)));
    }

    #[test]
    fn move_right_stays_put_when_both_the_adjacent_and_upper_cell_are_blocked() {
        let mut game = Game::new(7);
        game.player.row = 1;
        let target_col = game.player.col + 1;
        game.board.rows[game.player.row][target_col] = Cell::Rock { hits: 0 };
        game.board.rows[0][target_col] = Cell::Color(ColorKind::Blue); // 1段上も塞ぐ

        let events = game.try_move_right();

        assert_eq!(game.player.row, 1);
        assert_eq!(game.player.col, target_col - 1); // 移動していない
        assert_eq!(game.player.facing, Direction::Right); // facingだけは反映される
        assert!(events.is_empty());
        assert!(matches!(game.board.cell(1, target_col), Cell::Rock { hits: 0 })); // 壊れない
    }

    #[test]
    fn rock_survives_four_hits_then_breaks_on_fifth_reducing_oxygen_by_20_percent() {
        // spec.md 2章・4章・6章: 岩ブロックは4回攻撃では壊れず、5回目のヒットで
        // 破壊されて酸素が20%減る。この一連の流れをGameの公開APIを通して検証する。
        let mut game = Game::new(10);
        game.player.facing = Direction::Down;
        let target_row = game.player.row + 1;
        let col = game.player.col;
        game.board.rows[target_row][col] = Cell::Rock { hits: 0 };
        let oxygen_before = game.player.oxygen;

        for hit in 1u8..=4 {
            let events = game.try_drill();
            assert!(
                matches!(game.board.cell(target_row, col), Cell::Rock { hits } if hits == hit),
                "{hit}回目のヒット後もhitsが蓄積されているはず"
            );
            assert_eq!(game.player.oxygen, oxygen_before, "{hit}回目のヒットでは酸素は減らない");
            assert_eq!(game.player.row, target_row - 1, "岩が壊れるまでは降下しない");
            assert!(events.iter().any(|e| matches!(e, GameEvent::RockHitIntact)));
            // 次のヒットのためクールダウンを明ける(spec.md 9.9のクールダウンは本テストの本題ではない)
            game.input_cooldown_remaining = Duration::ZERO;
        }

        let events = game.try_drill(); // 5回目: 破壊

        assert_eq!(game.board.cell(target_row, col), Cell::Empty);
        assert_eq!(game.player.oxygen, oxygen_before - 20.0);
        assert_eq!(game.player.row, target_row, "破壊後は続けて1マス下降する");
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::BlockDestroyed { blocks: 1 })));
    }

    #[test]
    fn drilling_a_rock_to_its_fifth_hit_vanishes_the_whole_connected_rock_group() {
        // task4(ユーザー指摘): 岩ブロックも色ブロックと同様に4方向連結の対象になる。
        // 5回目のヒットで破壊に至ると、そのセルだけでなく連結している岩ブロック全部が
        // 消滅する(spec.md 4.9)。酸素ペナルティは実際に掘削した1回分(-20%)のみ。
        let mut game = Game::new(40);
        game.player.facing = Direction::Down;
        let target_row = game.player.row + 1;
        let col = game.player.col;
        game.board.rows[target_row][col] = Cell::Rock {
            hits: ROCK_HITS_TO_BREAK - 1,
        }; // あと1発で破壊
        game.board.rows[target_row][col + 1] = Cell::Rock { hits: 0 }; // 隣接、連結対象
        let oxygen_before = game.player.oxygen;

        let events = game.try_drill(); // 5回目: 破壊、連結岩ブロックも巻き込む

        assert_eq!(game.board.cell(target_row, col), Cell::Empty);
        assert_eq!(game.board.cell(target_row, col + 1), Cell::Empty, "連結していた岩ブロックも消滅する");
        assert_eq!(game.player.oxygen, oxygen_before - 20.0, "酸素ペナルティは1回分のみ");
        assert_eq!(game.player.score, 0, "岩ブロックの消滅は得点対象外");
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::BlockDestroyed { blocks: 2 })));
    }

    #[test]
    fn falling_rock_blocks_connecting_to_four_or_more_auto_vanish_via_update_without_score() {
        // task4: 岩ブロックも色ブロックと同様に、支えを失えば(揺れを経て)落下し、
        // 支持されている岩ブロックに接触して連結、4個以上になれば掘削されずに自動消滅する。
        // ただし得点は発生しない(spec.md 2章・4.9・7章)。
        let mut game = Game::new(41);
        for row in 997..1000 {
            for col in 0..crate::constants::FIELD_WIDTH {
                game.board.rows[row][col] = Cell::Empty;
            }
        }
        game.player.row = 999;
        game.player.col = 11; // 落下グループから十分離す

        game.board.rows[998][0] = Cell::Rock { hits: 0 };
        game.board.rows[998][1] = Cell::Rock { hits: 1 };
        game.board.rows[998][2] = Cell::Rock { hits: 2 };
        game.board.rows[999][3] = Cell::Rock { hits: 3 }; // 最深行=常に支持
        let score_before = game.player.score;

        let events = game.update(Duration::from_millis((SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10));

        assert_eq!(game.player.score, score_before, "岩ブロックの自動消滅で得点は増えない");
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::BlockDestroyed { blocks: 4 })));
        assert_eq!(game.board.cell(999, 0), Cell::Empty);
        assert_eq!(game.board.cell(999, 1), Cell::Empty);
        assert_eq!(game.board.cell(999, 2), Cell::Empty);
        assert_eq!(game.board.cell(999, 3), Cell::Empty);
    }

    #[test]
    fn falling_blocks_connecting_to_four_or_more_auto_vanish_via_update() {
        // spec.md 4章: 支えを失ったブロックが落下し、支持されている同色ブロックに
        // 接触して連結、4個以上になった時点で掘削されずに自動消滅する
        // (1個30点)。Game::updateを通した重力ティックの結果として検証する。
        let mut game = Game::new(11);
        for row in 997..1000 {
            for col in 0..crate::constants::FIELD_WIDTH {
                game.board.rows[row][col] = Cell::Empty;
            }
        }
        game.player.row = 999;
        game.player.col = 11; // 落下グループから十分離す

        game.board.rows[998][0] = Cell::Color(ColorKind::Red);
        game.board.rows[998][1] = Cell::Color(ColorKind::Red);
        game.board.rows[998][2] = Cell::Color(ColorKind::Red);
        game.board.rows[999][3] = Cell::Color(ColorKind::Red); // 最深行=常に支持

        // SHAKE_TICKSぶんは揺れるだけで、その次の周期で落下+着地+自動消滅する
        let events = game.update(Duration::from_millis((SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10));

        assert_eq!(game.player.score, 4 * 30);
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::BlockDestroyed { blocks: 4 })));
        assert_eq!(game.board.cell(999, 0), Cell::Empty);
        assert_eq!(game.board.cell(999, 1), Cell::Empty);
        assert_eq!(game.board.cell(999, 2), Cell::Empty);
        assert_eq!(game.board.cell(999, 3), Cell::Empty);
    }

    #[test]
    fn player_falls_automatically_through_empty_space_without_any_input() {
        // spec.md 1章(TERM独自拡張): 支えを失った(直下がEmptyな)プレイヤーは、入力が
        // 無くてもFALL_TICK_MSごとに1マスずつ自動的に落下し続ける。
        // ランダム生成された周囲のブロックが偶然崩れて割り込むことが無いよう、
        // プレイヤーの通り道を広めにEmptyでクリアしてから検証する。
        let mut game = Game::new(20);
        for row in 5..16 {
            for col in 0..crate::constants::FIELD_WIDTH {
                game.board.rows[row][col] = Cell::Empty;
            }
        }
        game.player.row = 10;
        let col = game.player.col;

        // FALL_TICK_MS(150ms)を3周期分進める -> 3マス落下するはず
        let events = game.update(Duration::from_millis(3 * FALL_TICK_MS + 10));

        assert_eq!(game.player.row, 13);
        assert_eq!(game.player.col, col);
        assert!(!events.iter().any(|e| matches!(e, GameEvent::LifeLost | GameEvent::GameOverMiss)));
    }

    #[test]
    fn player_does_not_fall_when_supported() {
        let mut game = Game::new(21);
        for row in 2..6 {
            for col in 0..crate::constants::FIELD_WIDTH {
                game.board.rows[row][col] = Cell::Empty;
            }
        }
        game.player.row = 5;
        let col = game.player.col;
        // 直下から最深行まで続く支柱にし、支柱自体が途中で崩れて外れる余地を無くす
        for row in 6..game.board.depth_rows() {
            game.board.rows[row][col] = Cell::Color(ColorKind::Red);
        }

        game.update(Duration::from_millis(3 * FALL_TICK_MS + 10));

        assert_eq!(game.player.row, 5);
    }

    // --- 押し潰されて死ぬ演出(TERM独自拡張、9章) ---

    #[test]
    fn crush_miss_activates_the_flash_effect_but_oxygen_miss_does_not() {
        // 落下ブロックに押し潰された場合のみ「潰れた」演出を開始する。酸素切れ由来の
        // ミスではこの演出は行わない(区別できていることの確認)。
        let mut game = Game::new(30);
        game.player.oxygen = 1.0;

        game.update(Duration::from_secs(1)); // 酸素切れでミス(押し潰しではない)

        assert!(!game.crush_flash_active());
    }

    #[test]
    fn crush_flash_decays_to_inactive_after_crush_flash_duration() {
        let mut game = Game::new(31);
        for row in 997..1000 {
            for col in 0..crate::constants::FIELD_WIDTH {
                game.board.rows[row][col] = Cell::Empty;
            }
        }
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[998][5] = Cell::Color(ColorKind::Red); // プレイヤーの真上、支えなし

        // SHAKE_TICKSぶんの揺れ+落下の1ティックで押し潰しが発生する
        game.update(Duration::from_millis((SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10));
        assert!(game.crush_flash_active(), "押し潰し直後は演出が有効なはず");

        // CRUSH_FLASH_MSぶん時間を進めると演出は終わる
        game.update(Duration::from_millis(crate::constants::CRUSH_FLASH_MS + 10));
        assert!(!game.crush_flash_active(), "CRUSH_FLASH_MS経過後は演出が終わっているはず");
    }

    // --- 移動の見た目補間アニメーション(TERM独自拡張、9章) ---

    #[test]
    fn new_game_starts_with_move_animation_already_settled() {
        // 開始直後にいきなり(0,0)相当からアニメーションしてしまわないことの確認。
        let game = Game::new(32);
        assert_eq!(game.move_anim_progress(), 1.0);
        assert_eq!(game.render_prev_position(), game.player.position());
    }

    #[test]
    fn lateral_move_starts_interpolation_from_the_previous_position_then_settles() {
        let mut game = Game::new(33);
        let before = game.player.position();

        let events = game.try_move_right();
        assert!(events.is_empty());
        assert_ne!(game.player.position(), before, "前提: 実際に移動しているはず");

        assert_eq!(game.render_prev_position(), before, "補間の起点は移動前の位置のはず");
        assert!(game.move_anim_progress() < 1.0, "移動直後は補間がまだ完了していないはず");

        game.update(Duration::from_millis(crate::constants::MOVE_ANIM_DURATION_MS + 10));
        assert_eq!(game.move_anim_progress(), 1.0, "MOVE_ANIM_DURATION_MS経過後は補間が完了しているはず");
    }
}

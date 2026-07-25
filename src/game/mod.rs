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
    CRUSH_FLASH_MS, DEBUG_FALL_TICK_MS_MAX, DEBUG_FALL_TICK_MS_MIN, DEBUG_FALL_TICK_STEP_MS,
    DEBUG_SHAKE_DURATION_MS_MAX, DEBUG_SHAKE_DURATION_MS_MIN, DEBUG_SHAKE_DURATION_STEP_MS,
    DEBUG_UNIFY_COLORS_RANGE_ROWS, FALL_TICK_MS, FIELD_DEPTH_M, FIELD_WIDTH, INPUT_COOLDOWN_MS,
    INVULNERABILITY_TICKS, LIVES_DEFAULT, LIVES_MAX, MOVE_ANIM_DURATION_MS, OXYGEN_WARNING_THRESHOLD,
    SHAKE_DURATION_MS,
};
use board::{tick_star_melting, Board, Cell, ColorKind, GravityState};
use physics::{DrillOutcome, FreeFallOutcome, LateralOutcome};
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
    /// MUSIC(BGM)のON/OFF切り替え(TERM独自拡張)。一時停止画面でのみ意味を持つ。
    /// Gameの内部状態には影響しないため、この解釈もGameの外側=main.rsが担う
    ToggleMusic,
    /// SE(効果音)のON/OFF切り替え(TERM独自拡張)。一時停止画面でのみ意味を持つ。
    ToggleSe,
    /// デバッグ: プレイヤー付近のブロックを2色に統一する(TERM独自拡張、動作確認用ショートカット)
    DebugUnifyNearbyColors,
    /// デバッグ: ライフを1増やす(TERM独自拡張、動作確認用ショートカット)
    DebugAddLife,
    /// デバッグ: プレイヤーより浅い(画面上で上にある)ブロックを全削除する
    /// (TERM独自拡張、動作確認用ショートカット)
    DebugClearAbovePlayer,
    /// デバッグ: ブロックの落下速度を遅くする(TERM独自拡張、動作確認用ショートカット)
    DebugBlockFallSlower,
    /// デバッグ: ブロックの落下速度を速くする(TERM独自拡張、動作確認用ショートカット)
    DebugBlockFallFaster,
    /// デバッグ: プレイヤー自身の自由落下速度を遅くする(TERM独自拡張、動作確認用ショートカット)
    DebugPlayerFallSlower,
    /// デバッグ: プレイヤー自身の自由落下速度を速くする(TERM独自拡張、動作確認用ショートカット)
    DebugPlayerFallFaster,
    /// デバッグ: 揺れ時間(落下開始までの時間)を長くする(TERM独自拡張、動作確認用ショートカット)
    DebugShakeDurationLonger,
    /// デバッグ: 揺れ時間(落下開始までの時間)を短くする(TERM独自拡張、動作確認用ショートカット)
    DebugShakeDurationShorter,
    /// 設定画面(MUSIC/SE)をオーバーレイ表示する(TERM独自拡張)。一時停止画面でのみ
    /// 意味を持つ。Gameの内部状態には影響しないため、この解釈もGameの外側=main.rsが担う
    OpenSettings,
    /// ヘルプ画面をオーバーレイ表示する(TERM独自拡張)。一時停止画面でのみ意味を持つ。
    /// ユーザー指摘: 「一時停止中にもヘルプページを開けるようにする」
    OpenHelp,
}

/// ゲーム全体の進行状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameStatus {
    Playing,
    Paused,
    GameOver,
    Cleared,
}

/// GameOverダイアログの選択肢(TERM独自拡張。ユーザー指摘: 「全部死んだら、タイトルに
/// 戻るか、その場から復活して再開するか、ダイアログ表示してカーソルで選べるように」)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameOverChoice {
    BackToTitle,
    Revive,
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
    /// プレイヤー自身の自由落下用のtick蓄積(TERM独自拡張)。ブロックの重力(`fall_tick_accum`)
    /// とは独立した速度で判定できるよう、デバッグショートカットで別々に調整可能にするため分離した。
    player_fall_tick_accum: Duration,
    /// ブロックの重力落下tick間隔(ms)。既定は`FALL_TICK_MS`だが、デバッグショートカット
    /// (`debug_adjust_block_fall_speed`)で動作確認用に実行時調整できる(TERM独自拡張)。
    block_fall_tick_ms: u64,
    /// 支えを失ってから実際に落下し始めるまでの揺れ時間(ms)。既定は`SHAKE_DURATION_MS`
    /// だが、デバッグショートカット(`debug_adjust_shake_duration`)で実行時調整できる
    /// (TERM独自拡張)。揺れティック数への変換は`block_fall_tick_ms`を使い都度計算する。
    shake_duration_ms: u64,
    /// プレイヤー自身の自由落下tick間隔(ms)。既定は`FALL_TICK_MS`だが、デバッグショートカット
    /// (`debug_adjust_player_fall_speed`)で動作確認用に実行時調整できる(TERM独自拡張)。
    player_fall_tick_ms: u64,
    /// 移動系入力(MoveLeft/MoveRight)専用のクールダウン。掘削(Drill)とは別に管理する
    /// (TERM独自拡張。ユーザー指摘: 「カーソルとスペース、両方押してるときにどちらかが
    /// 効かない」。1つの共有クールダウンだと、同一フレームで移動キーと掘削キーが両方
    /// 来た場合に片方がブロックされてしまうため分離した)。
    move_cooldown_remaining: Duration,
    /// 掘削系入力(Drill)専用のクールダウン。移動(MoveLeft/MoveRight)とは別に管理する。
    drill_cooldown_remaining: Duration,
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
    /// GameOverダイアログでの現在の選択項目(TERM独自拡張)。GameOver状態でのみ意味を持つ。
    game_over_selection: GameOverChoice,
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
            player_fall_tick_accum: Duration::ZERO,
            block_fall_tick_ms: FALL_TICK_MS,
            player_fall_tick_ms: FALL_TICK_MS,
            shake_duration_ms: SHAKE_DURATION_MS,
            move_cooldown_remaining: Duration::ZERO,
            drill_cooldown_remaining: Duration::ZERO,
            oxygen_warning_accum: Duration::ZERO,
            invulnerability_ticks_remaining: 0,
            last_level_reported,
            crush_flash_remaining: Duration::ZERO,
            render_prev_position: start_position,
            // 開始時点では補間の必要が無いため、既に完了した扱いにしておく
            // (さもないと初期表示が(0,0)相当からアニメーションしてしまう)。
            render_anim_elapsed: move_anim_duration_secs(),
            game_over_selection: GameOverChoice::BackToTitle,
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

    /// GameOverダイアログの現在の選択項目(TERM独自拡張)。
    pub fn game_over_selection(&self) -> GameOverChoice {
        self.game_over_selection
    }

    /// GameOverダイアログの選択をトグルする(2択なので↑↓どちらでも反転させる。
    /// TERM独自拡張)。GameOver状態でのみ意味を持つ。
    pub fn toggle_game_over_selection(&mut self) {
        if self.status != GameStatus::GameOver {
            return;
        }
        self.game_over_selection = match self.game_over_selection {
            GameOverChoice::BackToTitle => GameOverChoice::Revive,
            GameOverChoice::Revive => GameOverChoice::BackToTitle,
        };
    }

    /// GameOverダイアログで「その場から復活」を選んだ場合の処理(TERM独自拡張。ユーザー
    /// 指摘: 「全部死んだら、タイトルに戻るか、その場から復活して再開するか」)。
    /// ライフを既定値に戻し酸素を全回復してPlayingへ戻す。深度・スコア・盤面は
    /// そのまま維持する。復活直後は既存のライフ喪失時と同様に無敵時間を与える。
    pub fn revive(&mut self) {
        if self.status != GameStatus::GameOver {
            return;
        }
        self.player.lives = LIVES_DEFAULT;
        self.player.oxygen = crate::constants::OXYGEN_MAX;
        self.invulnerability_ticks_remaining = INVULNERABILITY_TICKS;
        self.status = GameStatus::Playing;
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
        if !self.consume_move_cooldown() {
            return Vec::new();
        }
        if !self.player_is_grounded() {
            // ユーザー指摘: 「キャラは落ちる速度おそくなっても、落ちずに横移動する
            // ことはできないものとする」「必ず落ちてから横移動が前提」。デバッグ
            // ショートカットでプレイヤーの自由落下tickを遅くしていても、直下が
            // 空いている(=次の自由落下tickで必ず1マス落ちる)間は横移動を受け付けない。
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
        if !self.consume_drill_cooldown() {
            return events;
        }

        let before = self.player.position();
        let outcome = physics::drill_facing(&mut self.board, &mut self.player, &self.gravity_state);
        self.push_drill_outcome_events(outcome, &mut events);
        self.note_possible_move(before);

        if self.player.row != before.0 {
            self.check_level_and_clear(&mut events);
        }
        events
    }

    /// 移動系入力(MoveLeft/MoveRight)のクールダウン(spec.md 9.9)が明けているかを確認し、
    /// 明けていればリセットする。Playing状態でない場合、またはクールダウン中は`false`を返す。
    /// 掘削(Drill)とは独立したクールダウンなので、同一フレームで両方の入力が来ても
    /// 互いをブロックしない(TERM独自拡張。ユーザー指摘対応)。
    fn consume_move_cooldown(&mut self) -> bool {
        if self.status != GameStatus::Playing {
            return false;
        }
        if self.move_cooldown_remaining > Duration::ZERO {
            return false;
        }
        self.move_cooldown_remaining = Duration::from_millis(INPUT_COOLDOWN_MS);
        true
    }

    /// 掘削系入力(Drill)のクールダウンが明けているかを確認し、明けていればリセットする。
    /// 移動(MoveLeft/MoveRight)とは独立したクールダウン(TERM独自拡張)。
    fn consume_drill_cooldown(&mut self) -> bool {
        if self.status != GameStatus::Playing {
            return false;
        }
        if self.drill_cooldown_remaining > Duration::ZERO {
            return false;
        }
        self.drill_cooldown_remaining = Duration::from_millis(INPUT_COOLDOWN_MS);
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
            DrillOutcome::OxygenUntouchedByDrill => {}
            DrillOutcome::CollectedDiamond => events.push(GameEvent::DiamondCollected),
            DrillOutcome::StarDestroyed => {
                events.push(GameEvent::DrillImpact);
                events.push(GameEvent::BlockDestroyed { blocks: 1 });
            }
            DrillOutcome::CrushedByUnstableOverhead => self.apply_miss(events, true),
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
            // 死んだ場所の左右列を含めて3列分、プレイヤーより上のブロックを全て
            // クリアする(TERM独自拡張。ユーザー指摘: 「キャラがブロックつぶされて
            // 死んだら、死んだ場所の左右列を含めて3列分の、キャラから上部ブロック
            // すべてクリアすること」)。再開直後(ライフが残っての復帰・GameOver
            // ダイアログでの「その場から復活」のどちらでも)に同じ場所でまた
            // 押し潰されるのを防ぐための安全対策。
            self.clear_three_columns_above_player();
        }

        let game_over = self.player.lose_life();
        if game_over {
            self.status = GameStatus::GameOver;
            self.game_over_selection = GameOverChoice::BackToTitle;
            events.push(GameEvent::GameOverMiss);
        } else {
            self.invulnerability_ticks_remaining = INVULNERABILITY_TICKS;
            events.push(GameEvent::LifeLost);
        }
    }

    /// プレイヤーの現在列を中心に左右1列ずつ(=3列分)、プレイヤーより浅い
    /// (画面上で上にある)行を全てEmptyにする(TERM独自拡張)。
    fn clear_three_columns_above_player(&mut self) {
        let col = self.player.col;
        let col_start = col.saturating_sub(1);
        let col_end = (col + 1).min(FIELD_WIDTH - 1);
        for row in 0..self.player.row {
            for c in col_start..=col_end {
                self.board.set(row, c, Cell::Empty);
            }
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

        if self.move_cooldown_remaining > Duration::ZERO {
            self.move_cooldown_remaining = self.move_cooldown_remaining.saturating_sub(delta);
        }
        if self.drill_cooldown_remaining > Duration::ZERO {
            self.drill_cooldown_remaining = self.drill_cooldown_remaining.saturating_sub(delta);
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
        let tick = Duration::from_millis(self.block_fall_tick_ms);
        while self.fall_tick_accum >= tick {
            self.fall_tick_accum -= tick;

            let invulnerable = self.invulnerability_ticks_remaining > 0;
            let shake_ticks = (self.shake_duration_ms / self.block_fall_tick_ms.max(1)).min(u8::MAX as u64) as u8;
            let result = physics::process_gravity_tick(
                &mut self.board,
                &mut self.player,
                &mut self.gravity_state,
                invulnerable,
                shake_ticks,
            );
            if invulnerable {
                self.invulnerability_ticks_remaining -= 1;
            }

            if result.oxygen_collected > 0 {
                events.push(GameEvent::OxygenCollected);
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

            let melted = tick_star_melting(&mut self.board, self.player.row);
            if melted > 0 {
                events.push(GameEvent::BlockDestroyed { blocks: melted });
            }

            if self.status != GameStatus::Playing {
                return events;
            }
        }

        // プレイヤー自身の自由落下(spec.md 1章、TERM独自拡張)。ブロックの重力とは
        // 独立したtick間隔(`player_fall_tick_ms`)で判定する(デバッグショートカットで
        // 両者を別々に速度調整できるようにするため、あえて別ループに分離している)。
        // 入力の有無や掘削とは無関係に、支えを失っていれば(直下がEmptyなら)落下する。
        // 直下が酸素カプセルの場合は掘削不要で「歩くだけで取得」する(spec.md公式マニュアル)。
        self.player_fall_tick_accum += delta;
        let player_tick = Duration::from_millis(self.player_fall_tick_ms);
        while self.player_fall_tick_accum >= player_tick {
            self.player_fall_tick_accum -= player_tick;

            let before_fall = self.player.position();
            let fall_outcome = physics::apply_player_free_fall(&mut self.board, &mut self.player);
            self.note_possible_move(before_fall);
            if fall_outcome == FreeFallOutcome::FellAndCollectedOxygen {
                events.push(GameEvent::OxygenCollected);
            }
            if self.player.row != before_fall.0 {
                self.check_level_and_clear(&mut events);
                if self.status != GameStatus::Playing {
                    break;
                }
            }
        }

        events
    }

    /// プレイヤーが現在支持されている(直下が塞がっている、または最深行に到達している)
    /// かどうか。支持されていなければ次の自由落下tickで必ず1マス落ちる状態であり、
    /// その間は横移動を受け付けない(TERM独自拡張。ユーザー指摘: 「必ず落ちてから
    /// 横移動が前提」)。直下が酸素カプセルの場合も自由落下でそのまま通過するため、
    /// 支持されているとはみなさない。
    fn player_is_grounded(&self) -> bool {
        let below = self.player.row + 1;
        if below >= self.board.depth_rows() {
            return true;
        }
        !matches!(self.board.cell(below, self.player.col), Cell::Empty | Cell::Oxygen)
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

    /// 指定セルが現在「震えている」(支えを失い、落下開始までの猶予期間中)かどうか
    /// (TERM独自拡張、描画用)。ユーザー指摘: 「落下開始までのアニメーションぐらぐら
    /// してほしい(各種ブロック)」。
    pub fn is_cell_shaking(&self, row: usize, col: usize) -> bool {
        self.gravity_state.is_shaking((row, col))
    }

    // -----------------------------------------------------------------------
    // デバッグショートカット(TERM独自拡張。動作確認を効率化するための機能で、
    // 初代の仕様やスコアには一切対応しない)
    // -----------------------------------------------------------------------

    /// 現在のブロック落下tick間隔(ms)。設定の永続化(main.rs/Settings)用に公開する。
    pub fn block_fall_tick_ms(&self) -> u64 {
        self.block_fall_tick_ms
    }

    /// 現在のプレイヤー自由落下tick間隔(ms)。設定の永続化(main.rs/Settings)用に公開する。
    pub fn player_fall_tick_ms(&self) -> u64 {
        self.player_fall_tick_ms
    }

    /// ブロック落下tick間隔を直接指定する(起動時、Settingsから読み込んだ値を適用する用途)。
    /// 範囲外の値は`DEBUG_FALL_TICK_MS_MIN`〜`MAX`にクランプする。
    pub fn set_block_fall_tick_ms(&mut self, ms: u64) {
        self.block_fall_tick_ms = ms.clamp(DEBUG_FALL_TICK_MS_MIN, DEBUG_FALL_TICK_MS_MAX);
    }

    /// プレイヤー自由落下tick間隔を直接指定する(起動時、Settingsから読み込んだ値を適用する用途)。
    pub fn set_player_fall_tick_ms(&mut self, ms: u64) {
        self.player_fall_tick_ms = ms.clamp(DEBUG_FALL_TICK_MS_MIN, DEBUG_FALL_TICK_MS_MAX);
    }

    /// 現在の揺れ時間(ms)。設定の永続化(main.rs/Settings)用に公開する。
    pub fn shake_duration_ms(&self) -> u64 {
        self.shake_duration_ms
    }

    /// 揺れ時間を直接指定する(起動時、Settingsから読み込んだ値を適用する用途)。
    pub fn set_shake_duration_ms(&mut self, ms: u64) {
        self.shake_duration_ms = ms.clamp(DEBUG_SHAKE_DURATION_MS_MIN, DEBUG_SHAKE_DURATION_MS_MAX);
    }

    /// `from_row`以降の岩(X)/AIR/スター/ダイヤブロック出現率を、指定の配分率(%、
    /// 100=通常のまま)で再抽選する(TERM独自拡張。ユーザー指摘: 「設定でXブロックの
    /// 配分量・AIRの配分量をいじれるようにしたい。プレイ中でもその数値をいじれるように
    /// したい」「ダイヤブロック0%設定」)。新規ゲーム開始直後は`from_row`に安全地帯明けの
    /// 行を渡せば盤面全体に反映され、プレイ中に呼ぶ場合は呼び出し側が
    /// `player.row + SPAWN_RATE_REROLL_SAFE_MARGIN_ROWS`のような画面外の行を渡すことで、
    /// 既に見えている地形を変えてしまわないようにする。
    #[allow(clippy::too_many_arguments)]
    pub fn reroll_spawn_rates_from(
        &mut self,
        from_row: usize,
        rock_rate_percent: u32,
        air_rate_percent: u32,
        star_rate_percent: u32,
        diamond_rate_percent: u32,
    ) {
        self.board.reroll_overlays_from_row(
            from_row,
            rock_rate_percent,
            air_rate_percent,
            star_rate_percent,
            diamond_rate_percent,
        );
    }

    /// デバッグ: 揺れ時間(ブロックが支えを失ってから実際に落下し始めるまでの時間)を
    /// `DEBUG_SHAKE_DURATION_STEP_MS`ぶん増減する。`longer`がtrueなら長く(遅く反応)、
    /// falseなら短く(速く反応、0まで)する。
    pub fn debug_adjust_shake_duration(&mut self, longer: bool) {
        self.shake_duration_ms = if longer {
            (self.shake_duration_ms + DEBUG_SHAKE_DURATION_STEP_MS).min(DEBUG_SHAKE_DURATION_MS_MAX)
        } else {
            self.shake_duration_ms.saturating_sub(DEBUG_SHAKE_DURATION_STEP_MS)
        };
    }

    /// デバッグ: ブロック落下速度を`DEBUG_FALL_TICK_STEP_MS`ぶん増減する。
    /// `faster`がtrueならtick間隔を短くして速く、falseなら長くして遅くする。
    pub fn debug_adjust_block_fall_speed(&mut self, faster: bool) {
        self.block_fall_tick_ms = adjust_fall_tick_ms(self.block_fall_tick_ms, faster);
    }

    /// デバッグ: プレイヤー自由落下速度を`DEBUG_FALL_TICK_STEP_MS`ぶん増減する。
    pub fn debug_adjust_player_fall_speed(&mut self, faster: bool) {
        self.player_fall_tick_ms = adjust_fall_tick_ms(self.player_fall_tick_ms, faster);
    }

    /// デバッグ: ライフを1増やす(`LIVES_MAX`でクランプ)。Playing中のみ有効。
    pub fn debug_add_life(&mut self) {
        if self.status == GameStatus::Playing {
            self.player.lives = (self.player.lives + 1).min(LIVES_MAX);
        }
    }

    /// デバッグ: プレイヤーより浅い(画面上で上にある)行を全てEmptyにする。Playing中のみ有効。
    pub fn debug_clear_above_player(&mut self) {
        if self.status != GameStatus::Playing {
            return;
        }
        for row in 0..self.player.row {
            for col in 0..FIELD_WIDTH {
                self.board.set(row, col, Cell::Empty);
            }
        }
    }

    /// デバッグ: プレイヤー付近(上下`DEBUG_UNIFY_COLORS_RANGE_ROWS`行)の色ブロックを
    /// ランダムに選んだ2色だけへ揃える。Playing中のみ有効。
    pub fn debug_unify_nearby_colors(&mut self) -> Vec<GameEvent> {
        let mut events = Vec::new();
        if self.status != GameStatus::Playing {
            return events;
        }
        use rand::RngExt;
        let mut rng = rand::rng();

        let all = ColorKind::ALL;
        let first = all[rng.random_range(0..all.len())];
        let second_offset = 1 + rng.random_range(0..all.len() - 1);
        let second = all[(all.iter().position(|&c| c == first).unwrap() + second_offset) % all.len()];

        let start_row = self.player.row.saturating_sub(DEBUG_UNIFY_COLORS_RANGE_ROWS);
        let end_row = (self.player.row + DEBUG_UNIFY_COLORS_RANGE_ROWS).min(self.board.depth_rows().saturating_sub(1));
        for row in start_row..=end_row {
            for col in 0..FIELD_WIDTH {
                if matches!(self.board.cell(row, col), Cell::Color(_)) {
                    let chosen = if rng.random_bool(0.5) { first } else { second };
                    self.board.set(row, col, Cell::Color(chosen));
                }
            }
        }

        // 重力ティックの外から色配置を直接書き換えたため、塊(連結グループ)の境界が
        // 変わっている。まだ揺れ猶予中(落下し始めていない)の古い揺れ状態は引きずらず、
        // 次の重力ティックで結合関係を一から作り直させる(ユーザー指摘: 「ちゃんと結合
        // 関係を再計算するように」)。ただし既に揺れが明けて連続落下中の塊まで巻き込んで
        // 揺れ直させてしまうと、Cを押した瞬間に「フリーズしたように見える」(ユーザー指摘:
        // 「ショートカット:Cにした瞬間これで落ちずにフリーズしてるように見える」)ため、
        // そちらは対象外にする。
        let current_shake_ticks = (self.shake_duration_ms / self.block_fall_tick_ms.max(1)).min(u8::MAX as u64) as u8;
        self.gravity_state.reset_shake_progress(current_shake_ticks);

        // 塗り替えによって既存の4連結以上の塊に同色ブロックが新たに隣接し、結合が
        // 拡大した場合も、実際に落下するのを待たずこの場で自動消滅させる(ユーザー指摘:
        // 「すでに4個以上の結合ブロックに、同色のブロックが隣接したら結合が拡大するが、
        // この変化においてもブロックは消えないとだめ」)。
        let (vanished_colors, vanished_rocks) =
            self.board.vanish_four_or_more_connected_groups_in_range(start_row, end_row);
        if vanished_colors > 0 {
            self.player.award_auto_vanish_score(vanished_colors);
            events.push(GameEvent::BlockDestroyed { blocks: vanished_colors });
        }
        if vanished_rocks > 0 {
            // 岩ブロックの自動消滅は得点対象外(spec.md 4.9・既存ルールと同じ)。
            events.push(GameEvent::BlockDestroyed { blocks: vanished_rocks });
        }

        events
    }
}

/// `ms`を`step`ぶん増減させ、`DEBUG_FALL_TICK_MS_MIN`〜`MAX`にクランプする
/// (`faster`がtrueならtick間隔を短く=速く、falseなら長く=遅くする)。
fn adjust_fall_tick_ms(ms: u64, faster: bool) -> u64 {
    if faster {
        ms.saturating_sub(DEBUG_FALL_TICK_STEP_MS).max(DEBUG_FALL_TICK_MS_MIN)
    } else {
        (ms + DEBUG_FALL_TICK_STEP_MS).min(DEBUG_FALL_TICK_MS_MAX)
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

    /// テスト用ヘルパー: 盤面全体を`Cell::Empty`にクリアする。`Game::new`はランダム
    /// 生成された盤面を持つため、テストが制御していない場所(意図した数行の外側)にも
    /// 未支持のグループが残っていると、支えの連鎖判定によって盤面全体で予期しない
    /// 自動消滅・スコア加算が起きてしまう。重力・自動消滅系のテストは必ずこれで
    /// クリアしてから対象セルだけを配置すること。
    fn clear_board(game: &mut Game) {
        for row in game.board.rows.iter_mut() {
            for cell in row.iter_mut() {
                *cell = Cell::Empty;
            }
        }
    }

    #[test]
    fn reaching_goal_depth_via_drill_clears_the_game() {
        let mut game = Game::new(1);
        game.player.row = FIELD_DEPTH_M - 2;
        game.player.facing = Direction::Down;
        let last_row = FIELD_DEPTH_M - 1;
        game.board.rows[last_row][game.player.col] = Cell::Empty;

        game.try_drill(); // 掘るだけでは移動しない(自然落下ペースを追い越さない)
        let events = game.update(Duration::from_millis(FALL_TICK_MS)); // 自由落下で最深行へ進む

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

    // --- GameOverダイアログ(TERM独自拡張) ---

    #[test]
    fn game_over_selection_defaults_to_back_to_title_and_toggles() {
        let mut game = Game::new_with_lives(2, 1);
        game.player.oxygen = 1.0;
        game.update(Duration::from_secs(1));
        assert_eq!(game.status, GameStatus::GameOver);
        assert_eq!(game.game_over_selection(), GameOverChoice::BackToTitle);

        game.toggle_game_over_selection();
        assert_eq!(game.game_over_selection(), GameOverChoice::Revive);

        game.toggle_game_over_selection();
        assert_eq!(game.game_over_selection(), GameOverChoice::BackToTitle);
    }

    #[test]
    fn toggle_game_over_selection_does_nothing_while_playing() {
        let mut game = Game::new(1);
        assert_eq!(game.status, GameStatus::Playing);

        game.toggle_game_over_selection();

        assert_eq!(game.game_over_selection(), GameOverChoice::BackToTitle);
    }

    #[test]
    fn revive_restores_lives_and_oxygen_and_resumes_playing_at_the_same_spot() {
        let mut game = Game::new_with_lives(2, 1);
        game.player.oxygen = 1.0;
        game.update(Duration::from_secs(1));
        assert_eq!(game.status, GameStatus::GameOver);
        let depth_before = game.player.depth_m();
        let score_before = game.player.score;

        game.revive();

        assert_eq!(game.status, GameStatus::Playing);
        assert_eq!(game.player.lives, LIVES_DEFAULT);
        assert_eq!(game.player.oxygen, crate::constants::OXYGEN_MAX);
        assert_eq!(game.player.depth_m(), depth_before, "深度は維持される");
        assert_eq!(game.player.score, score_before, "スコアは維持される");
    }

    #[test]
    fn revive_does_nothing_while_playing() {
        let mut game = Game::new(1);
        game.player.lives = 1;

        game.revive();

        assert_eq!(game.player.lives, 1, "GameOver状態でなければ何もしない");
    }

    #[test]
    fn input_cooldown_blocks_rapid_repeated_moves() {
        let mut game = Game::new(3);
        // 開始直後の上2行は常にEmpty(spec.md)なので、直下に足場を置いて
        // 「必ず落ちてから横移動が前提」の新ルールでも横移動できる状態にする。
        game.board.rows[game.player.row + 1][game.player.col] = Cell::Rock { hits: 0 };
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

        game.try_drill(); // 掘るだけでは移動しない
        let events = game.update(Duration::from_millis(FALL_TICK_MS)); // 自由落下でdepth=31 -> level 2へ

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

        game.move_cooldown_remaining = Duration::ZERO;
            game.drill_cooldown_remaining = Duration::ZERO; // クールダウンを明ける(本テストの本題ではない)
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

        game.move_cooldown_remaining = Duration::ZERO;
            game.drill_cooldown_remaining = Duration::ZERO; // クールダウンを明ける(本テストの本題ではない)
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
        // 開始直後の上2行は常にEmptyなので、直下に足場を置いて横移動できる状態にする。
        game.board.rows[game.player.row + 1][game.player.col] = Cell::Rock { hits: 0 };
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
            game.move_cooldown_remaining = Duration::ZERO;
            game.drill_cooldown_remaining = Duration::ZERO;
        }

        let events = game.try_drill(); // 5回目: 破壊

        assert_eq!(game.board.cell(target_row, col), Cell::Empty);
        assert_eq!(game.player.oxygen, oxygen_before - 20.0);
        assert_eq!(game.player.row, target_row - 1, "掘っただけでは移動しない");
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::BlockDestroyed { blocks: 1 })));

        game.update(Duration::from_millis(FALL_TICK_MS)); // 自由落下で開いたマスへ進む
        assert_eq!(game.player.row, target_row, "自由落下で続けて1マス下降する");
    }

    #[test]
    fn drilling_a_rock_to_its_fifth_hit_vanishes_only_that_block() {
        // ユーザー指摘: 「Xブロックは結合してても全体が消えるのではなく1ブロックしか
        // 消せないものとする」。5回目のヒットで破壊されるのはそのセルのみで、
        // 連結している隣の岩ブロックは影響を受けない。酸素ペナルティは-20%。
        let mut game = Game::new(40);
        game.player.facing = Direction::Down;
        let target_row = game.player.row + 1;
        let col = game.player.col;
        game.board.rows[target_row][col] = Cell::Rock {
            hits: ROCK_HITS_TO_BREAK - 1,
        }; // あと1発で破壊
        game.board.rows[target_row][col + 1] = Cell::Rock { hits: 0 }; // 連結していても巻き込まれない
        let oxygen_before = game.player.oxygen;

        let events = game.try_drill(); // 5回目: そのセルだけ破壊

        assert_eq!(game.board.cell(target_row, col), Cell::Empty);
        assert_eq!(
            game.board.cell(target_row, col + 1),
            Cell::Rock { hits: 0 },
            "連結していた岩ブロックは影響を受けない"
        );
        assert_eq!(game.player.oxygen, oxygen_before - 20.0, "酸素ペナルティは1回分のみ");
        assert_eq!(game.player.score, 0, "岩ブロックの消滅は得点対象外");
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::BlockDestroyed { blocks: 1 })));
    }

    #[test]
    fn falling_rock_blocks_connecting_to_four_or_more_auto_vanish_via_update() {
        // ユーザー指摘: 「4個以上結合したらちゃんと消えないといけない」。岩ブロックも
        // 支えを失えば(揺れを経て)落下し、支持されている岩ブロックに接触して連結、
        // 4個以上になれば自動消滅する(得点は対象外)。
        let mut game = Game::new(41);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 11; // 落下グループから十分離す

        game.board.rows[998][0] = Cell::Rock { hits: 0 };
        game.board.rows[998][1] = Cell::Rock { hits: 1 };
        game.board.rows[998][2] = Cell::Rock { hits: 2 };
        game.board.rows[999][3] = Cell::Rock { hits: 3 }; // 最深行=常に支持
        let score_before = game.player.score;

        let events = game.update(Duration::from_millis((SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10));

        assert_eq!(game.player.score, score_before, "岩ブロックの自動消滅はスコア対象外");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::BlockDestroyed { blocks: 4 })),
            "4個以上連結した岩ブロックの自動消滅でBlockDestroyedイベントが発生する"
        );
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
        clear_board(&mut game);
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
    fn falling_block_merges_after_a_long_multi_row_fall_via_many_small_frame_updates() {
        // ユーザー指摘: 「この緑の横に2つのところに、たて5が結合した。しかし消えなかった」
        // 「こういうテストをちゃんとやってほしい」。1回の大きなdeltaでまとめて進める
        // 既存テストと異なり、実際のmain.rs(FRAME_INTERVAL_MS=33msごとにupdate()を呼ぶ)
        // と同じ細かい刻みで、かつ何十行分もの長い空洞を連続落下させたうえで、
        // 既存の縦に連結した塊(3個)と接触・合流して合計5個以上になった時点で
        // 自動消滅することを確認する。
        const FRAME_MS: u64 = 33;
        let mut game = Game::new(40);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 11; // 落下グループから十分離す

        // 既存の縦連結(3個、最深行に固定、常に支持されている)。
        game.board.rows[997][0] = Cell::Color(ColorKind::Red);
        game.board.rows[998][0] = Cell::Color(ColorKind::Red);
        game.board.rows[999][0] = Cell::Color(ColorKind::Red);

        // 遠く離れた上空から落ちてくる縦連結(2個)。間の行は全てEmptyのまま
        // (=何十行分もの空洞)なので、既存の連結に到達するまで何十ティックもかかる。
        game.board.rows[900][0] = Cell::Color(ColorKind::Red);
        game.board.rows[901][0] = Cell::Color(ColorKind::Red);

        let mut events = Vec::new();
        // 十分な時間(揺れ+96行ぶんの落下)を、実フレームと同じ33ms刻みで積み上げる。
        let total_ms_needed = (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 97 * FALL_TICK_MS;
        let mut elapsed_ms = 0u64;
        while elapsed_ms < total_ms_needed {
            events.extend(game.update(Duration::from_millis(FRAME_MS)));
            elapsed_ms += FRAME_MS;
        }

        assert!(
            events.iter().any(|e| matches!(e, GameEvent::BlockDestroyed { blocks: 5 })),
            "縦5個での自動消滅イベントが発生していない"
        );
        assert_eq!(game.player.score, 5 * 30, "5個ぶんの自動消滅スコアが入っているはず");
        for row in [997, 998, 999] {
            assert_eq!(game.board.cell(row, 0), Cell::Empty, "row={row}が消えていない");
        }
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
    fn player_does_not_get_stuck_floating_over_a_tall_open_shaft_across_many_frames() {
        // ユーザー指摘: 「浮いてる、おかしいこれバグ」(スクリーンショット添付、プレイヤーが
        // 大きな縦穴の上でずっと静止して見える)。main.rsの実際の使い方(FRAME_INTERVAL_MS
        // =33msごとにupdate()を呼ぶ)を模して、細かいフレーム単位で何十フレームも進めても、
        // 支えを失ったプレイヤーが一度も止まらず連続して落下し続けることを確認する。
        const FRAME_MS: u64 = 33;
        let mut game = Game::new(30);
        clear_board(&mut game);
        game.player.row = 100;
        game.player.col = 5;
        // 100行下まで全てEmpty、その先(row 200)に床を置く。
        game.board.rows[200][5] = Cell::Rock { hits: 0 };

        let mut max_row_seen = game.player.row;
        let mut stalled_frames_in_a_row = 0;
        let mut worst_stall = 0;

        // 150フレームぶん(約5秒相当)を1フレームずつ進め、毎フレーム行が進むか
        // (または既に床に到達しているか)を確認する。
        for _ in 0..150 {
            let before = game.player.row;
            game.update(Duration::from_millis(FRAME_MS));
            if game.player.row == before && game.player.row < 199 {
                stalled_frames_in_a_row += 1;
                worst_stall = worst_stall.max(stalled_frames_in_a_row);
            } else {
                stalled_frames_in_a_row = 0;
            }
            max_row_seen = max_row_seen.max(game.player.row);
        }

        // player_fall_tick_ms(既定FALL_TICK_MS=150ms)ごとに1マス落ちるはずなので、
        // 33ms単位のフレームでは数フレームに1回しか実際には動かない。それでも
        // 「何十フレームも完全に静止したまま」になることはないはずで、目安として
        // 10フレーム(約330ms、既定tickの2倍以上)を超える連続静止は異常とみなす。
        assert!(
            worst_stall <= 10,
            "支えを失ったプレイヤーが{worst_stall}フレーム連続で静止した(床に到達済みでないのに浮いたまま)"
        );
        assert!(max_row_seen > 100, "プレイヤーは一度も動かなかった(浮いたまま)");
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
    fn crush_death_clears_three_columns_above_the_player() {
        // 押し潰しミス発生時、死亡地点の左右列を含めて3列分、プレイヤーより上の
        // ブロックが全てクリアされる(TERM独自拡張。ユーザー指摘: 「キャラがブロック
        // つぶされて死んだら、死んだ場所の左右列を含めて3列分の、キャラから上部
        // ブロックすべてクリアすること」。再開直後の連続死亡を防ぐ安全対策)。
        let mut game = Game::new_with_lives(34, 2); // ライフ2、押し潰されても即GameOverにならない
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        for row in 990..999 {
            game.board.rows[row][4] = Cell::Color(ColorKind::Blue);
            game.board.rows[row][5] = Cell::Color(ColorKind::Blue);
            game.board.rows[row][6] = Cell::Color(ColorKind::Blue);
        }
        // 対象外の列(3, 7)は影響を受けないことを確認するために配置しておく。
        // 最深行に置いて確実に支持された状態にする(そうしないと重力落下で
        // 位置がズレてテストの前提が崩れる)。
        game.board.rows[999][3] = Cell::Color(ColorKind::Green);
        game.board.rows[999][7] = Cell::Color(ColorKind::Green);
        game.board.rows[998][5] = Cell::Color(ColorKind::Red); // プレイヤーの真上、支えなし

        game.update(Duration::from_millis((SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10));

        assert_eq!(game.player.lives, 1, "押し潰されてライフを1つ失っているはず");
        for row in 0..999 {
            assert_eq!(game.board.cell(row, 4), Cell::Empty, "row={row} col=4はクリアされているはず");
            assert_eq!(game.board.cell(row, 5), Cell::Empty, "row={row} col=5はクリアされているはず");
            assert_eq!(game.board.cell(row, 6), Cell::Empty, "row={row} col=6はクリアされているはず");
        }
        assert_eq!(game.board.cell(999, 3), Cell::Color(ColorKind::Green), "対象外の列はクリアされない");
        assert_eq!(game.board.cell(999, 7), Cell::Color(ColorKind::Green), "対象外の列はクリアされない");
    }

    #[test]
    fn crush_flash_decays_to_inactive_after_crush_flash_duration() {
        let mut game = Game::new(31);
        clear_board(&mut game);
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

    #[test]
    fn taking_air_from_under_a_block_does_not_cause_an_immediate_crush_it_shakes_first() {
        // ユーザー指摘: 「AIRのうえにブロックがあるとき、そのAIRをとったら、すぐに
        // そのうえのブロックが落ちてつぶされるバグ」。AIRを取得して支えを失った
        // 直後も、通常の支え喪失(crush_flash_decays_to_inactive_after_crush_flash_duration
        // 等)と同様にSHAKE_TICKSぶん揺れてから落下するはずで、即座には押し潰されない。
        let mut game = Game::new(50);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.player.facing = Direction::Right;
        game.board.rows[999][6] = Cell::Oxygen; // 取得対象のAIR(プレイヤーと同じ高さ)
        game.board.rows[998][6] = Cell::Color(ColorKind::Red); // AIRの真上のブロック

        let events = game.try_move_right();
        assert!(events.iter().any(|e| matches!(e, GameEvent::OxygenCollected)));
        assert_eq!(game.player.col, 6, "AIRのマスへ移動しているはず");
        assert_eq!(game.player.lives, LIVES_DEFAULT, "移動しただけではまだ潰されていない");

        // 支えを失った直後、SHAKE_TICKSぶんはまだ落下しない(押し潰されない)。
        game.update(Duration::from_millis(SHAKE_TICKS as u64 * FALL_TICK_MS));
        assert_eq!(game.player.lives, LIVES_DEFAULT, "揺れている間は押し潰されないはず");
        assert_eq!(game.board.cell(998, 6), Cell::Color(ColorKind::Red), "まだ落下していない");

        // 揺れが明けた次のティックで初めて落下し、押し潰される。
        game.update(Duration::from_millis(FALL_TICK_MS + 10));
        assert_eq!(game.player.lives, LIVES_DEFAULT - 1, "揺れが明けてから押し潰されるはず");
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
        // 開始直後の上2行は常にEmptyなので、直下に足場を置いて横移動できる状態にする。
        game.board.rows[game.player.row + 1][game.player.col] = Cell::Rock { hits: 0 };
        let before = game.player.position();

        let events = game.try_move_right();
        assert!(events.is_empty());
        assert_ne!(game.player.position(), before, "前提: 実際に移動しているはず");

        assert_eq!(game.render_prev_position(), before, "補間の起点は移動前の位置のはず");
        assert!(game.move_anim_progress() < 1.0, "移動直後は補間がまだ完了していないはず");

        game.update(Duration::from_millis(crate::constants::MOVE_ANIM_DURATION_MS + 10));
        assert_eq!(game.move_anim_progress(), 1.0, "MOVE_ANIM_DURATION_MS経過後は補間が完了しているはず");
    }

    // --- ショートカットC: 2色化+結合再計算(TERM独自拡張) ---

    #[test]
    fn debug_unify_nearby_colors_repaints_to_exactly_two_colors_and_vanishes_new_four_connections() {
        // ユーザー指摘: 「ショートカット:Cは、既存ブロックを2色に変化するように
        // してほしい。その際結合関係を再計算で」。ランダムな2色のみへ塗り替え、
        // 塗り替えによって新たに4連結以上になった箇所はその場で自動消滅することを
        // 確認する(色の選択はOS乱数のため非決定的。十分な回数試行して確認する)。
        let mut merged_at_least_once = false;
        for _ in 0..300 {
            let mut game = Game::new(1);
            clear_board(&mut game);
            game.player.row = 500;
            game.player.col = 5;
            game.board.rows[500][0] = Cell::Color(ColorKind::Red);
            game.board.rows[500][1] = Cell::Color(ColorKind::Blue);
            game.board.rows[500][2] = Cell::Color(ColorKind::Green);
            game.board.rows[500][3] = Cell::Color(ColorKind::Yellow);

            let events = game.debug_unify_nearby_colors();

            let mut colors_seen: Vec<ColorKind> = Vec::new();
            for c in 0..4 {
                if let Cell::Color(k) = game.board.cell(500, c)
                    && !colors_seen.contains(&k)
                {
                    colors_seen.push(k);
                }
            }
            assert!(colors_seen.len() <= 2, "2色より多い色が残っている: {colors_seen:?}");

            if events.iter().any(|e| matches!(e, GameEvent::BlockDestroyed { .. })) {
                for c in 0..4 {
                    assert_eq!(game.board.cell(500, c), Cell::Empty, "4連結になった箇所は自動消滅しているはず");
                }
                merged_at_least_once = true;
                break;
            }
        }
        assert!(merged_at_least_once, "300回試行しても4連結による自動消滅が一度も発生しなかった");
    }
}

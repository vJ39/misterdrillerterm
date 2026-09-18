//! ゲーム全体のオーケストレーション(盤面+プレイヤー+タイマー類)。
//!
//! board/player/physicsの副作用のない純粋なロジックを、「1フレーム進める」「1回入力を
//! 処理する」という時間軸に沿ってまとめ、UI/audio層が反応すべき`GameEvent`列を返す。

mod attack;
pub mod board;
mod bomb;
pub mod physics;
pub mod player;
mod state_hash;

pub use attack::IncomingRock;
use bomb::BombInTheWay;
pub use bomb::{Bomb, BombPhase};

use std::rc::Rc;
use std::time::Duration;

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;

use crate::constants::{
    ATTACK_BLOCKS_PER_ROCK_DEFAULT, ATTACK_BLOCKS_PER_ROCK_MAX, ATTACK_BLOCKS_PER_ROCK_MIN,
    ATTACK_ROCKS_PER_WAVE_MAX_DEFAULT, ATTACK_ROCKS_PER_WAVE_MAX_MAX,
    ATTACK_ROCKS_PER_WAVE_MAX_MIN, BLOCK_VANISH_FLASH_MIN_MS, BLOCK_VANISH_FLASH_MS,
    BOARD_SNAPSHOT_ROWS_ABOVE_PLAYER, BOARD_SNAPSHOT_ROWS_BELOW_PLAYER,
    BOARD_SNAPSHOT_TICK_INTERVAL, BOMB_BLAST_COL_RANGE, BOMB_BLAST_ROW_RANGE, BOMB_DANGER_MS,
    BOMB_ENTER_MS, BOMB_EXPLOSION_FLASH_MS, BOMB_FUSE_MS, BOMB_FUSE_TICK_INTERVAL_MS,
    BOMB_MAX_COUNT_ON_BOARD, BOMB_ROLL_MS, BOMB_SETTLE_MS, BOMB_SETTLE_TICK_MS,
    BOMB_SPAWN_BASE_PROB, BOMB_SPAWN_CHECK_INTERVAL_MS, BOMB_SPAWN_DEPTH_MAX_BONUS,
    BONUS_FLOOR_DEPTH_M, BONUS_FLOOR_ITEM_AIR_RATE_PERCENT, CHAIN_VANISH_INTERVAL_MS_DEFAULT,
    CHAIN_VANISH_INTERVAL_MS_MAX, CHAIN_VANISH_INTERVAL_MS_MIN, CHECKPOINT_FLASH_MS,
    CHECKPOINT_SAFE_ZONE_M, CHECKPOINT_STEP_M, CHECKPOINT_ZONE_GAP_M, CRUSH_ASCEND_MS,
    CRUSH_FLASH_MS, DEBUG_FALL_TICK_MS_MAX, DEBUG_FALL_TICK_MS_MIN, DEBUG_FALL_TICK_STEP_MS,
    DEBUG_INCOMING_ATTACK_POWER, DEBUG_SHAKE_DURATION_MS_MAX, DEBUG_SHAKE_DURATION_MS_MIN,
    DEBUG_SHAKE_DURATION_STEP_MS, DEBUG_UNIFY_COLORS_RANGE_ROWS, DODGE_DETECT_WINDOW_MS,
    DODGE_RECOVERY_MS_DEFAULT, DODGE_RECOVERY_MS_MAX, DODGE_RECOVERY_MS_MIN, DODGE_SLIDE_MS,
    DRILL_ANIM_FRAME_MS, DRILL_ANIM_MS, FALL_SPEED_DEPTH_MAX_SPEEDUP, FALL_TICK_MS,
    FIELD_WIDTH_MAX, FIELD_WIDTH_MIN, INCOMING_ROCK_WARNING_MS, INPUT_COOLDOWN_ACCUM_CAP_MS,
    INPUT_COOLDOWN_MS, INVULNERABILITY_TICKS, LIVES_DEFAULT, LIVES_MAX, MOVE_ANIM_DURATION_MS,
    MOVE_COOLDOWN_MS_DEFAULT, MOVE_COOLDOWN_MS_MAX, MOVE_COOLDOWN_MS_MIN,
    OXYGEN_DECAY_DEPTH_MAX_MULTIPLIER, OXYGEN_WARNING_THRESHOLD, PLAYER_SCREEN_ROWS_ABOVE,
    REWIND_STOCK_INITIAL, REWIND_STOCK_MAX_DEFAULT, REWIND_STOCK_PER_CHECKPOINT, SHAKE_DURATION_MS,
    STAR_VISIBLE_RANGE_ROWS, depth_fraction,
};
use board::{
    BlockMove, Board, Cell, ColorKind, GravityState, ItemEffect, Pickup, Pos, bomb_blast_cells,
    connected_same_color, tick_star_melting,
};
use physics::{DrillOutcome, FreeFallOutcome, LateralOutcome};
use player::{Direction, Player};

use crate::debug_log::DebugLog;

// `Game::new`/`new_with_lives`(テスト専用)のみが参照するため、通常ビルドでは
// unused import警告になる。
#[cfg(test)]
use crate::constants::{FIELD_DEPTH_M, FIELD_WIDTH_DEFAULT};

/// 深度(m)から、直近で到達済みのチェックポイント区切り番号を計算する。地面
/// (`CHECKPOINT_SAFE_ZONE_M`)を実際に掘り抜いた地点(checkpoint*CHECKPOINT_STEP_M +
/// CHECKPOINT_SAFE_ZONE_M + 1)をもって到達とみなす(地面の手前に触れた時点ではない)。
fn checkpoint_index_for_depth(depth_m: usize) -> usize {
    depth_m.saturating_sub(CHECKPOINT_SAFE_ZONE_M + 1) / CHECKPOINT_STEP_M
}

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
    /// MUSIC(BGM)のON/OFF切り替え。一時停止画面でのみ意味を持ち、Gameの内部状態には
    /// 影響しないため、この解釈もGameの外側=main.rsが担う
    ToggleMusic,
    /// SE(効果音)のON/OFF切り替え。一時停止画面でのみ意味を持つ
    ToggleSe,
    /// デバッグ: プレイヤー付近のブロックを2色に統一する
    DebugUnifyNearbyColors,
    /// デバッグ: ライフを1増やす
    DebugAddLife,
    /// デバッグ: 酸素(AIR)を100%まで回復する
    DebugFillAir,
    /// デバッグ: プレイヤーより浅い(画面上で上にある)ブロックを全削除する
    DebugClearAbovePlayer,
    /// デバッグ: 画面内のXブロック・ダイヤブロックを全てスターブロックに変える
    DebugStarifyVisibleScreen,
    /// デバッグ: ボムを1個、画面内のランダムなEmptyマスへ即座に設置する
    DebugPlaceBomb,
    /// デバッグ: 対戦の妨害ルール(#247)の「攻撃交換1ラウンド」を疑似実行する
    DebugReceiveOpponentAttack,
    /// デバッグ: ブロックの落下速度を遅くする
    DebugBlockFallSlower,
    /// デバッグ: ブロックの落下速度を速くする
    DebugBlockFallFaster,
    /// デバッグ: プレイヤー自身の自由落下速度を遅くする
    DebugPlayerFallSlower,
    /// デバッグ: プレイヤー自身の自由落下速度を速くする
    DebugPlayerFallFaster,
    /// デバッグ: 揺れ時間(落下開始までの時間)を長くする
    DebugShakeDurationLonger,
    /// デバッグ: 揺れ時間(落下開始までの時間)を短くする
    DebugShakeDurationShorter,
    /// デバッグ: オートプレイ(自動操作)のON/OFFを切り替える。ONにすると無敵も同時にON。
    /// AIの実体は`autoplay::Autopilot`(Gameの外の仮想キーボード)なので、この解釈も
    /// Gameの外側=main.rsが担う
    DebugToggleAutopilot,
    /// デバッグ: 無敵(ミス無効)のON/OFFを切り替える。オートプレイとは独立したトグルで、
    /// 手動プレイのまま無敵にもできる
    DebugToggleInvincible,
    /// 設定画面(MUSIC/SE)をオーバーレイ表示する。一時停止画面でのみ意味を持ち、
    /// この解釈もGameの外側=main.rsが担う
    OpenSettings,
    /// ヘルプ画面をオーバーレイ表示する。一時停止画面でのみ意味を持つ
    OpenHelp,
    /// Enterキー。タイトル画面からの開始・GameOverダイアログでの選択確定は、このキーで
    /// のみ行う(他のキーでは進まない)
    Confirm,
    /// どのショートカットにも割り当てられていないキー。一時停止中(オーバーレイ非表示時)
    /// に限り、Pキーと同様に再開のトリガーとして扱う。この解釈もmain.rsが担う
    UnboundKey,
    /// Backspace/Uキー: フレーム巻き戻しの開始。過去のスナップショットを保持しているのは
    /// `Game`ではなく`rewind::RewindHistory`(Gameの外)なので、この解釈もmain.rsが担う。
    /// `Game`自身は起動可否(`can_start_rewind`)と復元(`restore_for_rewind`)だけを提供する
    Rewind,
}

/// ゲーム全体の進行状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameStatus {
    Playing,
    Paused,
    GameOver,
    Cleared,
}

/// 「わ〜!」スライダー演出の段階。ブロックが落ち始める直前に移動して間一髪回避した際、
/// まずスライダー(横滑り)で見せ、短い硬直(`dodge_recovery_ms`)を挟んで通常操作へ戻る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DodgeStage {
    /// 演出無し(通常プレイ中)。
    None,
    /// スライダー(横滑り)演出中。
    Sliding,
    /// スライダー後、起き上がるまでの短い硬直中。
    Recovering,
}

/// GameOverダイアログの選択肢(タイトルへ戻る/その場から復活する)。
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
    /// ブロックが消滅した(色ブロックの直接掘削消滅・自動消滅・スター消滅のいずれも。
    /// spec.md 10章「破壊音」)。消滅したブロック数を伴う。岩ブロックの消滅だけは専用SEの
    /// ため`RockDestroyed`を使う
    BlockDestroyed { blocks: usize },
    /// 岩ブロック(Xブロック)が消滅した(直接掘削の5回目破壊・自動消滅のいずれも)。
    /// 消滅したブロック数を伴う
    RockDestroyed { blocks: usize },
    /// ヒヤリ回避スライダー演出が発動した瞬間
    DodgeTriggered,
    /// 酸素カプセルを取得した
    OxygenCollected,
    /// ダイヤブロックを取得した
    DiamondCollected,
    /// 酸素残量が警告閾値以下の間、1秒間隔で発生
    OxygenWarningTick,
    /// レベル(30mごと)が上がった
    LevelUp { level: usize },
    /// Lv.10ごとに到達し、ライフを1つ獲得した。既にライフが上限(`LIVES_MAX`)でも
    /// イベント自体は発生する(実際に加算されたかは呼び出し側では区別しない)。
    ExtraLifeAtLevel { level: usize },
    /// ライフを1つ失ったが、まだライフが残っている(その場で酸素全回復して再開)。
    /// `cause`はソークテストが死因の内訳を数えるための記録用で、SE再生等では参照しない。
    LifeLost { cause: MissCause },
    /// 「天に召される」演出が終わり、その場に復活した瞬間
    Revived,
    /// 最後のライフでミスした(「天に召される」演出の完了後にGameOverへ遷移する)。
    /// `cause`は`LifeLost`と同じ記録用。
    GameOverMiss { cause: MissCause },
    /// 深度1000m到達でゲームクリアした
    Cleared,
    /// アイテムブロックを取得し、対応する効果が発動した
    ItemCollected(ItemEffect),
    /// ボムが爆発した。プレイヤーが爆風に巻き込まれたかどうかは、別途`LifeLost`/
    /// `GameOverMiss`が続けて発生するかで判断できる。
    BombExploded,
    /// ボムの残り時間が`BOMB_DANGER_MS`を切り、本体が激しく赤く点滅し始めた瞬間。
    /// 1個のボムにつき1回だけ発生する。
    BombFuseWarning,
    /// 危険域(残り`BOMB_DANGER_MS`以下)に入っている間、`BOMB_FUSE_TICK_INTERVAL_MS`
    /// おきに繰り返し発生する導火線の「チッ」。危険域に入った瞬間1回だけの
    /// `BombFuseWarning`とは別に、爆発が近いことを連続音で示す。
    BombFuseTick,
    /// チェックポイント(100mごと)に到達した瞬間。到達した深度(m、100の倍数)を伴う。
    Checkpoint100m { at_m: usize },
    /// 相手の攻撃で降ってきた岩が、予告を終えて盤面へ出現した(#247)。1ウェーブにつき
    /// 1回だけ発生し、`rocks`は実際に出現した個数。
    IncomingRocksSpawned { rocks: usize },
    /// 無敵(`Game::set_invincible`)が有効なため、本来のミスが回避された。ライフ減少・
    /// 「天に召される」演出・GameOver判定のいずれも発生していない。ソークテストで死因を
    /// 数えるための記録用で、演出・SEは伴わない。
    MissAverted { cause: MissCause },
}

/// ミスの原因。`Game::apply_miss`の各呼び出し元がそれぞれ渡す。無敵中はこの原因ごとに
/// 異なる後始末(酸素の回復・押し潰したブロックの除去)が要るため、単なる記録用の区分
/// ではなく処理の分岐にも使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissCause {
    /// 酸素切れ(`update`の自然減少、または岩掘削の消費による`check_oxygen_zero`)
    OxygenOut,
    /// 落下してきたブロックに押し潰された(重力ティックの`life_lost_to_crush`)
    CrushedByFallingBlock,
    /// 落下中の頭上ブロックへ上向き掘削した(`push_drill_outcome_events`)
    DrilledIntoFallingBlock,
    /// ボムの爆風に巻き込まれた(`detonate_bombs`)
    BombBlast,
}

/// 消滅フラッシュ演出1セルぶんの進行状態。
///
/// 重力tickで消えたセルは、その瞬間まだ落下ブロックが空中にいる(落下補間の途中)。すぐ
/// 光らせると着地前のブロックの着地先が先に光るため、`delay`だけ待ってから開始する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VanishedCell {
    pos: board::Pos,
    /// 消滅直前のセルの種類。待機中はこの見た目のまま描き続ける。
    kind: Cell,
    /// フラッシュ開始までの待ち。重力tickで消えた場合は落下補間が終わるまで(≒1tick)、
    /// それ以外(掘削・スター溶解・ボム爆風等)は0。
    delay: Duration,
    /// フラッシュの残り時間。`delay`が0になってから減り始める。
    remaining: Duration,
    /// このセルのフラッシュ全長(進捗計算用)。実効tickで変わるためセルごとに保持する。
    total: Duration,
}

impl MissCause {
    /// デバッグログ(`miss_events`テーブル)へ記録する際の原因名。
    fn as_str(self) -> &'static str {
        match self {
            MissCause::OxygenOut => "OxygenOut",
            MissCause::CrushedByFallingBlock => "CrushedByFallingBlock",
            MissCause::DrilledIntoFallingBlock => "DrilledIntoFallingBlock",
            MissCause::BombBlast => "BombBlast",
        }
    }
}

/// ノーマルコース シングルプレイのゲーム状態一式。
///
/// フレーム巻き戻しがこの構造体を丸ごと複製してリングバッファへ積むため`Clone`を
/// 実装する。唯一クローンできないSQLite接続(`debug_log`)だけは`Rc`で共有し、複製された
/// スナップショット同士が同じログを指すようにする(ログは巻き戻しの対象外)。
#[derive(Clone)]
pub struct Game {
    pub board: Board,
    pub player: Player,
    pub status: GameStatus,
    gravity_state: GravityState,
    fall_tick_accum: Duration,
    /// プレイヤー自身の自由落下用のtick蓄積。ブロックの重力(`fall_tick_accum`)とは
    /// 別々に速度調整できるよう分離している。
    player_fall_tick_accum: Duration,
    /// ブロックの重力落下tick間隔(ms)。既定は`FALL_TICK_MS`で、デバッグショートカット
    /// (`debug_adjust_block_fall_speed`)から実行時調整できる。
    block_fall_tick_ms: u64,
    /// 支えを失ってから実際に落下し始めるまでの揺れ時間(ms)。既定は`SHAKE_DURATION_MS`で、
    /// `debug_adjust_shake_duration`から実行時調整できる。揺れティック数への変換は
    /// `block_fall_tick_ms`を使い都度計算する。
    shake_duration_ms: u64,
    /// プレイヤー自身の自由落下tick間隔(ms)。既定は`FALL_TICK_MS`で、デバッグショート
    /// カット(`debug_adjust_player_fall_speed`)から実行時調整できる。
    player_fall_tick_ms: u64,
    /// 横移動(MoveLeft/MoveRight)のクールダウン間隔(ms、小さいほど速い)。既定は
    /// `MOVE_COOLDOWN_MS_DEFAULT`で設定画面から調整できる。掘削(Drill)のクールダウンは
    /// 対象外で、引き続き`INPUT_COOLDOWN_MS`固定のまま。
    move_cooldown_ms: u64,
    /// 4連結以上の自動消滅が連鎖するとき、1回消滅するごとに次の重力解決までの最小
    /// インターバル(ms)。既定の`CHAIN_VANISH_INTERVAL_MS_DEFAULT`(0)なら即座に連鎖する。
    chain_vanish_interval_ms: u64,
    /// `chain_vanish_interval_ms`による足止めの残り時間。自動消滅の直後にこの値へセット
    /// され、0になるまで次の重力tickの解決を1tickぶんずつ足止めする。
    chain_pause_remaining: Duration,
    /// 移動系入力(MoveLeft/MoveRight)専用のクールダウン。掘削(Drill)と共有すると、
    /// 同一フレームで移動キーと掘削キーが両方来た場合に片方がブロックされるため分離した。
    ///
    /// 経過時間を毎フレーム蓄積し、1スロットぶん貯まったら受理してそのぶんだけ差し引く
    /// (0へリセットしない)ことで、キーリピート間隔とクールダウン周期が一致しない場合の
    /// 「一定間隔で移動が遅くなって見える」うなりを軽減する。貯め込んだぶんが後からまとめて
    /// 通らないよう、`INPUT_COOLDOWN_ACCUM_CAP_MS`(クールダウンの1.5倍)で上限を設ける。
    move_cooldown_accum: Duration,
    /// 掘削系入力(Drill)専用のクールダウンアキュムレータ。移動(MoveLeft/MoveRight)
    /// とは別に管理する。`move_cooldown_accum`と同じ考え方。
    drill_cooldown_accum: Duration,
    oxygen_warning_accum: Duration,
    /// ライフ消費で再開した直後、残り何ティックの間 押し潰し判定を無効化するか
    /// (spec.md 5章末尾)。
    invulnerability_ticks_remaining: u32,
    /// 直近でGameEvent::LevelUpを通知した時点のレベル番号(重複通知防止)。
    last_level_reported: usize,
    /// 直近でGameEvent::Checkpoint100mを通知した時点の区切り番号。重複通知防止と、
    /// スキマのくり抜き済み判定を兼ねる。地面(`CHECKPOINT_SAFE_ZONE_M`)を実際に
    /// 掘り抜いた地点で1増える。
    last_checkpoint_reported: usize,
    /// 押し潰しミス発生時、残りこれだけの間「潰れた」見た目を表示し続ける
    /// (0になったらGameOverオーバーレイの表示を許す。9章)。
    crush_flash_remaining: Duration,
    /// チェックポイント(100mごと)到達演出の残り時間。`0`より大きい間、描画側が
    /// 到達演出(バナー等)を表示する。
    checkpoint_flash_remaining: Duration,
    /// 直近のチェックポイント到達演出が表示している到達深度(m)。
    checkpoint_flash_depth_m: usize,
    /// 押し潰されてもライフが残っている場合の「天に召される」演出の残り時間。`Some`の間は
    /// プレイヤー自身の処理を凍結し、0になった時点で死亡地点の3列クリア・ライフ減算・
    /// 酸素回復をまとめて行いその場に復活する。ライフが0になる場合はこの演出を行わず、
    /// 即座にGameOverへ進む。
    ascending_remaining: Option<Duration>,
    /// 掘削入力(Space)を押した直後、方向別の掘削アニメーションを表示し続ける残り時間
    /// (9章)。描画専用で、ロジックには一切影響しない。
    drill_flash_remaining: Duration,
    /// 「わ〜!」スライダー演出の現在の段階。`None`なら演出無し。
    dodge_stage: DodgeStage,
    /// 現在の段階(スライダー/硬直)の残り時間。
    dodge_stage_remaining: Duration,
    /// 「わ〜!」スライダー直後の硬直インターバル(ms)。設定画面/デバッグショートカット
    /// で調整できる。
    dodge_recovery_ms: u64,
    /// ヒヤリ回避スライダーの監視対象セル。直前の移動で、移動前の頭上(row-1)が実際に
    /// 「揺れていた」場合のみ、その移動前の座標を監視対象に設定する(単に「最近動いた」
    /// だけでは誤発火するため、実際に頭上の脅威から逃げたことを条件にする)。この座標へ
    /// ブロックが着地した瞬間にスライダー演出を発火し、監視は解除される。
    dodge_watch_cell: Option<(usize, usize)>,
    /// 監視対象セルの有効期限。揺れていたブロックが実際に落下して監視対象セルへ到達する
    /// までの猶予で、経過すると監視は自動的に解除される。
    dodge_watch_remaining: Duration,
    /// 描画専用: プレイヤーの直前の論理位置(移動の見た目補間アニメーション用、9章)。
    /// ロジック上の当たり判定・掘削・落下判定には一切使わない。
    render_prev_position: (usize, usize),
    /// 直前の論理位置変化からの経過時間(秒)。`MOVE_ANIM_DURATION_MS`に達すると
    /// 補間が完了したものとして扱う。
    render_anim_elapsed: f32,
    /// 現在進行中の移動補間アニメーションの長さ(秒)。横移動は`move_anim_duration_secs()`
    /// (固定の短い時間)、自由落下は`player_fall_tick_ms`(実際の落下速度)を使い、移動の
    /// 種類に応じて`note_possible_move_with_duration`が設定する。
    render_anim_duration_secs: f32,
    /// 直近の重力ティックで実際に1マス落下した各セルの(移動後の位置, 移動前の位置)。
    /// 次のティックが来るまでの間、描画側がこれと`block_fall_progress()`を使って
    /// ブロック落下をピクセル単位で補間する。
    last_block_moves: Vec<BlockMove>,
    /// 直近に消滅した(自動消滅・スター溶解)セルと、消滅フラッシュ演出の進行状態。
    /// 描画側(render.rs)がこの座標に一瞬フラッシュ演出を出す。
    recently_vanished: Vec<VanishedCell>,
    /// ボム爆発の爆風が届いた直後のセルと、炎の演出の残り時間・爆心地からの距離。距離
    /// (0=爆心地、遠いほど大きい)で炎の色調を変え、`recently_vanished`と同じ考え方で
    /// 描画側(render.rs)がフラッシュ演出に使う。
    recently_exploded: Vec<(board::Pos, Duration, u8)>,
    /// GameOverダイアログでの現在の選択項目。GameOver状態でのみ意味を持つ。
    game_over_selection: GameOverChoice,
    /// `update()`が呼ばれるたびに1増えるフレーム通し番号。ブロック状態遷移ログ
    /// (`debug_log`)の各行と突き合わせるための識別子。
    frame_counter: u64,
    /// ブロック状態遷移ログ。`refresh_debug_log`で明示的に有効化するまでは`None`(no-op)
    /// のままなので、通常のテスト等では disk I/O が発生しない。
    ///
    /// SQLite接続はクローンできないため`Rc`で包む。巻き戻しのスナップショットは`Game`
    /// 丸ごとの複製なので、複製元・複製先・復元後の全てが同じ1つのログを指す。
    debug_log: Option<Rc<DebugLog>>,
    /// 現在盤面上にあるボム。
    bombs: Vec<Bomb>,
    /// ボム出現判定の経過時間蓄積。`BOMB_SPAWN_CHECK_INTERVAL_MS`ぶん貯まるたびに1回、
    /// 出現確率を判定する。
    bomb_spawn_check_accum_ms: u64,
    /// ボム出現頻度設定(%、100=既定)。設定画面から調整できる。
    bomb_spawn_rate_percent: u32,
    /// 新規出現ボムの起爆までの時間(ms)。既定`BOMB_FUSE_MS`。設置済みボムの
    /// `remaining_ms`には影響しない。
    bomb_fuse_ms: u32,
    /// アイテムブロック3種の出現率設定(%、100=既定)。`reroll_spawn_rates_from`で最新の
    /// 設定値に更新され、`top_up_items_ahead`がtickごとの窓補充で参照する。
    item_clear_above_rate_percent: u32,
    item_unify_colors_rate_percent: u32,
    item_starify_screen_rate_percent: u32,
    /// アイテムブロック3種について、この行より前は既に抽選済み(補充対象外)。プレイヤーが
    /// 進むたびに`target_row`(`player.row + ITEM_WINDOW_AHEAD_ROWS`)まで前進させ、新たに
    /// 範囲へ入った行だけを抽選する。一度抽選した行の内容は二度と変えない。
    item_top_up_frontier_row: usize,
    /// ボム出現位置・確率判定専用の乱数生成器。ゲームのシードから派生させるため、同じ
    /// シードなら同じ出現パターンが再現される(盤面生成と同じ決定性の考え方)。
    rng: ChaCha8Rng,
    /// 選択したコースのゴール深度(m)。`Board::generate`の深さ(=盤面の行数)でもある。
    /// 難易度カーブ(`depth_fraction`)はコースに関わらず`FIELD_DEPTH_M`(ノーマルコース
    /// 基準)で正規化するため、イージーコースはカーブの前半しか体験しない。
    depth_goal_m: usize,
    /// 無敵(ミス無効)かどうか。`true`の間、`apply_miss`はライフを減らさず`avert_miss`
    /// (回避イベントの記録と最小限の後始末)へ振り替える。ライフ喪失直後の一時的な無敵
    /// 時間(`invulnerability_ticks_remaining`)は押し潰し判定自体を抑止してミスの件数を
    /// 数えられなくする別物なので、両者は混ぜずに独立して扱う。
    invincible: bool,
    /// 無敵によって回避されたミスの累計回数。ソークテストで「長時間プレイ中に何回死ぬ
    /// 場面があったか」を数えるための指標。
    misses_averted: u32,
    /// 残っているフレーム巻き戻しの使用回数。開始時は`REWIND_STOCK_INITIAL`で、100m
    /// チェックポイント到達ごとに`REWIND_STOCK_PER_CHECKPOINT`ずつ`rewind_stock_max`まで
    /// 補充され、1回巻き戻すごとに1減る。
    rewind_stock: u8,
    /// 巻き戻しストックの上限。設定画面から`REWIND_STOCK_MAX_SETTING_MIN`〜`MAX`で調整
    /// でき、`0`なら巻き戻し機能そのものが無効になる(ストックも常に0にクランプされる)。
    rewind_stock_max: u8,
    /// 対戦の妨害ルール(#247)が有効か。通常の1人プレイでは`false`のまま(攻撃力の計測・
    /// 岩の投下をいずれも行わない)。デバッグショートカット(O)の初回押下、または将来の
    /// 対戦開始処理(#10)で`true`にする。
    attack_rules_enabled: bool,
    /// 自分が消したブロック数の累計のうち、まだ相手へ送っていない攻撃力。
    attack_power_pending: u32,
    /// 相手から届いたが、まだ岩へ変換していない攻撃力(比率未満の端数・上限超過分)。
    incoming_attack_power: u32,
    /// 予告中(まだ盤面に出ていない)の岩。
    incoming_rocks: Vec<IncomingRock>,
    /// 設定: 攻撃力いくつで岩1個か。
    attack_blocks_per_rock: u32,
    /// 設定: 1回(1ウェーブ)で降らせる岩の上限。
    attack_rocks_per_wave_max: u32,
}

impl Game {
    /// 指定シードで、既定ライフ数・既定フィールド幅・ノーマルコースの新しいゲームを
    /// 開始する。実行時は常に`new_with_width`(設定のフィールド幅を反映)経由で
    /// 生成されるため、これはテストの簡便用ヘルパー。
    #[cfg(test)]
    pub fn new(seed: u64) -> Self {
        Self::new_with_lives(seed, LIVES_DEFAULT)
    }

    /// 指定シード・ライフ数で、既定フィールド幅・ノーマルコースの新しいゲームを
    /// 開始する(spec.md 8章「1〜5機から選べる」)。テスト専用ヘルパー。
    #[cfg(test)]
    pub fn new_with_lives(seed: u64, lives: u8) -> Self {
        Self::new_with_lives_and_width(seed, lives, FIELD_WIDTH_DEFAULT)
    }

    /// 指定シード・フィールド幅・コースのゴール深度で、既定ライフ数の新しいゲームを
    /// 開始する。
    pub fn new_with_width(seed: u64, width: usize, depth_goal_m: usize) -> Self {
        Self::new_with_lives_and_width_and_depth_goal(seed, LIVES_DEFAULT, width, depth_goal_m)
    }

    /// 指定シード・ライフ数・フィールド幅で、ノーマルコースの新しいゲームを開始する
    /// テスト専用ヘルパー(実際に使うコース選択は`new_with_width`が担う)。範囲外の幅は
    /// `FIELD_WIDTH_MIN`〜`MAX`にクランプする。
    #[cfg(test)]
    pub fn new_with_lives_and_width(seed: u64, lives: u8, width: usize) -> Self {
        Self::new_with_lives_and_width_and_depth_goal(seed, lives, width, FIELD_DEPTH_M)
    }

    /// 指定シード・ライフ数・フィールド幅・コースのゴール深度で新しいゲームを開始する。
    /// 範囲外の幅は`FIELD_WIDTH_MIN`〜`MAX`にクランプする。
    fn new_with_lives_and_width_and_depth_goal(
        seed: u64,
        lives: u8,
        width: usize,
        depth_goal_m: usize,
    ) -> Self {
        let width = width.clamp(FIELD_WIDTH_MIN, FIELD_WIDTH_MAX);
        let mut player = Player::with_lives(lives);
        player.recenter_for_width(width);
        let last_level_reported = player.level();
        let last_checkpoint_reported = checkpoint_index_for_depth(player.depth_m());
        let start_position = player.position();
        let gravity_state = GravityState::new();
        let board = Board::generate(seed, depth_goal_m, width);
        Game {
            board,
            player,
            status: GameStatus::Playing,
            gravity_state,
            fall_tick_accum: Duration::ZERO,
            player_fall_tick_accum: Duration::ZERO,
            block_fall_tick_ms: FALL_TICK_MS,
            player_fall_tick_ms: FALL_TICK_MS,
            shake_duration_ms: SHAKE_DURATION_MS,
            move_cooldown_ms: MOVE_COOLDOWN_MS_DEFAULT,
            chain_vanish_interval_ms: CHAIN_VANISH_INTERVAL_MS_DEFAULT,
            chain_pause_remaining: Duration::ZERO,
            // ゲーム開始直後は即座に入力を受理できるよう、アキュムレータを満タン
            // (=1クールダウンぶん貯まっている状態)から始める。
            move_cooldown_accum: Duration::from_millis(MOVE_COOLDOWN_MS_DEFAULT),
            drill_cooldown_accum: Duration::from_millis(INPUT_COOLDOWN_MS),
            oxygen_warning_accum: Duration::ZERO,
            invulnerability_ticks_remaining: 0,
            last_level_reported,
            last_checkpoint_reported,
            crush_flash_remaining: Duration::ZERO,
            checkpoint_flash_remaining: Duration::ZERO,
            checkpoint_flash_depth_m: 0,
            ascending_remaining: None,
            drill_flash_remaining: Duration::ZERO,
            dodge_stage: DodgeStage::None,
            dodge_stage_remaining: Duration::ZERO,
            dodge_recovery_ms: DODGE_RECOVERY_MS_DEFAULT,
            dodge_watch_cell: None,
            dodge_watch_remaining: Duration::ZERO,
            render_prev_position: start_position,
            // 開始時点では補間の必要が無いため、既に完了した扱いにしておく
            // (さもないと初期表示が(0,0)相当からアニメーションしてしまう)。
            render_anim_elapsed: move_anim_duration_secs(),
            render_anim_duration_secs: move_anim_duration_secs(),
            last_block_moves: Vec::new(),
            recently_vanished: Vec::new(),
            recently_exploded: Vec::new(),
            game_over_selection: GameOverChoice::BackToTitle,
            frame_counter: 0,
            debug_log: None,
            bombs: Vec::new(),
            bomb_spawn_check_accum_ms: 0,
            bomb_spawn_rate_percent: crate::constants::SPAWN_RATE_PERCENT_DEFAULT,
            bomb_fuse_ms: BOMB_FUSE_MS,
            item_clear_above_rate_percent: crate::constants::SPAWN_RATE_PERCENT_DEFAULT,
            item_unify_colors_rate_percent: crate::constants::SPAWN_RATE_PERCENT_DEFAULT,
            item_starify_screen_rate_percent: crate::constants::SPAWN_RATE_PERCENT_DEFAULT,
            // 実際の設定値は直後に必ず呼ばれるreroll_spawn_rates_fromが反映するため、
            // ここでは未抽選(0)のまま初期化する。
            item_top_up_frontier_row: 0,
            // ボード生成(`Board::generate`)とは別系統の乱数列にするため、シードを
            // ビット反転して使う。同じゲームシードなら同じボム出現パターンが再現される。
            rng: ChaCha8Rng::seed_from_u64(!seed),
            depth_goal_m,
            invincible: false,
            misses_averted: 0,
            rewind_stock: REWIND_STOCK_INITIAL,
            rewind_stock_max: REWIND_STOCK_MAX_DEFAULT,
            attack_rules_enabled: false,
            attack_power_pending: 0,
            incoming_attack_power: 0,
            incoming_rocks: Vec::new(),
            attack_blocks_per_rock: ATTACK_BLOCKS_PER_ROCK_DEFAULT,
            attack_rocks_per_wave_max: ATTACK_ROCKS_PER_WAVE_MAX_DEFAULT,
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

    /// GameOverダイアログの現在の選択項目。
    pub fn game_over_selection(&self) -> GameOverChoice {
        self.game_over_selection
    }

    /// GameOverダイアログの選択をトグルする(2択なので↑↓どちらでも反転させる)。
    /// GameOver状態でのみ意味を持つ。
    pub fn toggle_game_over_selection(&mut self) {
        if self.status != GameStatus::GameOver {
            return;
        }
        self.game_over_selection = match self.game_over_selection {
            GameOverChoice::BackToTitle => GameOverChoice::Revive,
            GameOverChoice::Revive => GameOverChoice::BackToTitle,
        };
    }

    /// GameOverダイアログで「その場から復活」を選んだ場合の処理。ライフを既定値に戻し
    /// 酸素を全回復してPlayingへ戻す。深度・スコア・盤面はそのまま維持し、復活直後は
    /// 既存のライフ喪失時と同様に無敵時間を与える。
    pub fn revive(&mut self) {
        if self.status != GameStatus::GameOver {
            return;
        }
        self.player.lives = LIVES_DEFAULT;
        self.player.oxygen = crate::constants::OXYGEN_MAX;
        self.invulnerability_ticks_remaining = INVULNERABILITY_TICKS;
        self.status = GameStatus::Playing;
        // 巻き戻しストックも、ライフ・酸素と同じくゲーム開始時の値まで回復させる。
        self.rewind_stock = REWIND_STOCK_INITIAL.min(self.rewind_stock_max);
    }

    /// ← キー: facingをLeftにし、掘削を伴わない地形追従の移動を試みる(spec.md 1章)。
    pub fn try_move_left(&mut self) -> Vec<GameEvent> {
        self.try_lateral_move(Direction::Left)
    }

    /// → キー: facingをRightにし、掘削を伴わない地形追従の移動を試みる(spec.md 1章)。
    pub fn try_move_right(&mut self) -> Vec<GameEvent> {
        self.try_lateral_move(Direction::Right)
    }

    /// ←/→ 共通の処理本体(spec.md 1章)。掘削は一切発生しないため原則として`GameEvent`は
    /// 生じないが、移動先が酸素カプセル・アイテムだった場合のみ取得イベントを発火する。
    fn try_lateral_move(&mut self, dir: Direction) -> Vec<GameEvent> {
        if !self.consume_move_cooldown() {
            return Vec::new();
        }
        if !self.player_is_grounded() {
            // 直下が空いている(=次の自由落下tickで必ず1マス落ちる)間は横移動を
            // 受け付けない。デバッグで自由落下tickを遅くしていても同じ。
            return Vec::new();
        }
        if let BombInTheWay::HandledAsClimb(pickups) = self.push_bomb_in_the_way(dir) {
            // 押し出せなかった場合は段差登り判定を自分で処理済みなので、
            // 通常の物理判定(physics::move_lateral)は呼ばない。
            return self.events_for_climb_pickups(pickups);
        }

        let before = self.player.position();
        let outcome = physics::move_lateral(&mut self.board, &mut self.player, dir);
        self.note_possible_move(before);

        match outcome {
            LateralOutcome::MovedLevelAndCollectedOxygen => {
                vec![GameEvent::OxygenCollected]
            }
            LateralOutcome::MovedLevelAndCollectedItem(effect) => {
                let mut events = Vec::new();
                self.apply_item_effect(effect, &mut events);
                events
            }
            LateralOutcome::ClimbedStep { overhead, landing } => {
                self.events_for_climb_pickups([overhead, landing])
            }
            _ => Vec::new(),
        }
    }

    /// 段差登りの道中で取得したAIR・アイテム(自分の真上→登り先の順)を`GameEvent`へ
    /// 変換する。AIRを2マスまとめて取得しても`OxygenCollected`は1回だけにまとめ(重力
    /// ティック経路の`oxygen_collected > 0`と同じ畳み込み)、アイテムは取得した数だけ
    /// 効果を発動してそれぞれ`ItemCollected`を発火する。
    fn events_for_climb_pickups(&mut self, pickups: [Option<Pickup>; 2]) -> Vec<GameEvent> {
        let mut events = Vec::new();
        let pickups: Vec<Pickup> = pickups.into_iter().flatten().collect();
        if pickups.iter().any(|p| matches!(p, Pickup::Oxygen)) {
            events.push(GameEvent::OxygenCollected);
        }
        for pickup in pickups {
            if let Pickup::Item(effect) = pickup {
                self.apply_item_effect(effect, &mut events);
            }
        }
        events
    }

    /// ↑ キー: facingをUpに変更するのみ(移動・掘削は発生しない。spec.md 1章)。
    /// Left/Rightの2ステップ段差登りにおける「ぶつかって停止中」の状態もリセットする。
    pub fn face_up(&mut self) {
        if self.status == GameStatus::Playing && !self.is_input_frozen() {
            self.player.facing = Direction::Up;
            self.player.bumped_direction = None;
        }
    }

    /// ↓ キー: facingをDownに変更するのみ(移動・掘削は発生しない。spec.md 1章)。
    /// Left/Rightの2ステップ段差登りにおける「ぶつかって停止中」の状態もリセットする。
    pub fn face_down(&mut self) {
        if self.status == GameStatus::Playing && !self.is_input_frozen() {
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

        // 方向別の掘削アニメーション(9章)を開始する。命中/空振りを問わず、入力が
        // あった事実に対して反応する。
        self.drill_flash_remaining = Duration::from_millis(DRILL_ANIM_MS);

        if self.destroy_bomb_facing() {
            // ボムを掘削で除去した。岩ブロック等の通常の掘削処理は行わない
            // (プレイヤーの位置も変わらない)。
            events.push(GameEvent::BlockDestroyed { blocks: 1 });
            return events;
        }

        let before = self.player.position();
        let solid = self.settled_bomb_positions();
        let outcome = physics::drill_facing(
            &mut self.board,
            &mut self.player,
            &self.gravity_state,
            &solid,
        );
        self.push_drill_outcome_events(outcome, &mut events);
        self.note_possible_move(before);

        if self.player.row != before.0 {
            self.check_level_and_clear(&mut events);
        }
        events
    }

    /// 移動系入力(MoveLeft/MoveRight)のクールダウン(spec.md 9.9)が明けているかを確認し、
    /// 明けていれば1スロット消費する。Playing状態でない、またはクールダウン中は`false`。
    /// 掘削(Drill)とは独立しているので、同一フレームで両方の入力が来ても互いを妨げない。
    fn consume_move_cooldown(&mut self) -> bool {
        if self.status != GameStatus::Playing || self.is_input_frozen() {
            return false;
        }
        let slot = Duration::from_millis(self.move_cooldown_ms);
        if self.move_cooldown_accum < slot {
            return false;
        }
        // 0へリセットせず、消費した1スロットぶんだけ差し引く。キーリピートがクール
        // ダウン周期をわずかに過ぎて届いた場合、その超過ぶんは次のスロットへ繰り越す。
        self.move_cooldown_accum -= slot;
        true
    }

    /// 掘削系入力(Drill)のクールダウンが明けているかを確認し、明けていれば1スロット
    /// 消費する。移動(MoveLeft/MoveRight)とは独立したクールダウン。
    fn consume_drill_cooldown(&mut self) -> bool {
        if self.status != GameStatus::Playing || self.is_input_frozen() {
            return false;
        }
        let slot = Duration::from_millis(INPUT_COOLDOWN_MS);
        if self.drill_cooldown_accum < slot {
            return false;
        }
        self.drill_cooldown_accum -= slot;
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
                events.push(GameEvent::RockDestroyed { blocks });
                self.add_attack_power(blocks);
                self.check_oxygen_zero(events);
            }
            DrillOutcome::ColorDestroyed { blocks } => {
                events.push(GameEvent::DrillImpact);
                events.push(GameEvent::BlockDestroyed { blocks });
                self.add_attack_power(blocks);
            }
            DrillOutcome::OxygenUntouchedByDrill => {}
            DrillOutcome::CollectedDiamond => events.push(GameEvent::DiamondCollected),
            DrillOutcome::StarDestroyed => {
                events.push(GameEvent::DrillImpact);
                events.push(GameEvent::BlockDestroyed { blocks: 1 });
                self.add_attack_power(1);
            }
            DrillOutcome::CrushedByUnstableOverhead => {
                self.apply_miss(MissCause::DrilledIntoFallingBlock, events)
            }
            DrillOutcome::ItemUntouchedByDrill => {}
        }
    }

    /// アイテムブロックの効果を実際に発動し、対応するイベントを追加する。AIRと同様
    /// 「触れるだけで取得」のため、横移動・自由落下・重力ティックでの落下着地、いずれの
    /// 取得経路からも共通で呼ばれる。
    fn apply_item_effect(&mut self, effect: ItemEffect, events: &mut Vec<GameEvent>) {
        match effect {
            ItemEffect::ClearAbove => self.debug_clear_above_player(),
            ItemEffect::UnifyColors => {
                self.debug_unify_nearby_colors();
            }
            ItemEffect::StarifyScreen => self.debug_starify_visible_screen(),
        }
        events.push(GameEvent::ItemCollected(effect));
    }

    /// 酸素が0になっていればミス処理(ライフ喪失/ゲームオーバー)を行う。
    fn check_oxygen_zero(&mut self, events: &mut Vec<GameEvent>) {
        if self.status != GameStatus::Playing {
            return;
        }
        if self.player.is_out_of_oxygen() {
            self.apply_miss(MissCause::OxygenOut, events);
        }
    }

    /// ミス(酸素切れ/押し潰し/爆風)を処理する(spec.md 8章)。原因を問わず同じ処理を行う。
    ///
    /// ライフの残りを問わず「天に召される」演出(`ascending_remaining`)から開始し、死亡
    /// 地点の3列クリア・ライフ減算・酸素回復・GameOverへの遷移は、演出が終わるまで
    /// `tick_ascending`へ遅延させる。最後のライフだった場合も同じ演出を見せてから
    /// GameOverダイアログへ進む(#257)。
    ///
    /// 無敵(`set_invincible`)が有効な場合は、ライフ処理・演出を一切行わず`avert_miss`
    /// (回避として記録するだけ)へ振り替える。
    fn apply_miss(&mut self, cause: MissCause, events: &mut Vec<GameEvent>) {
        self.log_miss(cause, self.invincible);

        if self.invincible {
            return self.avert_miss(cause, events);
        }

        self.crush_flash_remaining = Duration::from_millis(CRUSH_FLASH_MS);

        // 死亡SEはミスが発生した瞬間に即座に鳴らす(遅らせると演出完了まで3秒近く無音に
        // なる)。最後のライフを失う場合はミス(ゲームオーバー)音、まだ残っていればライフ
        // ロス音を鳴らし分ける。ライフ減算・GameOver遷移・復活自体は演出完了
        // (tick_ascending)まで遅延する。
        if self.player.lives <= 1 {
            events.push(GameEvent::GameOverMiss { cause });
        } else {
            events.push(GameEvent::LifeLost { cause });
        }
        self.ascending_remaining = Some(Duration::from_millis(CRUSH_ASCEND_MS));
    }

    /// 無敵中にミスが起きた場合の処理。ライフ・ステータス・演出には一切触れず、回避した
    /// ことを記録した上で、放置すると同じミスが毎フレーム再発する原因だけを後始末する。
    fn avert_miss(&mut self, cause: MissCause, events: &mut Vec<GameEvent>) {
        self.misses_averted = self.misses_averted.saturating_add(1);

        match cause {
            // 酸素0のまま放置すると毎フレーム再検出されるので満タンに戻し、
            // 以後の自然減衰・警告は通常通り回す。
            MissCause::OxygenOut => self.player.oxygen = crate::constants::OXYGEN_MAX,
            // 押し潰したブロックはプレイヤーのマスに書き込まれたまま残る仕様
            // (通常は「天に召される」演出の完了時に消える)。無敵ではその演出を
            // 行わないため、ここで即座に消して消滅フラッシュを出す。
            MissCause::CrushedByFallingBlock => {
                let pos = self.player.position();
                let cell = self.board.cell(pos.0, pos.1);
                // 押し潰したブロックが同じtickで4連結自動消滅していた場合は既にEmpty。
                // その場合は消滅フラッシュを二重に積まない。
                if cell != Cell::Empty {
                    self.board.set(pos.0, pos.1, Cell::Empty);
                    // 押し潰しは重力tick内で起きるため、押し潰したブロック自身も
                    // まだ落下補間の途中にいる。到着を待ってからフラッシュする。
                    let delay = self.gravity_vanish_delay();
                    self.note_vanished_cells([(pos, cell)], delay);
                }
            }
            // 上向き掘削が押し潰しに終わった場合は盤面が変化していない(掘削自体が
            // 行われていない)。ボム爆風は爆風処理側が既に盤面を書き換え済み。
            // どちらも追加の後始末は不要。
            MissCause::DrilledIntoFallingBlock | MissCause::BombBlast => {}
        }

        events.push(GameEvent::MissAverted { cause });
    }

    /// ミスの発生(および無敵による回避)をデバッグログへ1行記録する。ログが無効
    /// (`debug_log`が`None`)なら何もしない。
    fn log_miss(&self, cause: MissCause, averted: bool) {
        if let Some(log) = &self.debug_log {
            let (row, col) = self.player.position();
            log.log_miss(self.frame_counter, cause.as_str(), averted, row, col);
        }
    }

    /// 無敵(ミス無効)を切り替える。ソークテストや動作確認で、死なずに長時間プレイし
    /// 続けるためのデバッグ機能。
    pub fn set_invincible(&mut self, on: bool) {
        self.invincible = on;
    }

    /// 現在、無敵(ミス無効)かどうか。
    pub fn is_invincible(&self) -> bool {
        self.invincible
    }

    /// 無敵によって回避されたミスの累計回数。
    pub fn misses_averted(&self) -> u32 {
        self.misses_averted
    }

    // --- フレーム巻き戻し ---------------------------------------------------

    /// 残っている巻き戻しの使用回数。HUD表示・GameOverダイアログのヒントが参照する。
    pub fn rewind_stock(&self) -> u8 {
        self.rewind_stock
    }

    /// 現在の巻き戻しストック上限(設定画面の`rewind_stock_max`)。`0`なら機能OFF。
    pub fn rewind_stock_max(&self) -> u8 {
        self.rewind_stock_max
    }

    /// 巻き戻しストックの上限を設定値から反映する。上限を下げた場合は現在のストックも
    /// そこまで切り下げる(`0`=OFFにすれば即座に巻き戻せなくなる)。
    pub fn set_rewind_stock_max(&mut self, max: u8) {
        self.rewind_stock_max = max;
        self.rewind_stock = self.rewind_stock.min(max);
    }

    /// 巻き戻しを開始できるか。ストックが残っていて、かつプレイ中(昇天演出中を含む)か
    /// GameOver中であること(一時停止中・クリア後は開始できない)。履歴が1つでもあるか
    /// どうかは`Game`の外(`rewind::RewindHistory`)が別途判定する。
    pub fn can_start_rewind(&self) -> bool {
        self.rewind_stock > 0 && matches!(self.status, GameStatus::Playing | GameStatus::GameOver)
    }

    /// 現在の状態をスナップショットとして履歴へ残してよいか。「天に召される」演出中
    /// (`is_dying`)は記録しない。これにより履歴の最新は常に「まだ生きていた最後の瞬間」
    /// になり、押し潰された直後に巻き戻しても死んだ状態そのものへは戻らない。
    pub fn is_rewind_capturable(&self) -> bool {
        self.status == GameStatus::Playing && !self.is_dying()
    }

    /// スナップショットの状態へ戻す。盤面・プレイヤー・ボム・各種タイマー・演出フラグ・
    /// 乱数(`rng`)は`Game`を丸ごと差し替えてまとめて巻き戻すが、以下は現在の値を持ち越す:
    ///
    /// - `frame_counter`: デバッグログのフレーム番号を単調増加に保つため
    /// - `debug_log`: 記録先は巻き戻しの対象ではないため(同じ`Rc`を維持する)
    /// - `invincible` / `misses_averted`: デバッグ機能の状態・累計であり巻き戻さない
    /// - `rewind_stock`: 現在値から1消費する(スナップショット時点の値へは戻さない)
    /// - `rewind_stock_max`: 設定値であり巻き戻しの対象ではない
    /// - `attack_blocks_per_rock` / `attack_rocks_per_wave_max`: 同上(#247の設定値)。
    ///   攻撃力・受信待ちプール・予告中の岩は状態値なのでスナップショットごと巻き戻す
    pub fn restore_for_rewind(&mut self, snapshot: &Game) {
        let frame_counter = self.frame_counter;
        let debug_log = self.debug_log.clone();
        let invincible = self.invincible;
        let misses_averted = self.misses_averted;
        let rewind_stock = self.rewind_stock.saturating_sub(1);
        let rewind_stock_max = self.rewind_stock_max;
        let attack_blocks_per_rock = self.attack_blocks_per_rock;
        let attack_rocks_per_wave_max = self.attack_rocks_per_wave_max;

        *self = snapshot.clone();

        self.frame_counter = frame_counter;
        self.debug_log = debug_log;
        self.invincible = invincible;
        self.misses_averted = misses_averted;
        self.rewind_stock = rewind_stock;
        self.rewind_stock_max = rewind_stock_max;
        self.attack_blocks_per_rock = attack_blocks_per_rock;
        self.attack_rocks_per_wave_max = attack_rocks_per_wave_max;

        if let Some(log) = &self.debug_log {
            log.log_rewind(self.frame_counter, self.rewind_stock);
        }
    }

    /// 移動・向き・掘削の5操作を1つの入口へまとめたもの。main.rsの手入力処理と、オート
    /// プレイ(`autoplay::Autopilot`)が返す仮想入力の両方がこれを通ることで、AIが人間と
    /// 同じ経路でしかゲームを動かせないことを保証する。それ以外の`InputAction`は`Game`の
    /// 内部状態を変えない(main.rsが解釈する)ため、ここでは何もしない。
    pub fn apply_input(&mut self, action: InputAction) -> Vec<GameEvent> {
        match action {
            InputAction::MoveLeft => self.try_move_left(),
            InputAction::MoveRight => self.try_move_right(),
            InputAction::FaceUp => {
                self.face_up();
                Vec::new()
            }
            InputAction::FaceDown => {
                self.face_down();
                Vec::new()
            }
            InputAction::Drill => self.try_drill(),
            _ => Vec::new(),
        }
    }

    /// セル`(row, col)`が「落ちてくる可能性がある」かどうか。支えを失っている塊、または
    /// 揺れの猶予期間中(これから落ちる予告状態)の塊に属していれば`true`。Empty/AIR/
    /// アイテムは脅威にならないため常に`false`。オートプレイが安全確認に使う。
    pub fn is_cell_unstable(&self, row: usize, col: usize) -> bool {
        if row >= self.board.depth_rows() || col >= self.board.width() {
            return false;
        }
        physics::is_falling_hazard(
            &self.board,
            &self.gravity_state,
            (row, col),
            self.player.position(),
            &self.settled_bomb_positions(),
        )
    }

    /// 「天に召される」演出の進行を1フレームぶん進める。演出中は`is_input_frozen`経由で
    /// プレイヤー自身の入力・自由落下・酸素減少だけが凍結され、周囲の落下ブロックの重力
    /// 処理は止めない。演出が終わった瞬間、死亡地点の3列クリア・押し潰したブロック自体の
    /// クリア・ライフ減算をまとめて行い、ライフが残っていれば酸素を全回復してその場に
    /// 復活する。最後のライフだった場合はここでGameOverへ遷移する(#257)。
    fn tick_ascending(&mut self, delta: Duration, events: &mut Vec<GameEvent>) {
        let Some(remaining) = self.ascending_remaining else {
            return;
        };
        let remaining = remaining.saturating_sub(delta);
        if remaining == Duration::ZERO {
            self.ascending_remaining = None;
            // 押し潰したブロック自体は、潰された様子が見えるよう演出中その場に残して
            // いた。演出が終わるのでここで消す(死亡時の盤面処理より先に行い、この演出用の
            // 残骸をブロック/キャラ重なり解消の対象にしない)。
            self.board
                .set(self.player.row, self.player.col, Cell::Empty);
            // resolve_death_board_effectsは、内部で呼ぶdebug_clear_above_player等が
            // status == Playingを前提にしているため、statusを変更する前に呼ぶ。
            self.resolve_death_board_effects(events);
            if self.player.lose_life() {
                self.status = GameStatus::GameOver;
                self.game_over_selection = GameOverChoice::BackToTitle;
                // GameEvent::GameOverMiss(ミス音)は演出開始時にapply_missで既に発火済み
                // のため、ここでは何も発火しない(復活しないのでRevivedも出さない)。
            } else {
                self.invulnerability_ticks_remaining = INVULNERABILITY_TICKS;
                // GameEvent::LifeLost(死亡SE)は押し潰された瞬間にapply_missで既に発火済み
                // のため重複させない。復活した瞬間のSEだけをここで発火する。
                events.push(GameEvent::Revived);
            }
        } else {
            self.ascending_remaining = Some(remaining);
        }
    }

    /// 「わ〜!」スライダー演出の進行を1フレームぶん進める。演出中(スライダー/硬直の
    /// いずれか)は`is_input_frozen`経由で入力のみが凍結され、重力・自由落下・酸素減少は
    /// 通常通り進み続ける(全体が止まって見えないようにするため)。
    fn tick_dodge(&mut self, delta: Duration) {
        match self.dodge_stage {
            DodgeStage::None => {}
            DodgeStage::Sliding => {
                let remaining = self.dodge_stage_remaining.saturating_sub(delta);
                if remaining == Duration::ZERO {
                    self.dodge_stage = DodgeStage::Recovering;
                    self.dodge_stage_remaining = Duration::from_millis(self.dodge_recovery_ms);
                } else {
                    self.dodge_stage_remaining = remaining;
                }
            }
            DodgeStage::Recovering => {
                let remaining = self.dodge_stage_remaining.saturating_sub(delta);
                if remaining == Duration::ZERO {
                    self.dodge_stage = DodgeStage::None;
                } else {
                    self.dodge_stage_remaining = remaining;
                }
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
            // Lv.10ごとにライフ+1(LIVES_MAXでクランプする)。
            if level.is_multiple_of(10) {
                self.player.lives = (self.player.lives + 1).min(LIVES_MAX);
                events.push(GameEvent::ExtraLifeAtLevel { level });
            }
        }

        // チェックポイント(100mごと)。7章のレベル進行(30m刻み、表示のみ)とは別に、
        // 地面(`CHECKPOINT_SAFE_ZONE_M`)を実際に掘り抜いた地点で頭上を全クリアし、
        // ゴールSE・演出を出す。地面部分はプレイヤー自身がドリルで掘り進む対象なので、
        // ここでのくり抜きはスキマ(`CHECKPOINT_ZONE_GAP_M`)のみが対象。最終ゴール
        // (depth_goal_m)ちょうどはClearedイベントが同じ役割の演出を持つため二重に発火
        // させない(イージーコースの500mボーナスフロアも自動的にここでスキップされる)。
        //
        // 通常のプレイでは行が1つ変化するたびに呼ばれるため区切りの増分は常に高々1。
        // 2つ以上一気に進むのはテスト等が`player.row`を直接書き換えた非正規経路であり、
        // そこで頭上を破壊的に全クリアするのは意図と異なるので、番号の追従だけ行う。
        let checkpoint = checkpoint_index_for_depth(self.player.depth_m());
        if checkpoint > self.last_checkpoint_reported {
            let skipped_ahead = checkpoint > self.last_checkpoint_reported + 1;
            self.last_checkpoint_reported = checkpoint;
            let at_m = checkpoint * CHECKPOINT_STEP_M;
            if !skipped_ahead && self.status == GameStatus::Playing && at_m < self.depth_goal_m {
                self.debug_clear_above_player();
                self.apply_checkpoint_safe_zone(at_m);
                self.checkpoint_flash_remaining = Duration::from_millis(CHECKPOINT_FLASH_MS);
                self.checkpoint_flash_depth_m = at_m;
                // 巻き戻しストックの補充(上限を超えては増えない)。
                self.rewind_stock = self
                    .rewind_stock
                    .saturating_add(REWIND_STOCK_PER_CHECKPOINT)
                    .min(self.rewind_stock_max);
                events.push(GameEvent::Checkpoint100m { at_m });
            }
        }

        if self.status == GameStatus::Playing && self.player.depth_m() >= self.depth_goal_m {
            self.status = GameStatus::Cleared;
            events.push(GameEvent::Cleared);
        }

        // アイテムブロック3種の窓補充。行が進むたびに窓(プレイヤーの現在行
        // +ITEM_WINDOW_AHEAD_ROWS)を前進させる。
        self.top_up_items_ahead();
    }

    /// メインループから毎フレーム呼ぶ。deltaぶんの時間経過(酸素減少・落下tick)を反映する。
    pub fn update(&mut self, delta: Duration) -> Vec<GameEvent> {
        let mut events = Vec::new();
        self.frame_counter += 1;

        // このフレームのブロック変化ログをまとめて1トランザクションにし(insertの
        // 高速化)、キャラの位置・向き・ステータスもフレームに1回だけ記録する。
        if let Some(log) = &self.debug_log {
            log.begin_frame();
            log.log_player_state(
                self.frame_counter,
                self.player.row,
                self.player.col,
                &format!("{:?}", self.player.facing),
                &format!("{:?}", self.status),
            );
        }

        // 押し潰し演出・移動補間の経過時間は、GameOverでPlaying状態を抜けた後も
        // 描画側が最後まで追従できるよう、Playingガードより前に進めておく。
        self.crush_flash_remaining = self.crush_flash_remaining.saturating_sub(delta);
        self.checkpoint_flash_remaining = self.checkpoint_flash_remaining.saturating_sub(delta);
        self.render_anim_elapsed += delta.as_secs_f32();
        // 消滅フラッシュは、まず開始待ち(`delay`)を消化し、余った時間だけフラッシュ本体
        // (`remaining`)を進める。待機中はまだ落下ブロックが空中にいるため、光り始めずに
        // 消滅直前の見た目を保持する。
        for entry in self.recently_vanished.iter_mut() {
            let mut rest = delta;
            if entry.delay > Duration::ZERO {
                let consumed = rest.min(entry.delay);
                entry.delay -= consumed;
                rest -= consumed;
            }
            entry.remaining = entry.remaining.saturating_sub(rest);
        }
        self.recently_vanished
            .retain(|e| e.delay > Duration::ZERO || e.remaining > Duration::ZERO);
        for (_, remaining, _) in self.recently_exploded.iter_mut() {
            *remaining = remaining.saturating_sub(delta);
        }
        self.recently_exploded
            .retain(|&(_, remaining, _)| remaining > Duration::ZERO);

        if self.status != GameStatus::Playing {
            // GameOverになった瞬間に落下中だったブロック(押し潰したブロック自身を含む)の
            // 補間を、着地位置まで進め切ってから止める。ここで早期returnすると
            // `fall_tick_accum`が進まず、空中の中途半端な位置で凍り付いたままフラッシュ→
            // GameOverオーバーレイへ移ってしまう。一時停止(Paused)は対象外(再開時に
            // 大きなdeltaがまとめて来てtickが飛ぶのを避けるため)。
            if self.status == GameStatus::GameOver {
                let tick = Duration::from_millis(self.effective_block_fall_tick_ms());
                self.fall_tick_accum = (self.fall_tick_accum + delta).min(tick);
            }
            return events;
        }

        // 「天に召される」演出中は、プレイヤー自身の入力・自由落下・酸素減少のみを凍結し、
        // 周囲の落下ブロックの重力処理は止めない。演出完了時のブロッククリア・ライフ
        // 減算・酸素回復はtick_ascending内で行う。
        //
        // 完了したかどうかは、この呼び出し**前**の状態(`was_dying`)で判定する。呼び出し後に
        // is_dying()を見ると、演出がこのフレームで完了した場合に余った経過時間が「復活直後
        // のプレイヤー」へ適用され、全回復させた酸素を同じフレーム内で減衰させてしまう。
        let was_dying = self.is_dying();
        self.tick_ascending(delta, &mut events);

        // 演出の完了で最後のライフを失いGameOverになった場合は、この時点で打ち切る。
        // 続けてしまうと、酸素切れ死のときは酸素が0のまま(最後のライフでは
        // `lose_life`が回復しない)なので、同じフレームの酸素切れ判定が再び真になり
        // `apply_miss`が二重に走る。その結果`ascending_remaining`が再セットされ、
        // 以後のupdateはGameOverで早期returnして`tick_ascending`へ到達しなくなるため、
        // 演出が永久に終わらずGameOverダイアログが表示できなくなる。
        if self.status != GameStatus::Playing {
            return events;
        }

        // 「わ〜!」スライダー演出中は入力のみを凍結する(is_input_frozenが各入力ハンドラ
        // で担う)。周囲の重力・自由落下・酸素減少は止めない。
        self.tick_dodge(delta);

        // ヒヤリ回避スライダーの監視対象セルの有効期限を進める。揺れていたブロックが
        // 監視対象セルへ実際に落下する前に期限が切れたら監視解除する。
        if self.dodge_watch_cell.is_some() {
            self.dodge_watch_remaining = self.dodge_watch_remaining.saturating_sub(delta);
            if self.dodge_watch_remaining == Duration::ZERO {
                self.dodge_watch_cell = None;
            }
        }

        // 「天に召される」演出中は、プレイヤー自身に関する経過処理(酸素減少・クール
        // ダウン)だけを凍結する。復活直後の二重減衰を避けるため`was_dying`(呼び出し前の
        // 状態)で判定し、演出がこのフレームで完了しても再開は次のフレームからにする。
        if !was_dying {
            self.player.elapsed_seconds += delta.as_secs_f32();

            // 移動クールダウンは設定で変えられるため、上限も現在の値の1.5倍で都度計算
            // する。掘削クールダウンは固定値なので既存の定数上限のままでよい。
            let move_accum_cap =
                Duration::from_millis(self.move_cooldown_ms + self.move_cooldown_ms / 2);
            let drill_accum_cap = Duration::from_millis(INPUT_COOLDOWN_ACCUM_CAP_MS);
            self.move_cooldown_accum = (self.move_cooldown_accum + delta).min(move_accum_cap);
            self.drill_cooldown_accum = (self.drill_cooldown_accum + delta).min(drill_accum_cap);
            self.drill_flash_remaining = self.drill_flash_remaining.saturating_sub(delta);

            // 深度が進むほど酸素の自然減少を速くする。経過時間そのものを実効倍率ぶん
            // 引き伸ばすことで、`OXYGEN_DECAY_PER_SEC`(秒あたりの基準減少量)は変えずに
            // 実質的な減少速度だけを深度に応じて上げる。
            let oxygen_decay_multiplier = 1.0
                + depth_fraction(self.player.depth_m()) * (OXYGEN_DECAY_DEPTH_MAX_MULTIPLIER - 1.0);
            physics::apply_oxygen_decay(
                &mut self.player,
                delta.as_secs_f32() * oxygen_decay_multiplier,
            );

            if self.player.oxygen > 0.0 && self.player.oxygen <= OXYGEN_WARNING_THRESHOLD {
                self.oxygen_warning_accum += delta;
                if self.oxygen_warning_accum >= Duration::from_secs(1) {
                    self.oxygen_warning_accum -= Duration::from_secs(1);
                    events.push(GameEvent::OxygenWarningTick);
                }
            } else {
                self.oxygen_warning_accum = Duration::ZERO;
            }
        }

        if !self.is_dying() && self.player.is_out_of_oxygen() {
            self.apply_miss(MissCause::OxygenOut, &mut events);
            if self.status != GameStatus::Playing {
                return events;
            }
        }

        // 深度が進むほどブロック落下速度を上げる。設定画面/デバッグショートカットで
        // 調整した`block_fall_tick_ms`を「深度0mでの速度」として扱い、そこから深度に
        // 応じてtick間隔を短縮する。
        let effective_tick_ms = self.effective_block_fall_tick_ms();
        self.fall_tick_accum += delta;
        let tick = Duration::from_millis(effective_tick_ms);
        while self.fall_tick_accum >= tick {
            self.fall_tick_accum -= tick;

            // 自動消滅の連鎖インターバル。直前の自動消滅から`chain_vanish_interval_ms`が
            // 経過していなければ、この1tickぶんは重力解決自体を足止めする(既定の0なら
            // 何もしない=即座に解決する)。
            if self.chain_pause_remaining > Duration::ZERO {
                self.chain_pause_remaining = self.chain_pause_remaining.saturating_sub(tick);
                // 足止めするtickでも、前tickの落下補間はここで完了として確定させる。
                // `last_block_moves`を残したままにすると、足止め中に同じ移動の補間が
                // 0から再生され、着地済みのブロックが巻き戻って見えてしまう。
                self.last_block_moves.clear();
                continue;
            }

            // 「天に召される」演出中も重力処理自体は止めないため、プレイヤーの論理位置は
            // 演出完了まで押し潰された地点に固定されたままになる。その間に別の塊が同じ
            // 地点へ落ちてきても二重にライフを失わないよう、演出中は無敵として扱う。
            let invulnerable = self.invulnerability_ticks_remaining > 0 || self.is_dying();
            let shake_ticks = self.shake_ticks();
            let solid = self.settled_bomb_positions();
            let result = physics::process_gravity_tick(
                &mut self.board,
                &mut self.player,
                &solid,
                &mut self.gravity_state,
                invulnerable,
                shake_ticks,
            );
            // `invulnerable`は「天に召される」演出中(is_dying)にも真になるが、その場合
            // カウンタ自体は0のままなので、動いている場合のみ減算する(0からの減算で
            // オーバーフローするのを防ぐ)。
            if self.invulnerability_ticks_remaining > 0 {
                self.invulnerability_ticks_remaining -= 1;
            }

            // ブロックが落ち始める直前に移動して間一髪回避した場合、「わ〜!」スライダー
            // 演出を発火する。`dodge_watch_cell`は移動前の頭上が実際に揺れていた場合のみ
            // 設定されているため、その監視対象セルへちょうど今ブロックが着地した場合だけ
            // 発火する(押し潰された場合や、既に演出中の場合は対象外)。
            if !result.life_lost_to_crush
                && !self.is_dying()
                && self.dodge_stage == DodgeStage::None
                && let Some(watch_cell) = self.dodge_watch_cell
                && result
                    .moved_cells
                    .iter()
                    .any(|&(to, _)| to == watch_cell && to != self.player.position())
            {
                self.dodge_stage = DodgeStage::Sliding;
                self.dodge_stage_remaining = Duration::from_millis(DODGE_SLIDE_MS);
                self.dodge_watch_cell = None;
                events.push(GameEvent::DodgeTriggered);
            }

            if let Some(log) = &self.debug_log {
                for &(to, from) in &result.moved_cells {
                    let kind = self.board.cell(to.0, to.1);
                    log.log_move(self.frame_counter, to, from, &format!("{kind:?}"));
                }
            }
            self.last_block_moves = result.moved_cells;

            if result.oxygen_collected > 0 {
                events.push(GameEvent::OxygenCollected);
            }
            for effect in result.items_collected {
                self.apply_item_effect(effect, &mut events);
            }
            if result.auto_vanished_blocks > 0 {
                events.push(GameEvent::BlockDestroyed {
                    blocks: result.auto_vanished_blocks,
                });
                self.add_attack_power(result.auto_vanished_blocks);
            }
            if result.auto_vanished_rock_blocks > 0 {
                // 岩ブロックの自動消滅は得点対象外だが、専用の破壊音を鳴らす
                // (spec.md 4.9・10章)。
                events.push(GameEvent::RockDestroyed {
                    blocks: result.auto_vanished_rock_blocks,
                });
                self.add_attack_power(result.auto_vanished_rock_blocks);
            }
            // 重力tickで消えたセルは、落下補間が終わる(=次のtickが来る)まで待ってから
            // フラッシュを始める。
            let vanish_delay = self.gravity_vanish_delay();
            self.note_vanished_cells(result.vanished_cells, vanish_delay);
            self.purge_checkpoint_zone_debris(vanish_delay);
            self.log_board_snapshot_if_due();

            if (result.auto_vanished_blocks > 0 || result.auto_vanished_rock_blocks > 0)
                && self.chain_vanish_interval_ms > 0
            {
                self.chain_pause_remaining = Duration::from_millis(self.chain_vanish_interval_ms);
            }

            if result.life_lost_to_crush {
                self.apply_miss(MissCause::CrushedByFallingBlock, &mut events);
            }

            if self.status != GameStatus::Playing {
                return events;
            }
        }

        // スターブロックの溶解は実時間(ms)で進む。深度に応じて間隔が変わるブロック落下
        // tick(`effective_tick_ms`)とは切り離し、このフレームの実経過時間`delta`そのもの
        // で進行させることで、深度によらず常に一定の猶予時間になる。
        let melted = tick_star_melting(&mut self.board, self.player.row, delta.as_millis() as u32);
        if !melted.is_empty() {
            events.push(GameEvent::BlockDestroyed {
                blocks: melted.len(),
            });
            // スター溶解は落下とは無関係にその場で消えるため、待たずに光り始める。
            self.note_vanished_cells(melted, Duration::ZERO);
        }

        // ボムの進行(登場→転がり→静止→起爆カウントダウン→誘爆・新規出現判定)。
        // 「天に召される」演出中の扱いは`tick_bombs`内で判定する。
        if !self.tick_bombs(delta, was_dying, &mut events) {
            return events;
        }

        // 相手の攻撃で降ってくる岩(#247)。ボムと同じく「天に召される」演出中は止める。
        // 予告を進めて出現させたあと、空いた予告キューへ次のウェーブを積む。
        if !was_dying && self.attack_rules_enabled {
            let delta_ms = delta.as_millis() as u32;
            self.tick_incoming_rocks(delta_ms, &mut events);
            self.try_start_incoming_wave();
        }

        // プレイヤー自身の自由落下(spec.md 1章)。ブロックの重力とは別々に速度調整できる
        // よう、独立したtick間隔(`player_fall_tick_ms`)の別ループに分離している。入力・
        // 掘削とは無関係に、支えを失っていれば(直下がEmptyなら)落下し、直下が酸素カプセル
        // なら掘削不要で「歩くだけで取得」する。「天に召される」演出中は蓄積も含めて凍結
        // する(復活直後に積み残し分がまとめて落ちてしまうのを防ぐ)。
        if !self.is_dying() {
            self.player_fall_tick_accum += delta;
            let player_tick = Duration::from_millis(self.player_fall_tick_ms);
            while self.player_fall_tick_accum >= player_tick {
                self.player_fall_tick_accum -= player_tick;

                let before_fall = self.player.position();
                // 直下に設置済み(Settling/Ticking)のボムがあれば自由落下はそこを通過
                // させない。ボムはCellグリッド外のオーバーレイで盤面上はEmptyのままなので、
                // チェックしないとプレイヤーとボムが同じマスに重なって見える。
                let fall_outcome = if self.settled_bomb_at(self.player.row + 1, self.player.col) {
                    FreeFallOutcome::DidNotFall
                } else {
                    physics::apply_player_free_fall(&mut self.board, &mut self.player)
                };
                self.note_possible_move_with_duration(
                    before_fall,
                    self.player_fall_tick_ms as f32 / 1000.0,
                );
                if fall_outcome == FreeFallOutcome::FellAndCollectedOxygen {
                    events.push(GameEvent::OxygenCollected);
                }
                if let FreeFallOutcome::FellAndCollectedItem(effect) = fall_outcome {
                    self.apply_item_effect(effect, &mut events);
                }
                if self.player.row != before_fall.0 {
                    self.check_level_and_clear(&mut events);
                    if self.status != GameStatus::Playing {
                        break;
                    }
                }
            }
        }

        events
    }

    /// プレイヤーが現在支持されている(直下が塞がっている、または最深行に到達している)
    /// かどうか。支持されていなければ次の自由落下tickで必ず1マス落ちる状態で、その間は
    /// 横移動を受け付けない。直下が酸素カプセルの場合も自由落下でそのまま通過するため、
    /// 支持されているとはみなさない。オートプレイ(`autoplay.rs`)も「今フレームに横移動が
    /// 通るか」の判定へ使うため公開している。
    pub fn player_is_grounded(&self) -> bool {
        let below = self.player.row + 1;
        if below >= self.board.depth_rows() {
            return true;
        }
        // 設置済みのボムはCellグリッド外だが、自由落下を止める支えとして扱う(直下の
        // セル自体はEmptyのまま残るため、盤面だけ見ると支持なしに見える)。
        if self.settled_bomb_at(below, self.player.col) {
            return true;
        }
        !matches!(
            self.board.cell(below, self.player.col),
            Cell::Empty | Cell::Oxygen
        )
    }

    /// プレイヤーの位置が`before`から変化していれば、移動の見た目補間アニメーションを
    /// (描画専用の状態として)`move_anim_duration_secs()`(固定の短い時間)で開始する。
    /// ロジック上の位置(row/col)には一切影響しない(9章)。
    fn note_possible_move(&mut self, before: (usize, usize)) {
        self.note_possible_move_with_duration(before, move_anim_duration_secs());
    }

    /// `note_possible_move`の、補間時間を指定できる版。自由落下は固定の短い時間ではなく
    /// `player_fall_tick_ms`(実際の落下tick間隔)ぶんかけて補間することで、次のtickが来る
    /// までの間ずっと滑らかに動き続ける。
    fn note_possible_move_with_duration(&mut self, before: (usize, usize), duration_secs: f32) {
        let after = self.player.position();
        if after != before {
            self.render_prev_position = before;
            self.render_anim_elapsed = 0.0;
            self.render_anim_duration_secs = duration_secs.max(0.001);
            self.arm_dodge_watch_if_fled_a_shaking_block(before);
        }
    }

    /// 直前の移動が「頭上で揺れているブロックからの回避」だったかを判定し、該当すれば
    /// ヒヤリ回避スライダーの監視対象セルを設定する(単に「最近動いた」だけでは誤発火する
    /// ため)。該当しない移動なら、古い監視が誤って生き残らないよう監視を解除する。
    fn arm_dodge_watch_if_fled_a_shaking_block(&mut self, before: (usize, usize)) {
        let is_threatened = before.0 > 0 && self.gravity_state.is_shaking((before.0 - 1, before.1));
        if is_threatened {
            self.dodge_watch_cell = Some(before);
            self.dodge_watch_remaining = Duration::from_millis(DODGE_DETECT_WINDOW_MS);
        } else {
            self.dodge_watch_cell = None;
        }
    }

    /// 描画側が使う、移動補間の進捗(0.0=直前位置にいる, 1.0=現在位置に到達済み)。
    pub fn move_anim_progress(&self) -> f32 {
        (self.render_anim_elapsed / self.render_anim_duration_secs).clamp(0.0, 1.0)
    }

    /// 描画側が使う、移動補間の起点(直前の論理位置)。
    pub fn render_prev_position(&self) -> (usize, usize) {
        self.render_prev_position
    }

    /// 描画側が使う、直近の重力ティックで実際に1マス落下した各セルの(移動後の位置,
    /// 移動前の位置)一覧。ブロック落下のピクセル単位補間描画に使い、次のティックが
    /// 実行されるまでこのティックの内容を保持し続ける。
    pub fn recently_moved_blocks(&self) -> &[BlockMove] {
        &self.last_block_moves
    }

    /// ブロック状態遷移ログの記録先を新規に作り直して有効化する(ゲーム開始のたびに
    /// リフレッシュする)。開けなかった場合は記録自体を諦め、ゲーム進行には影響させない。
    /// `enabled`が`false`なら記録を行わない。設定画面から切り替えた場合に稼働中のgameへ
    /// 即座に反映する用途にも使う。
    pub fn refresh_debug_log(&mut self, enabled: bool) {
        self.debug_log = if enabled {
            // 巻き戻しのスナップショットが同じログ接続を共有できるようRcで包む。
            DebugLog::open_fresh().map(Rc::new)
        } else {
            None
        };
    }

    /// `update()`が呼ばれるたびに1増えるフレーム通し番号。ブロック状態遷移ログの各行と
    /// 突き合わせるための識別子として画面に表示する。
    pub fn debug_frame(&self) -> u64 {
        self.frame_counter
    }

    /// 消滅したセルを消滅フラッシュ演出の対象として記録する。
    ///
    /// 新たに消滅したセルに隣接するセルがまだフラッシュ中なら、その残り時間をこの
    /// フラッシュぶんへ延長する。これにより、落下したブロックが着地して連鎖的に4連結
    /// 消滅した場合でも、古い方の演出が先にフェードアウトして途切れず1つの連鎖に見える。
    /// `delay`はフラッシュを始めるまでの待ち時間。重力tickで消えたセルは落下補間が終わる
    /// まで(`gravity_vanish_delay`)待ち、それ以外は待たずに即座に光り始める。
    fn note_vanished_cells(
        &mut self,
        cells: impl IntoIterator<Item = (board::Pos, Cell)>,
        delay: Duration,
    ) {
        let total = Duration::from_millis(self.vanish_flash_duration_ms());
        let new_cells: Vec<(board::Pos, Cell)> = cells.into_iter().collect();

        if let Some(log) = &self.debug_log {
            for &(pos, kind) in &new_cells {
                log.log_vanish(self.frame_counter, pos, &format!("{kind:?}"));
            }
        }

        for &((row, col), _) in &new_cells {
            for (dr, dc) in [(-1isize, 0isize), (1, 0), (0, -1), (0, 1)] {
                let nr = row as isize + dr;
                let nc = col as isize + dc;
                if nr < 0 || nc < 0 {
                    continue;
                }
                let neighbor = (nr as usize, nc as usize);
                if let Some(entry) = self
                    .recently_vanished
                    .iter_mut()
                    .find(|e| e.pos == neighbor && e.delay.is_zero())
                {
                    // 既にフラッシュ中の隣接セルは、新規分がフラッシュを終えるのと同じ
                    // 時刻まで残り時間を伸ばし、一緒に消えるようにする。まだ待機中
                    // (delay>0)のセルは、待ちが明けてから自分で始めればよいので触らない。
                    entry.remaining = delay + total;
                }
            }
        }

        self.recently_vanished
            .extend(new_cells.into_iter().map(|(pos, kind)| VanishedCell {
                pos,
                kind,
                delay,
                remaining: total,
                total,
            }));
    }

    /// 重力tick内で消滅したセルの、フラッシュ開始までの待ち時間。`fall_tick_accum`は
    /// このtickぶんを差し引いた直後の値なので、`tick - accum`がそのまま「落下補間が1.0に
    /// 達するまでの残り実時間」になる。1フレームで複数tickを消化した場合は0になる。
    fn gravity_vanish_delay(&self) -> Duration {
        Duration::from_millis(self.effective_block_fall_tick_ms())
            .saturating_sub(self.fall_tick_accum)
    }

    /// 実効tickに応じた消滅フラッシュの長さ(ms)。基準tick(`FALL_TICK_MS`)で
    /// `BLOCK_VANISH_FLASH_MS`になる比例値で、短すぎて視認できなくならないよう
    /// `BLOCK_VANISH_FLASH_MIN_MS`を下限にする。
    pub(crate) fn vanish_flash_duration_ms(&self) -> u64 {
        let tick = self.effective_block_fall_tick_ms();
        (BLOCK_VANISH_FLASH_MS * tick / FALL_TICK_MS).max(BLOCK_VANISH_FLASH_MIN_MS)
    }

    /// 揺れ時間(`shake_duration_ms`)を実効tick単位へ換算した揺れtick数。揺れ時間が0なら
    /// 0、0より大きければ最低1tickは揺れる(整数除算で0になると予兆なしに落ち始めるため)。
    pub(crate) fn shake_ticks(&self) -> u8 {
        if self.shake_duration_ms == 0 {
            return 0;
        }
        let tick = self.effective_block_fall_tick_ms().max(1);
        (self.shake_duration_ms / tick).clamp(1, u8::MAX as u64) as u8
    }

    /// 描画側が使う、指定セルの消滅フラッシュ演出の進捗(0.0=フラッシュ開始直後、
    /// 1.0=演出完了直前)。フラッシュ中のセルのみ`Some`を返し、落下ブロックの到着待ち
    /// (`delay`>0)のセルは`None`を返す。
    pub fn vanish_flash_progress(&self, pos: board::Pos) -> Option<f32> {
        self.recently_vanished
            .iter()
            .find(|e| e.pos == pos && e.delay.is_zero())
            .map(|e| {
                let total = e.total.as_secs_f32().max(0.001);
                (1.0 - e.remaining.as_secs_f32() / total).clamp(0.0, 1.0)
            })
    }

    /// 描画側が使う、消滅は確定したがまだフラッシュに入っていない(落下ブロックの到着
    /// 待ちの)セルの、消滅直前の種類。着地と同一tickで4連結自動消滅した場合、盤面は既に
    /// Emptyのため描画側は表示すべきグリフを盤面から読めない。待機中はここから消滅直前の
    /// 種類を取得して、到着するまで元の見た目のまま描き続ける。
    pub fn pending_vanish_kind(&self, pos: board::Pos) -> Option<Cell> {
        self.recently_vanished
            .iter()
            .find(|e| e.pos == pos && !e.delay.is_zero())
            .map(|e| e.kind)
    }

    /// 指定セルの消滅演出が終わるまでの残り時間(ms、デバッグログ用)。フラッシュ開始
    /// 待ちも含めた合計を返す。対象でなければ`None`。
    fn recently_vanished_flash_remaining_ms(&self, pos: board::Pos) -> Option<u64> {
        self.recently_vanished
            .iter()
            .find(|e| e.pos == pos)
            .map(|e| (e.delay + e.remaining).as_millis() as u64)
    }

    /// `BOARD_SNAPSHOT_TICK_INTERVAL`ティックごとに、プレイヤー周辺の非Emptyセルをまとめて
    /// ログへ記録する。`block_events`は動いた/消えたセルしか記録しないため、これが無いと
    /// 「一度も動いていないセルが本当にEmptyか、生成時からの地形か」を後から見分けられない。
    /// デバッグログ無効時、または記録タイミングでなければ何もしない。
    fn log_board_snapshot_if_due(&self) {
        let Some(log) = &self.debug_log else {
            return;
        };
        if !self
            .frame_counter
            .is_multiple_of(BOARD_SNAPSHOT_TICK_INTERVAL)
        {
            return;
        }
        let row_start = self
            .player
            .row
            .saturating_sub(BOARD_SNAPSHOT_ROWS_ABOVE_PLAYER);
        let row_end = (self.player.row + BOARD_SNAPSHOT_ROWS_BELOW_PLAYER)
            .min(self.board.depth_rows().saturating_sub(1));
        let mut cells = Vec::new();
        for row in row_start..=row_end {
            for col in 0..self.board.width() {
                let cell = self.board.cell(row, col);
                if cell != Cell::Empty {
                    cells.push(((row, col), format!("{cell:?}")));
                }
            }
        }
        log.log_board_snapshot(self.frame_counter, &cells);
    }

    /// 落下ブロック補間描画(`draw_falling_blocks`)が、着地先セルが盤面上で既にEmptyに
    /// なっている場面に遭遇したことを記録する。デバッグログ無効時は何もしない。
    pub fn log_render_fallback(&self, to: board::Pos, from: board::Pos, resolved: Option<Cell>) {
        if let Some(log) = &self.debug_log {
            let resolved_kind = resolved.map(|k| format!("{k:?}"));
            log.log_render_fallback(
                self.frame_counter,
                to,
                from,
                resolved_kind.as_deref(),
                self.block_fall_progress(),
                self.effective_block_fall_tick_ms(),
                self.recently_vanished_flash_remaining_ms(to),
            );
        }
    }

    /// 描画側が使う、指定セルのボム爆発・炎演出の進捗(0.0=爆発直後、1.0=演出完了直前)と
    /// 爆心地からの距離。対象でなければ`None`を返す。
    pub fn explosion_flash_progress(&self, pos: board::Pos) -> Option<(f32, u8)> {
        let flash = Duration::from_millis(BOMB_EXPLOSION_FLASH_MS)
            .as_secs_f32()
            .max(0.001);
        self.recently_exploded
            .iter()
            .find(|&&(p, _, _)| p == pos)
            .map(|&(_, remaining, tier)| {
                (
                    (1.0 - remaining.as_secs_f32() / flash).clamp(0.0, 1.0),
                    tier,
                )
            })
    }

    /// 描画側が使う、ブロック落下ティックの進捗(0.0=直前のティック直後, 1.0=次のティック
    /// が来る直前)。`recently_moved_blocks`と組み合わせて移動前後を滑らかに補間する。
    pub fn block_fall_progress(&self) -> f32 {
        let tick_secs = self.effective_block_fall_tick_ms().max(1) as f32 / 1000.0;
        (self.fall_tick_accum.as_secs_f32() / tick_secs).clamp(0.0, 1.0)
    }

    /// 深度に応じて実効化したブロック落下tick間隔(ms)。設定画面/デバッグショートカットで
    /// 調整した`block_fall_tick_ms`を「深度0mでの速度」として扱い、
    /// `FALL_SPEED_DEPTH_MAX_SPEEDUP`まで短縮する(`DEBUG_FALL_TICK_MS_MIN`を下回らない)。
    /// オートプレイ(`autoplay.rs`)が「頭上のブロックが何ms後に落ちてくるか」の見積もりに
    /// 参照するため`pub(crate)`にしている。
    pub(crate) fn effective_block_fall_tick_ms(&self) -> u64 {
        let fraction = depth_fraction(self.player.depth_m());
        let speedup = 1.0 - fraction * (1.0 - FALL_SPEED_DEPTH_MAX_SPEEDUP);
        ((self.block_fall_tick_ms as f32 * speedup) as u64).max(DEBUG_FALL_TICK_MS_MIN)
    }

    /// 「天に召される」演出中かどうか。この間は移動・掘削入力を無視する。
    fn is_dying(&self) -> bool {
        self.ascending_remaining.is_some()
    }

    /// 「天に召される」演出中、または「わ〜!」スライダー演出中かどうか。
    /// この間は移動・掘削入力を無視する。
    fn is_input_frozen(&self) -> bool {
        self.is_dying() || self.dodge_stage != DodgeStage::None
    }

    /// 押し潰しの「潰れた」演出が表示中かどうか(GameOverオーバーレイの表示可否判定にも使う)。
    /// 最後のライフでのミスも同じ演出を経てからGameOverになるため、GameOverへ遷移した
    /// 時点ではこれは偽になっており、そのままオーバーレイを表示できる(#257)。
    pub fn crush_flash_active(&self) -> bool {
        self.crush_flash_remaining > Duration::ZERO || self.ascending_remaining.is_some()
    }

    /// チェックポイント(100mごと)到達演出が表示中なら、到達した深度(m)を返す。
    /// 描画側(render.rs)がバナー表示に使う。
    pub fn checkpoint_flash_depth_m(&self) -> Option<usize> {
        if self.checkpoint_flash_remaining > Duration::ZERO {
            Some(self.checkpoint_flash_depth_m)
        } else {
            None
        }
    }

    /// 掘削アニメーション中の描画フレーム(9章)。掘削演出中でなければ`None`、演出中は
    /// `DRILL_ANIM_FRAME_MS`ごとに`true`/`false`を切り替えて返す(描画側が2フレームを
    /// 交互に選ぶ)。
    pub fn drilling_frame(&self) -> Option<bool> {
        if self.drill_flash_remaining <= Duration::ZERO {
            return None;
        }
        let elapsed_ms =
            DRILL_ANIM_MS.saturating_sub(self.drill_flash_remaining.as_millis() as u64);
        Some((elapsed_ms / DRILL_ANIM_FRAME_MS.max(1)).is_multiple_of(2))
    }

    /// 「わ〜!」スライダー演出中(横滑り段階のみ)かどうか。描画側がスプライトを横滑り
    /// させる判断に使う。硬直(Recovering)段階では滑りが止まっているため`false`を返すが、
    /// `is_input_frozen`相当のフリーズ自体はそちらも継続する。
    pub fn is_dodge_sliding(&self) -> bool {
        self.dodge_stage == DodgeStage::Sliding
    }

    /// 「わ〜!」スライダー演出の横滑り進捗(0.0=開始直後、1.0=スライダー完了直前)。
    /// スライダー中でなければ0.0を返す。
    pub fn dodge_slide_progress(&self) -> f32 {
        if self.dodge_stage != DodgeStage::Sliding {
            return 0.0;
        }
        let total = DODGE_SLIDE_MS as f32 / 1000.0;
        if total <= 0.0 {
            return 1.0;
        }
        (1.0 - self.dodge_stage_remaining.as_secs_f32() / total).clamp(0.0, 1.0)
    }

    /// 「天に召される」演出の進捗(0.0=演出開始直後、1.0=演出完了直前)。演出中でなければ
    /// 0.0を返す。描画側(render.rs)がキャラのスプライトを少しずつ上へドリフトさせる。
    pub fn ascend_progress(&self) -> f32 {
        let Some(remaining) = self.ascending_remaining else {
            return 0.0;
        };
        let total = CRUSH_ASCEND_MS as f32 / 1000.0;
        if total <= 0.0 {
            return 1.0;
        }
        (1.0 - remaining.as_secs_f32() / total).clamp(0.0, 1.0)
    }

    /// 指定セルが現在「震えている」(支えを失い、落下開始までの猶予期間中)かどうか
    /// (描画用)。
    pub fn is_cell_shaking(&self, row: usize, col: usize) -> bool {
        self.gravity_state.is_shaking((row, col))
    }

    // -----------------------------------------------------------------------
    // デバッグショートカット(動作確認用で、初代の仕様やスコアには対応しない)
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

    /// 現在の横移動クールダウン間隔(ms)。オートプレイ(`autoplay.rs`)が「横へ1マス逃げる
    /// のに何msかかるか」を頭上の落下ブロックの到達時間と比べるために参照する。
    pub fn move_cooldown_ms(&self) -> u64 {
        self.move_cooldown_ms
    }

    /// 横移動のクールダウン間隔を直接指定する(起動時、Settingsから読み込んだ値を適用する
    /// 用途)。範囲外の値は`MOVE_COOLDOWN_MS_MIN`〜`MAX`にクランプする。
    pub fn set_move_cooldown_ms(&mut self, ms: u64) {
        self.move_cooldown_ms = ms.clamp(MOVE_COOLDOWN_MS_MIN, MOVE_COOLDOWN_MS_MAX);
    }

    /// 自動消滅の連鎖インターバルを直接指定する(起動時、Settingsから読み込んだ値を適用
    /// する用途)。範囲外の値は`CHAIN_VANISH_INTERVAL_MS_MIN`〜`MAX`にクランプする。
    pub fn set_chain_vanish_interval_ms(&mut self, ms: u64) {
        self.chain_vanish_interval_ms =
            ms.clamp(CHAIN_VANISH_INTERVAL_MS_MIN, CHAIN_VANISH_INTERVAL_MS_MAX);
    }

    /// 現在の揺れ時間(ms)。設定の永続化(main.rs/Settings)用に公開する。
    pub fn shake_duration_ms(&self) -> u64 {
        self.shake_duration_ms
    }

    /// 揺れ時間を直接指定する(起動時、Settingsから読み込んだ値を適用する用途)。
    pub fn set_shake_duration_ms(&mut self, ms: u64) {
        self.shake_duration_ms = ms.clamp(DEBUG_SHAKE_DURATION_MS_MIN, DEBUG_SHAKE_DURATION_MS_MAX);
    }

    /// 硬直インターバルを直接指定する(起動時、Settingsから読み込んだ値を適用する用途)。
    pub fn set_dodge_recovery_ms(&mut self, ms: u64) {
        self.dodge_recovery_ms = ms.clamp(DODGE_RECOVERY_MS_MIN, DODGE_RECOVERY_MS_MAX);
    }

    /// 永続化された設定(速度系・出現率系)を、開始したばかりのゲームへまとめて反映する。
    /// main.rsの`start_new_game`とオートプレイのソークテストの両方がここを通ることで、
    /// ソークテストが出現率の再抽選前の盤面(=実機と違う盤面)を測ってしまうのを防ぐ。
    ///
    /// デバッグログ(SQLite)の作り直しはここには含めない。設定の反映ではなく記録先の
    /// 準備であり、テストから呼ぶとファイルを作ってしまうため`start_new_game`に残す。
    pub fn apply_settings(&mut self, settings: &crate::settings::Settings) {
        self.set_block_fall_tick_ms(settings.block_fall_tick_ms);
        self.set_player_fall_tick_ms(settings.player_fall_tick_ms);
        self.set_shake_duration_ms(settings.shake_duration_ms);
        self.set_dodge_recovery_ms(settings.dodge_recovery_ms);
        self.set_move_cooldown_ms(settings.move_cooldown_ms);
        self.set_bomb_spawn_rate_percent(settings.bomb_spawn_rate_percent);
        self.set_bomb_fuse_ms(settings.bomb_fuse_ms);
        self.set_chain_vanish_interval_ms(settings.chain_vanish_interval_ms);
        self.set_rewind_stock_max(settings.rewind_stock_max);
        self.set_attack_blocks_per_rock(settings.attack_blocks_per_rock);
        self.set_attack_rocks_per_wave_max(settings.attack_rocks_per_wave_max);
        // Xブロック/AIR/スター/ダイヤの配分率設定を、安全地帯明け(行2)以降の全体へ反映する。
        self.reroll_spawn_rates_from(
            2,
            settings.rock_spawn_rate_percent,
            settings.air_spawn_rate_percent,
            settings.star_spawn_rate_percent,
            settings.diamond_spawn_rate_percent,
            settings.item_clear_above_rate_percent,
            settings.item_unify_colors_rate_percent,
            settings.item_starify_screen_rate_percent,
            settings.color_count,
            settings.color_cluster_rate_percent,
        );
    }

    /// `from_row`以降の岩(X)/AIR/スター/ダイヤブロック出現率を、指定の配分率(%、
    /// 100=通常のまま)で再抽選する。新規ゲーム開始直後は`from_row`に安全地帯明けの行を
    /// 渡せば盤面全体に反映され、プレイ中に呼ぶ場合は呼び出し側が
    /// `player.row + SPAWN_RATE_REROLL_SAFE_MARGIN_ROWS`のような画面外の行を渡すことで、
    /// 既に見えている地形を変えてしまわないようにする。
    ///
    /// `color_cluster_rate_percent`(%、100=通常のまま)は色ブロックの結合しやすさを調整する。
    ///
    /// `item_*_rate_percent`(%、100=通常のまま)はアイテムブロック3種の出現率を個別に調整
    /// する。アイテムだけは一括反映せず窓単位で補充する(一括だと配分率を上げた際に浅い
    /// 深度で生涯上限を使い切る)ため、ここでは設定値を保持するだけで抽選は
    /// `top_up_items_ahead`が行う。
    #[allow(clippy::too_many_arguments)]
    pub fn reroll_spawn_rates_from(
        &mut self,
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
    ) {
        self.item_clear_above_rate_percent = item_clear_above_rate_percent;
        self.item_unify_colors_rate_percent = item_unify_colors_rate_percent;
        self.item_starify_screen_rate_percent = item_starify_screen_rate_percent;
        self.board.reroll_overlays_from_row(
            &mut self.rng,
            from_row,
            rock_rate_percent,
            air_rate_percent,
            star_rate_percent,
            diamond_rate_percent,
            0,
            0,
            0,
            color_count,
            color_cluster_rate_percent,
            &self.gravity_state,
        );
        self.top_up_items_ahead();
    }

    /// アイテムブロック3種を、プレイヤーの現在行から`ITEM_WINDOW_AHEAD_ROWS`ぶん先までの
    /// 窓の範囲内で常に`ITEM_MAX_COUNT_ON_BOARD`個になるよう補充する。新しく窓へ入った行
    /// (`item_top_up_frontier_row`より先)だけを対象にするため、抽選済みの行は変えない。
    /// プレイヤーが進むと窓の下限も進み、既存アイテムが窓の外へ抜けたぶんだけ補充余地が
    /// 生まれる。`reroll_spawn_rates_from`と`check_level_and_clear`の両方から呼ぶ。
    fn top_up_items_ahead(&mut self) {
        let target_row = (self.player.row + crate::constants::ITEM_WINDOW_AHEAD_ROWS)
            .min(self.board.depth_rows());
        if target_row <= self.item_top_up_frontier_row {
            return;
        }
        self.board.top_up_items(
            &mut self.rng,
            self.player.row,
            self.item_top_up_frontier_row,
            target_row,
            self.item_clear_above_rate_percent,
            self.item_unify_colors_rate_percent,
            self.item_starify_screen_rate_percent,
        );
        self.item_top_up_frontier_row = target_row;
    }

    /// デバッグ: 揺れ時間(ブロックが支えを失ってから実際に落下し始めるまでの時間)を
    /// `DEBUG_SHAKE_DURATION_STEP_MS`ぶん増減する。`longer`がtrueなら長く(遅く反応)、
    /// falseなら短く(速く反応、0まで)する。
    pub fn debug_adjust_shake_duration(&mut self, longer: bool) {
        self.shake_duration_ms = if longer {
            (self.shake_duration_ms + DEBUG_SHAKE_DURATION_STEP_MS).min(DEBUG_SHAKE_DURATION_MS_MAX)
        } else {
            self.shake_duration_ms
                .saturating_sub(DEBUG_SHAKE_DURATION_STEP_MS)
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
    /// 「天に召される」演出中(`is_dying`)も無効にする。演出の完了時にライフを減らして
    /// 復活/GameOverを分岐するため、途中でライフを増やすと本来GameOverになる場面が
    /// 復活側へ倒れてしまうため(#257)。
    pub fn debug_add_life(&mut self) {
        if self.status == GameStatus::Playing && !self.is_dying() {
            self.player.lives = (self.player.lives + 1).min(LIVES_MAX);
        }
    }

    /// デバッグ: 酸素(AIR)を100%まで回復する。Playing中のみ有効。
    pub fn debug_fill_air(&mut self) {
        if self.status == GameStatus::Playing {
            self.player.oxygen = crate::constants::OXYGEN_MAX;
        }
    }

    /// デバッグ: プレイヤーより浅い(画面上で上にある)行を全てEmptyにする。Playing中のみ
    /// 有効。AIR(酸素カプセル)・アイテムブロックは消滅させず残す。死亡時の頭上クリアと
    /// 100mごとのチェックポイント到達時も、この同じ関数を呼び出して統一する。
    pub fn debug_clear_above_player(&mut self) {
        if self.status != GameStatus::Playing {
            return;
        }
        // 4連結自動消滅と同じ消滅フラッシュ演出を出す。AIR・アイテムブロック(C/R/K)は
        // 消さずに残す。
        //
        // 画面外(`entry_row`より浅い)に残っていたAIR/アイテムは、画面のすぐ外側
        // (`just_off_screen_row`)を起点にさらに浅い側へ積み上げ、以後の重力ティックで
        // 自然に画面内へ落ちてくるようにする(画面内へ直接置くといきなり現れて見える)。
        // 既に画面内にあった分はそのまま動かさない。
        let width = self.board.width();
        let entry_row = self.player.row.saturating_sub(PLAYER_SCREEN_ROWS_ABOVE);
        let just_off_screen_row = entry_row.saturating_sub(1);
        let mut off_screen_by_col: Vec<Vec<Cell>> = vec![Vec::new(); width];
        let mut cleared = Vec::new();
        for row in 0..self.player.row {
            for (col, bucket) in off_screen_by_col.iter_mut().enumerate() {
                let cell = self.board.cell(row, col);
                if matches!(cell, Cell::Oxygen | Cell::Item(_)) {
                    if row < entry_row {
                        bucket.push(cell);
                        self.board.set(row, col, Cell::Empty);
                    }
                } else if cell != Cell::Empty {
                    cleared.push(((row, col), cell));
                    self.board.set(row, col, Cell::Empty);
                }
            }
        }
        self.note_vanished_cells(cleared, Duration::ZERO);

        // 列ごとに、画面のすぐ外側(just_off_screen_row)を起点に、元の深さ順(浅い方が先)
        // を保ったまま浅い側(まだ画面に入らない側)へ空いているマスを探して詰め直す。
        for (col, cells) in off_screen_by_col.into_iter().enumerate() {
            let mut row = just_off_screen_row;
            for cell in cells {
                while self.board.cell(row, col) != Cell::Empty && row > 0 {
                    row -= 1;
                }
                if self.board.cell(row, col) != Cell::Empty {
                    break; // 置き場所が無ければそれ以上は諦める(起こりにくい極端なケース)
                }
                self.board.set(row, col, cell);
                if row == 0 {
                    break;
                }
                row -= 1;
            }
        }
    }

    /// チェックポイント(100mごと)の地面(`CHECKPOINT_SAFE_ZONE_M`)を実際に掘り抜いた時点で、
    /// その直後にスキマ(`CHECKPOINT_ZONE_GAP_M`)を空ける。地面部分そのものは強制的に
    /// くり抜かず、プレイヤーがドリルで掘り進む対象にする。`BONUS_FLOOR_DEPTH_M`(500m)
    /// だけは例外で、地面部分自体をC/K/Rアイテム・AIRが豊富なボーナスフロアとして生成する。
    ///
    /// スキマは、このチェックポイントの地面を掘り抜いた瞬間にだけくり抜く。まだ到達して
    /// いない深い場所を先回りしてくり抜くと広範囲崩落を招くため、常に1つ分だけを対象にする。
    fn apply_checkpoint_safe_zone(&mut self, at_m: usize) {
        let depth_rows = self.board.depth_rows();
        let zone_start_row = at_m;
        let zone_end_row = (zone_start_row + CHECKPOINT_SAFE_ZONE_M).min(depth_rows);
        // 地面(zone_start_row-zone_end_row)の直後、通常の地形が再開するまでのスキマ。
        let gap_end_row = (zone_end_row + CHECKPOINT_ZONE_GAP_M).min(depth_rows);
        if at_m == BONUS_FLOOR_DEPTH_M {
            self.board.reroll_overlays_in_row_range(
                &mut self.rng,
                zone_start_row,
                zone_end_row,
                100,
                BONUS_FLOOR_ITEM_AIR_RATE_PERCENT,
                100,
                100,
                BONUS_FLOOR_ITEM_AIR_RATE_PERCENT,
                BONUS_FLOOR_ITEM_AIR_RATE_PERCENT,
                BONUS_FLOOR_ITEM_AIR_RATE_PERCENT,
                ColorKind::ALL.len() as u8,
                100,
                &self.gravity_state,
            );
        }
        for row in zone_end_row..gap_end_row {
            for col in 0..self.board.width() {
                self.board.set(row, col, Cell::Empty);
            }
        }
    }

    /// チェックポイントのスキマ区間に、上から崩れてきたブロックやアイテムが滞留しない
    /// ようにする。既に到達済みのチェックポイントについてのみ、スキマ区間に何か入り込んで
    /// いれば消滅フラッシュ演出付きでパージする(地面部分はプレイヤーが掘り進む対象なので
    /// 対象外)。500mのボーナスフロアはアイテム/AIRを意図的に配置する区間のため対象外。
    fn purge_checkpoint_zone_debris(&mut self, vanish_delay: Duration) {
        let width = self.board.width();
        let depth_rows = self.board.depth_rows();
        let mut cleared = Vec::new();
        for checkpoint in 1..=self.last_checkpoint_reported {
            let at_m = checkpoint * CHECKPOINT_STEP_M;
            if at_m == BONUS_FLOOR_DEPTH_M {
                continue;
            }
            let zone_start_row = at_m + CHECKPOINT_SAFE_ZONE_M;
            let zone_end_row = (zone_start_row + CHECKPOINT_ZONE_GAP_M).min(depth_rows);
            for row in zone_start_row..zone_end_row {
                for col in 0..width {
                    let cell = self.board.cell(row, col);
                    if cell != Cell::Empty {
                        cleared.push(((row, col), cell));
                        self.board.set(row, col, Cell::Empty);
                    }
                }
            }
        }
        if !cleared.is_empty() {
            // 重力tickから呼ばれるため、ちょうどこのtickでスキマへ落ちてきたブロックも
            // 対象になりうる。落下補間の到着を待ってからフラッシュする。
            self.note_vanished_cells(cleared, vanish_delay);
        }
    }

    /// デバッグ: プレイヤー付近(上下`DEBUG_UNIFY_COLORS_RANGE_ROWS`行)の色ブロックを
    /// ランダムに選んだ2色だけへ揃える。Playing中のみ有効。
    ///
    /// 抽選にはOS乱数ではなくゲーム開始時のシードから作った`self.rng`を使う。同じシード・
    /// 同じ入力列なら盤面が完全に再現されるようにするため。
    pub fn debug_unify_nearby_colors(&mut self) -> Vec<GameEvent> {
        if self.status != GameStatus::Playing {
            return Vec::new();
        }
        let rng = &mut self.rng;

        let all = ColorKind::ALL;
        let first = all[rng.random_range(0..all.len())];
        let second_offset = 1 + rng.random_range(0..all.len() - 1);
        let second =
            all[(all.iter().position(|&c| c == first).unwrap() + second_offset) % all.len()];

        let start_row = self
            .player
            .row
            .saturating_sub(DEBUG_UNIFY_COLORS_RANGE_ROWS);
        let end_row = (self.player.row + DEBUG_UNIFY_COLORS_RANGE_ROWS)
            .min(self.board.depth_rows().saturating_sub(1));
        for row in start_row..=end_row {
            for col in 0..self.board.width() {
                if matches!(self.board.cell(row, col), Cell::Color(_)) {
                    let chosen = if rng.random_bool(0.5) { first } else { second };
                    self.board.set(row, col, Cell::Color(chosen));
                }
            }
        }

        // 重力ティックの外から色配置を直接書き換えたため、塊(連結グループ)の境界が
        // 変わっている。まだ揺れ猶予中(落下し始めていない)の古い揺れ状態は引きずらず、
        // 次の重力ティックで結合関係を一から作り直させる。ただし既に揺れが明けて連続
        // 落下中の塊まで揺れ直させると、押した瞬間にフリーズしたように見えるため対象外。
        //
        // 塗り替えで新たに4連結以上になった箇所はここでは消さない。消滅判定は通常の
        // 重力ティック(支えを失って落下・着地した場合のみ)に委ねる。
        self.gravity_state.reset_shake_progress(self.shake_ticks());

        Vec::new()
    }

    /// デバッグ: 画面内(プレイヤー位置から上下`STAR_VISIBLE_RANGE_ROWS`行)にあるXブロック・
    /// ダイヤブロックを、揺れ中のセルを除き全てスターブロックへ変える。Playing中のみ有効。
    pub fn debug_starify_visible_screen(&mut self) {
        if self.status != GameStatus::Playing {
            return;
        }
        let start_row = self
            .player
            .row
            .saturating_sub(crate::constants::STAR_VISIBLE_RANGE_ROWS);
        let end_row = (self.player.row + crate::constants::STAR_VISIBLE_RANGE_ROWS)
            .min(self.board.depth_rows().saturating_sub(1));
        for row in start_row..=end_row {
            for col in 0..self.board.width() {
                if matches!(self.board.cell(row, col), Cell::Rock { .. } | Cell::Diamond)
                    && !self.gravity_state.is_shaking((row, col))
                {
                    self.board.set(row, col, Cell::Star { visible_ms: 0 });
                }
            }
        }
    }

    /// プレイヤーのマスにブロックが重なってしまっていたら、空いているマスが見つかるまで
    /// 1マスずつ上へ押し上げる。見つからなければ(起こりにくい極端なケース)そのブロックは
    /// 諦めて消す。
    fn resolve_block_player_overlap(&mut self) {
        let (row, col) = self.player.position();
        let cell = self.board.cell(row, col);
        if cell == Cell::Empty {
            return;
        }
        let target = (0..row)
            .rev()
            .find(|&r| self.board.cell(r, col) == Cell::Empty);
        self.board.set(row, col, Cell::Empty);
        if let Some(r) = target {
            self.board.set(r, col, cell);
        }
    }

    /// 死亡時(押し潰し/酸素切れ)の盤面への影響をまとめて処理する。
    /// - 頭上のクリアはRアイテムと同じ処理(`debug_clear_above_player`)を使う。画面外に
    ///   残っていたAIR/アイテムも同じ処理内で画面内へ詰め直される。
    /// - 画面内外を問わず、盤面上の全てのボムをこの場で即座に爆発させる。
    /// - 上記の結果、プレイヤーのマスにブロックが重なってしまっていたら押し上げる。
    fn resolve_death_board_effects(&mut self, events: &mut Vec<GameEvent>) {
        self.debug_clear_above_player();
        self.detonate_all_bombs_immediately(events);
        self.resolve_block_player_overlap();
    }
}

/// `ms`を`step`ぶん増減させ、`DEBUG_FALL_TICK_MS_MIN`〜`MAX`にクランプする
/// (`faster`がtrueならtick間隔を短く=速く、falseなら長く=遅くする)。
fn adjust_fall_tick_ms(ms: u64, faster: bool) -> u64 {
    if faster {
        ms.saturating_sub(DEBUG_FALL_TICK_STEP_MS)
            .max(DEBUG_FALL_TICK_MS_MIN)
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
    use crate::constants::FIELD_WIDTH_DEFAULT as FIELD_WIDTH;
    use crate::constants::{FRAME_INTERVAL_MS, ROCK_HITS_TO_BREAK, SHAKE_TICKS};
    use board::{Cell, ColorKind};

    /// テスト用ヘルパー: 盤面全体を`Cell::Empty`にクリアする。`Game::new`の盤面はランダム
    /// 生成なので、テストが制御していない場所に未支持のグループが残っていると、支えの連鎖
    /// 判定で予期しない自動消滅・スコア加算が起きる。重力・自動消滅系のテストは必ずこれで
    /// クリアしてから対象セルだけを配置すること。`attack`モジュールのテスト(#262)からも
    /// 参照するため`pub(super)`にする。
    pub(super) fn clear_board(game: &mut Game) {
        for row in game.board.rows.iter_mut() {
            for cell in row.iter_mut() {
                *cell = Cell::Empty;
            }
        }
    }

    #[test]
    fn refresh_debug_log_disables_recording_when_the_setting_is_off() {
        // enabled=falseなら記録先を開かない(debug_logがNoneのまま)ことを確認する。
        // enabled=true側は実パスへのディスクI/Oに依存し環境依存になり得るため、ここでは
        // 確認しない(open_fresh自体はdebug_log.rs側で別途テスト済み)。
        let mut game = Game::new(1);
        game.refresh_debug_log(false);
        assert!(game.debug_log.is_none(), "無効化時はログを記録しないはず");
    }

    // --- フレーム巻き戻し ---------------------------------------------------

    /// 巻き戻しの決定性テスト用: 移動・掘削・時間経過を決まった順序で1ステップ進める。
    /// 同じ手順を同じ状態から踏めば、必ず同じ結果にならなければならない。
    fn advance_one_step(game: &mut Game, step: usize) {
        match step % 3 {
            0 => {
                game.face_down();
                game.try_drill();
            }
            1 => {
                game.try_move_right();
            }
            _ => {
                game.try_move_left();
            }
        }
        game.update(Duration::from_millis(FALL_TICK_MS));
    }

    /// 盤面・プレイヤー・ボム・乱数まで含めて2つのゲームが同じ状態かを確認する。
    /// `Game`自体は`PartialEq`を持たないため、巻き戻しの検証に必要な範囲を列挙して比べる。
    fn assert_same_state(actual: &mut Game, expected: &mut Game, label: &str) {
        assert_eq!(
            actual.board.depth_rows(),
            expected.board.depth_rows(),
            "{label}: 盤面の行数"
        );
        assert_eq!(
            actual.board.width(),
            expected.board.width(),
            "{label}: 盤面の列数"
        );
        for row in 0..expected.board.depth_rows() {
            for col in 0..expected.board.width() {
                assert_eq!(
                    actual.board.cell(row, col),
                    expected.board.cell(row, col),
                    "{label}: セル({row},{col})"
                );
            }
        }
        assert_eq!(
            actual.player.position(),
            expected.player.position(),
            "{label}: プレイヤー位置"
        );
        assert_eq!(
            actual.player.facing, expected.player.facing,
            "{label}: 向き"
        );
        assert_eq!(
            actual.player.score, expected.player.score,
            "{label}: スコア"
        );
        assert_eq!(
            actual.player.lives, expected.player.lives,
            "{label}: ライフ"
        );
        assert_eq!(
            actual.player.oxygen, expected.player.oxygen,
            "{label}: 酸素"
        );
        assert_eq!(
            actual.player.oxygen_capsules_collected, expected.player.oxygen_capsules_collected,
            "{label}: 取得カプセル数"
        );
        assert_eq!(
            actual.player.elapsed_seconds, expected.player.elapsed_seconds,
            "{label}: 経過時間"
        );
        assert_eq!(actual.bombs, expected.bombs, "{label}: ボム");
        assert_eq!(actual.status, expected.status, "{label}: ステータス");
        // 乱数は「次に出る値」が一致していれば同じ位置にあるとみなせる。
        assert_eq!(
            actual.rng.random::<u64>(),
            expected.rng.random::<u64>(),
            "{label}: 乱数の進み具合"
        );
    }

    #[test]
    fn restore_for_rewind_reproduces_the_exact_same_future_from_the_snapshot() {
        // 巻き戻しの核心。スナップショット→進める→戻す→同じ操作で進める、が
        // 「スナップショットをそのまま同じだけ進めた結果」と一致すること(決定性の往復)。
        let mut game = Game::new(7);
        // ボムの出現・爆発も判断に混ざるよう、出現頻度を上げておく。
        game.set_bomb_spawn_rate_percent(2000);
        for step in 0..30 {
            advance_one_step(&mut game, step);
        }

        let snapshot = game.clone();
        // スナップショットから独立に進めた「本来の未来」。
        let mut expected = snapshot.clone();
        for step in 0..40 {
            advance_one_step(&mut expected, step);
        }

        // 本体は別の手順で荒らしてから巻き戻す。
        for step in 0..25 {
            advance_one_step(&mut game, step + 1);
        }
        game.restore_for_rewind(&snapshot);
        for step in 0..40 {
            advance_one_step(&mut game, step);
        }

        assert_same_state(&mut game, &mut expected, "巻き戻し後の再現");
    }

    #[test]
    fn restore_for_rewind_carries_over_the_values_that_must_not_be_rewound() {
        let mut game = Game::new(3);
        let snapshot = game.clone();

        for step in 0..10 {
            advance_one_step(&mut game, step);
        }
        game.set_invincible(true);
        game.misses_averted = 5;
        let frame_before = game.frame_counter;
        assert!(frame_before > 0, "前提: フレームが進んでいること");

        game.restore_for_rewind(&snapshot);

        assert_eq!(
            game.frame_counter, frame_before,
            "フレーム番号は巻き戻さない(ログの通し番号を単調増加に保つ)"
        );
        assert!(game.is_invincible(), "無敵は現在値のまま");
        assert_eq!(game.misses_averted(), 5, "回避したミス数は現在値のまま");
        assert_eq!(
            game.rewind_stock(),
            REWIND_STOCK_INITIAL - 1,
            "ストックを1消費するはず"
        );
    }

    #[test]
    fn restore_for_rewind_keeps_pointing_at_the_same_debug_log() {
        // debug_logはRcで共有しており、巻き戻してもログの記録先は差し替わらない。
        let mut game = Game::new(4);
        // 実ユーザーディレクトリを触らないよう、テスト用の一時DBを直接差す。
        let dir = std::env::temp_dir().join(format!(
            "misterdrillerterm-rewind-log-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let log = Rc::new(
            DebugLog::open_fresh_at_for_test(&dir.join("debug_log.db"))
                .expect("一時ディレクトリで開けるはず"),
        );
        game.debug_log = Some(Rc::clone(&log));
        let snapshot = game.clone();

        game.restore_for_rewind(&snapshot);

        let restored = game.debug_log.as_ref().expect("ログが外れていないはず");
        assert!(
            Rc::ptr_eq(restored, &log),
            "巻き戻し後も同じログ接続を指しているはず"
        );
        assert!(
            Rc::ptr_eq(
                snapshot.debug_log.as_ref().unwrap(),
                game.debug_log.as_ref().unwrap()
            ),
            "スナップショット(Gameの複製)も同じログ接続を共有しているはず"
        );

        drop(game);
        drop(snapshot);
        drop(log);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rewind_stock_starts_at_the_initial_value_and_never_exceeds_the_maximum() {
        let game = Game::new(1);
        assert_eq!(game.rewind_stock(), REWIND_STOCK_INITIAL);
        assert_eq!(game.rewind_stock_max(), REWIND_STOCK_MAX_DEFAULT);
    }

    #[test]
    fn set_rewind_stock_max_clamps_the_current_stock_down() {
        let mut game = Game::new(1);
        game.set_rewind_stock_max(1);
        assert_eq!(game.rewind_stock(), 1, "上限を下げたら現在値も切り下げる");

        game.set_rewind_stock_max(0);
        assert_eq!(game.rewind_stock(), 0);
        assert!(!game.can_start_rewind(), "上限0(OFF)なら巻き戻せない");
    }

    #[test]
    fn reaching_a_100m_checkpoint_refills_one_rewind_stock_up_to_the_maximum() {
        let mut game = Game::new(90);
        game.set_rewind_stock_max(3);
        game.rewind_stock = 1;
        game.player.row =
            crate::constants::CHECKPOINT_STEP_M + crate::constants::CHECKPOINT_SAFE_ZONE_M - 1;
        game.player.facing = Direction::Down;
        game.board.rows[game.player.row + 1][game.player.col] = Cell::Empty;

        game.try_drill();
        let events = game.update(Duration::from_millis(FALL_TICK_MS));

        assert!(
            events.contains(&GameEvent::Checkpoint100m { at_m: 100 }),
            "前提: チェックポイントに到達していること"
        );
        assert_eq!(
            game.rewind_stock(),
            1 + REWIND_STOCK_PER_CHECKPOINT,
            "チェックポイントで補充されるはず"
        );
    }

    #[test]
    fn checkpoint_refill_does_not_exceed_the_configured_maximum() {
        let mut game = Game::new(90);
        game.set_rewind_stock_max(2);
        assert_eq!(game.rewind_stock(), 2, "前提: 既に上限まで持っている");
        game.player.row =
            crate::constants::CHECKPOINT_STEP_M + crate::constants::CHECKPOINT_SAFE_ZONE_M - 1;
        game.player.facing = Direction::Down;
        game.board.rows[game.player.row + 1][game.player.col] = Cell::Empty;

        game.try_drill();
        let events = game.update(Duration::from_millis(FALL_TICK_MS));

        assert!(events.contains(&GameEvent::Checkpoint100m { at_m: 100 }));
        assert_eq!(game.rewind_stock(), 2, "上限を超えては増えないはず");
    }

    #[test]
    fn revive_restores_the_rewind_stock_to_the_initial_value() {
        let mut game = Game::new(1);
        game.rewind_stock = 0;
        game.status = GameStatus::GameOver;

        game.revive();

        assert_eq!(game.rewind_stock(), REWIND_STOCK_INITIAL);
    }

    #[test]
    fn revive_does_not_restore_the_stock_beyond_the_configured_maximum() {
        let mut game = Game::new(1);
        game.set_rewind_stock_max(1);
        game.rewind_stock = 0;
        game.status = GameStatus::GameOver;

        game.revive();

        assert_eq!(game.rewind_stock(), 1);
    }

    #[test]
    fn can_start_rewind_is_true_only_while_playing_or_game_over_and_with_stock_left() {
        let mut game = Game::new(1);

        game.status = GameStatus::Playing;
        assert!(game.can_start_rewind(), "プレイ中は起動できる");

        // 昇天演出中(is_dying)もプレイ中扱いなので起動できる(押し潰された直後に
        // 押して戻る、が主要な使い方)。
        game.ascending_remaining = Some(Duration::from_millis(100));
        assert!(game.is_dying(), "前提: 昇天演出中");
        assert!(game.can_start_rewind(), "昇天演出中も起動できる");
        game.ascending_remaining = None;

        game.status = GameStatus::GameOver;
        assert!(game.can_start_rewind(), "GameOver中も起動できる");

        game.status = GameStatus::Paused;
        assert!(!game.can_start_rewind(), "一時停止中は起動できない");

        game.status = GameStatus::Cleared;
        assert!(!game.can_start_rewind(), "クリア後は起動できない");

        game.status = GameStatus::Playing;
        game.rewind_stock = 0;
        assert!(!game.can_start_rewind(), "ストックが無ければ起動できない");
    }

    #[test]
    fn is_rewind_capturable_excludes_dying_paused_game_over_and_cleared() {
        let mut game = Game::new(1);
        assert!(game.is_rewind_capturable(), "通常のプレイ中は記録してよい");

        game.ascending_remaining = Some(Duration::from_millis(100));
        assert!(
            !game.is_rewind_capturable(),
            "昇天演出中は記録しない(履歴の最新を「生きていた最後の瞬間」に保つ)"
        );
        game.ascending_remaining = None;

        for status in [
            GameStatus::Paused,
            GameStatus::GameOver,
            GameStatus::Cleared,
        ] {
            game.status = status;
            assert!(!game.is_rewind_capturable(), "{status:?}中は記録しない");
        }
    }

    #[test]
    fn is_rewind_capturable_allows_recording_during_the_dodge_slider() {
        // スライダー演出(わ〜!)はまだ生きている状態なので記録してよい。
        let mut game = Game::new(1);
        game.dodge_stage = DodgeStage::Sliding;
        assert!(game.is_rewind_capturable());
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
    fn a_custom_depth_goal_clears_the_game_there_instead_of_at_field_depth_m() {
        // コース長は`new_with_width`の`depth_goal_m`で選べるため、ノーマルコース既定の
        // 1000mより短いゴールでもそこで正しくクリアすることを確認する(テスト時間短縮の
        // ため、実際の500mではなく20mの短いコースで検証)。
        let depth_goal_m = 20;
        let mut game = Game::new_with_width(1, FIELD_WIDTH_DEFAULT, depth_goal_m);
        assert_eq!(
            game.board.depth_rows(),
            depth_goal_m,
            "盤面の行数も選んだゴール深度と一致するはず"
        );

        game.player.row = depth_goal_m - 2;
        game.player.facing = Direction::Down;
        let last_row = depth_goal_m - 1;
        game.board.rows[last_row][game.player.col] = Cell::Empty;

        game.try_drill();
        let events = game.update(Duration::from_millis(FALL_TICK_MS));

        assert_eq!(
            game.status,
            GameStatus::Cleared,
            "1000mではなく選んだゴール深度({depth_goal_m}m)でクリアするはず"
        );
        assert!(events.iter().any(|e| matches!(e, GameEvent::Cleared)));
    }

    #[test]
    fn drilling_an_item_does_nothing_like_air() {
        // アイテムはAIR同様「触れるだけで取得」なので、掘削では何も起きずその場に残る。
        let mut game = Game::new(74);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.player.facing = Direction::Down;
        game.board.rows[501][5] = Cell::Item(ItemEffect::ClearAbove);

        let events = game.try_drill();

        assert!(
            matches!(game.board.cell(501, 5), Cell::Item(ItemEffect::ClearAbove)),
            "掘削では取得されないはず"
        );
        assert!(events.is_empty());
    }

    #[test]
    fn touching_a_clear_above_item_clears_blocks_above_the_player_and_emits_event() {
        // Rアイテム(ショートカットRと同じ効果)は、掘らずに触れるだけで取得できる。
        let mut game = Game::new(74);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 足場(横移動には支持が必要)
        game.board.rows[500][6] = Cell::Item(ItemEffect::ClearAbove);
        game.board.rows[200][4] = Cell::Color(ColorKind::Red);

        let events = game.try_move_right();

        assert_eq!(
            game.player.col, 6,
            "AIRと同じく掘らずそのマスへ移動するはず"
        );
        assert!(matches!(game.board.cell(500, 6), Cell::Empty));
        assert!(
            matches!(game.board.cell(200, 4), Cell::Empty),
            "ショートカットRと同じく頭上のブロックが全クリアされるはず"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::ItemCollected(ItemEffect::ClearAbove)))
        );
    }

    #[test]
    fn touching_a_unify_colors_item_reduces_nearby_colors_to_two_and_emits_event() {
        // Cアイテム(ショートカットCと同じ効果)。
        let mut game = Game::new(74);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 足場
        game.board.rows[500][6] = Cell::Item(ItemEffect::UnifyColors);
        for (i, color) in [
            ColorKind::Red,
            ColorKind::Blue,
            ColorKind::Green,
            ColorKind::Yellow,
        ]
        .into_iter()
        .enumerate()
        {
            game.board.rows[499][i] = Cell::Color(color);
        }

        let events = game.try_move_right();

        let mut distinct_colors: Vec<ColorKind> = game.board.rows[499]
            .iter()
            .filter_map(|c| {
                if let Cell::Color(k) = c {
                    Some(*k)
                } else {
                    None
                }
            })
            .collect();
        distinct_colors.dedup();
        distinct_colors.sort_by_key(|k| ColorKind::ALL.iter().position(|c| c == k).unwrap());
        distinct_colors.dedup();
        assert!(
            distinct_colors.len() <= 2,
            "ショートカットCと同じく2色以内に統一されるはず: {distinct_colors:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::ItemCollected(ItemEffect::UnifyColors)))
        );
    }

    #[test]
    fn touching_a_starify_screen_item_converts_visible_rock_and_diamond_to_stars_and_emits_event() {
        // Kアイテム(ショートカットKと同じ効果)。
        let mut game = Game::new(74);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 足場
        game.board.rows[500][6] = Cell::Item(ItemEffect::StarifyScreen);
        game.board.rows[495][3] = Cell::Rock { hits: 1 };
        game.board.rows[498][4] = Cell::Diamond;

        let events = game.try_move_right();

        assert!(matches!(game.board.cell(495, 3), Cell::Star { .. }));
        assert!(matches!(game.board.cell(498, 4), Cell::Star { .. }));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::ItemCollected(ItemEffect::StarifyScreen)))
        );
    }

    #[test]
    fn item_survives_falling_together_with_a_diamond_above_it() {
        // アイテムの真上にダイヤがあり両方支えを失って一緒に落下しても、アイテムが
        // 消えずに着地することを確認する(過去に落下の過程で消えるバグがあった)。
        const FRAME_MS: u64 = 33;
        let mut game = Game::new(80);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 11;
        // ボムの自然発生を無効化する。この長時間シミュレーションの途中で自然発生した
        // ボムがプレイヤーに命中すると、その死亡処理が無関係な列のダイヤまで巻き込む。
        game.set_bomb_spawn_rate_percent(0);

        game.board.rows[500][0] = Cell::Diamond;
        game.board.rows[501][0] = Cell::Item(ItemEffect::ClearAbove);

        let total_ms_needed = (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 500 * FALL_TICK_MS;
        let mut elapsed_ms = 0u64;
        while elapsed_ms < total_ms_needed {
            // 酸素カプセルを置かないまま数分相当の時間を進めるため、何もしないと道中で
            // 酸素切れ→死亡→復活のサイクルが起き、死亡時の頭上クリア(盤面幅全体)が
            // 無関係な列のダイヤまで消してしまう。酸素は毎フレーム全回復させて防ぐ。
            game.player.oxygen = crate::constants::OXYGEN_MAX;
            game.update(Duration::from_millis(FRAME_MS));
            elapsed_ms += FRAME_MS;
        }

        let item_count = game
            .board
            .rows
            .iter()
            .flatten()
            .filter(|c| matches!(c, Cell::Item(_)))
            .count();
        assert_eq!(
            item_count, 1,
            "ダイヤと一緒に落下してもアイテムが消えないはず"
        );
        assert!(
            matches!(game.board.cell(999, 0), Cell::Item(ItemEffect::ClearAbove)),
            "アイテムは最深行まで落ちて残るはず"
        );
        assert!(
            matches!(game.board.cell(998, 0), Cell::Diamond),
            "ダイヤはアイテムのすぐ上に着地するはず"
        );
    }

    #[test]
    fn item_top_up_keeps_placing_new_items_far_ahead_as_the_player_advances() {
        // 「盤面全体で生涯10個」という旧仕様では、配分率300%で深度40〜50m台に上限を
        // 使い切り、残り900m以上まったく出現しなくなっていた。窓単位の補充に変更した
        // 後は、プレイヤーが深く進んでも常に前方の窓へアイテムが供給され続けるはず。
        let mut game = Game::new(1);
        game.reroll_spawn_rates_from(2, 100, 100, 100, 100, 300, 300, 300, 4, 100);

        let count_in_range = |game: &Game, effect: ItemEffect, from: usize, to: usize| {
            game.board.rows[from..to]
                .iter()
                .flatten()
                .filter(|c| matches!(c, Cell::Item(e) if *e == effect))
                .count()
        };

        assert!(
            count_in_range(&game, ItemEffect::UnifyColors, 0, 100) > 0,
            "ゲーム開始直後の窓にはCアイテムが存在するはず"
        );

        // プレイヤーを深度500mまで1行ずつ進め、その都度check_level_and_clearを呼ぶ
        // (通常プレイと同じ呼び出しパターン)。窓は毎回わずかに前進するだけなので、
        // 500行ぶんをまとめて抽選する場合とは結果が違う。
        for row in 1..=500 {
            game.player.row = row;
            game.check_level_and_clear(&mut Vec::new());
        }

        assert!(
            count_in_range(&game, ItemEffect::UnifyColors, 500, 600) > 0,
            "深度500m地点でも前方の窓にCアイテムが補充されているはず(旧仕様では\
             盤面全体の上限を序盤で使い切り、ここは0個になっていた)"
        );
        assert!(
            count_in_range(&game, ItemEffect::StarifyScreen, 500, 600) > 0,
            "深度500m地点でも前方の窓にKアイテムが補充されているはず"
        );
    }

    #[test]
    fn oxygen_running_out_during_update_costs_a_life_and_continues() {
        // 酸素切れも押し潰しと同じ処理を経るため、ライフ減算・酸素回復は
        // 「天に召される」演出完了まで遅延される。
        let mut game = Game::new(2);
        game.player.oxygen = 1.0;
        let lives_before = game.player.lives;

        let events = game.update(Duration::from_secs(1));

        assert_eq!(game.status, GameStatus::Playing);
        assert!(game.is_dying(), "酸素切れでも天に召される演出中のはず");
        assert_eq!(
            game.player.lives, lives_before,
            "演出完了までライフ減算は遅延されるはず"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::LifeLost { .. }))
        );

        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));

        assert_eq!(game.player.lives, lives_before - 1);
        assert_eq!(game.player.oxygen, crate::constants::OXYGEN_MAX);
    }

    #[test]
    fn oxygen_running_out_on_last_life_ends_the_game() {
        // 最後のライフでも「天に召される」演出を経てからGameOverになる(#257)。
        let mut game = Game::new_with_lives(2, 1);
        game.player.oxygen = 1.0;

        let events = game.update(Duration::from_secs(1));

        assert_eq!(game.status, GameStatus::Playing, "演出中はまだPlaying");
        assert!(game.is_dying(), "最後のライフでも演出を経由するはず");
        assert_eq!(game.player.lives, 1, "演出完了までライフ減算は遅延される");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::GameOverMiss { .. })),
            "ミス音は演出開始時に鳴らす: {events:?}"
        );

        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));

        assert_eq!(game.status, GameStatus::GameOver);
        assert_eq!(game.player.lives, 0);
    }

    #[test]
    fn oxygen_death_on_the_last_life_reaches_a_displayable_game_over_dialog() {
        // 演出完了でGameOverへ遷移するフレームで打ち切らないと、酸素0のままの
        // プレイヤーが同じフレーム内で再び酸素切れ判定に引っかかり、ミスが二重に
        // 走って演出が終わらなくなる(GameOverダイアログが永久に出ない)。
        let mut game = Game::new_with_lives(2, 1);
        game.player.oxygen = 1.0;

        let mut events = game.update(Duration::from_secs(1));
        events.extend(game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        )));

        assert_eq!(game.status, GameStatus::GameOver);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, GameEvent::GameOverMiss { .. }))
                .count(),
            1,
            "ミス音は1回だけのはず(二重ミスしていない): {events:?}"
        );
        assert!(!game.is_dying(), "演出は終わっているはず");
        assert!(
            !game.crush_flash_active(),
            "演出が終わっていればGameOverダイアログを表示できる"
        );

        // さらにフレームを進めても演出が復活しない(＝ダイアログが出続ける)こと。
        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));
        assert!(!game.crush_flash_active());
    }

    // --- 無敵(ミス無効) / オートプレイの土台 ---

    #[test]
    fn invincible_is_off_by_default_so_existing_behaviour_is_unchanged() {
        let game = Game::new(1);
        assert!(!game.is_invincible());
        assert_eq!(game.misses_averted(), 0);
    }

    #[test]
    fn invincible_turns_oxygen_death_into_an_averted_miss_and_refills_the_tank() {
        // 酸素0のまま放置すると毎フレーム再検出されてしまうため、回避時は満タンに
        // 戻して以後の減衰・警告を通常通り回す。
        let mut game = Game::new(2);
        game.set_invincible(true);
        let lives_before = game.player.lives;
        game.player.oxygen = 1.0;

        let events = game.update(Duration::from_secs(1));

        assert_eq!(game.status, GameStatus::Playing);
        assert!(!game.is_dying(), "天に召される演出は始まらないはず");
        assert_eq!(game.player.lives, lives_before, "ライフは減らないはず");
        assert_eq!(game.player.oxygen, crate::constants::OXYGEN_MAX);
        assert_eq!(game.misses_averted(), 1);
        assert_eq!(
            events
                .iter()
                .filter(|e| **e
                    == GameEvent::MissAverted {
                        cause: MissCause::OxygenOut
                    })
                .count(),
            1,
            "回避イベントはちょうど1回のはず: {events:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                GameEvent::LifeLost { .. } | GameEvent::GameOverMiss { .. } | GameEvent::Revived
            )),
            "ミス関連の既存イベントは一切発生しないはず: {events:?}"
        );
    }

    #[test]
    fn invincible_keeps_playing_even_on_the_last_life() {
        // ライフ1(次のミスでGameOver)でも、無敵中はGameOverにならない。
        let mut game = Game::new_with_lives(3, 1);
        game.set_invincible(true);
        game.player.oxygen = 1.0;

        game.update(Duration::from_secs(1));

        assert_eq!(game.status, GameStatus::Playing);
        assert_eq!(game.player.lives, 1);
    }

    #[test]
    fn invincible_clears_the_block_that_crushed_the_player_and_flashes_it() {
        // 押し潰したブロックはプレイヤーのマスに残る仕様(通常は復活処理が消す)。
        // 無敵では復活処理が走らないため、その場で消して消滅フラッシュを出す。
        let mut game = Game::new(34);
        game.set_invincible(true);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        let lives_before = game.player.lives;
        game.board.rows[998][5] = Cell::Color(ColorKind::Red); // プレイヤーの真上、支えなし

        let events = game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));

        assert_eq!(game.player.lives, lives_before);
        assert_eq!(game.status, GameStatus::Playing);
        assert!(events.contains(&GameEvent::MissAverted {
            cause: MissCause::CrushedByFallingBlock
        }));
        assert_eq!(
            game.board.cell(999, 5),
            Cell::Empty,
            "押し潰したブロックはその場で消えるはず"
        );
        assert!(
            game.vanish_flash_progress((999, 5)).is_some(),
            "消滅フラッシュが記録されているはず"
        );
    }

    #[test]
    fn invincible_averts_the_crush_from_drilling_into_a_falling_block() {
        let mut game = Game::new(36);
        game.set_invincible(true);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.player.facing = Direction::Up;
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 足場
        // 頭上のブロックは支えが無く、かつ揺れ状態の記録も無い(=まだupdateを
        // 一度も回していないので猶予期間ではない)。上向き掘削は押し潰しになる。
        game.board.rows[499][5] = Cell::Color(ColorKind::Red);
        let misses_before = game.misses_averted();

        let events = game.try_drill();

        assert!(
            events.contains(&GameEvent::MissAverted {
                cause: MissCause::DrilledIntoFallingBlock
            }),
            "掘削中落下として回避されるはず: {events:?}"
        );
        assert_eq!(game.misses_averted(), misses_before + 1);
        assert_eq!(game.status, GameStatus::Playing);
    }

    #[test]
    fn misses_averted_counts_every_averted_miss() {
        let mut game = Game::new(4);
        game.set_invincible(true);

        for expected in 1..=3 {
            game.player.oxygen = 1.0;
            game.update(Duration::from_secs(1));
            assert_eq!(game.misses_averted(), expected);
        }
    }

    #[test]
    fn turning_invincible_back_off_restores_the_normal_miss_handling() {
        let mut game = Game::new(5);
        game.set_invincible(true);
        game.player.oxygen = 1.0;
        game.update(Duration::from_secs(1));
        assert_eq!(game.player.lives, LIVES_DEFAULT);

        game.set_invincible(false);
        assert!(!game.is_invincible());
        game.player.oxygen = 1.0;
        let events = game.update(Duration::from_secs(1));

        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::LifeLost { .. })),
            "無敵を切れば通常どおりミスするはず: {events:?}"
        );
        assert!(game.is_dying());
    }

    // --- apply_input(手入力とオートプレイの共通入口) ---

    #[test]
    fn apply_input_routes_the_five_gameplay_actions_to_their_handlers() {
        // 横移動はクールダウンを1スロットしか持たないため、MoveLeft/MoveRightは
        // それぞれ新しいGameで確認する(同じGameで連続して出すと2回目が弾かれる)。
        fn grounded_game(seed: u64) -> Game {
            let mut game = Game::new(seed);
            clear_board(&mut game);
            game.player.row = 500;
            game.player.col = 5;
            for col in 4..=6 {
                game.board.rows[501][col] = Cell::Rock { hits: 0 }; // 足場
            }
            game
        }

        let mut game = grounded_game(40);
        game.apply_input(InputAction::FaceUp);
        assert_eq!(game.player.facing, Direction::Up);
        game.apply_input(InputAction::FaceDown);
        assert_eq!(game.player.facing, Direction::Down);

        let mut game = grounded_game(41);
        game.apply_input(InputAction::MoveLeft);
        assert_eq!(game.player.col, 4);
        assert_eq!(game.player.facing, Direction::Left);

        let mut game = grounded_game(42);
        game.apply_input(InputAction::MoveRight);
        assert_eq!(game.player.col, 6);
        assert_eq!(game.player.facing, Direction::Right);

        // Drillはfacing方向(Down)のブロックを掘る。
        let mut game = grounded_game(43);
        game.player.facing = Direction::Down;
        game.board.rows[501][5] = Cell::Color(ColorKind::Red);
        let events = game.apply_input(InputAction::Drill);
        assert!(
            events.contains(&GameEvent::DrillImpact),
            "掘削が実行されるはず: {events:?}"
        );
    }

    #[test]
    fn apply_input_ignores_actions_that_are_not_gameplay_operations() {
        // 一時停止・画面遷移・設定変更・デバッグトグルはmain.rsが解釈するもので、
        // apply_inputはGameの状態を一切変えない。
        let mut game = Game::new(41);
        let before = (
            game.player.position(),
            game.player.facing,
            game.status,
            game.player.lives,
            game.is_invincible(),
        );

        for action in [
            InputAction::TogglePause,
            InputAction::Quit,
            InputAction::Confirm,
            InputAction::ToggleMusic,
            InputAction::ToggleSe,
            InputAction::OpenSettings,
            InputAction::OpenHelp,
            InputAction::UnboundKey,
            InputAction::DebugToggleAutopilot,
            InputAction::DebugToggleInvincible,
            InputAction::DebugAddLife,
        ] {
            assert_eq!(
                game.apply_input(action),
                Vec::new(),
                "{action:?}はno-opのはず"
            );
        }

        assert_eq!(
            (
                game.player.position(),
                game.player.facing,
                game.status,
                game.player.lives,
                game.is_invincible()
            ),
            before
        );
    }

    // --- is_cell_unstable(オートプレイの安全判定) ---

    #[test]
    fn is_cell_unstable_is_false_for_cells_that_can_never_fall_on_you() {
        let mut game = Game::new(42);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.board.rows[400][1] = Cell::Oxygen;
        game.board.rows[400][2] = Cell::Item(ItemEffect::ClearAbove);

        assert!(!game.is_cell_unstable(400, 0), "Emptyは常にfalse");
        assert!(!game.is_cell_unstable(400, 1), "AIRは常にfalse");
        assert!(!game.is_cell_unstable(400, 2), "アイテムは常にfalse");
    }

    #[test]
    fn is_cell_unstable_is_true_for_an_unsupported_block_and_false_for_a_supported_one() {
        let mut game = Game::new(43);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        // 支えなし(直下がEmpty)
        game.board.rows[400][3] = Cell::Color(ColorKind::Red);
        // 支えあり(最深行に接している)
        game.board.rows[999][7] = Cell::Color(ColorKind::Blue);

        assert!(game.is_cell_unstable(400, 3), "未支持の塊は不安定");
        assert!(!game.is_cell_unstable(999, 7), "支えのある塊は安定");
    }

    #[test]
    fn is_cell_unstable_is_true_while_a_block_is_still_shaking() {
        // 揺れ中(これから落ちる予告状態)も「落ちてくる可能性がある」に含める。
        // 上向き掘削の可否判定(is_overhead_unstable)は揺れ中を除外するが、
        // オートプレイの回避判断は落ちる前に逃げる必要があるため含める。
        let mut game = Game::new(44);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[500][3] = Cell::Color(ColorKind::Red); // 支えなし

        game.update(Duration::from_millis(FALL_TICK_MS + 10)); // 1tickで揺れ始める
        assert!(
            game.gravity_state.is_shaking((500, 3)),
            "前提: このブロックは揺れ始めているはず"
        );
        assert!(game.is_cell_unstable(500, 3));
    }

    #[test]
    fn is_cell_unstable_is_false_outside_the_board() {
        let game = Game::new(45);
        assert!(!game.is_cell_unstable(game.board.depth_rows(), 0));
        assert!(!game.is_cell_unstable(0, game.board.width()));
    }

    // --- GameOverダイアログ ---

    #[test]
    fn game_over_selection_defaults_to_back_to_title_and_toggles() {
        let mut game = Game::new_with_lives(2, 1);
        game.player.oxygen = 1.0;
        game.update(Duration::from_secs(1));
        // 最後のライフでも「天に召される」演出を経てからGameOverになる(#257)。
        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));
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
        // 最後のライフでも「天に召される」演出を経てからGameOverになる(#257)。
        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));
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
    fn set_move_cooldown_ms_clamps_to_min_and_max() {
        let mut game = Game::new(4);
        game.set_move_cooldown_ms(0);
        assert_eq!(
            game.move_cooldown_ms,
            crate::constants::MOVE_COOLDOWN_MS_MIN
        );

        game.set_move_cooldown_ms(u64::MAX);
        assert_eq!(
            game.move_cooldown_ms,
            crate::constants::MOVE_COOLDOWN_MS_MAX
        );
    }

    #[test]
    fn set_move_cooldown_ms_changes_how_quickly_repeated_moves_are_accepted() {
        // 設定値を小さくすると、既定(INPUT_COOLDOWN_MS=80ms)では通らないはずの短い
        // 間隔でも次の移動入力が通ることを確認する。
        let mut game = Game::new(5);
        for col in 0..FIELD_WIDTH {
            game.board.rows[game.player.row + 1][col] = Cell::Rock { hits: 0 };
        }
        game.set_move_cooldown_ms(crate::constants::MOVE_COOLDOWN_MS_MIN);

        game.try_move_right();
        let col_after_first = game.player.col;
        // MOVE_COOLDOWN_MS_MIN(20ms)は上回るが、既定のINPUT_COOLDOWN_MS(80ms)は
        // 上回らない経過時間を進める。
        game.update(Duration::from_millis(30));
        game.try_move_right();

        assert_ne!(
            game.player.col, col_after_first,
            "短いクールダウン設定なら次の移動が通るはず"
        );
    }

    #[test]
    fn move_cooldown_overshoot_carries_forward_to_the_next_slot() {
        // クールダウンぶんを使い切った後、少し余分に時間が経ってから次の入力が来た場合、
        // その超過ぶんが繰り越され、次の入力までの待ち時間がその分だけ短くなることを確認
        // する(0へリセットする旧実装ではこの繰り越しが起きずジッターの原因になっていた)。
        let mut game = Game::new(6);
        for col in 4..=8 {
            game.board.rows[game.player.row + 1][col] = Cell::Rock { hits: 0 };
        }

        game.try_move_right(); // 1回目: 即座に受理される(accumが満タンから始まるため。accum=0になる)
        let col_after_first = game.player.col;

        // クールダウン(80ms)に30ms上乗せしてから2回目を試みる(超過30ms)。
        game.update(Duration::from_millis(INPUT_COOLDOWN_MS + 30));
        game.try_move_left();
        let col_after_second = game.player.col;
        assert_ne!(
            col_after_second, col_after_first,
            "2回目は受理されるはず(accumが30msへ繰り越される)"
        );

        // 3回目: 繰り越された30msぶん、フルの80ms待たなくても(50ms経過だけで)受理されるはず
        // (30+50=80msでちょうどスロットに達する)。
        game.update(Duration::from_millis(INPUT_COOLDOWN_MS - 30));
        game.try_move_right();
        assert_ne!(
            game.player.col, col_after_second,
            "繰り越し分により50ms経過でも受理されるはず"
        );
    }

    #[test]
    fn move_cooldown_accum_does_not_bank_unbounded_after_a_long_idle_period() {
        // 長時間入力が無い間にアキュムレータが際限なく貯まると、後からまとめて連続入力が
        // 全て即座に通ってしまう(バースト)。上限で頭打ちにして防いでいることを確認する。
        let mut game = Game::new(7);
        for col in 4..=8 {
            game.board.rows[game.player.row + 1][col] = Cell::Rock { hits: 0 };
        }

        // 10秒間、何も入力せず放置する(アキュムレータが上限で頭打ちになるはず)。
        game.update(Duration::from_secs(10));

        game.try_move_right(); // 1回目: 受理される
        let col_after_first = game.player.col;
        game.try_move_left(); // 2回目: 直後なのでまだクールダウン中のはず(受理されない)
        assert_eq!(
            game.player.col, col_after_first,
            "長時間放置後でも2回目は直後には受理されない(バーストしない)はず"
        );
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

        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::LevelUp { level: 2 }))
        );
        assert!(
            !events.contains(&GameEvent::ExtraLifeAtLevel { level: 2 }),
            "Lv.10の倍数でないレベルアップではライフを獲得しないはず"
        );
    }

    #[test]
    fn reaching_level_10_grants_an_extra_life() {
        // Lv.10ごとにライフ+1。
        let mut game = Game::new(5);
        game.player.row = 9 * crate::constants::LEVEL_STEP_M - 1; // depth=270, level=9のまま
        game.player.facing = Direction::Down;
        game.player.lives = 2;
        game.board.rows[game.player.row + 1][game.player.col] = Cell::Empty;

        game.try_drill(); // 掘るだけでは移動しない
        let events = game.update(Duration::from_millis(FALL_TICK_MS)); // 自由落下でdepth=271 -> level 10へ

        assert!(
            events.contains(&GameEvent::ExtraLifeAtLevel { level: 10 }),
            "Lv.10到達でライフ獲得イベントが発生するはず: {events:?}"
        );
        assert_eq!(game.player.lives, 3, "ライフが1増えているはず");
    }

    #[test]
    fn extra_life_at_level_10_is_clamped_at_the_lives_max() {
        // 既にライフが上限(LIVES_MAX)の場合、イベント自体は発生するが増えない。
        let mut game = Game::new(5);
        game.player.row = 9 * crate::constants::LEVEL_STEP_M - 1;
        game.player.facing = Direction::Down;
        game.player.lives = LIVES_MAX;
        game.board.rows[game.player.row + 1][game.player.col] = Cell::Empty;

        game.try_drill();
        let events = game.update(Duration::from_millis(FALL_TICK_MS));

        assert!(events.contains(&GameEvent::ExtraLifeAtLevel { level: 10 }));
        assert_eq!(
            game.player.lives, LIVES_MAX,
            "既に上限ならそれ以上増えないはず"
        );
    }

    #[test]
    fn reaching_a_100m_checkpoint_clears_above_and_emits_the_event_and_flash() {
        // 7章のレベル進行(30m刻み)とは別の、100m刻みの独立した節目。到達判定は地面
        // (CHECKPOINT_SAFE_ZONE_M)を実際に掘り抜いた地点(row=100+CHECKPOINT_SAFE_ZONE_M)。
        let mut game = Game::new(90);
        game.player.row =
            crate::constants::CHECKPOINT_STEP_M + crate::constants::CHECKPOINT_SAFE_ZONE_M - 1; // 地面を掘り抜く直前
        game.player.facing = Direction::Down;
        game.board.rows[game.player.row + 1][game.player.col] = Cell::Empty; // 自由落下させる
        game.board.rows[10][game.player.col] = Cell::Rock { hits: 0 }; // 頭上、クリア対象になるはず

        game.try_drill(); // 掘るだけでは移動しない
        let events = game.update(Duration::from_millis(FALL_TICK_MS)); // 地面を掘り抜く -> checkpoint到達

        assert!(
            events.contains(&GameEvent::Checkpoint100m { at_m: 100 }),
            "チェックポイント到達イベントが発生するはず: {events:?}"
        );
        assert_eq!(
            game.board.cell(10, game.player.col),
            Cell::Empty,
            "頭上のブロックはチェックポイント到達で全クリアされるはず"
        );
        assert_eq!(
            game.checkpoint_flash_depth_m(),
            Some(100),
            "到達演出が表示中のはず"
        );
    }

    #[test]
    fn checkpoint_ground_is_never_force_cleared_even_after_reaching_the_checkpoint() {
        // 地面(CHECKPOINT_SAFE_ZONE_M)部分は強制的にくり抜かずプレイヤーが掘り進む対象
        // なので、ゲーム開始直後もチェックポイント到達後も、地面区間の生成された地形
        // (プレイヤー自身が掘っていない列)はそのまま残るはず。
        let game = Game::new(300);
        let start = crate::constants::CHECKPOINT_STEP_M;
        let end = start + crate::constants::CHECKPOINT_SAFE_ZONE_M;
        let all_empty = (start..end)
            .flat_map(|row| (0..game.board.width()).map(move |col| (row, col)))
            .all(|(row, col)| game.board.cell(row, col) == Cell::Empty);
        assert!(
            !all_empty,
            "生成直後は100mチェックポイントの地面がまだくり抜かれていないはず"
        );
    }

    #[test]
    fn applying_a_checkpoint_safe_zone_only_carves_the_gap_not_the_ground() {
        // apply_checkpoint_safe_zoneが実際にくり抜くのはスキマ(CHECKPOINT_ZONE_GAP_M)
        // だけで、地面(CHECKPOINT_SAFE_ZONE_M)区間は残ることを確認する(頭上を全クリア
        // する`debug_clear_above_player`の副次効果と混同しないよう関数単体を直接呼ぶ)。
        let mut game = Game::new(301);
        game.board.rows[101][0] = Cell::Rock { hits: 0 };

        game.apply_checkpoint_safe_zone(100);

        assert_eq!(
            game.board.cell(101, 0),
            Cell::Rock { hits: 0 },
            "地面区間はくり抜かれず残るはず"
        );
        let width = game.board.width();
        for col in 0..width {
            for row in 105..105 + crate::constants::CHECKPOINT_ZONE_GAP_M {
                assert_eq!(
                    game.board.cell(row, col),
                    Cell::Empty,
                    "地面を掘り抜いた直後のスキマ(row={row}, col={col})はくり抜かれるはず"
                );
            }
        }
    }

    #[test]
    fn checkpoint_flash_clears_after_checkpoint_flash_ms_elapses() {
        let mut game = Game::new(91);
        game.player.row =
            crate::constants::CHECKPOINT_STEP_M + crate::constants::CHECKPOINT_SAFE_ZONE_M - 1;
        game.player.facing = Direction::Down;
        game.board.rows[game.player.row + 1][game.player.col] = Cell::Empty;

        game.try_drill();
        game.update(Duration::from_millis(FALL_TICK_MS));
        assert_eq!(game.checkpoint_flash_depth_m(), Some(100));

        game.update(Duration::from_millis(
            crate::constants::CHECKPOINT_FLASH_MS + 10,
        ));
        assert_eq!(
            game.checkpoint_flash_depth_m(),
            None,
            "表示時間が過ぎたら演出は終わっているはず"
        );
    }

    #[test]
    fn a_jump_that_skips_multiple_checkpoints_at_once_does_not_destructively_clear_or_fire_the_event()
     {
        // テストコード等が`player.row`を直接遠くへ書き換えると、通常のプレイ(1行ずつ
        // しか進まない)では起こらない「一気に複数チェックポイント分進む」ケースが生じる。
        // このとき区切り番号の追従はするが、頭上の破壊的な全クリア・演出・イベント発火は
        // 行わない(無関係なテストの前提を壊さないため)。
        let mut game = Game::new(92);
        clear_board(&mut game);
        game.player.row = 500; // 一気にcheckpoint=5相当まで飛ぶ(非正規経路)
        game.board.rows[10][game.player.col] = Cell::Rock { hits: 0 };

        let mut events = Vec::new();
        game.check_level_and_clear(&mut events);

        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::Checkpoint100m { .. })),
            "一気に複数チェックポイント分飛ぶジャンプではイベントを発火しないはず: {events:?}"
        );
        assert_eq!(
            game.board.cell(10, game.player.col),
            Cell::Rock { hits: 0 },
            "一気に複数チェックポイント分飛ぶジャンプでは頭上を破壊的にクリアしないはず"
        );
        assert_eq!(game.checkpoint_flash_depth_m(), None);
    }

    #[test]
    fn reaching_the_final_goal_depth_does_not_also_fire_a_checkpoint_event() {
        // 最終ゴール(FIELD_DEPTH_M)はGameEvent::Clearedが同じ役割の演出を持つため、
        // Checkpoint100mを二重発火させない。
        let mut game = Game::new(93);
        clear_board(&mut game);
        game.player.row = crate::constants::FIELD_DEPTH_M - 2; // depth=FIELD_DEPTH_M-1
        let mut events = Vec::new();
        game.check_level_and_clear(&mut events); // 区切り番号を追従させるだけ

        game.player.row = crate::constants::FIELD_DEPTH_M - 1; // depth=FIELD_DEPTH_M(ゴール)
        let mut events2 = Vec::new();
        game.check_level_and_clear(&mut events2);

        assert!(
            !events2
                .iter()
                .any(|e| matches!(e, GameEvent::Checkpoint100m { .. })),
            "最終ゴールではCheckpoint100mを二重発火させないはず: {events2:?}"
        );
        assert!(events2.contains(&GameEvent::Cleared));
    }

    #[test]
    fn move_right_never_drills_and_climbs_over_a_blocking_color_block_on_second_press() {
        // カーソルキー(MoveLeft/MoveRight)は掘削を一切行わない。隣が塞がっていると、
        // 1回目の入力ではぶつかって停止するだけで登らず、同じ方向への2回目の入力で初めて
        // 1段上(row-1)へ登る。ブロックはどちらの場合も破壊されない。
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

        game.move_cooldown_accum = Duration::from_millis(INPUT_COOLDOWN_MS);
        game.drill_cooldown_accum = Duration::from_millis(INPUT_COOLDOWN_MS); // クールダウンを明ける(本テストの本題ではない)
        let second_events = game.try_move_right(); // 2回目: 同じ方向への再入力で登る

        assert_eq!(game.player.row, 0); // 1段登った
        assert_eq!(game.player.col, target_col);
        assert_eq!(game.player.facing, Direction::Right);
        assert_eq!(game.player.score, 0); // 掘削していないので加点なし
        assert!(second_events.is_empty()); // 掘削・破壊イベントは一切発生しない
        assert_eq!(game.board.cell(1, target_col), Cell::Color(ColorKind::Red)); // ブロックは残る
    }

    #[test]
    fn drilling_between_bump_and_second_press_does_not_cancel_the_pending_climb() {
        // 1回目のぶつかり(bumped_direction記憶)と2回目の同方向入力(段差登り)の間に
        // 掘削キーが挟まっても、段差登りがキャンセルされないことを確認する。
        let mut game = Game::new(6);
        game.player.row = 1;
        let target_col = game.player.col + 1;
        game.board.rows[game.player.row][target_col] = Cell::Rock { hits: 0 }; // 1発では壊れない
        // row 0(1段上)は生成上つねにEmpty(安全地帯、spec.md 3.2)

        game.try_move_right(); // 1回目: ぶつかって停止
        assert_eq!(game.player.bumped_direction, Some(Direction::Right));

        game.move_cooldown_accum = Duration::from_millis(INPUT_COOLDOWN_MS);
        game.drill_cooldown_accum = Duration::from_millis(INPUT_COOLDOWN_MS);
        game.try_drill(); // 間に掘削キーを挟む(岩は1発では壊れず残る)
        assert_eq!(
            game.player.bumped_direction,
            Some(Direction::Right),
            "掘削を挟んでもぶつかり状態は保持されるはず"
        );

        game.move_cooldown_accum = Duration::from_millis(INPUT_COOLDOWN_MS);
        game.try_move_right(); // 2回目: 同じ方向への再入力で登る

        assert_eq!(
            game.player.row, 0,
            "掘削を挟んでも段差登りがキャンセルされてはいけない"
        );
        assert_eq!(game.player.col, target_col);
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

        game.move_cooldown_accum = Duration::from_millis(INPUT_COOLDOWN_MS);
        game.drill_cooldown_accum = Duration::from_millis(INPUT_COOLDOWN_MS); // クールダウンを明ける(本テストの本題ではない)
        let second_events = game.try_move_left(); // 2回目: 同じ方向への再入力で登る

        assert_eq!(game.player.row, 0); // 1段登った
        assert_eq!(game.player.col, target_col);
        assert_eq!(game.player.facing, Direction::Left);
        assert_eq!(game.player.score, 0); // 掘削していないので加点なし
        assert!(second_events.is_empty()); // 掘削・破壊イベントは一切発生しない
        assert_eq!(
            game.board.cell(1, target_col),
            Cell::Color(ColorKind::Green)
        ); // ブロックは残る
    }

    #[test]
    fn try_move_right_into_oxygen_capsule_collects_it_and_emits_event() {
        // AIRカプセルは掘削不要で、隣接移動だけでも自動的に取得でき、SE再生用の
        // GameEventも発火する。
        let mut game = Game::new(8);
        // 開始直後の上2行は常にEmptyなので、直下に足場を置いて横移動できる状態にする。
        game.board.rows[game.player.row + 1][game.player.col] = Cell::Rock { hits: 0 };
        let target_col = game.player.col + 1;
        game.board.rows[game.player.row][target_col] = Cell::Oxygen;
        game.player.oxygen = 40.0;

        let events = game.try_move_right();

        assert_eq!(game.player.col, target_col);
        assert_eq!(
            game.player.oxygen,
            40.0 + crate::constants::OXYGEN_CAPSULE_RESTORE
        );
        assert_eq!(game.player.score, 100);
        assert_eq!(game.board.cell(game.player.row, target_col), Cell::Empty);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::OxygenCollected))
        );
    }

    #[test]
    fn try_move_right_climbing_under_an_overhead_item_applies_its_effect_and_emits_the_event() {
        // #244: 自分の真上のアイテムは段差登りを妨げず、登る際に取得して効果も発動する。
        let mut game = Game::new(74);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 足場(横移動には支持が必要)
        game.board.rows[500][6] = Cell::Color(ColorKind::Red); // ぶつかる壁
        game.board.rows[499][5] = Cell::Item(ItemEffect::ClearAbove); // 自分の真上
        game.board.rows[200][4] = Cell::Color(ColorKind::Red); // 効果の確認用

        let first_events = game.try_move_right(); // 1回目: ぶつかって停止

        assert_eq!(game.player.row, 500, "1回目では登らない");
        assert!(first_events.is_empty());
        assert_eq!(
            game.board.cell(499, 5),
            Cell::Item(ItemEffect::ClearAbove),
            "登っていないのでアイテムも取得しない"
        );

        game.move_cooldown_accum = Duration::from_millis(INPUT_COOLDOWN_MS);
        let second_events = game.try_move_right(); // 2回目: 取得しながら登る

        assert_eq!(game.player.row, 499, "1段登った");
        assert_eq!(game.player.col, 6);
        assert_eq!(game.board.cell(499, 5), Cell::Empty); // アイテムは消費された
        assert!(
            second_events
                .iter()
                .any(|e| matches!(e, GameEvent::ItemCollected(ItemEffect::ClearAbove)))
        );
        assert!(
            matches!(game.board.cell(200, 4), Cell::Empty),
            "ショートカットRと同じく頭上のブロックが全クリアされるはず"
        );
    }

    #[test]
    fn try_move_right_climbing_through_two_oxygen_capsules_emits_a_single_oxygen_event() {
        // 頭上と登り先の2マスぶんAIRを取得しても、SEは重力ティック経路と同じく
        // 1回にまとめる(#244)。
        let mut game = Game::new(74);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.player.oxygen = 40.0;
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 足場
        game.board.rows[500][6] = Cell::Rock { hits: 0 }; // ぶつかる壁
        game.board.rows[499][5] = Cell::Oxygen; // 自分の真上
        game.board.rows[499][6] = Cell::Oxygen; // 登り先

        game.try_move_right(); // 1回目: ぶつかって停止
        game.move_cooldown_accum = Duration::from_millis(INPUT_COOLDOWN_MS);
        let events = game.try_move_right(); // 2回目: 2個取得しながら登る

        assert_eq!(game.player.row, 499);
        assert_eq!(game.player.col, 6);
        assert_eq!(game.player.oxygen_capsules_collected, 2);
        assert_eq!(game.player.score, 300); // 100(1個目)+200(2個目)
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, GameEvent::OxygenCollected))
                .count(),
            1,
            "2個まとめて取得してもOxygenCollectedは1回だけ"
        );
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
        assert!(matches!(
            game.board.cell(1, target_col),
            Cell::Rock { hits: 0 }
        )); // 壊れない
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
            assert_eq!(
                game.player.oxygen, oxygen_before,
                "{hit}回目のヒットでは酸素は減らない"
            );
            assert_eq!(
                game.player.row,
                target_row - 1,
                "岩が壊れるまでは降下しない"
            );
            assert!(events.iter().any(|e| matches!(e, GameEvent::RockHitIntact)));
            // 次のヒットのためクールダウンを明ける(spec.md 9.9のクールダウンは本テストの本題ではない)
            game.move_cooldown_accum = Duration::from_millis(INPUT_COOLDOWN_MS);
            game.drill_cooldown_accum = Duration::from_millis(INPUT_COOLDOWN_MS);
        }

        let events = game.try_drill(); // 5回目: 破壊

        assert_eq!(game.board.cell(target_row, col), Cell::Empty);
        assert_eq!(game.player.oxygen, oxygen_before - 20.0);
        assert_eq!(game.player.row, target_row - 1, "掘っただけでは移動しない");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::RockDestroyed { blocks: 1 }))
        );

        game.update(Duration::from_millis(FALL_TICK_MS)); // 自由落下で開いたマスへ進む
        assert_eq!(game.player.row, target_row, "自由落下で続けて1マス下降する");
    }

    #[test]
    fn drilling_a_rock_to_its_fifth_hit_vanishes_only_that_block() {
        // 岩ブロックは連結していても掘削で消えるのは1ブロックのみ。5回目のヒットで
        // 破壊されるのはそのセルだけで、隣の岩ブロックは影響を受けない(酸素は-20%)。
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
        assert_eq!(
            game.player.oxygen,
            oxygen_before - 20.0,
            "酸素ペナルティは1回分のみ"
        );
        assert_eq!(game.player.score, 0, "岩ブロックの消滅は得点対象外");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::RockDestroyed { blocks: 1 }))
        );
    }

    #[test]
    fn falling_rock_blocks_connecting_to_four_or_more_auto_vanish_via_update() {
        // 岩ブロックも支えを失えば(揺れを経て)落下し、支持されている岩ブロックに接触
        // して連結、4個以上になれば自動消滅する(得点は対象外)。
        let mut game = Game::new(41);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 11; // 落下グループから十分離す

        game.board.rows[998][0] = Cell::Rock { hits: 0 };
        game.board.rows[998][1] = Cell::Rock { hits: 1 };
        game.board.rows[998][2] = Cell::Rock { hits: 2 };
        game.board.rows[999][3] = Cell::Rock { hits: 3 }; // 最深行=常に支持
        let score_before = game.player.score;

        let events = game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));

        assert_eq!(
            game.player.score, score_before,
            "岩ブロックの自動消滅はスコア対象外"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::RockDestroyed { blocks: 4 })),
            "4個以上連結した岩ブロックの自動消滅でRockDestroyedイベントが発生する"
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
        let events = game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));

        assert_eq!(game.player.score, 4 * 30);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::BlockDestroyed { blocks: 4 }))
        );
        assert_eq!(game.board.cell(999, 0), Cell::Empty);
        assert_eq!(game.board.cell(999, 1), Cell::Empty);
        assert_eq!(game.board.cell(999, 2), Cell::Empty);
        assert_eq!(game.board.cell(999, 3), Cell::Empty);
    }

    #[test]
    fn chain_pause_blocks_gravity_resolution_until_it_elapses_then_resumes() {
        // `chain_pause_remaining`が0より大きい間は重力解決(盤面の変化)自体が一切進まず、
        // 経過後は通常通り再開することを確認する。深度に応じた落下速度上昇
        // (depth_fraction)の影響を避けるため、プレイヤーは浅い深度に置く。
        let mut game = Game::new(20);
        clear_board(&mut game);
        game.player.row = 1;
        game.player.col = 0;
        game.chain_pause_remaining = Duration::from_millis(300);

        // 支えを失って揺れ待ちの監視用ブロック。
        game.board.rows[5][5] = Cell::Rock { hits: 0 };
        game.board.rows[6][5] = Cell::Empty;

        // 足止め中は、監視用ブロックの揺れ・落下も一切進まないはず
        // (重力解決そのものが止まっているため)。
        game.update(Duration::from_millis(FALL_TICK_MS));
        assert_eq!(
            game.board.cell(5, 5),
            Cell::Rock { hits: 0 },
            "足止め中は重力解決が進まないはず"
        );
        assert!(game.chain_pause_remaining > Duration::ZERO);

        // 足止めが明ければ(合計300ms経過後)、揺れ→落下が通常通り再開する。
        game.update(Duration::from_millis(
            300 + (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));
        assert_eq!(
            game.board.cell(6, 5),
            Cell::Rock { hits: 0 },
            "足止めが明ければ監視用ブロックの落下も再開するはず"
        );
    }

    #[test]
    fn auto_vanish_sets_a_non_zero_chain_pause_when_the_interval_is_configured() {
        // 実際に自動消滅が発生した際、`chain_vanish_interval_ms`を設定していれば
        // `chain_pause_remaining`が0より大きい値にセットされることを確認する
        // (足止め時間の正確な残り値は深度依存の落下速度で変わるため、ここでは
        // 「セットされていること」だけを確認する)。
        let mut game = Game::new(11);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 11; // 落下グループから十分離す
        game.set_chain_vanish_interval_ms(300);

        game.board.rows[998][0] = Cell::Color(ColorKind::Red);
        game.board.rows[998][1] = Cell::Color(ColorKind::Red);
        game.board.rows[998][2] = Cell::Color(ColorKind::Red);
        game.board.rows[999][3] = Cell::Color(ColorKind::Red); // 最深行=常に支持

        // SHAKE_TICKSぶんは揺れるだけで、その次の周期で落下+着地+自動消滅する。
        let events = game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::BlockDestroyed { blocks: 4 })),
            "前提: 自動消滅が発生しているはず: {events:?}"
        );
        assert!(
            game.chain_pause_remaining > Duration::ZERO,
            "自動消滅直後は連鎖インターバルぶんの足止めがセットされるはず"
        );
    }

    #[test]
    fn chain_vanish_interval_of_zero_behaves_exactly_like_before_with_no_extra_delay() {
        // 既定値(0)では、従来通り足止め無しで即座に連鎖することを確認する回帰テスト。
        let mut game = Game::new(13);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 11;
        assert_eq!(
            game.chain_vanish_interval_ms, 0,
            "前提: 既定値は0(従来通り)のはず"
        );

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
        assert_eq!(
            game.chain_pause_remaining,
            Duration::ZERO,
            "インターバル0なら足止めはセットされないはず"
        );
    }

    #[test]
    fn auto_vanished_cells_show_a_vanish_flash_that_expires_after_block_vanish_flash_ms() {
        // 自動消滅したセルは消滅直後にフラッシュ演出の対象になり、
        // BLOCK_VANISH_FLASH_MS経過後に対象から外れることを確認する。
        let mut game = Game::new(12);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 11; // 落下グループから十分離す

        game.board.rows[998][0] = Cell::Color(ColorKind::Red);
        game.board.rows[998][1] = Cell::Color(ColorKind::Red);
        game.board.rows[998][2] = Cell::Color(ColorKind::Red);
        game.board.rows[999][3] = Cell::Color(ColorKind::Red); // 最深行=常に支持

        game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));

        for col in 0..=3 {
            assert!(
                game.vanish_flash_progress((999, col)).is_some(),
                "消滅直後のセル(999,{col})はフラッシュ演出の対象になっているはず"
            );
        }
        assert!(
            game.vanish_flash_progress((0, 0)).is_none(),
            "無関係なセルはフラッシュ演出の対象ではないはず"
        );

        game.update(Duration::from_millis(
            crate::constants::BLOCK_VANISH_FLASH_MS + 10,
        ));
        assert!(
            game.vanish_flash_progress((999, 0)).is_none(),
            "BLOCK_VANISH_FLASH_MS経過後はフラッシュ演出が終わっているはず"
        );
    }

    #[test]
    fn note_vanished_cells_extends_adjacent_still_flashing_cells_for_chain_reactions() {
        // 重力で連鎖的に4連結消滅が起きた場合、先に消えたセルの演出が途切れず
        // 1つの連鎖に見えるよう、隣接セルの消滅で残り時間が延長される。
        let mut game = Game::new(1);
        game.note_vanished_cells(vec![((0, 0), Cell::Color(ColorKind::Red))], Duration::ZERO);
        game.update(Duration::from_millis(BLOCK_VANISH_FLASH_MS / 2));
        let progress_before = game.vanish_flash_progress((0, 0)).unwrap();
        assert!(progress_before > 0.0, "前提: フラッシュが進行中であること");

        // 隣接セル(0,1)が新たに消滅 → (0,0)の残り時間もリセットされて延長されるはず。
        game.note_vanished_cells(vec![((0, 1), Cell::Color(ColorKind::Red))], Duration::ZERO);
        let progress_after = game.vanish_flash_progress((0, 0)).unwrap();
        assert!(
            progress_after < progress_before,
            "隣接消滅で演出が延長され、進捗が巻き戻るはず"
        );
        assert!(
            game.vanish_flash_progress((0, 1)).is_some(),
            "新しく消滅したセルもフラッシュ中のはず"
        );
    }

    #[test]
    fn note_vanished_cells_does_not_extend_non_adjacent_still_flashing_cells() {
        let mut game = Game::new(1);
        game.note_vanished_cells(vec![((0, 0), Cell::Color(ColorKind::Red))], Duration::ZERO);
        game.update(Duration::from_millis(BLOCK_VANISH_FLASH_MS / 2));
        let progress_before = game.vanish_flash_progress((0, 0)).unwrap();

        // 隣接していない遠いセル(5,5)が消滅しても、(0,0)の演出は延長されないはず。
        game.note_vanished_cells(vec![((5, 5), Cell::Color(ColorKind::Red))], Duration::ZERO);
        let progress_after = game.vanish_flash_progress((0, 0)).unwrap();
        assert!(
            (progress_after - progress_before).abs() < f32::EPSILON,
            "無関係な位置の消滅では演出が延長されないはず"
        );
    }

    #[test]
    fn melted_star_cell_also_shows_a_vanish_flash() {
        // スター溶解による消滅も、自動消滅と同様にフラッシュ演出の対象になることを確認する。
        let mut game = Game::new(13);
        clear_board(&mut game);
        game.player.row = 999; // 最深行=常に支持される安定した足場にできる
        game.player.col = 5;
        game.board.rows[999][3] = Cell::Rock { hits: 0 }; // 最深行=常に支持
        game.board.rows[998][3] = Cell::Star { visible_ms: 0 }; // 岩の上に乗った、支えのあるスター

        game.update(Duration::from_millis(
            crate::constants::STAR_VISIBLE_GRACE_MS as u64
                + crate::constants::STAR_MELT_DURATION_MS as u64,
        ));

        assert_eq!(
            game.board.cell(998, 3),
            Cell::Empty,
            "溶け切ったスターは消えているはず"
        );
        assert!(
            game.vanish_flash_progress((998, 3)).is_some(),
            "溶けて消えたスターもフラッシュ演出の対象になっているはず"
        );
    }

    #[test]
    fn color_block_resting_on_a_star_falls_once_the_star_melts_away() {
        // スターブロックの上に乗っていた色ブロックが、スターが溶けて消えた後に
        // 「浮いた」状態で残らず、ちゃんと支えを失って落下することを確認する。
        let mut game = Game::new(41);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 11; // 落下グループから十分離す
        game.board.rows[999][3] = Cell::Rock { hits: 0 }; // 最深行=常に支持
        game.board.rows[998][3] = Cell::Star { visible_ms: 0 }; // 岩の上に乗ったスター
        game.board.rows[997][3] = Cell::Color(ColorKind::Red); // スターの上に乗った色ブロック

        // スターが溶けきるまで進める(実時間ベース)。
        game.update(Duration::from_millis(
            crate::constants::STAR_VISIBLE_GRACE_MS as u64
                + crate::constants::STAR_MELT_DURATION_MS as u64
                + 10,
        ));
        assert_eq!(
            game.board.cell(998, 3),
            Cell::Empty,
            "スターは溶けて消えているはず"
        );

        // スターが消えた直後もまだ揺れ猶予中のはずなので、揺れ+落下ぶんのブロック
        // 落下tickが経過するまで細かく進める(実際のフレームレートに近い刻みで)。
        const FRAME_MS: u64 = 33;
        let ticks_needed = (SHAKE_TICKS as u64 + 2) * FALL_TICK_MS / FRAME_MS + 2;
        for _ in 0..ticks_needed {
            game.update(Duration::from_millis(FRAME_MS));
        }

        assert_ne!(
            game.board.cell(997, 3),
            Cell::Color(ColorKind::Red),
            "スターが消えた元の位置に色ブロックが浮いたまま残ってはいけない"
        );
        assert_eq!(
            game.board.cell(998, 3),
            Cell::Color(ColorKind::Red),
            "色ブロックはスターが消えた分だけ1マス落下しているはず"
        );
    }

    #[test]
    fn falling_block_merges_after_a_long_multi_row_fall_via_many_small_frame_updates() {
        // 1回の大きなdeltaでまとめて進める他のテストと異なり、実際のmain.rs
        // (FRAME_INTERVAL_MS=33msごとにupdate())と同じ細かい刻みで何十行分もの空洞を
        // 連続落下させ、既存の縦連結(3個)と合流して5個以上になった時点で自動消滅する
        // ことを確認する。
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
            events
                .iter()
                .any(|e| matches!(e, GameEvent::BlockDestroyed { blocks: 5 })),
            "縦5個での自動消滅イベントが発生していない"
        );
        assert_eq!(
            game.player.score,
            5 * 30,
            "5個ぶんの自動消滅スコアが入っているはず"
        );
        for row in [997, 998, 999] {
            assert_eq!(
                game.board.cell(row, 0),
                Cell::Empty,
                "row={row}が消えていない"
            );
        }
    }

    // --- ブロック落下のピクセル単位補間描画 ---

    #[test]
    fn recently_moved_blocks_and_progress_track_the_latest_gravity_tick() {
        // 実際に1マス落下したtickの直後は、その(移動後の位置, 移動前の位置)が
        // recently_moved_blocksに記録され、block_fall_progressはそのtickの開始直後を
        // 表す小さな値になっていることを確認する。
        let mut game = Game::new(1);
        clear_board(&mut game);
        // 深度による落下速度スケーリングの影響を受けないよう、プレイヤーは
        // 深度0m相当(等倍速)の浅い位置に置く。
        game.player.row = 1;
        game.player.col = 5;
        game.board.rows[0][3] = Cell::Color(ColorKind::Red);

        game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));

        let moves = game.recently_moved_blocks();
        assert_eq!(
            moves,
            &[((1, 3), (0, 3))],
            "揺れが明けて1マス落下した直後のはず"
        );
        assert!(
            game.block_fall_progress() < 0.2,
            "ティック開始直後なのでprogressは小さいはず: {}",
            game.block_fall_progress()
        );

        // 次のtickまでの間、時間経過とともにprogressが増える。
        game.update(Duration::from_millis(FALL_TICK_MS / 2));
        assert!(
            game.block_fall_progress() > 0.3,
            "半分近く経過すればprogressも増えるはず: {}",
            game.block_fall_progress()
        );
    }

    #[test]
    fn free_fall_move_animation_duration_matches_player_fall_tick_ms_not_the_fixed_default() {
        // 自由落下の見た目補間は、横移動用の固定の短い時間(MOVE_ANIM_DURATION_MS)では
        // なく、実際のplayer_fall_tick_msぶんかけて行われることを確認する。
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 10;
        game.set_player_fall_tick_ms(400); // MOVE_ANIM_DURATION_MS(100ms)よりずっと長い

        // ちょうど1回ぶんの自由落下tickを発生させる。
        game.update(Duration::from_millis(410));
        assert_eq!(game.player.row, 11, "1マス落下しているはず");
        assert_eq!(
            game.move_anim_progress(),
            0.0,
            "落下tick直後は補間がまだ始まったばかりのはず"
        );

        // 次のtick(400ms後)がまだ来ない150ms経過時点でも、補間が完了していないはず
        // (固定100msのままなら、ここで既に1.0=完了してしまう)。
        game.update(Duration::from_millis(150));
        assert!(
            game.move_anim_progress() < 1.0,
            "player_fall_tick_ms(400ms)に合わせた補間ならまだ完了していないはず: {}",
            game.move_anim_progress()
        );
    }

    #[test]
    fn player_falls_automatically_through_empty_space_without_any_input() {
        // spec.md 1章: 支えを失った(直下がEmptyな)プレイヤーは、入力が無くても
        // FALL_TICK_MSごとに1マスずつ自動的に落下し続ける。周囲のブロックが偶然崩れて
        // 割り込まないよう、通り道を広めにEmptyでクリアしてから検証する。
        let mut game = Game::new(20);
        for row in 5..16 {
            for col in 0..FIELD_WIDTH {
                game.board.rows[row][col] = Cell::Empty;
            }
        }
        game.player.row = 10;
        let col = game.player.col;

        // FALL_TICK_MS(150ms)を3周期分進める -> 3マス落下するはず
        let events = game.update(Duration::from_millis(3 * FALL_TICK_MS + 10));

        assert_eq!(game.player.row, 13);
        assert_eq!(game.player.col, col);
        assert!(!events.iter().any(|e| matches!(
            e,
            GameEvent::LifeLost { .. } | GameEvent::GameOverMiss { .. }
        )));
    }

    #[test]
    fn player_does_not_get_stuck_floating_over_a_tall_open_shaft_across_many_frames() {
        // main.rsの実際の使い方(FRAME_INTERVAL_MS=33msごとにupdate()を呼ぶ)を模して、
        // 細かいフレーム単位で何十フレームも進めても、支えを失ったプレイヤーが一度も
        // 止まらず(大きな縦穴の上で浮いたまま静止せず)落下し続けることを確認する。
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
        assert!(
            max_row_seen > 100,
            "プレイヤーは一度も動かなかった(浮いたまま)"
        );
    }

    #[test]
    fn player_does_not_fall_when_supported() {
        let mut game = Game::new(21);
        for row in 2..6 {
            for col in 0..FIELD_WIDTH {
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

    // --- 押し潰されて死ぬ演出(9章) ---

    #[test]
    fn oxygen_miss_also_activates_the_flash_effect() {
        // 酸素切れ死亡でも押し潰しと同じ「潰れた」フラッシュ演出が起きる。
        let mut game = Game::new(30);
        game.player.oxygen = 1.0;

        game.update(Duration::from_secs(1)); // 酸素切れでミス(押し潰しではない)

        assert!(game.crush_flash_active());
    }

    #[test]
    fn crush_death_clears_the_full_width_above_the_player_like_the_clear_above_item() {
        // 押し潰しミス発生時、プレイヤーより上のブロックが盤面幅全体でクリアされる
        // (Rアイテム`debug_clear_above_player`と同じ処理なので、離れた列も対象)。
        let mut game = Game::new_with_lives(34, 2); // ライフ2、押し潰されても即GameOverにならない
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        for row in 990..999 {
            game.board.rows[row][4] = Cell::Color(ColorKind::Blue);
            game.board.rows[row][5] = Cell::Color(ColorKind::Blue);
            game.board.rows[row][6] = Cell::Color(ColorKind::Blue);
        }
        // 離れた列(3, 7)も同じくクリア対象になることを確認するために配置しておく。
        game.board.rows[990][3] = Cell::Color(ColorKind::Green);
        game.board.rows[990][7] = Cell::Color(ColorKind::Green);
        game.board.rows[998][5] = Cell::Color(ColorKind::Red); // プレイヤーの真上、支えなし

        game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));
        // 押し潰し直後は「天に召される」演出中で、ライフ減算・頭上クリアは演出が
        // 終わるまで遅延される。演出の完了を待つ。
        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));

        assert_eq!(
            game.player.lives, 1,
            "押し潰されてライフを1つ失っているはず"
        );
        for row in 0..999 {
            for col in 0..game.board.width() {
                assert_eq!(
                    game.board.cell(row, col),
                    Cell::Empty,
                    "row={row} col={col}はクリアされているはず(盤面幅全体が対象)"
                );
            }
        }
    }

    #[test]
    fn oxygen_death_goes_through_the_same_ascend_and_full_width_clear_as_crush_death() {
        // 酸素切れ死亡でも押し潰し死亡と全く同じ処理(「天に召される」演出→演出完了後に
        // 盤面幅全体をクリア・ライフ減算)が行われることを確認する。
        let mut game = Game::new_with_lives(34, 2); // ライフ2、酸素切れでも即GameOverにならない
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        for row in 990..999 {
            game.board.rows[row][4] = Cell::Color(ColorKind::Blue);
            game.board.rows[row][5] = Cell::Color(ColorKind::Blue);
            game.board.rows[row][6] = Cell::Color(ColorKind::Blue);
        }
        game.board.rows[990][3] = Cell::Color(ColorKind::Green);
        game.board.rows[990][7] = Cell::Color(ColorKind::Green);
        game.player.oxygen = 1.0;

        game.update(Duration::from_secs(1)); // 酸素切れでミス(押し潰しではない)

        assert!(
            game.is_dying(),
            "酸素切れでも押し潰しと同様、天に召される演出中のはず"
        );
        assert_eq!(
            game.player.lives, 2,
            "演出完了までライフ減算は遅延されるはず"
        );

        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));

        assert_eq!(game.player.lives, 1, "酸素切れでライフを1つ失っているはず");
        for row in 0..999 {
            for col in 0..game.board.width() {
                assert_eq!(
                    game.board.cell(row, col),
                    Cell::Empty,
                    "row={row} col={col}はクリアされているはず(盤面幅全体が対象)"
                );
            }
        }
    }

    #[test]
    fn resolve_block_player_overlap_pushes_the_overlapping_block_up_to_the_nearest_empty_cell() {
        // 重なったブロックは1つ上へ、そこも塞がっていればさらに上へと押し上げる。
        let mut game = Game::new(75);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.board.rows[499][5] = Cell::Rock { hits: 0 }; // 1つ上は塞がっている
        game.board.rows[500][5] = Cell::Color(ColorKind::Red); // プレイヤーのマスに重なったブロック

        game.resolve_block_player_overlap();

        assert_eq!(
            game.board.cell(500, 5),
            Cell::Empty,
            "プレイヤーのマスは空くはず"
        );
        assert_eq!(
            game.board.cell(498, 5),
            Cell::Color(ColorKind::Red),
            "1つ上(499)も塞がっているため、さらにその上(498)まで押し上げられるはず"
        );
    }

    #[test]
    fn crush_death_lets_oxygen_capsules_fall_instead_of_vanishing() {
        // 死亡時の頭上クリアの範囲内にあったAIRは消滅させず、周囲がEmptyになった結果、
        // 通常の重力で自然に落下することを確認する。
        let mut game = Game::new_with_lives(70, 2); // ライフ2、押し潰されても即GameOverにならない
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[998][5] = Cell::Color(ColorKind::Red); // プレイヤーの真上、支えなし(押し潰す)
        game.board.rows[990][5] = Cell::Oxygen; // クリア範囲内のAIR(支えなし、押し潰しと並行して自然落下もする)

        let mut events = game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));
        // 天に召される演出中も周囲の重力処理は止まらないため、この1回のupdateだけで
        // AIRが最後まで落下しきる可能性もある。
        events.extend(game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        )));
        assert_eq!(game.player.lives, 1, "演出完了でライフが減っているはず");

        // AIRは頭上クリアで消滅させられたのではなく、通常の重力に従って落下を続け、
        // 最終的にプレイヤーへ到達して正規の取得イベントとして処理される。
        let oxygen_count = |game: &Game| {
            game.board
                .rows
                .iter()
                .flatten()
                .filter(|c| **c == Cell::Oxygen)
                .count()
        };
        for _ in 0..50 {
            if oxygen_count(&game) == 0 {
                break;
            }
            events.extend(game.update(Duration::from_millis(FALL_TICK_MS)));
        }
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::OxygenCollected)),
            "AIRは消滅させられたのではなく、落下し続けて最終的に取得イベントが発生するはず"
        );
    }

    #[test]
    fn ascending_sequence_does_not_freeze_unrelated_falling_blocks_elsewhere_on_the_board() {
        // 「天に召される」演出中も、押し潰しとは無関係な別の場所の落下ブロックは
        // 通常通り重力で落下し続けることを確認する。
        let mut game = Game::new_with_lives(72, 2); // ライフ2、押し潰されても即GameOverにならない
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[998][5] = Cell::Color(ColorKind::Red); // プレイヤーの真上、支えなし(押し潰す)
        game.board.rows[500][0] = Cell::Color(ColorKind::Blue); // 押し潰しとは無関係な、遠く離れた落下ブロック

        game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));
        assert!(game.is_dying(), "押し潰し直後は天に召される演出中のはず");
        let row_during_ascend_start = (0..game.board.depth_rows())
            .find(|&r| game.board.cell(r, 0) == Cell::Color(ColorKind::Blue))
            .expect("無関係なブロックはまだ盤面のどこかに存在するはず");

        // 演出が完了するよりずっと前の、演出継続中の時点で確認する。
        game.update(Duration::from_millis(
            FALL_TICK_MS * (SHAKE_TICKS as u64 + 3),
        ));
        assert!(
            game.is_dying(),
            "演出はまだ続いているはず(CRUSH_ASCEND_MSに対して十分短い経過時間)"
        );

        let row_during_ascend_later = (0..game.board.depth_rows())
            .find(|&r| game.board.cell(r, 0) == Cell::Color(ColorKind::Blue))
            .expect("演出中に消滅してしまってはいけない");
        assert!(
            row_during_ascend_later > row_during_ascend_start,
            "演出中も無関係なブロックは重力で落下し続けるはず: start={row_during_ascend_start}, later={row_during_ascend_later}"
        );
    }

    #[test]
    fn debug_clear_above_player_moves_off_screen_oxygen_capsules_to_just_outside_the_screen() {
        // 画面外のAIRは画面内へ直接テレポートさせず、画面のすぐ外側
        // (just_off_screen_row = entry_row - 1)まで移動させ、以後の重力ティックで
        // 自然に画面内へ落ちてくるようにする(いきなり現れないため)。
        let mut game = Game::new(71);
        clear_board(&mut game);
        game.player.row = 50;
        game.player.col = 5;
        game.board.rows[10][5] = Cell::Oxygen; // 画面外(entry_row=46より浅い)
        game.board.rows[10][6] = Cell::Rock { hits: 2 }; // 比較用: AIR以外は通常通り消える

        game.debug_clear_above_player();

        assert_eq!(
            game.board.cell(10, 5),
            Cell::Empty,
            "画面外に残っていたAIRは元の位置には残らないはず"
        );
        let entry_row = game.player.row - crate::constants::PLAYER_SCREEN_ROWS_ABOVE;
        let just_off_screen_row = entry_row - 1;
        assert_eq!(
            game.board.cell(just_off_screen_row, 5),
            Cell::Oxygen,
            "AIRは画面のすぐ外側まで移動しているはず(画面内にいきなり現れない)"
        );
        assert_eq!(
            game.board.cell(entry_row, 5),
            Cell::Empty,
            "この時点ではまだ画面内には現れていないはず"
        );
        assert_eq!(
            game.board.cell(10, 6),
            Cell::Empty,
            "AIR以外は通常通り消えるはず"
        );
    }

    #[test]
    fn debug_clear_above_player_moves_off_screen_item_blocks_to_just_outside_the_screen_preserving_order()
     {
        // アイテムブロックもAIRと同じく削除されない。同じ列に複数ある場合、元の深さ順
        // (浅い方が先)を保ったまま画面のすぐ外側からさらに浅い側へ詰め直す
        // (画面内にはまだ現れない)。
        let mut game = Game::new(71);
        clear_board(&mut game);
        game.player.row = 50;
        game.player.col = 5;
        game.board.rows[10][5] = Cell::Item(ItemEffect::ClearAbove);
        game.board.rows[11][5] = Cell::Item(ItemEffect::UnifyColors);
        game.board.rows[12][5] = Cell::Item(ItemEffect::StarifyScreen);
        game.board.rows[10][6] = Cell::Rock { hits: 2 }; // 比較用: アイテム以外は通常通り消える

        game.debug_clear_above_player();

        let entry_row = game.player.row - crate::constants::PLAYER_SCREEN_ROWS_ABOVE;
        let just_off_screen_row = entry_row - 1;
        assert_eq!(
            game.board.cell(just_off_screen_row, 5),
            Cell::Item(ItemEffect::ClearAbove),
            "元々一番浅かったRアイテムが画面のすぐ外側の先頭に詰め直されるはず"
        );
        assert_eq!(
            game.board.cell(just_off_screen_row - 1, 5),
            Cell::Item(ItemEffect::UnifyColors),
            "Cアイテムがその次(さらに浅い側)に詰め直されるはず"
        );
        assert_eq!(
            game.board.cell(just_off_screen_row - 2, 5),
            Cell::Item(ItemEffect::StarifyScreen),
            "Kアイテムがさらにその次に詰め直されるはず"
        );
        assert_eq!(
            game.board.cell(entry_row, 5),
            Cell::Empty,
            "この時点ではまだ画面内には現れていないはず"
        );
        assert_eq!(
            game.board.cell(10, 5),
            Cell::Empty,
            "アイテムは元の画面外の位置には残らないはず"
        );
        assert_eq!(
            game.board.cell(10, 6),
            Cell::Empty,
            "アイテム以外は通常通り消えるはず"
        );
    }

    #[test]
    fn debug_clear_above_player_shows_the_same_vanish_flash_as_auto_vanish() {
        // 頭上クリアでの消滅も、4連結自動消滅と同じフラッシュ演出を出す。
        let mut game = Game::new(71);
        clear_board(&mut game);
        game.player.row = 50;
        game.player.col = 5;
        game.board.rows[10][6] = Cell::Rock { hits: 2 };

        game.debug_clear_above_player();

        assert!(
            game.vanish_flash_progress((10, 6)).is_some(),
            "4連結自動消滅と同じ消滅フラッシュが出るはず"
        );
    }

    #[test]
    fn debug_fill_air_restores_oxygen_to_max_while_playing() {
        let mut game = Game::new(72);
        game.player.oxygen = 1.0;

        game.debug_fill_air();

        assert_eq!(game.player.oxygen, crate::constants::OXYGEN_MAX);
    }

    #[test]
    fn debug_fill_air_does_nothing_when_not_playing() {
        let mut game = Game::new_with_lives(72, 1);
        game.player.oxygen = 1.0;
        game.update(Duration::from_secs(1)); // 酸素切れ+ライフ1でミスさせる
        // 最後のライフでも「天に召される」演出を経てからGameOverになる(#257)。
        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));
        assert_eq!(game.status, GameStatus::GameOver);

        game.debug_fill_air();

        assert_eq!(game.player.oxygen, 0.0, "GameOver中は酸素を回復しない");
    }

    #[test]
    fn debug_starify_visible_screen_converts_rock_and_diamond_within_range_to_stars() {
        let mut game = Game::new(73);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[990][3] = Cell::Rock { hits: 2 };
        game.board.rows[995][4] = Cell::Diamond;

        game.debug_starify_visible_screen();

        assert!(matches!(game.board.cell(990, 3), Cell::Star { .. }));
        assert!(matches!(game.board.cell(995, 4), Cell::Star { .. }));
    }

    #[test]
    fn debug_starify_visible_screen_does_not_convert_cells_outside_the_visible_range() {
        let mut game = Game::new(73);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[900][3] = Cell::Rock { hits: 0 }; // STAR_VISIBLE_RANGE_ROWSより外

        game.debug_starify_visible_screen();

        assert!(
            matches!(game.board.cell(900, 3), Cell::Rock { .. }),
            "画面外のブロックは変化しないはず"
        );
    }

    #[test]
    fn debug_starify_visible_screen_leaves_color_and_oxygen_cells_untouched() {
        let mut game = Game::new(73);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[990][3] = Cell::Color(ColorKind::Red);
        game.board.rows[991][3] = Cell::Oxygen;

        game.debug_starify_visible_screen();

        assert!(matches!(
            game.board.cell(990, 3),
            Cell::Color(ColorKind::Red)
        ));
        assert!(matches!(game.board.cell(991, 3), Cell::Oxygen));
    }

    #[test]
    fn debug_starify_visible_screen_does_not_convert_shaking_cells() {
        // 揺れ中/落下中のブロックはスター化対象外。
        let mut game = Game::new(31);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        game.board.rows[498][5] = Cell::Rock { hits: 0 }; // 直下(499)が空=支えなし

        game.update(Duration::from_millis(game.block_fall_tick_ms())); // 1ティックで揺れ開始(まだ落下しない)
        assert!(game.is_cell_shaking(498, 5), "テスト前提: 揺れ中であること");

        game.debug_starify_visible_screen();

        assert!(
            matches!(game.board.cell(498, 5), Cell::Rock { .. }),
            "揺れ中のセルは変化しないはず"
        );
    }

    #[test]
    fn debug_starify_visible_screen_does_nothing_when_not_playing() {
        let mut game = Game::new_with_lives(73, 1);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.player.oxygen = 1.0;
        game.update(Duration::from_secs(1)); // 酸素切れ+ライフ1でミスさせる
        // 最後のライフでも「天に召される」演出を経てからGameOverになる(#257)。
        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));
        assert_eq!(game.status, GameStatus::GameOver);

        // GameOverになった後で改めて岩を置く(死亡時の頭上クリアの影響を受けずに、
        // starifyがGameOver中は何もしないことだけを確認するため)。
        game.board.rows[990][3] = Cell::Rock { hits: 0 };

        game.debug_starify_visible_screen();

        assert!(
            matches!(game.board.cell(990, 3), Cell::Rock { .. }),
            "GameOver中は変化しないはず"
        );
    }

    #[test]
    fn crush_flash_decays_to_inactive_after_crush_flash_duration() {
        let mut game = Game::new(31);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[998][5] = Cell::Color(ColorKind::Red); // プレイヤーの真上、支えなし

        // SHAKE_TICKSぶんの揺れ+落下の1ティックで押し潰しが発生する
        game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));
        assert!(game.crush_flash_active(), "押し潰し直後は演出が有効なはず");

        // ライフが残っているので「天に召される」演出(CRUSH_ASCEND_MS)が動く。
        // これが終わるまでは演出が有効なままのはず。
        game.update(Duration::from_millis(crate::constants::CRUSH_FLASH_MS + 10));
        assert!(
            game.crush_flash_active(),
            "天に召される演出(CRUSH_ASCEND_MS)がまだ終わっていないはず"
        );

        // CRUSH_ASCEND_MSぶん時間を進めると演出は終わる
        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));
        assert!(
            !game.crush_flash_active(),
            "CRUSH_ASCEND_MS経過後は演出が終わっているはず"
        );
    }

    #[test]
    fn crushed_on_the_last_life_plays_the_ascending_sequence_before_game_over() {
        // 最後のライフでの押し潰しも「天に召される」演出を見せてからGameOverへ進む
        // (#257。以前はライフ1のときだけ演出を飛ばして即GameOverにしていた)。
        let mut game = Game::new_with_lives(35, 1); // ライフ1(最後の1機)
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[998][5] = Cell::Color(ColorKind::Red); // プレイヤーの真上、支えなし

        let events = game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));

        assert_eq!(
            game.status,
            GameStatus::Playing,
            "演出中はまだGameOverにならないはず"
        );
        assert!(game.is_dying(), "「天に召される」演出が始まっているはず");
        assert!(game.crush_flash_active(), "「潰れた」見た目のままのはず");
        assert_eq!(game.player.lives, 1, "ライフ減算は演出完了まで遅延される");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::GameOverMiss { .. })),
            "ミス音は演出開始時に鳴らす: {events:?}"
        );

        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));

        assert_eq!(game.status, GameStatus::GameOver, "演出完了でGameOverへ");
        assert_eq!(game.player.lives, 0);
        assert!(
            !game.crush_flash_active(),
            "演出が終わっていればGameOverダイアログを表示できる"
        );
        assert_eq!(
            game.board.cell(999, 5),
            Cell::Empty,
            "押し潰したブロックは演出完了時に消えるはず"
        );
    }

    #[test]
    fn input_is_ignored_while_the_ascending_sequence_plays_on_the_last_life() {
        // 演出中(is_dying)は入力が凍結される。最後のライフでも同じ。
        let mut game = Game::new_with_lives(35, 1);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[998][5] = Cell::Color(ColorKind::Red); // プレイヤーの真上、支えなし
        game.board.rows[999][6] = Cell::Color(ColorKind::Blue); // 右隣: 掘削・移動の的

        game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));
        assert!(game.is_dying(), "前提: 演出中");
        let pos_before = game.player.position();

        game.apply_input(InputAction::MoveRight);
        game.apply_input(InputAction::MoveLeft);
        game.apply_input(InputAction::Drill);

        assert_eq!(game.player.position(), pos_before, "位置は変わらないはず");
        assert_eq!(
            game.board.cell(999, 6),
            Cell::Color(ColorKind::Blue),
            "掘削も効かないはず"
        );
    }

    #[test]
    fn pausing_during_the_ascending_sequence_on_the_last_life_defers_the_game_over() {
        // 一時停止中はupdateがPlaying以外で打ち切られるため演出も進まない。
        // 再開後に続きから進み、最後にGameOverへ到達する。
        let mut game = Game::new_with_lives(35, 1);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[998][5] = Cell::Color(ColorKind::Red); // プレイヤーの真上、支えなし

        game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));
        assert!(game.is_dying(), "前提: 演出中");
        let remaining_before = game.ascending_remaining;

        game.toggle_pause();
        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));

        assert_eq!(
            game.ascending_remaining, remaining_before,
            "一時停止中は演出が進まないはず"
        );
        assert_eq!(game.status, GameStatus::Paused);

        game.toggle_pause();
        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));

        assert_eq!(game.status, GameStatus::GameOver, "再開後に演出が完了する");
        assert_eq!(game.player.lives, 0);
    }

    #[test]
    fn debug_add_life_does_nothing_while_the_ascending_sequence_plays() {
        // 演出の完了時にライフを減らして復活/GameOverを分岐するため、演出中に
        // ライフを増やせてしまうと本来のGameOverが復活側へ倒れる(#257)。
        let mut game = Game::new_with_lives(35, 1);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[998][5] = Cell::Color(ColorKind::Red); // プレイヤーの真上、支えなし

        game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));
        assert!(game.is_dying(), "前提: 演出中");

        game.debug_add_life();

        assert_eq!(game.player.lives, 1, "演出中はライフが増えないはず");

        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));
        assert_eq!(game.status, GameStatus::GameOver);
    }

    #[test]
    fn ascending_sequence_freezes_gameplay_until_it_completes() {
        // 「天に召される」演出中はゲームプレイ全体(入力・重力・自由落下)を凍結する。
        let mut game = Game::new_with_lives(36, 2); // ライフ2、押し潰されても即GameOverにならない
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[998][5] = Cell::Color(ColorKind::Red); // プレイヤーの真上、支えなし
        // (999,4)はclear_board済みでEmptyのまま=フリーズしていなければ普通に移動できる。

        game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));
        assert_eq!(game.player.lives, 2, "演出中はまだライフ減算前のはず");

        // 演出中は移動入力を受け付けない。
        let events = game.try_move_left();
        assert!(events.is_empty());
        assert_eq!(game.player.col, 5, "演出中は移動できないはず");

        // 演出が終われば通常のプレイに戻り、ライフも減っている。
        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));
        assert_eq!(game.player.lives, 1, "演出完了時にライフが減るはず");
        assert_eq!(game.status, GameStatus::Playing);
    }

    #[test]
    fn crush_death_se_event_fires_immediately_not_after_the_ascend_delay() {
        // 押し潰された瞬間に(天に召される演出の完了=3秒近く後を待たず)即座に
        // GameEvent::LifeLostが発火し、演出完了時には重複して発火しないことを確認する。
        let mut game = Game::new_with_lives(80, 2); // ライフ2、押し潰されても即GameOverにならない
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[998][5] = Cell::Color(ColorKind::Red); // プレイヤーの真上、支えなし

        let events = game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::LifeLost { .. })),
            "押し潰された直後(演出開始時点)にLifeLostが発火するはず"
        );
        assert_eq!(
            game.player.lives, 2,
            "この時点ではまだライフは減っていないはず(演出完了時に減る)"
        );

        let events = game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::LifeLost { .. })),
            "演出完了時に重複してLifeLostが発火してはいけない"
        );
        assert_eq!(game.player.lives, 1, "演出完了時にライフは減るはず");
    }

    #[test]
    fn revived_event_fires_exactly_when_the_ascend_animation_completes() {
        let mut game = Game::new_with_lives(80, 2);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[998][5] = Cell::Color(ColorKind::Red); // プレイヤーの真上、支えなし

        let events = game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));
        assert!(
            !events.iter().any(|e| matches!(e, GameEvent::Revived)),
            "押し潰された直後(演出開始時点)ではまだ復活していないはず"
        );

        let events = game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));
        assert!(
            events.iter().any(|e| matches!(e, GameEvent::Revived)),
            "演出完了時にRevivedが発火するはず"
        );
    }

    #[test]
    fn taking_air_from_under_a_block_does_not_cause_an_immediate_crush_it_shakes_first() {
        // AIRを取得して支えを失った直後も、通常の支え喪失と同様にSHAKE_TICKSぶん
        // 揺れてから落下するはずで、即座には押し潰されない。
        let mut game = Game::new(50);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.player.facing = Direction::Right;
        game.board.rows[999][6] = Cell::Oxygen; // 取得対象のAIR(プレイヤーと同じ高さ)
        game.board.rows[998][6] = Cell::Color(ColorKind::Red); // AIRの真上のブロック

        let events = game.try_move_right();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::OxygenCollected))
        );
        assert_eq!(game.player.col, 6, "AIRのマスへ移動しているはず");
        assert_eq!(
            game.player.lives, LIVES_DEFAULT,
            "移動しただけではまだ潰されていない"
        );

        // 支えを失った直後、SHAKE_TICKSぶんはまだ落下しない(押し潰されない)。
        game.update(Duration::from_millis(SHAKE_TICKS as u64 * FALL_TICK_MS));
        assert_eq!(
            game.player.lives, LIVES_DEFAULT,
            "揺れている間は押し潰されないはず"
        );
        assert_eq!(
            game.board.cell(998, 6),
            Cell::Color(ColorKind::Red),
            "まだ落下していない"
        );

        // 揺れが明けた次のティックで初めて落下し、押し潰される。
        game.update(Duration::from_millis(FALL_TICK_MS + 10));
        assert_eq!(
            game.player.lives, LIVES_DEFAULT,
            "押し潰し直後は「天に召される」演出中でまだライフ減算前のはず"
        );

        // 演出が終わるとライフが減る。
        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));
        assert_eq!(
            game.player.lives,
            LIVES_DEFAULT - 1,
            "演出後に押し潰されるはず"
        );
    }

    // --- 掘削アニメーション(9章) ---

    #[test]
    fn drilling_frame_is_none_before_any_drill_input() {
        let game = Game::new(60);
        assert_eq!(
            game.drilling_frame(),
            None,
            "掘削していなければアニメーションフレームは無いはず"
        );
    }

    #[test]
    fn drilling_frame_alternates_then_clears_after_drill_anim_duration() {
        // 掘削入力直後はアニメーションフレームが交互に切り替わり、DRILL_ANIM_MS
        // 経過後は通常表示(None)に戻ることを確認する。
        let mut game = Game::new(61);
        clear_board(&mut game);
        game.player.row = 5;
        game.player.col = 5;

        game.try_drill();
        assert_eq!(
            game.drilling_frame(),
            Some(true),
            "掘削直後は最初のフレームのはず"
        );

        game.update(Duration::from_millis(DRILL_ANIM_FRAME_MS));
        assert_eq!(
            game.drilling_frame(),
            Some(false),
            "1フレーム経過で切り替わるはず"
        );

        game.update(Duration::from_millis(
            DRILL_ANIM_MS - DRILL_ANIM_FRAME_MS + 10,
        ));
        assert_eq!(
            game.drilling_frame(),
            None,
            "DRILL_ANIM_MS経過後は通常表示に戻るはず"
        );
    }

    // --- ヒヤリ回避スライダー演出(9章) ---

    #[test]
    fn dodge_slide_triggers_only_when_fleeing_a_block_that_was_actually_shaking_overhead() {
        // 単に「最近動いた」だけでなく、移動前の頭上が実際に揺れていた(=本物の脅威から
        // 逃げた)場合にのみスライダー演出が発火することを確認する。
        let mut game = Game::new(62);
        clear_board(&mut game);
        game.player.row = 5;
        game.player.col = 5;
        game.board.rows[6][4] = Cell::Rock { hits: 0 }; // 移動先(5,4)の真下=足場(player_is_grounded用)
        game.board.rows[6][5] = Cell::Rock { hits: 0 }; // 現在地(5,5)の真下=足場
        game.board.rows[4][5] = Cell::Color(ColorKind::Red); // 現在地の真上、支えなし(プレイヤーがまだ居る間から揺れ始める)

        game.update(Duration::from_millis(FALL_TICK_MS)); // 1ティック目: 揺れ始める(まだ落下しない)

        game.try_move_left(); // (5,5) -> (5,4)へ移動。移動前の頭上(4,5)が揺れているため監視対象になる
        assert_eq!(game.player.position(), (5, 4), "左へ1マス移動しているはず");

        // 残りの揺れティックの間はまだ発火しない。
        for _ in 1..SHAKE_TICKS {
            game.update(Duration::from_millis(FALL_TICK_MS));
            assert!(!game.is_dodge_sliding(), "揺れている間はまだ発火しないはず");
        }

        assert!(
            !game.is_dodge_sliding(),
            "落下前はまだスライダー演出は発火していないはず"
        );
        let events = game.update(Duration::from_millis(FALL_TICK_MS + 10)); // 揺れが明けて実際に落下するティック

        assert!(
            game.is_dodge_sliding(),
            "旧位置へ実際に脅威だったブロックが着地したので発火するはず"
        );
        assert_eq!(
            game.board.cell(5, 5),
            Cell::Color(ColorKind::Red),
            "ブロックは旧位置(5,5)へ着地しているはず"
        );
        assert_eq!(
            game.status,
            GameStatus::Playing,
            "プレイヤー自身は無事なはず"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::DodgeTriggered)),
            "ユーザー指摘: 「キャラがスライディングした瞬間...専用SEを鳴らす」。発動と同時に\
             GameEvent::DodgeTriggeredが発火するはず"
        );
    }

    #[test]
    fn dodge_slide_does_not_trigger_for_an_unrelated_move_with_no_threat_overhead() {
        // 頭上に何も脅威が無い、ただの通常移動ではスライダー演出は発火しないことを
        // 確認する(誤発動対策)。
        let mut game = Game::new(64);
        clear_board(&mut game);
        game.player.row = 5;
        game.player.col = 5;
        game.board.rows[6][4] = Cell::Rock { hits: 0 };
        game.board.rows[6][5] = Cell::Rock { hits: 0 };
        // 頭上(4,5)には何も置かない = 脅威なし

        game.try_move_left();
        assert_eq!(game.player.position(), (5, 4));

        game.update(Duration::from_millis(FALL_TICK_MS * 10));
        assert!(
            !game.is_dodge_sliding(),
            "脅威が無かった移動ではスライダー演出は発火しないはず"
        );
    }

    #[test]
    fn dodge_freeze_lifts_after_dodge_slide_ms_and_dodge_recovery_ms_elapse() {
        // スライダー演出(Sliding)→硬直(Recovering)の間は入力を凍結し、両方経過すれば
        // 通常通り入力が通ることを確認する。
        let mut game = Game::new(63);
        clear_board(&mut game);
        game.set_dodge_recovery_ms(300);
        game.player.row = 5;
        game.player.col = 5;
        game.board.rows[6][4] = Cell::Rock { hits: 0 }; // 移動先(5,4)の真下=足場(player_is_grounded用)
        game.board.rows[6][5] = Cell::Rock { hits: 0 }; // 現在地(5,5)の真下=足場
        game.board.rows[4][5] = Cell::Color(ColorKind::Red);

        game.update(Duration::from_millis(FALL_TICK_MS)); // 揺れ始める
        game.try_move_left();
        for _ in 1..SHAKE_TICKS {
            game.update(Duration::from_millis(FALL_TICK_MS));
        }
        game.update(Duration::from_millis(FALL_TICK_MS + 10)); // 揺れが明けて落下・発火
        assert!(game.is_dodge_sliding(), "スライダー演出が発火しているはず");

        game.player.facing = Direction::Down; // 目印としてfacingを固定しておく
        game.face_up(); // フリーズ中はfacingが変わらないはず
        assert_eq!(
            game.player.facing,
            Direction::Down,
            "スライダー中は入力を凍結しているはず"
        );

        // tick_dodgeは呼び出しごとにSliding/Recoveringのどちらか一方の残時間しか消費しない
        // (超過分は次の段階へ繰り越さない)ため、Sliding→Recoveringの遷移をまず1回、
        // その後Recovering→Noneの遷移をもう1回、と分けて経過させる。
        game.update(Duration::from_millis(DODGE_SLIDE_MS + 20)); // Sliding -> Recovering
        game.update(Duration::from_millis(300 + 20)); // Recovering -> None

        game.face_up();
        assert_eq!(
            game.player.facing,
            Direction::Up,
            "硬直が明ければ再び入力が通るはず"
        );
    }

    // --- 移動の見た目補間アニメーション(9章) ---

    #[test]
    fn new_game_starts_with_move_animation_already_settled() {
        // 開始直後にいきなり(0,0)相当からアニメーションしてしまわないことの確認。
        let game = Game::new(32);
        assert_eq!(game.move_anim_progress(), 1.0);
        assert_eq!(game.render_prev_position(), game.player.position());
    }

    #[test]
    fn debug_frame_starts_at_zero_and_increments_once_per_update_call() {
        // refresh_debug_logを呼ばない限りdisk I/Oは発生しない(debug_logがNoneのまま
        // no-opになる)ので、通常のテストには影響しない。
        let mut game = Game::new(1);
        assert_eq!(game.debug_frame(), 0);
        game.update(Duration::from_millis(16));
        assert_eq!(game.debug_frame(), 1);
        game.update(Duration::from_millis(16));
        assert_eq!(game.debug_frame(), 2);
    }

    #[test]
    fn new_with_width_generates_a_board_of_the_requested_width_and_centers_the_player() {
        // 指定した列数で盤面が生成され、プレイヤーの開始列もその幅の中央に
        // 合わせ直されることを確認する。
        let game = Game::new_with_width(33, 8, FIELD_DEPTH_M);
        assert_eq!(game.board.width(), 8);
        for row in &game.board.rows {
            assert_eq!(row.len(), 8, "各行の長さも指定した列数と一致するはず");
        }
        assert_eq!(game.player.col, 4);
    }

    #[test]
    fn new_with_width_clamps_out_of_range_values() {
        let too_narrow = Game::new_with_width(34, 1, FIELD_DEPTH_M);
        assert_eq!(too_narrow.board.width(), crate::constants::FIELD_WIDTH_MIN);

        let too_wide = Game::new_with_width(34, 999, FIELD_DEPTH_M);
        assert_eq!(too_wide.board.width(), crate::constants::FIELD_WIDTH_MAX);
    }

    #[test]
    fn each_checkpoint_is_followed_by_an_empty_gap_but_not_an_empty_ground() {
        // 各チェックポイント(100mごと)の地面(CHECKPOINT_SAFE_ZONE_M)を掘り抜いた直後、
        // その先のスキマ(CHECKPOINT_ZONE_GAP_M)は完全にEmptyになる一方、地面区間そのもの
        // はくり抜かれず残ることを確認する(500mのボーナスフロアは例外なので対象外)。
        let mut game = Game::new(200);
        for checkpoint_depth_m in (crate::constants::CHECKPOINT_STEP_M
            ..crate::constants::FIELD_DEPTH_M)
            .step_by(crate::constants::CHECKPOINT_STEP_M)
        {
            if checkpoint_depth_m == crate::constants::BONUS_FLOOR_DEPTH_M {
                continue; // ボーナスフロアは別テストで確認する
            }
            let ground_start = checkpoint_depth_m;
            let ground_end = (ground_start + crate::constants::CHECKPOINT_SAFE_ZONE_M)
                .min(game.board.depth_rows());
            let ground_all_empty_before = (ground_start..ground_end)
                .flat_map(|row| (0..game.board.width()).map(move |col| (row, col)))
                .all(|(row, col)| game.board.cell(row, col) == Cell::Empty);
            assert!(
                !ground_all_empty_before,
                "checkpoint={checkpoint_depth_m}m: くり抜き前の地面はまだ生の地形が残っているはず"
            );

            game.apply_checkpoint_safe_zone(checkpoint_depth_m);

            let ground_all_empty_after = (ground_start..ground_end)
                .flat_map(|row| (0..game.board.width()).map(move |col| (row, col)))
                .all(|(row, col)| game.board.cell(row, col) == Cell::Empty);
            assert!(
                !ground_all_empty_after,
                "checkpoint={checkpoint_depth_m}m: 地面区間はくり抜き後も強制的には空にならないはず"
            );

            let gap_start = ground_end;
            let gap_end =
                (gap_start + crate::constants::CHECKPOINT_ZONE_GAP_M).min(game.board.depth_rows());
            for row in gap_start..gap_end {
                for col in 0..game.board.width() {
                    assert_eq!(
                        game.board.cell(row, col),
                        Cell::Empty,
                        "checkpoint={checkpoint_depth_m}m: row={row} col={col}はスキマとして空のはず"
                    );
                }
            }
        }
    }

    #[test]
    fn a_gap_follows_the_ground_zone_before_normal_terrain_resumes() {
        // 安全地帯(地面ビジュアル区間、CHECKPOINT_SAFE_ZONE_M)の直後、さらに
        // CHECKPOINT_ZONE_GAP_Mぶんも空になっていることを確認する。
        let mut game = Game::new(202);
        game.apply_checkpoint_safe_zone(100);
        let width = game.board.width();
        let gap_start = 100 + crate::constants::CHECKPOINT_SAFE_ZONE_M;
        let gap_end = gap_start + crate::constants::CHECKPOINT_ZONE_GAP_M;
        for row in gap_start..gap_end {
            for col in 0..width {
                assert_eq!(
                    game.board.cell(row, col),
                    Cell::Empty,
                    "地面区間直後のスキマ(row={row}, col={col})は空のはず"
                );
            }
        }
    }

    #[test]
    fn the_gap_after_the_bonus_floor_is_also_cleared() {
        // 500mボーナスフロア自体はアイテム/AIRを意図的に配置するが、その直後のスキマは
        // 通常のチェックポイントと同様に空けるはず。
        let mut game = Game::new(203);
        game.apply_checkpoint_safe_zone(crate::constants::BONUS_FLOOR_DEPTH_M);
        let width = game.board.width();
        let gap_start =
            crate::constants::BONUS_FLOOR_DEPTH_M + crate::constants::CHECKPOINT_SAFE_ZONE_M;
        let gap_end = gap_start + crate::constants::CHECKPOINT_ZONE_GAP_M;
        for row in gap_start..gap_end {
            for col in 0..width {
                assert_eq!(
                    game.board.cell(row, col),
                    Cell::Empty,
                    "ボーナスフロア直後のスキマ(row={row}, col={col})は空のはず"
                );
            }
        }
    }

    #[test]
    fn debris_that_lands_inside_an_already_carved_checkpoint_zone_is_purged_next_tick() {
        // 既に到達済みのチェックポイントのスキマ区間に何か入り込んでも、次の重力tickで
        // 消滅フラッシュ演出付きでパージされることを確認する(地面部分は強制的に
        // くり抜かないため、パージ対象はスキマのみ)。
        let mut game = Game::new(204);
        clear_board(&mut game);
        game.player.row = 1;
        game.player.col = 0;
        game.last_checkpoint_reported = 1; // checkpoint 100mまで到達済み扱い

        // 本来は既にEmptyのはずのスキマ区間(105-110)に、崩れてきた想定のブロックと
        // アイテムを直接置く。
        game.board.rows[106][2] = Cell::Color(ColorKind::Red);
        game.board.rows[107][3] = Cell::Item(ItemEffect::ClearAbove);

        game.update(Duration::from_millis(FALL_TICK_MS));

        assert_eq!(
            game.board.cell(106, 2),
            Cell::Empty,
            "スキマ内に滞留したブロックはパージされるはず"
        );
        assert_eq!(
            game.board.cell(107, 3),
            Cell::Empty,
            "スキマ内に滞留したアイテムもパージされるはず"
        );
    }

    #[test]
    fn purge_does_not_touch_the_bonus_floor_zone() {
        // 500mボーナスフロアはアイテム/AIRを意図的に配置する区間なので、
        // パージの対象外であることを確認する。
        let mut game = Game::new(205);
        clear_board(&mut game);
        game.player.row = 1;
        game.player.col = 0;
        game.last_checkpoint_reported = 5; // 500mまで到達済み扱い

        game.board.rows[502][2] = Cell::Oxygen;

        game.update(Duration::from_millis(FALL_TICK_MS));

        assert_eq!(
            game.board.cell(502, 2),
            Cell::Oxygen,
            "ボーナスフロア内のアイテムはパージされないはず"
        );
    }

    #[test]
    fn the_500m_bonus_floor_has_a_noticeably_higher_oxygen_density_than_a_normal_band() {
        // 500mチェックポイント直後の帯はAIR(酸素カプセル、出現数に上限が無い)の密度が、
        // 同じ幅の通常の帯より明らかに高いはず。くり抜きはチェックポイントを踏んだ瞬間に
        // 行うため、`apply_checkpoint_safe_zone`を呼んでからボーナスフロアを判定する。
        let mut game = Game::new(201);
        game.apply_checkpoint_safe_zone(crate::constants::BONUS_FLOOR_DEPTH_M);
        let width = game.board.width();
        let bonus_start = crate::constants::BONUS_FLOOR_DEPTH_M;
        let bonus_end = bonus_start + crate::constants::CHECKPOINT_SAFE_ZONE_M;
        let count_oxygen = |from: usize, to: usize| {
            (from..to)
                .flat_map(|row| (0..width).map(move |col| (row, col)))
                .filter(|&(row, col)| game.board.cell(row, col) == Cell::Oxygen)
                .count()
        };
        let bonus_oxygen = count_oxygen(bonus_start, bonus_end);
        // 比較用の通常の帯(直前のチェックポイントの安全地帯明け、400m直後は
        // 別のチェックポイントの安全地帯なので避け、300m付近の通常区間を使う)。
        let normal_oxygen = count_oxygen(320, 320 + crate::constants::CHECKPOINT_SAFE_ZONE_M);
        assert!(
            bonus_oxygen > normal_oxygen,
            "ボーナスフロアのAIR密度({bonus_oxygen})は通常区間({normal_oxygen})より明らかに高いはず"
        );
    }

    #[test]
    fn lateral_move_starts_interpolation_from_the_previous_position_then_settles() {
        let mut game = Game::new(33);
        // 開始直後の上2行は常にEmptyなので、直下に足場を置いて横移動できる状態にする。
        game.board.rows[game.player.row + 1][game.player.col] = Cell::Rock { hits: 0 };
        let before = game.player.position();

        let events = game.try_move_right();
        assert!(events.is_empty());
        assert_ne!(
            game.player.position(),
            before,
            "前提: 実際に移動しているはず"
        );

        assert_eq!(
            game.render_prev_position(),
            before,
            "補間の起点は移動前の位置のはず"
        );
        assert!(
            game.move_anim_progress() < 1.0,
            "移動直後は補間がまだ完了していないはず"
        );

        game.update(Duration::from_millis(
            crate::constants::MOVE_ANIM_DURATION_MS + 10,
        ));
        assert_eq!(
            game.move_anim_progress(),
            1.0,
            "MOVE_ANIM_DURATION_MS経過後は補間が完了しているはず"
        );
    }

    // --- ショートカットC: 2色化+結合再計算 ---

    #[test]
    fn debug_unify_nearby_colors_repaints_to_exactly_two_colors_and_never_vanishes() {
        // ランダムな2色のみへ塗り替えるが、塗り替えによって新たに4連結以上になった箇所が
        // あっても即座には自動消滅させない(色の選択はシードから決まるため、シードを変えて
        // 十分な回数試行し両方の性質を確認する)。
        let mut saw_four_or_more_connected_and_intact = false;
        for trial in 0..300 {
            let mut game = Game::new(trial);
            clear_board(&mut game);
            game.player.row = 500;
            game.player.col = 5;
            game.board.rows[500][0] = Cell::Color(ColorKind::Red);
            game.board.rows[500][1] = Cell::Color(ColorKind::Blue);
            game.board.rows[500][2] = Cell::Color(ColorKind::Green);
            game.board.rows[500][3] = Cell::Color(ColorKind::Yellow);

            let events = game.debug_unify_nearby_colors();

            assert!(
                events.is_empty(),
                "塗り替えだけで自動消滅イベントは発生しないはず"
            );

            let mut colors_seen: Vec<ColorKind> = Vec::new();
            for c in 0..4 {
                if let Cell::Color(k) = game.board.cell(500, c)
                    && !colors_seen.contains(&k)
                {
                    colors_seen.push(k);
                }
            }
            assert!(
                colors_seen.len() <= 2,
                "2色より多い色が残っている: {colors_seen:?}"
            );
            for c in 0..4 {
                assert_ne!(
                    game.board.cell(500, c),
                    Cell::Empty,
                    "塗り替えただけで消滅してはいけない"
                );
            }

            if colors_seen.len() == 1 {
                saw_four_or_more_connected_and_intact = true;
                break;
            }
        }
        assert!(
            saw_four_or_more_connected_and_intact,
            "300回試行しても4連結が形成されるケースを確認できなかった"
        );
    }

    // -----------------------------------------------------------------------
    // 落下tick間隔を遅くした際の「落下→消滅」演出
    // -----------------------------------------------------------------------

    /// テスト用ヘルパー: 盤面を3行に切り詰め、最深行(row2)を常に支持される足場にした上で、
    /// 「(0,0)の色ブロックが2マス落下し、着地先(2,0)で(2,1)(2,2)(2,3)と4連結して消滅する」
    /// 盤面を作る。落下tick間隔だけをパラメータで変えて、演出の破綻を比較できるようにする。
    fn landing_vanish_game(block_fall_tick_ms: u64) -> Game {
        let mut game = Game::new(1);
        game.set_block_fall_tick_ms(block_fall_tick_ms);
        game.board.rows.truncate(3);
        clear_board(&mut game);
        game.player.row = 0;
        game.player.col = 5; // 落下グループから十分離す
        game.board.rows[0][0] = Cell::Color(ColorKind::Red);
        for col in 1..=3 {
            game.board.rows[2][col] = Cell::Color(ColorKind::Red);
        }
        game
    }

    /// テスト用ヘルパー: `landing_vanish_game`の盤面を1フレーム(33ms)ずつ進め、
    /// 落下ブロックが着地して4連結消滅するまで到達させる。消滅の判定には静止セル(2,1)を
    /// 使う(このセルは自分では動かないため、Emptyになるのは4連結消滅した時だけ)。
    fn advance_to_landing_vanish(game: &mut Game) {
        let frame = Duration::from_millis(FRAME_INTERVAL_MS);
        for _ in 0..400 {
            game.update(frame);
            if game.board.cell(2, 1) == Cell::Empty {
                return;
            }
        }
        panic!("着地と同一tickでの4連結消滅が起きなかった");
    }

    #[test]
    fn landing_block_keeps_its_look_until_the_fall_interpolation_finishes_at_any_tick_rate() {
        // 落下tick間隔を遅くすると、消滅フラッシュの寿命が1tickぶんの落下補間より短くなり、
        // 落下中のブロックが空中で消えてしまう。どのtick間隔でも「補間が終わるまでは消滅
        // 直前の見た目を保持している」ことを確認する。
        let frame = Duration::from_millis(FRAME_INTERVAL_MS);
        for tick_ms in [25u64, 150, 300, 450, 600] {
            let mut game = landing_vanish_game(tick_ms);
            let mut vanished = false;
            let mut checked_frames = 0;

            for _ in 0..400 {
                game.update(frame);
                // 着地セルが盤面から消えている(4連結消滅済み)のに、まだ落下補間が
                // 続いている間は、消滅直前の見た目を保持していなければならない。
                let still_falling = game
                    .recently_moved_blocks()
                    .iter()
                    .any(|&(to, _)| to == (2, 0));
                if still_falling && game.board.cell(2, 0) == Cell::Empty {
                    assert!(
                        game.pending_vanish_kind((2, 0)).is_some(),
                        "tick={tick_ms}ms: 落下補間中(progress={})に着地セルの見た目が失われている",
                        game.block_fall_progress()
                    );
                    checked_frames += 1;
                }
                if game.board.cell(2, 1) == Cell::Empty {
                    vanished = true;
                    if !still_falling {
                        break;
                    }
                }
            }

            assert!(
                vanished,
                "tick={tick_ms}ms: 着地と同一tickでの4連結消滅が起きなかった"
            );
            // 1フレーム(33ms)より短いtickでは落下tickがフレーム内で完結してしまい、
            // 「補間の途中」をフレーム境界で観測できない。それ以外は必ず観測できるはず。
            if tick_ms > FRAME_INTERVAL_MS {
                assert!(
                    checked_frames > 0,
                    "tick={tick_ms}ms: 落下補間中のフレームを1つも検証できていない"
                );
            }
        }
    }

    #[test]
    fn vanish_flash_starts_only_after_the_falling_block_has_arrived() {
        // 着地tickの瞬間にフラッシュを始めると、まだ空中にいるブロックの着地先が先に光る。
        // 着地セル・静止セルとも、落下補間が終わる(次のtickが来る)まではフラッシュに
        // 入らないことを確認する。
        let mut game = landing_vanish_game(300);
        advance_to_landing_vanish(&mut game);

        let frame = Duration::from_millis(FRAME_INTERVAL_MS);
        let mut waited_frames = 0;
        while game.vanish_flash_progress((2, 0)).is_none() {
            assert!(
                game.pending_vanish_kind((2, 0)).is_some(),
                "フラッシュ前の着地セルは待機中(消滅直前の見た目)であるはず"
            );
            assert!(
                game.vanish_flash_progress((2, 1)).is_none(),
                "一緒に消える静止セルも、落下ブロックが着くまでは光り始めないはず"
            );
            game.update(frame);
            waited_frames += 1;
            assert!(
                waited_frames < 40,
                "フラッシュが始まらないまま待ち続けている"
            );
        }

        assert!(
            waited_frames >= 2,
            "tick=300msなら着地から数フレームは待機するはず(実際は{waited_frames}フレーム)"
        );
        assert!(
            game.vanish_flash_progress((2, 0)).unwrap() < 0.3,
            "フラッシュは始まったばかりのはず"
        );
        assert!(
            game.vanish_flash_progress((2, 1)).is_some(),
            "静止セルも同じタイミングでフラッシュに入るはず"
        );
        assert!(
            game.pending_vanish_kind((2, 0)).is_none(),
            "フラッシュに入ったら待機中ではなくなるはず"
        );
    }

    #[test]
    fn vanish_flash_duration_scales_with_the_effective_fall_tick() {
        // フラッシュの長さを基準tick(FALL_TICK_MS)での`BLOCK_VANISH_FLASH_MS`から実効tickに
        // 比例させ、遅いtickでは伸ばす。短すぎて視認できなくならないよう下限を持つ。
        let mut game = Game::new(1);

        game.set_block_fall_tick_ms(FALL_TICK_MS);
        let at_default = game.vanish_flash_duration_ms() as i64;
        assert!(
            (at_default - BLOCK_VANISH_FLASH_MS as i64).abs() <= 3,
            "既定tickでは従来どおり約{BLOCK_VANISH_FLASH_MS}msのはず(実際は{at_default}ms)"
        );

        game.set_block_fall_tick_ms(600);
        let at_slowest = game.vanish_flash_duration_ms() as i64;
        assert!(
            (at_slowest - 800).abs() <= 5,
            "tick=600msでは4倍の約800msへ伸びるはず(実際は{at_slowest}ms)"
        );

        game.set_block_fall_tick_ms(DEBUG_FALL_TICK_MS_MIN);
        assert_eq!(
            game.vanish_flash_duration_ms(),
            BLOCK_VANISH_FLASH_MIN_MS,
            "最速tickでは比例値が下限(3フレーム)を下回るため下限で止まるはず"
        );
    }

    #[test]
    fn shake_ticks_never_drops_to_zero_while_a_shake_duration_is_set() {
        // 揺れtick数は整数除算のため、tick間隔が揺れ時間を超えると0になり「予兆なしで
        // いきなり落ちる」。揺れ時間が設定されている限り最低1tickは揺れる。
        let mut game = Game::new(1);
        for tick_ms in [150u64, 300, 450, 500, 600] {
            game.set_block_fall_tick_ms(tick_ms);
            assert!(
                game.shake_ticks() >= 1,
                "tick={tick_ms}ms: 揺れ時間が設定されているのに揺れtickが0になっている"
            );
        }

        game.set_block_fall_tick_ms(FALL_TICK_MS);
        assert_eq!(
            game.shake_ticks(),
            SHAKE_TICKS,
            "既定tickでは従来どおりの揺れtick数のはず"
        );

        game.set_shake_duration_ms(0);
        assert_eq!(game.shake_ticks(), 0, "揺れ時間0なら揺れないはず");
    }

    #[test]
    fn shake_ticks_uses_the_depth_adjusted_tick_everywhere_it_is_needed() {
        // 揺れtick数の換算は1つの関数(`shake_ticks`)に統一してある。深度補正前の生の
        // `block_fall_tick_ms`で換算すると重力tick側の基準と食い違うため、深度が進んでも
        // 両者が同じ値を見ることを確認する。
        let mut game = Game::new(1);
        game.player.row = 999; // 最深部=実効tickが最大まで短縮される
        game.set_block_fall_tick_ms(FALL_TICK_MS);

        let effective = game.effective_block_fall_tick_ms();
        assert!(
            effective < FALL_TICK_MS,
            "テスト前提: 深度によって実効tickが短縮されていること"
        );
        assert_eq!(
            game.shake_ticks(),
            (game.shake_duration_ms() / effective) as u8,
            "揺れtick数は深度補正後の実効tickで換算するはず"
        );
    }

    #[test]
    fn a_chain_pause_tick_finalizes_the_previous_fall_interpolation() {
        // 連鎖インターバルで足止めするtickが`last_block_moves`を更新しないまま次のtickへ
        // 進むと、足止め中に前tickの落下補間が0から再生され、着地済みのブロックが
        // 巻き戻って見える。
        let mut game = Game::new(5);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.chain_vanish_interval_ms = 300;
        game.chain_pause_remaining = Duration::from_millis(300);
        game.last_block_moves = vec![((10, 1), (9, 1))];

        game.update(Duration::from_millis(game.effective_block_fall_tick_ms()));

        assert!(
            game.recently_moved_blocks().is_empty(),
            "足止めtickが来た時点で前tickの補間は完了として確定させるはず"
        );
    }

    #[test]
    fn fall_interpolation_keeps_advancing_after_game_over() {
        // `update`が`status != Playing`で単純に早期returnすると、押し潰しでGameOverに
        // なった瞬間の落下補間が途中で凍り付き、押し潰したブロックが空中に止まったまま
        // フラッシュへ移ってしまう。
        let mut game = Game::new(1);
        game.set_block_fall_tick_ms(300);
        game.status = GameStatus::GameOver;
        assert!(
            game.block_fall_progress() < 0.2,
            "テスト前提: 補間が始まったばかりであること"
        );

        for _ in 0..12 {
            game.update(Duration::from_millis(FRAME_INTERVAL_MS));
        }
        assert_eq!(
            game.block_fall_progress(),
            1.0,
            "GameOver後も落下補間は着地位置まで進み切るはず"
        );
    }

    #[test]
    fn fall_interpolation_stays_frozen_while_paused() {
        // 一時停止中まで補間を進めると、再開時に大きなdeltaがまとめて来てtickが飛ぶ。
        // GameOver(見た目を最後まで見せる)とは区別し、Pausedでは止めたままにする。
        let mut game = Game::new(1);
        game.set_block_fall_tick_ms(300);
        game.status = GameStatus::Paused;
        let before = game.block_fall_progress();

        for _ in 0..12 {
            game.update(Duration::from_millis(FRAME_INTERVAL_MS));
        }
        assert_eq!(
            game.block_fall_progress(),
            before,
            "一時停止中は落下補間も止まったままのはず"
        );
    }

    /// #247のテスト用に、盤面を空にして妨害ルールを有効化し、ボムの乱入を止めた状態を作る。
    /// プレイヤーは最深行(=常に支持される)へ置き、自由落下で出現行がずれないようにする。
    /// `state_hash`モジュールのテスト(#262)からも参照するため`pub(super)`にする。
    pub(super) fn attack_rules_game(seed: u64) -> Game {
        let mut game = Game::new(seed);
        clear_board(&mut game);
        game.set_bomb_spawn_rate_percent(0);
        game.player.row = game.board.depth_rows() - 1;
        game.player.col = 0;
        game.set_attack_rules_enabled(true);
        game
    }
}

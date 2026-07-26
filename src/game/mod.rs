//! ゲーム全体のオーケストレーション(盤面+プレイヤー+タイマー類)。
//!
//! board/player/physics は副作用のない純粋なロジックだが、この`Game`はそれらを
//! 「1フレーム進める」「1回入力を処理する」という時間軸に沿ってまとめ、UI/audio層が
//! 反応すべき`GameEvent`列を返す薄いオーケストレーション層。

pub mod board;
pub mod physics;
pub mod player;

use std::time::Duration;

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;

use crate::constants::{
    BLOCK_VANISH_FLASH_MS, BOMB_BLAST_COL_RANGE, BOMB_BLAST_ROW_RANGE, BOMB_ENTER_MS,
    BOMB_EXPLOSION_FLASH_MS, BOMB_FUSE_MS, BOMB_MAX_COUNT_ON_BOARD, BOMB_ROLL_MS, BOMB_SETTLE_MS,
    BOMB_SETTLE_TICK_MS, BOMB_SPAWN_BASE_PROB, BOMB_SPAWN_CHECK_INTERVAL_MS,
    BOMB_SPAWN_DEPTH_MAX_BONUS, CRUSH_ASCEND_MS, CRUSH_FLASH_MS, DEBUG_FALL_TICK_MS_MAX,
    DEBUG_FALL_TICK_MS_MIN, DEBUG_FALL_TICK_STEP_MS, DEBUG_SHAKE_DURATION_MS_MAX,
    DEBUG_SHAKE_DURATION_MS_MIN, DEBUG_SHAKE_DURATION_STEP_MS, DEBUG_UNIFY_COLORS_RANGE_ROWS,
    DODGE_DETECT_WINDOW_MS, DODGE_RECOVERY_MS_DEFAULT, DODGE_RECOVERY_MS_MAX,
    DODGE_RECOVERY_MS_MIN, DODGE_SLIDE_MS, DRILL_ANIM_FRAME_MS, DRILL_ANIM_MS,
    FALL_SPEED_DEPTH_MAX_SPEEDUP, FALL_TICK_MS, FIELD_DEPTH_M, FIELD_WIDTH_DEFAULT,
    FIELD_WIDTH_MAX, FIELD_WIDTH_MIN, INPUT_COOLDOWN_ACCUM_CAP_MS, INPUT_COOLDOWN_MS,
    INVULNERABILITY_TICKS, LIVES_DEFAULT, LIVES_MAX, MOVE_ANIM_DURATION_MS,
    MOVE_COOLDOWN_MS_DEFAULT, MOVE_COOLDOWN_MS_MAX, MOVE_COOLDOWN_MS_MIN,
    OXYGEN_DECAY_DEPTH_MAX_MULTIPLIER, OXYGEN_WARNING_THRESHOLD, SHAKE_DURATION_MS,
    STAR_VISIBLE_RANGE_ROWS, depth_fraction,
};
use board::{
    BlockMove, Board, Cell, ColorKind, GravityState, ItemEffect, bomb_blast_cells,
    connected_same_color, tick_star_melting,
};
use physics::{DrillOutcome, FreeFallOutcome, LateralOutcome};
use player::{Direction, Player};

use crate::debug_log::DebugLog;

/// ボムの演出段階(TERM独自拡張。#123。ユーザー指摘: 「白ボンが画面の外から
/// とことこやってきて、日のついた爆弾をぼーんとなげてこんこんころころ...ってなって、
/// 爆発する」)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BombPhase {
    /// 白ボンが画面端(`Bomb::origin`)に登場し、ボムを投げる直前までの間。
    Entering,
    /// 投げられたボムが`origin`から`pos`(最終設置マス)まで転がっている間。
    Rolling,
    /// 転がり終えた直後、支えを失っていれば落下しつつ、左右に跳ねながら
    /// 落ち着き先を探している間(TERM独自拡張。#140。ユーザー指摘: 「落ちたら、
    /// またはねまくること左右に壁をぶつかり行き来しながらいいところで泊まる」)。
    Settling,
    /// 静止し、点滅しながら起爆までカウントダウンしている間
    /// (`remaining_ms`はこの段階でのみ減る。支えを失った場合はここでも落下を
    /// 続け、落下中は`remaining_ms`の減少を止める。#140)。
    Ticking,
}

/// 白ボンがランダムに投げ込むボム(TERM独自拡張。#96。ユーザー指摘: 「白ボンが、
/// 爆弾をランダムに投げてくるイメージで、敵は出現しないものとする」)。移動する
/// 敵キャラは持たず、盤面上に設置されたこのボム自体だけを管理する。ブロックとは
/// 別レイヤーのオブジェクトなので`Cell`列挙体には追加しない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bomb {
    /// 現在位置(落下・跳ねで`origin`/初期の`pos`から動くことがある。#140)。
    pub pos: board::Pos,
    /// 白ボンが登場する画面端の位置(同じ行、列0か列`width-1`)。
    pub origin: board::Pos,
    pub phase: BombPhase,
    /// 現在の`phase`に入ってからの経過時間(ms)。
    pub phase_elapsed_ms: u32,
    /// 起爆までの残り時間(ms)。`BombPhase::Ticking`に入って初めて減り始める。
    pub remaining_ms: u32,
    /// `BombPhase::Settling`中に左右へ跳ねる方向(+1=右、-1=左。TERM独自拡張。#140)。
    pub settle_bounce_dir: i8,
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
    /// MUSIC(BGM)のON/OFF切り替え(TERM独自拡張)。一時停止画面でのみ意味を持つ。
    /// Gameの内部状態には影響しないため、この解釈もGameの外側=main.rsが担う
    ToggleMusic,
    /// SE(効果音)のON/OFF切り替え(TERM独自拡張)。一時停止画面でのみ意味を持つ。
    ToggleSe,
    /// デバッグ: プレイヤー付近のブロックを2色に統一する(TERM独自拡張、動作確認用ショートカット)
    DebugUnifyNearbyColors,
    /// デバッグ: ライフを1増やす(TERM独自拡張、動作確認用ショートカット)
    DebugAddLife,
    /// デバッグ: 酸素(AIR)を100%まで回復する(TERM独自拡張、動作確認用ショートカット。
    /// ユーザー指摘: 「AIRを100%にするショートカット追加」)
    DebugFillAir,
    /// デバッグ: プレイヤーより浅い(画面上で上にある)ブロックを全削除する
    /// (TERM独自拡張、動作確認用ショートカット)
    DebugClearAbovePlayer,
    /// デバッグ: 画面内のXブロック・ダイヤブロックを全てスターブロックに変える
    /// (TERM独自拡張、動作確認用ショートカット。ユーザー指摘: 「画面内をスター化
    /// する(Xブロック,ダイヤブロック100%)」)
    DebugStarifyVisibleScreen,
    /// デバッグ: ボムを1個、画面内のランダムなEmptyマスへ即座に設置する(TERM独自拡張、
    /// 動作確認用ショートカット。#96。ユーザー指摘: 「ショートカットキーもくれ」)
    DebugPlaceBomb,
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
    /// Enterキー(TERM独自拡張)。タイトル画面からの開始・GameOverダイアログでの選択
    /// 確定は、このキーでのみ行う(ユーザー指摘: 「メニューから進むのEnter」「ゲーム
    /// オーバーなってメニュー設計するのEnter」「他のボタンで進んではいけない」)。
    Confirm,
    /// どのショートカットにも割り当てられていないキー(TERM独自拡張)。ユーザー指摘:
    /// 「ポーズ解除は、Pだけじゃなく、ショートカット設定されていない任意のキー入力でも
    /// 解除されるように」。一時停止中(オーバーレイ非表示時)に限り、Pキーと同様に
    /// 再開のトリガーとして扱う。Gameの内部状態には影響しないため、この解釈も
    /// Gameの外側=main.rsが担う
    UnboundKey,
}

/// ゲーム全体の進行状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameStatus {
    Playing,
    Paused,
    GameOver,
    Cleared,
}

/// 「わ〜!」スライダー演出(TERM独自拡張)の段階。ブロックが落ち始める直前に
/// 移動して間一髪回避した際、まずスライダー(横滑り)で見せてから、短い硬直
/// (`dodge_recovery_ms`)を挟んで通常操作へ戻る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DodgeStage {
    /// 演出無し(通常プレイ中)。
    None,
    /// スライダー(横滑り)演出中。
    Sliding,
    /// スライダー後、起き上がるまでの短い硬直中。
    Recovering,
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
    /// ブロックが消滅した(色ブロックの直接掘削消滅・自動消滅・スター消滅のいずれも。
    /// spec.md 10章「破壊音」)。消滅したブロック数を伴う。岩ブロックの消滅は専用の
    /// `RockDestroyed`を使う(TERM独自拡張。ユーザー指摘: 「Xブロックを壊したときに
    /// 専用SEを鳴らす」)
    BlockDestroyed { blocks: usize },
    /// 岩ブロック(Xブロック)が消滅した(直接掘削の5回目破壊・自動消滅のいずれも。
    /// TERM独自拡張)。消滅したブロック数を伴う
    RockDestroyed { blocks: usize },
    /// ヒヤリ回避スライダー演出が発動した瞬間(TERM独自拡張。ユーザー指摘:
    /// 「キャラがスライディングした瞬間...専用SEを鳴らす」)
    DodgeTriggered,
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
    /// 「天に召される」演出が終わり、その場に復活した瞬間(TERM独自拡張。ユーザー指摘:
    /// 「死んで、復活したときのSEほしい」)
    Revived,
    /// 最後のライフを失い、ゲームオーバーになった
    GameOverMiss,
    /// 深度1000m到達でゲームクリアした
    Cleared,
    /// アイテムブロックを取得し、対応する効果が発動した(TERM独自拡張。ユーザー指摘:
    /// 「ショートカットRと同じ効果のあるアイテムつくろ」「ショートカットC効果の
    /// アイテムも作って」)
    ItemCollected(ItemEffect),
    /// ボムが爆発した(TERM独自拡張。#96)。プレイヤーが爆風に巻き込まれたかどうかは
    /// 別途`LifeLost`/`GameOverMiss`が続けて発生するかで判断できる。
    BombExploded,
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
    /// 横移動(MoveLeft/MoveRight)のクールダウン間隔(ms、小さいほど速い)。既定は
    /// `MOVE_COOLDOWN_MS_DEFAULT`(`INPUT_COOLDOWN_MS`相当)だが、設定画面から調整
    /// できる(TERM独自拡張。ユーザー指摘: 「横移動のスピードを設定で変えられるように」)。
    /// 掘削(Drill)のクールダウンは対象外で、引き続き`INPUT_COOLDOWN_MS`固定のまま。
    move_cooldown_ms: u64,
    /// 移動系入力(MoveLeft/MoveRight)専用のクールダウン。掘削(Drill)とは別に管理する
    /// (TERM独自拡張。ユーザー指摘: 「カーソルとスペース、両方押してるときにどちらかが
    /// 効かない」。1つの共有クールダウンだと、同一フレームで移動キーと掘削キーが両方
    /// 来た場合に片方がブロックされてしまうため分離した)。
    ///
    /// 「前回の入力受理からの経過時間」を毎フレーム蓄積するアキュムレータとして持つ
    /// (`fall_tick_accum`等と同じ考え方)。`INPUT_COOLDOWN_MS`ぶん貯まったら入力を
    /// 受理し、そのぶんだけ差し引く(0へリセットしない)。これにより、ターミナルの
    /// キーリピート間隔とクールダウン周期が一致しない場合に生じる「一定間隔で移動が
    /// 遅くなって見える」ビート(うなり)を軽減する(TERM独自拡張。ユーザー指摘:
    /// 「左右にキャラ走るとき、速くなったり遅くなったりしてる。一定のインターバルで
    /// 速度が落ちたりする」)。ただし長時間入力が無い間に際限なく貯め込んで後から
    /// 連続入力がまとめて即座に通ってしまわないよう、`INPUT_COOLDOWN_ACCUM_CAP_MS`
    /// (クールダウン自体の1.5倍)で上限を設ける。
    move_cooldown_accum: Duration,
    /// 掘削系入力(Drill)専用のクールダウンアキュムレータ。移動(MoveLeft/MoveRight)
    /// とは別に管理する。`move_cooldown_accum`と同じ考え方。
    drill_cooldown_accum: Duration,
    oxygen_warning_accum: Duration,
    /// ライフ消費で再開した直後、残り何ティックの間 押し潰し判定を無効化するか
    /// (spec.md 5章末尾、TERM独自拡張)。
    invulnerability_ticks_remaining: u32,
    /// 直近でGameEvent::LevelUpを通知した時点のレベル番号(重複通知防止)。
    last_level_reported: usize,
    /// 押し潰しミス発生時、残りこれだけの間「潰れた」見た目を表示し続ける
    /// (0になったらGameOverオーバーレイの表示を許す。TERM独自拡張、9章)。
    crush_flash_remaining: Duration,
    /// 押し潰されてもライフが残っている場合、「天に召される」演出の残り時間
    /// (TERM独自拡張)。`Some`の間はゲームプレイ全体(重力・自由落下・酸素減少・入力)
    /// を凍結し、0になった時点で死亡地点の3列クリア・ライフ減算・酸素回復をまとめて
    /// 行いその場に復活する(ユーザー指摘: 「潰れたとき、もっとわかりやすいように
    /// 死んで、一度天に召される演出をして、ブロックが消える処理されてから、元の位置に
    /// 復活」)。ライフが0になる場合はこの演出を行わず、即座にGameOverへ進む。
    ascending_remaining: Option<Duration>,
    /// 掘削入力(Space)を押した直後、方向別の掘削アニメーションを表示し続ける残り時間
    /// (TERM独自拡張、9章。ユーザー指摘: 「上に掘る時、上向きながらピヨンピヨン跳ねる」
    /// 「左右に掘る時、横にドリルをぐいぐい」「下に掘る時、下向きながらドリルを
    /// ぐいぐい」)。描画専用で、ロジックには一切影響しない。
    drill_flash_remaining: Duration,
    /// 「わ〜!」スライダー演出(ブロックが落ち始める直前に移動して間一髪回避した際、
    /// TERM独自拡張)の現在の段階。`None`なら演出無し。
    dodge_stage: DodgeStage,
    /// 現在の段階(スライダー/硬直)の残り時間。
    dodge_stage_remaining: Duration,
    /// 「わ〜!」スライダー直後の硬直インターバル(ms、TERM独自拡張)。設定画面/
    /// デバッグショートカットで調整できる(ユーザー指摘: 「この設定値も作る」)。
    dodge_recovery_ms: u64,
    /// ヒヤリ回避スライダーの監視対象セル(TERM独自拡張)。直前の移動で、移動前の
    /// 頭上(row-1)が実際に「揺れていた」場合のみ、その移動前の座標を監視対象として
    /// 設定する(ユーザー指摘: 「そもそも避けてないのに発動してるように見える」を
    /// 受け、単に「最近動いた」だけでなく実際に頭上の脅威から逃げたことを条件にする)。
    /// この座標へブロックが着地した瞬間にスライダー演出を発火し、監視は解除される。
    dodge_watch_cell: Option<(usize, usize)>,
    /// 監視対象セルの有効期限(TERM独自拡張)。揺れていたブロックが実際に落下して
    /// 監視対象セルへ到達するまでの猶予。この時間が経過すると監視は自動的に解除される。
    dodge_watch_remaining: Duration,
    /// 描画専用: プレイヤーの直前の論理位置(移動の見た目補間アニメーション用、
    /// TERM独自拡張、9章)。ロジック上の当たり判定・掘削・落下判定には一切使わない。
    render_prev_position: (usize, usize),
    /// 直前の論理位置変化からの経過時間(秒)。`MOVE_ANIM_DURATION_MS`に達すると
    /// 補間が完了したものとして扱う。
    render_anim_elapsed: f32,
    /// 現在進行中の移動補間アニメーションの長さ(秒、TERM独自拡張)。横移動は
    /// `move_anim_duration_secs()`(固定の短い時間)、自由落下は`player_fall_tick_ms`
    /// (実際の落下速度)を使う。移動の種類によって`note_possible_move_with_duration`が設定する。
    render_anim_duration_secs: f32,
    /// 直近の重力ティックで実際に1マス落下した各セルの(移動後の位置, 移動前の位置)
    /// (TERM独自拡張。ブロック落下のピクセル単位補間描画に使う)。次のティックが
    /// 来るまでの間、描画側がこれと`block_fall_progress()`を使って補間する。
    last_block_moves: Vec<BlockMove>,
    /// 直近に消滅した(自動消滅・スター溶解)セルの座標と、消滅フラッシュ演出の残り時間
    /// (TERM独自拡張。ユーザー指摘: 「ブロックが消える瞬間に消える演出してほしい」)。
    /// 描画側(render.rs)がこの座標に一瞬フラッシュ演出を出す。
    recently_vanished: Vec<(board::Pos, Duration)>,
    /// ボム爆発の爆風が届いた直後のセルと、炎の演出の残り時間・爆心地からの距離
    /// (TERM独自拡張。#126。ユーザー指摘: 「爆弾が爆発するときは、ボンバーマンTERMの
    /// ように炎アニメーションほしい」)。距離(0=爆心地、遠いほど大きい)で炎の色調を
    /// 変え、`recently_vanished`と同じ考え方で描画側(render.rs)がフラッシュ演出に使う。
    recently_exploded: Vec<(board::Pos, Duration, u8)>,
    /// GameOverダイアログでの現在の選択項目(TERM独自拡張)。GameOver状態でのみ意味を持つ。
    game_over_selection: GameOverChoice,
    /// `update()`が呼ばれるたびに1増えるフレーム通し番号(TERM独自拡張。#85調査用。
    /// ユーザー指摘: 「フレームのユニーク番号を取得できるようにしておき」)。
    /// ブロック状態遷移ログ(`debug_log`)の各行と突き合わせるための識別子。
    frame_counter: u64,
    /// #85調査用のブロック状態遷移ログ(TERM独自拡張)。`refresh_debug_log`で明示的に
    /// 有効化するまでは`None`(no-op)のままなので、通常のテスト等では disk I/O が
    /// 発生しない。
    debug_log: Option<DebugLog>,
    /// 現在盤面上にあるボム(TERM独自拡張。#96)。
    bombs: Vec<Bomb>,
    /// ボム出現判定の経過時間蓄積(TERM独自拡張。#96)。`BOMB_SPAWN_CHECK_INTERVAL_MS`
    /// ぶん貯まるたびに1回、出現確率を判定する。
    bomb_spawn_check_accum_ms: u64,
    /// ボム出現頻度設定(%、100=既定、TERM独自拡張。#96)。設定画面から調整できる。
    bomb_spawn_rate_percent: u32,
    /// ボム出現位置・確率判定専用の乱数生成器(TERM独自拡張。#96)。ゲームのシードから
    /// 派生させるため、同じシードなら同じ出現パターンが再現できる(既存の盤面生成と
    /// 同じ決定性の考え方)。
    rng: ChaCha8Rng,
}

impl Game {
    /// 指定シードで、既定ライフ数・既定フィールド幅の新しいゲームを開始する。
    pub fn new(seed: u64) -> Self {
        Self::new_with_lives(seed, LIVES_DEFAULT)
    }

    /// 指定シード・ライフ数で、既定フィールド幅の新しいゲームを開始する
    /// (spec.md 8章「1〜5機から選べる」)。
    pub fn new_with_lives(seed: u64, lives: u8) -> Self {
        Self::new_with_lives_and_width(seed, lives, FIELD_WIDTH_DEFAULT)
    }

    /// 指定シード・フィールド幅で、既定ライフ数の新しいゲームを開始する(TERM独自拡張。
    /// ユーザー指摘: 「設定値に列の数を変更できるようにして」)。
    pub fn new_with_width(seed: u64, width: usize) -> Self {
        Self::new_with_lives_and_width(seed, LIVES_DEFAULT, width)
    }

    /// 指定シード・ライフ数・フィールド幅で新しいゲームを開始する(TERM独自拡張。
    /// ユーザー指摘: 「設定値に列の数を変更できるようにして」)。範囲外の値は
    /// `FIELD_WIDTH_MIN`〜`MAX`にクランプする。
    pub fn new_with_lives_and_width(seed: u64, lives: u8, width: usize) -> Self {
        let width = width.clamp(FIELD_WIDTH_MIN, FIELD_WIDTH_MAX);
        let mut player = Player::with_lives(lives);
        player.recenter_for_width(width);
        let last_level_reported = player.level();
        let start_position = player.position();
        Game {
            board: Board::generate(seed, FIELD_DEPTH_M, width),
            player,
            status: GameStatus::Playing,
            gravity_state: GravityState::new(),
            fall_tick_accum: Duration::ZERO,
            player_fall_tick_accum: Duration::ZERO,
            block_fall_tick_ms: FALL_TICK_MS,
            player_fall_tick_ms: FALL_TICK_MS,
            shake_duration_ms: SHAKE_DURATION_MS,
            move_cooldown_ms: MOVE_COOLDOWN_MS_DEFAULT,
            // ゲーム開始直後は即座に入力を受理できるよう、アキュムレータを満タン
            // (=1クールダウンぶん貯まっている状態)から始める。
            move_cooldown_accum: Duration::from_millis(MOVE_COOLDOWN_MS_DEFAULT),
            drill_cooldown_accum: Duration::from_millis(INPUT_COOLDOWN_MS),
            oxygen_warning_accum: Duration::ZERO,
            invulnerability_ticks_remaining: 0,
            last_level_reported,
            crush_flash_remaining: Duration::ZERO,
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
            // ボード生成(`Board::generate`)とは別系統の乱数列にするため、シードを
            // ビット反転して使う(TERM独自拡張。#96)。同じゲームシードなら同じボム
            // 出現パターンが再現される。
            rng: ChaCha8Rng::seed_from_u64(!seed),
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
        if !self.push_bomb_in_the_way(dir) {
            // 押し出せなかった(押し出し先が塞がっている)ので、壁にぶつかった
            // 時と同じ扱いでこの場に留まる(TERM独自拡張。#149)。
            return Vec::new();
        }

        let before = self.player.position();
        let outcome = physics::move_lateral(&mut self.board, &mut self.player, dir);
        self.note_possible_move(before);

        match outcome {
            LateralOutcome::MovedLevelAndCollectedOxygen
            | LateralOutcome::ClimbedStepAndCollectedOxygen => {
                vec![GameEvent::OxygenCollected]
            }
            LateralOutcome::MovedLevelAndCollectedItem(effect)
            | LateralOutcome::ClimbedStepAndCollectedItem(effect) => {
                let mut events = Vec::new();
                self.apply_item_effect(effect, &mut events);
                events
            }
            _ => Vec::new(),
        }
    }

    /// 移動先セルに静止中(Settling/Ticking、まだ登場・投擲演出中のEntering/Rolling
    /// は対象外)のボムがあれば、進行方向へさらに1マス押し出す(TERM独自拡張。#149。
    /// ユーザー指摘: 「爆弾はキャラが押したらそっちに転がる」)。押し出したボムは
    /// Settling(左右バウンド中)へ遷移させ、以後は既存の重力・バウンド判定
    /// (#140/#143/#144)にそのまま委ねる。押し出し先が盤面外・ブロック・他の
    /// ボムで塞がっていて動かせない場合は`false`を返し、呼び出し側で移動そのものを
    /// 妨げる(壁にぶつかった時と同じ扱い)。ボムが無い、またはまだ登場・投擲演出中
    /// であれば何もせず`true`を返す(移動を妨げない)。
    fn push_bomb_in_the_way(&mut self, dir: Direction) -> bool {
        let (_, dc) = dir.delta();
        let nc = self.player.col as isize + dc;
        if nc < 0 || nc as usize >= self.board.width() {
            return true; // 盤面外は既存の境界チェック(physics::move_lateral)に任せる。
        }
        let nc = nc as usize;
        let row = self.player.row;

        let Some(bomb_index) = self.bombs.iter().position(|b| {
            b.pos == (row, nc) && matches!(b.phase, BombPhase::Settling | BombPhase::Ticking)
        }) else {
            return true;
        };

        let push_c = nc as isize + dc;
        if push_c < 0 || push_c as usize >= self.board.width() {
            return false;
        }
        let push_pos = (row, push_c as usize);
        let occupied_by_other_bomb = self
            .bombs
            .iter()
            .enumerate()
            .any(|(i, b)| i != bomb_index && b.pos == push_pos);
        if self.board.cell(push_pos.0, push_pos.1) != Cell::Empty || occupied_by_other_bomb {
            return false;
        }

        let bomb = &mut self.bombs[bomb_index];
        bomb.pos = push_pos;
        bomb.phase = BombPhase::Settling;
        bomb.phase_elapsed_ms = 0;
        bomb.settle_bounce_dir = if dc > 0 { 1 } else { -1 };
        true
    }

    /// ↑ キー: facingをUpに変更するのみ(移動・掘削は発生しない。spec.md 1章)。
    ///
    /// Left/Rightの2ステップ段差登り(TERM独自拡張)における「ぶつかって停止中」の
    /// 状態もリセットする(方向キーを挟んだ場合の扱い)。
    pub fn face_up(&mut self) {
        if self.status == GameStatus::Playing && !self.is_input_frozen() {
            self.player.facing = Direction::Up;
            self.player.bumped_direction = None;
        }
    }

    /// ↓ キー: facingをDownに変更するのみ(移動・掘削は発生しない。spec.md 1章)。
    ///
    /// Left/Rightの2ステップ段差登り(TERM独自拡張)における「ぶつかって停止中」の
    /// 状態もリセットする(方向キーを挟んだ場合の扱い)。
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

        // 掘削入力そのものに反応して、方向別のアニメーション(TERM独自拡張、9章)を
        // 開始する。命中/空振りを問わず、入力があった事実に対して反応する。
        self.drill_flash_remaining = Duration::from_millis(DRILL_ANIM_MS);

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
        if self.status != GameStatus::Playing || self.is_input_frozen() {
            return false;
        }
        let slot = Duration::from_millis(self.move_cooldown_ms);
        if self.move_cooldown_accum < slot {
            return false;
        }
        // 0へリセットせず、消費した1スロットぶんだけ差し引く。ターミナルのキー
        // リピートがちょうどクールダウン周期をわずかに過ぎたタイミングで届いた
        // 場合、その超過ぶんは次のスロットへ繰り越される(TERM独自拡張)。
        self.move_cooldown_accum -= slot;
        true
    }

    /// 掘削系入力(Drill)のクールダウンが明けているかを確認し、明けていればリセットする。
    /// 移動(MoveLeft/MoveRight)とは独立したクールダウン(TERM独自拡張)。
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
            DrillOutcome::CrushedByUnstableOverhead => self.apply_miss(events),
            DrillOutcome::ItemUntouchedByDrill => {}
        }
    }

    /// アイテムブロックの効果を実際に発動し、対応するイベントを追加する(TERM独自拡張。
    /// AIRと同様「触れるだけで取得」のため、横移動・自由落下・重力ティックでの落下
    /// 着地、いずれの取得経路からも共通で呼ばれる。ユーザー指摘: 「アイテムはAIRと
    /// 同じ用に掘らなくても取得でき、上から振ってきても死なないように」)。
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
            self.apply_miss(events);
        }
    }

    /// ミス(酸素切れ/押し潰し)を処理する(spec.md 8章)。原因を問わず全く同じ処理を行う
    /// (TERM独自拡張。ユーザー指摘: 「AIR不足で死んだときもブロックにつぶされたときと
    /// 同じ処理」。以前は押し潰しのみ「潰れた」フラッシュ・「天に召される」演出付きで、
    /// 酸素切れは即座に処理する別扱いだったが、統一した)。
    ///
    /// ライフが残っていれば「天に召される」演出(`ascending_remaining`、TERM独自拡張)から
    /// 開始し、死亡地点の3列クリア・ライフ減算・酸素回復は演出が終わるまで`update()`側で
    /// 遅延させる(ユーザー指摘: 「潰れたとき、もっとわかりやすいように死んで、一度天に
    /// 召される演出をして、ブロックが消える処理されてから、元の位置に復活」)。ライフが
    /// 0になる場合はこの演出を行わず、従来通り即座にGameOverダイアログへ進む
    /// (ユーザー指摘: 「livesが0になったときはただちにゲームオーバーのダイアログ出てOK」)。
    fn apply_miss(&mut self, events: &mut Vec<GameEvent>) {
        self.crush_flash_remaining = Duration::from_millis(CRUSH_FLASH_MS);

        if self.player.lives <= 1 {
            self.clear_three_columns_above_player();
            let game_over = self.player.lose_life();
            debug_assert!(game_over, "lives<=1のはずなのでlose_lifeは必ずtrueを返す");
            self.status = GameStatus::GameOver;
            self.game_over_selection = GameOverChoice::BackToTitle;
            events.push(GameEvent::GameOverMiss);
            return;
        }

        // ライフ減算・酸素回復自体は演出完了まで遅延する(tick_ascending)が、
        // 死亡SEはミスが発生した瞬間に即座に鳴らす(TERM独自拡張。ユーザー指摘:
        // 「キャラが死んだとき(AIR不足/つぶされたとき)しんだときのSE鳴らして
        // ほしい」。演出完了まで3秒近く無音だったバグの修正)。
        events.push(GameEvent::LifeLost);
        self.ascending_remaining = Some(Duration::from_millis(CRUSH_ASCEND_MS));
    }

    /// 「天に召される」演出(TERM独自拡張)の進行を1フレームぶん進める。演出中は
    /// `is_input_frozen`経由でプレイヤー自身の入力・自由落下・酸素減少だけが凍結され、
    /// 周囲の他の落下ブロックの重力処理は止めない(ユーザー指摘: 「潰れた瞬間も
    /// まわりの落下アニメーションを止めない」)。演出が終わった瞬間、死亡地点の3列
    /// クリア・押し潰したブロック自体のクリア・ライフ減算・酸素回復をまとめて行い、
    /// その場に復活する。
    fn tick_ascending(&mut self, delta: Duration, events: &mut Vec<GameEvent>) {
        let Some(remaining) = self.ascending_remaining else {
            return;
        };
        let remaining = remaining.saturating_sub(delta);
        if remaining == Duration::ZERO {
            self.ascending_remaining = None;
            self.clear_three_columns_above_player();
            // 押し潰したブロック自体は演出中ずっと見えるようにその場に残していた
            // (ユーザー指摘: 「潰れる直前で消えてしまう」「潰した様子が認識できる
            // ように」)。復活するのでここで消す。
            self.board
                .set(self.player.row, self.player.col, Cell::Empty);
            let game_over = self.player.lose_life();
            debug_assert!(
                !game_over,
                "ライフ0のケースはapply_missで即座に処理済みのはず"
            );
            self.invulnerability_ticks_remaining = INVULNERABILITY_TICKS;
            // GameEvent::LifeLost(死亡SE)は押し潰された瞬間にapply_missで既に
            // 発火済みのため、ここでは重複して発火しない。復活した瞬間のSEは
            // ここで発火する(TERM独自拡張。ユーザー指摘: 「死んで、復活したときの
            // SEほしい」)。
            events.push(GameEvent::Revived);
        } else {
            self.ascending_remaining = Some(remaining);
        }
    }

    /// 「わ〜!」スライダー演出(TERM独自拡張)の進行を1フレームぶん進める。演出中
    /// (スライダー/硬直のいずれか)は`is_input_frozen`経由で入力のみが凍結され、
    /// 重力・自由落下・酸素減少は通常通り進み続ける(ユーザー指摘: 「ゲーム全体が
    /// 止まってるように見える」)。
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

    /// プレイヤーの現在列を中心に左右1列ずつ(=3列分)、プレイヤーより浅い
    /// (画面上で上にある)行を全てEmptyにする(TERM独自拡張)。ただしAIR(酸素
    /// カプセル)は消滅させずその場に残す(ユーザー指摘: 「キャラが死んだとき
    /// (AIR不足/つぶされたとき)...AIRは消えずに上から落下してくるように」)。
    /// 周囲がEmptyになれば通常の重力tickが未支持と判定して自然に落下させるため、
    /// ここでは単に上書きを避けるだけでよい。
    fn clear_three_columns_above_player(&mut self) {
        let col = self.player.col;
        let col_start = col.saturating_sub(1);
        let col_end = (col + 1).min(self.board.width() - 1);
        for row in 0..self.player.row {
            for c in col_start..=col_end {
                if !matches!(self.board.cell(row, c), Cell::Oxygen) {
                    self.board.set(row, c, Cell::Empty);
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
        }

        if self.status == GameStatus::Playing && self.player.depth_m() >= FIELD_DEPTH_M {
            self.status = GameStatus::Cleared;
            events.push(GameEvent::Cleared);
        }
    }

    /// メインループから毎フレーム呼ぶ。deltaぶんの時間経過(酸素減少・落下tick)を反映する。
    pub fn update(&mut self, delta: Duration) -> Vec<GameEvent> {
        let mut events = Vec::new();
        self.frame_counter += 1;

        // このフレームのブロック変化ログをまとめて1トランザクションにし(TERM独自拡張。
        // ユーザー指摘: 「あとちょっともっさりしてるからinsert高速化したい」)、
        // キャラの位置・向き・ステータスもフレームに1回だけ記録する(TERM独自拡張。
        // ユーザー指摘: 「どういう種類のブロックがっていう情報とキャラの向きや位置、
        // ステータスって残ってないと思うけど大丈夫？」)。
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
        self.render_anim_elapsed += delta.as_secs_f32();
        for (_, remaining) in self.recently_vanished.iter_mut() {
            *remaining = remaining.saturating_sub(delta);
        }
        self.recently_vanished
            .retain(|&(_, remaining)| remaining > Duration::ZERO);
        for (_, remaining, _) in self.recently_exploded.iter_mut() {
            *remaining = remaining.saturating_sub(delta);
        }
        self.recently_exploded
            .retain(|&(_, remaining, _)| remaining > Duration::ZERO);

        if self.status != GameStatus::Playing {
            return events;
        }

        // 「天に召される」演出中(TERM独自拡張)は、プレイヤー自身の入力・自由落下・
        // 酸素減少のみを凍結する。周囲の他の落下ブロックの重力処理は止めない
        // (ユーザー指摘: 「潰れた瞬間もまわりの落下アニメーションを止めない」)。
        // 演出が終わった時点でのブロッククリア・ライフ減算・酸素回復はtick_ascending内で行う。
        //
        // 演出が完了したかどうかは、この呼び出し**前**の状態で判定して以降の処理に使う
        // (`was_dying`)。tick_ascending呼び出し後にis_dying()を都度見てしまうと、演出が
        // ちょうどこのフレームで完了した場合、余った経過時間ぶんが「復活直後のプレイヤー」
        // へその場でさらに酸素減少・クールダウン加算として二重に適用されてしまう
        // (演出完了時に酸素を全回復させた直後、同じフレーム内で減衰させてしまうバグ)。
        let was_dying = self.is_dying();
        self.tick_ascending(delta, &mut events);

        // 「わ〜!」スライダー演出中(TERM独自拡張)は入力のみを凍結する(is_input_frozen
        // が各入力ハンドラで担う)。ユーザー指摘: 「ゲーム全体が止まってるように見える」
        // を受け、周囲の重力・自由落下・酸素減少は止めない。
        self.tick_dodge(delta);

        // ヒヤリ回避スライダーの監視対象セル(TERM独自拡張)の有効期限を進める。
        // 揺れていたブロックが監視対象セルへ実際に落下する前に期限が切れたら監視解除する。
        if self.dodge_watch_cell.is_some() {
            self.dodge_watch_remaining = self.dodge_watch_remaining.saturating_sub(delta);
            if self.dodge_watch_remaining == Duration::ZERO {
                self.dodge_watch_cell = None;
            }
        }

        // 「天に召される」演出中は、プレイヤー自身に関する経過処理(酸素減少・
        // クールダウン)だけを凍結する。演出がこのフレームで完了した場合も、
        // 復活直後の二重減衰を避けるため`was_dying`(呼び出し前の状態)で判定し、
        // このフレームでは通常処理を再開しない(次のフレームから再開する)。
        if !was_dying {
            self.player.elapsed_seconds += delta.as_secs_f32();

            // 移動クールダウンは設定で変えられるため、上限も現在の値の1.5倍で都度計算する
            // (TERM独自拡張。ユーザー指摘: 「横移動のスピードを設定で変えられるように」)。
            // 掘削クールダウンは引き続き固定値なので、既存の定数上限のままでよい。
            let move_accum_cap =
                Duration::from_millis(self.move_cooldown_ms + self.move_cooldown_ms / 2);
            let drill_accum_cap = Duration::from_millis(INPUT_COOLDOWN_ACCUM_CAP_MS);
            self.move_cooldown_accum = (self.move_cooldown_accum + delta).min(move_accum_cap);
            self.drill_cooldown_accum = (self.drill_cooldown_accum + delta).min(drill_accum_cap);
            self.drill_flash_remaining = self.drill_flash_remaining.saturating_sub(delta);

            // 深度が進むほど酸素の自然減少が速くなる(TERM独自拡張。ユーザー指摘:
            // 「進むにつれてAIRの減る速度が早い」)。経過時間そのものを実効倍率ぶん
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
            self.apply_miss(&mut events);
            if self.status != GameStatus::Playing {
                return events;
            }
        }

        // 深度が進むほどブロック落下速度が上がる(TERM独自拡張。ユーザー指摘:
        // 「階層が進むにつれてだんだんとブロックの落ちる速度があがり」)。設定画面/
        // デバッグショートカットで調整した`block_fall_tick_ms`を「深度0mでの速度」
        // として扱い、そこから深度に応じてtick間隔を短縮する。
        let effective_tick_ms = self.effective_block_fall_tick_ms();
        self.fall_tick_accum += delta;
        let tick = Duration::from_millis(effective_tick_ms);
        while self.fall_tick_accum >= tick {
            self.fall_tick_accum -= tick;

            // 「天に召される」演出中(TERM独自拡張)も重力処理自体は止めないため、
            // プレイヤーの論理位置は演出完了まで押し潰された地点に固定されたままになる。
            // その間に別の塊が同じ地点へ落ちてきても二重にライフを失わないよう、
            // 演出中は無敵として扱う(既存の`invulnerability_ticks_remaining`と同じ仕組み)。
            let invulnerable = self.invulnerability_ticks_remaining > 0 || self.is_dying();
            let shake_ticks =
                (self.shake_duration_ms / effective_tick_ms.max(1)).min(u8::MAX as u64) as u8;
            let result = physics::process_gravity_tick(
                &mut self.board,
                &mut self.player,
                &mut self.gravity_state,
                invulnerable,
                shake_ticks,
            );
            // `invulnerable`は「天に召される」演出中(is_dying)にも真になるが、その場合
            // `invulnerability_ticks_remaining`自体は0のままなので、実際にカウンタが
            // 動いている場合のみ減算する(0からの減算でオーバーフローするのを防ぐ)。
            if self.invulnerability_ticks_remaining > 0 {
                self.invulnerability_ticks_remaining -= 1;
            }

            // ブロックが落ち始める直前に移動して間一髪回避した場合、「わ〜!」スライダー
            // 演出を発火する(TERM独自拡張。ユーザー指摘: 「ブロックが落ち始める直前に
            // 移動してにげたとき、「わ〜!」ってスライダー(アニメーションしてねキャラ)
            // して切り間に合う感じ」)。`dodge_watch_cell`は移動前の頭上が実際に揺れて
            // いた場合のみ設定されている(単に「最近動いた」だけでは発火しない。
            // ユーザー指摘: 「そもそも避けてないのに発動してるように見える」)ため、
            // その監視対象セルへちょうど今ブロックが着地した場合のみ発火する
            // (押し潰された場合や、既に演出中の場合は対象外)。
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
            }
            if result.auto_vanished_rock_blocks > 0 {
                // 岩ブロックの自動消滅は得点対象外だが、専用の破壊音を鳴らす
                // (spec.md 4.9・10章。TERM独自拡張。ユーザー指摘: 「Xブロックを
                // 壊したときに専用SEを鳴らす」)。
                events.push(GameEvent::RockDestroyed {
                    blocks: result.auto_vanished_rock_blocks,
                });
            }
            self.note_vanished_cells(result.vanished_cells);

            if result.life_lost_to_crush {
                self.apply_miss(&mut events);
            }

            if self.status != GameStatus::Playing {
                return events;
            }
        }

        // スターブロックの溶解は実時間(ms)で進む(TERM独自拡張。ユーザー指摘:
        // 「スターブロックは画面内に見えてから5秒たったら消えはじめること」)。
        // ブロック落下tick(深度に応じて間隔が変わる`effective_tick_ms`)とは切り離し、
        // このフレームの実経過時間`delta`そのもので進行させることで、深度によらず
        // 常に一定の猶予時間になる。
        let melted = tick_star_melting(&mut self.board, self.player.row, delta.as_millis() as u32);
        if !melted.is_empty() {
            events.push(GameEvent::BlockDestroyed {
                blocks: melted.len(),
            });
            self.note_vanished_cells(melted);
        }

        // ボム(TERM独自拡張。#96。ユーザー指摘: 「白ボンが、爆弾をランダムに投げて
        // くるイメージで、敵は出現しないものとする」)。「天に召される」演出中は
        // 位置の食い違いを避けるため、他のプレイヤー関連処理と同様に進行を止める。
        if !was_dying {
            let delta_ms = delta.as_millis() as u32;
            let mut exploded = Vec::new();
            // 他のボムの現在位置のスナップショット(TERM独自拡張。#143。ユーザー指摘:
            // 「爆弾は爆弾に重ならないようにする」)。ボムはCellグリッドとは別の
            // オーバーレイ(`Vec<Bomb>`)のため、盤面のセルだけを見て重力・バウンドを
            // 判定すると他のボムへ重なって落下・移動してしまう。このフレーム開始時点の
            // 位置で判定するため、同一フレーム内で複数のボムがほぼ同時に同じマスへ
            // 動こうとする極めて稀なケースでは1フレームだけ一時的にずれる場合がある
            // (次フレームで解消される)。
            let bomb_positions: Vec<board::Pos> = self.bombs.iter().map(|b| b.pos).collect();
            for (i, bomb) in self.bombs.iter_mut().enumerate() {
                match bomb.phase {
                    BombPhase::Entering => {
                        bomb.phase_elapsed_ms = bomb.phase_elapsed_ms.saturating_add(delta_ms);
                        if bomb.phase_elapsed_ms >= BOMB_ENTER_MS {
                            bomb.phase = BombPhase::Rolling;
                            bomb.phase_elapsed_ms = 0;
                        }
                    }
                    BombPhase::Rolling => {
                        bomb.phase_elapsed_ms = bomb.phase_elapsed_ms.saturating_add(delta_ms);
                        if bomb.phase_elapsed_ms >= BOMB_ROLL_MS {
                            bomb.phase = BombPhase::Settling;
                            bomb.phase_elapsed_ms = 0;
                            bomb.settle_bounce_dir = if self.rng.random_bool(0.5) { 1 } else { -1 };
                        }
                    }
                    BombPhase::Settling => {
                        // 支えを失っていれば落下しつつ、支持されていれば左右に跳ねて
                        // 落ち着き先を探す(TERM独自拡張。#140。ユーザー指摘: 「爆弾は
                        // 宙に浮かないように落ちること、落ちたら、またはねまくること
                        // 左右に壁をぶつかり行き来しながらいいところで泊まる」)。
                        // `BOMB_SETTLE_TICK_MS`ごとに1歩ぶん進める。
                        let prev_ticks = bomb.phase_elapsed_ms / BOMB_SETTLE_TICK_MS;
                        bomb.phase_elapsed_ms = bomb.phase_elapsed_ms.saturating_add(delta_ms);
                        let new_ticks = bomb.phase_elapsed_ms / BOMB_SETTLE_TICK_MS;
                        // 1フレームのdeltaが大きく複数tickぶんまたぐ場合(低フレームレート等)
                        // でも歩数が実時間ぶんきちんと進むよう、またいだ回数ぶん繰り返す。
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
                            // 他のボムの真上、またはプレイヤーの頭上に来た場合は、
                            // 地面に着地した時と違いそこで静止せず、Settling同様に
                            // 左右へバウンドしながら転がり続ける(TERM独自拡張。
                            // #143/#144。ユーザー指摘: 「爆弾がしたにあったら、はねな
                            // がら転がること」「爆弾はキャラの頭にぶつかったら別の列に
                            // ころがっていく」)。その間は起爆カウントダウンも進めない
                            // (Settling中と同じ扱い)。
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
                            // 起爆カウントダウン中も支えを失っていれば落下を続ける
                            // (TERM独自拡張。#140)。落下している間は`remaining_ms`を
                            // 減らさない(空中で起爆させないため)。
                            bomb.pos = below;
                            bomb.phase_elapsed_ms = 0;
                        } else {
                            bomb.phase_elapsed_ms = 0;
                            bomb.remaining_ms = bomb.remaining_ms.saturating_sub(delta_ms);
                            if bomb.remaining_ms == 0 {
                                exploded.push(i);
                            }
                        }
                    }
                }
            }
            for &i in exploded.iter().rev() {
                let bomb = self.bombs.remove(i);
                let blast_cells = bomb_blast_cells(
                    &self.board,
                    bomb.pos,
                    BOMB_BLAST_ROW_RANGE,
                    BOMB_BLAST_COL_RANGE,
                );
                let mut hit_player = false;
                let flash = Duration::from_millis(BOMB_EXPLOSION_FLASH_MS);
                // 爆風が届いた色ブロックは一色に統一する(TERM独自拡張。#137。ユーザー
                // 指摘: 「色ブロックは爆弾の炎によって一色に統一される」)。爆発ごとに
                // 1色をランダムに選び、その爆発の範囲内にある色ブロック全てを同じ色に
                // 揃える。ショートカットC/UnifyColorsアイテムは「4連結以上でも即座には
                // 自動消滅させない」方針(#49)だが、ボム爆発については「爆弾で変化した
                // 壁は落ちたときと同じ反応を発動させる。4マス以上結合している場合は
                // 消える」というユーザーの明示的な指摘(#140)により、着地時の自動消滅と
                // 同じ判定をこの場で発火させる。
                use rand::RngExt;
                let all_colors = ColorKind::ALL;
                let unify_color = all_colors[self.rng.random_range(0..all_colors.len())];
                let mut unified_positions = Vec::new();
                for &(row, col) in &blast_cells {
                    if (row, col) == self.player.position() {
                        hit_player = true;
                    }
                    // 爆心地(ボム設置マス)からの距離が遠いほど炎の色調を外側寄りに
                    // する(TERM独自拡張。#126。bombermantermの爆風スプライトが
                    // 中心ほど白熱・外側ほど赤黒くなるのに倣う)。爆風は上下左右の
                    // 直線上にしか届かないため、マンハッタン距離がそのまま
                    // 「軸方向に何マス離れているか」と一致する。
                    let tier = row.abs_diff(bomb.pos.0) + col.abs_diff(bomb.pos.1);
                    let tier = tier.min(u8::MAX as usize) as u8;
                    if matches!(self.board.cell(row, col), Cell::Rock { .. } | Cell::Diamond) {
                        self.board.set(row, col, Cell::Star { visible_ms: 0 });
                        self.recently_exploded.push(((row, col), flash, tier));
                    } else if matches!(self.board.cell(row, col), Cell::Color(_)) {
                        self.board.set(row, col, Cell::Color(unify_color));
                        self.recently_exploded.push(((row, col), flash, tier));
                        unified_positions.push((row, col));
                    }
                }

                // 一色に統一した結果、新たに4連結以上になったグループはこの場で消滅
                // させる(TERM独自拡張。#140)。同じグループに属する複数の位置を
                // 二重に処理しないよう、既に判定した位置は`checked`で除外する。
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
                        self.note_vanished_cells(vanished);
                    }
                }

                events.push(GameEvent::BombExploded);
                // 同一フレームで複数のボムが爆発し、どちらもプレイヤーを巻き込んだ
                // 場合に二重でミス処理しないよう、既にミス処理済み(is_dying/GameOver
                // へ遷移済み)でないことを確認してから適用する。
                if hit_player && !self.is_dying() && self.status == GameStatus::Playing {
                    self.apply_miss(&mut events);
                    if self.status != GameStatus::Playing {
                        return events;
                    }
                }
            }

            self.bomb_spawn_check_accum_ms += delta.as_millis() as u64;
            while self.bomb_spawn_check_accum_ms >= BOMB_SPAWN_CHECK_INTERVAL_MS {
                self.bomb_spawn_check_accum_ms -= BOMB_SPAWN_CHECK_INTERVAL_MS;
                self.maybe_spawn_bomb();
            }
        }

        // プレイヤー自身の自由落下(spec.md 1章、TERM独自拡張)。ブロックの重力とは
        // 独立したtick間隔(`player_fall_tick_ms`)で判定する(デバッグショートカットで
        // 両者を別々に速度調整できるようにするため、あえて別ループに分離している)。
        // 入力の有無や掘削とは無関係に、支えを失っていれば(直下がEmptyなら)落下する。
        // 直下が酸素カプセルの場合は掘削不要で「歩くだけで取得」する(spec.md公式マニュアル)。
        // 「天に召される」演出中はプレイヤー自身の論理位置を動かさないため、この間は
        // 蓄積も含めて凍結する(復活直後に積み残し分がまとめて落ちてしまうのを防ぐ)。
        if !self.is_dying() {
            self.player_fall_tick_accum += delta;
            let player_tick = Duration::from_millis(self.player_fall_tick_ms);
            while self.player_fall_tick_accum >= player_tick {
                self.player_fall_tick_accum -= player_tick;

                let before_fall = self.player.position();
                let fall_outcome =
                    physics::apply_player_free_fall(&mut self.board, &mut self.player);
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
    /// かどうか。支持されていなければ次の自由落下tickで必ず1マス落ちる状態であり、
    /// その間は横移動を受け付けない(TERM独自拡張。ユーザー指摘: 「必ず落ちてから
    /// 横移動が前提」)。直下が酸素カプセルの場合も自由落下でそのまま通過するため、
    /// 支持されているとはみなさない。
    fn player_is_grounded(&self) -> bool {
        let below = self.player.row + 1;
        if below >= self.board.depth_rows() {
            return true;
        }
        !matches!(
            self.board.cell(below, self.player.col),
            Cell::Empty | Cell::Oxygen
        )
    }

    /// プレイヤーの位置が`before`から変化していれば、移動の見た目補間アニメーションを
    /// (描画専用の状態として)`move_anim_duration_secs()`(固定の短い時間)で開始する。
    /// ロジック上の位置(row/col)には一切影響しない(TERM独自拡張、9章)。
    fn note_possible_move(&mut self, before: (usize, usize)) {
        self.note_possible_move_with_duration(before, move_anim_duration_secs());
    }

    /// `note_possible_move`の、補間時間を指定できる版(TERM独自拡張)。自由落下は
    /// 「現在の落ちるスピードにあうように滑らかに」という指摘を受け、固定の短い時間
    /// ではなく`player_fall_tick_ms`(実際の落下tick間隔)ぶんかけて補間することで、
    /// 次のtickが来るまでの間ずっと滑らかに動き続けるようにする。
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
    /// ヒヤリ回避スライダーの監視対象セルを設定する(TERM独自拡張。ユーザー指摘:
    /// 「そもそも避けてないのに発動してるように見える」を受け、単に「最近動いた」
    /// だけでなく、移動前の頭上が実際に揺れていた場合のみ監視対象にする)。該当しない
    /// 移動なら、古い監視が誤って生き残らないよう監視を解除する。
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

    /// 描画側が使う、直近の重力ティックで実際に1マス落下した各セルの
    /// (移動後の位置, 移動前の位置)一覧(TERM独自拡張)。ブロック落下のピクセル単位
    /// 補間描画に使う。次のティックが実行されるまで、このティックの内容を保持し続ける。
    pub fn recently_moved_blocks(&self) -> &[BlockMove] {
        &self.last_block_moves
    }

    /// #85(揺れているブロックが浮いたまま落下しない)の調査用に、ブロック状態遷移
    /// ログの記録先を新規に作り直して有効化する(TERM独自拡張。ユーザー指摘:
    /// 「タイトルからゲームスタートした時点でログdbは毎回リフレッシュするものと
    /// する」)。開けなかった場合は記録自体を諦め、ゲーム進行には影響させない。
    pub fn refresh_debug_log(&mut self) {
        self.debug_log = DebugLog::open_fresh();
    }

    /// `update()`が呼ばれるたびに1増えるフレーム通し番号(TERM独自拡張。#85調査用。
    /// ユーザー指摘: 「フレームのユニーク番号を取得できるようにしておき」)。
    /// ブロック状態遷移ログの各行と突き合わせるための識別子として画面に表示する。
    pub fn debug_frame(&self) -> u64 {
        self.frame_counter
    }

    /// 消滅したセルを消滅フラッシュ演出の対象として記録する(TERM独自拡張。
    /// ユーザー指摘: 「ブロックが消える瞬間に消える演出してほしい」)。
    ///
    /// 新たに消滅したセルに隣接する、まだフラッシュ中(=直前の消滅演出がまだ終わって
    /// いない)セルがあれば、その残り時間をこのフラッシュぶんへ延長する(TERM独自拡張。
    /// ユーザー指摘: 「隣接ブロックで消える演出に入るなかで完全に消える前に別の
    /// 隣接ブロックがあったら、消える演出を延長してそれも消す」)。これにより、重力で
    /// 落下したブロックが着地して連鎖的に4連結消滅した場合、古い方の演出が先に
    /// フェードアウトして途切れず、1つの連続した「連鎖」に見えるようにする。
    fn note_vanished_cells(&mut self, cells: impl IntoIterator<Item = (board::Pos, Cell)>) {
        let flash = Duration::from_millis(BLOCK_VANISH_FLASH_MS);
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
                    .find(|(p, _)| *p == neighbor)
                {
                    entry.1 = flash;
                }
            }
        }

        self.recently_vanished
            .extend(new_cells.into_iter().map(|(pos, _)| (pos, flash)));
    }

    /// 描画側が使う、指定セルの消滅フラッシュ演出の進捗(0.0=消滅直後、1.0=演出完了
    /// 直前。TERM独自拡張)。対象でなければ`None`を返す。
    pub fn vanish_flash_progress(&self, pos: board::Pos) -> Option<f32> {
        let flash = Duration::from_millis(BLOCK_VANISH_FLASH_MS)
            .as_secs_f32()
            .max(0.001);
        self.recently_vanished
            .iter()
            .find(|&&(p, _)| p == pos)
            .map(|&(_, remaining)| (1.0 - remaining.as_secs_f32() / flash).clamp(0.0, 1.0))
    }

    /// 描画側が使う、指定セルのボム爆発・炎演出の進捗(0.0=爆発直後、1.0=演出完了
    /// 直前)と爆心地からの距離(TERM独自拡張。#126)。対象でなければ`None`を返す。
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

    /// 描画側が使う、ブロック落下ティックの進捗(0.0=直前のティック直後,
    /// 1.0=次のティックが来る直前。TERM独自拡張)。`recently_moved_blocks`と組み合わせて、
    /// 移動前の位置から移動後の位置へ向けて滑らかに補間する。
    pub fn block_fall_progress(&self) -> f32 {
        let tick_secs = self.effective_block_fall_tick_ms().max(1) as f32 / 1000.0;
        (self.fall_tick_accum.as_secs_f32() / tick_secs).clamp(0.0, 1.0)
    }

    /// 深度に応じて実効化したブロック落下tick間隔(ms、TERM独自拡張)。設定画面/
    /// デバッグショートカットで調整した`block_fall_tick_ms`を「深度0mでの速度」
    /// として扱い、`FALL_SPEED_DEPTH_MAX_SPEEDUP`まで深度に応じて短縮する
    /// (`DEBUG_FALL_TICK_MS_MIN`を下回らない)。
    fn effective_block_fall_tick_ms(&self) -> u64 {
        let fraction = depth_fraction(self.player.depth_m());
        let speedup = 1.0 - fraction * (1.0 - FALL_SPEED_DEPTH_MAX_SPEEDUP);
        ((self.block_fall_tick_ms as f32 * speedup) as u64).max(DEBUG_FALL_TICK_MS_MIN)
    }

    /// 「天に召される」演出中かどうか(TERM独自拡張)。この間は移動・掘削入力を無視する。
    fn is_dying(&self) -> bool {
        self.ascending_remaining.is_some()
    }

    /// 「天に召される」演出中、または「わ〜!」スライダー演出中(TERM独自拡張)かどうか。
    /// この間は移動・掘削入力を無視する。
    fn is_input_frozen(&self) -> bool {
        self.is_dying() || self.dodge_stage != DodgeStage::None
    }

    /// 押し潰しの「潰れた」演出が表示中かどうか(GameOverオーバーレイの表示可否判定にも使う)。
    pub fn crush_flash_active(&self) -> bool {
        self.crush_flash_remaining > Duration::ZERO || self.ascending_remaining.is_some()
    }

    /// 掘削アニメーション中の描画フレーム(TERM独自拡張、9章)。掘削演出中でなければ
    /// `None`、演出中は`DRILL_ANIM_FRAME_MS`ごとに`true`/`false`を切り替えて返す
    /// (方向別のアニメーション用に描画側が2フレームを交互に選ぶ)。
    pub fn drilling_frame(&self) -> Option<bool> {
        if self.drill_flash_remaining <= Duration::ZERO {
            return None;
        }
        let elapsed_ms =
            DRILL_ANIM_MS.saturating_sub(self.drill_flash_remaining.as_millis() as u64);
        Some((elapsed_ms / DRILL_ANIM_FRAME_MS.max(1)).is_multiple_of(2))
    }

    /// 「わ〜!」スライダー演出中(横滑り段階のみ、TERM独自拡張)かどうか。描画側が
    /// スプライトを横滑りさせる判断に使う。硬直(Recovering)段階では滑りは止まっている
    /// ため`false`を返すが、`is_input_frozen`相当のフリーズ自体はそちらも継続する。
    pub fn is_dodge_sliding(&self) -> bool {
        self.dodge_stage == DodgeStage::Sliding
    }

    /// 「わ〜!」スライダー演出の横滑り進捗(0.0=開始直後、1.0=スライダー完了直前。
    /// TERM独自拡張)。スライダー中でなければ0.0を返す。
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

    /// 「天に召される」演出の進捗(0.0=演出開始直後、1.0=演出完了直前。TERM独自拡張)。
    /// 演出中でなければ0.0を返す。描画側(render.rs)がこれを使って、キャラのスプライトを
    /// 少しずつ上へドリフトさせる(「天に召される」見た目の演出)。
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

    /// 横移動のクールダウン間隔を直接指定する(起動時、Settingsから読み込んだ値を適用する
    /// 用途。TERM独自拡張。ユーザー指摘: 「横移動のスピードを設定で変えられるように」)。
    /// 範囲外の値は`MOVE_COOLDOWN_MS_MIN`〜`MAX`にクランプする。
    pub fn set_move_cooldown_ms(&mut self, ms: u64) {
        self.move_cooldown_ms = ms.clamp(MOVE_COOLDOWN_MS_MIN, MOVE_COOLDOWN_MS_MAX);
    }

    /// 現在の揺れ時間(ms)。設定の永続化(main.rs/Settings)用に公開する。
    pub fn shake_duration_ms(&self) -> u64 {
        self.shake_duration_ms
    }

    /// 揺れ時間を直接指定する(起動時、Settingsから読み込んだ値を適用する用途)。
    pub fn set_shake_duration_ms(&mut self, ms: u64) {
        self.shake_duration_ms = ms.clamp(DEBUG_SHAKE_DURATION_MS_MIN, DEBUG_SHAKE_DURATION_MS_MAX);
    }

    /// 硬直インターバルを直接指定する(起動時、Settingsから読み込んだ値を適用する用途。
    /// TERM独自拡張。ユーザー指摘: 「この設定値も作る」)。
    pub fn set_dodge_recovery_ms(&mut self, ms: u64) {
        self.dodge_recovery_ms = ms.clamp(DODGE_RECOVERY_MS_MIN, DODGE_RECOVERY_MS_MAX);
    }

    /// ボム出現頻度を直接指定する(起動時、Settingsから読み込んだ値を適用する用途。
    /// TERM独自拡張。#96)。範囲外の値は`BOMB_SPAWN_RATE_PERCENT_MIN`〜
    /// `SPAWN_RATE_PERCENT_MAX`にクランプする。
    pub fn set_bomb_spawn_rate_percent(&mut self, percent: u32) {
        self.bomb_spawn_rate_percent = percent.clamp(
            crate::constants::BOMB_SPAWN_RATE_PERCENT_MIN,
            crate::constants::SPAWN_RATE_PERCENT_MAX,
        );
    }

    /// 現在盤面上にあるボムの一覧(TERM独自拡張。#96)。描画側(render.rs)が参照する。
    pub fn bombs(&self) -> &[Bomb] {
        &self.bombs
    }

    /// `from_row`以降の岩(X)/AIR/スター/ダイヤブロック出現率を、指定の配分率(%、
    /// 100=通常のまま)で再抽選する(TERM独自拡張。ユーザー指摘: 「設定でXブロックの
    /// 配分量・AIRの配分量をいじれるようにしたい。プレイ中でもその数値をいじれるように
    /// したい」「ダイヤブロック0%設定」)。新規ゲーム開始直後は`from_row`に安全地帯明けの
    /// 行を渡せば盤面全体に反映され、プレイ中に呼ぶ場合は呼び出し側が
    /// `player.row + SPAWN_RATE_REROLL_SAFE_MARGIN_ROWS`のような画面外の行を渡すことで、
    /// 既に見えている地形を変えてしまわないようにする。
    ///
    /// `color_cluster_rate_percent`(%、100=通常のまま)は色ブロックの結合しやすさを
    /// 調整する(TERM独自拡張。ユーザー指摘: 「ブロック配置の結合関係の割合を設定
    /// できるようにして」)。
    ///
    /// `item_*_rate_percent`(%、100=通常のまま)はアイテムブロック3種(#98/#101/#107)の
    /// 出現率をそれぞれ個別に調整する(TERM独自拡張。ユーザー指摘: 「各種アイテムの
    /// 出現頻度の設定項目増やして」)。
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
        self.board.reroll_overlays_from_row(
            from_row,
            rock_rate_percent,
            air_rate_percent,
            star_rate_percent,
            diamond_rate_percent,
            item_clear_above_rate_percent,
            item_unify_colors_rate_percent,
            item_starify_screen_rate_percent,
            color_count,
            color_cluster_rate_percent,
            &self.gravity_state,
        );
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
    pub fn debug_add_life(&mut self) {
        if self.status == GameStatus::Playing {
            self.player.lives = (self.player.lives + 1).min(LIVES_MAX);
        }
    }

    /// デバッグ: 酸素(AIR)を100%まで回復する。Playing中のみ有効(TERM独自拡張。
    /// ユーザー指摘: 「AIRを100%にするショートカット追加」)。
    pub fn debug_fill_air(&mut self) {
        if self.status == GameStatus::Playing {
            self.player.oxygen = crate::constants::OXYGEN_MAX;
        }
    }

    /// デバッグ: プレイヤーより浅い(画面上で上にある)行を全てEmptyにする。Playing中
    /// のみ有効。AIR(酸素カプセル)は消滅させずその場に残す(ユーザー指摘: 「Xで
    /// ブロック消したときAIRは消えずに上から落下してくるように」)。
    pub fn debug_clear_above_player(&mut self) {
        if self.status != GameStatus::Playing {
            return;
        }
        // 4連結自動消滅と同じ消滅フラッシュ演出を出す(TERM独自拡張。ユーザー指摘:
        // 「ショートカットRの動作だけど、消えるとき、結合して消えるときと同じ消える
        // アニメーションして」)。AIR・アイテムブロック(C/R/Kアイテム)は消さずに残す
        // (TERM独自拡張。ユーザー指摘: 「ショートカットRは、Cアイテム、Rアイテム、
        // Kアイテムを削除しない(AIRと同じ扱い)」)。
        let mut cleared = Vec::new();
        for row in 0..self.player.row {
            for col in 0..self.board.width() {
                let cell = self.board.cell(row, col);
                if !matches!(cell, Cell::Oxygen | Cell::Item(_)) {
                    if cell != Cell::Empty {
                        cleared.push(((row, col), cell));
                    }
                    self.board.set(row, col, Cell::Empty);
                }
            }
        }
        self.note_vanished_cells(cleared);
    }

    /// デバッグ: プレイヤー付近(上下`DEBUG_UNIFY_COLORS_RANGE_ROWS`行)の色ブロックを
    /// ランダムに選んだ2色だけへ揃える。Playing中のみ有効。
    pub fn debug_unify_nearby_colors(&mut self) -> Vec<GameEvent> {
        if self.status != GameStatus::Playing {
            return Vec::new();
        }
        use rand::RngExt;
        let mut rng = rand::rng();

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
        // 次の重力ティックで結合関係を一から作り直させる(ユーザー指摘: 「ちゃんと結合
        // 関係を再計算するように」)。ただし既に揺れが明けて連続落下中の塊まで巻き込んで
        // 揺れ直させてしまうと、Cを押した瞬間に「フリーズしたように見える」(ユーザー指摘:
        // 「ショートカット:Cにした瞬間これで落ちずにフリーズしてるように見える」)ため、
        // そちらは対象外にする。
        //
        // なお、塗り替えで新たに4連結以上になった箇所を即座に自動消滅させる処理は
        // 過去に実装していたが、「単にブロックの色を2色に変換するだけでよくて、
        // 消滅させなくていい」というユーザー指摘により廃止した。連結の再計算(揺れ状態の
        // リセット)だけ行い、実際の消滅判定は通常の重力ティック(支えを失って落下・
        // 着地した場合のみ)に委ねる。
        let current_shake_ticks =
            (self.shake_duration_ms / self.block_fall_tick_ms.max(1)).min(u8::MAX as u64) as u8;
        self.gravity_state.reset_shake_progress(current_shake_ticks);

        Vec::new()
    }

    /// デバッグ: プレイヤーに最も近いスターブロックを1つ、実際にドリルで取得した
    /// (`DrillOutcome::StarDestroyed`)のと全く同じ挙動(消滅・スコア加算・同じ
    /// イベント列)で取得する。盤面上にスターが1つも無ければ何もしない。Playing中
    /// のみ有効(TERM独自拡張。当初「最寄りのスターを取得」として実装したが、
    /// ユーザー指摘: 「Kは最寄りのスターを取得になってるけど、違う 画面内をスター化
    /// する(Xブロック,ダイヤブロック100%)」を受けて仕様変更。画面内(プレイヤー位置
    /// から上下`STAR_VISIBLE_RANGE_ROWS`行)にあるXブロック・ダイヤブロックを、
    /// 揺れ中のセルを除き全て(100%)スターブロックへ変える)。
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

    /// ボム出現を1回判定する(TERM独自拡張。#96)。盤面全体のボム数が上限未満で、
    /// 深度・設定に応じた確率の抽選に当たれば、画面内のランダムなEmptyマスへ
    /// ボムを1個設置する。
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

    /// 画面内(プレイヤー位置から上下`STAR_VISIBLE_RANGE_ROWS`行)のEmptyマスを1つ
    /// ランダムに選び、ボムを設置する(TERM独自拡張。#96)。候補が無ければ何もしない。
    /// 既存の他のボムが既に占めているマスは候補から除外する(TERM独自拡張。#143。
    /// ユーザー指摘: 「爆弾は爆弾に重ならないようにする」)。
    fn spawn_bomb_at_random_empty_cell(&mut self) {
        let start_row = self.player.row.saturating_sub(STAR_VISIBLE_RANGE_ROWS);
        let end_row = (self.player.row + STAR_VISIBLE_RANGE_ROWS)
            .min(self.board.depth_rows().saturating_sub(1));
        let width = self.board.width();
        let occupied: Vec<board::Pos> = self.bombs.iter().map(|b| b.pos).collect();
        let candidates: Vec<board::Pos> = (start_row..=end_row)
            .flat_map(|row| (0..width).map(move |col| (row, col)))
            .filter(|&(row, col)| {
                self.board.cell(row, col) == Cell::Empty && !occupied.contains(&(row, col))
            })
            .collect();
        if candidates.is_empty() {
            return;
        }
        let idx = self.rng.random_range(0..candidates.len());
        let pos = candidates[idx];
        // 白ボンは画面の左端・右端のどちらかから登場する(TERM独自拡張。#123。
        // ユーザー指摘: 「白ボンが画面の外からとことこやってきて」)。同じ行の
        // 反対側の端から登場すれば、必ず盤面内を横切って転がってくる形になる。
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
            remaining_ms: BOMB_FUSE_MS,
            settle_bounce_dir: 1,
        });
    }

    /// デバッグ: ボムを1個、画面内のランダムなEmptyマスへ即座に設置する(TERM独自拡張。
    /// #96。ユーザー指摘: 「ショートカットキーもくれ」)。盤面全体のボム数が上限に
    /// 達している、または出現先が無ければ何もしない。Playing中のみ有効。
    pub fn debug_place_bomb(&mut self) {
        if self.status != GameStatus::Playing || self.bombs.len() >= BOMB_MAX_COUNT_ON_BOARD {
            return;
        }
        self.spawn_bomb_at_random_empty_cell();
    }
}

/// `BombPhase::Settling`中の1歩ぶんの移動(TERM独自拡張。#140)。支えを失って
/// いれば1マス落下し、支持されていれば現在の`bounce_dir`(+1=右、-1=左)方向へ
/// 1マス移動を試みる。移動先が壁・既存ブロック・他のボム・プレイヤーの現在地で
/// 塞がっていれば方向を反転する(次のステップで反対方向を試す)。
/// `other_bomb_positions`は他のボムの現在位置(TERM独自拡張。#143。ユーザー指摘:
/// 「爆弾は爆弾に重ならないようにする」)。`player_pos`はプレイヤーの現在位置
/// (TERM独自拡張。#144。ユーザー指摘: 「爆弾はキャラの頭にぶつかったら別の列に
/// ころがっていく」)。いずれもCellグリッドとは別のオーバーレイ/エンティティの
/// ため、盤面のセルだけを見ていると重なって落下・移動してしまう。
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
    use crate::constants::{ROCK_HITS_TO_BREAK, SHAKE_TICKS};
    use board::{Cell, ColorKind};

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
    fn drilling_an_item_does_nothing_like_air() {
        // ユーザー指摘: 「アイテムはAIRと同じ用に掘らなくても取得でき」。AIR同様、
        // 掘削では何も起きず、ブロックはそのまま残る。
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
        // ユーザー指摘: 「ショートカットRと同じ効果のあるアイテムつくろ」「アイテムは
        // AIRと同じ用に掘らなくても取得でき」。
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
        // ユーザー指摘: 「ショートカットC効果のアイテムも作って」。
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
        // ユーザー指摘: 「ショートカットKアイテムつくって」。
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
        // ユーザー指摘: 「RアイテムやKアイテムがその上にダイヤブロックなどがあるとき、
        // 一緒に落下する過程で消えてしまう(必ず再現する)」。アイテムの真上にダイヤが
        // あり両方支えを失って一緒に落下しても、アイテムが消えずに着地することを確認する。
        const FRAME_MS: u64 = 33;
        let mut game = Game::new(80);
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 11;

        game.board.rows[500][0] = Cell::Diamond;
        game.board.rows[501][0] = Cell::Item(ItemEffect::ClearAbove);

        let total_ms_needed = (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 500 * FALL_TICK_MS;
        let mut elapsed_ms = 0u64;
        while elapsed_ms < total_ms_needed {
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
    fn oxygen_running_out_during_update_costs_a_life_and_continues() {
        // 酸素切れも押し潰しと同じ処理を経る(TERM独自拡張。ユーザー指摘:
        // 「AIR不足で死んだときもブロックにつぶされたときと同じ処理」)ため、
        // ライフ減算・酸素回復は「天に召される」演出完了まで遅延される。
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
        assert!(events.iter().any(|e| matches!(e, GameEvent::LifeLost)));

        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));

        assert_eq!(game.player.lives, lives_before - 1);
        assert_eq!(game.player.oxygen, crate::constants::OXYGEN_MAX);
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
        // ユーザー指摘: 「横移動のスピードを設定で変えられるように」。設定値を小さく
        // すると、既定(INPUT_COOLDOWN_MS=80ms)では通らないはずの短い間隔でも
        // 次の移動入力が通ることを確認する。
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
        // ユーザー指摘: 「左右にキャラ走るとき、速くなったり遅くなったりしてる。
        // 一定のインターバルで速度が落ちたりする」。クールダウンぶんを使い切った後、
        // 少し余分に時間が経ってから次の入力が来た場合、その超過ぶんは繰り越され、
        // その次の入力までの待ち時間がその分だけ短くなることを確認する
        // (0へリセットする旧実装では、この繰り越しが起きずジッターの原因になっていた)。
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
        // 長時間入力が無い間にアキュムレータが際限なく貯まると、後からまとめて
        // 連続入力が全て即座に通ってしまう(バースト)。上限で頭打ちにして
        // それを防いでいることを確認する。
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
        // ユーザー指摘: 「カーソル押しっぱなしのときにzやx押すと進むのをキャンセル
        // してしまう」。1回目のぶつかり(bumped_direction記憶)と2回目の同方向入力
        // (段差登り)の間に掘削キー(Z/X)が挟まっても、段差登りがキャンセルされない
        // ことを確認する。
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
    fn auto_vanished_cells_show_a_vanish_flash_that_expires_after_block_vanish_flash_ms() {
        // ユーザー指摘: 「ブロックが消える瞬間に消える演出してほしい」。自動消滅した
        // セルは消滅直後にフラッシュ演出の対象になり、BLOCK_VANISH_FLASH_MS経過後に
        // 対象から外れることを確認する。
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
        // ユーザー指摘: 「隣接ブロックで消える演出に入るなかで完全に消える前に別の
        // 隣接ブロックがあったら、消える演出を延長してそれも消す」。重力で連鎖的に
        // 4連結消滅が起きた場合、先に消えたセルの演出が途切れず1つの連鎖に見えるように
        // する。
        let mut game = Game::new(1);
        game.note_vanished_cells(vec![((0, 0), Cell::Color(ColorKind::Red))]);
        game.update(Duration::from_millis(BLOCK_VANISH_FLASH_MS / 2));
        let progress_before = game.vanish_flash_progress((0, 0)).unwrap();
        assert!(progress_before > 0.0, "前提: フラッシュが進行中であること");

        // 隣接セル(0,1)が新たに消滅 → (0,0)の残り時間もリセットされて延長されるはず。
        game.note_vanished_cells(vec![((0, 1), Cell::Color(ColorKind::Red))]);
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
        game.note_vanished_cells(vec![((0, 0), Cell::Color(ColorKind::Red))]);
        game.update(Duration::from_millis(BLOCK_VANISH_FLASH_MS / 2));
        let progress_before = game.vanish_flash_progress((0, 0)).unwrap();

        // 隣接していない遠いセル(5,5)が消滅しても、(0,0)の演出は延長されないはず。
        game.note_vanished_cells(vec![((5, 5), Cell::Color(ColorKind::Red))]);
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
        // ユーザー報告: 「掘っていないのに設置済みブロックが消える/落下する」
        // (スター処理が原因ではないかとの推測)。スターブロックの上に乗っていた
        // 色ブロックが、スターが溶けて消えた後もそのまま「浮いた」状態で残らず、
        // ちゃんと支えを失って落下することを確認する。
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

    // --- ブロック落下のピクセル単位補間描画(TERM独自拡張) ---

    #[test]
    fn recently_moved_blocks_and_progress_track_the_latest_gravity_tick() {
        // ユーザー指摘: 「ブロックの落ち方をコマ送りでなくピクセル単位で滑らかにして
        // ほしい」。実際に1マス落下したtickの直後は、その(移動後の位置, 移動前の位置)が
        // recently_moved_blocksに記録され、block_fall_progressはそのtickの開始直後を
        // 表す小さな値になっていることを確認する。
        let mut game = Game::new(1);
        clear_board(&mut game);
        // 深度による落下速度スケーリング(TERM独自拡張)の影響を受けないよう、
        // プレイヤーは深度0m相当(等倍速)の浅い位置に置く。
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
        // ユーザー指摘: 「キャラの落ち方も1コマずつではなく、現在の落ちるスピードに
        // あうように滑らかに落ちてほしい」。自由落下の見た目補間は、横移動用の固定の
        // 短い時間(MOVE_ANIM_DURATION_MS)ではなく、実際のplayer_fall_tick_msぶんかけて
        // 行われることを確認する。
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
        // spec.md 1章(TERM独自拡張): 支えを失った(直下がEmptyな)プレイヤーは、入力が
        // 無くてもFALL_TICK_MSごとに1マスずつ自動的に落下し続ける。
        // ランダム生成された周囲のブロックが偶然崩れて割り込むことが無いよう、
        // プレイヤーの通り道を広めにEmptyでクリアしてから検証する。
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
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::LifeLost | GameEvent::GameOverMiss))
        );
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

    // --- 押し潰されて死ぬ演出(TERM独自拡張、9章) ---

    #[test]
    fn oxygen_miss_also_activates_the_flash_effect() {
        // ユーザー指摘: 「AIR不足で死んだときもブロックにつぶされたときと同じ処理」。
        // 以前は押し潰しのみ「潰れた」フラッシュ演出を行っていたが、酸素切れ死亡でも
        // 同じフラッシュ演出が起きるよう統一した。
        let mut game = Game::new(30);
        game.player.oxygen = 1.0;

        game.update(Duration::from_secs(1)); // 酸素切れでミス(押し潰しではない)

        assert!(game.crush_flash_active());
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

        game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));
        // 押し潰し直後は「天に召される」演出中で、ライフ減算・3列クリアは演出が
        // 終わるまで遅延される(TERM独自拡張)。演出の完了を待つ。
        game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        ));

        assert_eq!(
            game.player.lives, 1,
            "押し潰されてライフを1つ失っているはず"
        );
        for row in 0..999 {
            assert_eq!(
                game.board.cell(row, 4),
                Cell::Empty,
                "row={row} col=4はクリアされているはず"
            );
            assert_eq!(
                game.board.cell(row, 5),
                Cell::Empty,
                "row={row} col=5はクリアされているはず"
            );
            assert_eq!(
                game.board.cell(row, 6),
                Cell::Empty,
                "row={row} col=6はクリアされているはず"
            );
        }
        assert_eq!(
            game.board.cell(999, 3),
            Cell::Color(ColorKind::Green),
            "対象外の列はクリアされない"
        );
        assert_eq!(
            game.board.cell(999, 7),
            Cell::Color(ColorKind::Green),
            "対象外の列はクリアされない"
        );
    }

    #[test]
    fn oxygen_death_goes_through_the_same_ascend_and_three_column_clear_as_crush_death() {
        // ユーザー指摘: 「AIR不足で死んだときもブロックにつぶされたときと同じ処理」。
        // 酸素切れ死亡でも押し潰し死亡と全く同じ処理(「天に召される」演出→演出完了後に
        // 3列クリア・ライフ減算)が行われることを確認する。
        let mut game = Game::new_with_lives(34, 2); // ライフ2、酸素切れでも即GameOverにならない
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        for row in 990..999 {
            game.board.rows[row][4] = Cell::Color(ColorKind::Blue);
            game.board.rows[row][5] = Cell::Color(ColorKind::Blue);
            game.board.rows[row][6] = Cell::Color(ColorKind::Blue);
        }
        game.board.rows[999][3] = Cell::Color(ColorKind::Green);
        game.board.rows[999][7] = Cell::Color(ColorKind::Green);
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
            assert_eq!(
                game.board.cell(row, 4),
                Cell::Empty,
                "row={row} col=4はクリアされているはず"
            );
            assert_eq!(
                game.board.cell(row, 5),
                Cell::Empty,
                "row={row} col=5はクリアされているはず"
            );
            assert_eq!(
                game.board.cell(row, 6),
                Cell::Empty,
                "row={row} col=6はクリアされているはず"
            );
        }
        assert_eq!(
            game.board.cell(999, 3),
            Cell::Color(ColorKind::Green),
            "対象外の列はクリアされない"
        );
        assert_eq!(
            game.board.cell(999, 7),
            Cell::Color(ColorKind::Green),
            "対象外の列はクリアされない"
        );
    }

    #[test]
    fn crush_death_lets_oxygen_capsules_fall_instead_of_vanishing() {
        // ユーザー指摘: 「キャラが死んだとき(AIR不足/つぶされたとき)...AIRは消えずに
        // 上から落下してくるように」。3列クリアの範囲内にあったAIRは消滅させず、
        // 周囲がEmptyになった結果、通常の重力で自然に落下することを確認する。
        let mut game = Game::new_with_lives(70, 2); // ライフ2、押し潰されても即GameOverにならない
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[998][5] = Cell::Color(ColorKind::Red); // プレイヤーの真上、支えなし(押し潰す)
        game.board.rows[990][5] = Cell::Oxygen; // クリア範囲内のAIR(支えなし、押し潰しと並行して自然落下もする)

        let mut events = game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));
        // 天に召される演出中も周囲の重力処理は止まらない(#68)ため、この1回の
        // updateだけでAIRが最後まで落下しきる可能性もある。
        events.extend(game.update(Duration::from_millis(
            crate::constants::CRUSH_ASCEND_MS + 10,
        )));
        assert_eq!(game.player.lives, 1, "演出完了でライフが減っているはず");

        // AIRは3列クリアで消滅させられたわけではなく、通常の重力に従って落下を
        // 続け、最終的にプレイヤーへ到達して取得(酸素回復)される。単に消滅した
        // のではなく、正規のイベントとして処理されることを確認する。
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
        // ユーザー指摘: 「潰れた瞬間もまわりの落下アニメーションを止めない」。
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
    fn debug_clear_above_player_leaves_oxygen_capsules_in_place_to_fall_naturally() {
        // ユーザー指摘: 「Xでブロック消したときAIRは消えずに上から落下してくるように」。
        let mut game = Game::new(71);
        clear_board(&mut game);
        game.player.row = 50;
        game.player.col = 5;
        game.board.rows[10][5] = Cell::Oxygen;
        game.board.rows[10][6] = Cell::Rock { hits: 2 }; // 比較用: AIR以外は通常通り消える

        game.debug_clear_above_player();

        assert_eq!(
            game.board.cell(10, 5),
            Cell::Oxygen,
            "AIRは消えずに残るはず"
        );
        assert_eq!(
            game.board.cell(10, 6),
            Cell::Empty,
            "AIR以外は通常通り消えるはず"
        );
    }

    #[test]
    fn debug_clear_above_player_leaves_item_blocks_in_place_to_fall_naturally() {
        // ユーザー指摘: 「ショートカットRは、Cアイテム、Rアイテム、Kアイテムを削除
        // しない(AIRと同じ扱い)」。
        let mut game = Game::new(71);
        clear_board(&mut game);
        game.player.row = 50;
        game.player.col = 5;
        game.board.rows[10][5] = Cell::Item(ItemEffect::ClearAbove);
        game.board.rows[11][5] = Cell::Item(ItemEffect::UnifyColors);
        game.board.rows[12][5] = Cell::Item(ItemEffect::StarifyScreen);
        game.board.rows[10][6] = Cell::Rock { hits: 2 }; // 比較用: アイテム以外は通常通り消える

        game.debug_clear_above_player();

        assert_eq!(
            game.board.cell(10, 5),
            Cell::Item(ItemEffect::ClearAbove),
            "Rアイテムは消えずに残るはず"
        );
        assert_eq!(
            game.board.cell(11, 5),
            Cell::Item(ItemEffect::UnifyColors),
            "Cアイテムは消えずに残るはず"
        );
        assert_eq!(
            game.board.cell(12, 5),
            Cell::Item(ItemEffect::StarifyScreen),
            "Kアイテムは消えずに残るはず"
        );
        assert_eq!(
            game.board.cell(10, 6),
            Cell::Empty,
            "アイテム以外は通常通り消えるはず"
        );
    }

    #[test]
    fn debug_clear_above_player_shows_the_same_vanish_flash_as_auto_vanish() {
        // ユーザー指摘: 「ショートカットRの動作だけど、消えるとき、結合して消えるとき
        // と同じ消えるアニメーションして」。
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
        // ユーザー指摘: 「AIRを100%にするショートカット追加」。
        let mut game = Game::new(72);
        game.player.oxygen = 1.0;

        game.debug_fill_air();

        assert_eq!(game.player.oxygen, crate::constants::OXYGEN_MAX);
    }

    #[test]
    fn debug_fill_air_does_nothing_when_not_playing() {
        let mut game = Game::new_with_lives(72, 1);
        game.player.oxygen = 1.0;
        game.update(Duration::from_secs(1)); // 酸素切れ+ライフ1でGameOverにする
        assert_eq!(game.status, GameStatus::GameOver);

        game.debug_fill_air();

        assert_eq!(game.player.oxygen, 0.0, "GameOver中は酸素を回復しない");
    }

    #[test]
    fn debug_starify_visible_screen_converts_rock_and_diamond_within_range_to_stars() {
        // ユーザー指摘: 「画面内をスター化する(Xブロック,ダイヤブロック100%)」。
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
        // ユーザー指摘(#99と同じ理由): 揺れ中/落下中のブロックはスター化対象外。
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
        game.board.rows[990][3] = Cell::Rock { hits: 0 };
        game.player.oxygen = 1.0;
        game.update(Duration::from_secs(1)); // 酸素切れ+ライフ1でGameOverにする
        assert_eq!(game.status, GameStatus::GameOver);

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
    fn crush_on_the_last_life_skips_the_ascending_sequence_and_ends_the_game_immediately() {
        // ユーザー指摘: 「livesが0になったときはただちにゲームオーバーのダイアログ
        // 出てOK」。最後のライフでの押し潰しは「天に召される」演出を行わず、
        // 即座にGameOverへ進む。
        let mut game = Game::new_with_lives(35, 1); // ライフ1(最後の1機)
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[998][5] = Cell::Color(ColorKind::Red); // プレイヤーの真上、支えなし

        game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));

        assert_eq!(
            game.status,
            GameStatus::GameOver,
            "演出を待たず即座にGameOverになるはず"
        );
        assert_eq!(game.player.lives, 0);
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
        // ユーザー指摘: 「キャラが死んだとき(AIR不足/つぶされたとき)しんだときのSEを
        // 鳴らしてほしい」。押し潰された瞬間に(天に召される演出の完了=3秒近く後を
        // 待たず)即座にGameEvent::LifeLostが発火し、演出完了時には重複して発火しない
        // ことを確認する。
        let mut game = Game::new_with_lives(80, 2); // ライフ2、押し潰されても即GameOverにならない
        clear_board(&mut game);
        game.player.row = 999;
        game.player.col = 5;
        game.board.rows[998][5] = Cell::Color(ColorKind::Red); // プレイヤーの真上、支えなし

        let events = game.update(Duration::from_millis(
            (SHAKE_TICKS as u64 + 1) * FALL_TICK_MS + 10,
        ));
        assert!(
            events.iter().any(|e| matches!(e, GameEvent::LifeLost)),
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
            !events.iter().any(|e| matches!(e, GameEvent::LifeLost)),
            "演出完了時に重複してLifeLostが発火してはいけない"
        );
        assert_eq!(game.player.lives, 1, "演出完了時にライフは減るはず");
    }

    #[test]
    fn revived_event_fires_exactly_when_the_ascend_animation_completes() {
        // ユーザー指摘: 「死んで、復活したときのSEほしい(よーし、がんばるぞーみたいな)」。
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

    // --- 掘削アニメーション(TERM独自拡張、9章) ---

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
        // ユーザー指摘: 「上に掘る時、上向きながらピヨンピヨン跳ねる。左右に掘る時、
        // 横にドリルをぐいぐい。下に掘る時、下向きながらドリルをぐいぐい」。掘削入力
        // 直後はアニメーションフレームが交互に切り替わり、DRILL_ANIM_MS経過後は
        // 通常表示(None)に戻ることを確認する。
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

    // --- ヒヤリ回避スライダー演出(TERM独自拡張、9章) ---

    #[test]
    fn dodge_slide_triggers_only_when_fleeing_a_block_that_was_actually_shaking_overhead() {
        // ユーザー指摘: 「そもそも避けてないのに発動してるように見える」。単に
        // 「最近動いた」だけでなく、移動前の頭上が実際に揺れていた(=本物の脅威から
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
        // 通常通り入力が通ることを確認する(ユーザー指摘: 「スライダー直後その状態で
        // 起き上がるまでに1秒インターバル=この設定値も作る」)。
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

    // --- 移動の見た目補間アニメーション(TERM独自拡張、9章) ---

    #[test]
    fn new_game_starts_with_move_animation_already_settled() {
        // 開始直後にいきなり(0,0)相当からアニメーションしてしまわないことの確認。
        let game = Game::new(32);
        assert_eq!(game.move_anim_progress(), 1.0);
        assert_eq!(game.render_prev_position(), game.player.position());
    }

    #[test]
    fn debug_frame_starts_at_zero_and_increments_once_per_update_call() {
        // ユーザー指摘: 「#85のデバッグ情報として、フレームのユニーク番号を取得
        // できるようにしておき」。refresh_debug_logを呼ばない限りdisk I/Oは発生しない
        // (debug_logがNoneのままno-opになる)ので、通常のテストには影響しない。
        let mut game = Game::new(1);
        assert_eq!(game.debug_frame(), 0);
        game.update(Duration::from_millis(16));
        assert_eq!(game.debug_frame(), 1);
        game.update(Duration::from_millis(16));
        assert_eq!(game.debug_frame(), 2);
    }

    #[test]
    fn new_with_width_generates_a_board_of_the_requested_width_and_centers_the_player() {
        // ユーザー指摘: 「設定値に列の数を変更できるようにして」。指定した列数で
        // 盤面が生成され、プレイヤーの開始列もその幅の中央に合わせ直されることを確認する。
        let game = Game::new_with_width(33, 8);
        assert_eq!(game.board.width(), 8);
        for row in &game.board.rows {
            assert_eq!(row.len(), 8, "各行の長さも指定した列数と一致するはず");
        }
        assert_eq!(game.player.col, 4);
    }

    #[test]
    fn new_with_width_clamps_out_of_range_values() {
        let too_narrow = Game::new_with_width(34, 1);
        assert_eq!(too_narrow.board.width(), crate::constants::FIELD_WIDTH_MIN);

        let too_wide = Game::new_with_width(34, 999);
        assert_eq!(too_wide.board.width(), crate::constants::FIELD_WIDTH_MAX);
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

    // --- ショートカットC: 2色化+結合再計算(TERM独自拡張) ---

    #[test]
    fn debug_unify_nearby_colors_repaints_to_exactly_two_colors_and_never_vanishes() {
        // ユーザー指摘: 「単にブロックの色を2色に変換するだけでよくて、消滅させなくて
        // いい」。ランダムな2色のみへ塗り替えることは行うが、塗り替えによって新たに
        // 4連結以上になった箇所があっても即座には自動消滅させない(色の選択はOS乱数の
        // ため非決定的。十分な回数試行して両方の性質を確認する)。
        let mut saw_four_or_more_connected_and_intact = false;
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

    // --- ボム(TERM独自拡張。#96。ユーザー指摘: 「白ボンが、爆弾をランダムに投げて
    // くるイメージで、敵は出現しないものとする」) ---

    #[test]
    fn pushing_into_a_resting_bomb_rolls_it_further_in_the_move_direction() {
        // ユーザー指摘: 「爆弾はキャラが押したらそっちに転がる」(#149)。
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
    fn pushing_a_bomb_against_a_wall_blocks_the_move() {
        // 押し出し先が塞がっていれば、壁にぶつかった時と同じくその場に留まるはず
        // (TERM独自拡張。#149)。
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
        // (TERM独自拡張。#149。#143の「爆弾は爆弾に重ならない」と一貫させる)。
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
        // 押し出しの対象外(TERM独自拡張。#149)。
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
    fn bomb_advances_through_entering_and_rolling_before_ticking_down_the_fuse() {
        let mut game = Game::new(1);
        clear_board(&mut game);
        game.player.row = 500;
        game.player.col = 5;
        // ボムを盤面の最深行に置く(#140で落下判定が入ったため支えが必要)。Rockで
        // 床を作ると、その床自体が支えを失って落下してしまう(このテストの経過
        // 時間ではRockの揺れ猶予が明けるほど長い)ため、それ自体が常に支持される
        // 最深行を使う。
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

        // Rollingを終えるとSettling(左右に跳ねて落ち着き先を探す段階、#140)へ進む。
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
    }

    #[test]
    fn bomb_explosion_converts_rock_and_diamond_within_blast_range_to_star_but_leaves_air_and_items_untouched()
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
        game.board.rows[520][6] = Cell::Rock { hits: 0 };
        game.board.rows[521][5] = Cell::Diamond;
        game.board.rows[520][4] = Cell::Oxygen;
        game.board.rows[519][5] = Cell::Item(ItemEffect::ClearAbove);

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
            Cell::Item(ItemEffect::ClearAbove),
            "アイテムブロックは爆風の影響を受けないはず"
        );
        assert!(events.contains(&GameEvent::BombExploded));
    }

    #[test]
    fn bomb_explosion_unifies_color_blocks_within_blast_range_to_a_single_shared_color() {
        // ユーザー指摘: 「色ブロックは爆弾の炎によって一色に統一される」(#137)。
        // 爆風内の異なる色のブロックが、爆発後は全て同じ1色になっていることを
        // 確認する(具体的な色はランダムに選ばれるため、色そのものではなく
        // 「全部同じ色になっているか」を検証する)。
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
        // 爆風範囲外(縦距離BOMB_BLAST_ROW_RANGE+1、画面外。#142で横は画面幅全部が
        // 範囲になったため、範囲外を示すには縦方向を使う)。
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
    fn bomb_explosion_unify_that_forms_a_group_of_four_or_more_vanishes_immediately_like_a_landing()
    {
        // ユーザー指摘: 「爆弾で変化した壁は落ちたときと同じ反応を発動させる。
        // つまり４マス以上結合している場合は、消える」(#140)。爆風内の隣接する
        // 4マスの色ブロック(元は別々の色)が一色に統一された結果、4連結以上に
        // なった場合はその場で消滅する(通常の着地時の自動消滅と同じ扱い)ことを
        // 確認する。
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
        // ユーザー指摘: 「爆弾は宙に浮かないように落ちること」(#140)。起爆カウント
        // ダウン中でも、直下が空いていれば1マス落下し、その間はカウントダウンを
        // 進めない(空中で起爆させないため)ことを確認する。
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
        // ユーザー指摘: 「爆弾は宙に浮かないように落ちること」(#140)。Settling中も
        // 直下が空いていれば`BOMB_SETTLE_TICK_MS`ごとに1マスずつ落下することを
        // 確認する。
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
        // ユーザー指摘: 「爆弾は爆弾に重ならないようにする」「爆弾がしたにあったら、
        // はねながら転がること」(#143)。直下が空セルでも、既に他のボムが
        // 占めていれば、そこへは落下せず左右へバウンドするはず。
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
        // ユーザー指摘: 「爆弾がしたにあったら、はねながら転がること」(#143)。
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
        // ユーザー指摘: 「爆弾は爆弾に重ならないようにする」(#143)。ボムはCellグリッド
        // とは別のオーバーレイのため、既存ボムの位置も候補から除外されているかを
        // 確認する。画面内(±STAR_VISIBLE_RANGE_ROWS)を岩で埋め、既存ボムが占める
        // マスと、本当に空いているマスの2つだけをEmptyにすることで、新しいボムが
        // 確実に後者へ設置されることを保証する(RNGのseedによらず決定的)。
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
        // ユーザー指摘: 「爆弾はキャラの頭にぶつかったら別の列にころがっていく」
        // (#144)。直下が空セルでも、プレイヤーがそこに居れば落下せず左右へ
        // バウンドするはず。
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
        // ユーザー指摘: 「爆弾はキャラの頭にぶつかったら別の列にころがっていく」
        // (#144)。起爆カウントダウン中でも、直下にプレイヤーが居ればそこで
        // 静止せず、1 settle tick経過後に左右へバウンドするはず。
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
        game.board.rows[521][5] = Cell::Rock { hits: 0 }; // 支え(#140で落下判定が入ったため必要)
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
        game.board.rows[501][6] = Cell::Rock { hits: 0 }; // 支え(#140で落下判定が入ったため必要)

        let events = game.update(Duration::from_millis(60));

        assert!(events.contains(&GameEvent::BombExploded));
        assert!(
            events.contains(&GameEvent::LifeLost),
            "爆風に巻き込まれたら押し潰し相当のミスになるはず: {events:?}"
        );
    }

    #[test]
    fn bomb_blast_range_now_reaches_across_the_entire_field_width() {
        // ユーザー指摘: 「爆弾の爆発範囲は、横全部...に拡大したい」(#142)。遮るものが
        // 無ければ、フィールド幅の端から端までプレイヤーを巻き込むことを確認する
        // (BOMB_BLAST_COL_RANGE=FIELD_WIDTH_MAX、既定フィールド幅12なら距離11でも届く)。
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
        game.board.rows[521][0] = Cell::Rock { hits: 0 }; // 支え(#140で落下判定が入ったため必要)

        let events = game.update(Duration::from_millis(60));
        assert!(
            events.contains(&GameEvent::LifeLost),
            "フィールド幅の反対端でも爆風に巻き込まれるはず: {events:?}"
        );
    }

    #[test]
    fn bomb_blast_row_range_catches_the_player_within_the_screen_but_not_beyond() {
        // ユーザー指摘: 「縦方向も全部(画面内ね)に拡大したい」(#142)。縦方向は盤面
        // 全体の深度ではなく画面内(BOMB_BLAST_ROW_RANGE=14マス)に限定されるため、
        // その距離ちょうどは巻き込むが、1マス超えたら巻き込まないことを確認する。
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
        game.board.rows[521][5] = Cell::Rock { hits: 0 }; // 支え(#140で落下判定が入ったため必要)

        let events = game.update(Duration::from_millis(60));
        assert!(
            events.contains(&GameEvent::LifeLost),
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
        game.board.rows[521][5] = Cell::Rock { hits: 0 }; // 支え(#140で落下判定が入ったため必要)

        let events = game.update(Duration::from_millis(60));
        assert!(
            !events.contains(&GameEvent::LifeLost),
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
        game.board.rows[521][5] = Cell::Rock { hits: 0 }; // 支え(#140で落下判定が入ったため必要)

        let events = game.update(Duration::from_millis(100));

        assert_eq!(game.bombs.len(), 1, "起爆時間前は消えないはず");
        assert_eq!(game.bombs[0].remaining_ms, BOMB_FUSE_MS - 100);
        assert!(!events.contains(&GameEvent::BombExploded));
    }
}

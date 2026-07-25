//! 仕様書(docs/spec.md 13章 定数一覧)に対応する定数群。
//! Phase1(ノーマルコース シングルプレイ)で使用するものだけを定義する。
//! タイムアタック/ネットワーク対戦向けの定数(TIME_ATTACK_SEED, DISCOVERY_UDP_PORT 等)は
//! それらのモードを実装するフェーズで追加する。

/// フィールド幅(列数)
pub const FIELD_WIDTH: usize = 12;

/// フィールド深さ(行数、m)。現在の実装対象はノーマルコース(1000m)。
pub const FIELD_DEPTH_M: usize = 1000;

/// レベル区切り(spec.md 7章。確定事実「100フィートごとに1レベル」を30mに丸めた値)
pub const LEVEL_STEP_M: usize = 30;

/// 岩ブロックが破壊されるまでの累積ヒット数(spec.md 2章・4章)
pub const ROCK_HITS_TO_BREAK: u8 = 5;

/// 岩ブロック破壊時の酸素減少量(spec.md 2章・6章「20%消費」)
pub const ROCK_BREAK_OXYGEN_PENALTY: f32 = 20.0;

/// 酸素ゲージ上限
pub const OXYGEN_MAX: f32 = 100.0;

/// 酸素自然減少量/秒
pub const OXYGEN_DECAY_PER_SEC: f32 = 2.0;

/// 酸素カプセル取得時の回復量
pub const OXYGEN_CAPSULE_RESTORE: f32 = 50.0;

/// 酸素警告を出し始める残量(spec.md 6章。旧版の20から30へ修正)
pub const OXYGEN_WARNING_THRESHOLD: f32 = 30.0;

/// 直接掘削による消滅1ブロックあたりの得点(spec.md 4.6・7章)
pub const SCORE_PER_DRILLED_BLOCK: u64 = 10;

/// 自動消滅(4個以上の落下連結)1ブロックあたりの得点(spec.md 4.5・7章)
pub const SCORE_PER_AUTO_VANISH_BLOCK: u64 = 30;

/// 酸素カプセルn個目取得時の得点 = n × この値(spec.md 7章)
pub const AIR_CAPSULE_SCORE_STEP: u64 = 100;

/// ダイヤブロック1個あたりの得点(TERM独自拡張)
pub const DIAMOND_SCORE: u64 = 500;

/// 選択可能なライフ数の範囲(spec.md 8章)
pub const LIVES_MIN: u8 = 1;
pub const LIVES_MAX: u8 = 5;
/// 既定ライフ数(spec.md 8章)
pub const LIVES_DEFAULT: u8 = 3;

/// 連結落下判定の論理tick間隔(ms)
pub const FALL_TICK_MS: u64 = 150;

/// 未支持になってから実際に落下し始めるまでの揺れ時間(ms、spec.md 4.3)。
/// 公式の「ブロックは落ちる直前に震える」演出を再現するため300〜500msの目安幅を取る。
pub const SHAKE_DURATION_MS: u64 = 450;

/// `SHAKE_DURATION_MS`を`FALL_TICK_MS`単位に換算した揺れティック数(spec.md 4.3)。
/// 未支持と判定されてから、この数のティックが経過するまでは実際には落下せず
/// 「震えている(shaking)」状態のまま待機する。
pub const SHAKE_TICKS: u8 = (SHAKE_DURATION_MS / FALL_TICK_MS) as u8;

/// ライフ消費で再開した直後の無敵ティック数(TERM独自拡張、spec.md 5章)
pub const INVULNERABILITY_TICKS: u32 = 10;

/// 通常プレイの移動・掘削入力のクールダウン(ms)
pub const INPUT_COOLDOWN_MS: u64 = 80;

/// 落下ブロックに押し潰された際、GameOverオーバーレイを表示するまでの一呼吸
/// (「潰れた」見た目に切り替えておく時間、ms。TERM独自拡張、9章)
pub const CRUSH_FLASH_MS: u64 = 400;

/// プレイヤー移動の見た目補間アニメーションの長さ(ms)。ロジック上の位置(row/col)は
/// 即座に確定するが、描画側だけ前回位置からこの時間をかけて滑らかに追従する
/// (TERM独自拡張、9章)
pub const MOVE_ANIM_DURATION_MS: u64 = 100;

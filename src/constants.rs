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

/// `SHAKE_DURATION_MS`を`FALL_TICK_MS`単位に換算した、既定レートでの揺れティック数
/// (spec.md 4.3)。実行時はGame::update()が`shake_duration_ms`(デバッグショートカットで
/// 調整可能)と`block_fall_tick_ms`から都度この換算を行うため、本体コードはこの定数を
/// 直接使わない。テストコードが既定レートでの揺れティック数を表す簡潔な値として使う。
#[cfg(test)]
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

/// デバッグショートカット: 落下速度(ブロック用・キャラ用それぞれ独立)を1回の
/// +/- 入力でどれだけ増減させるか(ms)。TERM独自拡張・動作確認用。
pub const DEBUG_FALL_TICK_STEP_MS: u64 = 25;
/// デバッグショートカットで調整できる落下速度(tick間隔)の下限(ms)。
pub const DEBUG_FALL_TICK_MS_MIN: u64 = 25;
/// デバッグショートカットで調整できる落下速度(tick間隔)の上限(ms)。
pub const DEBUG_FALL_TICK_MS_MAX: u64 = 600;

/// デバッグショートカット「付近のブロックを2色に揃える」の対象範囲
/// (プレイヤーの行を中心に上下何行を対象にするか)。TERM独自拡張・動作確認用。
/// ユーザー指摘: 「ショートカットCを10画面分に適用」を受け、
/// `ui::render::FIELD_VISIBLE_ROWS`(表示可能な論理行数、14)の10画面分
/// (上下合計140行=半径70行)をカバーする値にしている。
pub const DEBUG_UNIFY_COLORS_RANGE_ROWS: usize = 70;

/// デバッグショートカット: 揺れ時間(`SHAKE_DURATION_MS`相当)を1回の,/.入力で
/// どれだけ増減させるか(ms)。TERM独自拡張・動作確認用・設定ファイルに永続化する。
pub const DEBUG_SHAKE_DURATION_STEP_MS: u64 = 50;
/// デバッグショートカットで調整できる揺れ時間の下限(ms)。0なら揺れ無しで即座に落下する。
pub const DEBUG_SHAKE_DURATION_MS_MIN: u64 = 0;
/// デバッグショートカットで調整できる揺れ時間の上限(ms)。
pub const DEBUG_SHAKE_DURATION_MS_MAX: u64 = 2000;

/// スターブロックの出現率(全深度帯共通、TERM独自拡張。ユーザー指摘: 「画面内に
/// きたら、溶けて自然と消えるスターブロックも欲しい」)。
pub const STAR_SPAWN_PROB: f32 = 0.015;
/// スターブロックが画面内に入ってから溶けて消えるまでのティック数(`FALL_TICK_MS`
/// と同じ間隔で数える。TERM独自拡張)。
pub const STAR_MELT_TICKS: u8 = 6;
/// 「画面内」とみなす、プレイヤー位置からの行範囲(上下±この値、TERM独自拡張)。
/// `ui::render::FIELD_VISIBLE_ROWS`(表示可能な論理行数)に合わせている。
pub const STAR_VISIBLE_RANGE_ROWS: usize = 14;

/// Xブロック(岩)・AIR(酸素カプセル)の出現率設定(%、100=通常の確率のまま。
/// TERM独自拡張。ユーザー指摘: 「設定でXブロックの配分量・AIRの配分量をいじれる
/// ようにしたい。プレイ中でもその数値をいじれるようにしたい」)。設定画面から
/// 調整でき、settings.jsonに永続化する。
pub const SPAWN_RATE_PERCENT_DEFAULT: u32 = 100;
pub const SPAWN_RATE_PERCENT_MIN: u32 = 20;
pub const SPAWN_RATE_PERCENT_MAX: u32 = 300;
pub const SPAWN_RATE_PERCENT_STEP: u32 = 20;

/// プレイ中に配分率(岩/AIR)を変更した際、書き換え対象をプレイヤーの十分先(画面外)
/// に限定するための安全マージン(行数、TERM独自拡張)。既に見えている範囲の地形が
/// 突然変わって見えることを防ぐ。
pub const SPAWN_RATE_REROLL_SAFE_MARGIN_ROWS: usize = 40;

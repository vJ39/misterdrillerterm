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

/// 移動・掘削の入力クールダウンアキュムレータの上限(ms、TERM独自拡張)。
/// `INPUT_COOLDOWN_MS`ぶん貯まった後もキー入力が来ない間は際限なく貯め込まず、
/// この値で頭打ちにする(長時間放置後にまとめて連続入力が即座に通ってしまうのを防ぐ)。
/// ユーザー指摘: 「左右にキャラ走るとき、速くなったり遅くなったりしてる。一定の
/// インターバルで速度が落ちたりする」を受け、キー入力がクールダウン周期と揃わない
/// 場合の「うなり」を軽減するため、多少のオーバーシュートは許容しつつ上限を設ける。
pub const INPUT_COOLDOWN_ACCUM_CAP_MS: u64 = INPUT_COOLDOWN_MS + INPUT_COOLDOWN_MS / 2;

/// 落下ブロックに押し潰された際、GameOverオーバーレイを表示するまでの一呼吸
/// (「潰れた」見た目に切り替えておく時間、ms。TERM独自拡張、9章)。ライフが
/// 残っていて続行する場合は`CRUSH_ASCEND_MS`の演出に置き換わる(そちらの方が
/// 長いため、実質そちらの長さぶん潰れた見た目が続く)。
pub const CRUSH_FLASH_MS: u64 = 400;

/// 押し潰されてもライフが残っている場合の「天に召される」演出の長さ(ms、TERM独自
/// 拡張)。ユーザー指摘: 「潰れたとき、もっとわかりやすいように死んで、一度天に
/// 召される演出をして、ブロックが消える処理されてから、元の位置に復活」。この間
/// ゲームプレイ全体(重力・自由落下・酸素減少・入力)を凍結し、演出が終わった時点で
/// 死亡地点の3列クリア・ライフ減算・酸素回復をまとめて行い、その場で復活する。
/// ライフが0になる(GameOverになる)場合はこの演出を行わず、即座にGameOverダイアログへ進む。
/// ユーザー指摘: 「キャラが死んだら3秒間死んだ演出」を受け、600msから3000msへ延長した。
pub const CRUSH_ASCEND_MS: u64 = 3000;

/// プレイヤー移動の見た目補間アニメーションの長さ(ms)。ロジック上の位置(row/col)は
/// 即座に確定するが、描画側だけ前回位置からこの時間をかけて滑らかに追従する
/// (TERM独自拡張、9章)
pub const MOVE_ANIM_DURATION_MS: u64 = 100;

/// 掘削入力(Space)を押した瞬間から、方向別の掘削アニメーション(上=ピヨンピヨン跳ねる、
/// 左右/下=ドリルをぐいぐい)を表示し続ける長さ(ms、TERM独自拡張)。ユーザー指摘:
/// 「上に掘る時、上向きながらピヨンピヨン跳ねる」「左右に掘る時、横にドリルをぐいぐい」
/// 「下に掘る時、下向きながらドリルをぐいぐい」。
pub const DRILL_ANIM_MS: u64 = 200;
/// 掘削アニメーション中、2フレームを何msごとに切り替えるか(TERM独自拡張)。
pub const DRILL_ANIM_FRAME_MS: u64 = 80;

/// ブロックが消滅した瞬間、一瞬明るくフラッシュしてから背景色へ消えていく演出の
/// 長さ(ms、TERM独自拡張)。ユーザー指摘: 「ブロックが消える瞬間に消える演出して
/// ほしい」。自動消滅(4連結以上)・スター溶解消滅が対象。
pub const BLOCK_VANISH_FLASH_MS: u64 = 200;

/// ブロックが落ち始める直前に移動して間一髪回避した際の「わ〜!」スライダー演出の
/// 長さ(ms、TERM独自拡張)。ユーザー指摘: 「ブロックが落ち始める直前に移動してにげた
/// とき、「わ〜!」ってスライダー(アニメーションしてねキャラ)して切り間に合う感じ」。
pub const DODGE_SLIDE_MS: u64 = 250;

/// 「わ〜!」スライダー直後、キャラが起き上がるまでの硬直インターバル(ms、TERM独自
/// 拡張)。設定画面/デバッグショートカットで調整できる(ユーザー指摘: 「この設定値も
/// 作る」)。
pub const DODGE_RECOVERY_MS_DEFAULT: u64 = 1000;
pub const DODGE_RECOVERY_MS_MIN: u64 = 0;
pub const DODGE_RECOVERY_MS_MAX: u64 = 3000;
pub const DODGE_RECOVERY_MS_STEP: u64 = 100;

/// ヒヤリ回避の監視対象セルの有効期限(ms、TERM独自拡張)。移動前の頭上が実際に
/// 揺れていた場合のみ監視対象になり(誤発動対策。ユーザー指摘: 「そもそも避けてない
/// のに発動してるように見える」)、この時間以内に監視対象セルへブロックが着地した
/// 場合のみスライダー演出を発火する。期限切れの監視は自動的に解除される。
pub const DODGE_DETECT_WINDOW_MS: u64 = 500;

/// デバッグショートカット: 落下速度(ブロック用・キャラ用それぞれ独立)を1回の
/// +/- 入力でどれだけ増減させるか(ms)。TERM独自拡張・動作確認用。
pub const DEBUG_FALL_TICK_STEP_MS: u64 = 25;
/// デバッグショートカットで調整できる落下速度(tick間隔)の下限(ms)。
pub const DEBUG_FALL_TICK_MS_MIN: u64 = 25;
/// デバッグショートカットで調整できる落下速度(tick間隔)の上限(ms)。
pub const DEBUG_FALL_TICK_MS_MAX: u64 = 600;

/// デバッグショートカット「付近のブロックを2色に揃える」の対象範囲
/// (プレイヤーの行を中心に上下何行を対象にするか)。TERM独自拡張・動作確認用。
/// 当初「10画面分」だったが、ユーザー指摘により「3画面分」へ変更した。
/// `ui::render::FIELD_VISIBLE_ROWS`(表示可能な論理行数、14)の3画面分
/// (上下合計42行=半径21行)をカバーする値にしている。
pub const DEBUG_UNIFY_COLORS_RANGE_ROWS: usize = 35;

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
/// スターブロックが画面内に入ってから溶け始めるまでの猶予時間(ms、実時間で数える。
/// TERM独自拡張。ユーザー指摘: 「スターブロックは画面内に見えてから5秒たったら
/// 消えはじめること」。当初5000msだったが、ユーザー指摘: 「スターが画面内にあって
/// 5秒は遅いから2秒にして」を受け2000msへ短縮した)。
///
/// 当初はブロック落下tick数(`FALL_TICK_MS`間隔)で数えていたが、深度に応じて
/// tick間隔自体が短縮される(#50)ため、深いほど溶けるのが速くなってしまい
/// 体感と一致しなかった。ブロック落下tickの間隔とは独立した実時間
/// (ms)で管理することで、深度によらず常に一定の猶予時間になるようにした。
pub const STAR_VISIBLE_GRACE_MS: u32 = 2000;
/// 猶予時間経過後、スターブロックが完全に溶けて消えるまでの所要時間(ms、実時間)。
pub const STAR_MELT_DURATION_MS: u32 = 1000;
/// スターブロックが画面内にある間ずっと(消える前から)キラキラ点滅する周期(ms、
/// 実時間。TERM独自拡張。ユーザー指摘: 「スターブロックは消えるまえからキラキラ
/// してほしい」)。`visible_ms`をこの値で割った商の偶奇で☆/★を切り替える。
pub const STAR_SPARKLE_PERIOD_MS: u32 = 400;
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

/// スターブロックの出現率設定の下限(%、TERM独自拡張)。ユーザー指摘: 「スターブロック
/// 比率0〜」。岩/AIRと異なり、完全に出現させない(0%)設定も許可する。
pub const STAR_SPAWN_RATE_PERCENT_MIN: u32 = 0;

/// ダイヤブロックの出現率設定の下限(%、TERM独自拡張)。スターと同様、完全に
/// 出現させない(0%)設定も許可する。ユーザー指摘: 「ダイヤブロック0%設定」。
pub const DIAMOND_SPAWN_RATE_PERCENT_MIN: u32 = 0;

/// 出現する色ブロックの色数設定(TERM独自拡張)。`ColorKind::ALL`の先頭からこの数だけを
/// 使う。ユーザー指摘: 「出現する色ブロックの色数を設定で選べるようにしたい(1〜4)」。
pub const COLOR_COUNT_MIN: u8 = 1;
pub const COLOR_COUNT_MAX: u8 = 4;
pub const COLOR_COUNT_DEFAULT: u8 = 4;

/// プレイ中に配分率(岩/AIR/スター/ダイヤ)を変更した際、書き換え対象をプレイヤーの十分先
/// (画面外)に限定するための安全マージン(行数、TERM独自拡張)。既に見えている範囲の
/// 地形が突然変わって見えることを防ぐ。
pub const SPAWN_RATE_REROLL_SAFE_MARGIN_ROWS: usize = 40;

// ---------------------------------------------------------------------------
// 深度に応じた難易度カーブ(TERM独自拡張)。ユーザー指摘: 「階層が進むにつれて
// だんだんとブロックの落ちる速度があがり、初期配置されるブロックがあまり結合状態に
// なく、個別でばらばらであり、Xブロックが結合で大量にあったりするようにして、
// 難易度をあげていってほしい」「進むにつれてAIRの減る速度が早い」。
//
// 4項目とも、深度0m〜FIELD_DEPTH_M(1000m)の間で線形に効果を強めていく
// (`depth_fraction`、0.0〜1.0)。数値は初回実装の目安であり、実プレイ後に
// 調整すべきチューニング値(バランス上の「正解」ではない)。
// ---------------------------------------------------------------------------

/// ブロック落下速度が深度によってどこまで速くなるか(倍率。TERM独自拡張)。
/// 深度0mでは設定値(等倍)のまま、深度`FIELD_DEPTH_M`到達時にはtick間隔が
/// この倍率まで短縮される(値が小さいほど速い)。`DEBUG_FALL_TICK_MS_MIN`を
/// 下回ることはない。
pub const FALL_SPEED_DEPTH_MAX_SPEEDUP: f32 = 0.4;

/// 色ブロックの初期配置クラスタリング(隣接色を継承する確率)が、深度によって
/// どこまで弱まるか(TERM独自拡張)。深度0mでは`LEFT_INHERIT_PROB`相当のまとまりの
/// まま、深度`FIELD_DEPTH_M`到達時には完全独立抽選(0.0、バラバラ)になる。
pub const COLOR_CLUSTER_DEPTH_START_PROB: f32 = 0.65;

/// 岩ブロックが隣接岩ブロックにつられて出現しやすくなる「塊化ボーナス」の、
/// 深度による最大値(確率の加算値、TERM独自拡張)。深度0mではボーナス無し
/// (完全独立抽選)、深度`FIELD_DEPTH_M`到達時にはこの値まで加算される。
pub const ROCK_CLUSTER_DEPTH_MAX_BONUS: f32 = 0.5;

/// 酸素自然減少速度が深度によってどこまで速くなるか(倍率、TERM独自拡張)。
/// 深度0mでは`OXYGEN_DECAY_PER_SEC`のまま、深度`FIELD_DEPTH_M`到達時には
/// この倍率まで増加する。
pub const OXYGEN_DECAY_DEPTH_MAX_MULTIPLIER: f32 = 2.5;

/// 深度(m)を0.0(深度0m)〜1.0(深度`FIELD_DEPTH_M`以深)へ線形に正規化する
/// (TERM独自拡張)。難易度カーブ4項目(落下速度・色クラスタリング・岩塊化・
/// 酸素減少)が共通で使う進行度。
pub fn depth_fraction(depth_m: usize) -> f32 {
    (depth_m as f32 / FIELD_DEPTH_M as f32).clamp(0.0, 1.0)
}

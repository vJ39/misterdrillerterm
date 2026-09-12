//! 仕様書(docs/spec.md 13章 定数一覧)に対応する定数群。
//! Phase1(ノーマルコース シングルプレイ)で使用するものだけを定義し、
//! タイムアタック/ネットワーク対戦向けの定数はそれらの実装フェーズで追加する。

/// フィールド幅(列数)の既定値。設定画面から調整でき、settings.jsonに永続化する。
/// プレイ中の盤面は`Board.width`(生成時に決まる実際の列数)を都度参照するため、
/// この定数は「新規ゲーム開始時に使う既定値」としてのみ使う。
pub const FIELD_WIDTH_DEFAULT: usize = 12;
pub const FIELD_WIDTH_MIN: usize = 6;
pub const FIELD_WIDTH_MAX: usize = 20;
pub const FIELD_WIDTH_STEP: usize = 1;

/// フィールド深さ(行数、m)。ノーマルコースのゴール深度であり、難易度カーブ
/// (`depth_fraction`)の正規化基準。コースに関わらずこの値を基準に難易度が上がる。
pub const FIELD_DEPTH_M: usize = 1000;

/// ノーマルコースのゴール深度(m)。`FIELD_DEPTH_M`の別名。
pub const COURSE_NORMAL_DEPTH_M: usize = FIELD_DEPTH_M;

/// イージーコースのゴール深度(m)。モードセレクト画面で選択できる。
pub const COURSE_EASY_DEPTH_M: usize = 500;

/// レベル区切り(spec.md 7章。「100フィートごとに1レベル」を30mに丸めた値)
pub const LEVEL_STEP_M: usize = 30;

/// チェックポイント区切り(m)。7章のレベル進行(30m刻み、表示のみ)とは別の、
/// 到達演出・上部オブジェクト全クリアを伴う100m刻みの独立した節目。
pub const CHECKPOINT_STEP_M: usize = 100;

/// チェックポイント到達時の演出(バナー表示)の表示時間(ms)。
pub const CHECKPOINT_FLASH_MS: u64 = 1800;

/// 各チェックポイント(100mごと)の「地面」の厚み(m)。地面は実際にドリルで掘り抜く
/// 固体の障壁で、到達判定は掘り抜いた地点(depth_m = checkpoint*CHECKPOINT_STEP_M +
/// CHECKPOINT_SAFE_ZONE_M + 1)で成立する(`checkpoint_index_for_depth`参照)。
pub const CHECKPOINT_SAFE_ZONE_M: usize = 5;

/// 地面区間(`CHECKPOINT_SAFE_ZONE_M`)の直後、通常の地形が再開するまでに空ける
/// スキマ(m)。`Cell::Empty`にするが地面テクスチャは付けない(見た目は素の空白)。
pub const CHECKPOINT_ZONE_GAP_M: usize = 5;

/// ボーナスフロア(アイテム/AIRが増量される特別な地面)が発生する深度(m)。
pub const BONUS_FLOOR_DEPTH_M: usize = 500;

/// ボーナスフロアでのC/K/Rアイテム・AIR出現率(%。100=通常のまま、500=5倍相当)。
pub const BONUS_FLOOR_ITEM_AIR_RATE_PERCENT: u32 = 500;

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

/// 酸素警告を出し始める残量(spec.md 6章)
pub const OXYGEN_WARNING_THRESHOLD: f32 = 30.0;

/// 直接掘削による消滅1ブロックあたりの得点(spec.md 4.6・7章)
pub const SCORE_PER_DRILLED_BLOCK: u64 = 10;

/// 自動消滅(4個以上の落下連結)1ブロックあたりの得点(spec.md 4.5・7章)
pub const SCORE_PER_AUTO_VANISH_BLOCK: u64 = 30;

/// 酸素カプセルn個目取得時の得点 = n × この値(spec.md 7章)
pub const AIR_CAPSULE_SCORE_STEP: u64 = 100;

/// ダイヤブロック1個あたりの得点
pub const DIAMOND_SCORE: u64 = 500;

/// 選択可能なライフ数の範囲(spec.md 8章)
pub const LIVES_MIN: u8 = 1;
pub const LIVES_MAX: u8 = 5;
/// 既定ライフ数(spec.md 8章)
pub const LIVES_DEFAULT: u8 = 3;

/// 連結落下判定の論理tick間隔(ms)
pub const FALL_TICK_MS: u64 = 150;

/// 未支持になってから実際に落下し始めるまでの揺れ時間(ms、spec.md 4.3)。
/// 「ブロックは落ちる直前に震える」演出のため、300〜500msの目安幅から採用した値。
pub const SHAKE_DURATION_MS: u64 = 450;

/// `SHAKE_DURATION_MS`を`FALL_TICK_MS`単位に換算した、既定レートでの揺れティック数。
/// 本体コードは実行時に都度換算するため使わず、テストコードのみが使う。
#[cfg(test)]
pub const SHAKE_TICKS: u8 = (SHAKE_DURATION_MS / FALL_TICK_MS) as u8;

/// ライフ消費で再開した直後の無敵ティック数(spec.md 5章)
pub const INVULNERABILITY_TICKS: u32 = 10;

/// 通常プレイの移動・掘削入力のクールダウン(ms)
pub const INPUT_COOLDOWN_MS: u64 = 80;

/// 入力クールダウンアキュムレータの上限(ms)。キー入力が来ない間も際限なく貯め込まず
/// 頭打ちにし、放置後にまとめて連続入力が即座に通るのを防ぎつつ、キー入力と
/// クールダウン周期のズレによる移動速度の「うなり」を軽減する。
pub const INPUT_COOLDOWN_ACCUM_CAP_MS: u64 = INPUT_COOLDOWN_MS + INPUT_COOLDOWN_MS / 2;

/// 横移動(MoveLeft/MoveRight)のクールダウン間隔設定(ms、小さいほど速い)。
/// 掘削(Drill)は`INPUT_COOLDOWN_MS`固定で対象外。設定画面から調整でき、
/// settings.jsonに永続化する。
pub const MOVE_COOLDOWN_MS_DEFAULT: u64 = INPUT_COOLDOWN_MS;
pub const MOVE_COOLDOWN_MS_MIN: u64 = 20;
pub const MOVE_COOLDOWN_MS_MAX: u64 = 300;
pub const MOVE_COOLDOWN_MS_STEP: u64 = 20;

/// 押し潰され時、GameOverオーバーレイ表示前に「潰れた」見た目を見せる一呼吸(ms)。
/// ライフが残って続行する場合はより長い`CRUSH_ASCEND_MS`の演出に置き換わる。
pub const CRUSH_FLASH_MS: u64 = 400;

/// 押し潰されてもライフが残る場合の「天に召される」演出の長さ(ms)。この間ゲーム全体
/// (重力・自由落下・酸素減少・入力)を凍結し、終了時に死亡地点の3列クリア・ライフ減算・
/// 酸素回復をまとめて行いその場で復活する。ライフ0の場合は演出せず即GameOverへ進む。
pub const CRUSH_ASCEND_MS: u64 = 3000;

/// プレイヤー移動の見た目補間アニメーションの長さ(ms)。ロジック上の位置(row/col)は
/// 即座に確定し、描画側だけ前回位置からこの時間をかけて滑らかに追従する。
pub const MOVE_ANIM_DURATION_MS: u64 = 100;

/// 掘削入力を押した瞬間から、方向別の掘削アニメーション(上=跳ねる、左右/下=ドリルを
/// ぐいぐい)を表示し続ける長さ(ms)。
pub const DRILL_ANIM_MS: u64 = 200;
/// 掘削アニメーション中、2フレームを切り替える間隔(ms)。
pub const DRILL_ANIM_FRAME_MS: u64 = 80;

/// ブロック消滅時、一瞬明るくフラッシュしてから背景色へ消えていく演出の長さ(ms)。
/// 自動消滅(4連結以上)・スター溶解消滅が対象。
pub const BLOCK_VANISH_FLASH_MS: u64 = 200;

/// 自動消滅の連鎖で、1回消滅した後もう一段の重力解決へ進むまでの最小インターバル(ms)。
/// 0より大きいと消滅直後の重力tickをこの時間だけ足止めし、連鎖の一段ごとに一拍おいて
/// 見える。既定0=間を空けない(従来通り)。設定画面から調整する。
pub const CHAIN_VANISH_INTERVAL_MS_DEFAULT: u64 = 0;
pub const CHAIN_VANISH_INTERVAL_MS_MIN: u64 = 0;
pub const CHAIN_VANISH_INTERVAL_MS_MAX: u64 = 1000;
pub const CHAIN_VANISH_INTERVAL_MS_STEP: u64 = 50;

/// ブロックが落ち始める直前に移動して間一髪回避した際の「わ〜!」スライダー演出の長さ(ms)。
pub const DODGE_SLIDE_MS: u64 = 250;

/// 「わ〜!」スライダー直後、キャラが起き上がるまでの硬直インターバル(ms)。
/// 設定画面/デバッグショートカットで調整できる。
pub const DODGE_RECOVERY_MS_DEFAULT: u64 = 1000;
pub const DODGE_RECOVERY_MS_MIN: u64 = 0;
pub const DODGE_RECOVERY_MS_MAX: u64 = 3000;
pub const DODGE_RECOVERY_MS_STEP: u64 = 100;

/// ヒヤリ回避の監視対象セルの有効期限(ms)。移動前の頭上が実際に揺れていた場合のみ
/// 監視対象になり(誤発動対策)、この時間以内に監視対象セルへブロックが着地した場合のみ
/// スライダー演出を発火する。期限切れの監視は自動的に解除される。
pub const DODGE_DETECT_WINDOW_MS: u64 = 500;

/// デバッグショートカット: 落下速度(ブロック用・キャラ用それぞれ独立)を
/// 1回の+/-入力でどれだけ増減させるか(ms)。
pub const DEBUG_FALL_TICK_STEP_MS: u64 = 25;
/// デバッグショートカットで調整できる落下速度(tick間隔)の下限(ms)。
pub const DEBUG_FALL_TICK_MS_MIN: u64 = 25;
/// デバッグショートカットで調整できる落下速度(tick間隔)の上限(ms)。
pub const DEBUG_FALL_TICK_MS_MAX: u64 = 600;

/// デバッグショートカット「付近のブロックを2色に揃える」の対象範囲(プレイヤーの行を
/// 中心に上下何行か)。`ui::render::FIELD_VISIBLE_ROWS`(14)の3画面分
/// (上下合計42行=半径21行)をカバーする値にしている。
pub const DEBUG_UNIFY_COLORS_RANGE_ROWS: usize = 35;

/// デバッグショートカット: 揺れ時間(`SHAKE_DURATION_MS`相当)を1回の,/.入力で
/// どれだけ増減させるか(ms)。設定ファイルに永続化する。
pub const DEBUG_SHAKE_DURATION_STEP_MS: u64 = 50;
/// デバッグショートカットで調整できる揺れ時間の下限(ms)。0なら揺れ無しで即座に落下する。
pub const DEBUG_SHAKE_DURATION_MS_MIN: u64 = 0;
/// デバッグショートカットで調整できる揺れ時間の上限(ms)。
pub const DEBUG_SHAKE_DURATION_MS_MAX: u64 = 2000;

/// スターブロック(画面内に入ると溶けて自然消滅する)の出現率(全深度帯共通)。
pub const STAR_SPAWN_PROB: f32 = 0.015;
/// スターブロックが画面内に入ってから溶け始めるまでの猶予時間(ms)。ブロック落下tickの
/// 間隔は深度で短縮されるため、tick数ではなく実時間で数え、深度によらず一定の猶予にする。
pub const STAR_VISIBLE_GRACE_MS: u32 = 2000;
/// 猶予時間経過後、スターブロックが完全に溶けて消えるまでの所要時間(ms、実時間)。
pub const STAR_MELT_DURATION_MS: u32 = 1000;
/// スターブロックが画面内にある間ずっと(消える前から)キラキラ点滅する周期(ms、実時間)。
/// `visible_ms`をこの値で割った商の偶奇で☆/★を切り替える。
pub const STAR_SPARKLE_PERIOD_MS: u32 = 400;
/// 「画面内」とみなす、プレイヤー位置からの行範囲(上下±この値)。
/// `ui::render::FIELD_VISIBLE_ROWS`(表示可能な論理行数)に合わせている。
pub const STAR_VISIBLE_RANGE_ROWS: usize = 14;

/// プレイヤーの現在行より、実際に画面内に見えている上側の行数。
/// `ui::render::FIELD_VISIBLE_ROWS`(14)×プレイヤー表示位置比(1/3)の計算結果(4)。
/// 頭上クリアで画面外のAIR/アイテムを画面内へ移動させる際の到達行の基準に使う。
pub const PLAYER_SCREEN_ROWS_ABOVE: usize = 4;

/// アイテムブロック(R効果=頭上クリア/C効果=2色化)の出現率。画面全体に効果が及ぶ
/// 強力な効果のため、スター(`STAR_SPAWN_PROB`=0.015)より大幅に低い固定値とする。
pub const ITEM_CLEAR_ABOVE_SPAWN_PROB: f32 = 0.003;
pub const ITEM_UNIFY_COLORS_SPAWN_PROB: f32 = 0.003;
/// ショートカットKと同じ効果(画面内X/ダイヤ100%スター化)を持つアイテムの出現率。
pub const ITEM_STARIFY_SCREEN_SPAWN_PROB: f32 = 0.003;

/// アイテムブロック3種の出現率設定の下限(%)。0%(完全に出現させない)も許可する。
/// 上限・既定値・刻み幅は`SPAWN_RATE_PERCENT_MAX/DEFAULT/STEP`を共用する。
pub const ITEM_SPAWN_RATE_PERCENT_MIN: u32 = 0;

/// アイテムブロック1種類あたり、`ITEM_WINDOW_AHEAD_ROWS`の窓内で同時に存在できる
/// 最大個数。窓は深度が進むにつれてスライドし、プレイヤーより後ろに落ちたアイテムは
/// 窓の外になるため、その分だけ前方に新規出現の余地が生まれる。
pub const ITEM_MAX_COUNT_ON_BOARD: usize = 10;

/// アイテム3種を「プレイヤーの少し先で常に`ITEM_MAX_COUNT_ON_BOARD`個」へ補充する
/// 窓の広さ(行数)。現在行からこの行数先までの既存個数を数え、上限未満なら不足分を
/// 追加抽選する。値が大きいほど同じ上限が広い範囲に分散し、窓内の密度は薄まる。
pub const ITEM_WINDOW_AHEAD_ROWS: usize = 100;

// ---------------------------------------------------------------------------
// ボム爆発イベント。移動する敵キャラは持たず、盤面上にランダムに出現するボムが
// 一定時間後に爆発し、Xブロック・ダイヤブロックをスターブロックへ変える。
// 爆風にプレイヤーが触れると押し潰し相当のミスになる。
// ---------------------------------------------------------------------------

/// ボム設置から爆発までの時間(ms、Ticking段階のみで経過する)。他の演出と同じく、
/// 爆発前に必ず視覚的な予兆(点滅)を出す設計方針を踏襲する。
pub const BOMB_FUSE_MS: u32 = 5000;

/// 起爆間際、本体が激しく赤く点滅し始める残り時間の閾値(ms)。`ui::render`の点滅開始と
/// `game`側の導火線カウントダウンSEが同じタイミングで始まるよう、両者で共有する。
pub const BOMB_DANGER_MS: u32 = 1000;

/// 危険域(残り`BOMB_DANGER_MS`以下)の間、導火線の連続「チッ」音を鳴らす間隔(ms)。
/// 危険域に入った瞬間1回だけの駆け上がり4音(`play_bomb_fuse_warning`)とは別に、
/// 危険域が続く間ずっと鳴り続けて爆発が近いことを連続音で煽る。
pub const BOMB_FUSE_TICK_INTERVAL_MS: u32 = 200;

/// 白ボンが画面端に登場してからボムを投げるまでの時間(ms)。
pub const BOMB_ENTER_MS: u32 = 400;

/// 投げられたボムが画面端から最終設置マスまで転がる時間(ms)。
pub const BOMB_ROLL_MS: u32 = 400;

/// ボムの爆風が上下へ伸びる距離(マス数)。「画面内」の縦方向全域に限定するため、
/// `ui::render::FIELD_VISIBLE_ROWS`(14)に合わせている。途中にXブロック・ダイヤ
/// ブロックがあっても遮蔽されずこの距離まで届く。
pub const BOMB_BLAST_ROW_RANGE: usize = 14;

/// ボムの爆風が左右へ伸びる距離(マス数)。`FIELD_WIDTH_MAX`(20)に合わせ、爆心地が
/// どちらに寄っていても盤面の端まで確実に届く(実際の到達は盤面境界でのみクリップされ、
/// 遮蔽は無いため常に「横全部」になる)。
pub const BOMB_BLAST_COL_RANGE: usize = FIELD_WIDTH_MAX;

/// 盤面全体で同時に存在できるボムの最大数。
pub const BOMB_MAX_COUNT_ON_BOARD: usize = 10;

/// ボム出現の判定間隔(ms)。この間隔が経過するたびに出現確率を1回判定する。
pub const BOMB_SPAWN_CHECK_INTERVAL_MS: u64 = 1000;

/// ボム出現の基礎確率(判定間隔ごと、深度0m時点)。設定の出現頻度(%)を乗算する。
pub const BOMB_SPAWN_BASE_PROB: f32 = 0.05;

/// ボム出現確率が深度によってどこまで強まるか(加算値)。深度0mでは
/// `BOMB_SPAWN_BASE_PROB`のまま、深度`FIELD_DEPTH_M`到達時にはこの値まで加算される。
pub const BOMB_SPAWN_DEPTH_MAX_BONUS: f32 = 0.10;

/// ボム出現頻度設定の下限(%)。他の出現率設定と同様、0%で完全に出現しなくもできる。
pub const BOMB_SPAWN_RATE_PERCENT_MIN: u32 = 0;

/// ボム出現頻度設定の上限(%)。毎回の判定確率が1.0に飽和するのは基礎確率の逆数
/// `1.0 / BOMB_SPAWN_BASE_PROB = 20倍`(=2000%)であり、これ以上は同時存在数の上限
/// (`BOMB_MAX_COUNT_ON_BOARD`)もあって意味を持たないため、2000%を上限とする。
pub const BOMB_SPAWN_RATE_PERCENT_MAX: u32 = 2000;
/// ボム出現頻度設定の調整刻み幅(%)。上限2000%を20ステップで調整できる100刻み。
pub const BOMB_SPAWN_RATE_PERCENT_STEP: u32 = 100;

/// ボム爆発時、爆風が届いたセルに炎の演出を表示する時間(ms)。この間はスター変換後の
/// 見た目を炎の色で覆い、消滅フラッシュ(`BLOCK_VANISH_FLASH_MS`)と同じ考え方で
/// 爆発の瞬間を視覚的に強調する。
pub const BOMB_EXPLOSION_FLASH_MS: u64 = 350;

/// 転がり終えた直後、支えを失っていれば落下しつつ左右に跳ねて落ち着き先を探す
/// (`BombPhase::Settling`)時間の合計(ms)。経過したらその位置で`BombPhase::Ticking`
/// (起爆カウントダウン)へ進む。
pub const BOMB_SETTLE_MS: u32 = 600;
/// Settling中に1歩(1マス)ぶん移動する間隔(ms)。
pub const BOMB_SETTLE_TICK_MS: u32 = 80;

/// Xブロック(岩)・AIR(酸素カプセル)の出現率設定(%、100=通常の確率のまま)。
/// 設定画面からプレイ中でも調整でき、settings.jsonに永続化する。
pub const SPAWN_RATE_PERCENT_DEFAULT: u32 = 100;
pub const SPAWN_RATE_PERCENT_MIN: u32 = 20;
pub const SPAWN_RATE_PERCENT_MAX: u32 = 300;
pub const SPAWN_RATE_PERCENT_STEP: u32 = 20;

/// スターブロックの出現率設定の下限(%)。岩/AIRと異なり0%(完全に出現させない)も許可する。
pub const STAR_SPAWN_RATE_PERCENT_MIN: u32 = 0;

/// スターブロックの出現率設定の上限(%)。基礎確率`STAR_SPAWN_PROB`(0.015)がマス単体
/// 確率の上限クランプ(0.9)にちょうど到達する倍率(0.9 / 0.015 = 60倍)。これ以上は
/// 最終確率が0.9で飽和するだけで見た目が変わらないため、意味を持つ最大値がこれになる。
pub const STAR_SPAWN_RATE_PERCENT_MAX: u32 = 6000;
/// スターブロックの出現率設定の調整刻み幅(%)。上限が大きいため専用の大きな刻みにする。
pub const STAR_SPAWN_RATE_PERCENT_STEP: u32 = 300;

/// ダイヤブロックの出現率設定の下限(%)。スターと同様、0%(完全に出現させない)も許可する。
pub const DIAMOND_SPAWN_RATE_PERCENT_MIN: u32 = 0;

/// 色ブロックの結合しやすさ(隣接色を継承する確率`COLOR_CLUSTER_DEPTH_START_PROB`に
/// 乗算する係数%)の設定の下限。0%(常に均等ランダム抽選、完全にバラバラ)も許可する。
pub const COLOR_CLUSTER_RATE_PERCENT_MIN: u32 = 0;

/// 出現する色ブロックの色数設定(1〜4)。`ColorKind::ALL`の先頭からこの数だけを使う。
pub const COLOR_COUNT_MIN: u8 = 1;
pub const COLOR_COUNT_MAX: u8 = 4;
pub const COLOR_COUNT_DEFAULT: u8 = 4;

/// プレイ中に配分率(岩/AIR/スター/ダイヤ)を変更した際、再抽選の対象をプレイヤーの
/// 十分先(画面外)に限定する安全マージン(行数)。見えている地形が突然変わるのを防ぐ。
/// 縮退表示では可視行数がターミナル実高さから動的に増えるため、それを確実に上回る値
/// (200なら402行超のターミナル高さが必要=現実的に起こり得ない)にしている。
pub const SPAWN_RATE_REROLL_SAFE_MARGIN_ROWS: usize = 200;

// ---------------------------------------------------------------------------
// 深度に応じた難易度カーブ。4項目とも、深度0m〜FIELD_DEPTH_M(1000m)の間で線形に
// 効果を強めていく(`depth_fraction`、0.0〜1.0)。数値は実プレイを踏まえて調整する
// 前提のチューニング値。
// ---------------------------------------------------------------------------

/// ブロック落下速度が深度によってどこまで速くなるか(tick間隔への倍率、小さいほど速い)。
/// 深度0mでは設定値(等倍)のまま、深度`FIELD_DEPTH_M`到達時にはこの倍率まで短縮される。
/// `DEBUG_FALL_TICK_MS_MIN`を下回ることはない。
pub const FALL_SPEED_DEPTH_MAX_SPEEDUP: f32 = 0.4;

/// 色ブロックの初期配置クラスタリング(隣接色を継承する確率)の深度0m時点の値。
/// 深度`FIELD_DEPTH_M`到達時には完全独立抽選(0.0、バラバラ)まで弱まる。
pub const COLOR_CLUSTER_DEPTH_START_PROB: f32 = 0.65;

/// 岩ブロックが隣接岩ブロックにつられて出現しやすくなる「塊化ボーナス」の、深度による
/// 最大加算値。最深帯の基礎岩出現率0.20と合わせても隣接セルの岩化確率を最大0.40に抑え、
/// パーコレーション閾値(2次元格子で約0.59)超えによる「画面全体が岩で埋まる」状態を防ぐ。
pub const ROCK_CLUSTER_DEPTH_MAX_BONUS: f32 = 0.2;

/// 酸素自然減少速度が深度によってどこまで速くなるか(倍率)。深度0mでは
/// `OXYGEN_DECAY_PER_SEC`のまま、深度`FIELD_DEPTH_M`到達時にはこの倍率まで増加する。
pub const OXYGEN_DECAY_DEPTH_MAX_MULTIPLIER: f32 = 2.5;

/// 深度(m)を0.0(深度0m)〜1.0(深度`FIELD_DEPTH_M`以深)へ線形に正規化する。
/// 難易度カーブ4項目(落下速度・色クラスタリング・岩塊化・酸素減少)が共通で使う進行度。
pub fn depth_fraction(depth_m: usize) -> f32 {
    (depth_m as f32 / FIELD_DEPTH_M as f32).clamp(0.0, 1.0)
}

/// 盤面スナップショットログを記録するティック間隔。`block_events`は動いた/消えたセル
/// しか記録しないため、定期的にプレイヤー周辺の非Emptyセルを丸ごと記録しておき、
/// 「一度も動いていないセルが生成時からの地形か」を後から確認できるようにする。
pub const BOARD_SNAPSHOT_TICK_INTERVAL: u64 = 10;

/// 盤面スナップショットで記録する、プレイヤーより浅い(画面上で上にある)側の行数。
/// 浮きブロック等の調査対象はプレイヤーの数十行上で起きることがあるため画面外まで広めに取る。
pub const BOARD_SNAPSHOT_ROWS_ABOVE_PLAYER: usize = 40;

/// 盤面スナップショットで記録する、プレイヤーより深い(画面上で下にある)側の行数。
pub const BOARD_SNAPSHOT_ROWS_BELOW_PLAYER: usize = 5;

// ---------------------------------------------------------------------------
// オートプレイ(#218)。長時間プレイでしか出ない稀なバグを再現・検出するソークテスト
// 用のデバッグ機能。無敵(`Game::set_invincible`)と、盤面から次の一手を決める
// 貪欲AI(`autoplay::Autopilot`)の2つを独立したトグルとして持つ。
// ---------------------------------------------------------------------------

/// メインループの目安フレーム間隔(ms。spec.md 9章 ポーリング間隔目安16〜33ms=
/// 30〜60fps相当)。オートプレイ側が「何フレーム待ったか」を実時間へ換算する際にも
/// 参照するため、main.rsのローカル定数ではなくここで一元管理する。
pub const FRAME_INTERVAL_MS: u64 = 33;

/// 列採点で先読みする、プレイヤーより深い側の行数。この行数までの「岩に当たらず
/// 掘り進める連続行数」が列スコアの基礎点になる。
pub const AUTOPLAY_LOOKAHEAD_ROWS: usize = 14;

// --- 酸素の使いどころ(#221) -------------------------------------------------
// 岩1個の破壊は酸素20%+5ヒット分の時間を使い、深度0mなら70行ぶん・最深でも30行ぶんの
// 下降と釣り合う。対して横1列の迂回は1行ぶんにも満たない。どちらが安いかを固定値では
// なく`rock_cost_rows`/`detour_cost_rows`の比較で毎回決めるため、閾値の定数は置かず、
// 判断に必要な「AIRを拾う価値」「緊急とみなす残量」だけを定数にする。

/// AIRへ寄り道する価値があるとみなす最低回復量(%)。回復量は`min(50, 100-残量)`で
/// 上限クランプされるため、満タンに近いほど小さくなる。これを下回るAIRは無視する。
pub const AUTOPLAY_AIR_MIN_GAIN: f32 = 10.0;

/// 平常時にAIRのために横へ逸れてよい最大列数。これを超えて離れたAIRは列スコアへ
/// 加点しない(緊急時はこの制限を外す)。
pub const AUTOPLAY_AIR_DETOUR_MAX_COLS: usize = 4;

/// 「この秒数ぶんの自然減少を賄えるか」で緊急判定する地平線(秒)。残量が
/// `深度別の減少速度 × この秒数`を下回ったら緊急とみなす。深度0mで12%、最深で30%
/// (=`OXYGEN_WARNING_THRESHOLD`)に一致する。
pub const AUTOPLAY_EMERGENCY_HORIZON_SEC: f32 = 6.0;

/// 緊急時にAIRを探す先読み行数。平常時(`AUTOPLAY_LOOKAHEAD_ROWS`)より深くまで見る。
pub const AUTOPLAY_EMERGENCY_AIR_LOOKAHEAD_ROWS: usize = 28;

/// 緊急時にAIR加点へ掛ける倍率。多少遠回りでも確保しに行かせる。
pub const AUTOPLAY_EMERGENCY_AIR_SCORE_MULTIPLIER: f32 = 3.0;

// --- 列スコアリング(#221) ---------------------------------------------------
// 単位は「行」。基礎点が`clear_run`(掘り進める行数、最大`AUTOPLAY_LOOKAHEAD_ROWS`)
// なので、各加減点も「何行ぶんの価値か」で揃えている。

/// AIRの実効回復量(%)を列スコア(行単位)へ換算する除数。回復50%(=残量50%以下)で
/// 16.7行ぶんとなり、先読み範囲いっぱいの直進(14行)より価値が高くなる。逆に
/// `AUTOPLAY_AIR_MIN_GAIN`(10%)ちょうどなら3.3行ぶんで、3列より遠い寄り道には
/// 見合わなくなる。「残量が減るほど寄り道の許容距離が伸びる」挙動がこの1つの除数で決まる。
pub const AUTOPLAY_SCORE_AIR_DIVISOR: f32 = 3.0;

/// 経路上のアイテムブロック(頭上クリア/スター化)への加点。どちらも進路を大きく
/// 拓くため、先読み範囲いっぱい(14行)に迫る価値を与える。2色化は効果が進路と
/// 無関係なため加点しない。
pub const AUTOPLAY_SCORE_ITEM_BONUS: f32 = 10.0;

/// 横に1列離れるごとの減点。移動そのものの所要時間より大きめに置き、僅かな得点差で
/// ふらふら横移動しないようにする。
pub const AUTOPLAY_SCORE_LATERAL_PER_COL: f32 = 1.0;

/// 空洞へ飛び込んで自由落下する列への、無防備な1行あたりの減点係数。
///
/// 自由落下中は横移動が効かず、掘っても落下は速くならない。しかもブロックの落下tickは
/// 深度で最大2.5倍まで短くなるのにプレイヤーの自由落下tickは一定なので、深いほど落下中に
/// 頭上の塊との差を詰められる。実際の減点はこの係数×「落下中に詰められる行数」なので、
/// 深度0m(両者同速)では0になり、深いほど自動的に効くようになる。
pub const AUTOPLAY_SCORE_VOID_EXPOSURE: f32 = 1.0;

/// 頭上に落下予定の塊を抱えた列への減点の最大値。余裕が少ないほど満額に近づく
/// (`AUTOPLAY_THREAT_MIN_SLACK_ROWS`を割り込む列はそもそも候補から外す)。
pub const AUTOPLAY_SCORE_THREAT_PENALTY: f32 = 6.0;

/// 頭上の塊との間に最低限保っておく余裕(行数)の下限。1手ぶん動いた後に残る行数で測る。
///
/// 実際の必要量は「1手動くのに要する時間 ÷ ブロックの落下tick」の
/// `AUTOPLAY_THREAT_REACTION_STEPS`手ぶんで、深いほど大きくなる(ブロックの落下tickだけが
/// 深度で短くなるため、同じ時間でも詰められる行数が増える)。この定数はその下限。
pub const AUTOPLAY_THREAT_MIN_SLACK_ROWS: f32 = 3.0;

/// 頭上の塊から逃げ始めるまでに、何手ぶんの余裕を残しておくか。
///
/// 1手で足りるように見えても、入力クールダウンが明けるのを待つ空振りフレームが挟まるため
/// 実際には間に合わない(実測した押し潰しの6割強が「危険は検知できているのに、逃げる
/// 入力が通る前に潰される」パターンだった)。
pub const AUTOPLAY_THREAT_REACTION_STEPS: f32 = 3.0;

/// 列採点の候補範囲(現在列の左右何列まで見るか)。escalation1以上・緊急時は全幅へ広げる。
pub const AUTOPLAY_COLUMN_SCAN_RADIUS: usize = 4;

/// 目的列を乗り換えるのに必要なスコア差(ヒステリシス)。これ未満の差では今の目的列を
/// 保ち、AIRと危険回避の間で左右に往復するのを防ぐ。
pub const AUTOPLAY_COLUMN_SWITCH_MARGIN: f32 = 3.0;

/// 頭上の落下脅威を探す行数(画面高さぶん)。支持された固体ブロックに当たった時点で
/// 遮蔽されているとみなして打ち切る。
pub const AUTOPLAY_THREAT_SCAN_ROWS: usize = 14;

/// 位置が変わらないままこのフレーム数が過ぎたら、行動の段階(escalation)を1つ上げる。
pub const AUTOPLAY_STUCK_FRAMES: u32 = 90;

/// 行(深度)が進まないままこのフレーム数が過ぎたら、行動の段階(escalation)を1つ上げる。
/// 横移動が自由になると位置(row,col)は変わり続けるため、位置ベースの
/// `AUTOPLAY_STUCK_FRAMES`だけでは「同じ行を横に往復し続ける」手詰まりを検出できない。
/// 33ms/フレーム換算で約9秒。
pub const AUTOPLAY_DESCENT_WATCHDOG_FRAMES: u32 = 270;

/// ボムの起爆残り時間がこれ以下になったら回避行動へ移る(ms)。
pub const AUTOPLAY_BOMB_EVADE_MS: u32 = 2500;

/// 無敵OFFのままGameOverになった場合、自動でReviveするまでの待ち時間(ms)。
pub const AUTOPLAY_REVIVE_DELAY_MS: u64 = 1500;

/// タイトル画面で無操作のままこの時間が過ぎたら、アトラクトモード(自動デモプレイ)を
/// 開始する(ms)。
pub const ATTRACT_MODE_IDLE_MS: u64 = 30000;

// ---------------------------------------------------------------------------
// SE/MUSIC音量調整(#224)。既存のON/OFF(M/Eキー・設定画面トグル)とは別に、
// アプリ内部のミックスゲインだけを0〜100%(10%刻み)で調整できるようにする。
// OS/システム側の音量には一切触れない。
// ---------------------------------------------------------------------------

/// SE/MUSIC音量(%)。100=現在のミックス(sfx::SE_VOLUME / bgm::BGM_VOLUME)そのまま。
/// アプリ内部のゲインのみを変え、OS側のシステム音量には一切触れない。
pub const SOUND_VOLUME_PERCENT_DEFAULT: u32 = 100;
pub const SOUND_VOLUME_PERCENT_MIN: u32 = 0;
pub const SOUND_VOLUME_PERCENT_MAX: u32 = 100;
pub const SOUND_VOLUME_PERCENT_STEP: u32 = 10;

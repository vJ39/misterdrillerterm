//! 仕様書(docs/spec.md 13章 定数一覧)に対応する定数群。
//! Phase1(ノーマルコース シングルプレイ)で使用するものだけを定義する。
//! タイムアタック/ネットワーク対戦向けの定数(TIME_ATTACK_SEED, DISCOVERY_UDP_PORT 等)は
//! それらのモードを実装するフェーズで追加する。

/// フィールド幅(列数)
pub const FIELD_WIDTH: usize = 12;

/// フィールド深さ(行数、m)
pub const FIELD_DEPTH_M: usize = 1000;

/// 酸素ゲージ上限
pub const OXYGEN_MAX: f32 = 100.0;

/// 酸素自然減少量/秒
pub const OXYGEN_DECAY_PER_SEC: f32 = 2.0;

/// 酸素カプセル取得時の回復量
pub const OXYGEN_CAPSULE_RESTORE: f32 = 50.0;

/// 酸素警告音を鳴らし始める残量
pub const OXYGEN_WARNING_THRESHOLD: f32 = 20.0;

/// ダイヤブロック1個あたりの得点
pub const DIAMOND_SCORE: u64 = 500;

/// 深度(m)あたりのスコア倍率
pub const DEPTH_SCORE_MULTIPLIER: u64 = 10;

/// チェックポイント1件の定義(深度・タイムボーナス計算に使う基準タイム・基礎ボーナス)。
/// spec.md 7章。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Checkpoint {
    /// 到達深度(m)
    pub depth_m: usize,
    /// タイムボーナスの基準タイム(秒)
    pub base_time_sec: f32,
    /// 基礎ボーナス(点)
    pub base_bonus: u64,
}

/// チェックポイント一覧(深度昇順)。深度・基準タイム・基礎ボーナスを1箇所にまとめ、
/// 個数や並び順を変える際に複数配列を手で同期させる必要をなくす。
pub const CHECKPOINTS: [Checkpoint; 5] = [
    Checkpoint {
        depth_m: 200,
        base_time_sec: 40.0,
        base_bonus: 1000,
    },
    Checkpoint {
        depth_m: 400,
        base_time_sec: 90.0,
        base_bonus: 1000,
    },
    Checkpoint {
        depth_m: 600,
        base_time_sec: 150.0,
        base_bonus: 1000,
    },
    Checkpoint {
        depth_m: 800,
        base_time_sec: 220.0,
        base_bonus: 1000,
    },
    Checkpoint {
        depth_m: 1000,
        base_time_sec: 300.0,
        base_bonus: 2000,
    },
];

/// 連結落下判定の論理tick間隔(ms)
pub const FALL_TICK_MS: u64 = 150;

/// 通常プレイの移動入力クールダウン(ms)
pub const INPUT_COOLDOWN_MS: u64 = 80;

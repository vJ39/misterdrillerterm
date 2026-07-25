//! プレイヤー状態(位置・酸素・スコア・経過タイム)。spec.md 6〜7章。

use crate::constants::{FIELD_WIDTH, OXYGEN_MAX};

/// プレイヤーの現在状態。
#[derive(Debug, Clone)]
pub struct Player {
    /// 現在いる行(=盤面の行インデックス)
    pub row: usize,
    /// 現在いる列
    pub col: usize,
    /// 酸素残量(内部値。f32で保持し表示は整数値に切り捨てる。spec.md 6章)
    pub oxygen: f32,
    /// 取得したダイヤ数
    pub diamonds_collected: u32,
    /// ダイヤ取得による加算スコア累計
    pub diamond_score: u64,
    /// チェックポイントのタイムボーナス累計
    pub time_bonus_total: u64,
    /// 経過タイム(秒)
    pub elapsed_seconds: f32,
    /// 到達済みチェックポイント深度(m)の一覧(二重付与防止用)
    pub checkpoints_reached: Vec<usize>,
    /// 生存中か(false=酸素切れ or 押し潰されでミス)
    pub alive: bool,
    /// クリア(深度1000m到達)したか
    pub cleared: bool,
}

impl Player {
    /// 初期状態のプレイヤーを生成する。開始列はフィールド中央。
    pub fn new() -> Self {
        Player {
            row: 0,
            col: FIELD_WIDTH / 2,
            oxygen: OXYGEN_MAX,
            diamonds_collected: 0,
            diamond_score: 0,
            time_bonus_total: 0,
            elapsed_seconds: 0.0,
            checkpoints_reached: Vec::new(),
            alive: true,
            cleared: false,
        }
    }

    pub fn position(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    /// プレイヤーが到達している深度(m)。
    ///
    /// 行インデックスは0始まり(row=0が最初の1マス)だが、プレイヤーが到達した
    /// 「深さ」は掘り進んだマス数そのものであるため depth_m = row + 1 とする。
    /// これによりフィールド最終行(row = FIELD_DEPTH_M - 1)到達時に
    /// ちょうど深度1000m(spec.mdのゴール条件・チェックポイント境界)と一致する。
    pub fn depth_m(&self) -> usize {
        self.row + 1
    }

    /// 現在到達している最大深度(m)。スコア計算に用いる(spec.md 7章)。
    /// プレイヤーは掘り進むだけ(後退しない)なので、現在深度がそのまま最大深度になる。
    pub fn max_depth_m(&self) -> usize {
        self.depth_m()
    }

    /// 酸素を回復する(上限OXYGEN_MAXでクランプ)。
    pub fn add_oxygen(&mut self, amount: f32) {
        self.oxygen = (self.oxygen + amount).min(OXYGEN_MAX);
    }

    /// 酸素を自然減少させる(delta_seconds経過ぶん)。0未満にはならない。
    pub fn decay_oxygen(&mut self, decay_per_sec: f32, delta_seconds: f32) {
        self.oxygen -= decay_per_sec * delta_seconds;
        if self.oxygen < 0.0 {
            self.oxygen = 0.0;
        }
    }

    /// 酸素が尽きているか。
    pub fn is_out_of_oxygen(&self) -> bool {
        self.oxygen <= 0.0
    }

    /// 表示用に整数値へ切り捨てた酸素残量。
    pub fn oxygen_display(&self) -> u32 {
        self.oxygen.floor().max(0.0) as u32
    }

    /// スコア合計(spec.md 7章)。
    /// スコア = 最大到達深度(m) × 10 + 取得ダイヤ数 × 500 + Σ(各チェックポイントのタイムボーナス)
    pub fn total_score(&self, depth_score_multiplier: u64) -> u64 {
        self.max_depth_m() as u64 * depth_score_multiplier + self.diamond_score + self.time_bonus_total
    }
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- 正常系: 酸素回復 ---

    #[test]
    fn add_oxygen_clamps_at_max() {
        let mut player = Player::new();
        player.oxygen = 90.0;

        player.add_oxygen(50.0);

        assert_eq!(player.oxygen, OXYGEN_MAX);
    }

    #[test]
    fn add_oxygen_below_max_adds_normally() {
        let mut player = Player::new();
        player.oxygen = 30.0;

        player.add_oxygen(20.0);

        assert_eq!(player.oxygen, 50.0);
    }

    // --- 異常系: 酸素消費が0を下回らない ---

    #[test]
    fn decay_oxygen_does_not_go_below_zero() {
        let mut player = Player::new();
        player.oxygen = 5.0;

        player.decay_oxygen(2.0, 10.0); // 20.0ぶん減衰させても0未満にはならない

        assert_eq!(player.oxygen, 0.0);
        assert!(player.is_out_of_oxygen());
    }

    #[test]
    fn is_out_of_oxygen_is_false_while_oxygen_remains() {
        let mut player = Player::new();
        player.oxygen = 0.1;

        assert!(!player.is_out_of_oxygen());
    }

    // --- 深度計算 ---

    #[test]
    fn depth_m_is_row_plus_one() {
        let mut player = Player::new();
        player.row = 0;
        assert_eq!(player.depth_m(), 1);

        player.row = 999;
        assert_eq!(player.depth_m(), 1000);
    }

    // --- スコア計算 ---

    #[test]
    fn total_score_combines_depth_diamonds_and_time_bonus() {
        let mut player = Player::new();
        player.row = 99; // depth_m = 100
        player.diamond_score = 1500; // 例: ダイヤ3個 * 500
        player.time_bonus_total = 2000;

        let score = player.total_score(10);

        assert_eq!(score, 100 * 10 + 1500 + 2000);
    }

    #[test]
    fn total_score_with_no_diamonds_or_bonus_is_depth_score_only() {
        let mut player = Player::new();
        player.row = 49; // depth_m = 50

        let score = player.total_score(10);

        assert_eq!(score, 500);
    }
}

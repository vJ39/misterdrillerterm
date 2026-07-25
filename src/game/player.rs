//! プレイヤー状態(位置・向き・酸素・ライフ・スコア・経過タイム)。spec.md 1章・6〜8章。

use crate::constants::{
    AIR_CAPSULE_SCORE_STEP, DIAMOND_SCORE, FIELD_WIDTH, LEVEL_STEP_M, LIVES_DEFAULT, LIVES_MAX, LIVES_MIN, OXYGEN_MAX,
    ROCK_BREAK_OXYGEN_PENALTY, SCORE_PER_AUTO_VANISH_BLOCK, SCORE_PER_DRILLED_BLOCK,
};

/// プレイヤーが向いている方向、兼 移動入力の方向(spec.md 1章)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

impl Direction {
    /// この方向へ1マス進んだ場合の(行, 列)差分。
    pub fn delta(self) -> (isize, isize) {
        match self {
            Direction::Up => (-1, 0),
            Direction::Down => (1, 0),
            Direction::Left => (0, -1),
            Direction::Right => (0, 1),
        }
    }
}

/// プレイヤーの現在状態。
#[derive(Debug, Clone)]
pub struct Player {
    /// 現在いる行(=盤面の行インデックス)
    pub row: usize,
    /// 現在いる列
    pub col: usize,
    /// 現在向いている方向(spec.md 1章、初期値Down)
    pub facing: Direction,
    /// 直前のLeft/Right入力で「隣接マスが塞がっていてぶつかり、その場に停止した」方向
    /// (spec.md 1章、2ステップ地形追従のTERM独自拡張)。次のLeft/Right入力がこれと
    /// 同じ方向であれば、その時点で1段上が空いていれば登る。異なる方向の入力・移動成立・
    /// 掘削(Drill)・上下の向き変更(FaceUp/FaceDown)でリセットされる
    pub bumped_direction: Option<Direction>,
    /// 酸素残量(内部値。f32で保持し表示は整数値に切り捨てる。spec.md 6章)
    pub oxygen: f32,
    /// 残ライフ数(spec.md 8章。1〜5機、既定3機)
    pub lives: u8,
    /// スコア合計(spec.md 7章)
    pub score: u64,
    /// 取得した酸素カプセルの累計個数(n個目取得でn×100点の算出に使う)
    pub oxygen_capsules_collected: u32,
    /// 取得したダイヤ数
    pub diamonds_collected: u32,
    /// 経過タイム(秒)
    pub elapsed_seconds: f32,
}

impl Player {
    /// 初期状態のプレイヤーを生成する(既定ライフ数)。開始列はフィールド中央。
    pub fn new() -> Self {
        Self::with_lives(LIVES_DEFAULT)
    }

    /// ライフ数を指定して初期状態のプレイヤーを生成する(spec.md 8章「1〜5機から選べる」)。
    /// 範囲外の値は`LIVES_MIN`〜`LIVES_MAX`にクランプする(不正な設定値からの防御)。
    pub fn with_lives(lives: u8) -> Self {
        let lives = lives.clamp(LIVES_MIN, LIVES_MAX);
        Player {
            row: 0,
            col: FIELD_WIDTH / 2,
            facing: Direction::Down,
            bumped_direction: None,
            oxygen: OXYGEN_MAX,
            lives,
            score: 0,
            oxygen_capsules_collected: 0,
            diamonds_collected: 0,
            elapsed_seconds: 0.0,
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
    /// ちょうど深度1000m(spec.mdのゴール条件)と一致する。
    pub fn depth_m(&self) -> usize {
        self.row + 1
    }

    /// 現在のレベル番号(spec.md 7.1、`LEVEL_STEP_M`=30ごとに1レベル)。
    pub fn level(&self) -> usize {
        (self.depth_m() - 1) / LEVEL_STEP_M + 1
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

    /// 岩ブロック破壊時の酸素ペナルティを適用する(spec.md 2章・6章「20%消費」)。
    /// 上限100とは逆に、0未満にはならない。
    pub fn apply_rock_break_penalty(&mut self) {
        self.oxygen = (self.oxygen - ROCK_BREAK_OXYGEN_PENALTY).max(0.0);
    }

    /// 酸素が尽きているか。
    pub fn is_out_of_oxygen(&self) -> bool {
        self.oxygen <= 0.0
    }

    /// 表示用に整数値へ切り捨てた酸素残量。
    pub fn oxygen_display(&self) -> u32 {
        self.oxygen.floor().max(0.0) as u32
    }

    /// 直接掘削による消滅(spec.md 4.6・7章)のスコアを加算する。1ブロックにつき10点。
    pub fn award_drill_score(&mut self, blocks: usize) {
        self.score += blocks as u64 * SCORE_PER_DRILLED_BLOCK;
    }

    /// 自動消滅(spec.md 4.5・7章、4個以上の落下連結)のスコアを加算する。1ブロックにつき30点。
    pub fn award_auto_vanish_score(&mut self, blocks: usize) {
        self.score += blocks as u64 * SCORE_PER_AUTO_VANISH_BLOCK;
    }

    /// 酸素カプセルを取得する(spec.md 2・6・7章)。酸素+50(上限クランプ)、
    /// n個目の取得でn×100点を加算する。
    pub fn collect_oxygen_capsule(&mut self) {
        self.oxygen_capsules_collected += 1;
        self.score += self.oxygen_capsules_collected as u64 * AIR_CAPSULE_SCORE_STEP;
        self.add_oxygen(crate::constants::OXYGEN_CAPSULE_RESTORE);
    }

    /// ダイヤブロックを取得する(TERM独自拡張)。即時+500点。
    pub fn collect_diamond(&mut self) {
        self.diamonds_collected += 1;
        self.score += DIAMOND_SCORE;
    }

    /// ライフを1つ失う(spec.md 8章)。
    ///
    /// 戻り値: `true`ならライフを使い切った(ゲームオーバー)。ライフが残っていれば、
    /// この場(位置は変更しない)で酸素を全回復して再開する。
    pub fn lose_life(&mut self) -> bool {
        if self.lives == 0 {
            return true; // 呼び出し側の不整合防止用の防御的分岐(通常発生しない)
        }
        self.lives -= 1;
        if self.lives == 0 {
            true
        } else {
            self.oxygen = OXYGEN_MAX;
            false
        }
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
    use crate::constants::{LIVES_DEFAULT, OXYGEN_CAPSULE_RESTORE};

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

    #[test]
    fn rock_break_penalty_does_not_go_below_zero() {
        let mut player = Player::new();
        player.oxygen = 5.0;

        player.apply_rock_break_penalty(); // -20.0

        assert_eq!(player.oxygen, 0.0);
    }

    // --- 深度・レベル計算 ---

    #[test]
    fn depth_m_is_row_plus_one() {
        let mut player = Player::new();
        player.row = 0;
        assert_eq!(player.depth_m(), 1);

        player.row = 999;
        assert_eq!(player.depth_m(), 1000);
    }

    #[test]
    fn level_matches_30m_segments() {
        let mut player = Player::new();
        player.row = 0; // depth 1
        assert_eq!(player.level(), 1);

        player.row = 29; // depth 30
        assert_eq!(player.level(), 1);

        player.row = 30; // depth 31
        assert_eq!(player.level(), 2);

        player.row = 999; // depth 1000
        assert_eq!(player.level(), 34);
    }

    // --- スコア加算 ---

    #[test]
    fn award_drill_score_is_ten_per_block() {
        let mut player = Player::new();
        player.award_drill_score(3);
        assert_eq!(player.score, 30);
    }

    #[test]
    fn award_auto_vanish_score_is_thirty_per_block() {
        let mut player = Player::new();
        player.award_auto_vanish_score(4);
        assert_eq!(player.score, 120);
    }

    #[test]
    fn oxygen_capsule_score_is_n_times_step_and_restores_oxygen() {
        let mut player = Player::new();
        player.oxygen = 10.0;

        player.collect_oxygen_capsule(); // 1個目: 100点
        assert_eq!(player.score, 100);
        assert_eq!(player.oxygen, 10.0 + OXYGEN_CAPSULE_RESTORE);

        player.collect_oxygen_capsule(); // 2個目: 200点(累積300)
        assert_eq!(player.score, 300);
    }

    #[test]
    fn collect_diamond_awards_flat_500() {
        let mut player = Player::new();
        player.collect_diamond();
        player.collect_diamond();
        assert_eq!(player.score, 1000);
        assert_eq!(player.diamonds_collected, 2);
    }

    // --- ライフ ---

    #[test]
    fn lose_life_restores_oxygen_when_lives_remain() {
        let mut player = Player::with_lives(3);
        player.oxygen = 0.0;

        let game_over = player.lose_life();

        assert!(!game_over);
        assert_eq!(player.lives, 2);
        assert_eq!(player.oxygen, OXYGEN_MAX);
    }

    #[test]
    fn lose_life_on_last_life_ends_game_without_restoring_oxygen() {
        let mut player = Player::with_lives(1);
        player.oxygen = 0.0;

        let game_over = player.lose_life();

        assert!(game_over);
        assert_eq!(player.lives, 0);
        assert_eq!(player.oxygen, 0.0); // ゲームオーバーなので回復しない
    }

    #[test]
    fn default_lives_matches_constant() {
        let player = Player::new();
        assert_eq!(player.lives, LIVES_DEFAULT);
    }

    #[test]
    fn with_lives_clamps_out_of_range_values() {
        assert_eq!(Player::with_lives(0).lives, crate::constants::LIVES_MIN);
        assert_eq!(Player::with_lives(255).lives, crate::constants::LIVES_MAX);
    }
}

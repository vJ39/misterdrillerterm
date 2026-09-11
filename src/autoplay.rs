//! 「死なないオートプレイ」の判断ロジック(TERM独自拡張。#218)。
//!
//! 長時間プレイでしか出ない稀なバグを再現・検出するソークテスト用のデバッグ機能。
//! `Game`の外に置いた「仮想キーボード」として振る舞い、盤面を見て次に押すべきキー
//! (`InputAction`)を返すだけで、ゲームの内部状態は一切直接触らない。返した入力は
//! 人間の操作と全く同じ`Game::apply_input`を通るため、AIだけが使える裏口は無い。
//!
//! 判断は毎フレーム盤面から再計算する貪欲方式で、経路は保持しない(落下で盤面が
//! 変わるため、立てた計画を追うより都度評価する方が安全)。**乱数は一切使わない**
//! ため、同じ盤面・同じ内部状態からは常に同じ入力列が出る(再現性の担保)。
//!
//! 「死なない」は2つの独立した仕組みの併用で成り立っている:
//! - 方式A(無敵): `Game::set_invincible`。ミス処理を回避イベントへ置き換える土台
//! - 方式B(安全志向AI): このモジュール。そもそもミスを踏まないよう立ち回る
//!
//! 無敵とAIは独立したトグルで、AIだけを動かして「どれだけ自力で生き延びるか」を
//! 見ることも、手動操作のまま無敵にすることもできる。

use crate::constants::{
    AUTOPLAY_BOMB_EVADE_MS, AUTOPLAY_LOOKAHEAD_ROWS, AUTOPLAY_OXYGEN_ROCK_BUDGET,
    AUTOPLAY_OXYGEN_SEEK_THRESHOLD, AUTOPLAY_REVIVE_DELAY_MS, AUTOPLAY_STUCK_FRAMES,
    BOMB_BLAST_ROW_RANGE, FRAME_INTERVAL_MS,
};
use crate::game::board::Cell;
use crate::game::player::Direction;
use crate::game::{BombPhase, Game, GameStatus, InputAction};

/// 頭上の危険を確認する行数(自分の列の直上から数えて何行ぶんを見るか)。
/// 落下ブロックは1マスずつ落ちてくるため、遠すぎる行まで見ると回避が早すぎて
/// 前に進めなくなり、1行だけだと揺れ終わってからでは間に合わない。間を取って
/// 「揺れに気づいてから1マス横へ逃げるのが間に合う」範囲に設定している。
const OVERHEAD_SCAN_ROWS: usize = 3;

/// オートプレイの状態。`Game`とは独立して持ち、Tキーで生成・破棄する。
pub struct Autopilot {
    /// Tキーでオートプレイを開始する直前の無敵状態。解除時にここへ戻すことで、
    /// 「Gキーで自分で無敵にしていた」場合にその設定を壊さないようにする。
    restore_invincible: bool,
    /// 横移動の優先方向。一度横へ動き出したらその向きを保ち、左右に往復して
    /// 進まなくなる(振動する)のを防ぐ。
    side_preference: Direction,
    /// 前フレームのプレイヤー位置(進捗判定用)。
    last_pos: (usize, usize),
    /// 位置が変わらないまま経過したフレーム数。
    frames_without_progress: u32,
    /// 手詰まりの度合い。0=通常、1=逆側を試す、2=岩破壊・酸素探索を無視、
    /// 3=段差登りを試みる。位置が動いた時点で0へ戻る。
    escalation: u8,
    /// GameOverになってから経過したフレーム数(自動Reviveまでの待ち時間の計測用)。
    game_over_frames: u32,
}

/// そのフレームでオートプレイが何をしようとしたか(TERM独自拡張。#218)。
/// 挙動のテスト・デバッグ表示用で、ゲーム進行には影響しない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// 何もしない(自由落下待ち・演出中・プレイ中でない)
    Idle,
    /// 隣接したボムを掘って取り除く
    DefuseBomb,
    /// 起爆間近のボムの爆風範囲から逃げる
    EvadeBomb,
    /// 頭上の落ちてきそうなブロックから逃げる
    DodgeOverhead,
    /// 酸素が減ったのでAIRを取りに行く
    SeekOxygen,
    /// 直下のブロックを掘って進む(既定の行動)
    DigDown,
    /// 迂回できない岩ブロックを掘る
    BreakRock,
    /// 岩・障害を避けて横へ回り込む
    Sidestep,
    /// 手詰まりからの脱出として段差を登る
    EscapeClimb,
    /// GameOverから自動で復活する
    Revive,
}

impl Autopilot {
    /// オートプレイを開始する。`restore_invincible`には、開始直前の無敵状態
    /// (解除時に戻す値)を渡す。
    pub fn new(restore_invincible: bool) -> Self {
        Autopilot {
            restore_invincible,
            side_preference: Direction::Right,
            // 実在しない番兵ではなく(0,0)で始める。初回の`decide`で実際の位置と
            // 比較され、ほぼ必ず「進捗あり」と判定されてカウンタが初期化される。
            last_pos: (0, 0),
            frames_without_progress: 0,
            escalation: 0,
            game_over_frames: 0,
        }
    }

    /// オートプレイ開始直前の無敵状態(解除時に戻す値)。
    pub fn restore_invincible(&self) -> bool {
        self.restore_invincible
    }

    /// 1フレームぶんの判断。押すべきキーだけを返す。
    pub fn decide(&mut self, game: &Game) -> Vec<InputAction> {
        self.decide_with_intent(game).1
    }

    /// `decide`に、そのフレームの判断理由(`Intent`)を添えた版。0〜2個の
    /// `InputAction`を返す(向き変更1個+掘削1個の組み合わせもある)。
    pub fn decide_with_intent(&mut self, game: &Game) -> (Intent, Vec<InputAction>) {
        match game.status {
            GameStatus::GameOver => {
                // 無敵OFFのまま死んだ場合の保険。すぐ復活させると死因の瞬間が
                // 見えないため、少し待ってからEnter(=Reviveの選択確定)を押す。
                // GameOverダイアログの初期選択は「タイトルへ戻る」なので、
                // main.rs側でConfirmをRevive呼び出しへ読み替える。
                self.game_over_frames = self.game_over_frames.saturating_add(1);
                if u64::from(self.game_over_frames) * FRAME_INTERVAL_MS >= AUTOPLAY_REVIVE_DELAY_MS
                {
                    self.game_over_frames = 0;
                    return (Intent::Revive, vec![InputAction::Confirm]);
                }
                return (Intent::Idle, Vec::new());
            }
            GameStatus::Playing => {}
            // 一時停止中・クリア後は操作しない(クリア到達後はその場で待機する)。
            GameStatus::Paused | GameStatus::Cleared => return (Intent::Idle, Vec::new()),
        }
        self.game_over_frames = 0;

        self.note_progress(game.player.position());

        if let Some(actions) = self.defuse_adjacent_bomb(game) {
            return (Intent::DefuseBomb, actions);
        }
        if let Some(actions) = self.evade_imminent_bomb(game) {
            return (Intent::EvadeBomb, actions);
        }
        if let Some(actions) = self.dodge_overhead(game) {
            return (Intent::DodgeOverhead, actions);
        }
        if let Some(actions) = self.seek_oxygen(game) {
            return (Intent::SeekOxygen, actions);
        }
        self.descend(game)
    }

    /// 進捗(位置の変化)を記録し、手詰まりが続けば`escalation`を1段上げる。
    fn note_progress(&mut self, pos: (usize, usize)) {
        if pos != self.last_pos {
            self.last_pos = pos;
            self.frames_without_progress = 0;
            self.escalation = 0;
            return;
        }
        self.frames_without_progress = self.frames_without_progress.saturating_add(1);
        if self.frames_without_progress > AUTOPLAY_STUCK_FRAMES {
            self.escalation = (self.escalation + 1).min(3);
            self.frames_without_progress = 0;
        }
    }

    // --- 1. 隣接ボムの除去 ---------------------------------------------------

    /// 隣接マスに静止中(Settling/Ticking)のボムがあれば掘って取り除く。まだ登場・
    /// 投擲演出中(Entering/Rolling)のボムは盤面上の実体が無いため対象外。
    fn defuse_adjacent_bomb(&mut self, game: &Game) -> Option<Vec<InputAction>> {
        let (row, col) = game.player.position();

        // 真下・真上は向き変更(FaceDown/FaceUp)で狙いを付けられるため、同じフレームに
        // 向き変更と掘削をまとめて出せる。
        for (dir, target) in [
            (Direction::Down, (row + 1, col)),
            (Direction::Up, (row.wrapping_sub(1), col)),
        ] {
            if dir == Direction::Up && row == 0 {
                continue;
            }
            if settled_bomb_at(game, target) {
                return Some(self.drill_vertically(game, dir));
            }
        }

        // 左右は向き変更専用の入力が無いため、MoveLeft/MoveRightで一度ぶつかって
        // facingを合わせ(押し出せるなら押し出して解決)、次フレームで掘る。
        for dir in [Direction::Left, Direction::Right] {
            let Some(target) = neighbor(game, (row, col), dir) else {
                continue;
            };
            if settled_bomb_at(game, target) {
                return Some(if game.player.facing == dir {
                    vec![InputAction::Drill]
                } else {
                    vec![move_action(dir)]
                });
            }
        }

        None
    }

    // --- 2. 起爆間近ボムの回避 -----------------------------------------------

    /// 起爆が近いボムの爆風範囲にいるなら逃げる。爆風は同じ行なら盤面幅の端まで、
    /// 同じ列なら`BOMB_BLAST_ROW_RANGE`行ぶん届くため、脅威の向きによって
    /// 「行を変える(掘って落ちる)」「列を変える(横へ逃げる)」を選び分ける。
    ///
    /// 設計メモでは常に横移動を先に試す想定だったが、同じ行にいる場合は横へ動いても
    /// 同じ行のまま(爆風は幅全体に届く)で逃げたことにならないため、脅威の向きで
    /// 優先順を変えている。
    fn evade_imminent_bomb(&mut self, game: &Game) -> Option<Vec<InputAction>> {
        let (row, col) = game.player.position();

        let mut same_row_threat = false;
        let mut same_col_threat = false;
        for bomb in game.bombs() {
            if bomb.phase != BombPhase::Ticking || bomb.remaining_ms > AUTOPLAY_BOMB_EVADE_MS {
                continue;
            }
            if bomb.pos.0 == row {
                same_row_threat = true;
            }
            if bomb.pos.1 == col && bomb.pos.0.abs_diff(row) <= BOMB_BLAST_ROW_RANGE {
                same_col_threat = true;
            }
        }
        if !same_row_threat && !same_col_threat {
            return None;
        }

        let escape_by_row = self.dig_down_to_change_row(game);
        let escape_by_col = self
            .safe_sidestep_direction(game)
            .map(|dir| vec![move_action(dir)]);

        if same_row_threat {
            escape_by_row.or(escape_by_col)
        } else {
            escape_by_col.or(escape_by_row)
        }
    }

    // --- 3. 頭上回避 ---------------------------------------------------------

    /// 自分の列の直上数行に落ちてきそうなブロックがあれば、横へ逃げるか、掘って
    /// 1行下がる。
    fn dodge_overhead(&mut self, game: &Game) -> Option<Vec<InputAction>> {
        let (row, col) = game.player.position();
        let threatened = (1..=OVERHEAD_SCAN_ROWS)
            .filter_map(|d| row.checked_sub(d))
            .any(|r| game.is_cell_unstable(r, col));
        if !threatened {
            return None;
        }

        if let Some(dir) = self.safe_sidestep_direction(game) {
            return Some(vec![move_action(dir)]);
        }
        self.dig_down_to_change_row(game)
    }

    // --- 4. 酸素探索 ---------------------------------------------------------

    /// 酸素が減っていれば、近傍のAIRへ向けて列を寄せる。深い行にあるAIRは下降
    /// (5.)に任せるが、列がずれていると通り過ぎてしまうため、列だけは先に合わせる。
    fn seek_oxygen(&mut self, game: &Game) -> Option<Vec<InputAction>> {
        if game.player.oxygen >= AUTOPLAY_OXYGEN_SEEK_THRESHOLD || self.escalation >= 2 {
            return None;
        }

        let target_col = self.nearest_oxygen_column(game)?;
        let (_, col) = game.player.position();
        let dir = match target_col.cmp(&col) {
            std::cmp::Ordering::Less => Direction::Left,
            std::cmp::Ordering::Greater => Direction::Right,
            // 列が既に合っているなら、あとは下降に任せれば自然に到達する。
            std::cmp::Ordering::Equal => return None,
        };
        if !self.can_step(game, dir) {
            return None;
        }
        self.side_preference = dir;
        Some(vec![move_action(dir)])
    }

    /// プレイヤーと同じ行から`AUTOPLAY_LOOKAHEAD_ROWS`行ぶん下までを走査し、最も
    /// 近い(行が浅く、同率なら列が近い)AIRの列を返す。走査順は固定なので結果は
    /// 常に一意に定まる。
    fn nearest_oxygen_column(&self, game: &Game) -> Option<usize> {
        let (row, col) = game.player.position();
        let last_row = (row + AUTOPLAY_LOOKAHEAD_ROWS).min(game.board.depth_rows() - 1);
        let mut best: Option<(usize, usize)> = None; // (行距離, 列距離)
        let mut best_col = None;

        for r in row..=last_row {
            for c in 0..game.board.width() {
                if game.board.cell(r, c) != Cell::Oxygen {
                    continue;
                }
                let key = (r - row, c.abs_diff(col));
                if best.is_none_or(|current| key < current) {
                    best = Some(key);
                    best_col = Some(c);
                }
            }
        }
        best_col
    }

    // --- 5. 下降(既定の行動) ------------------------------------------------

    /// 既定の行動。直下の状況に応じて、自由落下を待つ・掘る・岩を迂回する・
    /// 手詰まりなら段差を登る、を選ぶ。
    fn descend(&mut self, game: &Game) -> (Intent, Vec<InputAction>) {
        let (row, col) = game.player.position();
        let Some(below) = game.board.cell_or_none(row + 1, col) else {
            // 最深行。ゴール判定(Cleared)が入るまで何もしない。
            return (Intent::Idle, Vec::new());
        };

        match below {
            // 直下が空いていれば自由落下に任せる。掘る用意だけ整えておく。
            Cell::Empty | Cell::Oxygen | Cell::Item(_) => {
                if game.player.facing == Direction::Down {
                    (Intent::Idle, Vec::new())
                } else {
                    (Intent::Idle, vec![InputAction::FaceDown])
                }
            }
            Cell::Color(_) | Cell::Diamond | Cell::Star { .. } => (
                Intent::DigDown,
                self.drill_vertically(game, Direction::Down),
            ),
            Cell::Rock { .. } => self.handle_rock_below(game),
        }
    }

    /// 直下が岩ブロックの場合の判断。岩は5回掘らないと壊れず酸素も減るため、
    /// 酸素が心許ないうちは隣の列へ回り込む方を優先する。
    fn handle_rock_below(&mut self, game: &Game) -> (Intent, Vec<InputAction>) {
        // 手詰まりが極まったら、掘るのをやめて段差を登って別の場所を試す。
        if self.escalation >= 3
            && let Some(actions) = self.escape_climb(game)
        {
            return (Intent::EscapeClimb, actions);
        }

        let low_on_oxygen = game.player.oxygen < AUTOPLAY_OXYGEN_ROCK_BUDGET;
        if low_on_oxygen
            && self.escalation < 2
            && let Some(dir) = self.rock_sidestep_direction(game)
        {
            self.side_preference = dir;
            return (Intent::Sidestep, vec![move_action(dir)]);
        }

        (
            Intent::BreakRock,
            self.drill_vertically(game, Direction::Down),
        )
    }

    /// 手詰まり脱出用の段差登り。頭上が空いている場合のみ、優先方向へ`MoveX`を
    /// 出し続ける。`move_lateral`の段差登りは「同じ方向へ2回ぶつかる」ことで
    /// 成立するため、同方向の入力を連投してよいのはこの経路だけ(通常の
    /// `step_toward`は誤って段差を登らないよう、ぶつかった次は掘りに切り替える)。
    ///
    /// ここでは`side_preference`を書き換えない。`primary_side`はescalationが1以上の
    /// 間`side_preference`の逆を返すため、ここで結果を代入し直すと毎フレーム左右が
    /// 入れ替わってしまい、段差登りの成立条件(同方向に2回ぶつかる)を永久に
    /// 満たせなくなる。書き換えないことで、位置が動く(=escalationが0へ戻る)まで
    /// 同じ方向を出し続ける。
    fn escape_climb(&self, game: &Game) -> Option<Vec<InputAction>> {
        let (row, col) = game.player.position();
        if row == 0 || game.board.cell(row - 1, col) != Cell::Empty {
            return None;
        }
        let dir = self.primary_side();
        neighbor(game, (row, col), dir)?;
        Some(vec![move_action(dir)])
    }

    // --- 共通ヘルパー --------------------------------------------------------

    /// 上下方向の掘削。向きが合っていなければ、同じフレームで向き変更と掘削の
    /// 2つを出す(`face_up`/`face_down`は即座に反映されるため1フレームで足りる)。
    fn drill_vertically(&self, game: &Game, dir: Direction) -> Vec<InputAction> {
        let face = match dir {
            Direction::Up => InputAction::FaceUp,
            _ => InputAction::FaceDown,
        };
        if game.player.facing == dir {
            vec![InputAction::Drill]
        } else {
            vec![face, InputAction::Drill]
        }
    }

    /// 直下を掘って行を変える(爆風・落下ブロックからの緊急避難用)。既に直下が
    /// 空いていれば落下を待つだけでよいので`None`を返し、呼び出し側の次の手段へ譲る。
    fn dig_down_to_change_row(&self, game: &Game) -> Option<Vec<InputAction>> {
        let (row, col) = game.player.position();
        match game.board.cell_or_none(row + 1, col)? {
            Cell::Empty | Cell::Oxygen | Cell::Item(_) => None,
            _ => Some(self.drill_vertically(game, Direction::Down)),
        }
    }

    /// `escalation`を踏まえた、今フレームに最初に試すべき横方向。手詰まりが1段
    /// 進んだら逆側から試す。
    fn primary_side(&self) -> Direction {
        if self.escalation >= 1 {
            opposite(self.side_preference)
        } else {
            self.side_preference
        }
    }

    /// 安全に横へ1マス逃げられる方向(優先方向から順に試す)。見つからなければ`None`。
    fn safe_sidestep_direction(&mut self, game: &Game) -> Option<Direction> {
        let primary = self.primary_side();
        let dir = [primary, opposite(primary)]
            .into_iter()
            .find(|&dir| self.can_step(game, dir))?;
        self.side_preference = dir;
        Some(dir)
    }

    /// 岩を迂回できる方向。横へ抜けられ、かつその先が岩で塞がっていない
    /// (回り込んだ意味がある)方向を優先方向から順に探す。
    fn rock_sidestep_direction(&self, game: &Game) -> Option<Direction> {
        let primary = self.primary_side();
        [primary, opposite(primary)].into_iter().find(|&dir| {
            if !self.can_step(game, dir) {
                return false;
            }
            let Some(target) = neighbor(game, game.player.position(), dir) else {
                return false;
            };
            // 回り込んだ先の直下がまた岩なら、迂回できていないので数えない。
            !matches!(
                game.board.cell_or_none(target.0 + 1, target.1),
                Some(Cell::Rock { .. })
            )
        })
    }

    /// その方向へ「掘らずに」1マス動けるか。移動先が通り抜けられるマスで、その
    /// 頭上に落ちてきそうなブロックが無く、今フレームに横移動自体が通る
    /// (接地している)ことを確認する。
    fn can_step(&self, game: &Game, dir: Direction) -> bool {
        if !game.player_is_grounded() {
            // 落下中は`try_lateral_move`が横移動を受け付けない。
            return false;
        }
        let Some(target) = neighbor(game, game.player.position(), dir) else {
            return false;
        };
        if !matches!(
            game.board.cell(target.0, target.1),
            Cell::Empty | Cell::Oxygen | Cell::Item(_)
        ) {
            return false;
        }
        if settled_bomb_at(game, target) {
            return false;
        }
        // 移動先の頭上が不安定なら、飛び込んだ先で潰されるので待つ。
        // 手詰まりが続けば`escalation`が上がり、別の手段へ移る。
        !target
            .0
            .checked_sub(1)
            .is_some_and(|above| game.is_cell_unstable(above, target.1))
    }
}

/// `pos`から`dir`へ1マス進んだ座標。盤面外なら`None`。
fn neighbor(game: &Game, pos: (usize, usize), dir: Direction) -> Option<(usize, usize)> {
    let (dr, dc) = dir.delta();
    let row = pos.0.checked_add_signed(dr)?;
    let col = pos.1.checked_add_signed(dc)?;
    (row < game.board.depth_rows() && col < game.board.width()).then_some((row, col))
}

/// 指定マスに静止中(Settling/Ticking)のボムがあるか。登場・投擲演出中のボムは
/// まだ盤面上の障害物として振る舞わないため対象外。
fn settled_bomb_at(game: &Game, pos: (usize, usize)) -> bool {
    game.bombs()
        .iter()
        .any(|b| b.pos == pos && matches!(b.phase, BombPhase::Settling | BombPhase::Ticking))
}

/// 左右の反転。
fn opposite(dir: Direction) -> Direction {
    match dir {
        Direction::Left => Direction::Right,
        _ => Direction::Left,
    }
}

/// 横方向に対応する移動入力。
fn move_action(dir: Direction) -> InputAction {
    match dir {
        Direction::Left => InputAction::MoveLeft,
        _ => InputAction::MoveRight,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{FIELD_DEPTH_M, FIELD_WIDTH_DEFAULT, LIVES_DEFAULT};
    use crate::game::Bomb;
    use crate::game::board::ColorKind;

    /// テスト用ヘルパー: 盤面全体を`Cell::Empty`にクリアし、プレイヤーを指定位置へ置く。
    /// `Game::new_for_test`はランダム生成された盤面を持つため、テストが意図していない
    /// 場所の未支持ブロックが判断へ紛れ込まないよう必ずクリアしてから配置する。
    /// 手詰まり判定で`escalation`が1段上がるまでに必要な`decide`の呼び出し回数。
    /// 初回は「前フレームと位置が違う」扱いでカウンタが初期化されるため、その1回と、
    /// カウンタが`AUTOPLAY_STUCK_FRAMES`を「超える」のに必要な1回を足す。
    fn stuck_frames_to_escalate() -> u32 {
        AUTOPLAY_STUCK_FRAMES + 2
    }

    fn game_at(seed: u64, row: usize, col: usize) -> Game {
        let mut game = Game::new(seed);
        for r in game.board.rows.iter_mut() {
            for cell in r.iter_mut() {
                *cell = Cell::Empty;
            }
        }
        game.player.row = row;
        game.player.col = col;
        game.player.facing = Direction::Down;
        game
    }

    #[test]
    fn decide_does_nothing_while_paused_or_cleared() {
        let mut game = game_at(1, 500, 5);
        let mut pilot = Autopilot::new(false);

        game.status = GameStatus::Paused;
        assert_eq!(pilot.decide(&game), Vec::new());

        game.status = GameStatus::Cleared;
        assert_eq!(pilot.decide(&game), Vec::new());
    }

    #[test]
    fn decide_waits_then_presses_confirm_to_revive_after_game_over() {
        // 無敵OFFのまま死んだ場合の保険。すぐには復活させず、
        // AUTOPLAY_REVIVE_DELAY_MSぶん待ってからEnter(Confirm)を出す。
        let mut game = game_at(2, 500, 5);
        game.status = GameStatus::GameOver;
        let mut pilot = Autopilot::new(false);

        let frames_to_wait = AUTOPLAY_REVIVE_DELAY_MS / FRAME_INTERVAL_MS;
        for frame in 0..frames_to_wait {
            assert_eq!(
                pilot.decide(&game),
                Vec::new(),
                "frame={frame}: 待機中は何も押さないはず"
            );
        }
        assert_eq!(pilot.decide(&game), vec![InputAction::Confirm]);
    }

    #[test]
    fn decide_returns_no_input_when_the_cell_below_is_empty_so_the_player_just_falls() {
        // 直下が空いていれば自由落下に任せる(掘っても落下は速くならない仕様のため)。
        let game = game_at(3, 500, 5);
        let mut pilot = Autopilot::new(false);

        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::Idle);
        assert_eq!(actions, Vec::new(), "既にDown向きなので向き変更すら不要");
    }

    #[test]
    fn decide_faces_down_first_when_the_cell_below_is_empty_but_facing_elsewhere() {
        let mut game = game_at(3, 500, 5);
        game.player.facing = Direction::Left;
        let mut pilot = Autopilot::new(false);

        assert_eq!(pilot.decide(&game), vec![InputAction::FaceDown]);
    }

    #[test]
    fn decide_drills_down_through_a_color_block() {
        let mut game = game_at(4, 500, 5);
        game.board.rows[501][5] = Cell::Color(ColorKind::Red);
        let mut pilot = Autopilot::new(false);

        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::DigDown);
        assert_eq!(actions, vec![InputAction::Drill], "既にDown向き");
    }

    #[test]
    fn decide_turns_down_and_drills_in_the_same_frame_when_facing_elsewhere() {
        // 上下方向は向き変更専用の入力があるため、1フレームで向き変更+掘削を出せる。
        let mut game = game_at(4, 500, 5);
        game.board.rows[501][5] = Cell::Color(ColorKind::Red);
        game.player.facing = Direction::Up;
        let mut pilot = Autopilot::new(false);

        assert_eq!(
            pilot.decide(&game),
            vec![InputAction::FaceDown, InputAction::Drill]
        );
    }

    #[test]
    fn decide_walks_around_a_rock_when_oxygen_is_low_and_a_detour_exists() {
        // 岩は5回掘らないと壊れず酸素も減るため、酸素が心許ないうちは迂回を優先する。
        let mut game = game_at(5, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 };
        game.player.oxygen = AUTOPLAY_OXYGEN_ROCK_BUDGET - 1.0;
        let mut pilot = Autopilot::new(false);

        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::Sidestep);
        assert_eq!(
            actions,
            vec![InputAction::MoveRight],
            "既定の優先方向(Right)へ回り込むはず"
        );
    }

    #[test]
    fn decide_breaks_a_rock_when_both_sides_are_also_rock() {
        let mut game = game_at(6, 500, 5);
        for col in 4..=6 {
            game.board.rows[501][col] = Cell::Rock { hits: 0 };
        }
        // 両隣も岩で塞がっており、横へ抜けられない。
        game.board.rows[500][4] = Cell::Rock { hits: 0 };
        game.board.rows[500][6] = Cell::Rock { hits: 0 };
        game.player.oxygen = AUTOPLAY_OXYGEN_ROCK_BUDGET - 1.0;
        let mut pilot = Autopilot::new(false);

        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::BreakRock);
        assert_eq!(actions, vec![InputAction::Drill]);
    }

    #[test]
    fn decide_breaks_a_rock_without_detouring_when_oxygen_is_plentiful() {
        // 酸素に余裕があるなら、回り道せず掘って最短で下へ進む。
        let mut game = game_at(7, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 };
        game.player.oxygen = AUTOPLAY_OXYGEN_ROCK_BUDGET + 1.0;
        let mut pilot = Autopilot::new(false);

        assert_eq!(pilot.decide_with_intent(&game).0, Intent::BreakRock);
    }

    #[test]
    fn decide_steps_toward_a_nearby_air_capsule_when_oxygen_is_low() {
        let mut game = game_at(8, 500, 5);
        game.board.rows[501][5] = Cell::Color(ColorKind::Red); // 掘る対象(接地もさせる)
        game.board.rows[503][2] = Cell::Oxygen; // 少し下の左側にAIR
        game.player.oxygen = AUTOPLAY_OXYGEN_SEEK_THRESHOLD - 1.0;
        let mut pilot = Autopilot::new(false);

        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::SeekOxygen);
        assert_eq!(
            actions,
            vec![InputAction::MoveLeft],
            "AIRのある列(2)へ寄るため左へ動くはず"
        );
    }

    #[test]
    fn decide_ignores_air_that_is_out_of_the_lookahead_range() {
        let mut game = game_at(9, 500, 5);
        game.board.rows[501][5] = Cell::Color(ColorKind::Red);
        game.board.rows[500 + AUTOPLAY_LOOKAHEAD_ROWS + 5][2] = Cell::Oxygen; // 遠すぎるAIR
        game.player.oxygen = AUTOPLAY_OXYGEN_SEEK_THRESHOLD - 1.0;
        let mut pilot = Autopilot::new(false);

        assert_eq!(
            pilot.decide_with_intent(&game).0,
            Intent::DigDown,
            "探索範囲外のAIRには向かわず、通常の下降を続けるはず"
        );
    }

    #[test]
    fn decide_does_not_step_under_a_block_that_is_about_to_fall() {
        // 移動先の頭上が不安定(支えなし=落下対象)なら、そちらへは逃げない。
        let mut game = game_at(10, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 };
        game.board.rows[499][6] = Cell::Color(ColorKind::Blue); // 右へ動いた先の頭上、支えなし
        game.player.oxygen = AUTOPLAY_OXYGEN_ROCK_BUDGET - 1.0;
        let mut pilot = Autopilot::new(false);

        assert!(
            game.is_cell_unstable(499, 6),
            "前提: 支えのないブロックは不安定と判定されるはず"
        );
        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::Sidestep);
        assert_eq!(
            actions,
            vec![InputAction::MoveLeft],
            "右は頭上が危険なので、逆側(左)へ回り込むはず"
        );
    }

    #[test]
    fn decide_dodges_sideways_when_a_block_overhead_is_unsupported() {
        let mut game = game_at(11, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 足場
        game.board.rows[498][5] = Cell::Color(ColorKind::Blue); // 頭上、支えなし
        let mut pilot = Autopilot::new(false);

        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::DodgeOverhead);
        assert_eq!(actions, vec![InputAction::MoveRight]);
    }

    #[test]
    fn decide_drills_the_adjacent_bomb_below() {
        let mut game = game_at(12, 500, 5);
        game.board.rows[502][5] = Cell::Rock { hits: 0 };
        game.bombs_mut().push(Bomb {
            pos: (501, 5),
            origin: (501, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 4000,
            settle_bounce_dir: 1,
        });
        let mut pilot = Autopilot::new(false);

        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::DefuseBomb);
        assert_eq!(actions, vec![InputAction::Drill], "既にDown向き");
    }

    #[test]
    fn decide_bumps_into_a_side_bomb_first_then_drills_it() {
        let mut game = game_at(13, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 足場
        game.bombs_mut().push(Bomb {
            pos: (500, 6),
            origin: (500, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 4000,
            settle_bounce_dir: 1,
        });
        let mut pilot = Autopilot::new(false);

        // 1フレーム目: 向きが合っていないので、まず右へぶつかりに行く
        // (押し出せれば押し出し、押せなければfacingだけ右になる)。
        assert_eq!(pilot.decide(&game), vec![InputAction::MoveRight]);

        // facingが合った次のフレームは掘って除去する。
        game.player.facing = Direction::Right;
        assert_eq!(pilot.decide(&game), vec![InputAction::Drill]);
    }

    #[test]
    fn decide_ignores_bombs_that_are_still_entering_or_rolling() {
        // 登場・投擲演出中のボムはまだ盤面上の実体が無いため、掘る対象にしない。
        let mut game = game_at(14, 500, 5);
        game.board.rows[501][5] = Cell::Color(ColorKind::Red);
        game.bombs_mut().push(Bomb {
            pos: (500, 6),
            origin: (500, 0),
            phase: BombPhase::Rolling,
            phase_elapsed_ms: 0,
            remaining_ms: 4000,
            settle_bounce_dir: 1,
        });
        let mut pilot = Autopilot::new(false);

        assert_eq!(pilot.decide_with_intent(&game).0, Intent::DigDown);
    }

    #[test]
    fn decide_escapes_the_row_when_a_bomb_on_the_same_row_is_about_to_explode() {
        // 爆風は同じ行なら盤面幅の端まで届くため、横へ逃げても意味が無い。
        // 掘って行を変えることを優先する。
        let mut game = game_at(15, 500, 5);
        game.board.rows[501][5] = Cell::Color(ColorKind::Red);
        game.bombs_mut().push(Bomb {
            pos: (500, 1),
            origin: (500, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: AUTOPLAY_BOMB_EVADE_MS - 1,
            settle_bounce_dir: 1,
        });
        let mut pilot = Autopilot::new(false);

        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::EvadeBomb);
        assert_eq!(actions, vec![InputAction::Drill]);
    }

    #[test]
    fn decide_changes_column_when_a_bomb_in_the_same_column_is_about_to_explode() {
        let mut game = game_at(16, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 足場
        game.bombs_mut().push(Bomb {
            pos: (500 - BOMB_BLAST_ROW_RANGE, 5),
            origin: (500 - BOMB_BLAST_ROW_RANGE, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: AUTOPLAY_BOMB_EVADE_MS - 1,
            settle_bounce_dir: 1,
        });
        let mut pilot = Autopilot::new(false);

        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::EvadeBomb);
        assert_eq!(actions, vec![InputAction::MoveRight], "列を変えて逃げる");
    }

    #[test]
    fn decide_ignores_bombs_whose_fuse_is_still_long() {
        let mut game = game_at(17, 500, 5);
        game.board.rows[501][5] = Cell::Color(ColorKind::Red);
        game.bombs_mut().push(Bomb {
            pos: (500, 1),
            origin: (500, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: AUTOPLAY_BOMB_EVADE_MS + 1000,
            settle_bounce_dir: 1,
        });
        let mut pilot = Autopilot::new(false);

        assert_eq!(pilot.decide_with_intent(&game).0, Intent::DigDown);
    }

    #[test]
    fn escalation_rises_only_while_the_player_stays_in_place() {
        let mut game = game_at(18, 500, 5);
        game.board.rows[501][5] = Cell::Color(ColorKind::Red);
        let mut pilot = Autopilot::new(false);

        // 初回の`decide`は「前フレームと位置が違う」扱いでカウンタを初期化するため、
        // そのぶんを足して回す。
        for _ in 0..stuck_frames_to_escalate() {
            pilot.decide(&game);
        }
        assert_eq!(pilot.escalation, 1, "進捗が無いので1段上がるはず");

        // 位置が変われば0へ戻る。
        game.player.col = 4;
        pilot.decide(&game);
        assert_eq!(pilot.escalation, 0);
    }

    #[test]
    fn escalation_flips_the_preferred_side_so_a_stuck_pilot_tries_the_other_way() {
        let mut game = game_at(19, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 };
        game.player.oxygen = AUTOPLAY_OXYGEN_ROCK_BUDGET - 1.0;
        let mut pilot = Autopilot::new(false);

        assert_eq!(pilot.decide(&game), vec![InputAction::MoveRight]);

        // 同じ位置に留まり続けるとescalationが上がり、逆側を試すようになる。
        for _ in 0..stuck_frames_to_escalate() {
            pilot.decide(&game);
        }
        assert_eq!(pilot.decide(&game), vec![InputAction::MoveLeft]);
    }

    #[test]
    fn escalation_two_stops_the_air_detour_so_the_pilot_keeps_digging() {
        // 手詰まりが2段進んだら酸素探索より前進を優先する(AIRを取りに行けない
        // 位置で延々と横移動し続けるのを防ぐ)。
        let mut game = game_at(20, 500, 5);
        game.board.rows[501][5] = Cell::Color(ColorKind::Red);
        game.board.rows[503][2] = Cell::Oxygen;
        game.player.oxygen = AUTOPLAY_OXYGEN_SEEK_THRESHOLD - 1.0;
        let mut pilot = Autopilot::new(false);

        assert_eq!(pilot.decide_with_intent(&game).0, Intent::SeekOxygen);

        for _ in 0..stuck_frames_to_escalate() * 2 {
            pilot.decide(&game);
        }
        assert!(pilot.escalation >= 2);
        assert_eq!(pilot.decide_with_intent(&game).0, Intent::DigDown);
    }

    #[test]
    fn escalation_three_climbs_a_step_to_escape_a_dead_end() {
        let mut game = game_at(21, 500, 5);
        // 直下も両隣も岩で完全に囲まれ、頭上だけが空いている手詰まり。
        game.board.rows[501][5] = Cell::Rock { hits: 0 };
        game.board.rows[500][4] = Cell::Rock { hits: 0 };
        game.board.rows[500][6] = Cell::Rock { hits: 0 };
        let mut pilot = Autopilot::new(false);

        for _ in 0..stuck_frames_to_escalate() * 3 {
            pilot.decide(&game);
        }
        assert_eq!(pilot.escalation, 3);

        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::EscapeClimb);
        assert!(
            actions == vec![InputAction::MoveLeft] || actions == vec![InputAction::MoveRight],
            "段差登りのため横移動を出し続けるはず: {actions:?}"
        );

        // 段差登りは「同じ方向へ2回ぶつかる」ことで成立するため、抜け出せない限り
        // 同じ方向を出し続けなければならない。左右が交互に出ると永久に登れない。
        for frame in 0..10 {
            assert_eq!(
                pilot.decide(&game),
                actions,
                "frame={frame}: 登り切るまで同じ方向を出し続けるはず(左右が交互になってはいけない)"
            );
        }
    }

    #[test]
    fn decide_is_deterministic_for_the_same_board_and_internal_state() {
        // 乱数を一切使わないため、同じ盤面・同じ内部状態からは必ず同じ入力列が出る
        // (ソークテストの再現性の担保)。
        let mut game = game_at(22, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 };
        game.board.rows[503][3] = Cell::Oxygen;
        game.player.oxygen = AUTOPLAY_OXYGEN_SEEK_THRESHOLD - 1.0;

        let mut first = Autopilot::new(false);
        let mut second = Autopilot::new(false);
        for frame in 0..(AUTOPLAY_STUCK_FRAMES * 3) {
            assert_eq!(
                first.decide_with_intent(&game),
                second.decide_with_intent(&game),
                "frame={frame}: 同じ状態からは同じ判断になるはず"
            );
        }
    }

    #[test]
    fn restore_invincible_remembers_the_state_from_before_autoplay_started() {
        assert!(!Autopilot::new(false).restore_invincible());
        assert!(Autopilot::new(true).restore_invincible());
    }

    // --- ソークテスト -------------------------------------------------------

    /// 無敵ON+オートプレイで`frames`フレームぶん自動プレイし、崩れていないことを
    /// 確認する。main.rsのメインループと同じ順序(判断→入力→update)で回す。
    /// `depth_goal_m`・`width`で盤面の規模を変えられる(重力処理は毎tick盤面全体を
    /// 走査するため、盤面を小さくすると同じフレーム数でも大幅に速く回せる)。
    ///
    /// 不変条件:
    /// - パニックしない(掘削・落下・爆風・アイテム効果のどの組み合わせでも)
    /// - 無敵なのでライフは減らない(Lv.10ごとの加算で増えることはある)
    /// - 状態はPlayingかCleared(=ゴール到達)のいずれか。GameOverには決してならない
    fn soak(seed: u64, frames: u32, depth_goal_m: usize, width: usize) {
        let mut game = Game::new_with_width(seed, width, depth_goal_m);
        game.set_invincible(true);
        let mut pilot = Autopilot::new(false);
        let lives_at_start = game.player.lives;
        let delta = std::time::Duration::from_millis(FRAME_INTERVAL_MS);

        for frame in 0..frames {
            for action in pilot.decide(&game) {
                match action {
                    InputAction::Confirm => game.revive(),
                    other => {
                        game.apply_input(other);
                    }
                }
            }
            game.update(delta);

            assert!(
                game.player.lives >= lives_at_start,
                "frame={frame}: 無敵中はライフが減らないはず(lives={}, 開始時={lives_at_start})",
                game.player.lives
            );
            assert!(
                matches!(game.status, GameStatus::Playing | GameStatus::Cleared),
                "frame={frame}: PlayingかClearedのはずだが{:?}だった",
                game.status
            );
        }
    }

    #[test]
    fn soak_reaches_the_goal_without_ever_losing_a_life() {
        // 通常のテスト実行に含める短めのソーク。小さい盤面(200m・幅6)にして、
        // 「開始からゴール到達まで一度も死なずに完走できる」ところまで通しで確認する。
        // フルサイズの長時間版(`soak_long_run`)は#[ignore]付き。
        let mut game = Game::new_with_width(918, 6, 200);
        game.set_invincible(true);
        let mut pilot = Autopilot::new(false);
        let delta = std::time::Duration::from_millis(FRAME_INTERVAL_MS);

        for _ in 0..4000 {
            for action in pilot.decide(&game) {
                match action {
                    InputAction::Confirm => game.revive(),
                    other => {
                        game.apply_input(other);
                    }
                }
            }
            game.update(delta);
            if game.status == GameStatus::Cleared {
                break;
            }
        }

        assert_eq!(
            game.status,
            GameStatus::Cleared,
            "オートプレイだけでゴールまで到達できるはず(到達深度={}m)",
            game.player.depth_m()
        );
        assert!(
            game.player.lives >= LIVES_DEFAULT,
            "無敵中はライフが減らないはず: {}",
            game.player.lives
        );
    }

    #[test]
    fn soak_survives_a_few_thousand_frames_on_the_normal_course() {
        soak(918, 1500, FIELD_DEPTH_M, FIELD_WIDTH_DEFAULT);
    }

    #[test]
    #[ignore = "長時間のソークテスト。cargo test --release -- --ignored で実行する"]
    fn soak_long_run() {
        // 稀なバグの再現が主眼なので、シードを変えて複数回まわす(盤面生成・ボム
        // 出現パターンはシード由来で決まるため、シードを変えないと同じ地形しか
        // 踏めない)。
        for seed in 0..8 {
            soak(seed, 120_000, FIELD_DEPTH_M, FIELD_WIDTH_DEFAULT);
        }
    }
}

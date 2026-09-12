//! 「無敵に頼らず完走するオートプレイ」の判断ロジック(TERM独自拡張。#218/#221)。
//!
//! 長時間プレイでしか出ない稀なバグを再現・検出するソークテスト用のデバッグ機能。
//! `Game`の外に置いた「仮想キーボード」として振る舞い、盤面を見て次に押すべきキー
//! (`InputAction`)を返すだけで、ゲームの内部状態は一切直接触らない。返した入力は
//! 人間の操作と全く同じ`Game::apply_input`を通るため、AIだけが使える裏口は無い。
//!
//! 判断は毎フレーム盤面から再計算する。経路は保持しない(落下で盤面が変わるため、
//! 立てた計画を追うより都度評価する方が安全)。**乱数は一切使わず走査順も固定**
//! なので、同じ盤面・同じ内部状態からは常に同じ入力列が出る(再現性の担保)。
//!
//! # 何を避けて何を稼ぐか(#221)
//!
//! 初版(#218)は直下を掘り続けるだけで、横移動は「酸素が半分を切ってAIRを見つけた」
//! 「岩に当たった」時しか起きなかった。その結果、
//!
//! - 酸素消費の8割強が岩の破壊(1完走あたり150〜200個×20%)で、自然減少は誤差だった
//! - 無敵OFFでは完走できず、死因の6割は落下ブロックによる押し潰しだった
//!
//! ため、次の3点を柱に組み直している。
//!
//! 1. **押し潰し回避が最優先**。危険は距離でも時間でもなく「余裕(行)」で測る
//!    (`threat_slack_rows`)。頭上の列を`AUTOPLAY_THREAT_SCAN_ROWS`行ぶん見て、
//!    揺れの残り・横移動の所要時間・掘り下げで稼げる1行を、すべてブロックの落下tickで
//!    割って行数に揃えて足し引きする。`AUTOPLAY_THREAT_MIN_SLACK_ROWS`を割り込む列へは
//!    入らず、今いる列が割り込んだら採点をやめて最も余裕が残る手で逃げる。
//! 2. **空洞へ不用意に飛び込まない**。自由落下中は横移動が効かない上、ブロックの落下
//!    tickは深度で最大2.5倍まで短くなるのにプレイヤーの自由落下tickは一定なので、
//!    最深帯ではブロックの方が2.5倍速く落ちてくる。落ち切るまでの行数を危険の見積りにも
//!    列の採点にも織り込む(`commitment_fall_rows`)。
//! 3. **岩は原則割らない**。岩1個は酸素20%+5ヒット分の時間で、深度0mなら70行・
//!    最深でも30行ぶんの下降と釣り合う(`rock_cost_rows`)。横1列の迂回は1行ぶんにも
//!    満たない(`detour_cost_rows`)ので、迂回できる限り迂回が勝つ。固定の閾値ではなく
//!    この比較式で決めるため、定数を変えても判断が追従する。
//! 4. **AIRは価値がある間に拾う**。逼迫してから探し始めるのでは間に合わないため、
//!    実効回復量(`min(50, 100-残量)`)が`AUTOPLAY_AIR_MIN_GAIN`以上なら平常時から
//!    列スコアへ加点して寄り道する。
//!
//! 経路探索はA*等を使わず、毎フレーム「現在行から先読み範囲で各列を採点→最良列へ
//! 横移動→着いたら掘る」を繰り返す(`score_columns`)。目的列には
//! `AUTOPLAY_COLUMN_SWITCH_MARGIN`のヒステリシスを効かせ、AIRと危険回避の間で
//! 左右に往復するのを防ぐ。
//!
//! 支持関係は「移った後」で判定するのが要点(`column_threat`)。AIRを取る・横へ掘り
//! 抜くとそのマスは消えるため、移る前の盤面では支えられて見えるブロックが、移った
//! 瞬間に落ちてくる。実測ではこれが押し潰しの最多パターンだった。
//!
//! 到達度は`soak_short_course_quality`(300m)と
//! `soak_full_course_without_invincibility`(1000m)に実測値付きで記録している。
//!
//! 無敵(`Game::set_invincible`)とは独立したトグルで、Tキーはオートプレイだけを
//! 切り替える(#221。無敵はGキーが単独で管理する)。無人で回り続けるアトラクト
//! モードだけは安全策として無敵も併用する。

use crate::constants::{
    AUTOPLAY_AIR_DETOUR_MAX_COLS, AUTOPLAY_AIR_MIN_GAIN, AUTOPLAY_BOMB_EVADE_MS,
    AUTOPLAY_COLUMN_SCAN_RADIUS, AUTOPLAY_COLUMN_SWITCH_MARGIN, AUTOPLAY_DESCENT_WATCHDOG_FRAMES,
    AUTOPLAY_EMERGENCY_AIR_LOOKAHEAD_ROWS, AUTOPLAY_EMERGENCY_AIR_SCORE_MULTIPLIER,
    AUTOPLAY_EMERGENCY_HORIZON_SEC, AUTOPLAY_LOOKAHEAD_ROWS, AUTOPLAY_REVIVE_DELAY_MS,
    AUTOPLAY_SCORE_AIR_DIVISOR, AUTOPLAY_SCORE_ITEM_BONUS, AUTOPLAY_SCORE_LATERAL_PER_COL,
    AUTOPLAY_SCORE_THREAT_PENALTY, AUTOPLAY_SCORE_VOID_EXPOSURE, AUTOPLAY_STUCK_FRAMES,
    AUTOPLAY_THREAT_MIN_SLACK_ROWS, AUTOPLAY_THREAT_REACTION_STEPS, AUTOPLAY_THREAT_SCAN_ROWS,
    BOMB_BLAST_ROW_RANGE, FRAME_INTERVAL_MS, INPUT_COOLDOWN_MS, OXYGEN_CAPSULE_RESTORE,
    OXYGEN_DECAY_DEPTH_MAX_MULTIPLIER, OXYGEN_DECAY_PER_SEC, OXYGEN_MAX, ROCK_BREAK_OXYGEN_PENALTY,
    ROCK_HITS_TO_BREAK, depth_fraction,
};
use crate::game::board::{Cell, ItemEffect, connected_same_color};
use crate::game::player::Direction;
use crate::game::{BombPhase, Game, GameStatus, InputAction};

/// 手詰まり(escalation1以上)のとき、現在列の列スコアから引く減点。3列ぶんの横移動
/// より重いので、その場に留まる選択は必ず捨てられる。
const STUCK_STAY_PENALTY: f32 = 3.0;

/// オートプレイの状態。`Game`とは独立して持ち、Tキーで生成・破棄する。
pub struct Autopilot {
    /// オートプレイを開始する直前の無敵状態。アトラクトモード(無人デモのため無敵を
    /// 強制ONにする)を抜けるときに、この値へ戻して元の設定を壊さないようにする。
    restore_invincible: bool,
    /// 横移動の優先方向。同点の列が並んだときの選び方と、手詰まり脱出の向きに使う。
    /// 書き換えるのは「escalationが0→1へ上がった瞬間の1回だけの反転」と
    /// 「escalation0で目的列を決めたときの追従」の2箇所だけ(#221)。毎フレーム
    /// 書き換えると左右に往復して進めなくなる。
    side_preference: Direction,
    /// 列採点で選んだ目的列。到着するか候補から外れるまで保持し、僅差での乗り換えを
    /// 抑える(ヒステリシス)。
    target_col: Option<usize>,
    /// 前フレームのプレイヤー位置(進捗判定用)。
    last_pos: (usize, usize),
    /// 位置が変わらないまま経過したフレーム数。
    frames_without_progress: u32,
    /// これまでに到達した最も深い行。
    last_row: usize,
    /// 行(深度)が進まないまま経過したフレーム数。横移動が自由になると位置は変わり
    /// 続けるため、位置ベースの停滞検知だけでは「同じ行を横に往復し続ける」手詰まりを
    /// 見逃す。
    frames_without_descent: u32,
    /// 手詰まりの度合い。0=通常、1=逆側を試し採点を全幅へ広げる、2=岩も選択肢に入れる、
    /// 3=段差登りを試みる。行が進んだ時点で0へ戻る。
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
    /// 頭上から落ちてくるブロックに潰される前に逃げる
    DodgeOverhead,
    /// 酸素が減ったのでAIRを取りに行く
    SeekOxygen,
    /// 直下のブロックを掘って進む(既定の行動)
    DigDown,
    /// 迂回できない岩ブロックを掘る
    BreakRock,
    /// 空いているマスを横へ歩いて別の列へ移る
    Sidestep,
    /// 横のブロックを掘って別の列へ移る
    DigSideways,
    /// 手詰まりからの脱出として段差を登る
    EscapeClimb,
    /// GameOverから自動で復活する
    Revive,
}

/// 頭上から落ちてくる塊の情報(TERM独自拡張。#221)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ColumnThreat {
    /// プレイヤーの行から何行上にあるか(1=直上)。
    dist: usize,
    /// まだ揺れ(落下開始前の猶予)の最中か。落下中ならfalse。
    shaking: bool,
}

/// 1つの列の採点結果(TERM独自拡張。#221)。
#[derive(Debug, Clone, Copy, PartialEq)]
struct ColumnScore {
    col: usize,
    score: f32,
    /// 内訳のうちAIR加点ぶん。0より大きければ「AIR目当てで選んだ列」と分かるため、
    /// `Intent`を`SeekOxygen`にするかの判断に使う。
    air: f32,
}

impl Autopilot {
    /// オートプレイを開始する。`restore_invincible`には、開始直前の無敵状態
    /// (アトラクトモードを抜けるときに戻す値)を渡す。
    pub fn new(restore_invincible: bool) -> Self {
        Autopilot {
            restore_invincible,
            side_preference: Direction::Right,
            target_col: None,
            // 実在しない番兵ではなく(0,0)で始める。初回の`decide`で実際の位置と
            // 比較され、ほぼ必ず「進捗あり」と判定されてカウンタが初期化される。
            last_pos: (0, 0),
            frames_without_progress: 0,
            last_row: 0,
            frames_without_descent: 0,
            escalation: 0,
            game_over_frames: 0,
        }
    }

    /// オートプレイ開始直前の無敵状態(アトラクトモードを抜けるときに戻す値)。
    pub fn restore_invincible(&self) -> bool {
        self.restore_invincible
    }

    /// 1フレームぶんの判断。押すべきキーだけを返す。
    pub fn decide(&mut self, game: &Game) -> Vec<InputAction> {
        self.decide_with_intent(game).1
    }

    /// `decide`に、そのフレームの判断理由(`Intent`)を添えた版。0〜2個の
    /// `InputAction`を返す(向き変更1個+掘削1個の組み合わせもある)。
    ///
    /// 優先順は「隣のボムを消す→爆風から逃げる→押し潰しから逃げる→良い列へ寄る→
    /// 掘り下げる」。どれも同じ安全判定(`is_safe_to_enter`/`column_is_safe_to_enter`)を
    /// 通してから動くため、目的が違う行動どうしで左右に往復することがない。
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
        if let Some(actions) = self.escape_overhead_threat(game) {
            return (Intent::DodgeOverhead, actions);
        }

        // 手詰まりが極まったら列の採点をやめ、段差を登って別の場所からやり直す。
        // 採点は「今いる行から横に見える範囲」しか評価しないため、その範囲ごと
        // 行き止まりの時に抜け出す手段がこれしかない。
        if self.escalation >= 3
            && let Some(actions) = self.escape_climb(game)
        {
            return (Intent::EscapeClimb, actions);
        }

        if let Some(target) = self.choose_target_column(game)
            && target.col != game.player.col
        {
            return self.step_toward(game, &target);
        }
        self.descend(game)
    }

    /// 進捗を記録し、手詰まりが続けば`escalation`を1段上げる。
    ///
    /// 「進捗」は位置の変化(`AUTOPLAY_STUCK_FRAMES`)と行の前進
    /// (`AUTOPLAY_DESCENT_WATCHDOG_FRAMES`)の2本立てで見る。横移動が自由になると
    /// 位置は変わり続けるため、位置だけでは「同じ行を横に往復し続ける」手詰まりを
    /// 検出できない。段階を戻すのは行が進んだとき(=本当の前進)だけにする。
    fn note_progress(&mut self, pos: (usize, usize)) {
        if pos == self.last_pos {
            self.frames_without_progress = self.frames_without_progress.saturating_add(1);
        } else {
            self.last_pos = pos;
            self.frames_without_progress = 0;
        }

        if pos.0 > self.last_row {
            self.last_row = pos.0;
            self.frames_without_descent = 0;
            self.escalation = 0;
            return;
        }
        self.frames_without_descent = self.frames_without_descent.saturating_add(1);

        let stuck = self.frames_without_progress > AUTOPLAY_STUCK_FRAMES
            || self.frames_without_descent > AUTOPLAY_DESCENT_WATCHDOG_FRAMES;
        if !stuck {
            return;
        }
        self.frames_without_progress = 0;
        self.frames_without_descent = 0;
        let previous = self.escalation;
        self.escalation = (self.escalation + 1).min(3);
        if previous == 0 && self.escalation == 1 {
            // 0→1へ上がった瞬間だけ1回反転する。毎フレーム逆側を返す実装にすると、
            // 段差登り(同じ方向へ2回ぶつかる必要がある)が永久に成立しない。
            self.side_preference = opposite(self.side_preference);
            self.target_col = None;
        }
    }

    // --- 1. 隣接ボムの除去 ---------------------------------------------------

    /// 隣接マスに静止中(Settling/Ticking)のボムがあれば掘って取り除く。まだ登場・
    /// 投擲演出中(Entering/Rolling)のボムは盤面上の実体が無いため対象外。
    fn defuse_adjacent_bomb(&self, game: &Game) -> Option<Vec<InputAction>> {
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
    /// 同じ行にいる場合は横へ動いても同じ行のまま(爆風は幅全体に届く)で逃げたことに
    /// ならないため、まず掘って行を変える。
    fn evade_imminent_bomb(&self, game: &Game) -> Option<Vec<InputAction>> {
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

    // --- 3. 押し潰しの回避 ---------------------------------------------------

    /// 自分の列の頭上に落ちてくる塊が近すぎるなら、列の採点をやめて最も速くこのマスを
    /// 離れられる手を打つ。
    ///
    /// 危険と判断したら必ず何かを返す(`None`で採点へ戻さない)のが要点。採点側は
    /// AIRや先の掘りやすさで列を選ぶため、危険なまま`DigSideways`のような3手がかりの
    /// 行動を選んでしまい、その間に潰される(実測した死因の1つ)。
    ///
    /// 逃げ方は横移動とは限らない。掘り下げも1行ぶん距離を稼げるので、直下が柔らかければ
    /// 横へ回るより速い。逆に直下が岩なら5ヒットぶん足止めされるため横へ逃げる。
    fn escape_overhead_threat(&self, game: &Game) -> Option<Vec<InputAction>> {
        let (row, col) = game.player.position();
        let threat = self.column_threat(game, col, row)?;
        let stay_slack = self.threat_slack_rows(game, col, &threat);
        if stay_slack >= min_slack_rows(game) {
            return None;
        }

        // 「その手を打った後にどれだけ余裕が残るか」で選ぶ。速さで選ぶと、掘り下げが
        // 一番速いのにその先が空洞で、掘った勢いのまま落下して潰される(実測した死因)。
        let mut options: Vec<(f32, u64, bool, Vec<InputAction>)> = Vec::new();
        let descend_ms = self.descend_time_ms(game);
        if descend_ms != u64::MAX {
            options.push((
                stay_slack,
                descend_ms,
                false,
                self.drill_vertically(game, Direction::Down),
            ));
        }
        if game.player_is_grounded() {
            for dir in [Direction::Left, Direction::Right] {
                let Some(cost_ms) = self.lateral_step_ms(game, dir) else {
                    continue;
                };
                let Some(target) = neighbor(game, (row, col), dir) else {
                    continue;
                };
                options.push((
                    self.column_slack_rows(game, target.1),
                    cost_ms,
                    dir != self.side_preference,
                    self.lateral_actions(game, dir),
                ));
            }
        }
        options.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        options.into_iter().next().map(|(_, _, _, actions)| actions)
    }

    /// `col`列の頭上`AUTOPLAY_THREAT_SCAN_ROWS`行を`from_row`から上へ走査し、最初に
    /// ぶつかった固体ブロックが不安定(揺れ中または未支持)なら脅威として返す。
    /// 支持された固体ブロックは遮蔽物なので、その先は見ずに「脅威なし」とする。
    /// Empty/AIR/アイテムは押し潰さないが遮蔽にもならないため素通りする。
    fn column_threat(&self, game: &Game, col: usize, from_row: usize) -> Option<ColumnThreat> {
        // 他の列へ移ると、足元になるマスはプレイヤーが占める。AIR・アイテムなら取得して、
        // ブロックなら掘って(色ブロックは同色連結グループごと)消えるため、そこに乗って
        // いたものは支えを失って落ちてくる。移った後の支持関係で見ないと「AIRを取った
        // 瞬間に真上のブロックに潰される」「横へ掘り抜いた瞬間にその上が崩れる」を
        // 見落とす(どちらも実測した死因)。
        let vacated: Vec<(usize, usize)> = if col == game.player.col {
            Vec::new()
        } else {
            let mut cells = cells_removed_by_drilling(game, (from_row, col));
            if !cells.contains(&(from_row, col)) {
                cells.push((from_row, col));
            }
            cells
        };

        for dist in 1..=AUTOPLAY_THREAT_SCAN_ROWS {
            let row = from_row.checked_sub(dist)?;
            match game.board.cell(row, col) {
                Cell::Empty | Cell::Oxygen | Cell::Item(_) => continue,
                _ => {
                    let unstable = game.is_cell_unstable(row, col)
                        || is_unsupported_after_removal(game, &vacated, (row, col));
                    return unstable.then(|| ColumnThreat {
                        dist,
                        shaking: game.is_cell_shaking(row, col),
                    });
                }
            }
        }
        None
    }

    /// `c`列へ移って1手ぶん進めた時点で、頭上の塊との間に何行ぶんの余裕が残るか。
    /// 1行を割り込むほど詰められるなら、その列にいる間に潰される。
    ///
    /// 距離(行)だけでも時間(ms)だけでも足りないため、すべてを「行」に揃えて足し引きする:
    ///
    /// - 揺れの残り・横移動の所要時間は、ブロックの落下tickで割って行数へ換算する
    /// - **自由落下で入る列は、落ち切るまで横移動が効かない**。プレイヤーの自由落下tickは
    ///   深度で変わらないのに対しブロックの落下tickは最大2.5倍まで速くなるため、深いほど
    ///   落下中に差を詰められる(実測した死因の大半がこれ。落下先の頭上を見ずに飛び込むと
    ///   着地と同時に潰される)
    /// - 接地したまま掘る列は、掘っている間に詰められるが、掘り抜ければ1行ぶん離れられる
    fn threat_slack_rows(&self, game: &Game, c: usize, threat: &ColumnThreat) -> f32 {
        let block_tick = game.effective_block_fall_tick_ms().max(1) as f32;

        let mut slack = threat.dist as f32 + shake_allowance_ms(game, threat) as f32 / block_tick;
        slack -= self.lateral_travel_ms(game, c) as f32 / block_tick;

        if free_fall_rows(game, c, game.player.row + 1) == 0 {
            // 接地したまま1行掘る。掘っている間に詰められるが、掘り抜ければ1行離れられる。
            slack += 1.0 - self.descend_time_in_column(game, c) as f32 / block_tick;
        }
        slack - commitment_fall_rows(game, c) as f32 * fall_rows_lost_per_row(game)
    }

    /// その列にいた場合に残る余裕(行)。頭上に落ちてくる塊が無ければ無限大。
    fn column_slack_rows(&self, game: &Game, c: usize) -> f32 {
        match self.column_threat(game, c, game.player.row) {
            None => f32::INFINITY,
            Some(threat) => self.threat_slack_rows(game, c, &threat),
        }
    }

    /// 現在行を横に進んで`c`列へ着くまでの所要時間(ms)。
    ///
    /// 空きマスは移動1回、ブロックは「ぶつかって向きを合わせる→掘る→動く」で数える。
    /// 全部を掘る前提の概算にすると、深い場所では1列あたり3行ぶんもの余裕を要求する
    /// ことになり、安全な列まで軒並み候補から外れて岩を割る羽目になる。
    fn lateral_travel_ms(&self, game: &Game, c: usize) -> u64 {
        let (row, col) = game.player.position();
        let step: isize = if c < col { -1 } else { 1 };
        let mut total = 0;
        let mut cursor = col;
        while cursor != c {
            let Some(next) = cursor.checked_add_signed(step) else {
                break;
            };
            if next >= game.board.width() {
                break;
            }
            total += game.move_cooldown_ms() + FRAME_INTERVAL_MS;
            match game.board.cell(row, next) {
                Cell::Empty | Cell::Oxygen | Cell::Item(_) => {}
                Cell::Rock { hits } => {
                    total += game.move_cooldown_ms() + FRAME_INTERVAL_MS;
                    total += u64::from(ROCK_HITS_TO_BREAK.saturating_sub(hits)) * drill_action_ms();
                }
                _ => total += game.move_cooldown_ms() + FRAME_INTERVAL_MS + drill_action_ms(),
            }
            cursor = next;
        }
        // 移動している間にも盤面は動く(途中の列でブロックが落ち始める、掘った先が
        // 崩れる)ため、見積りは安全側へ倍にしておく。実測でも、ぴったりの見積りにすると
        // 「間に合うつもりで動き出して間に合わない」押し潰しが倍近くに増えた。
        total * 2
    }

    /// 掘って(または落ちて)1行下がるのに要する時間(ms)。最深行でこれ以上下がれない
    /// 場合は`u64::MAX`を返し、必ず横へ逃げる判断になるようにする。
    fn descend_time_ms(&self, game: &Game) -> u64 {
        self.descend_time_in_column(game, game.player.col)
    }

    /// `col`列の、プレイヤーと同じ行から1行下がるのに要する時間(ms)。岩は残りヒット数
    /// ぶんだけ余計にかかる。
    ///
    /// どの見積りにも入力1回あたり`FRAME_INTERVAL_MS`を足す。判断は1フレームに1回しか
    /// できず、クールダウンが明けるのを待つ空振りフレームが必ず挟まるため、クールダウン
    /// そのものだけで数えると実際より速く動けるつもりになる。
    fn descend_time_in_column(&self, game: &Game, col: usize) -> u64 {
        match game.board.cell_or_none(game.player.row + 1, col) {
            None => u64::MAX,
            Some(Cell::Empty | Cell::Oxygen | Cell::Item(_)) => game.player_fall_tick_ms(),
            Some(Cell::Rock { hits }) => {
                u64::from(ROCK_HITS_TO_BREAK.saturating_sub(hits)) * drill_action_ms()
            }
            Some(_) => drill_action_ms(),
        }
    }

    /// `dir`へ1列移り終えるまでの所要時間(ms)。空きマスなら移動1回、掘れるブロックなら
    /// 「ぶつかって向きを合わせる→掘る→動く」、岩なら破壊に要するヒット数ぶんが乗る。
    /// 盤外・静止ボムで塞がっている場合は`None`。
    fn lateral_step_ms(&self, game: &Game, dir: Direction) -> Option<u64> {
        let target = neighbor(game, game.player.position(), dir)?;
        if settled_bomb_at(game, target) {
            return None;
        }
        let move_ms = game.move_cooldown_ms() + FRAME_INTERVAL_MS;
        Some(match game.board.cell(target.0, target.1) {
            Cell::Empty | Cell::Oxygen | Cell::Item(_) => move_ms,
            Cell::Color(_) | Cell::Star { .. } | Cell::Diamond => move_ms * 2 + drill_action_ms(),
            Cell::Rock { hits } => {
                move_ms * 2 + u64::from(ROCK_HITS_TO_BREAK.saturating_sub(hits)) * drill_action_ms()
            }
        })
    }

    // --- 4. 列の採点と目的列の決定 -------------------------------------------

    /// 目的列を決める。候補が1つも無い(全て到達不能)なら`None`を返し、呼び出し側は
    /// 掘り下げ(岩の破壊を含む)へ進む。
    ///
    /// 一度決めた目的列は`AUTOPLAY_COLUMN_SWITCH_MARGIN`を超える差がつくまで乗り換え
    /// ない。僅差で乗り換えると、AIR目当てで右へ・危険回避で左へ…と往復して一歩も
    /// 進まなくなる(初版で実測した振動)。
    fn choose_target_column(&mut self, game: &Game) -> Option<ColumnScore> {
        let col = game.player.col;
        let scores = self.score_columns(game);
        let mut best: Option<ColumnScore> = None;
        for candidate in &scores {
            if best.is_none_or(|current| self.is_better(col, candidate, &current)) {
                best = Some(*candidate);
            }
        }
        let best = best?;

        let chosen = match self.target_col {
            Some(previous) if previous != col => match scores.iter().find(|s| s.col == previous) {
                Some(kept) if best.score <= kept.score + AUTOPLAY_COLUMN_SWITCH_MARGIN => *kept,
                // 候補から消えた(到達不能になった)か、明確に良い列が現れたら乗り換える。
                _ => best,
            },
            _ => best,
        };

        self.target_col = (chosen.col != col).then_some(chosen.col);
        if self.escalation == 0 && chosen.col != col {
            self.side_preference = if chosen.col < col {
                Direction::Left
            } else {
                Direction::Right
            };
        }
        Some(chosen)
    }

    /// 同点時の決定的な優先順: 現在列 → `side_preference`側 → 横距離が近い方。
    /// 走査順と合わせて、同じ盤面からは必ず同じ列が選ばれる。
    fn is_better(&self, col: usize, a: &ColumnScore, b: &ColumnScore) -> bool {
        if a.score != b.score {
            return a.score > b.score;
        }
        if (a.col == col) != (b.col == col) {
            return a.col == col;
        }
        let preferred = |c: usize| match self.side_preference {
            Direction::Left => c < col,
            _ => c > col,
        };
        if preferred(a.col) != preferred(b.col) {
            return preferred(a.col);
        }
        a.col.abs_diff(col) < b.col.abs_diff(col)
    }

    /// 候補列を左から順に採点する。候補範囲は通常±`AUTOPLAY_COLUMN_SCAN_RADIUS`列、
    /// 手詰まり(escalation1以上)または酸素の緊急時は全幅。
    fn score_columns(&self, game: &Game) -> Vec<ColumnScore> {
        let col = game.player.col;
        let width = game.board.width();
        let emergency = is_emergency(game);
        let (lo, hi) = if emergency || self.escalation >= 1 {
            (0, width - 1)
        } else {
            (
                col.saturating_sub(AUTOPLAY_COLUMN_SCAN_RADIUS),
                (col + AUTOPLAY_COLUMN_SCAN_RADIUS).min(width - 1),
            )
        };

        // 経路の途中で通り過ぎるだけの列も、入った瞬間に潰されるなら通ってはいけない。
        // 目的列しか見ないと「安全な列を目指して危険な列を踏み抜く」ことになる(実測した
        // 死因の最多パターン)。列ごとに一度だけ判定して経路チェックで使い回す。
        let safe: Vec<bool> = (lo..=hi)
            .map(|c| self.column_is_safe_to_enter(game, c))
            .collect();

        (lo..=hi)
            .filter_map(|c| self.score_column(game, c, emergency, &safe, lo))
            .collect()
    }

    /// 1列ぶんの採点。到達できない・入った瞬間に潰される列は`None`(=スコア-∞)。
    ///
    /// 基礎点は`clear_run`(その列を岩に当たらず掘り進める行数、上限
    /// `AUTOPLAY_LOOKAHEAD_ROWS`)で、単位は「行」。加減点もすべて行に換算して揃える。
    fn score_column(
        &self,
        game: &Game,
        c: usize,
        emergency: bool,
        safe: &[bool],
        safe_offset: usize,
    ) -> Option<ColumnScore> {
        let (row, col) = game.player.position();
        let dist = c.abs_diff(col);

        if !self.lateral_path_is_open(game, c, safe, safe_offset) {
            return None;
        }

        // 頭上の塊は「その列へ移って1手ぶん進めた後、まだ何行ぶん離れていられるか」で
        // 評価する(`threat_slack_rows`)。
        //
        // 掘り進んだ後の自分の列には、掘ってきた穴の天井が必ず未支持のまま残る。つまり
        // 現在列はほぼ常に「脅威あり」なので、脅威の有無や距離をそのまま減点にすると
        // 現在列だけが永久に不利になり、AIが毎フレーム横へ逃げて前へ進まなくなる。
        // 余裕そのもので測れば、振り切れる見込みがある限り減点は0になる。
        let threat_penalty = match self.column_threat(game, c, row) {
            None => 0.0,
            Some(threat) => {
                let slack = self.threat_slack_rows(game, c, &threat);
                let minimum = min_slack_rows(game);
                if slack < minimum {
                    return None;
                }
                let comfort = slack / min_slack_rows(game) - 1.0;
                AUTOPLAY_SCORE_THREAT_PENALTY * (1.0 - comfort).clamp(0.0, 1.0)
            }
        };

        let gain = air_gain(game.player.oxygen);
        let air_counts =
            gain >= AUTOPLAY_AIR_MIN_GAIN && (emergency || dist <= AUTOPLAY_AIR_DETOUR_MAX_COLS);
        let air_multiplier = if emergency {
            AUTOPLAY_EMERGENCY_AIR_SCORE_MULTIPLIER
        } else {
            1.0
        };
        let scan_rows = if emergency {
            AUTOPLAY_EMERGENCY_AIR_LOOKAHEAD_ROWS
        } else {
            AUTOPLAY_LOOKAHEAD_ROWS
        };

        let mut run = 0usize;
        let mut air = 0.0f32;
        let mut items = 0.0f32;
        let mut rock_penalty = 0.0f32;
        let mut run_open = true;
        for d in 1..=scan_rows {
            let Some(cell) = game.board.cell_or_none(row + d, c) else {
                break;
            };
            if run_open {
                if settled_bomb_at(game, (row + d, c)) {
                    run_open = false;
                } else if matches!(cell, Cell::Rock { .. }) {
                    if self.escalation >= 2 {
                        // 手が尽きたら岩も選択肢に入れる。代償は`rock_cost_rows`ぶんで、
                        // 先読み範囲(14行)より必ず大きいため他に手が無い時しか選ばれない。
                        rock_penalty += rock_cost_rows(game);
                    } else {
                        run_open = false;
                    }
                }
            }
            if run_open && d <= AUTOPLAY_LOOKAHEAD_ROWS {
                run = d;
            }
            // AIR・アイテムは経路が塞がっていても数える。盤面は落下で刻々と変わるため、
            // 今この瞬間に塞がっていることを理由に切り捨てない。
            match cell {
                Cell::Oxygen if air_counts => {
                    air += gain / AUTOPLAY_SCORE_AIR_DIVISOR * air_multiplier;
                }
                Cell::Item(ItemEffect::ClearAbove | ItemEffect::StarifyScreen) => {
                    items += AUTOPLAY_SCORE_ITEM_BONUS;
                }
                _ => {}
            }
        }

        // 横移動の代償は「1列あたりの目安(ふらつき防止のための下駄)」と「実際の所要
        // 時間の行換算」の大きい方。移動クールダウンを遅く設定してあるほど後者が効く。
        let lateral =
            (dist as f32 * AUTOPLAY_SCORE_LATERAL_PER_COL).max(detour_cost_rows(game, dist));
        let stay_penalty = if self.escalation >= 1 && c == col {
            STUCK_STAY_PENALTY
        } else {
            0.0
        };
        // 直下が岩なら、その列で1行進むために必ず酸素20%を払う。`run`が0になるだけでは
        // 「割るのはタダ」に見えてしまい、数十行ぶんの遠回りより岩を選ぶ(実測では
        // 1走あたり40個も割っていた)。設計R1の「岩と迂回のコスト比較」をそのまま
        // スコアに載せる。全列が岩なら全列が同じだけ減点されるので、他に手が無いときは
        // 今まで通り割る。
        let rock_below_penalty =
            if matches!(game.board.cell_or_none(row + 1, c), Some(Cell::Rock { .. })) {
                rock_cost_rows(game)
            } else {
                0.0
            };
        // 今この瞬間に脅威が見えていなくても、長い空洞は危険を抱え込む。落下中は
        // 何もできず、落ちている間に頭上の塊が新たに崩れれば着地と同時に潰される。
        // 掘り進む方が速くもあるので(掘削80ms+判断1フレーム 対 落下150ms)、
        // 深いほど「穴に飛び込まず掘って下りる」を選ばせる。
        let void_penalty = commitment_fall_rows(game, c) as f32
            * fall_rows_lost_per_row(game)
            * AUTOPLAY_SCORE_VOID_EXPOSURE;

        Some(ColumnScore {
            col: c,
            score: run as f32 + air + items
                - lateral
                - threat_penalty
                - rock_penalty
                - stay_penalty
                - void_penalty
                - rock_below_penalty,
            air,
        })
    }

    /// 現在行を横に進んで`c`列へ到達できるか。途中に岩(escalation2未満)・静止ボムが
    /// あるか、頭上の塊に潰される列を踏むなら通れない。落下中は横移動自体が通らない
    /// ため、現在列以外は到達不能とする。
    fn lateral_path_is_open(
        &self,
        game: &Game,
        c: usize,
        safe: &[bool],
        safe_offset: usize,
    ) -> bool {
        let (row, col) = game.player.position();
        if c == col {
            return true;
        }
        if !game.player_is_grounded() {
            return false;
        }
        let step: isize = if c < col { -1 } else { 1 };
        let mut cursor = col;
        while cursor != c {
            let Some(next) = cursor.checked_add_signed(step) else {
                return false;
            };
            if next >= game.board.width() {
                return false;
            }
            if settled_bomb_at(game, (row, next)) {
                return false;
            }
            if matches!(game.board.cell(row, next), Cell::Rock { .. }) && self.escalation < 2 {
                return false;
            }
            if !safe
                .get(next.wrapping_sub(safe_offset))
                .copied()
                .unwrap_or(false)
            {
                return false;
            }
            cursor = next;
        }
        true
    }

    /// 目的列へ1歩進む。空きマスなら歩き、ブロックなら掘って道を作る。
    ///
    /// 横移動は向き変更専用の入力が無いため、まず`MoveX`でぶつかってfacingを合わせ、
    /// 次のフレームで掘る。移動クールダウン中は横移動処理がfacingを変えずに抜けるので、
    /// `MoveX`と`Drill`を同じフレームに出すと真下を掘ってしまう(必ず分ける)。
    fn step_toward(&self, game: &Game, target: &ColumnScore) -> (Intent, Vec<InputAction>) {
        let (row, col) = game.player.position();
        let dir = if target.col < col {
            Direction::Left
        } else {
            Direction::Right
        };
        let Some(next) = neighbor(game, (row, col), dir) else {
            return self.descend(game);
        };
        let intent = match game.board.cell(next.0, next.1) {
            Cell::Empty | Cell::Oxygen | Cell::Item(_) => {
                if target.air > 0.0 {
                    Intent::SeekOxygen
                } else {
                    Intent::Sidestep
                }
            }
            Cell::Rock { .. } => Intent::BreakRock,
            Cell::Color(_) | Cell::Star { .. } | Cell::Diamond => Intent::DigSideways,
        };
        (intent, self.lateral_actions(game, dir))
    }

    // --- 5. 下降(既定の行動) ------------------------------------------------

    /// 既定の行動。直下の状況に応じて、自由落下を待つ・掘る・手詰まりなら段差を登る、
    /// を選ぶ。ここへ来るのは「今の列が最良」と採点で決まった後なので、直下が岩でも
    /// (迂回が全て塞がっているということなので)掘って進む。
    fn descend(&self, game: &Game) -> (Intent, Vec<InputAction>) {
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
            Cell::Rock { .. } => (
                Intent::BreakRock,
                self.drill_vertically(game, Direction::Down),
            ),
        }
    }

    /// 手詰まり脱出用の段差登り。頭上が空いている場合のみ、`side_preference`へ
    /// `MoveX`を出し続ける。`move_lateral`の段差登りは「同じ方向へ2回ぶつかる」ことで
    /// 成立するため、同方向の入力を連投してよいのはこの経路だけ(通常の`step_toward`は
    /// 誤って段差を登らないよう、ぶつかった次は掘りに切り替える)。
    fn escape_climb(&self, game: &Game) -> Option<Vec<InputAction>> {
        let (row, col) = game.player.position();
        if row == 0 || game.board.cell(row - 1, col) != Cell::Empty {
            return None;
        }
        neighbor(game, (row, col), self.side_preference)?;
        Some(vec![move_action(self.side_preference)])
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

    /// 横へ1マス進むための、そのフレームぶんの入力。空きマスならそのまま移動し、
    /// ブロックならfacingを合わせてから掘る(同じフレームに両方は出さない)。
    fn lateral_actions(&self, game: &Game, dir: Direction) -> Vec<InputAction> {
        let Some(target) = neighbor(game, game.player.position(), dir) else {
            return Vec::new();
        };
        match game.board.cell(target.0, target.1) {
            Cell::Empty | Cell::Oxygen | Cell::Item(_) => vec![move_action(dir)],
            _ if game.player.facing == dir => vec![InputAction::Drill],
            _ => vec![move_action(dir)],
        }
    }

    /// 直下を掘って行を変える(爆風からの緊急避難用)。既に直下が空いていれば落下を
    /// 待つだけでよいので`None`を返し、呼び出し側の次の手段へ譲る。
    fn dig_down_to_change_row(&self, game: &Game) -> Option<Vec<InputAction>> {
        let (row, col) = game.player.position();
        match game.board.cell_or_none(row + 1, col)? {
            Cell::Empty | Cell::Oxygen | Cell::Item(_) => None,
            _ => Some(self.drill_vertically(game, Direction::Down)),
        }
    }

    /// 安全に横へ1マス逃げられる方向(`side_preference`側から順に試す)。
    fn safe_sidestep_direction(&self, game: &Game) -> Option<Direction> {
        [self.side_preference, opposite(self.side_preference)]
            .into_iter()
            .find(|&dir| self.can_step(game, dir))
    }

    /// その方向へ「掘らずに」1マス動けるか。今フレームに横移動自体が通る(接地して
    /// いる)こと、移動先が安全に入れるマスであること、飛び込んだ先で潰されないことを
    /// 確認する。
    fn can_step(&self, game: &Game, dir: Direction) -> bool {
        if !game.player_is_grounded() {
            // 落下中は`try_lateral_move`が横移動を受け付けない。
            return false;
        }
        let Some(target) = neighbor(game, game.player.position(), dir) else {
            return false;
        };
        self.is_safe_to_enter(game, target) && self.column_is_safe_to_enter(game, target.1)
    }

    /// そのマスへ掘らずに入れて、静止ボムも無いか。
    fn is_safe_to_enter(&self, game: &Game, pos: (usize, usize)) -> bool {
        matches!(
            game.board.cell(pos.0, pos.1),
            Cell::Empty | Cell::Oxygen | Cell::Item(_)
        ) && !settled_bomb_at(game, pos)
    }

    /// その列へ入っても、1手ぶん進めた後になお余裕が残るか。頭上に落ちてきそうな塊を
    /// 抱えた列へ飛び込んで自分から潰されにいくのを防ぐ。
    fn column_is_safe_to_enter(&self, game: &Game, col: usize) -> bool {
        self.column_slack_rows(game, col) >= min_slack_rows(game)
    }
}

// --- 酸素・コストの換算 ------------------------------------------------------

/// 深度別の酸素自然減少速度(%/秒)。`Game::update`の計算式と同じで、深度0mで2.0、
/// 最深で5.0になる。
fn oxygen_decay_per_sec(depth_m: usize) -> f32 {
    OXYGEN_DECAY_PER_SEC
        * (1.0 + depth_fraction(depth_m) * (OXYGEN_DECAY_DEPTH_MAX_MULTIPLIER - 1.0))
}

/// 緊急とみなす酸素残量。`AUTOPLAY_EMERGENCY_HORIZON_SEC`秒ぶんの自然減少を賄えるか
/// で判定するため、深いほど大きくなる(深度0mで12%、最深で30%)。
fn oxygen_reserve(depth_m: usize) -> f32 {
    oxygen_decay_per_sec(depth_m) * AUTOPLAY_EMERGENCY_HORIZON_SEC
}

/// 酸素が逼迫しているか。
fn is_emergency(game: &Game) -> bool {
    game.player.oxygen < oxygen_reserve(game.player.depth_m())
}

/// AIRを1個取ったときの実効回復量(%)。上限100でクランプされるため、満タンに近いほど
/// 小さくなる。
fn air_gain(oxygen: f32) -> f32 {
    OXYGEN_CAPSULE_RESTORE.min((OXYGEN_MAX - oxygen).max(0.0))
}

/// 1行ぶんの下降に相当する時間(ms)。列スコアの単位「行」と時間を行き来する換算基準。
fn row_time_ms(game: &Game) -> f32 {
    game.player_fall_tick_ms().max(1) as f32
}

/// 頭上の塊との間に最低限保つ余裕(行)。「逃げ始めるか」と「その列へ入ってよいか」の
/// 両方をこの1つの基準で判定する(別々の緩さにする案も試したが、緩めると押し潰しが、
/// 締めると逃げ場を失っての岩割り=酸素切れが増え、同じ値が最良だった)。
///
/// 1手動くのに要する時間をブロックの落下tickで割って行数へ換算し、その
/// `AUTOPLAY_THREAT_REACTION_STEPS`手ぶんを要求する。深いほどブロックだけが速く落ちる
/// ようになるため、必要な行数も自動的に増える。
fn min_slack_rows(game: &Game) -> f32 {
    let reaction_ms = (game.move_cooldown_ms() + FRAME_INTERVAL_MS) as f32;
    let block_tick = game.effective_block_fall_tick_ms().max(1) as f32;
    (AUTOPLAY_THREAT_REACTION_STEPS * reaction_ms / block_tick).max(AUTOPLAY_THREAT_MIN_SLACK_ROWS)
}

/// 自由落下1行につき、頭上の塊に詰められる行数。ブロックの落下tickは深度で最大2.5倍まで
/// 短くなるのに対しプレイヤーの自由落下tickは一定のため、深いほど大きくなる(深度0mでは
/// 両者同速なので0)。
fn fall_rows_lost_per_row(game: &Game) -> f32 {
    let block_tick = game.effective_block_fall_tick_ms().max(1) as f32;
    let player_tick = game.player_fall_tick_ms().max(1) as f32;
    (player_tick / block_tick - 1.0).max(0.0)
}

/// `c`列へ進むと、続けて何行ぶん身動きが取れなくなるか。直下が既に空洞ならそのまま
/// 落ち始め、そうでなければ1行掘り抜いた先から落ち始める。落下中は横移動が効かないため、
/// 危険の見積り(`threat_slack_rows`)と列の採点(空洞ペナルティ)の両方がこれを使う。
fn commitment_fall_rows(game: &Game, c: usize) -> usize {
    let immediate = free_fall_rows(game, c, game.player.row + 1);
    if immediate > 0 {
        immediate
    } else {
        free_fall_rows_after_drilling(game, c)
    }
}

/// `c`列の直下を掘り抜いた後に続く空洞の行数。色ブロックは同色連結グループごと消えるため、
/// 1マス掘っただけで数行ぶんの縦穴が一度に開くことがある。掘る前の盤面だけを見ていると
/// 「掘った瞬間に穴が開いてそのまま落下し、頭上の塊に潰される」を見落とす。
fn free_fall_rows_after_drilling(game: &Game, c: usize) -> usize {
    let target = (game.player.row + 1, c);
    if target.0 >= game.board.depth_rows() {
        return 0;
    }
    let removed = cells_removed_by_drilling(game, target);
    let mut rows = 0;
    while rows < AUTOPLAY_LOOKAHEAD_ROWS {
        let row = game.player.row + 2 + rows;
        if row >= game.board.depth_rows() {
            break;
        }
        match cell_after_removal(game, &removed, (row, c)) {
            Cell::Empty | Cell::Oxygen => rows += 1,
            _ => break,
        }
    }
    rows
}

/// 掘削入力1回にかかる実時間(ms)。クールダウンに加え、判断が1フレームに1回しか
/// できないぶんの待ちを足す。
fn drill_action_ms() -> u64 {
    INPUT_COOLDOWN_MS + FRAME_INTERVAL_MS
}

/// 揺れ中の塊が落ち始めるまでに見込める猶予(ms)。残りの揺れ時間は外から分からない
/// ため、期待値として半分を見込む。
fn shake_allowance_ms(game: &Game, threat: &ColumnThreat) -> u64 {
    if threat.shaking {
        game.shake_duration_ms() / 2
    } else {
        0
    }
}

/// `col`列の`from_row`から続く、自由落下で通過するマスの行数。落下が始まると着地する
/// まで横移動が効かないため、この行数がそのまま「身動きが取れない長さ」になる。
fn free_fall_rows(game: &Game, col: usize, from_row: usize) -> usize {
    let mut rows = 0;
    // 先読み範囲より深い落下は、着地までに盤面が変わるので数えても精度が出ない。
    while rows < AUTOPLAY_LOOKAHEAD_ROWS {
        let row = from_row + rows;
        match game.board.cell_or_none(row, col) {
            Some(Cell::Empty | Cell::Oxygen) if !settled_bomb_at(game, (row, col)) => rows += 1,
            _ => break,
        }
    }
    rows
}

/// 岩1個を壊す代償を「何行ぶんの下降と釣り合うか」で表す。酸素20%の直接消費に加え、
/// 5ヒットぶんの時間で進む自然減少も含める。既定設定なら深度0mで約70行、最深で
/// 約30行になり、先読み範囲(14行)より必ず大きい。
fn rock_cost_rows(game: &Game) -> f32 {
    let decay = oxygen_decay_per_sec(game.player.depth_m()).max(f32::EPSILON);
    let drill_ms = f32::from(ROCK_HITS_TO_BREAK) * INPUT_COOLDOWN_MS as f32;
    let oxygen_cost = ROCK_BREAK_OXYGEN_PENALTY + decay * (drill_ms / 1000.0);
    let oxygen_per_row = decay * (row_time_ms(game) / 1000.0);
    oxygen_cost / oxygen_per_row + drill_ms / row_time_ms(game)
}

/// `cols`列ぶん横へ迂回する代償を「何行ぶん」で表す。掘りながら進む最悪ケースで
/// 見積もっても既定設定で1列あたり約1行で、`rock_cost_rows`とは桁が違う。
fn detour_cost_rows(game: &Game, cols: usize) -> f32 {
    let per_col_ms = (game.move_cooldown_ms() + INPUT_COOLDOWN_MS) as f32;
    cols as f32 * per_col_ms / row_time_ms(game)
}

// --- 崩落予測のための「もしも盤面」検索 --------------------------------------

/// `target`を1回掘り切ったときに盤面から消えるセル。色ブロックは同色連結グループが
/// まるごと消え(spec.md 4.6)、岩・スター・ダイヤは掘ったセル1個だけが消える
/// (岩の連結巻き込みは落下着地時の自動消滅だけで、掘削では起きない)。
fn cells_removed_by_drilling(game: &Game, target: (usize, usize)) -> Vec<(usize, usize)> {
    match game.board.cell(target.0, target.1) {
        Cell::Color(color) => connected_same_color(&game.board, target, color),
        Cell::Rock { .. } | Cell::Star { .. } | Cell::Diamond => vec![target],
        Cell::Empty | Cell::Oxygen | Cell::Item(_) => Vec::new(),
    }
}

/// `removed`を空とみなした盤面でのセル内容。
fn cell_after_removal(game: &Game, removed: &[(usize, usize)], pos: (usize, usize)) -> Cell {
    if removed.contains(&pos) {
        Cell::Empty
    } else {
        game.board.cell(pos.0, pos.1)
    }
}

/// `removed`を空とみなした盤面で、`start`の属する塊が支えを失うか。
///
/// 塊の作り方・支持判定は`physics`の重力処理と同じ規則に揃えている(色は同色4方向
/// 連結、岩はhits問わず連結、スター/ダイヤは単独。仲間のセルは支えにならず、
/// プレイヤーの居るマスも支えにならない)。
fn is_unsupported_after_removal(
    game: &Game,
    removed: &[(usize, usize)],
    start: (usize, usize),
) -> bool {
    let depth_rows = game.board.depth_rows();
    let width = game.board.width();
    let start_cell = cell_after_removal(game, removed, start);

    let mut group = vec![start];
    let mut index = 0;
    while index < group.len() {
        let (r, c) = group[index];
        index += 1;
        for next in [
            (r.wrapping_sub(1), c),
            (r + 1, c),
            (r, c.wrapping_sub(1)),
            (r, c + 1),
        ] {
            if next.0 >= depth_rows || next.1 >= width || group.contains(&next) {
                continue;
            }
            if falls_together(start_cell, cell_after_removal(game, removed, next)) {
                group.push(next);
            }
        }
    }

    let player = game.player.position();
    !group.iter().any(|&(r, c)| {
        if r + 1 >= depth_rows {
            return true;
        }
        let below = (r + 1, c);
        if group.contains(&below) {
            return false; // 仲間は支えにならない
        }
        cell_after_removal(game, removed, below) != Cell::Empty && below != player
    })
}

/// 2つのセルが1つの塊として一緒に落ちるか。スター・ダイヤ・AIR・アイテムは連結対象外
/// なので、自分自身どうしでも`false`(常に単独の塊)。
fn falls_together(a: Cell, b: Cell) -> bool {
    match (a, b) {
        (Cell::Color(x), Cell::Color(y)) => x == y,
        (Cell::Rock { .. }, Cell::Rock { .. }) => true,
        _ => false,
    }
}

// --- 座標・入力の小道具 ------------------------------------------------------

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
    use crate::constants::{FIELD_WIDTH_DEFAULT, OXYGEN_WARNING_THRESHOLD};
    use crate::game::board::ColorKind;
    use crate::game::{Bomb, GameEvent, MissCause};

    /// 手詰まり判定で`escalation`が1段上がるまでに必要な`decide`の呼び出し回数。
    /// 初回は「前フレームと位置が違う」扱いでカウンタが初期化されるため、その1回と、
    /// カウンタが`AUTOPLAY_STUCK_FRAMES`を「超える」のに必要な1回を足す。
    fn stuck_frames_to_escalate() -> u32 {
        AUTOPLAY_STUCK_FRAMES + 2
    }

    /// テスト用ヘルパー: 盤面全体を`Cell::Empty`にクリアし、プレイヤーを指定位置へ置く。
    /// `Game::new`はランダム生成された盤面を持つため、テストが意図していない場所の
    /// 未支持ブロックが判断へ紛れ込まないよう必ずクリアしてから配置する。
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

    /// `game_at`に加えて直下へ足場を置き、横移動が通る(接地している)状態にする。
    /// 足場はダイヤブロックにする。連結しない種別なので、掘削で消える範囲が必ず
    /// 1マスに閉じ、足場のせいで崩落予測が反応することがない。
    fn grounded_game_at(seed: u64, row: usize, col: usize) -> Game {
        let mut game = game_at(seed, row, col);
        game.board.rows[row + 1][col] = Cell::Diamond;
        game
    }

    /// `decide`が返した入力のうち、横移動の向きだけを取り出す。
    fn lateral_of(actions: &[InputAction]) -> Option<Direction> {
        actions.iter().find_map(|a| match a {
            InputAction::MoveLeft => Some(Direction::Left),
            InputAction::MoveRight => Some(Direction::Right),
            _ => None,
        })
    }

    // --- 基本動作 -----------------------------------------------------------

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

    // --- 岩を割るか迂回するか(#221 R1) --------------------------------------

    #[test]
    fn decide_digs_sideways_around_a_rock_even_with_a_full_oxygen_tank() {
        // 岩1個は酸素20%+5ヒット分の時間で数十行ぶんの下降に相当する。横1列の迂回は
        // 1行ぶんにも満たないため、酸素が満タンでも迂回の方が常に安い(#221で方針変更。
        // 旧実装は酸素40%以上なら無条件で割っていた)。
        let mut game = game_at(5, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 };
        game.board.rows[500][6] = Cell::Color(ColorKind::Red); // 右へは掘って抜ける
        assert_eq!(game.player.oxygen, OXYGEN_MAX, "前提: 酸素は満タン");
        let mut pilot = Autopilot::new(false);

        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::DigSideways);
        assert_eq!(
            actions,
            vec![InputAction::MoveRight],
            "既定の優先方向(Right)へ、まずぶつかって向きを合わせるはず"
        );
    }

    #[test]
    fn decide_breaks_a_rock_when_every_reachable_column_is_blocked() {
        // 迂回先がどこも岩で塞がっているなら、代償を払ってでも割るしかない。
        let mut game = game_at(6, 500, 5);
        for col in 0..game.board.width() {
            game.board.rows[501][col] = Cell::Rock { hits: 0 };
        }
        let mut pilot = Autopilot::new(false);

        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::BreakRock);
        assert_eq!(actions, vec![InputAction::Drill]);
    }

    #[test]
    fn rock_cost_always_outweighs_a_detour_within_the_scan_radius() {
        // 定数を変えても「迂回できるなら迂回が勝つ」が保たれることを、判断の元になる
        // 2つの換算式そのもので確認する(浅い方が酸素の1%あたりの価値が高く、岩が
        // 相対的に高くつく。最深でも逆転しないことまで見る)。
        let shallow = game_at(7, 0, 5);
        let deep = game_at(7, 999, 5);
        for game in [&shallow, &deep] {
            let widest_detour = detour_cost_rows(game, AUTOPLAY_COLUMN_SCAN_RADIUS);
            assert!(
                rock_cost_rows(game) > widest_detour,
                "岩({})は候補範囲いっぱいの迂回({widest_detour})より高くつくはず",
                rock_cost_rows(game)
            );
            assert!(
                rock_cost_rows(game) > AUTOPLAY_LOOKAHEAD_ROWS as f32,
                "岩の代償は先読み範囲より大きく、他に手が無い時しか選ばれないはず"
            );
        }
    }

    // --- AIRの拾い方(#221 R2/R3) -------------------------------------------

    #[test]
    fn decide_steps_toward_a_nearby_air_capsule_while_oxygen_is_still_comfortable() {
        // 逼迫してから探すのでは間に合わないため、回復量に見合う限り平常時から寄る。
        let mut game = grounded_game_at(8, 500, 5);
        game.board.rows[503][2] = Cell::Oxygen; // 3列左
        game.player.oxygen = 85.0;
        assert!(
            game.player.oxygen > OXYGEN_WARNING_THRESHOLD,
            "前提: まだ警告域にも入っていない"
        );
        let mut pilot = Autopilot::new(false);

        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::SeekOxygen);
        assert_eq!(actions, vec![InputAction::MoveLeft]);
    }

    #[test]
    fn decide_ignores_an_air_capsule_when_the_tank_is_nearly_full() {
        // 実効回復量が`min(50, 100-残量)`で頭打ちになるため、満タン近くでは寄る価値が無い。
        let mut game = grounded_game_at(9, 500, 5);
        game.board.rows[503][2] = Cell::Oxygen;
        game.player.oxygen = 98.0;
        assert!(
            air_gain(game.player.oxygen) < AUTOPLAY_AIR_MIN_GAIN,
            "前提: 回復量が寄り道の最低ラインを下回っている"
        );
        let mut pilot = Autopilot::new(false);

        assert_eq!(pilot.decide_with_intent(&game).0, Intent::DigDown);
    }

    #[test]
    fn decide_ignores_an_air_capsule_that_is_too_far_sideways() {
        let mut game = grounded_game_at(10, 500, 5);
        game.board.rows[503][0] = Cell::Oxygen; // 5列左 = AUTOPLAY_AIR_DETOUR_MAX_COLS超
        game.player.oxygen = 85.0;
        const { assert!(5 > AUTOPLAY_AIR_DETOUR_MAX_COLS) } // 前提: 寄り道の上限を超えた距離
        let mut pilot = Autopilot::new(false);

        assert_eq!(pilot.decide_with_intent(&game).0, Intent::DigDown);
    }

    #[test]
    fn decide_seeks_a_far_air_capsule_once_oxygen_is_critical() {
        // 緊急時(残量 < 深度別のリザーブ)は寄り道の距離制限を外し、加点も3倍にする。
        let mut game = grounded_game_at(11, 500, 5);
        game.board.rows[503][0] = Cell::Oxygen; // 5列左
        game.player.oxygen = oxygen_reserve(game.player.depth_m()) - 1.0;
        assert!(is_emergency(&game), "前提: 緊急とみなされる残量");
        let mut pilot = Autopilot::new(false);

        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::SeekOxygen);
        assert_eq!(actions, vec![InputAction::MoveLeft]);
    }

    #[test]
    fn oxygen_reserve_grows_with_depth_and_matches_the_warning_threshold_at_the_bottom() {
        // 「何秒ぶんの自然減少を賄えるか」で決めるため、深いほど大きくなる。
        assert!(oxygen_reserve(0) < oxygen_reserve(500));
        assert!(oxygen_reserve(500) < oxygen_reserve(1000));
        assert!(
            (oxygen_reserve(1000) - OXYGEN_WARNING_THRESHOLD).abs() < 0.01,
            "最深では酸素警告の閾値と一致するはず: {}",
            oxygen_reserve(1000)
        );
    }

    // --- 列スコアのヒステリシス ---------------------------------------------

    #[test]
    fn target_column_only_switches_when_the_gain_clears_the_hysteresis_margin() {
        let mut game = grounded_game_at(12, 500, 5);
        game.player.oxygen = 85.0;
        game.board.rows[503][3] = Cell::Oxygen; // 2列左
        let mut pilot = Autopilot::new(false);

        pilot.decide(&game);
        assert_eq!(pilot.target_col, Some(3), "まず左のAIRを目的列にする");

        // 1列右にもAIRが現れる。距離が1つ近いぶん僅かに良いが、差はマージン未満。
        game.board.rows[503][6] = Cell::Oxygen;
        let actions = pilot.decide(&game);
        assert_eq!(pilot.target_col, Some(3), "僅差では乗り換えないはず");
        assert_eq!(actions, vec![InputAction::MoveLeft]);

        // 右にもう1つ増えてマージンを超える差がついたら乗り換える。
        game.board.rows[504][6] = Cell::Oxygen;
        let actions = pilot.decide(&game);
        assert_eq!(pilot.target_col, Some(6), "明確に良くなったら乗り換える");
        assert_eq!(actions, vec![InputAction::MoveRight]);
    }

    #[test]
    fn the_pilot_does_not_oscillate_between_an_air_detour_and_a_hazard_on_the_same_side() {
        // 実測で確認した振動(AIRへ寄る→危険で戻る→またAIRへ寄る)が起きないこと。
        // 左にAIR、同じ左隣の列の頭上に落下予定の塊を置く。
        let mut game = grounded_game_at(13, 500, 5);
        game.player.oxygen = 60.0;
        game.board.rows[503][3] = Cell::Oxygen;
        game.board.rows[499][4] = Cell::Color(ColorKind::Blue); // 左隣の頭上、支えなし
        assert!(game.is_cell_unstable(499, 4), "前提: 支えのない塊は不安定");
        let mut pilot = Autopilot::new(false);

        let mut directions = Vec::new();
        for _ in 0..30 {
            if let Some(dir) = lateral_of(&pilot.decide(&game)) {
                directions.push(dir);
            }
        }
        assert!(
            directions.windows(2).all(|w| w[0] == w[1]),
            "30フレーム回しても左右が入れ替わらないはず: {directions:?}"
        );
    }

    // --- 押し潰しの回避(#221の本丸) ----------------------------------------

    #[test]
    fn column_threat_stops_at_a_supported_block_and_reports_nothing() {
        // 支えが残るブロックは遮蔽物。その上に何があっても落ちてこない。
        // ここでは「移った先で自分が占めるマスより上にある土台」が支えなので、
        // 移っても支えは消えない。
        let mut game = game_at(14, 500, 5);
        game.board.rows[499][6] = Cell::Oxygen; // 土台(AIRは支えになる)
        game.board.rows[498][6] = Cell::Color(ColorKind::Blue); // 土台に乗っている
        let pilot = Autopilot::new(false);

        assert_eq!(pilot.column_threat(&game, 6, 500), None);
    }

    #[test]
    fn column_threat_sees_the_block_that_the_air_capsule_under_it_stops_holding_up() {
        // AIRの上に乗ったブロックは、そのAIRを取った瞬間に支えを失って落ちてくる。
        // 移る前の盤面だけを見ると「支えられている」ので脅威に見えない。
        let mut game = game_at(14, 500, 5);
        game.board.rows[500][6] = Cell::Oxygen; // 移った先で取得し、消えるマス
        game.board.rows[499][6] = Cell::Color(ColorKind::Blue);
        let pilot = Autopilot::new(false);

        assert!(
            !game.is_cell_unstable(499, 6),
            "前提: 今はAIRに支えられていて安定している"
        );
        assert_eq!(
            pilot.column_threat(&game, 6, 500),
            Some(ColumnThreat {
                dist: 1,
                shaking: false
            }),
            "AIRを取れば支えが消えるので、移る前から脅威として見えるはず"
        );
    }

    #[test]
    fn column_threat_sees_an_unstable_block_at_the_far_edge_of_the_scan_range() {
        let mut game = game_at(15, 500, 5);
        game.board.rows[499][6] = Cell::Oxygen; // 頭上を空洞のままにしない土台
        let pilot = Autopilot::new(false);

        // 走査範囲の端(14行上)に浮いた塊があれば見つける。
        let edge_row = 500 - AUTOPLAY_THREAT_SCAN_ROWS;
        game.board.rows[edge_row][6] = Cell::Color(ColorKind::Blue);
        assert_eq!(
            pilot.column_threat(&game, 6, 500),
            Some(ColumnThreat {
                dist: AUTOPLAY_THREAT_SCAN_ROWS,
                shaking: false
            })
        );

        // 1行でも外なら見ない(遠すぎる脅威に反応すると前へ進めなくなる)。
        game.board.rows[edge_row][6] = Cell::Empty;
        game.board.rows[edge_row - 1][6] = Cell::Color(ColorKind::Blue);
        assert_eq!(pilot.column_threat(&game, 6, 500), None);
    }

    #[test]
    fn decide_dodges_sideways_when_a_falling_block_arrives_before_the_rock_below_breaks() {
        // 直下が岩(5ヒット=400ms)だと、2行上の塊(2ティック=約210ms)に追いつかれる。
        let mut game = game_at(16, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 };
        game.board.rows[498][5] = Cell::Color(ColorKind::Blue); // 2行上、支えなし
        let mut pilot = Autopilot::new(false);

        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::DodgeOverhead);
        assert_eq!(actions, vec![InputAction::MoveRight]);
    }

    #[test]
    fn decide_keeps_digging_when_the_overhead_block_can_be_outrun() {
        // 直下が色ブロック(1ヒット)なら掘り抜くのは速いので、4行上の塊からは掘り
        // 進んで振り切れる。逃げる方が遅いので掘り続けるのが正しい。
        let mut game = game_at(17, 500, 5);
        // 同色を縦に並べると連結グループごと消えて大穴が開くため、色を交互にする
        // (spec.md 3.3の同色連続上限に沿った、実際に生成されうる地形にする)。
        for row in 501..=515 {
            game.board.rows[row][5] = if row % 2 == 0 {
                Cell::Color(ColorKind::Red)
            } else {
                Cell::Color(ColorKind::Green)
            };
        }
        game.board.rows[496][5] = Cell::Color(ColorKind::Blue); // 4行上、支えなし
        // 左右を岩で塞ぎ、「逃げるか掘るか」だけの選択にする。
        game.board.rows[500][4] = Cell::Rock { hits: 0 };
        game.board.rows[500][6] = Cell::Rock { hits: 0 };
        let pilot_view = Autopilot::new(false);
        assert!(
            pilot_view.column_slack_rows(&game, 5) >= AUTOPLAY_THREAT_MIN_SLACK_ROWS,
            "前提: 掘り進めば振り切れるだけの余裕がある"
        );

        let mut pilot = Autopilot::new(false);
        assert_eq!(pilot.decide_with_intent(&game).0, Intent::DigDown);
    }

    #[test]
    fn decide_refuses_to_drop_into_a_shaft_with_a_block_overhead() {
        // 落下が始まると着地まで横移動が効かない。ブロックの落下tickは深度で短くなる
        // のにプレイヤーの自由落下tickは一定なので、深い場所で頭上に塊を抱えたまま
        // 空洞へ飛び込むと、着地と同時に潰される(実測した死因の最多パターン)。
        let mut game = game_at(17, 500, 5);
        game.board.rows[501][5] = Cell::Color(ColorKind::Red); // 掘れば下は空洞
        game.board.rows[498][5] = Cell::Color(ColorKind::Blue); // 2行上、支えなし
        let pilot_view = Autopilot::new(false);
        assert!(
            pilot_view.column_slack_rows(&game, 5) < AUTOPLAY_THREAT_MIN_SLACK_ROWS,
            "前提: 掘った先の落下まで含めると余裕が残らない"
        );

        let mut pilot = Autopilot::new(false);
        assert_eq!(pilot.decide_with_intent(&game).0, Intent::DodgeOverhead);
    }

    #[test]
    fn decide_dodges_a_threat_more_than_three_rows_up_when_falls_are_fast() {
        // 旧実装は頭上3行しか見ておらず、深い場所(落下が最大2.5倍速)では6行上の塊にも
        // 追いつかれて潰されていた。時間で比べるようにしたため距離では切らない。
        let mut game = game_at(18, 900, 5);
        game.board.rows[901][5] = Cell::Rock { hits: 0 };
        game.board.rows[894][5] = Cell::Color(ColorKind::Blue); // 6行上、支えなし
        let pilot_view = Autopilot::new(false);
        let threat = pilot_view
            .column_threat(&game, 5, 900)
            .expect("6行上の浮いた塊は脅威として見つかるはず");
        assert_eq!(threat.dist, 6);

        let mut pilot = Autopilot::new(false);
        assert_eq!(pilot.decide_with_intent(&game).0, Intent::DodgeOverhead);
    }

    #[test]
    fn decide_does_not_step_into_a_column_that_is_about_to_be_crushed() {
        // 逃げ込んだ先で潰されては意味が無い。爆風から列を変えて逃げる場面で、
        // 頭上に浮いた塊のある側は選ばない。
        let mut game = game_at(19, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 足場
        game.board.rows[499][6] = Cell::Color(ColorKind::Blue); // 右隣の頭上、支えなし
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
        assert_eq!(
            actions,
            vec![InputAction::MoveLeft],
            "右は頭上が危険なので逆側へ逃げるはず"
        );
    }

    // --- 崩落予測 -----------------------------------------------------------

    #[test]
    fn decide_avoids_the_side_whose_support_it_would_remove_by_digging() {
        // 右隣を掘ると、その上に乗っている塊が支えを失ってそのまま落ちてくる。
        // 掘る前の盤面では支えられているので、消えるセルを織り込んで初めて分かる。
        let mut game = game_at(20, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 直下は岩なので迂回したい
        game.board.rows[500][6] = Cell::Color(ColorKind::Red); // 右の踏み台兼支え
        game.board.rows[499][6] = Cell::Color(ColorKind::Blue); // その上に乗る塊
        game.board.rows[500][4] = Cell::Color(ColorKind::Green); // 左は掘っても崩れない
        game.board.rows[499][4] = Cell::Empty;
        let pilot_view = Autopilot::new(false);
        assert!(
            !game.is_cell_unstable(499, 6),
            "前提: 掘る前は支えられていて、そのままでは脅威に見えない"
        );
        assert_eq!(
            pilot_view.column_threat(&game, 6, 500),
            Some(ColumnThreat {
                dist: 1,
                shaking: false
            }),
            "掘れば支えが消えることを織り込んで脅威として見えるはず"
        );

        let mut pilot = Autopilot::new(false);
        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::DigSideways);
        assert_eq!(
            actions,
            vec![InputAction::MoveLeft],
            "優先方向(Right)より、崩落を招かない左を選ぶはず"
        );
    }

    // --- 横掘りの手順 -------------------------------------------------------

    #[test]
    fn digging_sideways_never_emits_a_move_and_a_drill_in_the_same_frame() {
        // 移動クールダウン中は横移動処理がfacingを変えずに抜けるため、同じフレームに
        // 両方出すと真下を掘ってしまう。必ず「ぶつける→掘る→動く」の3フレームに分ける。
        let mut game = game_at(22, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 };
        game.board.rows[500][4] = Cell::Rock { hits: 0 }; // 左は塞いで右へ誘導する
        game.board.rows[500][6] = Cell::Color(ColorKind::Red);
        let mut pilot = Autopilot::new(false);

        assert_eq!(
            pilot.decide(&game),
            vec![InputAction::MoveRight],
            "1フレーム目: 向きを合わせるためにぶつかるだけ"
        );

        game.player.facing = Direction::Right;
        assert_eq!(
            pilot.decide(&game),
            vec![InputAction::Drill],
            "2フレーム目: 向きが合ったので掘る"
        );

        game.board.rows[500][6] = Cell::Empty;
        assert_eq!(
            pilot.decide(&game),
            vec![InputAction::MoveRight],
            "3フレーム目: 空いたので移動する"
        );
    }

    // --- ボム -------------------------------------------------------------

    #[test]
    fn decide_drills_the_adjacent_bomb_below() {
        let mut game = game_at(23, 500, 5);
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
        let mut game = game_at(24, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 };
        game.bombs_mut().push(Bomb {
            pos: (500, 6),
            origin: (500, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: 4000,
            settle_bounce_dir: 1,
        });
        let mut pilot = Autopilot::new(false);

        assert_eq!(pilot.decide(&game), vec![InputAction::MoveRight]);

        game.player.facing = Direction::Right;
        assert_eq!(pilot.decide(&game), vec![InputAction::Drill]);
    }

    #[test]
    fn decide_ignores_bombs_that_are_still_entering_or_rolling() {
        let mut game = game_at(25, 500, 5);
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
        let mut game = game_at(26, 500, 5);
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
        let mut game = game_at(27, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 };
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
        let mut game = game_at(28, 500, 5);
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

    // --- 手詰まりの段階(escalation) -----------------------------------------

    /// 完全な行き止まり(直下・両隣が岩、頭上だけ空き)の盤面。
    fn dead_end_game(seed: u64) -> Game {
        let mut game = game_at(seed, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 };
        game.board.rows[500][4] = Cell::Rock { hits: 0 };
        game.board.rows[500][6] = Cell::Rock { hits: 0 };
        game
    }

    #[test]
    fn escalation_rises_while_the_row_does_not_advance_and_resets_once_it_does() {
        let game = dead_end_game(29);
        let mut pilot = Autopilot::new(false);

        for _ in 0..stuck_frames_to_escalate() {
            pilot.decide(&game);
        }
        assert_eq!(pilot.escalation, 1, "前へ進めていないので1段上がるはず");

        // 行が進んだ(=本当の前進)ときだけ0へ戻る。横に動いただけでは戻さない
        // (戻すと「同じ行を横に往復し続ける」手詰まりを永久に検出できない)。
        let mut moved = dead_end_game(29);
        moved.player.row = 501;
        pilot.decide(&moved);
        assert_eq!(pilot.escalation, 0);
    }

    #[test]
    fn escalation_flips_the_preferred_side_exactly_once_and_then_keeps_it() {
        let game = dead_end_game(30);
        let mut pilot = Autopilot::new(false);
        assert_eq!(pilot.side_preference, Direction::Right, "既定は右");

        for _ in 0..stuck_frames_to_escalate() {
            pilot.decide(&game);
        }
        assert_eq!(pilot.escalation, 1);
        assert_eq!(pilot.side_preference, Direction::Left, "1回だけ反転する");

        // 段数がさらに上がっても向きは変えない。毎フレーム逆側を返す実装だと
        // 段差登り(同じ方向へ2回ぶつかる必要がある)が永久に成立しない。
        for _ in 0..stuck_frames_to_escalate() * 2 {
            pilot.decide(&game);
            assert_eq!(pilot.side_preference, Direction::Left);
        }
        assert_eq!(pilot.escalation, 3);
    }

    #[test]
    fn escalation_two_opens_up_rock_as_a_last_resort_route() {
        let game = dead_end_game(31);
        let mut pilot = Autopilot::new(false);

        for _ in 0..stuck_frames_to_escalate() * 2 {
            pilot.decide(&game);
        }
        assert_eq!(pilot.escalation, 2);
        // 反転後の優先方向(左)の岩を割って抜ける経路が選べるようになる。
        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::BreakRock);
        assert_eq!(actions, vec![InputAction::MoveLeft]);
    }

    #[test]
    fn escalation_three_climbs_a_step_to_escape_a_dead_end() {
        let game = dead_end_game(32);
        let mut pilot = Autopilot::new(false);

        for _ in 0..stuck_frames_to_escalate() * 3 {
            pilot.decide(&game);
        }
        assert_eq!(pilot.escalation, 3);

        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::EscapeClimb);
        assert_eq!(actions, vec![InputAction::MoveLeft]);

        // 段差登りは「同じ方向へ2回ぶつかる」ことで成立するため、抜け出せない限り
        // 同じ方向を出し続けなければならない。左右が交互に出ると永久に登れない。
        for frame in 0..10 {
            assert_eq!(
                pilot.decide(&game),
                actions,
                "frame={frame}: 登り切るまで同じ方向を出し続けるはず"
            );
        }
    }

    #[test]
    fn escalation_rises_when_the_row_stops_advancing_even_though_the_player_keeps_moving() {
        // 横移動が自由になると位置は変わり続けるため、位置ベースの停滞検知だけでは
        // 「同じ行を横に往復し続ける」手詰まりを見逃す。
        let mut pilot = Autopilot::new(false);
        for frame in 0..=AUTOPLAY_DESCENT_WATCHDOG_FRAMES + 1 {
            pilot.note_progress((500, 5 + usize::from(frame % 2 == 0)));
            assert_eq!(
                pilot.frames_without_progress, 0,
                "frame={frame}: 位置は毎フレーム変わっている(位置ベースでは検出できない)"
            );
        }
        assert_eq!(pilot.escalation, 1, "行が進まなければ段階が上がるはず");
    }

    // --- 再現性 -------------------------------------------------------------

    #[test]
    fn decide_is_deterministic_for_the_same_board_and_internal_state() {
        // 乱数を一切使わず走査順も固定のため、同じ盤面・同じ内部状態からは必ず同じ
        // 入力列が出る(ソークテストの再現性の担保)。
        let mut game = grounded_game_at(33, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 };
        game.board.rows[503][3] = Cell::Oxygen;
        game.board.rows[498][6] = Cell::Color(ColorKind::Blue);
        game.player.oxygen = 45.0;

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

    /// 1回の自動プレイの結果。
    struct SoakResult {
        cleared: bool,
        frames: u32,
        deepest_m: usize,
        /// 掘削で壊した岩の数(酸素20%を払った回数。着地での自動消滅は含まない)。
        rocks_drilled: usize,
        /// ライフを失った/ゲームオーバーになった原因の一覧。
        deaths: Vec<MissCause>,
        /// 発生イベントの並び(再現性の検証用)。
        event_log: Vec<(u32, GameEvent)>,
    }

    impl SoakResult {
        fn unharmed(&self) -> bool {
            self.cleared && self.deaths.is_empty()
        }
    }

    /// 無敵OFFのオートプレイで1本通しプレイし、結果を集計する。main.rsのメインループと
    /// 同じ順序(判断→入力→update)で回す。
    fn play(seed: u64, depth_goal_m: usize, width: usize, max_frames: u32) -> SoakResult {
        let mut game = Game::new_with_width(seed, width, depth_goal_m);
        let mut pilot = Autopilot::new(false);
        let delta = std::time::Duration::from_millis(FRAME_INTERVAL_MS);
        let mut result = SoakResult {
            cleared: false,
            frames: 0,
            deepest_m: 0,
            rocks_drilled: 0,
            deaths: Vec::new(),
            event_log: Vec::new(),
        };

        for frame in 0..max_frames {
            result.frames = frame + 1;
            for action in pilot.decide(&game) {
                let events = match action {
                    InputAction::Confirm => {
                        game.revive();
                        Vec::new()
                    }
                    other => game.apply_input(other),
                };
                for event in &events {
                    // 掘削で壊した岩だけを数える。着地での4連結自動消滅はupdate側で
                    // 起きるため、ここには混ざらない。
                    if let GameEvent::RockDestroyed { blocks } = event {
                        result.rocks_drilled += blocks;
                    }
                }
                record(&mut result, frame, &events);
            }
            let events = game.update(delta);
            record(&mut result, frame, &events);

            result.deepest_m = result.deepest_m.max(game.player.depth_m());
            assert!(
                matches!(
                    game.status,
                    GameStatus::Playing | GameStatus::Cleared | GameStatus::GameOver
                ),
                "seed={seed} frame={frame}: 想定外の状態 {:?}",
                game.status
            );
            if game.status == GameStatus::Cleared {
                result.cleared = true;
                break;
            }
        }
        result
    }

    fn record(result: &mut SoakResult, frame: u32, events: &[GameEvent]) {
        for event in events {
            match event {
                GameEvent::LifeLost { cause } | GameEvent::GameOverMiss { cause } => {
                    result.deaths.push(*cause);
                }
                _ => {}
            }
            result.event_log.push((frame, *event));
        }
    }

    /// 幅12・300mを無敵OFFで完走できること(T1)。通常のテスト実行に含める軽量版。
    /// 多シードでの品質(`soak_short_course_quality`)とフルコース
    /// (`soak_full_course_without_invincibility`)は#[ignore]付き。
    #[test]
    fn soak_reaches_the_goal_without_invincibility_on_a_short_course() {
        for seed in [1, 2] {
            let result = play(seed, 300, FIELD_WIDTH_DEFAULT, 30_000);
            assert!(
                result.cleared,
                "seed={seed}: 無敵なしでゴールまで到達できるはず(到達={}m, {}フレーム)",
                result.deepest_m, result.frames
            );
            assert!(
                result.deaths.is_empty(),
                "seed={seed}: 一度も死なずに完走できるはず: {:?}",
                result.deaths
            );
        }
    }

    /// 同じシード・同じ入力列なら、盤面生成もボム抽選もアイテム補充も完全に再現される
    /// こと(T4)。`Board`の再抽選が呼び出しごとにOS乱数から作り直していた頃は、同じ
    /// シードでも結果が揺れてソークテストの失敗を再現できなかった(#221で修正)。
    #[test]
    fn replaying_the_same_seed_reproduces_the_run_exactly() {
        let first = play(4242, 300, FIELD_WIDTH_DEFAULT, 30_000);
        let second = play(4242, 300, FIELD_WIDTH_DEFAULT, 30_000);

        assert_eq!(first.frames, second.frames);
        assert_eq!(first.cleared, second.cleared);
        assert_eq!(first.deepest_m, second.deepest_m);
        assert_eq!(first.rocks_drilled, second.rocks_drilled);
        assert_eq!(first.event_log, second.event_log);
    }

    /// 無敵ONで長時間回し、どんな組み合わせでも盤面処理が破綻しないことを確認する。
    /// 生存性ではなくクラッシュ・不変条件違反の検出が目的。
    #[test]
    fn soak_survives_a_few_thousand_frames_with_invincibility() {
        let mut game = Game::new_with_width(918, FIELD_WIDTH_DEFAULT, 1000);
        game.set_invincible(true);
        let mut pilot = Autopilot::new(false);
        let lives_at_start = game.player.lives;
        let delta = std::time::Duration::from_millis(FRAME_INTERVAL_MS);

        for frame in 0..1500 {
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
                "frame={frame}: 無敵中はライフが減らないはず(lives={})",
                game.player.lives
            );
            assert!(
                matches!(game.status, GameStatus::Playing | GameStatus::Cleared),
                "frame={frame}: PlayingかClearedのはずだが{:?}だった",
                game.status
            );
        }
    }

    /// 300m・幅12を無敵OFFで16シード走らせ、設計が置いた品質基準を満たすことを確認する
    /// (到達率9割・酸素切れゼロ・無傷完走5割・完走時の岩破壊15回以下)。
    ///
    /// 実測値(#221時点)は 到達16/16・無傷完走11/16・酸素切れ0・平均岩破壊3.1回・
    /// 平均1,457フレーム(自由落下の理論下限1,364フレームの107%)。
    /// 同じ基準をフルコースに当てると届かない(理由は
    /// `soak_full_course_without_invincibility`のコメント)。
    #[test]
    #[ignore = "長時間のソークテスト。cargo test --release -- --ignored で実行する"]
    fn soak_short_course_quality() {
        const SEEDS: u64 = 16;
        let results: Vec<(u64, SoakResult)> = (0..SEEDS)
            .map(|seed| (seed, play(seed, 300, FIELD_WIDTH_DEFAULT, 30_000)))
            .collect();
        let summary = Summary::of(&results);
        summary.print("300m", SEEDS);

        assert!(
            summary.out_of_oxygen.is_empty(),
            "酸素切れで死んだシードがある: {:?}",
            summary.out_of_oxygen
        );
        assert!(
            summary.cleared * 10 >= (SEEDS as usize) * 9,
            "ゴール到達が9割に届いていない: {}/{SEEDS}",
            summary.cleared
        );
        assert!(
            summary.unharmed * 2 >= SEEDS as usize,
            "無傷完走が5割に届いていない: {}/{SEEDS}",
            summary.unharmed
        );
        assert!(
            summary.average_rocks <= 15.0,
            "完走時の岩破壊が多すぎる(平均{:.1}回)",
            summary.average_rocks
        );
    }

    /// フルコース(1000m・幅12)を無敵OFFで32シード走らせる重量級のソーク(T2)。
    ///
    /// 深い場所は浅い場所と質が違う。ブロックの落下tickは深度で最大2.5倍まで短くなるのに
    /// プレイヤーの自由落下tickは一定なので、最深帯ではブロックの方が2.5倍速く落ちる。
    /// 落下中は横移動が効かないため、空洞へ入った時点で頭上の塊に追いつかれる状況が
    /// 構造的に発生する。岩の出現率・酸素の減少速度も深いほど上がる。
    ///
    /// そのためここでの合格ラインは「ゴールへ到達し続けること」と、改善の実測値からの
    /// 明確な後退を検出することに置く。設計が置いた品質基準(酸素切れゼロ・無傷完走5割・
    /// 岩破壊15回以下)は300mでは満たすがフルコースでは届いておらず、
    /// `soak_short_course_quality`が担当する。
    ///
    /// 実測値(#221時点、32シード): 到達32/32・無傷完走0/32・1走あたり押し潰され4.2回/
    /// 酸素切れ1.8回/爆風0.3回・完走時の平均岩破壊36.9回・平均5,589フレーム
    /// (自由落下の理論下限4,545フレームの123%)。#218時点は無敵OFFでは1本も完走できず
    /// (8シードすべて535〜789mでゲームオーバー)、岩は1走あたり150〜200個割っていた。
    #[test]
    #[ignore = "長時間のソークテスト。cargo test --release -- --ignored で実行する"]
    fn soak_full_course_without_invincibility() {
        const SEEDS: u64 = 32;
        let results: Vec<(u64, SoakResult)> = (0..SEEDS)
            .map(|seed| (seed, play(seed, 1000, FIELD_WIDTH_DEFAULT, 120_000)))
            .collect();
        let summary = Summary::of(&results);
        summary.print("1000m", SEEDS);

        assert!(
            summary.cleared * 10 >= (SEEDS as usize) * 9,
            "ゴール到達が9割に届いていない: {}/{SEEDS}",
            summary.cleared
        );
        assert!(
            summary.average_rocks <= 55.0,
            "完走時の岩破壊が実測(36.9回)から大きく増えている: 平均{:.1}回",
            summary.average_rocks
        );
        let deaths_per_run = summary.total_deaths as f64 / SEEDS as f64;
        assert!(
            deaths_per_run <= 9.5,
            "1走あたりの死亡回数が実測(6.4回)から大きく増えている: {deaths_per_run:.1}回"
        );
    }

    /// ソーク結果の集計。
    struct Summary {
        cleared: usize,
        unharmed: usize,
        out_of_oxygen: Vec<u64>,
        average_rocks: f64,
        average_frames: f64,
        total_deaths: usize,
    }

    impl Summary {
        fn of(results: &[(u64, SoakResult)]) -> Self {
            let rocks: Vec<usize> = results
                .iter()
                .filter(|(_, r)| r.cleared)
                .map(|(_, r)| r.rocks_drilled)
                .collect();
            Summary {
                cleared: results.iter().filter(|(_, r)| r.cleared).count(),
                unharmed: results.iter().filter(|(_, r)| r.unharmed()).count(),
                out_of_oxygen: results
                    .iter()
                    .filter(|(_, r)| r.deaths.contains(&MissCause::OxygenOut))
                    .map(|(seed, _)| *seed)
                    .collect(),
                average_rocks: rocks.iter().sum::<usize>() as f64 / rocks.len().max(1) as f64,
                average_frames: results.iter().map(|(_, r)| r.frames as f64).sum::<f64>()
                    / results.len().max(1) as f64,
                total_deaths: results.iter().map(|(_, r)| r.deaths.len()).sum(),
            }
        }

        fn print(&self, course: &str, seeds: u64) {
            println!(
                "{course}: 到達 {}/{seeds} / 無傷完走 {}/{seeds} / 酸素切れ {}シード / \
                 完走時の平均岩破壊 {:.1}回 / 平均 {:.0}フレーム / 死亡 計{}回",
                self.cleared,
                self.unharmed,
                self.out_of_oxygen.len(),
                self.average_rocks,
                self.average_frames,
                self.total_deaths
            );
        }
    }
}

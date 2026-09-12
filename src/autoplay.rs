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
//! # 酸素切れ死への対策(#225)
//!
//! 通常のライフ予算(自動Revive無し)で測り直したところ、ユーザーの実設定(`Profile::harsh`)
//! では完走2/32・死因の95%が酸素切れだった。効いた修正は次の4つ。
//!
//! 1. **揺れ猶予の見積り誤りを直した**(最大の要因)。`column_threat`が返す脅威には
//!    「既に落ちてくる塊」と「こちらがその列へ掘り込んで初めて支えを失う塊」の2種類が
//!    あるのに、後者にも猶予0を与えていた。実際には支えを失ってから`shake_duration_ms`
//!    ぶん揺れてからでないと落ちてこないため、横へ掘り抜ける列がほぼ全て「入った瞬間に
//!    潰される」と誤判定され、迂回路が候補から消えて岩を割るしかなくなっていた
//!    (`shake_allowance_ms`の`pending`)
//! 2. **岩を割る前に酸素が払えるか見る**。緊急判定は残量が減ってから反応するので、判定に
//!    入る前に岩1個で20%持っていかれると手遅れになる。払えないなら`conserve_oxygen`
//!    (迂回→着地を待つ→段差登り)へ逃がす(`rock_is_affordable`)
//! 3. **経路上の岩を「禁止」から「有料」にした**。「横に1個割れば直進できる列」が候補に
//!    すら上がらなかったため、`lateral_rock_count`ぶんの減点として縦の岩と同じ土俵に載せる
//! 4. **岩で買った前進を停滞判定の「前進」に数えない**。数えると岩を割るたびに段階が0へ
//!    戻り、横方向の岩掘りが解禁される段階へ永久に到達しなかった
//!    (`AUTOPLAY_ROCK_STREAK_ESCALATE`)
//!
//! 逆に**採らなかった**案も実測で判断している。頭上の塊に対する必要余裕
//! (`min_slack_rows`)を割り込む列を「却下せず減点で残す」・隣接列への移動時間見積りから
//! 安全マージンを外す、のいずれも酸素切れは減るが、減ったぶんがそのまま押し潰し死に
//! 変わった(1走あたり1.66回→2.47〜4.59回)。安全側の基準は緩めず、見積りの誤りだけを
//! 直すのが正解だった。
//!
//! # 判断そのものの計測と、そこで見つけたバグ(#229)
//!
//! 完走率と死因の内訳だけでは「なぜそう動いたのか」が見えない。ソークに判断の計測を
//! 足し(テストの`Telemetry`)、意図別のフレーム数・段差登りの空振り・待ちの累積・
//! AIRとアイテムの取得/通過とそのときの見え方(`PassState`)を記録するようにした。
//! 「見えていたのに取らなかった」の原因を、落下コミット(横移動が効かない)・列の
//! 却下・同一行の死角のどれかに切り分けられる。
//!
//! これで挙動のバグが2つ見つかり、どちらも直している。
//!
//! 1. **登れない壁へ段差登りを出し続けていた**(`escape_climb`)。登り先の棚を確かめずに
//!    `side_preference`側へ`MoveX`を出していたため、登れない壁に対して最長510フレーム
//!    (17秒)ぶつかり続け、その間の自然減少だけで窒息していた。棚がある方向にだけ
//!    出すようにしたところ、harsh設定の完走が25/32→31/32・死亡が2.12→1.88回/走
//! 2. **待ち予算が事実上無効だった**(`note_decision`)。`WaitOut`以外の意図が1フレーム
//!    挟まるだけで`waiting_frames`を0へ戻していたため、`conserve_oxygen`の
//!    「待つ→予算切れ→別の手→また待つ」が無限に繰り返せていた(実測で495フレーム=
//!    16秒の待ち)。行が進んだときだけ戻すようにし、あわせて予算切れの後は縦に岩を
//!    割ると決める前に横の岩も同じ土俵で比べるようにした(1000mの完走10→11/32・
//!    平均到達886→893m・死亡5.09→5.03回/走)
//!
//! 逆に、設計案のうち次の3つは実測で**採らなかった**。いずれも死亡総数か到達深度が
//! 悪化した(32シードずつ、基準は完走11/32・平均893m・5.03回/走の1000mコース)。
//!
//! - **段差登りを押し潰し回避の選択肢に足す**: 完走5/32・平均868m。押し潰されは
//!   1.81→1.66回/走へ減るが、登ると1行戻るぶん深度が伸びず酸素切れが増える。
//!   「両隣が固体で歩いて出られない場面だけ登る」と絞っても結果は同じだった。
//!   ボムから逃げる場面だけは岩1個ぶん(酸素20%)を節約できるため残してある
//! - **同一行・斜め上のAIR/アイテムも採点に入れる**: 完走7/32・平均887m・5.22回/走。
//!   AIRの取得数は2213→2280個とほぼ変わらず、寄り道の手間だけが増えた
//! - **頭上クリア(R)の加点を頭上の脅威の数に連動させる**: 完走5/32・平均869m。
//!   取得率が35%→20%へ落ち、酸素切れが2.94→3.34回/走へ増えた。頭上クリアは
//!   「今見えている脅威」より先の掘り進みやすさに効いているらしく、今の盤面から
//!   価値を測る方法が見つかっていない。スター化(K)の岩連動だけを採用している
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
//! 到達度は`soak_short_course_within_life_budget`(300m)・
//! `soak_full_course_within_life_budget`(1000m)・
//! `soak_harsh_profile_within_life_budget`(ユーザーの実設定)に実測値付きで記録している。
//! いずれも通常のライフ予算内(`RevivePolicy::Never`)で測る。
//!
//! 無敵(`Game::set_invincible`)とは独立したトグルで、Tキーはオートプレイだけを
//! 切り替える(#221。無敵はGキーが単独で管理する)。無人で回り続けるアトラクト
//! モードだけは安全策として無敵も併用する。GameOverになったら何も返さず、人間が
//! プレイしたときと同じようにダイアログを出したまま操作を待つ(#225)。

use crate::constants::{
    AUTOPLAY_AIR_DETOUR_MAX_COLS, AUTOPLAY_AIR_MIN_GAIN, AUTOPLAY_BOMB_EVADE_MS,
    AUTOPLAY_COLUMN_SCAN_RADIUS, AUTOPLAY_COLUMN_SWITCH_MARGIN, AUTOPLAY_DESCENT_WATCHDOG_FRAMES,
    AUTOPLAY_EMERGENCY_AIR_LOOKAHEAD_ROWS, AUTOPLAY_EMERGENCY_AIR_SCORE_MULTIPLIER,
    AUTOPLAY_EMERGENCY_HORIZON_SEC, AUTOPLAY_ITEM_STARIFY_ROCK_SATURATION, AUTOPLAY_LOOKAHEAD_ROWS,
    AUTOPLAY_ROCK_STREAK_ESCALATE, AUTOPLAY_SCORE_AIR_DIVISOR, AUTOPLAY_SCORE_ITEM_BONUS,
    AUTOPLAY_SCORE_LATERAL_PER_COL, AUTOPLAY_SCORE_THREAT_PENALTY, AUTOPLAY_SCORE_VOID_EXPOSURE,
    AUTOPLAY_STUCK_FRAMES, AUTOPLAY_THREAT_MAX_SLACK_ROWS, AUTOPLAY_THREAT_MIN_SLACK_ROWS,
    AUTOPLAY_THREAT_REACTION_STEPS, AUTOPLAY_THREAT_SCAN_ROWS, AUTOPLAY_WAIT_FOR_THREAT_MAX_MS,
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
    /// 岩を割って買った前進が何行ぶん続いているか(#225)。停滞ベースの判定は岩で1行
    /// 進んだだけでも「前進」とみなして`escalation`を0へ戻すため、これが無いと横方向の
    /// 岩掘りが解禁される段階(2)へ実質到達できない。
    rock_bought_rows: u8,
    /// 直前に`BreakRock`を選んでおり、その行前進がまだ計上されていないか(#225)。
    /// 岩が砕けた次のフレームは自由落下待ち(`Idle`)になるため、「前フレームの意図」だけ
    /// 見ても岩で買った前進を取りこぼす。
    rock_break_pending: bool,
    /// 酸素温存のためその場で待ったフレーム数(#225)。待ち続けて酸素切れになるのを
    /// 防ぐため、`AUTOPLAY_WAIT_FOR_THREAT_MAX_MS`ぶんで打ち切る。
    waiting_frames: u32,
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
    /// 危険を避ける・AIRやアイテムを取るために段差を登る(TERM独自拡張。#229)
    ClimbOver,
    /// 岩を割る酸素が無いので、頭上の塊が着地して道が空くのをその場で待つ
    WaitOut,
}

#[cfg(test)]
impl Intent {
    /// 意図の種類数。ソークの意図別フレーム集計(#229)の配列長。
    const COUNT: usize = 12;

    /// 集計配列の添字。網羅的なmatchにしてあるため、意図を増やすと`COUNT`の更新漏れが
    /// コンパイルエラーになる。
    fn index(self) -> usize {
        match self {
            Intent::Idle => 0,
            Intent::DefuseBomb => 1,
            Intent::EvadeBomb => 2,
            Intent::DodgeOverhead => 3,
            Intent::SeekOxygen => 4,
            Intent::DigDown => 5,
            Intent::BreakRock => 6,
            Intent::Sidestep => 7,
            Intent::DigSideways => 8,
            Intent::EscapeClimb => 9,
            Intent::ClimbOver => 10,
            Intent::WaitOut => 11,
        }
    }

    /// 添字順の一覧(集計結果を名前付きで出力するため)。
    const ALL: [Intent; Self::COUNT] = [
        Intent::Idle,
        Intent::DefuseBomb,
        Intent::EvadeBomb,
        Intent::DodgeOverhead,
        Intent::SeekOxygen,
        Intent::DigDown,
        Intent::BreakRock,
        Intent::Sidestep,
        Intent::DigSideways,
        Intent::EscapeClimb,
        Intent::ClimbOver,
        Intent::WaitOut,
    ];
}

/// 頭上から落ちてくる塊の情報(TERM独自拡張。#221)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ColumnThreat {
    /// プレイヤーの行から何行上にあるか(1=直上)。
    dist: usize,
    /// まだ揺れ(落下開始前の猶予)の最中か。落下中ならfalse。
    shaking: bool,
    /// 今はまだ支えられていて、こちらがその列へ掘り込んだ瞬間に支えを失う塊か。
    /// この場合、落下が始まるのは揺れ(`shake_duration_ms`)が明けてからなので、
    /// 猶予を丸ごと見込める(#225)。
    pending: bool,
}

/// 経路上の岩の扱い(TERM独自拡張。#229)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RockBudget {
    /// 通常。酸素で払える岩しか経路に含めない。
    Affordable,
    /// 迂回も待ちも尽きた場面。払えない岩も候補に残し、縦に割るのと同じ土俵で比べる。
    LastResort,
}

/// 列採点の共通材料(TERM独自拡張。#229)。列ごとに変わらない計算を`score_columns`が
/// 1回だけ行い、各列の採点へ配る。
struct ScoreContext {
    /// 候補範囲(`slack`/`safe`の先頭が指す列)。
    lo: usize,
    hi: usize,
    /// 酸素が逼迫しているか(AIRの加点と先読み行数が変わる)。
    emergency: bool,
    rock_budget: RockBudget,
    /// 候補範囲の各列の余裕(行)。頭上に脅威が無ければ+∞。
    slack: Vec<f32>,
    /// 候補範囲の各列へ入ってよいか(必要な余裕を満たすか)。
    safe: Vec<bool>,
    /// AIR1個ぶんの加点(行)。回復量が寄り道の最低ラインに届かなければ0。
    air_bonus: f32,
    /// アイテム1個ぶんの加点(行)。効果ごとに、今の盤面でどれだけ役に立つかで決める
    /// (TERM独自拡張。#229 F4)。
    item_clear_above: f32,
    item_starify: f32,
}

impl ScoreContext {
    /// そのマスに乗っているAIR/アイテムの加点(AIRぶん, アイテムぶん)。
    /// `air_value`は列ごとの寄り道距離を織り込んだAIRの加点。
    fn pickup_value(&self, cell: Cell, air_value: f32) -> (f32, f32) {
        match cell {
            Cell::Oxygen => (air_value, 0.0),
            Cell::Item(ItemEffect::ClearAbove) => (0.0, self.item_clear_above),
            Cell::Item(ItemEffect::StarifyScreen) => (0.0, self.item_starify),
            // 色の統一は「掘りやすさ」を変えるだけで、行数への換算が立たない。
            // 寄り道してまで取る理由が無いため加点しない(#221からの据え置き)。
            _ => (0.0, 0.0),
        }
    }

    /// `c`列の余裕(行)。候補範囲の外は安全側に倒して「余裕なし」とする。
    fn slack_of(&self, c: usize) -> f32 {
        self.slack
            .get(c.wrapping_sub(self.lo))
            .copied()
            .unwrap_or(f32::NEG_INFINITY)
    }

    /// `c`列へ入ってよいか。候補範囲の外は通さない。
    fn is_safe(&self, c: usize) -> bool {
        self.safe
            .get(c.wrapping_sub(self.lo))
            .copied()
            .unwrap_or(false)
    }
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
            rock_bought_rows: 0,
            rock_break_pending: false,
            waiting_frames: 0,
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
            // GameOver中は何もしない(#225)。以前はここで1.5秒待って自動Reviveしていたが、
            // 通常ライフ予算での完走率が測れなくなり「死んだのに見えないまま自動継続」して
            // いた。人間が操作したときと全く同じにGameOverダイアログを出したまま待つ。
            GameStatus::GameOver => return (Intent::Idle, Vec::new()),
            GameStatus::Playing => {}
            // 一時停止中・クリア後は操作しない(クリア到達後はその場で待機する)。
            GameStatus::Paused | GameStatus::Cleared => return (Intent::Idle, Vec::new()),
        }

        self.note_progress(game.player.position());
        let decision = self.decide_while_playing(game);
        self.note_decision(&decision.0);
        decision
    }

    /// `decide_with_intent`の本体(プレイ中のみ)。優先順の分岐だけを持ち、判断結果の
    /// 記録(`note_decision`)は呼び出し側がまとめて行う。
    fn decide_while_playing(&mut self, game: &Game) -> (Intent, Vec<InputAction>) {
        if let Some(actions) = self.defuse_adjacent_bomb(game) {
            return (Intent::DefuseBomb, actions);
        }
        if let Some(decision) = self.evade_imminent_bomb(game) {
            return decision;
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

    /// そのフレームの判断を次フレームのために記録する(#225)。
    ///
    /// - 岩を割った直後は`rock_break_pending`を立てておく。岩が砕けた次のフレームは
    ///   自由落下待ち(`Idle`)になるため、「前フレームの意図」だけ見ると岩で買った前進を
    ///   `note_progress`が取りこぼす。
    /// - 待ち(`WaitOut`)はフレーム数を数え、上限を超えたら待つのをやめる。数えた値を
    ///   戻すのは`note_progress`が行の前進を検出したときだけ(#229)。**以前はWaitOut
    ///   以外の意図が1フレーム挟まるだけで0へ戻していた**ため、`conserve_oxygen`の
    ///   手順(待つ→予算切れで別の手→また待つ)がそのまま無限ループになり、待機上限
    ///   (`AUTOPLAY_WAIT_FOR_THREAT_MAX_MS`)が実質無効だった。実測では1000mコースの
    ///   酸素切れ死82/99件が、直前300フレームの過半を待ち・登り・落下待ちで費やしていた。
    fn note_decision(&mut self, intent: &Intent) {
        if *intent == Intent::BreakRock {
            self.rock_break_pending = true;
        }
        if *intent == Intent::WaitOut {
            self.waiting_frames = self.waiting_frames.saturating_add(1);
        }
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
            // 待ち予算は「実際に行が進んだ」ときだけ戻す(#229)。
            self.waiting_frames = 0;
            // 岩を割って買った前進は「前進」に数えない(#225)。数えてしまうと、岩を
            // 割るたびに段階が0へ戻り、横方向の岩掘りが解禁される段階(2)へ永久に
            // 到達しない(実測で全岩破壊判断のうちescalation1以上は0件だった)。
            if self.rock_break_pending {
                self.rock_break_pending = false;
                self.rock_bought_rows = self.rock_bought_rows.saturating_add(1);
                if self.rock_bought_rows >= AUTOPLAY_ROCK_STREAK_ESCALATE {
                    self.escalation = self.escalation.max(1);
                }
            } else {
                self.rock_bought_rows = 0;
            }
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
    fn evade_imminent_bomb(&self, game: &Game) -> Option<(Intent, Vec<InputAction>)> {
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

        let escape_by_row = self
            .dig_down_to_change_row(game)
            .map(|actions| (Intent::EvadeBomb, actions));
        let escape_by_col = self
            .safe_sidestep_direction(game)
            .map(|dir| (Intent::EvadeBomb, vec![move_action(dir)]));
        let escape_by_climb = self
            .safe_climb_direction(game)
            .map(|dir| (Intent::ClimbOver, vec![move_action(dir)]));

        if same_row_threat {
            // 同じ行から出るには行を変えるしかない。直下が岩なら掘り下げは酸素20%の
            // 買い物になるため、無料で行を変えられる段差登りを先に試す(#229 F3)。
            if matches!(
                game.board.cell_or_none(row + 1, col),
                Some(Cell::Rock { .. })
            ) {
                escape_by_climb.or(escape_by_row).or(escape_by_col)
            } else {
                escape_by_row.or(escape_by_climb).or(escape_by_col)
            }
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
        //
        // ここへ「段差を登って避ける」を第3の選択肢として足す案(#229 F3)は実測で
        // 不採用。1000mコースの完走が11/32→5/32・平均到達893m→868mへ落ちた
        // (押し潰されは1.81→1.66回/走へ減るが、登ると1行戻るぶん深度が伸びず、
        // 酸素切れが2.94→3.12回/走へ増えて差し引きで損)。「両隣が固体で歩いて
        // 出られない場面だけ登る」と絞っても結果は同じだった。同じ登りでも、ボムから
        // 逃げる場面(`evade_imminent_bomb`)だけは岩1個ぶんの酸素を節約できるため
        // 残してある。
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
                    let already_unstable = game.is_cell_unstable(row, col);
                    // 「今は支えられているが、こちらが掘り込めば支えを失う」塊。実際に
                    // 崩れ始めるのはこちらが掘り抜いた後で、しかもその後さらに揺れの
                    // 猶予がある(physics: 支えを失った直後は即落下せず揺れる)。
                    let pending = !already_unstable
                        && is_unsupported_after_removal(game, &vacated, (row, col));
                    return (already_unstable || pending).then(|| ColumnThreat {
                        dist,
                        shaking: game.is_cell_shaking(row, col),
                        pending,
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
        // 「間に合うつもりで動き出して間に合わない」押し潰しが倍近くに増えた。隣接列だけ
        // 倍率を外す案も#225で実測したが、やはり押し潰しが1.66→2.47回/走へ増えたため
        // 距離によらず倍のままにしている。
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
        self.score_columns_with(game, RockBudget::Affordable)
    }

    /// 経路上の岩の扱いを指定して採点する(#229)。待ち予算まで使い切った場面では
    /// 「払えない岩でも通る」候補を出し直し、縦に割るのと同じ土俵で比べる。
    fn score_columns_with(&self, game: &Game, budget: RockBudget) -> Vec<ColumnScore> {
        let context = self.score_context(game, budget);
        (context.lo..=context.hi)
            .filter_map(|c| self.score_column(game, c, &context))
            .collect()
    }

    /// 列ごとに変わらない計算を1フレームに1回だけ行う(#229)。頭上の脅威は「入って
    /// よいかの判定」と「採点の減点」の両方で要るため、以前は列あたり2回計算していた。
    fn score_context(&self, game: &Game, rock_budget: RockBudget) -> ScoreContext {
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

        let threats: Vec<Option<ColumnThreat>> = (lo..=hi)
            .map(|c| self.column_threat(game, c, game.player.row))
            .collect();
        let slack: Vec<f32> = threats
            .iter()
            .enumerate()
            .map(|(index, threat)| match threat {
                None => f32::INFINITY,
                Some(threat) => self.threat_slack_rows(game, lo + index, threat),
            })
            .collect();
        // 経路の途中で通り過ぎるだけの列も、入った瞬間に潰されるなら通ってはいけない。
        // 目的列しか見ないと「安全な列を目指して危険な列を踏み抜く」ことになる(実測した
        // 死因の最多パターン)。列ごとに一度だけ判定して経路チェックで使い回す。
        let minimum = min_slack_rows(game);
        let safe: Vec<bool> = slack.iter().map(|slack| *slack >= minimum).collect();

        let gain = air_gain(game.player.oxygen);
        let air_bonus = if gain >= AUTOPLAY_AIR_MIN_GAIN {
            let multiplier = if emergency {
                AUTOPLAY_EMERGENCY_AIR_SCORE_MULTIPLIER
            } else {
                1.0
            };
            gain / AUTOPLAY_SCORE_AIR_DIVISOR * multiplier
        } else {
            0.0
        };

        // スター化(K)は画面内の岩をまとめてスターへ変えるアイテム。先読み範囲に岩が
        // 無ければ何も起きないので、寄り道してまで取る理由も無い(#229 F4)。価値の
        // 材料は列に依らないため、ここで1回だけ数える。
        let near_lo = col.saturating_sub(AUTOPLAY_COLUMN_SCAN_RADIUS).max(lo);
        let near_hi = (col + AUTOPLAY_COLUMN_SCAN_RADIUS).min(hi);
        let rocks_ahead = (near_lo..=near_hi)
            .flat_map(|c| (1..=AUTOPLAY_LOOKAHEAD_ROWS).map(move |d| (game.player.row + d, c)))
            .filter(|&(r, c)| matches!(game.board.cell_or_none(r, c), Some(Cell::Rock { .. })))
            .count();

        ScoreContext {
            lo,
            hi,
            emergency,
            rock_budget,
            slack,
            safe,
            air_bonus,
            // 頭上クリア(R)も同じように「頭上の脅威の数」へ連動させる案は実測で不採用
            // (#229 F4)。取得率が35%→20%へ落ち、1000mコースの完走が11/32→5/32・
            // 酸素切れが2.94→3.34回/走へ悪化した。今見えている脅威の数では、この
            // アイテムの価値(掘り進んだ後の天井をまとめて消せること)を測れていない。
            item_clear_above: AUTOPLAY_SCORE_ITEM_BONUS,
            item_starify: saturating_item_bonus(rocks_ahead, AUTOPLAY_ITEM_STARIFY_ROCK_SATURATION),
        }
    }

    /// 1列ぶんの採点。到達できない・入った瞬間に潰される列は`None`(=スコア-∞)。
    ///
    /// 基礎点は`clear_run`(その列を岩に当たらず掘り進める行数、上限
    /// `AUTOPLAY_LOOKAHEAD_ROWS`)で、単位は「行」。加減点もすべて行に換算して揃える。
    fn score_column(&self, game: &Game, c: usize, ctx: &ScoreContext) -> Option<ColumnScore> {
        let (row, col) = game.player.position();
        let dist = c.abs_diff(col);
        let emergency = ctx.emergency;

        if !self.lateral_path_is_open(game, c, ctx) {
            return None;
        }

        // 頭上の塊は「その列へ移って1手ぶん進めた後、まだ何行ぶん離れていられるか」で
        // 評価する(`threat_slack_rows`)。
        //
        // 掘り進んだ後の自分の列には、掘ってきた穴の天井が必ず未支持のまま残る。つまり
        // 現在列はほぼ常に「脅威あり」なので、脅威の有無や距離をそのまま減点にすると
        // 現在列だけが永久に不利になり、AIが毎フレーム横へ逃げて前へ進まなくなる。
        // 余裕そのもので測れば、振り切れる見込みがある限り減点は0になる。
        //
        // 必要な余裕(`min_slack_rows`)を割り込む列は候補から外す。この基準を緩めて
        // 「減点はするが候補には残す」方式も実測したが、緩めたぶんがそのまま押し潰し死に
        // 変わった(1走あたり1.66回→4.12〜4.59回)ため採らない。候補が空になる問題は
        // 基準を緩めてではなく、揺れ猶予の見積り誤り(`shake_allowance_ms`)を直して
        // 解決している(#225)。
        let threat_penalty = {
            let slack = ctx.slack_of(c);
            let minimum = min_slack_rows(game);
            if slack < minimum {
                return None;
            }
            if slack.is_finite() {
                let comfort = slack / minimum - 1.0;
                AUTOPLAY_SCORE_THREAT_PENALTY * (1.0 - comfort).clamp(0.0, 1.0)
            } else {
                0.0
            }
        };

        // 寄り道の距離制限(緊急時は外す)を満たす列だけがAIRの加点を受ける。
        let air_value = if emergency || dist <= AUTOPLAY_AIR_DETOUR_MAX_COLS {
            ctx.air_bonus
        } else {
            0.0
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
            let (gained_air, gained_items) = ctx.pickup_value(cell, air_value);
            air += gained_air;
            items += gained_items;
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
        // 経路上の岩は「禁止」ではなく「有料」として扱う(#225)。縦に割る岩と同じ
        // `rock_cost_rows`で数えることで、「横へ1個割って直進する」と「縦に割り続ける」が
        // 同じ土俵で比較される。以前は経路に岩があるだけで到達不能扱いにしていたため、
        // 1個割れば抜けられる列が候補にすら上がらなかった(実測で既定62%・harsh80%の
        // 岩破壊場面にそういう列が存在した)。
        // 緊急時に岩を安く見積もる(設計案のF6)のは実測で逆効果だった。酸素が乏しい
        // ときこそ20%の出費は重く、harsh設定で酸素切れが1走あたり0.3回増えた(#225)。
        let lateral_rocks = self.lateral_rock_count(game, c);
        // 払えない岩は縦も横も同じ20%。`descend`側だけで止めると、横へ岩を割りに行く
        // 経路が素通しになって酸素の歯止めが効かない(#225)。ただし待ち予算まで使い切った
        // 場面(`RockBudget::LastResort`)では、払えなくても縦に割るしかなくなるため、
        // 横の岩も同じ土俵に載せて比べる(#229 F2)。
        if lateral_rocks > 0
            && ctx.rock_budget == RockBudget::Affordable
            && !rock_is_affordable(game)
        {
            return None;
        }
        let lateral_rock_penalty = lateral_rocks as f32 * rock_cost_rows(game);

        Some(ColumnScore {
            col: c,
            score: run as f32 + air + items
                - lateral
                - threat_penalty
                - rock_penalty
                - stay_penalty
                - void_penalty
                - rock_below_penalty
                - lateral_rock_penalty,
            air,
        })
    }

    /// 現在行を横に進んで`c`列へ着くまでに、経路上で割ることになる岩の数。
    /// 目的列そのもののマスも含む(そこも通り抜ける必要があるため)。
    fn lateral_rock_count(&self, game: &Game, c: usize) -> usize {
        let (row, col) = game.player.position();
        let step: isize = if c < col { -1 } else { 1 };
        let mut cursor = col;
        let mut rocks = 0;
        while cursor != c {
            let Some(next) = cursor.checked_add_signed(step) else {
                break;
            };
            if next >= game.board.width() {
                break;
            }
            if matches!(game.board.cell(row, next), Cell::Rock { .. }) {
                rocks += 1;
            }
            cursor = next;
        }
        rocks
    }

    /// 現在行を横に進んで`c`列へ到達できるか。静止ボムで塞がっているか、入った瞬間に
    /// 潰される列を踏むなら通れない。落下中は横移動自体が通らないため、現在列以外は
    /// 到達不能とする。
    ///
    /// 経路上の岩はここでは弾かない(#225)。岩は通れないのではなく酸素20%を払えば通れる
    /// ものなので、`score_column`が`lateral_rock_count`ぶんの減点として扱う。
    fn lateral_path_is_open(&self, game: &Game, c: usize, ctx: &ScoreContext) -> bool {
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
            if !ctx.is_safe(next) {
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
            // 岩は「今の酸素で払えるか」を見てから割る(#225)。ここが無条件だったため、
            // 緊急判定(`oxygen_reserve`)に入るより前に岩1個で20%を一気に持っていかれ、
            // 気づいた時には手遅れの残量になっていた(酸素切れ死の直前5秒間に割った岩は
            // 中央値4個=80%)。
            Cell::Rock { .. } if !rock_is_affordable(game) => self.conserve_oxygen(game),
            Cell::Rock { .. } => (
                Intent::BreakRock,
                self.drill_vertically(game, Direction::Down),
            ),
        }
    }

    /// 直下が岩だが、割ると酸素が緊急域へ落ちる場面での代替手段(#225)。
    ///
    /// 岩1個の20%に対し、自然減少は2〜5%/秒。数秒の遠回りや待ちの方がはるかに安いので、
    /// 「横へ迂回する→頭上の塊が着地するのを待つ→段差を登る」の順に試し、どれも無理な
    /// ときだけ最後の手段として割る。割った瞬間に致死圏へ入る残量なら、それすらしない。
    fn conserve_oxygen(&self, game: &Game) -> (Intent, Vec<InputAction>) {
        let col = game.player.col;

        // 1. 岩を踏まずに行ける横の候補列があればそちらへ。ここで岩のある経路を選ぶと
        //    縦に割るのと同じ20%を払うことになるので、無料の経路だけを見る。
        let mut best: Option<ColumnScore> = None;
        for candidate in self
            .score_columns(game)
            .iter()
            .filter(|s| s.col != col && self.lateral_rock_count(game, s.col) == 0)
        {
            if best.is_none_or(|current| self.is_better(col, candidate, &current)) {
                best = Some(*candidate);
            }
        }
        if let Some(target) = best {
            return self.step_toward(game, &target);
        }

        // 2. 隣が落ちてくる塊で塞がっているだけなら、着地して道が空くまでその場で待つ。
        if u64::from(self.waiting_frames) * FRAME_INTERVAL_MS < AUTOPLAY_WAIT_FOR_THREAT_MAX_MS
            && self.threat_settles_soon(game)
        {
            return (Intent::WaitOut, Vec::new());
        }

        // 3. 段差を登って別の場所からやり直す。
        if let Some(actions) = self.escape_climb(game) {
            return (Intent::EscapeClimb, actions);
        }

        // 4. 迂回も待ちも登りも尽きた。ここから先はどうせ岩を払うことになるので、
        //    縦に割ると決める前に「横へ抜ける経路の岩」を同じ土俵で比べ直す(#229 F2)。
        //    同じ20%でも、横1個割って掘り進める列へ移れるなら、その先で稼げる行数が
        //    まるごと違う。以前はここを見ずに直下を割っていたため、掘り止まりの列に
        //    留まったまま酸素だけを払い続けることがあった。
        //    現在列も候補に含めて比べる。現在列の点には直下の岩の代償
        //    (`rock_below_penalty`)が既に載っているため、「横へ1個割って抜ける」と
        //    「縦に割る」が同じ尺度で並ぶ。
        let mut best: Option<ColumnScore> = None;
        for candidate in self.score_columns_with(game, RockBudget::LastResort).iter() {
            if best.is_none_or(|current| self.is_better(col, candidate, &current)) {
                best = Some(*candidate);
            }
        }
        if let Some(target) = best
            && target.col != col
        {
            return self.step_toward(game, &target);
        }

        // 5. 最後の手段。ただし割った時点で致死圏に入るなら、自分から即死を選ばない。
        if rock_break_is_lethal(game) {
            return (Intent::Idle, Vec::new());
        }
        (
            Intent::BreakRock,
            self.drill_vertically(game, Direction::Down),
        )
    }

    /// 隣の列を塞いでいる塊が、待てば着地して道が空くか(#225)。
    ///
    /// 着地までの見込み(揺れの残り+落下距離×ブロックの落下tick)が
    /// `AUTOPLAY_WAIT_FOR_THREAT_MAX_MS`以内なら待つ価値がある。岩で塞がれている側は
    /// 待っても変わらないので数えない。
    ///
    /// こちらが掘り込んで初めて落ちる塊(`pending`)も数に入れている。「待っても勝手には
    /// 落ちてこないのだから除くべき」と考えて除いてみたが、実測では既定設定の押し潰し死が
    /// 1走あたり1.47回→1.97回へ悪化した(#225)。隣に落下予定の塊がある間ひと呼吸置く
    /// こと自体に、盤面が落ち着くのを待つ効果がある。
    fn threat_settles_soon(&self, game: &Game) -> bool {
        let (row, col) = game.player.position();
        if !game.player_is_grounded() {
            return false;
        }
        let block_tick = game.effective_block_fall_tick_ms().max(1);
        [Direction::Left, Direction::Right]
            .into_iter()
            .filter_map(|dir| neighbor(game, (row, col), dir))
            .filter(|target| !matches!(game.board.cell(target.0, target.1), Cell::Rock { .. }))
            .filter_map(|target| self.column_threat(game, target.1, row))
            .any(|threat| {
                let shake_ms = if threat.shaking {
                    game.shake_duration_ms()
                } else {
                    0
                };
                shake_ms + threat.dist as u64 * block_tick <= AUTOPLAY_WAIT_FOR_THREAT_MAX_MS
            })
    }

    /// 手詰まり脱出用の段差登り。登り先の棚がある方向へ`MoveX`を出し続ける。
    /// `move_lateral`の段差登りは「同じ方向へ2回ぶつかる」ことで成立するため、同方向の
    /// 入力を連投してよいのはこの経路だけ(通常の`step_toward`は誤って段差を登らないよう、
    /// ぶつかった次は掘りに切り替える)。
    ///
    /// **棚の有無を必ず確かめてから出す**(#229)。以前は`side_preference`側に盤面が
    /// 続いてさえいれば`MoveX`を出し続けていたため、登れない壁に対して最大510フレーム
    /// (17秒)ぶつかり続け、その間の自然減少だけで酸素切れに至っていた(harsh設定の
    /// 酸素切れ死14件中9件が、死の直前300フレームの半分以上をこの空振りに使っていた)。
    /// 該当する方向が無ければ`None`を返し、呼び出し側の採点・岩割りへ素直に譲る。
    fn escape_climb(&self, game: &Game) -> Option<Vec<InputAction>> {
        self.climb_direction(game).map(|dir| vec![move_action(dir)])
    }

    /// 段差登りが成立する方向(`side_preference`側→逆側の順)。
    fn climb_direction(&self, game: &Game) -> Option<Direction> {
        [self.side_preference, opposite(self.side_preference)]
            .into_iter()
            .find(|&dir| self.can_climb_step(game, dir))
    }

    /// 段差登りが成立し、かつ登り切った先で頭上の塊との余裕が残る方向(#229)。
    /// 危険から逃げる手段として登るときは、逃げ込んだ先で潰されては意味が無いため、
    /// 横移動(`column_is_safe_to_enter`)と同じ基準で登り先も確かめる。
    fn safe_climb_direction(&self, game: &Game) -> Option<Direction> {
        let minimum = min_slack_rows(game);
        [self.side_preference, opposite(self.side_preference)]
            .into_iter()
            .find(|&dir| {
                self.can_climb_step(game, dir) && self.climb_slack_rows(game, dir) >= minimum
            })
    }

    /// `dir`へ段差を登り切った時点で、登り先の列の頭上との間に何行ぶんの余裕が残るか。
    ///
    /// 登ると行が1つ上がる=脅威に1行ぶん近づくため、`threat_slack_rows`(同じ行に
    /// 留まったまま横へ移る前提)ではなく登り先の行から測り直す。登り切るには同方向へ
    /// 2回入力する必要があるので、その所要時間も行数に換算して差し引く。
    fn climb_slack_rows(&self, game: &Game, dir: Direction) -> f32 {
        let (row, col) = game.player.position();
        let Some(side) = neighbor(game, (row, col), dir) else {
            return f32::NEG_INFINITY;
        };
        let Some(landing_row) = row.checked_sub(1) else {
            return f32::NEG_INFINITY;
        };
        match self.column_threat(game, side.1, landing_row) {
            None => f32::INFINITY,
            Some(threat) => {
                let block_tick = game.effective_block_fall_tick_ms().max(1) as f32;
                let climb_ms = 2 * (game.move_cooldown_ms() + FRAME_INTERVAL_MS);
                threat.dist as f32 + shake_allowance_ms(game, &threat) as f32 / block_tick
                    - climb_ms as f32 / block_tick
            }
        }
    }

    /// `dir`へ段差登り(`physics::move_lateral`)が実際に成立するか。`move_lateral`の
    /// 成立条件をそのまま写したもの: 接地していること・自分の頭上が空いていること・
    /// 隣が固体(=ぶつかれる)であること・登り先の1マス斜め上が入れるマスであること。
    /// 静止ボムは押し出し/登り判定がゲーム側の別経路になるため、どちらの位置でも
    /// 「登れない」として扱う。
    fn can_climb_step(&self, game: &Game, dir: Direction) -> bool {
        let (row, col) = game.player.position();
        if row == 0 || !game.player_is_grounded() {
            return false;
        }
        if game.board.cell(row - 1, col) != Cell::Empty {
            return false;
        }
        let Some(side) = neighbor(game, (row, col), dir) else {
            return false;
        };
        let side_is_solid = !matches!(
            game.board.cell(side.0, side.1),
            Cell::Empty | Cell::Oxygen | Cell::Item(_)
        );
        if !side_is_solid || settled_bomb_at(game, side) {
            return false;
        }
        self.is_safe_to_enter(game, (row - 1, side.1))
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

/// 岩1個ぶんの酸素(直接消費20%+割り切るまでの自然減少)を払っても、緊急域
/// (`oxygen_reserve`)より上に居られるか(#225)。
///
/// 緊急判定そのものは残量が減ってから初めて反応するため、判定に入る前に岩1個で20%を
/// 持っていかれると手遅れになる(実測では緊急判定に入った時点で既に中央値22%、harsh設定
/// では10.6%しか残っていなかった)。割る前にこの1歩先を見る。
fn rock_is_affordable(game: &Game) -> bool {
    let depth = game.player.depth_m();
    let drill_ms = f32::from(ROCK_HITS_TO_BREAK) * drill_action_ms() as f32;
    let spent = ROCK_BREAK_OXYGEN_PENALTY + oxygen_decay_per_sec(depth) * (drill_ms / 1000.0);
    game.player.oxygen - spent >= oxygen_reserve(depth)
}

/// 岩を割った時点で緊急域に入る(=即死または致死圏)残量か(#225)。ここまで減っていたら、
/// 他に手が無くても自分から割りにはいかない。
fn rock_break_is_lethal(game: &Game) -> bool {
    game.player.oxygen <= oxygen_reserve(game.player.depth_m()) + ROCK_BREAK_OXYGEN_PENALTY
}

/// AIRを1個取ったときの実効回復量(%)。上限100でクランプされるため、満タンに近いほど
/// 小さくなる。
fn air_gain(oxygen: f32) -> f32 {
    OXYGEN_CAPSULE_RESTORE.min((OXYGEN_MAX - oxygen).max(0.0))
}

/// アイテムの加点(行)。`amount`がそのアイテムの効き目の材料(岩の数・脅威を抱えた
/// 列の数)で、`saturation`に達したところで`AUTOPLAY_SCORE_ITEM_BONUS`いっぱいになる
/// (TERM独自拡張。#229 F4)。0なら加点0=寄り道しない。
fn saturating_item_bonus(amount: usize, saturation: usize) -> f32 {
    let saturation = saturation.max(1);
    AUTOPLAY_SCORE_ITEM_BONUS * amount.min(saturation) as f32 / saturation as f32
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
///
/// 上限(`AUTOPLAY_THREAT_MAX_SLACK_ROWS`)を設けるのは、横移動を遅く・ブロック落下を速く
/// 設定すると要求が際限なく伸びてしまうため(#225)。実測では横移動120ms・落下75msの設定で
/// 6.1〜8.8行を要求し、頭上に塊がある列がほぼ全て候補から外れて岩を割るしかなくなっていた。
fn min_slack_rows(game: &Game) -> f32 {
    let reaction_ms = (game.move_cooldown_ms() + FRAME_INTERVAL_MS) as f32;
    let block_tick = game.effective_block_fall_tick_ms().max(1) as f32;
    (AUTOPLAY_THREAT_REACTION_STEPS * reaction_ms / block_tick).clamp(
        AUTOPLAY_THREAT_MIN_SLACK_ROWS,
        AUTOPLAY_THREAT_MAX_SLACK_ROWS,
    )
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

/// その塊が落ち始めるまでに見込める猶予(ms)。
///
/// - こちらが掘り込んで初めて支えを失う塊(`pending`)は、崩れ始めるのがこちらの掘削の
///   後で、しかも支えを失ってから`shake_duration_ms`ぶん揺れてからでないと落ちてこない
///   (physics側の仕様)。猶予を丸ごと見込める(#225)。**以前はここを0にしていた**ため、
///   横へ掘り抜ける列がほぼ全て「入った瞬間に潰される」と判定され、迂回路が候補から
///   消えて岩を割るしかなくなっていた。酸素切れ死の最大の原因がこの見積り誤り
/// - 既に揺れている塊は残り時間が外から分からないため、期待値として半分を見込む
/// - 既に落下中の塊には猶予が無い
fn shake_allowance_ms(game: &Game, threat: &ColumnThreat) -> u64 {
    if threat.pending {
        game.shake_duration_ms()
    } else if threat.shaking {
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
    use crate::settings::Settings;
    use std::collections::{HashMap, VecDeque};

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

    /// 判断の前提を1つずつ組み立てるための厳密な盤面(#229)。盤面をクリアしたうえで
    /// プレイヤーより下を最深行までダイヤで埋め、アイテムの出現率を0%にする。
    ///
    /// `game_at`(全マスEmpty)は手で置いたブロックが軒並み「支えを失った塊」になるため、
    /// 崩落予測が意図しない形で反応する。また`Game::update`を回すテストでは窓補充
    /// (`top_up_items_ahead`)が走り、置いた覚えのないアイテムが湧く。どちらも
    /// 「この盤面ならこう判断するはず」を検証したいテストにとっては雑音になる。
    fn solid_floor_game_at(seed: u64, row: usize, col: usize) -> Game {
        let mut game = Game::new(seed);
        game.apply_settings(&Settings {
            item_clear_above_rate_percent: 0,
            item_unify_colors_rate_percent: 0,
            item_starify_screen_rate_percent: 0,
            ..Settings::default()
        });
        for r in game.board.rows.iter_mut() {
            for cell in r.iter_mut() {
                *cell = Cell::Empty;
            }
        }
        // 足場はダイヤにする。連結しない種別なので、掘削で消える範囲が1マスに閉じ、
        // 足場そのものが崩落予測を動かすことがない。
        for r in (row + 1)..game.board.depth_rows() {
            for c in 0..game.board.width() {
                game.board.rows[r][c] = Cell::Diamond;
            }
        }
        game.player.row = row;
        game.player.col = col;
        game.player.facing = Direction::Down;
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

    /// GameOverになったら何もしない(#225)。以前は1.5秒待って自動Reviveしていたが、
    /// それでは通常のライフ予算内の完走率が測れず、「死んだのに見えないまま自動継続」
    /// していた。人間がプレイしたときと同じくダイアログを出したまま操作を待つ。
    #[test]
    fn decide_never_touches_the_game_over_dialog() {
        let mut game = game_at(2, 500, 5);
        game.status = GameStatus::GameOver;
        let mut pilot = Autopilot::new(false);

        for frame in 0..200 {
            assert_eq!(
                pilot.decide_with_intent(&game),
                (Intent::Idle, Vec::new()),
                "frame={frame}: GameOver中は何も押さないはず"
            );
        }
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

    // --- 酸素を見てから岩を割る(#225 F1) -----------------------------------

    /// 岩の代償を払うと緊急域へ落ちる残量では、横に安全な迂回路がある限りそちらへ向かう。
    #[test]
    fn decide_detours_instead_of_breaking_a_rock_it_cannot_afford() {
        let mut game = grounded_game_at(40, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 };
        // 右隣は歩いて入れて、その先も掘り進められる(=安全な迂回路)。
        game.board.rows[500][6] = Cell::Empty;
        game.board.rows[501][6] = Cell::Color(ColorKind::Red);
        // 「割ったら緊急域へ落ちる」ぎりぎりの残量(致死圏よりは上)にする。
        game.player.oxygen =
            oxygen_reserve(game.player.depth_m()) + ROCK_BREAK_OXYGEN_PENALTY + 1.0;
        assert!(
            !rock_is_affordable(&game),
            "前提: 岩を割ると緊急域へ落ちる残量"
        );
        let mut pilot = Autopilot::new(false);

        let (intent, _) = pilot.decide_with_intent(&game);
        assert_ne!(intent, Intent::BreakRock, "払えない岩は割らないはず");
    }

    /// 酸素が十分あれば、同じ盤面でも従来通り岩を割って進んでよい。
    #[test]
    fn decide_still_breaks_an_affordable_rock_when_there_is_no_way_around() {
        let mut game = game_at(41, 500, 5);
        for col in 0..game.board.width() {
            game.board.rows[501][col] = Cell::Rock { hits: 0 };
        }
        assert!(rock_is_affordable(&game), "前提: 満タンなら岩は払える");
        let mut pilot = Autopilot::new(false);

        assert_eq!(pilot.decide_with_intent(&game).0, Intent::BreakRock);
    }

    /// 割った時点で緊急域に入る残量では、他に手が無くても自分から岩を割りにはいかない。
    #[test]
    fn decide_refuses_to_break_a_rock_that_would_be_immediately_lethal() {
        let mut game = game_at(42, 500, 5);
        // 直下も両隣も岩の完全な行き止まり。通常なら割るしかない場面。
        game.board.rows[501][5] = Cell::Rock { hits: 0 };
        game.board.rows[500][4] = Cell::Rock { hits: 0 };
        game.board.rows[500][6] = Cell::Rock { hits: 0 };
        game.player.oxygen = oxygen_reserve(game.player.depth_m()) + 1.0;
        assert!(
            rock_break_is_lethal(&game),
            "前提: 割ったら緊急域へ落ちる残量"
        );
        let mut pilot = Autopilot::new(false);

        assert_ne!(
            pilot.decide_with_intent(&game).0,
            Intent::BreakRock,
            "自分から即死を選ばないはず"
        );
    }

    /// 岩が払えないとき、隣が落下中の塊で塞がっているだけなら着地を待つ。
    /// 自然減少(2〜5%/秒)は岩1個の20%よりずっと安い。
    #[test]
    fn decide_waits_for_a_falling_block_to_land_rather_than_paying_for_a_rock() {
        let mut game = game_at(43, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 直下は岩=足場でもある
        // 左右とも、頭上に落ちてくる塊を抱えていて今は入れない。
        for col in [4, 6] {
            game.board.rows[500][col] = Cell::Empty;
            game.board.rows[499][col] = Cell::Color(ColorKind::Blue); // 支えなし
        }
        game.player.oxygen =
            oxygen_reserve(game.player.depth_m()) + ROCK_BREAK_OXYGEN_PENALTY + 1.0;
        assert!(!rock_is_affordable(&game), "前提: 岩は払えない");
        let mut pilot = Autopilot::new(false);

        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::WaitOut);
        assert_eq!(actions, Vec::new(), "待つ間は何も押さない");
    }

    /// 待ちは`AUTOPLAY_WAIT_FOR_THREAT_MAX_MS`で打ち切る。待ち続けて酸素切れになるより、
    /// 打ち切って他の手(段差登り・最後の手段の岩)へ進む。
    #[test]
    fn waiting_for_a_threat_is_bounded_so_the_pilot_never_stalls_until_it_suffocates() {
        let mut game = game_at(44, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 };
        for col in [4, 6] {
            game.board.rows[500][col] = Cell::Empty;
            game.board.rows[499][col] = Cell::Color(ColorKind::Blue);
        }
        game.player.oxygen =
            oxygen_reserve(game.player.depth_m()) + ROCK_BREAK_OXYGEN_PENALTY + 1.0;
        let mut pilot = Autopilot::new(false);

        let budget_frames = AUTOPLAY_WAIT_FOR_THREAT_MAX_MS / FRAME_INTERVAL_MS;
        for _ in 0..=budget_frames {
            pilot.decide_with_intent(&game);
        }
        assert_ne!(
            pilot.decide_with_intent(&game).0,
            Intent::WaitOut,
            "予算を使い切ったら待つのをやめるはず"
        );
    }

    // --- 待ち予算の累積(#229 F2) -------------------------------------------

    /// 待ち予算は「WaitOut以外の意図が1フレーム挟まった」程度では戻さない。戻して
    /// いたため、`conserve_oxygen`の「待つ→予算切れで別の手→また待つ」がそのまま
    /// 無限ループになり、待機上限が実質無効だった(#229)。
    #[test]
    fn the_wait_budget_only_resets_once_the_row_actually_advances() {
        let mut pilot = Autopilot::new(false);
        pilot.note_progress((500, 5));

        let budget_frames = (AUTOPLAY_WAIT_FOR_THREAT_MAX_MS / FRAME_INTERVAL_MS) as u32;
        for _ in 0..budget_frames {
            pilot.note_decision(&Intent::WaitOut);
        }
        assert_eq!(
            pilot.waiting_frames, budget_frames,
            "前提: 予算を使い切った"
        );

        // 別の手を1フレーム挟み、横にだけ動く(行は進んでいない)。
        pilot.note_decision(&Intent::DigSideways);
        pilot.note_progress((500, 6));
        assert_eq!(
            pilot.waiting_frames, budget_frames,
            "行が進まない限り予算は戻らないはず"
        );
        pilot.note_decision(&Intent::WaitOut);
        assert_eq!(
            pilot.waiting_frames,
            budget_frames + 1,
            "続きから累積するはず"
        );

        // 実際に1行進んだら、そこで初めて予算が戻る。
        pilot.note_progress((501, 6));
        assert_eq!(pilot.waiting_frames, 0);
    }

    // --- 横の岩は禁止ではなく有料(#225 F2) ---------------------------------

    /// 「横へ1個割れば直進できる列」と「縦に岩を割り続ける列」を同じ土俵で比較する。
    /// 以前は経路上に岩があるだけで到達不能扱いで、候補にすら上がらなかった。
    #[test]
    fn a_column_behind_one_rock_is_reachable_and_priced_rather_than_forbidden() {
        let mut game = grounded_game_at(45, 500, 5);
        game.board.rows[500][6] = Cell::Rock { hits: 0 }; // 横1個だけ岩
        let pilot = Autopilot::new(false);

        assert_eq!(
            pilot.lateral_rock_count(&game, 6),
            1,
            "経路上の岩は数えられるはず"
        );
        let context = pilot.score_context(&game, RockBudget::Affordable);
        assert!(
            pilot.lateral_path_is_open(&game, 6, &context),
            "岩があっても到達不能にはしない(有料なだけ)"
        );
    }

    /// 縦に岩を割り続けるより、横1個で抜けて直進できる方を選ぶ。
    #[test]
    fn decide_prefers_paying_for_one_sideways_rock_over_a_column_of_rocks() {
        let mut game = game_at(46, 500, 5);
        // 現在列は下へ岩が続く。左も岩で塞ぐ。
        for row in 501..=506 {
            game.board.rows[row][5] = Cell::Rock { hits: 0 };
        }
        game.board.rows[500][4] = Cell::Rock { hits: 0 };
        game.board.rows[501][4] = Cell::Rock { hits: 0 };
        // 右は岩1個の向こうが素通しで、その先も掘り進められる。
        game.board.rows[500][6] = Cell::Rock { hits: 0 };
        for row in 501..=514 {
            game.board.rows[row][6] = if row % 2 == 0 {
                Cell::Color(ColorKind::Red)
            } else {
                Cell::Color(ColorKind::Green)
            };
        }
        let mut pilot = Autopilot::new(false);

        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::BreakRock);
        assert_eq!(
            actions,
            vec![InputAction::MoveRight],
            "縦に割り続けるのではなく、横1個を割って抜ける向きを向くはず"
        );
    }

    // --- 岩で買った前進は「前進」に数えない(#225 F5) -----------------------

    #[test]
    fn rows_bought_by_breaking_rocks_escalate_instead_of_resetting_the_stall_counter() {
        let mut pilot = Autopilot::new(false);
        pilot.note_progress((500, 5));
        assert_eq!(pilot.escalation, 0);

        // 岩を割って1行進む、を繰り返す。停滞判定だけなら毎回「前進」で段階は0のまま。
        for step in 1..=u32::from(AUTOPLAY_ROCK_STREAK_ESCALATE) {
            pilot.note_decision(&Intent::BreakRock);
            pilot.note_progress((500 + step as usize, 5));
        }
        assert!(
            pilot.escalation >= 1,
            "岩で買った前進が続いたら段階が上がるはず(escalation={})",
            pilot.escalation
        );

        // 岩以外で前進したら通常通り0へ戻り、連続回数もリセットされる。
        pilot.note_decision(&Intent::DigDown);
        pilot.note_progress((520, 5));
        assert_eq!(pilot.escalation, 0);
        assert_eq!(pilot.rock_bought_rows, 0);
    }

    // --- 安全判定(#225 F3/F4の実測結果) -----------------------------------

    /// 必要な余裕を割り込む列は、減点ではなく却下のままにする。設計案では「致死でない
    /// 限り候補に残す」としていたが、実測で緩めたぶんがそのまま押し潰し死に変わった
    /// (1走あたり1.66回→4.12〜4.59回)ため採らなかった。
    #[test]
    fn a_column_without_enough_slack_is_rejected_outright_not_merely_penalised() {
        let mut game = game_at(47, 900, 5);
        game.board.rows[901][5] = Cell::Rock { hits: 0 }; // 足場
        game.board.rows[899][6] = Cell::Color(ColorKind::Blue); // 右隣の頭上、支えなし
        let pilot = Autopilot::new(false);

        let slack = pilot.column_slack_rows(&game, 6);
        assert!(
            slack < min_slack_rows(&game),
            "前提: 必要な余裕に届かない列({slack})"
        );
        assert!(!pilot.column_is_safe_to_enter(&game, 6));
        let context = pilot.score_context(&game, RockBudget::Affordable);
        assert!(
            pilot.score_column(&game, 6, &context).is_none(),
            "採点でも候補から外れるはず"
        );
    }

    /// 必要な余裕には上限を設ける。横移動が遅い/ブロック落下が速い設定では要求が
    /// 際限なく伸び、頭上に塊のある列がほぼ全て候補から外れてしまう(#225 F4)。
    #[test]
    fn the_required_slack_is_capped_so_slow_settings_do_not_reject_every_column() {
        let mut game = game_at(48, 999, 5);
        game.set_move_cooldown_ms(120);
        game.set_block_fall_tick_ms(75);
        assert!(
            min_slack_rows(&game) <= AUTOPLAY_THREAT_MAX_SLACK_ROWS,
            "上限を超えないはず: {}",
            min_slack_rows(&game)
        );
        assert!(min_slack_rows(&game) >= AUTOPLAY_THREAT_MIN_SLACK_ROWS);
    }

    /// こちらが掘り込んで初めて支えを失う塊には、揺れの猶予を丸ごと見込む(#225)。
    /// ここを0にしていたことが酸素切れ死の最大の原因だった。
    #[test]
    fn a_block_that_only_loses_support_when_we_dig_in_still_gets_its_full_shake_grace() {
        let mut game = game_at(49, 500, 5);
        game.board.rows[500][6] = Cell::Color(ColorKind::Red); // 掘って入るマス(=支え)
        game.board.rows[499][6] = Cell::Color(ColorKind::Blue); // その上に乗る塊
        let pilot = Autopilot::new(false);

        let threat = pilot
            .column_threat(&game, 6, 500)
            .expect("掘れば支えが消えるので脅威として見える");
        assert!(threat.pending, "まだ支えられている=pending");
        assert_eq!(
            shake_allowance_ms(&game, &threat),
            game.shake_duration_ms(),
            "揺れ猶予を丸ごと見込むはず"
        );

        // 既に浮いている塊は、こちらの掘削と無関係に落ちてくるので猶予は無い。
        let mut falling = game_at(49, 500, 5);
        falling.board.rows[499][6] = Cell::Color(ColorKind::Blue);
        let threat = pilot
            .column_threat(&falling, 6, 500)
            .expect("浮いた塊は脅威");
        assert!(!threat.pending);
        assert_eq!(shake_allowance_ms(&falling, &threat), 0);
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

    // --- アイテムの価値連動加点(#229 F4) -----------------------------------

    /// スター化アイテム(K)は、先読み範囲に岩が1つも無ければ寄り道の価値が無い
    /// (#229 F4)。以前は盤面に関わらず一律+10だったため、岩0個でも寄っていた。
    #[test]
    fn a_starify_item_is_worthless_when_there_is_no_rock_to_turn_into_a_star() {
        let mut game = solid_floor_game_at(70, 500, 5);
        game.board.rows[501][5] = Cell::Color(ColorKind::Red); // 直下は掘れる
        game.board.rows[503][8] = Cell::Item(ItemEffect::StarifyScreen); // 3列右
        let pilot = Autopilot::new(false);
        let context = pilot.score_context(&game, RockBudget::Affordable);
        assert_eq!(
            context.item_starify, 0.0,
            "先読み範囲に岩が無いので加点しないはず"
        );

        let mut pilot = Autopilot::new(false);
        assert_eq!(
            pilot.decide_with_intent(&game).0,
            Intent::DigDown,
            "寄り道せず掘り下げるはず"
        );
    }

    /// 同じ盤面でも、先読み範囲に岩があればスター化アイテムは寄り道に見合う(#229 F4)。
    #[test]
    fn a_starify_item_is_worth_a_detour_once_there_are_rocks_ahead() {
        let mut game = solid_floor_game_at(71, 500, 5);
        game.board.rows[501][5] = Cell::Color(ColorKind::Red);
        game.board.rows[503][8] = Cell::Item(ItemEffect::StarifyScreen);
        // 先読み範囲(現在列±4列)の左側に岩を並べる。経路にも目的列にも掛からない
        // ので、寄り道の判断だけがアイテムの価値で変わる。
        for row in 502..=505 {
            for col in 1..=3 {
                game.board.rows[row][col] = Cell::Rock { hits: 0 };
            }
        }
        let pilot = Autopilot::new(false);
        let context = pilot.score_context(&game, RockBudget::Affordable);
        assert_eq!(
            context.item_starify, AUTOPLAY_SCORE_ITEM_BONUS,
            "岩が飽和数以上あるので上限まで加点するはず"
        );

        let mut pilot = Autopilot::new(false);
        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::Sidestep);
        assert_eq!(actions, vec![InputAction::MoveRight], "アイテムへ寄るはず");
    }

    /// 頭上クリアアイテム(R)の加点は、頭上の脅威の数に連動させず上限のままにする
    /// (#229 F4)。連動させる案は実測で成績が落ちた(取得率35%→20%・1000mコースの
    /// 完走11/32→5/32・酸素切れ2.94→3.34回/走)ため採らなかった、という判断を
    /// 定数に固定しておく。
    #[test]
    fn a_clear_above_item_keeps_its_full_bonus_regardless_of_what_hangs_overhead() {
        let mut game = solid_floor_game_at(72, 500, 5);
        game.board.rows[501][5] = Cell::Color(ColorKind::Red);
        game.board.rows[503][8] = Cell::Item(ItemEffect::ClearAbove);
        let pilot = Autopilot::new(false);
        let context = pilot.score_context(&game, RockBudget::Affordable);
        assert_eq!(context.item_clear_above, AUTOPLAY_SCORE_ITEM_BONUS);

        let mut pilot = Autopilot::new(false);
        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::Sidestep);
        assert_eq!(actions, vec![InputAction::MoveRight], "アイテムへ寄るはず");
    }

    // --- ボムからの段差登り(#229 F3) ---------------------------------------

    /// 手組み盤面を実際に動かし、`done`が成立するかフレーム上限に達するまで進める。
    /// 発生したイベントをすべて返す(「岩を割らずに済んだか」の検証に使う)。
    fn run_until(
        game: &mut Game,
        pilot: &mut Autopilot,
        max_frames: u32,
        done: impl Fn(&Game) -> bool,
    ) -> Vec<GameEvent> {
        let delta = std::time::Duration::from_millis(FRAME_INTERVAL_MS);
        let mut events = Vec::new();
        for _ in 0..max_frames {
            if done(game) {
                break;
            }
            for action in pilot.decide(game) {
                events.extend(game.apply_input(action));
            }
            events.extend(game.update(delta));
        }
        events
    }

    /// 同じ行のボムから逃げるとき、直下が岩なら「掘って行を変える」は酸素20%の
    /// 買い物になる。登って行を変えられるならそちらが先(#229 F3)。
    #[test]
    fn decide_climbs_away_from_a_bomb_instead_of_paying_for_the_rock_below() {
        let mut game = solid_floor_game_at(66, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 };
        game.board.rows[500][6] = Cell::Diamond; // 右の足がかり(1段上は空き)
        game.bombs_mut().push(Bomb {
            pos: (500, 1),
            origin: (500, 0),
            phase: BombPhase::Ticking,
            phase_elapsed_ms: 0,
            remaining_ms: AUTOPLAY_BOMB_EVADE_MS - 1,
            settle_bounce_dir: 1,
        });
        let oxygen_before = game.player.oxygen;
        let mut pilot = Autopilot::new(false);

        let (intent, actions) = pilot.decide_with_intent(&game);
        assert_eq!(intent, Intent::ClimbOver);
        assert_eq!(actions, vec![InputAction::MoveRight]);

        let events = run_until(&mut game, &mut pilot, 60, |g| g.player.row == 499);
        assert_eq!(game.player.position(), (499, 6));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, GameEvent::RockDestroyed { .. })),
            "岩を割らずに行を変えるはず: {events:?}"
        );
        assert!(
            oxygen_before - game.player.oxygen < ROCK_BREAK_OXYGEN_PENALTY,
            "岩1個ぶんの酸素を払っている: {} → {}",
            oxygen_before,
            game.player.oxygen
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
                shaking: false,
                // 今はAIRに支えられており、AIRを取った瞬間に支えを失う=pending
                pending: true
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
                shaking: false,
                // 既に浮いている(こちらの掘削とは無関係に落ちてくる)
                pending: false
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
                shaking: false,
                pending: true
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

    // --- 段差登りの棚ゲート(#229 F1) ----------------------------------------

    /// 両隣が固体で、その1段上も塞がっている行き止まり(=どちら側にも棚が無い)。
    fn walled_dead_end(seed: u64) -> Game {
        let mut game = solid_floor_game_at(seed, 500, 5);
        game.board.rows[501][5] = Cell::Rock { hits: 0 }; // 直下は岩
        for col in [4, 6] {
            game.board.rows[500][col] = Cell::Rock { hits: 0 }; // 両隣も岩
            game.board.rows[499][col] = Cell::Rock { hits: 0 }; // 斜め上も塞がる=棚が無い
        }
        game
    }

    /// 棚が無い壁に対しては段差登りを出さない(#229 F1)。以前は`side_preference`側に
    /// 盤面が続いてさえいれば`MoveX`を出し続けていたため、登れない壁へ最大510フレーム
    /// (17秒)ぶつかり続け、その間の自然減少だけで窒息していた。
    #[test]
    fn a_dead_end_without_a_ledge_never_emits_the_escape_climb() {
        let game = walled_dead_end(60);
        let mut pilot = Autopilot::new(false);
        for _ in 0..stuck_frames_to_escalate() * 3 {
            pilot.decide(&game);
        }
        assert_eq!(pilot.escalation, 3, "前提: 手詰まりが極まっている");
        assert!(
            pilot.climb_direction(&game).is_none(),
            "前提: どちら側にも登り先の棚が無い"
        );

        for frame in 0..600 {
            assert_ne!(
                pilot.decide_with_intent(&game).0,
                Intent::EscapeClimb,
                "frame={frame}: 登れない壁に向かって登ろうとしている"
            );
        }
    }

    /// 棚が無く、岩を割ると危険域に入るが致死圏ではない残量。壁にぶつかり続けて
    /// 窒息するのではなく、代償を払って岩を割る判断へ進む(#229 F1)。
    #[test]
    fn a_dead_end_without_a_ledge_pays_for_the_rock_rather_than_stalling_until_it_suffocates() {
        let mut game = walled_dead_end(61);
        game.player.oxygen =
            oxygen_reserve(game.player.depth_m()) + ROCK_BREAK_OXYGEN_PENALTY + 1.0;
        assert!(!rock_is_affordable(&game), "前提: 割ると危険域へ落ちる残量");
        assert!(!rock_break_is_lethal(&game), "前提: ただし致死圏ではない");
        let mut pilot = Autopilot::new(false);

        let deadline_frames = (1500 / FRAME_INTERVAL_MS) as u32;
        let mut paid_for_a_rock = false;
        for frame in 0..=deadline_frames {
            let (intent, _) = pilot.decide_with_intent(&game);
            assert_ne!(
                intent,
                Intent::EscapeClimb,
                "frame={frame}: 登れない壁に向かって登ろうとしている"
            );
            if matches!(intent, Intent::BreakRock | Intent::DigSideways) {
                paid_for_a_rock = true;
                break;
            }
        }
        assert!(paid_for_a_rock, "1.5秒以内に岩を割る判断へ進むはず");
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

    /// ソーク1本ぶんの条件(コース・盤面設定)。実機と同じ`Game::apply_settings`を
    /// 通すため、`Settings`をそのまま持つ(#225)。以前は`Game::new_with_width`直後の
    /// 盤面をそのまま使っていたため、既定設定の計測ですら出現率の再抽選前の盤面を
    /// 測っていた。
    struct Profile {
        width: usize,
        depth_goal_m: usize,
        settings: Settings,
    }

    impl Profile {
        /// 既定設定のコース。
        fn default_course(width: usize, depth_goal_m: usize) -> Self {
            Profile {
                width,
                depth_goal_m,
                settings: Settings {
                    field_width: width,
                    ..Settings::default()
                },
            }
        }

        /// ユーザーが実際に使っている設定(#225の調査で`settings.json`から採取)。
        /// 幅が広く・AIRが少なく・岩とスターが多く・ブロックの落下が速く・横移動が遅い、
        /// 既定よりかなり厳しい条件。
        fn harsh(depth_goal_m: usize) -> Self {
            const WIDTH: usize = 20;
            Profile {
                width: WIDTH,
                depth_goal_m,
                settings: Settings {
                    field_width: WIDTH,
                    air_spawn_rate_percent: 40,
                    rock_spawn_rate_percent: 300,
                    star_spawn_rate_percent: 6000,
                    diamond_spawn_rate_percent: 300,
                    block_fall_tick_ms: 75,
                    player_fall_tick_ms: 100,
                    shake_duration_ms: 2000,
                    move_cooldown_ms: 120,
                    ..Settings::default()
                },
            }
        }
    }

    /// GameOverになったときの扱い。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum RevivePolicy {
        /// 通常のライフ予算内での結果を測る(実機で人間がプレイするのと同じ)。
        Never,
        /// 到達可能性だけを見るため、何度でも復活させる。完走率の意味は持たない。
        Unlimited,
    }

    /// 窓に入ったAIR/アイテムが、そのとき判断からどう見えていたか(#229)。
    ///
    /// 「見えていたのに取らなかった」の原因を、落下コミット(横移動が効かない)・
    /// 列の却下(候補にすら上がらない)・同一行の死角(採点が現在行より下しか見ない)の
    /// どれかに切り分けるための区分。宣言順がそのまま優先順(下ほど「寄れたはず」)で、
    /// 同じマスが窓にいた全フレームでの最大値を採る。
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    enum PassState {
        /// 自由落下中にしか見えていない(横移動が効かず、寄る手段が無かった)
        SeenFallingOnly,
        /// 候補から却下された列にあった
        RejectedColumn,
        /// プレイヤーと同じ行にあった
        SameRow,
        /// 採点候補に残った列にあった
        Candidate,
        /// そのフレームの目的列にあった
        Targeted,
    }

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

        // --- 以下は「賢さ」の計測(#229) ---
        /// 意図別のフレーム数。
        intent_frames: [u32; Intent::COUNT],
        /// 段差登りを出したが、その方向に登り先の棚が無かったフレーム数。F1の適用後は
        /// 常に0になるはず(壁にぶつかり続けて窒息していた分)。
        escape_climb_no_ledge_frames: u32,
        /// 段差登りが連続したフレーム数の最大値。
        escape_climb_max_streak: u32,
        /// その場待ち(`WaitOut`)のフレーム数。
        wait_out_frames: u32,
        /// 行が進まないまま累積した`WaitOut`フレーム数の最大値。待機上限
        /// (`AUTOPLAY_WAIT_FOR_THREAT_MAX_MS`)が実際に効いているかを見る。
        wait_out_max_budget_frames: u32,
        /// 前フレームより行が浅くなった回数(段差登りが成立した回数)。
        climbs: u32,
        /// AIRの取得(フレーム, 取得直前の残量, 深度m)。
        air_pickups: Vec<(u32, f32, usize)>,
        /// 取らずに通り過ぎたAIR(通過時の残量, 横距離, 見え方)。
        air_passes: Vec<(f32, i32, PassState)>,
        /// アイテムの取得(効果, フレーム, 残量, 先読み窓の岩の数, 頭上の不安定な塊の数)。
        item_pickups: Vec<(ItemEffect, u32, f32, usize, usize)>,
        /// 取らずに通り過ぎたアイテム(効果, 横距離, 見え方)。
        item_passes: Vec<(ItemEffect, i32, PassState)>,
        /// 頭上からの落下を横/下へ避けた回数。
        dodge_events: u32,
        /// そのうち、安全に登れる棚があった回数(登りを選択肢にする価値の目安)。
        dodge_with_safe_ledge: u32,
        /// 押し潰し死のうち、直前`LEDGE_REVIEW_FRAMES`フレームに安全な棚があった回数。
        crush_deaths_with_recent_safe_ledge: u32,
        /// 酸素切れ死のうち、直前`DEATH_REVIEW_FRAMES`フレームの過半を
        /// 「登り・待ち・自由落下待ち」で費やしていた回数(停滞して窒息した死に方)。
        stalled_oxygen_deaths: u32,
        /// 酸素が警告域・緊急域だったフレーム数。
        frames_warning: u32,
        frames_emergency: u32,
        /// 走中の酸素の最小値。
        min_oxygen: f32,
    }

    impl SoakResult {
        fn new() -> Self {
            SoakResult {
                cleared: false,
                frames: 0,
                deepest_m: 0,
                rocks_drilled: 0,
                deaths: Vec::new(),
                event_log: Vec::new(),
                intent_frames: [0; Intent::COUNT],
                escape_climb_no_ledge_frames: 0,
                escape_climb_max_streak: 0,
                wait_out_frames: 0,
                wait_out_max_budget_frames: 0,
                climbs: 0,
                air_pickups: Vec::new(),
                air_passes: Vec::new(),
                item_pickups: Vec::new(),
                item_passes: Vec::new(),
                dodge_events: 0,
                dodge_with_safe_ledge: 0,
                crush_deaths_with_recent_safe_ledge: 0,
                stalled_oxygen_deaths: 0,
                frames_warning: 0,
                frames_emergency: 0,
                min_oxygen: OXYGEN_MAX,
            }
        }

        fn unharmed(&self) -> bool {
            self.cleared && self.deaths.is_empty()
        }

        /// 原因別の死亡回数。
        fn deaths_by_cause(&self, cause: MissCause) -> usize {
            self.deaths.iter().filter(|c| **c == cause).count()
        }

        /// 残量が`OXYGEN_MAX`の半分以下の状態で、寄れたはず(候補列・目的列)のAIRを
        /// 取らずに通り過ぎた回数(#229 G3)。`only_targeted`にすると、そのとき実際に
        /// 目指していた列にあったものだけを数える(候補列は最大9列あり、窓に入った
        /// だけで「寄れたはず」と数えると実態より大きく出るため、両方を記録する)。
        fn missed_air_while_low(&self, only_targeted: bool) -> usize {
            self.air_passes
                .iter()
                .filter(|(oxygen, _, state)| {
                    *oxygen <= OXYGEN_MAX / 2.0
                        && if only_targeted {
                            *state == PassState::Targeted
                        } else {
                            matches!(state, PassState::Candidate | PassState::Targeted)
                        }
                })
                .count()
        }

        /// アイテム効果別の(取得数, 通過数)。
        fn item_take_rate(&self, effect: ItemEffect) -> (usize, usize) {
            (
                self.item_pickups.iter().filter(|p| p.0 == effect).count(),
                self.item_passes.iter().filter(|p| p.0 == effect).count(),
            )
        }
    }

    /// 死因を分析するとき、死の直前どれだけ遡って意図の内訳を見るか(#229 G2)。
    const DEATH_REVIEW_FRAMES: usize = 300;
    /// 押し潰し死の直前、安全な棚があったかを遡って見るフレーム数(#229 G5)。
    const LEDGE_REVIEW_FRAMES: usize = 15;

    /// 窓で見かけたAIR/アイテム1マスぶんの記録(#229)。
    struct Sighting {
        /// AIRなら`None`、アイテムならその効果。
        item: Option<ItemEffect>,
        /// これまでで最も良かった見え方。
        state: PassState,
        /// 最後に見たときの横距離(負=左)。
        lateral_dist: i32,
    }

    /// そのフレームの判断直前(入力を当てる前)の状況(#229)。取得イベントが飛んできた
    /// ときに「どれだけ価値のある状況で取れたか」を後から言えるようにする。
    struct FrameSnapshot {
        frame: u32,
        oxygen: f32,
        depth_m: usize,
        rocks_in_lookahead: usize,
        unstable_above: usize,
    }

    /// ソーク1走ぶんの計測器(#229)。`play`が毎フレーム`observe`を呼ぶ。
    ///
    /// 判断の内部状態(`escalation`/`target_col`/採点結果)と盤面を突き合わせて、
    /// 「その手を選んだとき他に何が見えていたか」まで残すのが目的。完走率や死因だけでは
    /// 「なぜ寄らなかったのか」「なぜ登らなかったのか」が分からず、実際にF1の空振りを
    /// 見落としていた。
    struct Telemetry {
        sightings: HashMap<(usize, usize), Sighting>,
        /// 直近`DEATH_REVIEW_FRAMES`フレームが停滞していたか(死因分析用)。
        recent_stalls: VecDeque<bool>,
        /// 直近`LEDGE_REVIEW_FRAMES`フレームで安全に登れる棚があったか。
        recent_safe_ledge: VecDeque<bool>,
        escape_climb_streak: u32,
        wait_out_budget: u32,
        deepest_row: usize,
        previous_row: usize,
    }

    impl Telemetry {
        fn new(start_row: usize) -> Self {
            Telemetry {
                sightings: HashMap::new(),
                recent_stalls: VecDeque::with_capacity(DEATH_REVIEW_FRAMES),
                recent_safe_ledge: VecDeque::with_capacity(LEDGE_REVIEW_FRAMES),
                escape_climb_streak: 0,
                wait_out_budget: 0,
                deepest_row: start_row,
                previous_row: start_row,
            }
        }

        /// 1フレームぶんの観測。判断の直後・入力を当てる前に呼ぶ。
        fn observe(
            &mut self,
            game: &Game,
            pilot: &Autopilot,
            intent: Intent,
            actions: &[InputAction],
            result: &mut SoakResult,
        ) -> FrameSnapshot {
            let row = game.player.row;
            result.intent_frames[intent.index()] += 1;
            result.min_oxygen = result.min_oxygen.min(game.player.oxygen);
            if game.player.oxygen < OXYGEN_WARNING_THRESHOLD {
                result.frames_warning += 1;
            }
            if is_emergency(game) {
                result.frames_emergency += 1;
            }

            // 自由落下の待ち(Idle)は下へ進んでいるので停滞ではない。地に足が着いた
            // まま何も進んでいないフレームだけを停滞として数える(#229 G2)。
            let stalled = matches!(intent, Intent::EscapeClimb | Intent::WaitOut)
                || (intent == Intent::Idle && game.player_is_grounded());
            push_bounded(&mut self.recent_stalls, stalled, DEATH_REVIEW_FRAMES);
            let safe_ledge = pilot.safe_climb_direction(game).is_some();
            push_bounded(&mut self.recent_safe_ledge, safe_ledge, LEDGE_REVIEW_FRAMES);

            if intent == Intent::EscapeClimb {
                self.escape_climb_streak += 1;
                result.escape_climb_max_streak =
                    result.escape_climb_max_streak.max(self.escape_climb_streak);
                // 出した方向に本当に棚があるか(F1が塞いだ空振りの検出)。
                let has_ledge =
                    lateral_of(actions).is_some_and(|dir| pilot.can_climb_step(game, dir));
                if !has_ledge {
                    result.escape_climb_no_ledge_frames += 1;
                }
            } else {
                self.escape_climb_streak = 0;
            }

            if intent == Intent::DodgeOverhead {
                result.dodge_events += 1;
                if safe_ledge {
                    result.dodge_with_safe_ledge += 1;
                }
            }

            if row > self.deepest_row {
                self.deepest_row = row;
                self.wait_out_budget = 0;
            }
            if intent == Intent::WaitOut {
                result.wait_out_frames += 1;
                self.wait_out_budget += 1;
                result.wait_out_max_budget_frames =
                    result.wait_out_max_budget_frames.max(self.wait_out_budget);
            }
            if row < self.previous_row {
                result.climbs += 1;
            }
            self.previous_row = row;

            self.watch_window(game, pilot);
            self.collect_passes(game, result);

            FrameSnapshot {
                frame: result.frames,
                oxygen: game.player.oxygen,
                depth_m: game.player.depth_m(),
                rocks_in_lookahead: rocks_in_lookahead(game),
                unstable_above: unstable_mass_above(game),
            }
        }

        /// 窓(現在列±`AUTOPLAY_COLUMN_SCAN_RADIUS`列 × 現在行から下`AUTOPLAY_LOOKAHEAD_ROWS`行)
        /// のAIR/アイテムを、そのフレームの見え方とともに覚える。
        fn watch_window(&mut self, game: &Game, pilot: &Autopilot) {
            let (row, col) = game.player.position();
            let grounded = game.player_is_grounded();
            let candidates: Vec<usize> = pilot.score_columns(game).iter().map(|s| s.col).collect();
            let lo = col.saturating_sub(AUTOPLAY_COLUMN_SCAN_RADIUS);
            let hi = (col + AUTOPLAY_COLUMN_SCAN_RADIUS).min(game.board.width() - 1);

            for c in lo..=hi {
                let column_state = if !grounded {
                    // 落下中は横移動そのものが通らない。列の良し悪し以前に寄れない。
                    PassState::SeenFallingOnly
                } else if pilot.target_col == Some(c) {
                    PassState::Targeted
                } else if candidates.contains(&c) {
                    PassState::Candidate
                } else {
                    PassState::RejectedColumn
                };
                for d in 0..=AUTOPLAY_LOOKAHEAD_ROWS {
                    let Some(cell) = game.board.cell_or_none(row + d, c) else {
                        break;
                    };
                    let item = match cell {
                        Cell::Oxygen => None,
                        Cell::Item(effect) => Some(effect),
                        _ => continue,
                    };
                    let state = if d == 0 {
                        column_state.max(PassState::SameRow)
                    } else {
                        column_state
                    };
                    let lateral_dist = c as i32 - col as i32;
                    self.sightings
                        .entry((row + d, c))
                        .and_modify(|seen| {
                            seen.state = seen.state.max(state);
                            seen.lateral_dist = lateral_dist;
                        })
                        .or_insert(Sighting {
                            item,
                            state,
                            lateral_dist,
                        });
                }
            }
        }

        /// プレイヤーより上になったマスを「通過」として確定する。通過した時点でまだ
        /// 盤面に残っていたものだけが「取らなかった」1件になる。
        fn collect_passes(&mut self, game: &Game, result: &mut SoakResult) {
            let row = game.player.row;
            self.sightings.retain(|&(cell_row, cell_col), seen| {
                if cell_row >= row {
                    return true;
                }
                let still_there = match seen.item {
                    None => game.board.cell(cell_row, cell_col) == Cell::Oxygen,
                    Some(effect) => game.board.cell(cell_row, cell_col) == Cell::Item(effect),
                };
                if still_there {
                    match seen.item {
                        None => {
                            result.air_passes.push((
                                game.player.oxygen,
                                seen.lateral_dist,
                                seen.state,
                            ));
                        }
                        Some(effect) => {
                            result
                                .item_passes
                                .push((effect, seen.lateral_dist, seen.state));
                        }
                    }
                }
                false
            });
        }

        /// 直近`DEATH_REVIEW_FRAMES`フレームのうち、停滞(登り・待ち・接地したまま
        /// 何もしない)が過半を占めていたか。
        fn recently_stalled(&self) -> bool {
            if self.recent_stalls.is_empty() {
                return false;
            }
            let stalled = self
                .recent_stalls
                .iter()
                .filter(|stalled| **stalled)
                .count();
            stalled * 2 > self.recent_stalls.len()
        }

        /// 直近`LEDGE_REVIEW_FRAMES`フレームのどこかで安全に登れる棚があったか。
        fn had_safe_ledge_recently(&self) -> bool {
            self.recent_safe_ledge.iter().any(|had| *had)
        }
    }

    /// 固定長のリングバッファとして`VecDeque`へ積む。
    fn push_bounded<T>(queue: &mut VecDeque<T>, value: T, capacity: usize) {
        if queue.len() == capacity {
            queue.pop_front();
        }
        queue.push_back(value);
    }

    /// 窓(現在列±`AUTOPLAY_COLUMN_SCAN_RADIUS`列 × 下`AUTOPLAY_LOOKAHEAD_ROWS`行)にある
    /// 岩の数。スター化アイテムがどれだけ役に立つ場面だったかの目安。
    fn rocks_in_lookahead(game: &Game) -> usize {
        let (row, col) = game.player.position();
        let lo = col.saturating_sub(AUTOPLAY_COLUMN_SCAN_RADIUS);
        let hi = (col + AUTOPLAY_COLUMN_SCAN_RADIUS).min(game.board.width() - 1);
        (lo..=hi)
            .flat_map(|c| (1..=AUTOPLAY_LOOKAHEAD_ROWS).map(move |d| (row + d, c)))
            .filter(|&(r, c)| matches!(game.board.cell_or_none(r, c), Some(Cell::Rock { .. })))
            .count()
    }

    /// 頭上`AUTOPLAY_THREAT_SCAN_ROWS`行の窓にある「直下に穴が空いた固体」の数。
    /// 頭上クリアアイテムがどれだけ役に立つ場面だったかの目安で、塊の支持関係まで
    /// 追う正確な判定(`column_threat`)ではなく1マスだけ見る安価な近似。
    fn unstable_mass_above(game: &Game) -> usize {
        let (row, col) = game.player.position();
        let lo = col.saturating_sub(AUTOPLAY_COLUMN_SCAN_RADIUS);
        let hi = (col + AUTOPLAY_COLUMN_SCAN_RADIUS).min(game.board.width() - 1);
        (lo..=hi)
            .flat_map(|c| (1..=AUTOPLAY_THREAT_SCAN_ROWS).map(move |d| (row.checked_sub(d), c)))
            .filter_map(|(r, c)| r.map(|r| (r, c)))
            .filter(|&(r, c)| {
                let solid = !matches!(
                    game.board.cell(r, c),
                    Cell::Empty | Cell::Oxygen | Cell::Item(_)
                );
                solid
                    && matches!(
                        game.board.cell_or_none(r + 1, c),
                        Some(Cell::Empty | Cell::Oxygen | Cell::Item(_))
                    )
            })
            .count()
    }

    /// 無敵OFFのオートプレイで1本通しプレイし、結果を集計する。main.rsのメインループと
    /// 同じ順序(判断→入力→update)で、かつ`start_new_game`と同じ設定反映
    /// (`Game::apply_settings`)を通して回す。
    fn play(seed: u64, profile: &Profile, revive: RevivePolicy, max_frames: u32) -> SoakResult {
        let mut game = Game::new_with_width(seed, profile.width, profile.depth_goal_m);
        game.apply_settings(&profile.settings);
        let mut pilot = Autopilot::new(false);
        let delta = std::time::Duration::from_millis(FRAME_INTERVAL_MS);
        let mut result = SoakResult::new();
        let mut telemetry = Telemetry::new(game.player.row);

        for frame in 0..max_frames {
            result.frames = frame + 1;
            let (intent, actions) = pilot.decide_with_intent(&game);
            let snapshot = telemetry.observe(&game, &pilot, intent, &actions, &mut result);
            for action in actions {
                let events = game.apply_input(action);
                for event in &events {
                    // 掘削で壊した岩だけを数える。着地での4連結自動消滅はupdate側で
                    // 起きるため、ここには混ざらない。
                    if let GameEvent::RockDestroyed { blocks } = event {
                        result.rocks_drilled += blocks;
                    }
                }
                record(&mut result, frame, &events, &snapshot, &telemetry);
            }
            let events = game.update(delta);
            record(&mut result, frame, &events, &snapshot, &telemetry);

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
            if game.status == GameStatus::GameOver {
                match revive {
                    RevivePolicy::Never => break,
                    // オートプレイはもうRevive意図を返さないため、ハーネス側で呼ぶ。
                    RevivePolicy::Unlimited => game.revive(),
                }
            }
        }
        result
    }

    fn record(
        result: &mut SoakResult,
        frame: u32,
        events: &[GameEvent],
        snapshot: &FrameSnapshot,
        telemetry: &Telemetry,
    ) {
        for event in events {
            match event {
                GameEvent::LifeLost { cause } | GameEvent::GameOverMiss { cause } => {
                    result.deaths.push(*cause);
                    // 死んだ瞬間の「直前に何をしていたか」は、死因の内訳だけでは
                    // 見えない詰まり方(壁にぶつかり続けて窒息する等)を捕まえる(#229)。
                    match cause {
                        MissCause::OxygenOut if telemetry.recently_stalled() => {
                            result.stalled_oxygen_deaths += 1;
                        }
                        MissCause::CrushedByFallingBlock if telemetry.had_safe_ledge_recently() => {
                            result.crush_deaths_with_recent_safe_ledge += 1;
                        }
                        _ => {}
                    }
                }
                GameEvent::OxygenCollected => {
                    result
                        .air_pickups
                        .push((snapshot.frame, snapshot.oxygen, snapshot.depth_m));
                }
                GameEvent::ItemCollected(effect) => {
                    result.item_pickups.push((
                        *effect,
                        snapshot.frame,
                        snapshot.oxygen,
                        snapshot.rocks_in_lookahead,
                        snapshot.unstable_above,
                    ));
                }
                _ => {}
            }
            result.event_log.push((frame, *event));
        }
    }

    /// 同じシード・同じ入力列なら、盤面生成もボム抽選もアイテム補充も完全に再現される
    /// こと(T4)。`Board`の再抽選が呼び出しごとにOS乱数から作り直していた頃は、同じ
    /// シードでも結果が揺れてソークテストの失敗を再現できなかった(#221で修正)。
    #[test]
    fn replaying_the_same_seed_reproduces_the_run_exactly() {
        let profile = Profile::default_course(FIELD_WIDTH_DEFAULT, 300);
        let first = play(4242, &profile, RevivePolicy::Never, 30_000);
        let second = play(4242, &profile, RevivePolicy::Never, 30_000);

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
                game.apply_input(action);
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

    /// 300m・幅12(既定設定)を無敵OFF・通常のライフ予算(自動Revive無し)で16シード走らせ、
    /// 設計が置いた品質基準を満たすことを確認する(全シード完走・酸素切れゼロ・
    /// 無傷完走5割・完走時の岩破壊15回以下)。
    ///
    /// 実測値(#225時点): 完走16/16・無傷完走11/16・酸素切れ0・完走時の平均岩破壊2.4回・
    /// 1走あたりの死亡0.38回。(#229時点): 完走16/16・無傷完走10/16・酸素切れ0・
    /// 平均岩破壊2.4回・1走あたりの死亡0.44回(押し潰され0.25回・爆風0.19回)。
    /// このコースは浅く、手詰まりも酸素の逼迫もほぼ起きない(待ち0フレーム・
    /// 段差登り0回)ため、#229の修正はここでは数字に出ない。
    #[test]
    fn soak_short_course_within_life_budget() {
        const SEEDS: u64 = 16;
        let profile = Profile::default_course(FIELD_WIDTH_DEFAULT, 300);
        let results: Vec<(u64, SoakResult)> = (0..SEEDS)
            .map(|seed| (seed, play(seed, &profile, RevivePolicy::Never, 30_000)))
            .collect();
        let summary = Summary::of(&results);
        summary.print("300m(既定・ライフ予算内)", SEEDS);
        assert_never_stalls(&summary);

        assert!(
            summary.out_of_oxygen.is_empty(),
            "酸素切れで死んだシードがある: {:?}",
            summary.out_of_oxygen
        );
        assert_eq!(
            summary.cleared, SEEDS as usize,
            "通常のライフ予算内で全シード完走できるはず: {}/{SEEDS}",
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

    /// フルコース(1000m・幅12・既定設定)を無敵OFF・通常のライフ予算で32シード走らせる
    /// 重量級のソーク(T2)。
    ///
    /// 深い場所は浅い場所と質が違う。ブロックの落下tickは深度で最大2.5倍まで短くなるのに
    /// プレイヤーの自由落下tickは一定なので、最深帯ではブロックの方が2.5倍速く落ちる。
    /// 落下中は横移動が効かないため、空洞へ入った時点で頭上の塊に追いつかれる状況が
    /// 構造的に発生する。岩の出現率・酸素の減少速度も深いほど上がる。
    ///
    /// 押し潰され回数の上限は、ユーザー判断「押し潰し死を一切増やさない」を機械的に
    /// 検証するためのもの(#225)。基準は**このハーネスで測った修正前の実測値1.78回/走**に
    /// 置く。設計メモにある3.22回/走は`Game::apply_settings`を通さない旧ハーネスでの値で、
    /// 実機の盤面(出現率の再抽選後は岩が12.1%→15.8%に増える)とは条件が違うため使わない。
    ///
    /// #229では**採否の基準を「1走あたりの死亡総数」と「平均到達深度」に変えた**
    /// (ユーザー判断)。このコースでは「岩を割る回数」と「押し潰される回数」がほぼ1対1で
    /// 入れ替わり、死因単体では改善を判定できないため。死因ごとの上限は歯止めとして残す。
    ///
    /// 実測値(いずれも同じハーネス・32シード):
    /// - #225修正前: 完走6/32・平均到達871m・押し潰され1.78回/酸素切れ3.06回・
    ///   完走時の平均岩破壊50.8回
    /// - #225修正後: 完走8/32・平均到達859m・押し潰され1.47回/酸素切れ3.44回・
    ///   死亡5.16回/走・完走時の平均岩破壊43.4回
    /// - #229修正後: 完走7/32・平均到達879m・押し潰され1.66回/酸素切れ3.19回・
    ///   死亡5.06回/走・完走時の平均岩破壊44.9回
    ///
    /// 調整に使っていないシード(100〜163の64本)でも確認している: 完走8/64・
    /// 平均到達850m・死亡5.25回/走。完走率が32シードの値より低いのは、この構成の
    /// 完走率自体が2割前後で、シードの当たり外れの幅が大きいため。
    #[test]
    #[ignore = "長時間のソークテスト。cargo test --release -- --ignored で実行する"]
    fn soak_full_course_within_life_budget() {
        const SEEDS: u64 = 32;
        let profile = Profile::default_course(FIELD_WIDTH_DEFAULT, 1000);
        let results: Vec<(u64, SoakResult)> = (0..SEEDS)
            .map(|seed| (seed, play(seed, &profile, RevivePolicy::Never, 120_000)))
            .collect();
        let summary = Summary::of(&results);
        summary.print("1000m(既定・ライフ予算内)", SEEDS);
        assert_never_stalls(&summary);

        // #229の採否基準: 死亡総数と到達深度。どちらも#225時点の実測を下回らないこと。
        assert!(
            summary.deaths_per_run() <= 5.16,
            "1走あたりの死亡総数が#225時点の実測(5.16回/走)を上回っている: {:.2}回/走",
            summary.deaths_per_run()
        );
        assert!(
            summary.average_deepest_m >= 859.0,
            "平均到達深度が#225時点の実測(859m)を下回っている: {:.0}m",
            summary.average_deepest_m
        );
        // 死因ごとの歯止め。総数が同じでも特定の死に方へ偏っていないことを見る。
        assert!(
            summary.crush_deaths_per_run() <= 1.78,
            "押し潰し死が#225修正前の実測(1.78回/走)を上回っている: {:.2}回/走",
            summary.crush_deaths_per_run()
        );
        assert!(
            summary.cleared >= 6,
            "完走が#225修正前の実測(6/32)を下回っている: {}/{SEEDS}",
            summary.cleared
        );
        assert!(
            summary.oxygen_deaths_per_run() <= 4.0,
            "酸素切れ死が実測(3.44回/走)から大きく増えている: {:.2}回/走",
            summary.oxygen_deaths_per_run()
        );
        assert!(
            summary.average_rocks <= 55.0,
            "完走時の岩破壊が実測(43.4回)から大きく増えている: 平均{:.1}回",
            summary.average_rocks
        );
    }

    /// ユーザーが実際に使っている設定(`Profile::harsh`)で500mを32シード走らせる(#225)。
    /// 「AIR不足でめっちゃ死ぬ」という指摘の再現条件そのもの。
    ///
    /// 実測値(同じハーネス・32シード):
    /// - #225修正前: 完走2/32・平均到達423m・酸素切れ3.81回/走(死因の98%)・
    ///   押し潰され0.09回/走・完走時の平均岩破壊37.5回
    /// - #225修正後: 完走25/32・平均到達492m・酸素切れ0.81回/走・押し潰され1.28回/走・
    ///   死亡2.12回/走・完走時の平均岩破壊20.1回
    /// - #229修正後: 完走32/32・平均到達500m・酸素切れ0.91回/走・押し潰され0.88回/走・
    ///   死亡1.84回/走・完走時の平均岩破壊20.7回
    ///
    /// 押し潰されが0.09→1.28回/走へ増えて見えるのは、#225修正前は平均423mで酸素切れに
    /// なり潰される前に死んでいたため。#229では登れない壁への段差登り(最長17秒の空振り)を
    /// 塞いだのが効いていて、完走が25→32/32・死亡総数が2.12→1.84回/走になった。
    /// 調整に使っていないシード(100〜163の64本)でも完走56/64・死亡2.06回/走。
    #[test]
    #[ignore = "長時間のソークテスト。cargo test --release -- --ignored で実行する"]
    fn soak_harsh_profile_within_life_budget() {
        const SEEDS: u64 = 32;
        let profile = Profile::harsh(500);
        let results: Vec<(u64, SoakResult)> = (0..SEEDS)
            .map(|seed| (seed, play(seed, &profile, RevivePolicy::Never, 120_000)))
            .collect();
        let summary = Summary::of(&results);
        summary.print("500m(harsh・ライフ予算内)", SEEDS);
        assert_never_stalls(&summary);

        assert!(
            summary.cleared >= 28,
            "ユーザーの実設定での完走が#229時点の実測(32/32)から大きく落ちている: \
             {}/{SEEDS}",
            summary.cleared
        );
        assert!(
            summary.average_deepest_m >= 492.0,
            "平均到達深度が#225時点の実測(492m)を下回っている: {:.0}m",
            summary.average_deepest_m
        );
        assert!(
            summary.oxygen_deaths_per_run() <= 1.5,
            "酸素切れ死が#225修正前の実測(3.81回/走)から改善しきれていない: {:.2}回/走",
            summary.oxygen_deaths_per_run()
        );
        assert!(
            summary.deaths_per_run() <= 2.12,
            "1走あたりの死亡総数が#225時点の実測(2.12回/走)を上回っている: {:.2}回/走",
            summary.deaths_per_run()
        );
    }

    /// 何度でも復活させる前提で、フルコースの終端まで到達し切れること。
    ///
    /// **これは通常のライフ予算内での完走率を意味しない**(#225)。以前はこのテストを
    /// 「到達32/32」と読んでいたが、それは無制限リトライ込みの数字で、実際の
    /// ライフ予算内の完走率は当時10/32だった。ここで見るのはクラッシュ・不変条件違反の
    /// 不在と、いつかは終端へ到達できること(=永久に進めなくなる盤面が無いこと)だけ。
    /// ライフ予算内の品質は`soak_full_course_within_life_budget`が担当する。
    #[test]
    #[ignore = "長時間のソークテスト。cargo test --release -- --ignored で実行する"]
    fn soak_full_course_unlimited_revive_reaches_the_goal_eventually() {
        const SEEDS: u64 = 8;
        let profile = Profile::default_course(FIELD_WIDTH_DEFAULT, 1000);
        for seed in 0..SEEDS {
            let result = play(seed, &profile, RevivePolicy::Unlimited, 120_000);
            assert!(
                result.cleared,
                "seed={seed}: 無制限に復活できるなら終端まで到達できるはず\
                 (到達={}m, {}フレーム)",
                result.deepest_m, result.frames
            );
        }
    }

    /// ソーク結果の集計。
    struct Summary {
        seeds: usize,
        cleared: usize,
        unharmed: usize,
        out_of_oxygen: Vec<u64>,
        average_rocks: f64,
        average_frames: f64,
        average_deepest_m: f64,
        total_deaths: usize,
        /// 原因別の死亡回数(全シード合計)。
        deaths_by_cause: Vec<(MissCause, usize)>,

        // --- 以下は「賢さ」の計測(#229) ---
        total_frames: u64,
        intent_frames: [u64; Intent::COUNT],
        escape_climb_no_ledge_frames: u64,
        escape_climb_max_streak: u32,
        wait_out_frames: u64,
        wait_out_max_budget_frames: u32,
        climbs: u64,
        stalled_oxygen_deaths: u32,
        dodge_events: u64,
        dodge_with_safe_ledge: u64,
        crush_deaths_with_recent_safe_ledge: u32,
        air_pickups: usize,
        air_passes: usize,
        missed_air_while_low: usize,
        missed_air_while_low_targeted: usize,
        /// 効果別の(取得数, 通過数)。
        item_stats: Vec<(ItemEffect, usize, usize)>,
        /// スター化アイテムを取ったときの、先読み窓の岩の数の平均。
        starify_rocks_at_pickup: f64,
        /// 頭上クリアアイテムを取ったときの、頭上の不安定な塊の数の平均。
        clear_above_unstable_at_pickup: f64,
        frames_warning: u64,
        frames_emergency: u64,
        min_oxygen: f32,
    }

    impl Summary {
        fn of(results: &[(u64, SoakResult)]) -> Self {
            let rocks: Vec<usize> = results
                .iter()
                .filter(|(_, r)| r.cleared)
                .map(|(_, r)| r.rocks_drilled)
                .collect();
            let causes = [
                MissCause::OxygenOut,
                MissCause::CrushedByFallingBlock,
                MissCause::DrilledIntoFallingBlock,
                MissCause::BombBlast,
            ];
            let sum = |pick: fn(&SoakResult) -> u64| -> u64 {
                results.iter().map(|(_, r)| pick(r)).sum()
            };
            let mut intent_frames = [0u64; Intent::COUNT];
            for (_, r) in results {
                for (total, frames) in intent_frames.iter_mut().zip(r.intent_frames) {
                    *total += u64::from(frames);
                }
            }
            let item_stats = [
                ItemEffect::ClearAbove,
                ItemEffect::UnifyColors,
                ItemEffect::StarifyScreen,
            ]
            .into_iter()
            .map(|effect| {
                let (taken, passed) = results
                    .iter()
                    .map(|(_, r)| r.item_take_rate(effect))
                    .fold((0, 0), |acc, (t, p)| (acc.0 + t, acc.1 + p));
                (effect, taken, passed)
            })
            .collect();

            Summary {
                seeds: results.len(),
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
                average_deepest_m: results.iter().map(|(_, r)| r.deepest_m as f64).sum::<f64>()
                    / results.len().max(1) as f64,
                total_deaths: results.iter().map(|(_, r)| r.deaths.len()).sum(),
                deaths_by_cause: causes
                    .into_iter()
                    .map(|cause| {
                        let total = results
                            .iter()
                            .map(|(_, r)| r.deaths_by_cause(cause))
                            .sum::<usize>();
                        (cause, total)
                    })
                    .collect(),
                total_frames: sum(|r| u64::from(r.frames)),
                intent_frames,
                escape_climb_no_ledge_frames: sum(|r| u64::from(r.escape_climb_no_ledge_frames)),
                escape_climb_max_streak: results
                    .iter()
                    .map(|(_, r)| r.escape_climb_max_streak)
                    .max()
                    .unwrap_or(0),
                wait_out_frames: sum(|r| u64::from(r.wait_out_frames)),
                wait_out_max_budget_frames: results
                    .iter()
                    .map(|(_, r)| r.wait_out_max_budget_frames)
                    .max()
                    .unwrap_or(0),
                climbs: sum(|r| u64::from(r.climbs)),
                stalled_oxygen_deaths: sum(|r| u64::from(r.stalled_oxygen_deaths)) as u32,
                dodge_events: sum(|r| u64::from(r.dodge_events)),
                dodge_with_safe_ledge: sum(|r| u64::from(r.dodge_with_safe_ledge)),
                crush_deaths_with_recent_safe_ledge: sum(|r| {
                    u64::from(r.crush_deaths_with_recent_safe_ledge)
                }) as u32,
                air_pickups: results.iter().map(|(_, r)| r.air_pickups.len()).sum(),
                air_passes: results.iter().map(|(_, r)| r.air_passes.len()).sum(),
                missed_air_while_low: results
                    .iter()
                    .map(|(_, r)| r.missed_air_while_low(false))
                    .sum(),
                missed_air_while_low_targeted: results
                    .iter()
                    .map(|(_, r)| r.missed_air_while_low(true))
                    .sum(),
                item_stats,
                starify_rocks_at_pickup: average_at_pickup(
                    results,
                    ItemEffect::StarifyScreen,
                    |p| p.3 as f64,
                ),
                clear_above_unstable_at_pickup: average_at_pickup(
                    results,
                    ItemEffect::ClearAbove,
                    |p| p.4 as f64,
                ),
                frames_warning: sum(|r| u64::from(r.frames_warning)),
                frames_emergency: sum(|r| u64::from(r.frames_emergency)),
                min_oxygen: results
                    .iter()
                    .map(|(_, r)| r.min_oxygen)
                    .fold(OXYGEN_MAX, f32::min),
            }
        }

        fn per_run(&self, cause: MissCause) -> f64 {
            let total = self
                .deaths_by_cause
                .iter()
                .find(|(c, _)| *c == cause)
                .map_or(0, |(_, n)| *n);
            total as f64 / self.seeds.max(1) as f64
        }

        fn oxygen_deaths_per_run(&self) -> f64 {
            self.per_run(MissCause::OxygenOut)
        }

        fn crush_deaths_per_run(&self) -> f64 {
            self.per_run(MissCause::CrushedByFallingBlock)
        }

        fn deaths_per_run(&self) -> f64 {
            self.total_deaths as f64 / self.seeds.max(1) as f64
        }

        /// 段差登りを出したのに棚が無かったフレームの割合(#229 G1)。
        fn escape_climb_no_ledge_rate(&self) -> f64 {
            self.escape_climb_no_ledge_frames as f64 / self.total_frames.max(1) as f64
        }

        /// 残量が半分以下でCandidate/TargetedだったAIRを取り逃した回数/走(#229 G3)。
        fn missed_air_while_low_per_run(&self) -> f64 {
            self.missed_air_while_low as f64 / self.seeds.max(1) as f64
        }

        fn print(&self, course: &str, seeds: u64) {
            println!(
                "{course}: 完走 {}/{seeds} / 無傷完走 {}/{seeds} / 酸素切れ {}シード / \
                 平均到達 {:.0}m / 完走時の平均岩破壊 {:.1}回 / 平均 {:.0}フレーム / \
                 死亡 計{}回({:.2}回/走)",
                self.cleared,
                self.unharmed,
                self.out_of_oxygen.len(),
                self.average_deepest_m,
                self.average_rocks,
                self.average_frames,
                self.total_deaths,
                self.deaths_per_run(),
            );
            for (cause, total) in &self.deaths_by_cause {
                println!(
                    "    {cause:?}: 計{total}回 ({:.2}回/走)",
                    *total as f64 / self.seeds.max(1) as f64
                );
            }
            println!(
                "    意図別フレーム: {}",
                Intent::ALL
                    .iter()
                    .zip(self.intent_frames)
                    .filter(|(_, frames)| *frames > 0)
                    .map(|(intent, frames)| format!(
                        "{intent:?} {:.1}%",
                        frames as f64 * 100.0 / self.total_frames.max(1) as f64
                    ))
                    .collect::<Vec<_>>()
                    .join(" / ")
            );
            println!(
                "    段差登り: 成立 {}回 / 棚無しの空振り {}フレーム({:.3}%) / 最長連続 {}フレーム",
                self.climbs,
                self.escape_climb_no_ledge_frames,
                self.escape_climb_no_ledge_rate() * 100.0,
                self.escape_climb_max_streak,
            );
            println!(
                "    待ち: 計{}フレーム / 行が進まないまま最大{}フレーム連続 / \
                 停滞したまま酸素切れ {}件",
                self.wait_out_frames, self.wait_out_max_budget_frames, self.stalled_oxygen_deaths,
            );
            println!(
                "    AIR: 取得{}回 / 通過{}回(取得率 {:.0}%) / 残量50%以下で寄れたはずの\
                 取り逃し {}回({:.2}回/走、うち目指していた列 {}回)",
                self.air_pickups,
                self.air_passes,
                self.air_pickups as f64 * 100.0
                    / (self.air_pickups + self.air_passes).max(1) as f64,
                self.missed_air_while_low,
                self.missed_air_while_low_per_run(),
                self.missed_air_while_low_targeted,
            );
            for (effect, taken, passed) in &self.item_stats {
                println!(
                    "    {effect:?}: 取得{taken}回 / 通過{passed}回 (取得率 {:.0}%)",
                    *taken as f64 * 100.0 / (taken + passed).max(1) as f64
                );
            }
            println!(
                "    アイテムの取り時: スター化の先読み窓の岩 平均{:.1}個 / \
                 頭上クリアの不安定な塊 平均{:.1}個",
                self.starify_rocks_at_pickup, self.clear_above_unstable_at_pickup,
            );
            println!(
                "    回避: 横/下へ {}回(うち安全な棚あり {}回) / \
                 押し潰し死のうち直前に棚があった {}件",
                self.dodge_events,
                self.dodge_with_safe_ledge,
                self.crush_deaths_with_recent_safe_ledge,
            );
            println!(
                "    酸素: 警告域 {:.1}% / 緊急域 {:.1}% / 最小 {:.1}%",
                self.frames_warning as f64 * 100.0 / self.total_frames.max(1) as f64,
                self.frames_emergency as f64 * 100.0 / self.total_frames.max(1) as f64,
                self.min_oxygen,
            );
        }
    }

    /// どのコースでも共通で満たすべき「詰まっていないこと」の基準(#229)。
    ///
    /// 完走率・死因の内訳は、詰まり方(登れない壁に張り付く・待ち続ける)が別の死因へ
    /// 化けるだけでも動いてしまう。ここでは結果ではなく過程を直接見る。
    ///
    /// 一方、次の2つは`Summary::print`に出すだけで閾値にしていない。どちらも設計時は
    /// 「0件/1回以下」を目標に置いたが、実測してみると目標そのものが成立しなかった。
    ///
    /// - **停滞したまま酸素切れ**(死の直前10秒の過半を待ち・登り・接地したままの待機に
    ///   費やした死に方): 1000mで57/102件、harshで8/29件。行き止まりに入り込んで
    ///   そのまま窒息する経路が残っている。段差登りの空振り(F1)を塞いでも消えず、
    ///   これ以上は経路探索の作り自体(1行ずつの貪欲法)の問題になる
    /// - **寄れたはずのAIRの取り逃し**: 窓(9列×14行)に入ったAIRのうち、却下されて
    ///   いない列にあったものを数えると1000mで59.94回/走になる。候補列は最大9列
    ///   あるため「候補列にあった=寄れたはず」とは言えず、指標として閾値化できない。
    ///   実際に目指していた列にあったものに絞ると3.53回/走まで下がるが、それでも
    ///   目的列は毎フレーム変わるため「取り逃し」と断じるには弱い
    fn assert_never_stalls(summary: &Summary) {
        assert!(
            summary.escape_climb_no_ledge_rate() <= 0.01,
            "登り先の棚が無いのに段差登りを出しているフレームが多すぎる: \
             {}フレーム({:.3}%)",
            summary.escape_climb_no_ledge_frames,
            summary.escape_climb_no_ledge_rate() * 100.0
        );
        assert!(
            summary.escape_climb_max_streak <= 100,
            "段差登りが{}フレーム連続している(登れない壁に張り付いている疑い)",
            summary.escape_climb_max_streak
        );
        assert!(
            summary.wait_out_max_budget_frames <= 60,
            "行が進まないまま{}フレーム待ち続けている\
             (待機上限{}msが実質無効になっている疑い)",
            summary.wait_out_max_budget_frames,
            AUTOPLAY_WAIT_FOR_THREAT_MAX_MS
        );
    }

    /// アイテム取得時の状況(岩の数・不安定な塊の数)の平均。
    fn average_at_pickup(
        results: &[(u64, SoakResult)],
        effect: ItemEffect,
        pick: fn(&(ItemEffect, u32, f32, usize, usize)) -> f64,
    ) -> f64 {
        let samples: Vec<f64> = results
            .iter()
            .flat_map(|(_, r)| r.item_pickups.iter())
            .filter(|pickup| pickup.0 == effect)
            .map(pick)
            .collect();
        samples.iter().sum::<f64>() / samples.len().max(1) as f64
    }
}

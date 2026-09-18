//! ミスドリTERM: メインループ(spec.md 9章)。
//! Phase1(ノーマルコース シングルプレイ)のみを実装する。

mod app;
mod audio;
mod autoplay;
mod battle;
mod constants;
mod debug_log;
mod discovery;
mod game;
mod input;
mod lobby;
mod lockstep;
mod net;
mod rewind;
mod settings;
mod ui;

use std::io;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use rodio::mixer::Mixer;

use app::audio::{
    effective_gameplay_bgm_enabled, effective_title_bgm_enabled, play_se, should_restart_title_bgm,
};
use app::screens::{
    tick_battle, tick_help_screen, tick_mode_select, tick_network_lobby, tick_playing, tick_rewind,
    tick_settings_screen, tick_title,
};
use battle::BattleState;
use game::{Game, InputAction};
use lobby::LobbyState;
use settings::Settings;

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();

    // Kittyキーボードプロトコル(対応ターミナルのみ)を有効化する。レガシーANSIでは
    // 矢印キーと1文字キーの同時押しで生バイト列の解釈が曖昧になり得るため、
    // DISAMBIGUATE_ESCAPE_CODESで解消する。非対応ターミナルでは何もしない。
    let keyboard_enhancement_enabled = crossterm::terminal::supports_keyboard_enhancement()
        .unwrap_or(false)
        && execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok();

    let result = run(&mut terminal);

    if keyboard_enhancement_enabled {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        let _ = io::stdout().flush();
    }
    ratatui::restore();
    result
}

/// 1フレームの処理をまたいで持ち越すアプリ全体の状態(#264)。
///
/// 画面状態`Screen`はここに含めない。`Screen::Playing(Box<Game>)`から`game`を取り出して
/// 画面別の関数へ渡す間、他の全フィールド(`&mut App`)も同時に可変借用する必要があるため、
/// `screen`は`run()`のローカル変数として別に持つ。
struct App {
    /// 音声出力デバイス。ヘッドレス環境等でデバイスが無い場合は`None`のままで、
    /// 以後の再生を全てスキップする。
    mixer: Option<Mixer>,
    settings: Settings,
    /// タイトル画面用BGMの実効ON/OFF。BGMスレッドと共有する。
    title_music_enabled: Arc<AtomicBool>,
    /// プレイ中BGMの実効ON/OFF。BGMスレッドと共有する。
    gameplay_music_enabled: Arc<AtomicBool>,
    se_enabled: Arc<AtomicBool>,
    /// MUSIC音量(#224)。タイトル用・プレイ中用の両BGMスレッドで共有する
    /// (MUSIC音量は画面によらず1つ)。
    music_volume_percent: Arc<AtomicU32>,
    /// タイトル画面へ戻るたびにタイトルBGMを先頭から再生し直すためのフラグ。
    title_bgm_restart: Arc<AtomicBool>,
    /// 直前フレームでのタイトルBGMの実効ON/OFF。`title_bgm_restart`を立てる
    /// 「無効→有効」の切り替わり判定に使う。
    was_title_bgm_enabled: bool,
    /// タイトル画面へ戻るたびにプレイ中BGMも先頭の曲・先頭位置からリセットするフラグ。
    /// `title_bgm_restart`とはトリガー条件が異なる(あちらは「無効→有効」の切り替わり、
    /// こちらは「タイトル画面へ戻った瞬間」)ため、別フラグとして扱う。
    gameplay_bgm_restart: Arc<AtomicBool>,
    bgm_stop: Arc<AtomicBool>,
    rng: rand::rngs::ThreadRng,
    last_tick: Instant,
    /// モードセレクト画面での現在の選択(イージー/ノーマル)。
    mode_select_choice: ui::render::CourseChoice,
    /// 設定画面での現在の選択項目。
    settings_selection: ui::render::SettingsChoice,
    /// 一時停止中にオーバーレイ表示する設定/ヘルプ画面。Gameを作り直さずScreen::Playingの
    /// まま上に重ねて描画するだけなので、画面遷移ではなくこの状態フラグで管理する。
    pause_overlay: PauseOverlay,
    /// ヘルプ画面(タイトルから開く独立画面)のジュークボックスのカーソル位置。
    /// 画面を離れても保持する。
    help_jukebox_selection: usize,
    /// ジュークボックスで再生中の曲。その再生を制御するハンドル(stop/finishedフラグ)と
    /// セットで持つ。
    help_jukebox_playing: Option<(usize, audio::bgm::JukeboxPreview)>,
    /// オートプレイ(TERM独自拡張。#218)。Tキーで生成・破棄する。`Some`の間、
    /// 毎フレーム`decide`が返す仮想入力を人間の操作と同じ経路(`Game::apply_input`)へ
    /// 流し込む。Gameを作り直す場面(タイトルへ戻る)では必ずNoneへ戻す。
    autopilot: Option<autoplay::Autopilot>,
    /// 現在のオートプレイが、タイトル画面放置による自動デモ(アトラクトモード)として
    /// 始まったものかどうか。手動でTキーを押した場合(=操作を引き継ぎたい)と、デモを
    /// 見ていた人が割り込んだ場合(=タイトルへ戻す)で挙動を分けるために区別する。
    autopilot_is_attract_demo: bool,
    /// タイトル画面で最後にキーが押されてからの経過時間。`ATTRACT_MODE_IDLE_MS`を
    /// 超えるとアトラクトモードを自動開始する。
    title_idle: Duration,
    /// フレーム巻き戻し(TERM独自拡張。#233)の履歴。`autopilot`と同じくGameの外に置く
    /// 寿命の状態で、Gameを作り直す場面(タイトルへ戻る・新規ゲーム開始)では必ず捨てる。
    rewind_history: rewind::RewindHistory,
    /// 進行中の巻き戻しセッション。`Some`の間はゲーム本体を凍結し、逆再生の操作だけを
    /// 受け付ける。
    rewind_session: Option<rewind::RewindSession>,
}

/// 画面別の1フレーム処理が返す「このフレームで起きた画面遷移」(#264)。
/// 遷移が起きなかったフレームは`None`(`Option<ScreenTransition>`)で表す。
enum ScreenTransition {
    /// アプリを終了する(タイトル画面でのEscのみ)。
    Quit,
    /// タイトル画面へ戻る(設定/ヘルプ/モードセレクト画面からの離脱)。Gameを持たない
    /// 画面からの遷移なので、ゲーム側の状態の後始末は伴わない。
    ToTitle,
    /// プレイ中からタイトル画面へ戻る。Gameを破棄するため、Gameの外に置いた状態
    /// (オーバーレイ・オートプレイ・巻き戻し履歴)も畳み、プレイ中BGMもリセットする。
    ToTitleDiscardingGame,
    ToModeSelect,
    ToSettings,
    ToHelp,
    ToPlaying(Box<Game>),
    /// 対戦相手を探すロビー画面へ(#256)。タイトルでNキーを押すと、探索用の
    /// ソケットを確保済みの`LobbyState`がここに載って渡ってくる。
    ToNetworkLobby(Box<LobbyState>),
    /// ロビーで対戦が成立した(#256)。ハンドシェイク済みの状態をそのまま対戦画面へ渡す。
    ToBattle(Box<BattleState>),
}

fn run(terminal: &mut ratatui::DefaultTerminal) -> io::Result<()> {
    // 音声出力デバイスを開く。ヘッドレス環境等でデバイスが無い場合でも
    // ゲーム自体はプレイ続行できるよう、失敗時はNoneにして以後の再生をスキップする。
    let sink_handle = rodio::DeviceSinkBuilder::open_default_sink().ok();
    let mixer: Option<Mixer> = sink_handle.as_ref().map(|handle| handle.mixer().clone());

    // MUSIC/SE個別ON/OFF設定(spec.md 10章)。前回終了時の状態を復元し、
    // BGMスレッド・SE再生の双方から参照できるよう`Arc<AtomicBool>`で共有する。
    let settings = Settings::load();
    // タイトル画面用・プレイ中用でBGMを別トラックにする。同時に両方鳴らないよう
    // `effective_title_bgm_enabled`/`effective_gameplay_bgm_enabled`は排他的になるよう
    // 設計している。起動直後はタイトル画面から始まる。
    let title_music_enabled = Arc::new(AtomicBool::new(effective_title_bgm_enabled(
        settings.music_enabled,
        &Screen::Title,
    )));
    let gameplay_music_enabled = Arc::new(AtomicBool::new(effective_gameplay_bgm_enabled(
        settings.music_enabled,
        &Screen::Title,
    )));
    let se_enabled = Arc::new(AtomicBool::new(settings.se_enabled));
    let music_volume_percent = Arc::new(AtomicU32::new(settings.music_volume_percent));
    // 起動直後の初回表示は「戻ってきた」わけではないので、ここではまだ立てない。
    let title_bgm_restart = Arc::new(AtomicBool::new(false));
    let was_title_bgm_enabled = title_music_enabled.load(Ordering::Relaxed);
    let gameplay_bgm_restart = Arc::new(AtomicBool::new(false));

    let bgm_stop = Arc::new(AtomicBool::new(false));
    if let Some(m) = &mixer {
        audio::bgm::spawn_title_bgm_thread(
            m.clone(),
            Arc::clone(&bgm_stop),
            Arc::clone(&title_music_enabled),
            Arc::clone(&title_bgm_restart),
            Arc::clone(&music_volume_percent),
        );
        audio::bgm::spawn_gameplay_bgm_thread(
            m.clone(),
            Arc::clone(&bgm_stop),
            Arc::clone(&gameplay_music_enabled),
            Arc::clone(&gameplay_bgm_restart),
            Arc::clone(&music_volume_percent),
        );
    }

    // モードセレクト画面は、タイトルから開くたびに前回選んだコース
    // (`settings.last_course_depth_m`)を初期選択として引き継ぐ。
    let mode_select_choice =
        ui::render::CourseChoice::from_depth_goal_m(settings.last_course_depth_m);

    let mut app = App {
        mixer,
        settings,
        title_music_enabled,
        gameplay_music_enabled,
        se_enabled,
        music_volume_percent,
        title_bgm_restart,
        was_title_bgm_enabled,
        gameplay_bgm_restart,
        bgm_stop,
        // 通常プレイはOS乱数から生成したシードを使う(spec.md 3章)。
        rng: rand::rng(),
        last_tick: Instant::now(),
        mode_select_choice,
        settings_selection: ui::render::SettingsChoice::Music,
        pause_overlay: PauseOverlay::None,
        help_jukebox_selection: 0,
        help_jukebox_playing: None,
        autopilot: None,
        autopilot_is_attract_demo: false,
        title_idle: Duration::ZERO,
        rewind_history: rewind::RewindHistory::new(),
        rewind_session: None,
    };

    // アプリの画面状態(spec.md 1章)。タイトル画面でのEscのみアプリを終了し、それ以外の
    // 画面でのEscはGameを作り直してタイトルへ戻す(酸素・スコア・深度等が全てリセットされる)。
    let mut screen = Screen::Title;

    loop {
        let transition = match &mut screen {
            // フレーム巻き戻し中(TERM独自拡張。#233)はScreen::Playingのままゲームを
            // 凍結し、逆再生の操作だけを扱う(この間は画面遷移が起きない)。
            Screen::Playing(game) if app.rewind_session.is_some() => {
                tick_rewind(&mut app, game, terminal)?;
                None
            }
            Screen::Playing(game) => tick_playing(&mut app, game, terminal)?,
            // 対戦中(#252)。通常プレイとは扱う入力もtickの刻み方も異なるため、
            // `tick_playing`に分岐を混ぜず独立した関数へ渡す。
            Screen::Battle(state) => tick_battle(&mut app, state, terminal)?,
            // 対戦相手を探すロビー(#256)。
            Screen::NetworkLobby(state) => tick_network_lobby(&mut app, state, terminal)?,
            Screen::Settings => tick_settings_screen(&mut app, terminal)?,
            Screen::Help => tick_help_screen(&mut app, terminal)?,
            Screen::ModeSelect => tick_mode_select(&mut app, terminal)?,
            Screen::Title => tick_title(&mut app, terminal)?,
        };

        match transition {
            Some(ScreenTransition::Quit) => break,
            Some(ScreenTransition::ToTitle) => screen = Screen::Title,
            Some(ScreenTransition::ToTitleDiscardingGame) => {
                screen = Screen::Title;
                app.pause_overlay = PauseOverlay::None;
                // タイトルへ戻るとGameごと破棄されるため、オートプレイも必ず手放す
                // (TERM独自拡張。#218)。アイドルタイマーも0から数え直す。
                app.autopilot = None;
                app.autopilot_is_attract_demo = false;
                app.title_idle = Duration::ZERO;
                // Gameを破棄するので、それを複製した巻き戻し履歴・進行中のセッションも捨てる(#233)。
                app.rewind_history.clear();
                app.rewind_session = None;
                // タイトル画面へ戻った瞬間にプレイ中BGMもリセットする。次にプレイを始めた
                // とき、前回の再生位置・曲順を引きずらず必ず1曲目の先頭から鳴るようにする。
                app.gameplay_bgm_restart.store(true, Ordering::Relaxed);
            }
            Some(ScreenTransition::ToModeSelect) => screen = Screen::ModeSelect,
            Some(ScreenTransition::ToSettings) => screen = Screen::Settings,
            Some(ScreenTransition::ToHelp) => screen = Screen::Help,
            Some(ScreenTransition::ToPlaying(game)) => {
                screen = Screen::Playing(game);
                app.last_tick = Instant::now();
            }
            Some(ScreenTransition::ToNetworkLobby(state)) => screen = Screen::NetworkLobby(state),
            Some(ScreenTransition::ToBattle(state)) => {
                screen = Screen::Battle(state);
                // ロビーでの待ち時間(招待の応答待ち・ハンドシェイク)がそのまま
                // 1フレーム目のdeltaにならないよう、対戦開始時に計り直す。
                app.last_tick = Instant::now();
            }
            None => {}
        }

        // 画面遷移(タイトルへ戻る/タイトルから抜ける)を反映して、BGMスレッドが参照する
        // 実効MUSIC状態を毎フレーム同期する。タイトル用・プレイ中用のいずれか一方だけがtrueになる。
        let title_bgm_now_enabled =
            effective_title_bgm_enabled(app.settings.music_enabled, &screen);
        app.title_music_enabled
            .store(title_bgm_now_enabled, Ordering::Relaxed);
        app.gameplay_music_enabled.store(
            effective_gameplay_bgm_enabled(app.settings.music_enabled, &screen),
            Ordering::Relaxed,
        );
        // タイトル画面へ戻ってきた(無効→有効に転じた)瞬間に、タイトルBGMを
        // 先頭から再生し直す。
        if should_restart_title_bgm(app.was_title_bgm_enabled, title_bgm_now_enabled) {
            app.title_bgm_restart.store(true, Ordering::Relaxed);
        }
        app.was_title_bgm_enabled = title_bgm_now_enabled;
    }

    app.bgm_stop.store(true, Ordering::Relaxed);

    Ok(())
}

/// 新規ゲームを1つ作り、永続化された設定を全て反映して返す。モードセレクトでEnterを
/// 押した場合と、タイトル画面の放置から始まるアトラクトモード(TERM独自拡張。#218)の
/// どちらからも同じ経路を通すため、共通の関数として切り出している。
fn start_new_game(seed: u64, settings: &Settings, depth_goal_m: usize) -> Game {
    // フィールド幅(列数)設定は新規ゲーム開始時にのみ反映される。
    let mut game = Game::new_with_width(seed, settings.field_width, depth_goal_m);
    // 調査用のブロック状態遷移ログをタイトルからのゲーム開始時に毎回作り直す。
    // 設定画面のトグルで無効化していれば記録自体を行わない。
    game.refresh_debug_log(settings.debug_log_enabled);
    // 速度系デバッグショートカットの調整値と出現率の配分は設定ファイルに永続化されており
    // (settings.rs)、新しいゲーム開始時にも引き継ぐ。オートプレイのソークテストが実機と
    // 同じ盤面を測れるよう、反映処理はGame側(`apply_settings`)に置いて共有している(#225)。
    game.apply_settings(settings);
    game
}

/// ヘルプ画面のジュークボックスの選択カーソルを`len`個の巡回範囲内で動かす。
/// `forward`がtrueなら次へ、falseなら前へ進み、端では反対の端へ巡回する。
fn cycle_jukebox_selection(selection: usize, len: usize, forward: bool) -> usize {
    if forward {
        (selection + 1) % len
    } else {
        selection.checked_sub(1).unwrap_or(len - 1)
    }
}

/// アプリ全体の画面状態(spec.md 1章)。`Game`は演出・補間用の状態が増え
/// バリアント間のサイズ差が大きくなったため`Box`で包む。
enum Screen {
    Title,
    /// コース選択画面。タイトルでEnterを押した直後に経由し、
    /// ここでEnterを押すと実際にゲームが始まる。
    ModeSelect,
    Settings,
    Help,
    Playing(Box<Game>),
    /// 対戦中(#252。spec.md 12章)。ロビー(#256)で対戦が成立するとここへ移る。
    Battle(Box<BattleState>),
    /// 対戦相手を探すロビー画面(#256。spec.md 12.1)。タイトルでNキーを押すと移る。
    NetworkLobby(Box<LobbyState>),
}

/// 一時停止中にオーバーレイ表示する画面。`Screen::Playing`のまま
/// (Gameを手放さず)上に重ねて描画するだけなので、独立した状態として持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PauseOverlay {
    None,
    Settings,
    Help,
}

/// 巻き戻し(逆再生)セッションの1フレーム分を進める(TERM独自拡張。#233)。
///
/// 入力の解釈はセッション側(`rewind::RewindSession::handle`)に任せ、ここはその結果に
/// 応じた後始末だけを行う。戻り値は「このフレームで描画すべきスナップショットの位置」で、
/// `None`ならセッションはこのフレームで終了した(確定またはキャンセル)。
#[allow(clippy::too_many_arguments)]
fn advance_rewind_session(
    game: &mut Game,
    rewind_session: &mut Option<rewind::RewindSession>,
    rewind_history: &mut rewind::RewindHistory,
    autopilot: &mut Option<autoplay::Autopilot>,
    actions: &[InputAction],
    delta: Duration,
    mixer: Option<&Mixer>,
    se_enabled: &Arc<AtomicBool>,
    se_volume_percent: u32,
) -> Option<usize> {
    let history_len = rewind_history.len();
    let session = rewind_session.as_mut()?;

    let mut outcome = rewind::RewindOutcome::Continue;
    for &action in actions {
        outcome = session.handle(action, history_len);
        if outcome != rewind::RewindOutcome::Continue {
            break;
        }
    }
    if outcome == rewind::RewindOutcome::Continue {
        session.tick(delta, history_len);
        return Some(session.cursor());
    }

    // 以降はセッション終了。確定位置だけ取り出してセッションを手放す。
    let confirmed = match outcome {
        rewind::RewindOutcome::Confirm(cursor) => Some(cursor),
        rewind::RewindOutcome::Continue | rewind::RewindOutcome::Cancel => None,
    };
    *rewind_session = None;

    // cursor=0は巻き戻し開始時点の「現在」そのものなので、キャンセルと同じ扱いにし
    // 復元もストック消費も行わない。
    if let Some(cursor) = confirmed.filter(|&cursor| cursor > 0) {
        game.restore_for_rewind(&rewind_history.snapshot_at(cursor).game);
        // 戻した先より新しいスナップショットは「起こらなかった未来」なので捨てる。
        rewind_history.discard_newer_than(cursor);
        // オートプレイ中だった場合、位置履歴・目的列といった内部状態が巻き戻し前の
        // 盤面を前提にしたままになるため作り直す(#218のAutopilotはGameの外の状態)。
        if autopilot.is_some() {
            *autopilot = Some(autoplay::Autopilot::new(game.is_invincible()));
        }
        play_se(
            mixer,
            se_enabled,
            se_volume_percent,
            audio::sfx::play_revive,
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_jukebox_selection_wraps_around_at_both_ends() {
        // ↑/↓での選択移動が両端で正しく巡回することを確認する。
        assert_eq!(cycle_jukebox_selection(0, 4, true), 1);
        assert_eq!(cycle_jukebox_selection(3, 4, true), 0, "末尾の次は先頭へ");
        assert_eq!(cycle_jukebox_selection(2, 4, false), 1);
        assert_eq!(cycle_jukebox_selection(0, 4, false), 3, "先頭の前は末尾へ");
    }
}

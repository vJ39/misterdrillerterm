# main.rs分割 段階(b) 設計: App状態構造体とrun()の画面別関数化(#264)

## 方針

`run()`のローカル変数群を`App`構造体にまとめる。ただし`screen: Screen`は**Appに含めず、
run()のローカル変数のまま残す**。理由: `Screen::Playing(Box<Game>)`から`game`を取り出して
画面関数に渡す際、`&mut screen`を借用しながら同時に`&mut App`(他の全フィールド)を借用する
必要があるが、`screen`と`app`を最初から別変数にしておけば、これは単純な「2つの独立した
可変参照を関数に渡す」だけになり、借用チェッカー上の問題が一切発生しない。

ロジックの中身(条件分岐の意味、設定変更の処理内容)は一切変更しない。#265(段階c、
PauseOverlay::Settings処理とScreen::Settings処理の重複統合)は今回のスコープ外。

## App構造体(screenを含まない、21フィールド)

```rust
struct App {
    mixer: Option<Mixer>,
    settings: Settings,
    title_music_enabled: Arc<AtomicBool>,
    gameplay_music_enabled: Arc<AtomicBool>,
    se_enabled: Arc<AtomicBool>,
    music_volume_percent: Arc<AtomicU32>,
    title_bgm_restart: Arc<AtomicBool>,
    was_title_bgm_enabled: bool,
    gameplay_bgm_restart: Arc<AtomicBool>,
    bgm_stop: Arc<AtomicBool>,
    rng: rand::rngs::ThreadRng, // rand::rng()の戻り値の実際の型をcargo docまたは型推論で確認すること
    last_tick: Instant,
    mode_select_choice: ui::render::CourseChoice,
    settings_selection: ui::render::SettingsChoice,
    pause_overlay: PauseOverlay,
    help_jukebox_selection: usize,
    help_jukebox_playing: Option<(usize, audio::bgm::JukeboxPreview)>,
    autopilot: Option<autoplay::Autopilot>,
    autopilot_is_attract_demo: bool,
    title_idle: Duration,
    rewind_history: rewind::RewindHistory,
    rewind_session: Option<rewind::RewindSession>,
}
```

## 画面遷移の表現

```rust
enum ScreenTransition {
    Quit,
    ToTitle,
    ToModeSelect,
    ToSettings,
    ToHelp,
    ToPlaying(Box<Game>),
}
```

各画面関数は`io::Result<Option<ScreenTransition>>`を返す。`None`は「このフレームでは
画面遷移なし」。

## 画面関数のシグネチャ

```rust
fn tick_rewind(app: &mut App, game: &mut Game, terminal: &mut DefaultTerminal) -> io::Result<()>
fn tick_playing(app: &mut App, game: &mut Game, terminal: &mut DefaultTerminal) -> io::Result<Option<ScreenTransition>>
fn tick_settings_screen(app: &mut App, terminal: &mut DefaultTerminal) -> io::Result<Option<ScreenTransition>>
fn tick_help_screen(app: &mut App, terminal: &mut DefaultTerminal) -> io::Result<Option<ScreenTransition>>
fn tick_mode_select(app: &mut App, terminal: &mut DefaultTerminal) -> io::Result<Option<ScreenTransition>>
fn tick_title(app: &mut App, terminal: &mut DefaultTerminal) -> io::Result<Option<ScreenTransition>>
```

- `tick_rewind`: 元の164〜211行。巻き戻し中は画面遷移が起きないため戻り値なし。
- `tick_playing`: 元の212〜777行(約560行、最大のブロック)。`InputAction`の巨大matchも
  含めてそのまま移す。`back_to_title = true`になっていた箇所は全て
  `return Ok(Some(ScreenTransition::ToTitle))`に置き換える(即座にreturnしてよいか、
  元のロジックが「フラグを立てて後続処理を続ける」ものだったかを1箇所ずつ確認すること。
  特に`InputAction::Quit`はループ内の`break`で残りの入力を捨てる意図があるため、
  「以後の入力ループを抜けてreturnする」という挙動を保つこと)。
  ゲームオーバーで`GameOverChoice::BackToTitle`のケースも同様。
  アトラクトモード中の割り込みで`back_to_title = true; break;`となっていた箇所も同様。
  `Screen::Playing(Box::new(game))`のような新規ゲーム開始はこの関数の中では発生しない
  (ModeSelect/Titleからのみ発生)。
- `tick_settings_screen`/`tick_help_screen`/`tick_mode_select`/`tick_title`: 元の
  778〜1029行/1030〜1090行/1091〜1118行/1119〜1166行。`Screen::Title`で
  `Screen::Playing(Box::new(game))`に遷移するケース(モードセレクト経由・アトラクト
  モード開始)は`ScreenTransition::ToPlaying(Box::new(game))`を返す。

## run()本体の骨格

```rust
fn run(terminal: &mut ratatui::DefaultTerminal) -> io::Result<()> {
    let mut app = App { /* 初期化、元の66〜156行相当 */ };
    let mut screen = Screen::Title;

    loop {
        let transition = match &mut screen {
            Screen::Playing(game) if app.rewind_session.is_some() => {
                tick_rewind(&mut app, game, terminal)?;
                None
            }
            Screen::Playing(game) => tick_playing(&mut app, game, terminal)?,
            Screen::Settings => tick_settings_screen(&mut app, terminal)?,
            Screen::Help => tick_help_screen(&mut app, terminal)?,
            Screen::ModeSelect => tick_mode_select(&mut app, terminal)?,
            Screen::Title => tick_title(&mut app, terminal)?,
        };

        match transition {
            Some(ScreenTransition::Quit) => break,
            Some(ScreenTransition::ToTitle) => {
                screen = Screen::Title;
                // 元の1168〜1182行相当(back_to_title後処理)をここで行う:
                // pause_overlayリセット・autopilot解除・autopilot_is_attract_demoリセット・
                // title_idleリセット・rewind_history/session クリア・
                // gameplay_bgm_restartフラグを立てる。
            }
            Some(ScreenTransition::ToModeSelect) => screen = Screen::ModeSelect,
            Some(ScreenTransition::ToSettings) => screen = Screen::Settings,
            Some(ScreenTransition::ToHelp) => screen = Screen::Help,
            Some(ScreenTransition::ToPlaying(game)) => {
                screen = Screen::Playing(game);
                app.last_tick = Instant::now();
            }
            None => {}
        }

        // 元の1184〜1197行相当(BGM実効状態の毎フレーム同期)。screenの現在値を読むだけ。
        let title_bgm_now_enabled = effective_title_bgm_enabled(app.settings.music_enabled, &screen);
        app.title_music_enabled.store(title_bgm_now_enabled, Ordering::Relaxed);
        app.gameplay_music_enabled.store(
            effective_gameplay_bgm_enabled(app.settings.music_enabled, &screen),
            Ordering::Relaxed,
        );
        if should_restart_title_bgm(app.was_title_bgm_enabled, title_bgm_now_enabled) {
            app.title_bgm_restart.store(true, Ordering::Relaxed);
        }
        app.was_title_bgm_enabled = title_bgm_now_enabled;
    }

    app.bgm_stop.store(true, Ordering::Relaxed);
    Ok(())
}
```

## 借用・所有権上の注意点

1. `Screen::Playing(game)`のパターンマッチで得られる`game`の型は`&mut Box<Game>`。
   関数シグネチャは`&mut Game`で受け取りたいので、呼び出し側は`tick_playing(&mut app, game, terminal)`
   のように渡す(自動derefでコンパイルが通るはず。通らない場合は`&mut **game`に調整する)。
2. `match &mut screen { ... }`の各アームで`tick_xxx(&mut app, ...)`を呼ぶ際、`screen`と`app`は
   別変数なので同時可変借用の問題は起きない。
3. `tick_playing`の中で`mixer.as_ref()`のような呼び出しは`app.mixer.as_ref()`に、
   `&se_enabled`は`&app.se_enabled`に、`settings.se_volume_percent`は`app.settings.se_volume_percent`
   に、それぞれ単純に置き換える(フィールドアクセスはメソッド全体の可変借用を必要としないため、
   `game`を同時に借用していても問題ない)。
4. `ScreenTransition::ToPlaying(Box::new(game))`を返す箇所(ModeSelect確定時、アトラクト
   モード開始時)は、`start_new_game`の戻り値をそのまま`Box::new(...)`で包む。
5. `#[allow(clippy::too_many_arguments)]`が必要になる関数があれば付与する。

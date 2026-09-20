//! 画面別の1フレーム処理(#264。#263に続くsrc/main.rs分割、段階b)。
//!
//! `run()`のループ本体にあった画面ごとのブロックをそのまま関数へ切り出したもので、
//! 各関数は入力の取り込み・状態更新・描画を1フレーム分行い、画面遷移が起きた場合だけ
//! `ScreenTransition`を返す。フレームをまたぐ状態は`App`が持ち、画面状態`Screen`の
//! 付け替えは呼び出し元(`run()`)が行う。

use std::io;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use rand::RngExt;

use crate::app::audio::{handle_events, play_se};
use crate::app::settings_menu::{
    adjust_attack_blocks_per_rock, adjust_attack_rocks_per_wave_max, adjust_bomb_fuse_ms,
    adjust_bomb_rate_percent, adjust_chain_vanish_interval_ms, adjust_dodge_recovery_ms,
    adjust_fall_speed_ms, adjust_field_width, adjust_move_cooldown_ms, adjust_rewind_stock_max,
    adjust_shake_duration_ms, adjust_sound_volume_percent, adjust_spawn_rate_setting,
};
use crate::battle::BattleState;
use crate::constants::{
    ATTRACT_MODE_IDLE_MS, FRAME_INTERVAL_MS, SPAWN_RATE_REROLL_SAFE_MARGIN_ROWS,
};
use crate::game::{Game, GameOverChoice, GameStatus, InputAction};
use crate::lobby::{LobbyOutcome, LobbyState};
use crate::net::BattleConfig;
use crate::{
    App, PauseOverlay, ScreenTransition, advance_rewind_session, audio, autoplay,
    cycle_jukebox_selection, input, rewind, start_new_game, ui,
};

/// フレーム巻き戻し中(TERM独自拡張。#233)の1フレーム。`game.update`を呼ばず
/// ゲームを凍結し、逆再生セッションの操作(←→での調整・確定・キャンセル)だけを扱う。
/// この間は画面遷移が起きないため戻り値を持たない。
pub fn tick_rewind(
    app: &mut App,
    game: &mut Game,
    terminal: &mut ratatui::DefaultTerminal,
) -> io::Result<()> {
    let actions = input::poll_input_batch(FRAME_INTERVAL_MS)?;
    let now = Instant::now();
    let delta = now
        .duration_since(app.last_tick)
        .min(Duration::from_millis(250));
    app.last_tick = now;

    let viewing_cursor = advance_rewind_session(
        game,
        &mut app.rewind_session,
        &mut app.rewind_history,
        &mut app.autopilot,
        &actions,
        delta,
        app.mixer.as_ref(),
        &app.se_enabled,
        app.settings.se_volume_percent,
    );

    let music_on = app.gameplay_music_enabled.load(Ordering::Relaxed);
    let se_on = app.se_enabled.load(Ordering::Relaxed);
    let autoplay_on = app.autopilot.is_some();
    let field_width = game.board.width();
    match viewing_cursor {
        // まだ巻き戻し中。今見ている時点のスナップショットを描画し、案内を重ねる。
        Some(cursor) => {
            let snapshot = app.rewind_history.snapshot_at(cursor);
            // ゲームは凍結中でフレーム番号が進まないため、現在との差がそのまま
            // 「どれだけ過去を見ているか」になる。
            let frames_back = game.debug_frame().saturating_sub(snapshot.frame_at_capture);
            let snapshot_game = &snapshot.game;
            terminal.draw(|frame| {
                ui::render::draw(frame, snapshot_game, music_on, se_on, autoplay_on);
                ui::render::draw_rewind_overlay(frame, field_width, cursor, frames_back);
            })?;
        }
        // このフレームで確定/キャンセルした。通常どおり現在の状態を描画する。
        None => {
            terminal.draw(|frame| ui::render::draw(frame, game, music_on, se_on, autoplay_on))?;
        }
    }

    Ok(())
}

/// プレイ中(`Screen::Playing`)の1フレーム。入力処理・オートプレイ・`Game::update`・
/// 描画をこの順で行う。
///
/// タイトルへ戻る操作は、元の実装では`back_to_title`フラグを立てて入力ループを`break`し、
/// 後続のゲーム進行・描画をまとめて飛ばしていた。ここでは同じ挙動を早期returnで表す
/// (以降のキューされた入力を捨て、このフレームのゲーム進行・描画も行わない)。
pub fn tick_playing(
    app: &mut App,
    game: &mut Game,
    terminal: &mut ratatui::DefaultTerminal,
) -> io::Result<Option<ScreenTransition>> {
    // poll_input_batch: 1フレーム内にキューされた全キーイベントを処理する。
    // 移動・向き変更と掘削をほぼ同時に押しても、同一フレームに届いた
    // 両方のイベントを取りこぼさず反映するため。
    for action in input::poll_input_batch(FRAME_INTERVAL_MS)? {
        // 巻き戻しを開始したら、同じフレームに溜まっていた残りの入力は捨てる
        // (次フレームから巻き戻し中の操作として解釈する)。
        if app.rewind_session.is_some() {
            break;
        }
        // アトラクトモード(タイトル放置から始まった自動デモ)中に人が何か
        // 操作したら、そこから引き継ぐのではなくタイトルへ戻す(TERM独自拡張。
        // #218。デモを見ていた人の割り込みは「やめる」意思表示とみなす)。
        if app.autopilot_is_attract_demo {
            // アトラクトモードは無人デモの安全策として無敵を強制ONにしている。
            // 人が割り込んだこの時点でデモ開始前の状態へ戻す。
            if let Some(pilot) = app.autopilot.take() {
                game.set_invincible(pilot.restore_invincible());
            }
            return Ok(Some(ScreenTransition::ToTitleDiscardingGame));
        }
        match action {
            // オーバーレイ(設定/ヘルプ)が開いている間のQはタイトルへ戻らず、
            // オーバーレイを閉じるだけにする。
            InputAction::Quit if app.pause_overlay != PauseOverlay::None => {
                app.pause_overlay = PauseOverlay::None;
            }
            InputAction::Quit => return Ok(Some(ScreenTransition::ToTitleDiscardingGame)),
            InputAction::TogglePause => {
                game.toggle_pause();
                app.pause_overlay = PauseOverlay::None;
            }
            // Backspace/U: フレーム巻き戻しの開始(TERM独自拡張。#233)。
            // 設定/ヘルプのオーバーレイ表示中は無効。ストックが残っていて
            // (`can_start_rewind`)、戻れる履歴が1つでもあるときだけ起動する。
            // 一時停止中・クリア後は`can_start_rewind`がfalseなので何も起きない。
            InputAction::Rewind => {
                if app.pause_overlay == PauseOverlay::None
                    && game.can_start_rewind()
                    && !app.rewind_history.is_empty()
                {
                    app.rewind_session =
                        Some(rewind::RewindSession::start(&mut app.rewind_history, game));
                    play_se(
                        app.mixer.as_ref(),
                        &app.se_enabled,
                        app.settings.se_volume_percent,
                        audio::sfx::play_rewind_start,
                    );
                }
            }
            // ポーズ解除はPだけでなく、ショートカット未割り当ての任意キーでも行える。
            // オーバーレイ(設定/ヘルプ)表示中は対象外(そちらはQ/S/Hで明示的に閉じる)。
            InputAction::UnboundKey
                if game.status == GameStatus::Paused && app.pause_overlay == PauseOverlay::None =>
            {
                game.toggle_pause();
            }
            InputAction::UnboundKey => {}
            // M/EキーでのMUSIC/SE切り替えは、一時停止画面でのみ意味を持つ
            // (spec.md 1章・10章)。プレイ中(Paused以外)は無視する。
            InputAction::ToggleMusic => {
                if game.status == GameStatus::Paused {
                    app.settings.music_enabled = !app.settings.music_enabled;
                    // ここはScreen::Playing(かつPaused)確定なので、タイトル用
                    // BGMは触れず(既に無音のはず)、プレイ中BGMのみ即時反映する。
                    app.gameplay_music_enabled
                        .store(app.settings.music_enabled, Ordering::Relaxed);
                    app.settings.save();
                }
            }
            InputAction::ToggleSe => {
                if game.status == GameStatus::Paused {
                    app.settings.se_enabled = !app.settings.se_enabled;
                    app.se_enabled
                        .store(app.settings.se_enabled, Ordering::Relaxed);
                    app.settings.save();
                }
            }
            // S/Hキーでの設定/ヘルプ画面オーバーレイ表示。プレイ中に押した場合は
            // 自動的に一時停止してからオーバーレイを開く。同じキーの再入力で閉じる
            // (閉じても一時停止状態はそのまま、Pキーで別途再開する)。
            InputAction::OpenSettings => {
                if game.status == GameStatus::Playing {
                    game.toggle_pause();
                }
                if game.status == GameStatus::Paused {
                    app.pause_overlay = if app.pause_overlay == PauseOverlay::Settings {
                        PauseOverlay::None
                    } else {
                        // 設定画面では盤面の前提(配分率・速度・列数)を変えられる
                        // ため、開いた時点で巻き戻し履歴は捨てる(#233)。
                        app.rewind_history.clear();
                        PauseOverlay::Settings
                    };
                }
            }
            InputAction::OpenHelp => {
                if game.status == GameStatus::Playing {
                    game.toggle_pause();
                }
                if game.status == GameStatus::Paused {
                    app.pause_overlay = if app.pause_overlay == PauseOverlay::Help {
                        PauseOverlay::None
                    } else {
                        PauseOverlay::Help
                    };
                }
            }
            // 設定オーバーレイ表示中は上下キー/Spaceを選択操作として扱う(タイトル画面
            // のScreen::Settingsと同じ操作感)。
            InputAction::FaceUp if app.pause_overlay == PauseOverlay::Settings => {
                app.settings_selection = app.settings_selection.cycle_back();
            }
            InputAction::FaceDown if app.pause_overlay == PauseOverlay::Settings => {
                app.settings_selection = app.settings_selection.cycle();
            }
            InputAction::Drill if app.pause_overlay == PauseOverlay::Settings => {
                match app.settings_selection {
                    ui::render::SettingsChoice::Music => {
                        app.settings.music_enabled = !app.settings.music_enabled;
                        app.gameplay_music_enabled
                            .store(app.settings.music_enabled, Ordering::Relaxed);
                        app.settings.save();
                    }
                    ui::render::SettingsChoice::Se => {
                        app.settings.se_enabled = !app.settings.se_enabled;
                        app.se_enabled
                            .store(app.settings.se_enabled, Ordering::Relaxed);
                        app.settings.save();
                    }
                    // 調査用のブロック状態遷移ログのON/OFF。一時停止中の
                    // オーバーレイからは、稼働中のgameへも即座に反映する
                    // (無効化時は記録を止め、有効化時は新規にログを開き直す)。
                    ui::render::SettingsChoice::DebugLogEnabled => {
                        app.settings.debug_log_enabled = !app.settings.debug_log_enabled;
                        // ログを開き直す前に履歴を捨てる(#233)。古いスナップ
                        // ショットは差し替え前のログ接続を掴んだままのため。
                        app.rewind_history.clear();
                        game.refresh_debug_log(app.settings.debug_log_enabled);
                        app.settings.save();
                    }
                    // 配分率・色数・落下速度・回避硬直時間・音量は←→で調整するので、
                    // Spaceは無効(トグル対象ではない)。
                    ui::render::SettingsChoice::MusicVolume
                    | ui::render::SettingsChoice::SeVolume
                    | ui::render::SettingsChoice::RockRate
                    | ui::render::SettingsChoice::AirRate
                    | ui::render::SettingsChoice::StarRate
                    | ui::render::SettingsChoice::DiamondRate
                    | ui::render::SettingsChoice::ItemClearAboveRate
                    | ui::render::SettingsChoice::ItemUnifyColorsRate
                    | ui::render::SettingsChoice::ItemStarifyScreenRate
                    | ui::render::SettingsChoice::ColorCount
                    | ui::render::SettingsChoice::ColorClusterRate
                    | ui::render::SettingsChoice::FieldWidth
                    | ui::render::SettingsChoice::BlockFallSpeed
                    | ui::render::SettingsChoice::PlayerFallSpeed
                    | ui::render::SettingsChoice::ShakeDuration
                    | ui::render::SettingsChoice::MoveSpeed
                    | ui::render::SettingsChoice::DodgeRecoveryMs
                    | ui::render::SettingsChoice::BombRate
                    | ui::render::SettingsChoice::BombFuse
                    | ui::render::SettingsChoice::AttackBlocksPerRock
                    | ui::render::SettingsChoice::AttackRocksPerWaveMax
                    | ui::render::SettingsChoice::ChainVanishInterval
                    | ui::render::SettingsChoice::RewindStockMax => {}
                }
            }
            // MUSIC/SEのトグルは←→キーでも行える。トグルなので方向は問わず、
            // 押されたら反転する。
            InputAction::MoveLeft | InputAction::MoveRight
                if app.pause_overlay == PauseOverlay::Settings
                    && matches!(
                        app.settings_selection,
                        ui::render::SettingsChoice::Music
                            | ui::render::SettingsChoice::Se
                            | ui::render::SettingsChoice::DebugLogEnabled
                    ) =>
            {
                match app.settings_selection {
                    ui::render::SettingsChoice::Music => {
                        app.settings.music_enabled = !app.settings.music_enabled;
                        app.gameplay_music_enabled
                            .store(app.settings.music_enabled, Ordering::Relaxed);
                    }
                    ui::render::SettingsChoice::Se => {
                        app.settings.se_enabled = !app.settings.se_enabled;
                        app.se_enabled
                            .store(app.settings.se_enabled, Ordering::Relaxed);
                    }
                    ui::render::SettingsChoice::DebugLogEnabled => {
                        app.settings.debug_log_enabled = !app.settings.debug_log_enabled;
                        // 上と同じ理由で、ログを開き直す前に履歴を捨てる(#233)。
                        app.rewind_history.clear();
                        game.refresh_debug_log(app.settings.debug_log_enabled);
                    }
                    _ => {}
                }
                app.settings.save();
            }
            // MUSIC/SEの音量調整(#224)。ON/OFFとは別に、アプリ内部のミックス
            // ゲインだけを変える。SE音量は変更のたびに確認用サンプルSE(酸素
            // カプセル取得音)を1回鳴らす。
            InputAction::MoveLeft | InputAction::MoveRight
                if app.pause_overlay == PauseOverlay::Settings
                    && matches!(
                        app.settings_selection,
                        ui::render::SettingsChoice::MusicVolume
                            | ui::render::SettingsChoice::SeVolume
                    ) =>
            {
                let increase = action == InputAction::MoveRight;
                match app.settings_selection {
                    ui::render::SettingsChoice::MusicVolume => {
                        app.settings.music_volume_percent = adjust_sound_volume_percent(
                            app.settings.music_volume_percent,
                            increase,
                        );
                        app.music_volume_percent
                            .store(app.settings.music_volume_percent, Ordering::Relaxed);
                    }
                    ui::render::SettingsChoice::SeVolume => {
                        app.settings.se_volume_percent =
                            adjust_sound_volume_percent(app.settings.se_volume_percent, increase);
                        if app.settings.se_enabled
                            && let Some(m) = app.mixer.as_ref()
                        {
                            audio::sfx::play_oxygen_pickup(
                                m,
                                audio::sfx::se_gain(app.settings.se_volume_percent),
                            );
                        }
                    }
                    _ => {}
                }
                app.settings.save();
            }
            // ブロック落下速度・キャラ落下速度・回避硬直時間の調整。
            // 配分率・色数と異なり盤面の書き換えを伴わないため、即座にgameへ反映してよい。
            InputAction::MoveLeft | InputAction::MoveRight
                if app.pause_overlay == PauseOverlay::Settings
                    && matches!(
                        app.settings_selection,
                        ui::render::SettingsChoice::BlockFallSpeed
                            | ui::render::SettingsChoice::PlayerFallSpeed
                            | ui::render::SettingsChoice::ShakeDuration
                            | ui::render::SettingsChoice::MoveSpeed
                            | ui::render::SettingsChoice::DodgeRecoveryMs
                            | ui::render::SettingsChoice::BombRate
                            | ui::render::SettingsChoice::BombFuse
                            | ui::render::SettingsChoice::AttackBlocksPerRock
                            | ui::render::SettingsChoice::AttackRocksPerWaveMax
                            | ui::render::SettingsChoice::ChainVanishInterval
                            | ui::render::SettingsChoice::RewindStockMax
                    ) =>
            {
                let increase = action == InputAction::MoveRight;
                match app.settings_selection {
                    ui::render::SettingsChoice::BlockFallSpeed => {
                        app.settings.block_fall_tick_ms =
                            adjust_fall_speed_ms(app.settings.block_fall_tick_ms, increase);
                        game.set_block_fall_tick_ms(app.settings.block_fall_tick_ms);
                    }
                    ui::render::SettingsChoice::PlayerFallSpeed => {
                        app.settings.player_fall_tick_ms =
                            adjust_fall_speed_ms(app.settings.player_fall_tick_ms, increase);
                        game.set_player_fall_tick_ms(app.settings.player_fall_tick_ms);
                    }
                    ui::render::SettingsChoice::ShakeDuration => {
                        app.settings.shake_duration_ms =
                            adjust_shake_duration_ms(app.settings.shake_duration_ms, increase);
                        game.set_shake_duration_ms(app.settings.shake_duration_ms);
                    }
                    ui::render::SettingsChoice::MoveSpeed => {
                        app.settings.move_cooldown_ms =
                            adjust_move_cooldown_ms(app.settings.move_cooldown_ms, increase);
                        game.set_move_cooldown_ms(app.settings.move_cooldown_ms);
                    }
                    ui::render::SettingsChoice::DodgeRecoveryMs => {
                        app.settings.dodge_recovery_ms =
                            adjust_dodge_recovery_ms(app.settings.dodge_recovery_ms, increase);
                        game.set_dodge_recovery_ms(app.settings.dodge_recovery_ms);
                    }
                    ui::render::SettingsChoice::BombRate => {
                        app.settings.bomb_spawn_rate_percent = adjust_bomb_rate_percent(
                            app.settings.bomb_spawn_rate_percent,
                            increase,
                        );
                        game.set_bomb_spawn_rate_percent(app.settings.bomb_spawn_rate_percent);
                    }
                    ui::render::SettingsChoice::BombFuse => {
                        app.settings.bomb_fuse_ms =
                            adjust_bomb_fuse_ms(app.settings.bomb_fuse_ms, increase);
                        game.set_bomb_fuse_ms(app.settings.bomb_fuse_ms);
                    }
                    ui::render::SettingsChoice::AttackBlocksPerRock => {
                        app.settings.attack_blocks_per_rock = adjust_attack_blocks_per_rock(
                            app.settings.attack_blocks_per_rock,
                            increase,
                        );
                        game.set_attack_blocks_per_rock(app.settings.attack_blocks_per_rock);
                    }
                    ui::render::SettingsChoice::AttackRocksPerWaveMax => {
                        app.settings.attack_rocks_per_wave_max = adjust_attack_rocks_per_wave_max(
                            app.settings.attack_rocks_per_wave_max,
                            increase,
                        );
                        game.set_attack_rocks_per_wave_max(app.settings.attack_rocks_per_wave_max);
                    }
                    ui::render::SettingsChoice::ChainVanishInterval => {
                        app.settings.chain_vanish_interval_ms = adjust_chain_vanish_interval_ms(
                            app.settings.chain_vanish_interval_ms,
                            increase,
                        );
                        game.set_chain_vanish_interval_ms(app.settings.chain_vanish_interval_ms);
                    }
                    ui::render::SettingsChoice::RewindStockMax => {
                        app.settings.rewind_stock_max =
                            adjust_rewind_stock_max(app.settings.rewind_stock_max, increase);
                        game.set_rewind_stock_max(app.settings.rewind_stock_max);
                    }
                    _ => {}
                }
                app.settings.save();
            }
            // フィールド幅(列数)の調整。盤面の列数そのものを変えるため
            // 現在の盤面には反映できず、次回の新規ゲーム開始時にのみ適用される。
            InputAction::MoveLeft | InputAction::MoveRight
                if app.pause_overlay == PauseOverlay::Settings
                    && app.settings_selection == ui::render::SettingsChoice::FieldWidth =>
            {
                let increase = action == InputAction::MoveRight;
                app.settings.field_width = adjust_field_width(app.settings.field_width, increase);
                app.settings.save();
            }
            // Xブロック/AIR/スター/ダイヤの配分率・色数調整。プレイ中なので、
            // 既に画面に見えている範囲は変えず、十分先(画面外)から新しい配分率を反映する。
            InputAction::MoveLeft | InputAction::MoveRight
                if app.pause_overlay == PauseOverlay::Settings
                    && matches!(
                        app.settings_selection,
                        ui::render::SettingsChoice::RockRate
                            | ui::render::SettingsChoice::AirRate
                            | ui::render::SettingsChoice::StarRate
                            | ui::render::SettingsChoice::DiamondRate
                            | ui::render::SettingsChoice::ItemClearAboveRate
                            | ui::render::SettingsChoice::ItemUnifyColorsRate
                            | ui::render::SettingsChoice::ItemStarifyScreenRate
                            | ui::render::SettingsChoice::ColorCount
                            | ui::render::SettingsChoice::ColorClusterRate
                    ) =>
            {
                let increase = action == InputAction::MoveRight;
                adjust_spawn_rate_setting(&mut app.settings, app.settings_selection, increase);
                app.settings.save();
                let from_row = game.player.row + SPAWN_RATE_REROLL_SAFE_MARGIN_ROWS;
                game.reroll_spawn_rates_from(
                    from_row,
                    app.settings.rock_spawn_rate_percent,
                    app.settings.air_spawn_rate_percent,
                    app.settings.star_spawn_rate_percent,
                    app.settings.diamond_spawn_rate_percent,
                    app.settings.item_clear_above_rate_percent,
                    app.settings.item_unify_colors_rate_percent,
                    app.settings.item_starify_screen_rate_percent,
                    app.settings.color_count,
                    app.settings.color_cluster_rate_percent,
                );
            }
            // GameOverダイアログ中は上下キー/Spaceを選択操作として扱う
            // (タイトルへ戻るか、その場から復活して再開するかを選ぶ)。
            InputAction::FaceUp | InputAction::FaceDown if game.status == GameStatus::GameOver => {
                game.toggle_game_over_selection();
            }
            InputAction::Confirm if game.status == GameStatus::GameOver => {
                match game.game_over_selection() {
                    GameOverChoice::BackToTitle => {
                        return Ok(Some(ScreenTransition::ToTitleDiscardingGame));
                    }
                    GameOverChoice::Revive => {
                        // 復活は「ここから仕切り直す」選択なので、死ぬ前へ
                        // 巻き戻せる履歴は残さない(#233)。
                        app.rewind_history.clear();
                        game.revive();
                    }
                }
            }
            InputAction::Confirm => {}
            // ルーム開始(Tab)はロビー画面専用の操作なので、プレイ中は何も起きない(#276)。
            InputAction::StartRoom => {}
            // 移動・向き・掘削の5操作は`apply_input`へ統一する(TERM独自拡張。
            // #218)。オートプレイの仮想入力と全く同じ経路を通ることで、
            // AIだけが使える裏口が生まれないようにする。人が実際に操作した
            // 時点でオートプレイは解除し、無敵も開始前の状態へ戻す。
            InputAction::MoveLeft
            | InputAction::MoveRight
            | InputAction::FaceUp
            | InputAction::FaceDown
            | InputAction::Drill => {
                // 人が操作した時点でオートプレイは解除する。無敵はGキーが
                // 単独で管理するため、ここでは変更しない(#221)。
                app.autopilot = None;
                let events = game.apply_input(action);
                handle_events(
                    &events,
                    app.mixer.as_ref(),
                    &app.se_enabled,
                    app.settings.se_volume_percent,
                );
            }
            // T: オートプレイのON/OFF。無敵は連動させず、Gキーの状態をそのまま
            // 残す(#221。AIが無敵に頼らず生き延びられるかをTだけで試せるように
            // するため。無人のアトラクトモードだけは安全策として無敵もONにする)。
            InputAction::DebugToggleAutopilot => {
                app.autopilot = match app.autopilot.take() {
                    Some(_) => None,
                    None => Some(autoplay::Autopilot::new(game.is_invincible())),
                };
            }
            // G: 無敵の単独トグル。オートプレイとは独立して切り替えられる。
            InputAction::DebugToggleInvincible => {
                game.set_invincible(!game.is_invincible());
            }
            InputAction::DebugUnifyNearbyColors => {
                let events = game.debug_unify_nearby_colors();
                handle_events(
                    &events,
                    app.mixer.as_ref(),
                    &app.se_enabled,
                    app.settings.se_volume_percent,
                );
            }
            InputAction::DebugAddLife => game.debug_add_life(),
            InputAction::DebugFillAir => game.debug_fill_air(),
            InputAction::DebugClearAbovePlayer => game.debug_clear_above_player(),
            InputAction::DebugStarifyVisibleScreen => game.debug_starify_visible_screen(),
            InputAction::DebugPlaceBomb => game.debug_place_bomb(),
            InputAction::DebugReceiveOpponentAttack => game.debug_receive_opponent_attack(),
            // 速度系デバッグショートカット([ ] - = , .)。落下・揺れの速度は
            // スナップショット(Game丸ごと)にも含まれるため、変更前の履歴へ
            // 戻ると変更を取り消したのと同じことになる。混乱を避けるため、
            // 速度を変えた時点で履歴を捨てる(#233)。
            InputAction::DebugBlockFallSlower => {
                app.rewind_history.clear();
                game.debug_adjust_block_fall_speed(false);
                app.settings.block_fall_tick_ms = game.block_fall_tick_ms();
                app.settings.save();
            }
            InputAction::DebugBlockFallFaster => {
                app.rewind_history.clear();
                game.debug_adjust_block_fall_speed(true);
                app.settings.block_fall_tick_ms = game.block_fall_tick_ms();
                app.settings.save();
            }
            InputAction::DebugPlayerFallSlower => {
                app.rewind_history.clear();
                game.debug_adjust_player_fall_speed(false);
                app.settings.player_fall_tick_ms = game.player_fall_tick_ms();
                app.settings.save();
            }
            InputAction::DebugPlayerFallFaster => {
                app.rewind_history.clear();
                game.debug_adjust_player_fall_speed(true);
                app.settings.player_fall_tick_ms = game.player_fall_tick_ms();
                app.settings.save();
            }
            InputAction::DebugShakeDurationLonger => {
                app.rewind_history.clear();
                game.debug_adjust_shake_duration(true);
                app.settings.shake_duration_ms = game.shake_duration_ms();
                app.settings.save();
            }
            InputAction::DebugShakeDurationShorter => {
                app.rewind_history.clear();
                game.debug_adjust_shake_duration(false);
                app.settings.shake_duration_ms = game.shake_duration_ms();
                app.settings.save();
            }
        }
    }

    // 巻き戻しを開始したフレームはゲームを進めず描画もしない(次フレームから
    // 巻き戻し専用の処理へ入る)。
    if app.rewind_session.is_none() {
        // オートプレイ(TERM独自拡張。#218)。盤面から決めた仮想入力を、人の
        // 操作と同じ経路へ流し込む。GameOver中は何も返さないため、ダイアログは
        // 人間がプレイしたときと同じように表示されたまま操作を待つ(#225)。
        if let Some(pilot) = app.autopilot.as_mut() {
            for action in pilot.decide(game) {
                let events = game.apply_input(action);
                handle_events(
                    &events,
                    app.mixer.as_ref(),
                    &app.se_enabled,
                    app.settings.se_volume_percent,
                );
            }
        }

        let now = Instant::now();
        let delta = now.duration_since(app.last_tick);
        app.last_tick = now;

        let events = game.update(delta.min(Duration::from_millis(250)));
        handle_events(
            &events,
            app.mixer.as_ref(),
            &app.se_enabled,
            app.settings.se_volume_percent,
        );

        // 巻き戻し用スナップショットの蓄積(TERM独自拡張。#233)。記録の可否・
        // 間隔の判定はRewindHistory側が持つ(ここは毎フレーム呼ぶだけ)。
        app.rewind_history.maybe_capture(game);

        let music_on = app.gameplay_music_enabled.load(Ordering::Relaxed);
        let se_on = app.se_enabled.load(Ordering::Relaxed);
        let autoplay_on = app.autopilot.is_some();
        terminal.draw(|frame| {
            ui::render::draw(frame, game, music_on, se_on, autoplay_on);
            // 一時停止中の設定/ヘルプオーバーレイ。Screen::Playingのまま
            // Gameを手放さずに上へ重ね描きするだけで、専用のScreen遷移は行わない。
            match app.pause_overlay {
                PauseOverlay::None => {}
                PauseOverlay::Settings => ui::render::draw_settings(
                    frame,
                    app.settings_selection,
                    music_on,
                    se_on,
                    app.settings.music_volume_percent,
                    app.settings.se_volume_percent,
                    app.settings.rock_spawn_rate_percent,
                    app.settings.air_spawn_rate_percent,
                    app.settings.star_spawn_rate_percent,
                    app.settings.diamond_spawn_rate_percent,
                    app.settings.item_clear_above_rate_percent,
                    app.settings.item_unify_colors_rate_percent,
                    app.settings.item_starify_screen_rate_percent,
                    app.settings.color_count,
                    app.settings.color_cluster_rate_percent,
                    app.settings.field_width,
                    app.settings.block_fall_tick_ms,
                    app.settings.player_fall_tick_ms,
                    app.settings.shake_duration_ms,
                    app.settings.move_cooldown_ms,
                    app.settings.dodge_recovery_ms,
                    app.settings.bomb_spawn_rate_percent,
                    app.settings.bomb_fuse_ms,
                    app.settings.attack_blocks_per_rock,
                    app.settings.attack_rocks_per_wave_max,
                    app.settings.debug_log_enabled,
                    app.settings.chain_vanish_interval_ms,
                    app.settings.rewind_stock_max,
                    false,
                ),
                PauseOverlay::Help => ui::render::draw_help(frame, None, false),
            }
        })?;
    }

    Ok(None)
}

/// 対戦中(`Screen::Battle`)に受け付ける入力の分類(#252。spec.md 12.5)。
///
/// 対戦中に使えない操作のための個別の無効化フラグは持たず、ここに挙げたもの以外を
/// `Ignored`にすることで無効化を表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BattleInput {
    /// 対戦を中断してタイトルへ戻る。
    Quit,
    /// このtickの自分の入力として`lockstep::run_tick`へ渡す5操作。
    Local(InputAction),
    /// MUSICのトグル。音声はローカル専用でシミュレーションに影響しないため、
    /// 通常プレイのPaused限定と異なり対戦中は常時受け付ける。
    ToggleMusic,
    /// SEのトグル。扱いは`ToggleMusic`と同じ。
    ToggleSe,
    /// 対戦中は無視する操作(一時停止・巻き戻し・設定/ヘルプ・デバッグ系等)。
    Ignored,
}

/// 入力を対戦中の扱い(`BattleInput`)へ振り分ける。
fn classify_battle_input(action: InputAction) -> BattleInput {
    match action {
        InputAction::MoveLeft
        | InputAction::MoveRight
        | InputAction::FaceUp
        | InputAction::FaceDown
        | InputAction::Drill => BattleInput::Local(action),
        InputAction::ToggleMusic => BattleInput::ToggleMusic,
        InputAction::ToggleSe => BattleInput::ToggleSe,
        InputAction::Quit => BattleInput::Quit,
        _ => BattleInput::Ignored,
    }
}

/// このフレームにキューされた入力列から、このtickで自分の入力として採用する1つを選ぶ。
/// 2つ目以降は次tickへ持ち越さず捨てる(1tickにつき高々1アクション。spec.md 12.2)。
fn first_local_action(actions: &[InputAction]) -> Option<InputAction> {
    actions
        .iter()
        .find_map(|&action| match classify_battle_input(action) {
            BattleInput::Local(local) => Some(local),
            _ => None,
        })
}

/// 対戦中(`Screen::Battle`)の1フレーム(#252。spec.md 12章)。
///
/// 通常プレイの`tick_playing`とは独立した関数にしている(あちらへ対戦用の分岐を混ぜると
/// さらに肥大化し、通常プレイ側の挙動を壊すリスクも生むため)。ゲームの進行は実測フレーム
/// 時間を`NET_TICK_MS`固定tickへ量子化して`BattleState::advance`へ任せ、ここは入力の
/// 仕分けと描画だけを行う。
pub fn tick_battle(
    app: &mut App,
    state: &mut BattleState,
    terminal: &mut ratatui::DefaultTerminal,
) -> io::Result<Option<ScreenTransition>> {
    let actions = input::poll_input_batch(FRAME_INTERVAL_MS)?;

    // 決着後は結果表示だけの画面になる(#256)。ここを抜ける操作は「タイトルへ戻る」
    // のみで、盤面の操作・音声トグルはもう意味を持たない。
    if state.outcome().is_some() && actions.iter().any(|&action| leaves_battle_result(action)) {
        state.notify_bye();
        return Ok(Some(ScreenTransition::ToTitleDiscardingGame));
    }

    for &action in &actions {
        match classify_battle_input(action) {
            // 対戦を中断してタイトルへ戻る。相手を待たせないよう、抜けることを
            // 伝えてから戻る(#256。相手側は不戦勝として決着する)。
            BattleInput::Quit => {
                state.notify_bye();
                return Ok(Some(ScreenTransition::ToTitleDiscardingGame));
            }
            BattleInput::ToggleMusic => {
                app.settings.music_enabled = !app.settings.music_enabled;
                // 対戦画面ではタイトル用BGMは鳴らないため、プレイ中BGMのみ即時反映する。
                app.gameplay_music_enabled
                    .store(app.settings.music_enabled, Ordering::Relaxed);
                app.settings.save();
            }
            BattleInput::ToggleSe => {
                app.settings.se_enabled = !app.settings.se_enabled;
                app.se_enabled
                    .store(app.settings.se_enabled, Ordering::Relaxed);
                app.settings.save();
            }
            // 自分の操作は1tickにつき高々1つのため、`first_local_action`でまとめて選ぶ。
            BattleInput::Local(_) | BattleInput::Ignored => {}
        }
    }

    // 決着後は`advance`が何もしないため、自分の入力もそこで受け付けられなくなる。
    let now = Instant::now();
    let delta = now.duration_since(app.last_tick);
    app.last_tick = now;
    state.advance(delta, first_local_action(&actions));

    let music_on = app.gameplay_music_enabled.load(Ordering::Relaxed);
    let se_on = app.se_enabled.load(Ordering::Relaxed);
    let outcome = state.outcome();
    terminal.draw(|frame| {
        // N人対戦(#273)でも、この段階では相手パネルにindex 1の1人だけを表示する
        // (N人分の表示は#270/#276の範囲)。
        ui::render::draw_battle(
            frame,
            &state.games[0],
            &state.games[1],
            &state.player_names[1],
            music_on,
            se_on,
            outcome,
        )
    })?;

    Ok(None)
}

/// 決着後の結果表示から抜ける操作か(#256。設計書6節「Confirm(EnterまたはSpace)
/// またはQuitキー」)。Spaceは通常プレイでは一時停止だが、対戦には一時停止が無いため
/// ここでは確定の意味で受け付ける。
fn leaves_battle_result(action: InputAction) -> bool {
    matches!(
        action,
        InputAction::Confirm | InputAction::TogglePause | InputAction::Quit
    )
}

/// 対戦相手を探すロビー(`Screen::NetworkLobby`)の1フレーム(#256。spec.md 12.1)。
///
/// 探索・招待・接続の状態遷移は`LobbyState::update`が持ち、ここは入力の取り込みと
/// 描画、遷移結果の受け渡しだけを行う。
pub fn tick_network_lobby(
    app: &mut App,
    state: &mut LobbyState,
    terminal: &mut ratatui::DefaultTerminal,
) -> io::Result<Option<ScreenTransition>> {
    let actions = input::poll_input_batch(FRAME_INTERVAL_MS)?;

    // 自分がホスト役になった場合に相手へ強制適用する設定(spec.md 12.2)。コースは
    // 前回選んだもの(モードセレクトの初期選択と同じ)を使う。
    let config = BattleConfig::from_settings(&app.settings, app.settings.last_course_depth_m);

    match state.update(&actions, config) {
        LobbyOutcome::Leave => return Ok(Some(ScreenTransition::ToTitle)),
        LobbyOutcome::Battle(battle) => return Ok(Some(ScreenTransition::ToBattle(battle))),
        LobbyOutcome::Stay => {}
    }

    terminal.draw(|frame| ui::render::draw_network_lobby(frame, state))?;

    Ok(None)
}

/// 設定画面(タイトルから開く独立画面)の1フレーム。
///
/// 元の実装はループ内で`screen`へ直接代入しており、タイトルへ戻ると決めた後も同じ
/// フレームに溜まっていた残りの入力をそのまま処理していた。挙動を変えないため、
/// 途中でreturnせず遷移をローカル変数に控えてループを最後まで回す。
pub fn tick_settings_screen(
    app: &mut App,
    terminal: &mut ratatui::DefaultTerminal,
) -> io::Result<Option<ScreenTransition>> {
    let music_on = app.settings.music_enabled;
    let se_on = app.settings.se_enabled;
    terminal.draw(|frame| {
        ui::render::draw_settings(
            frame,
            app.settings_selection,
            music_on,
            se_on,
            app.settings.music_volume_percent,
            app.settings.se_volume_percent,
            app.settings.rock_spawn_rate_percent,
            app.settings.air_spawn_rate_percent,
            app.settings.star_spawn_rate_percent,
            app.settings.diamond_spawn_rate_percent,
            app.settings.item_clear_above_rate_percent,
            app.settings.item_unify_colors_rate_percent,
            app.settings.item_starify_screen_rate_percent,
            app.settings.color_count,
            app.settings.color_cluster_rate_percent,
            app.settings.field_width,
            app.settings.block_fall_tick_ms,
            app.settings.player_fall_tick_ms,
            app.settings.shake_duration_ms,
            app.settings.move_cooldown_ms,
            app.settings.dodge_recovery_ms,
            app.settings.bomb_spawn_rate_percent,
            app.settings.bomb_fuse_ms,
            app.settings.attack_blocks_per_rock,
            app.settings.attack_rocks_per_wave_max,
            app.settings.debug_log_enabled,
            app.settings.chain_vanish_interval_ms,
            app.settings.rewind_stock_max,
            true,
        )
    })?;

    let mut transition = None;

    // 設定画面もpoll_input_batchを使う(FaceUp/FaceDown=選択切替、Drill=トグル、
    // MoveLeft/MoveRight=配分率調整、Quit=タイトルへ戻る、と既存のInputActionを
    // そのまま再利用できるため)。
    for action in input::poll_input_batch(FRAME_INTERVAL_MS)? {
        match action {
            InputAction::Quit => transition = Some(ScreenTransition::ToTitle),
            InputAction::FaceUp => {
                app.settings_selection = app.settings_selection.cycle_back();
            }
            InputAction::FaceDown => {
                app.settings_selection = app.settings_selection.cycle();
            }
            InputAction::Drill => match app.settings_selection {
                ui::render::SettingsChoice::Music => {
                    app.settings.music_enabled = !app.settings.music_enabled;
                    app.gameplay_music_enabled
                        .store(app.settings.music_enabled, Ordering::Relaxed);
                    app.settings.save();
                }
                ui::render::SettingsChoice::Se => {
                    app.settings.se_enabled = !app.settings.se_enabled;
                    app.se_enabled
                        .store(app.settings.se_enabled, Ordering::Relaxed);
                    app.settings.save();
                }
                // 調査用のブロック状態遷移ログのON/OFF。このScreen::Settings
                // (タイトルから開く独立画面)にはgameが無いため、次回のゲーム
                // 開始時(refresh_debug_log呼び出し時)に反映される。
                ui::render::SettingsChoice::DebugLogEnabled => {
                    app.settings.debug_log_enabled = !app.settings.debug_log_enabled;
                    app.settings.save();
                }
                ui::render::SettingsChoice::MusicVolume
                | ui::render::SettingsChoice::SeVolume
                | ui::render::SettingsChoice::RockRate
                | ui::render::SettingsChoice::AirRate
                | ui::render::SettingsChoice::StarRate
                | ui::render::SettingsChoice::DiamondRate
                | ui::render::SettingsChoice::ItemClearAboveRate
                | ui::render::SettingsChoice::ItemUnifyColorsRate
                | ui::render::SettingsChoice::ItemStarifyScreenRate
                | ui::render::SettingsChoice::ColorCount
                | ui::render::SettingsChoice::ColorClusterRate
                | ui::render::SettingsChoice::FieldWidth
                | ui::render::SettingsChoice::BlockFallSpeed
                | ui::render::SettingsChoice::PlayerFallSpeed
                | ui::render::SettingsChoice::ShakeDuration
                | ui::render::SettingsChoice::MoveSpeed
                | ui::render::SettingsChoice::DodgeRecoveryMs
                | ui::render::SettingsChoice::BombRate
                | ui::render::SettingsChoice::BombFuse
                | ui::render::SettingsChoice::AttackBlocksPerRock
                | ui::render::SettingsChoice::AttackRocksPerWaveMax
                | ui::render::SettingsChoice::ChainVanishInterval
                | ui::render::SettingsChoice::RewindStockMax => {}
            },
            // MUSIC/SEのトグルはSpace(TogglePause)・←→キーでも行える(ヘルプ表示
            // 「Spaceか←→でトグル」と一致させるため)。一時停止中のオーバーレイの
            // Spaceは別途「閉じて再開する」処理を持つため対象外。方向は問わず反転する。
            InputAction::TogglePause | InputAction::MoveLeft | InputAction::MoveRight
                if matches!(
                    app.settings_selection,
                    ui::render::SettingsChoice::Music
                        | ui::render::SettingsChoice::Se
                        | ui::render::SettingsChoice::DebugLogEnabled
                ) =>
            {
                match app.settings_selection {
                    ui::render::SettingsChoice::Music => {
                        app.settings.music_enabled = !app.settings.music_enabled;
                        app.gameplay_music_enabled
                            .store(app.settings.music_enabled, Ordering::Relaxed);
                    }
                    ui::render::SettingsChoice::Se => {
                        app.settings.se_enabled = !app.settings.se_enabled;
                        app.se_enabled
                            .store(app.settings.se_enabled, Ordering::Relaxed);
                    }
                    ui::render::SettingsChoice::DebugLogEnabled => {
                        app.settings.debug_log_enabled = !app.settings.debug_log_enabled;
                    }
                    _ => {}
                }
                app.settings.save();
            }
            // MUSIC/SEの音量調整(#224)。一時停止オーバーレイと同じ挙動で、
            // SE音量は変更のたびに確認用サンプルSEを1回鳴らす。
            InputAction::MoveLeft | InputAction::MoveRight
                if matches!(
                    app.settings_selection,
                    ui::render::SettingsChoice::MusicVolume | ui::render::SettingsChoice::SeVolume
                ) =>
            {
                let increase = action == InputAction::MoveRight;
                match app.settings_selection {
                    ui::render::SettingsChoice::MusicVolume => {
                        app.settings.music_volume_percent = adjust_sound_volume_percent(
                            app.settings.music_volume_percent,
                            increase,
                        );
                        app.music_volume_percent
                            .store(app.settings.music_volume_percent, Ordering::Relaxed);
                    }
                    ui::render::SettingsChoice::SeVolume => {
                        app.settings.se_volume_percent =
                            adjust_sound_volume_percent(app.settings.se_volume_percent, increase);
                        if app.settings.se_enabled
                            && let Some(m) = app.mixer.as_ref()
                        {
                            audio::sfx::play_oxygen_pickup(
                                m,
                                audio::sfx::se_gain(app.settings.se_volume_percent),
                            );
                        }
                    }
                    _ => {}
                }
                app.settings.save();
            }
            InputAction::MoveLeft | InputAction::MoveRight
                if matches!(
                    app.settings_selection,
                    ui::render::SettingsChoice::RockRate
                        | ui::render::SettingsChoice::AirRate
                        | ui::render::SettingsChoice::StarRate
                        | ui::render::SettingsChoice::DiamondRate
                        | ui::render::SettingsChoice::ItemClearAboveRate
                        | ui::render::SettingsChoice::ItemUnifyColorsRate
                        | ui::render::SettingsChoice::ItemStarifyScreenRate
                        | ui::render::SettingsChoice::ColorCount
                        | ui::render::SettingsChoice::ColorClusterRate
                        | ui::render::SettingsChoice::FieldWidth
                        | ui::render::SettingsChoice::BlockFallSpeed
                        | ui::render::SettingsChoice::PlayerFallSpeed
                        | ui::render::SettingsChoice::ShakeDuration
                        | ui::render::SettingsChoice::MoveSpeed
                        | ui::render::SettingsChoice::DodgeRecoveryMs
                        | ui::render::SettingsChoice::BombRate
                        | ui::render::SettingsChoice::BombFuse
                        | ui::render::SettingsChoice::AttackBlocksPerRock
                        | ui::render::SettingsChoice::AttackRocksPerWaveMax
                        | ui::render::SettingsChoice::ChainVanishInterval
                        | ui::render::SettingsChoice::RewindStockMax
                ) =>
            {
                let increase = action == InputAction::MoveRight;
                if adjust_spawn_rate_setting(&mut app.settings, app.settings_selection, increase) {
                    app.settings.save();
                    continue;
                }
                match app.settings_selection {
                    ui::render::SettingsChoice::FieldWidth => {
                        app.settings.field_width =
                            adjust_field_width(app.settings.field_width, increase);
                    }
                    ui::render::SettingsChoice::BlockFallSpeed => {
                        app.settings.block_fall_tick_ms =
                            adjust_fall_speed_ms(app.settings.block_fall_tick_ms, increase);
                    }
                    ui::render::SettingsChoice::PlayerFallSpeed => {
                        app.settings.player_fall_tick_ms =
                            adjust_fall_speed_ms(app.settings.player_fall_tick_ms, increase);
                    }
                    ui::render::SettingsChoice::ShakeDuration => {
                        app.settings.shake_duration_ms =
                            adjust_shake_duration_ms(app.settings.shake_duration_ms, increase);
                    }
                    ui::render::SettingsChoice::MoveSpeed => {
                        app.settings.move_cooldown_ms =
                            adjust_move_cooldown_ms(app.settings.move_cooldown_ms, increase);
                    }
                    ui::render::SettingsChoice::DodgeRecoveryMs => {
                        app.settings.dodge_recovery_ms =
                            adjust_dodge_recovery_ms(app.settings.dodge_recovery_ms, increase);
                    }
                    ui::render::SettingsChoice::BombRate => {
                        app.settings.bomb_spawn_rate_percent = adjust_bomb_rate_percent(
                            app.settings.bomb_spawn_rate_percent,
                            increase,
                        );
                    }
                    ui::render::SettingsChoice::BombFuse => {
                        app.settings.bomb_fuse_ms =
                            adjust_bomb_fuse_ms(app.settings.bomb_fuse_ms, increase);
                    }
                    ui::render::SettingsChoice::AttackBlocksPerRock => {
                        app.settings.attack_blocks_per_rock = adjust_attack_blocks_per_rock(
                            app.settings.attack_blocks_per_rock,
                            increase,
                        );
                    }
                    ui::render::SettingsChoice::AttackRocksPerWaveMax => {
                        app.settings.attack_rocks_per_wave_max = adjust_attack_rocks_per_wave_max(
                            app.settings.attack_rocks_per_wave_max,
                            increase,
                        );
                    }
                    ui::render::SettingsChoice::ChainVanishInterval => {
                        app.settings.chain_vanish_interval_ms = adjust_chain_vanish_interval_ms(
                            app.settings.chain_vanish_interval_ms,
                            increase,
                        );
                    }
                    ui::render::SettingsChoice::RewindStockMax => {
                        app.settings.rewind_stock_max =
                            adjust_rewind_stock_max(app.settings.rewind_stock_max, increase);
                    }
                    _ => {}
                }
                app.settings.save();
            }
            _ => {}
        }
    }

    Ok(transition)
}

/// ヘルプ画面(タイトルから開く独立画面)の1フレーム。
///
/// `tick_settings_screen`と同じく、元の実装はタイトルへ戻ると決めた後も同じフレームの
/// 残りの入力を処理していたため、遷移はローカル変数に控えてループを最後まで回す。
pub fn tick_help_screen(
    app: &mut App,
    terminal: &mut ratatui::DefaultTerminal,
) -> io::Result<Option<ScreenTransition>> {
    // 曲が最後まで自然に終わっていたら、再生中表示を消す。
    if app
        .help_jukebox_playing
        .as_ref()
        .is_some_and(|(_, preview)| preview.is_finished())
    {
        app.help_jukebox_playing = None;
    }

    let jukebox_state = ui::render::HelpJukeboxState {
        selection: app.help_jukebox_selection,
        playing: app.help_jukebox_playing.as_ref().map(|(idx, _)| *idx),
    };
    terminal.draw(|frame| ui::render::draw_help(frame, Some(&jukebox_state), true))?;

    let mut transition = None;

    // ヘルプ画面はEscキーでタイトルへ戻る。↑/↓で曲を選び、
    // X/Zで再生・停止するジュークボックス操作を持つ。
    for action in input::poll_input_batch(FRAME_INTERVAL_MS)? {
        match action {
            InputAction::Quit => {
                if let Some((_, preview)) = app.help_jukebox_playing.take() {
                    preview.stop();
                }
                transition = Some(ScreenTransition::ToTitle);
            }
            InputAction::FaceUp => {
                app.help_jukebox_selection = cycle_jukebox_selection(
                    app.help_jukebox_selection,
                    audio::bgm::JUKEBOX_TRACKS.len(),
                    false,
                );
            }
            InputAction::FaceDown => {
                app.help_jukebox_selection = cycle_jukebox_selection(
                    app.help_jukebox_selection,
                    audio::bgm::JUKEBOX_TRACKS.len(),
                    true,
                );
            }
            InputAction::Drill => {
                if let Some(m) = &app.mixer {
                    let already_playing_selection =
                        app.help_jukebox_playing.as_ref().map(|(idx, _)| *idx)
                            == Some(app.help_jukebox_selection);
                    if let Some((_, preview)) = app.help_jukebox_playing.take() {
                        preview.stop();
                    }
                    if !already_playing_selection {
                        let (_, track) = audio::bgm::JUKEBOX_TRACKS[app.help_jukebox_selection];
                        let preview = audio::bgm::start_jukebox_preview(
                            m,
                            track,
                            app.settings.music_volume_percent,
                        );
                        app.help_jukebox_playing = Some((app.help_jukebox_selection, preview));
                    }
                }
            }
            _ => {}
        }
    }

    Ok(transition)
}

/// モードセレクト画面の1フレーム。
///
/// `tick_settings_screen`と同じく、元の実装は画面を決めた後も同じフレームの残りの入力を
/// 処理していたため、遷移はローカル変数に控えてループを最後まで回す。
pub fn tick_mode_select(
    app: &mut App,
    terminal: &mut ratatui::DefaultTerminal,
) -> io::Result<Option<ScreenTransition>> {
    terminal.draw(|frame| ui::render::draw_mode_select(frame, app.mode_select_choice))?;

    let mut transition = None;

    // モードセレクト画面。↑/↓・←/→どちらでもイージー/ノーマルを
    // 切り替えられるようにする(設定画面の選択操作と揃える)。
    for action in input::poll_input_batch(FRAME_INTERVAL_MS)? {
        match action {
            InputAction::Quit => transition = Some(ScreenTransition::ToTitle),
            InputAction::FaceUp
            | InputAction::FaceDown
            | InputAction::MoveLeft
            | InputAction::MoveRight => {
                app.mode_select_choice = app.mode_select_choice.toggle();
            }
            InputAction::Confirm => {
                let depth_goal_m = app.mode_select_choice.depth_goal_m();
                app.settings.last_course_depth_m = depth_goal_m;
                app.settings.save();
                let game = start_new_game(app.rng.random(), &app.settings, depth_goal_m);
                // 新しい盤面なので前のゲームの履歴は引き継がない(#233)。
                app.rewind_history.clear();
                app.rewind_session = None;
                transition = Some(ScreenTransition::ToPlaying(Box::new(game)));
            }
            _ => {}
        }
    }

    Ok(transition)
}

/// タイトル画面の1フレーム。キー入力は1つだけ取り出すため(`poll_any_key`)、
/// 画面遷移が決まった時点でそのまま返す。
pub fn tick_title(
    app: &mut App,
    terminal: &mut ratatui::DefaultTerminal,
) -> io::Result<Option<ScreenTransition>> {
    terminal.draw(ui::render::draw_title)?;

    let title_frame_started = Instant::now();
    let key = input::poll_any_key(FRAME_INTERVAL_MS)?;
    // アトラクトモード(TERM独自拡張。#218)のアイドル計測。`poll_any_key`は
    // 最大FRAME_INTERVAL_MSだけ待つが、キーが来れば早く返るため、実際の
    // 経過時間で数える。どのキーであっても(画面遷移しないキーでも)
    // 押された時点でタイマーは0へ戻す。
    if key.is_some() {
        app.title_idle = Duration::ZERO;
    } else {
        app.title_idle += title_frame_started.elapsed();
    }

    if let Some(action) = key {
        match action {
            input::AnyKeyAction::Quit => return Ok(Some(ScreenTransition::Quit)),
            input::AnyKeyAction::OpenSettings => return Ok(Some(ScreenTransition::ToSettings)),
            input::AnyKeyAction::OpenHelp => return Ok(Some(ScreenTransition::ToHelp)),
            // 対戦相手を探すロビーへ(#256)。表示名の入力UIは作らず、毎回生成した
            // 名前をそのまま使う。探索用ソケットを確保できなかった場合(ポート使用中等)は
            // タイトルに留まる。
            input::AnyKeyAction::OpenNetworkLobby => {
                let my_name = format!("Player-{}", &uuid::Uuid::new_v4().to_string()[..4]);
                if let Ok(state) = LobbyState::new(my_name) {
                    return Ok(Some(ScreenTransition::ToNetworkLobby(Box::new(state))));
                }
            }
            input::AnyKeyAction::Advance => {
                app.mode_select_choice =
                    ui::render::CourseChoice::from_depth_goal_m(app.settings.last_course_depth_m);
                return Ok(Some(ScreenTransition::ToModeSelect));
            }
            // 画面遷移は起こさないが、アイドルタイマーのリセットは上で済んでいる。
            input::AnyKeyAction::Ignored => {}
        }
    } else if app.title_idle >= Duration::from_millis(ATTRACT_MODE_IDLE_MS) {
        // 放置されたので自動デモを始める。モードセレクトは挟まず、前回選んだ
        // コースでそのまま開始し、無敵ONのオートプレイに操作を任せる。
        let mut game = start_new_game(
            app.rng.random(),
            &app.settings,
            app.settings.last_course_depth_m,
        );
        // 無人で回り続けるデモなので、手動のTキー(#221で無敵と切り離した)とは
        // 違い、ここだけは安全策として無敵も自動でONにする。
        game.set_invincible(true);
        // 新規に作ったゲームなので、デモ開始前の無敵状態は常にOFF。
        app.autopilot = Some(autoplay::Autopilot::new(false));
        app.autopilot_is_attract_demo = true;
        app.title_idle = Duration::ZERO;
        // 新しい盤面なので前のゲームの履歴は引き継がない(#233)。
        app.rewind_history.clear();
        app.rewind_session = None;
        return Ok(Some(ScreenTransition::ToPlaying(Box::new(game))));
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battle_takes_the_five_gameplay_actions_as_the_local_input() {
        // 移動・向き変更・掘削の5操作だけがそのtickの自分の入力になる。
        for action in [
            InputAction::MoveLeft,
            InputAction::MoveRight,
            InputAction::FaceUp,
            InputAction::FaceDown,
            InputAction::Drill,
        ] {
            assert_eq!(
                classify_battle_input(action),
                BattleInput::Local(action),
                "{action:?}は対戦中の自分の入力として扱うはず"
            );
        }
    }

    #[test]
    fn battle_accepts_the_audio_toggles_and_the_quit_action() {
        // 音声トグルはローカル専用でシミュレーションに影響しないため常時受け付ける。
        // Escは対戦の中断(タイトルへ戻る)として扱う。
        assert_eq!(
            classify_battle_input(InputAction::ToggleMusic),
            BattleInput::ToggleMusic
        );
        assert_eq!(
            classify_battle_input(InputAction::ToggleSe),
            BattleInput::ToggleSe
        );
        assert_eq!(classify_battle_input(InputAction::Quit), BattleInput::Quit);
    }

    #[test]
    fn battle_ignores_pause_rewind_overlay_and_debug_actions() {
        // spec.md 12.5の無効化は、これらを握りつぶすことで実現する。
        for action in [
            InputAction::TogglePause,
            InputAction::Rewind,
            InputAction::OpenSettings,
            InputAction::OpenHelp,
            InputAction::Confirm,
            InputAction::UnboundKey,
            InputAction::DebugUnifyNearbyColors,
            InputAction::DebugAddLife,
            InputAction::DebugFillAir,
            InputAction::DebugClearAbovePlayer,
            InputAction::DebugStarifyVisibleScreen,
            InputAction::DebugPlaceBomb,
            InputAction::DebugReceiveOpponentAttack,
            InputAction::DebugToggleAutopilot,
            InputAction::DebugToggleInvincible,
            InputAction::DebugBlockFallSlower,
            InputAction::DebugBlockFallFaster,
            InputAction::DebugPlayerFallSlower,
            InputAction::DebugPlayerFallFaster,
            InputAction::DebugShakeDurationLonger,
            InputAction::DebugShakeDurationShorter,
        ] {
            assert_eq!(
                classify_battle_input(action),
                BattleInput::Ignored,
                "{action:?}は対戦中には無視するはず"
            );
        }
    }

    #[test]
    fn only_the_first_gameplay_action_queued_in_a_frame_is_used() {
        // 1フレームに複数キーが届いても、採用するのは最初の1つだけ(1tick高々1アクション)。
        // 途中に挟まる無視対象・音声トグルは選択に影響しない。
        let actions = [
            InputAction::ToggleMusic,
            InputAction::TogglePause,
            InputAction::MoveRight,
            InputAction::Drill,
        ];
        assert_eq!(first_local_action(&actions), Some(InputAction::MoveRight));
    }

    #[test]
    fn a_frame_without_any_gameplay_action_produces_no_local_input() {
        assert_eq!(first_local_action(&[]), None);
        assert_eq!(
            first_local_action(&[InputAction::TogglePause, InputAction::DebugAddLife]),
            None
        );
    }

    #[test]
    fn the_battle_result_screen_is_left_with_enter_space_or_escape() {
        // #256: 決着後はEnter(Confirm)・Space(TogglePause)・Escのいずれでもタイトルへ戻る。
        for action in [
            InputAction::Confirm,
            InputAction::TogglePause,
            InputAction::Quit,
        ] {
            assert!(
                leaves_battle_result(action),
                "{action:?}は結果表示から抜ける操作のはず"
            );
        }
    }

    #[test]
    fn the_battle_result_screen_ignores_the_gameplay_and_debug_keys() {
        // 決着後に盤面操作・デバッグ操作で誤ってタイトルへ戻らないことを確認する。
        for action in [
            InputAction::MoveLeft,
            InputAction::MoveRight,
            InputAction::FaceUp,
            InputAction::FaceDown,
            InputAction::Drill,
            InputAction::Rewind,
            InputAction::ToggleMusic,
            InputAction::ToggleSe,
            InputAction::OpenSettings,
            InputAction::OpenHelp,
            InputAction::UnboundKey,
            InputAction::DebugAddLife,
        ] {
            assert!(
                !leaves_battle_result(action),
                "{action:?}では結果表示から抜けないはず"
            );
        }
    }
}

//! BGM実効状態判定とゲームイベント→効果音再生の変換ロジック(#262に続くsrc/main.rs分割、
//! 段階a)。
//!
//! ここでの`audio`は「BGM有効判定・SE再生呼び出しへの変換」を指し、実際の音声ファイル
//! 再生を担う既存の低レベルモジュール`crate::audio`(main.rsの`mod audio;`)とは別物。
//! このモジュールは`crate::audio::sfx`を呼び出す側にあたる。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rodio::mixer::Mixer;

use crate::Screen;
use crate::game::{GameEvent, GameStatus};

/// MUSIC設定・現在の画面から、実際にタイトル画面用BGMを鳴らすべきかを判定する。
/// タイトル画面にいる間だけ鳴らす。
pub fn effective_title_bgm_enabled(settings_music_enabled: bool, screen: &Screen) -> bool {
    // モードセレクト画面はタイトルから直接つながる短い経由画面のため、
    // タイトルBGMをそのまま鳴らし続ける(往復で途切れさせない)。
    settings_music_enabled && matches!(screen, Screen::Title | Screen::ModeSelect)
}

/// MUSIC設定・現在の画面から、実際にプレイ中BGM(交代制プレイリスト)を鳴らすべきかを
/// 判定する。タイトル画面・ゲームオーバー中・ゴールクリア後はMUSIC設定のON/OFFに
/// 関わらず常に無音にする(クリアファンファーレはBGMと別にSEとして再生される)。
pub fn effective_gameplay_bgm_enabled(settings_music_enabled: bool, screen: &Screen) -> bool {
    if !settings_music_enabled {
        return false;
    }
    match screen {
        Screen::Title | Screen::ModeSelect => false,
        Screen::Playing(game) => matches!(game.status, GameStatus::Playing | GameStatus::Paused),
        // 対戦中(#252)は自分の盤面の進行状態で判断する。対戦には一時停止が無く、
        // 決着(クリア/脱落)後はプレイ中BGMを止める点は通常プレイと同じ。
        Screen::Battle(state) => state.game_local.status == GameStatus::Playing,
        Screen::Settings => true,
        // 独立画面としてのヘルプはジュークボックス試聴の置き場のため、プレイ中BGMを
        // 流すと試聴と二重に聞こえてしまう。常に無音にし、聞こえる音は選んだ曲のプレビュー
        // だけにする。一時停止中のヘルプオーバーレイは`Screen::Playing`のままなので対象外。
        Screen::Help => false,
    }
}

/// タイトルBGMを先頭から再生し直すべきかを、直前フレームの有効状態(`was_enabled`)と
/// 現在の有効状態(`now_enabled`)から判定する。無効→有効に転じた瞬間だけtrueを返す
/// (有効のまま/無効のままでは巻き戻さない)。
pub fn should_restart_title_bgm(was_enabled: bool, now_enabled: bool) -> bool {
    now_enabled && !was_enabled
}

/// SE(効果音)を1つ鳴らす。SE OFF設定中・音量0%・音声デバイス無しのいずれでも
/// 何もしない(`handle_events`と同じ条件判定を単発の再生でも使い回すための共通化)。
pub fn play_se(
    mixer: Option<&Mixer>,
    se_enabled: &Arc<AtomicBool>,
    se_volume_percent: u32,
    play: fn(&Mixer, f32),
) {
    if !se_enabled.load(Ordering::Relaxed) || se_volume_percent == 0 {
        return;
    }
    let Some(mixer) = mixer else {
        return;
    };
    play(mixer, crate::audio::sfx::se_gain(se_volume_percent));
}

/// ゲームイベントを対応する効果音再生へ変換する。SE OFF設定中、またはSE音量が0%の
/// 間は何もしない。
pub fn handle_events(
    events: &[GameEvent],
    mixer: Option<&Mixer>,
    se_enabled: &Arc<AtomicBool>,
    se_volume_percent: u32,
) {
    if !se_enabled.load(Ordering::Relaxed) || se_volume_percent == 0 {
        return;
    }
    let Some(mixer) = mixer else {
        return;
    };
    let gain = crate::audio::sfx::se_gain(se_volume_percent);

    for event in events {
        match event {
            GameEvent::DrillImpact => crate::audio::sfx::play_dig(mixer, gain),
            GameEvent::RockHitIntact => crate::audio::sfx::play_rock_hit(mixer, gain),
            GameEvent::BlockDestroyed { blocks } => {
                crate::audio::sfx::play_destroy(mixer, *blocks, gain)
            }
            GameEvent::RockDestroyed { blocks } => {
                crate::audio::sfx::play_rock_destroy(mixer, *blocks, gain)
            }
            GameEvent::DodgeTriggered => crate::audio::sfx::play_dodge(mixer, gain),
            GameEvent::OxygenCollected => crate::audio::sfx::play_oxygen_pickup(mixer, gain),
            // ダイヤ取得の専用SEはspec.md 10章のSE一覧に定義が無いため無音(得点加算のみ)。
            GameEvent::DiamondCollected => {}
            GameEvent::OxygenWarningTick => crate::audio::sfx::play_oxygen_warning(mixer, gain),
            GameEvent::LevelUp { .. } => crate::audio::sfx::play_level_up(mixer, gain),
            GameEvent::ExtraLifeAtLevel { .. } => crate::audio::sfx::play_extra_life(mixer, gain),
            // 死因(cause)はソークテストの集計専用で、SE再生では区別しない。
            GameEvent::LifeLost { .. } => crate::audio::sfx::play_life_lost(mixer, gain),
            GameEvent::Revived => crate::audio::sfx::play_revive(mixer, gain),
            GameEvent::GameOverMiss { .. } => crate::audio::sfx::play_miss(mixer, gain),
            GameEvent::Cleared => crate::audio::sfx::play_clear_fanfare(mixer, gain),
            GameEvent::ItemCollected(_) => crate::audio::sfx::play_item_collected(mixer, gain),
            GameEvent::BombExploded => crate::audio::sfx::play_bomb_explosion(mixer, gain),
            GameEvent::BombFuseWarning => crate::audio::sfx::play_bomb_fuse_warning(mixer, gain),
            GameEvent::BombFuseTick => crate::audio::sfx::play_bomb_fuse_tick(mixer, gain),
            // 相手の攻撃で降ってきた岩の出現(#247)。専用の波形は作らず、既存の岩ヒット音を
            // 1ウェーブにつき1回だけ鳴らす(個数は問わない)。予告開始時は無音のまま。
            GameEvent::IncomingRocksSpawned { .. } => crate::audio::sfx::play_rock_hit(mixer, gain),
            // 100mごとのチェックポイント到達。最終ゴール(Cleared)と同じファンファーレを使い回す。
            GameEvent::Checkpoint100m { .. } => crate::audio::sfx::play_clear_fanfare(mixer, gain),
            // 無敵によるミス回避(TERM独自拡張。#218)。デバッグ用の記録専用イベントで、
            // 演出もSEも伴わない(回数はHUDのGOD表示とデバッグログに残る)。
            GameEvent::MissAverted { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::Game;

    #[test]
    fn effective_title_bgm_enabled_is_true_only_on_title() {
        // タイトル画面にいる間だけタイトル用BGMを鳴らす。
        assert!(effective_title_bgm_enabled(true, &Screen::Title));
        assert!(!effective_title_bgm_enabled(false, &Screen::Title));
        assert!(!effective_title_bgm_enabled(true, &Screen::Settings));
        assert!(!effective_title_bgm_enabled(true, &Screen::Help));

        let game = Game::new(1);
        assert!(!effective_title_bgm_enabled(
            true,
            &Screen::Playing(Box::new(game))
        ));
    }

    #[test]
    fn effective_gameplay_bgm_enabled_is_always_false_on_title_regardless_of_setting() {
        // タイトル画面は専用曲(タイトルBGM)の担当のため、プレイ中BGM側は常に鳴らないはず。
        assert!(!effective_gameplay_bgm_enabled(true, &Screen::Title));
        assert!(!effective_gameplay_bgm_enabled(false, &Screen::Title));
    }

    #[test]
    fn mode_select_screen_keeps_the_title_bgm_playing_and_never_the_gameplay_bgm() {
        // モードセレクト画面はタイトルから直接つながる短い経由画面のため、
        // タイトルBGMを途切れさせずそのまま鳴らし続ける。
        assert!(effective_title_bgm_enabled(true, &Screen::ModeSelect));
        assert!(!effective_title_bgm_enabled(false, &Screen::ModeSelect));
        assert!(!effective_gameplay_bgm_enabled(true, &Screen::ModeSelect));
        assert!(!effective_gameplay_bgm_enabled(false, &Screen::ModeSelect));
    }

    #[test]
    fn effective_gameplay_bgm_enabled_follows_the_setting_on_settings_screen() {
        assert!(effective_gameplay_bgm_enabled(true, &Screen::Settings));
        assert!(!effective_gameplay_bgm_enabled(false, &Screen::Settings));
    }

    #[test]
    fn effective_gameplay_bgm_enabled_is_always_false_on_the_standalone_help_screen() {
        // ヘルプ画面(タイトルから開く独立画面)はジュークボックスの置き場のため、
        // 自動でプレイ中BGMを流し続けると試聴と二重に聞こえてしまう。常に無音にする。
        assert!(!effective_gameplay_bgm_enabled(true, &Screen::Help));
        assert!(!effective_gameplay_bgm_enabled(false, &Screen::Help));
    }

    #[test]
    fn effective_gameplay_bgm_enabled_is_true_while_playing_or_paused() {
        let game = Game::new(1);
        assert!(effective_gameplay_bgm_enabled(
            true,
            &Screen::Playing(Box::new(game))
        ));

        let mut game = Game::new(1);
        game.status = GameStatus::Paused;
        assert!(effective_gameplay_bgm_enabled(
            true,
            &Screen::Playing(Box::new(game))
        ));
    }

    #[test]
    fn effective_gameplay_bgm_enabled_is_false_on_game_over() {
        // ゲームオーバー時は短いミス音の後、BGMを停止する。
        let mut game = Game::new(1);
        game.status = GameStatus::GameOver;
        assert!(!effective_gameplay_bgm_enabled(
            true,
            &Screen::Playing(Box::new(game))
        ));
    }

    #[test]
    fn effective_gameplay_bgm_enabled_is_false_on_cleared() {
        // クリア時はBGMを止め、ファンファーレはSEとして別途再生する。
        let mut game = Game::new(1);
        game.status = GameStatus::Cleared;
        assert!(!effective_gameplay_bgm_enabled(
            true,
            &Screen::Playing(Box::new(game))
        ));
    }

    #[test]
    fn effective_gameplay_bgm_enabled_follows_the_local_game_while_battling() {
        // 対戦中(#252)は自分の盤面がプレイ中の間だけプレイ中BGMを鳴らし、決着後は止める。
        use crate::battle::BattleState;

        let battling = |local_status: GameStatus| {
            let mut game_local = Game::new(1);
            game_local.status = local_status;
            Screen::Battle(Box::new(BattleState::new(
                game_local,
                Game::new(1),
                "opponent".to_string(),
            )))
        };

        assert!(effective_gameplay_bgm_enabled(
            true,
            &battling(GameStatus::Playing)
        ));
        assert!(!effective_gameplay_bgm_enabled(
            false,
            &battling(GameStatus::Playing)
        ));
        assert!(!effective_gameplay_bgm_enabled(
            true,
            &battling(GameStatus::Cleared)
        ));
        assert!(!effective_gameplay_bgm_enabled(
            true,
            &battling(GameStatus::GameOver)
        ));
        // 対戦画面はタイトル画面ではないため、タイトルBGM側は常に無音。
        assert!(!effective_title_bgm_enabled(
            true,
            &battling(GameStatus::Playing)
        ));
    }

    #[test]
    fn title_and_gameplay_bgm_are_never_both_enabled_at_once() {
        // BGMは2系統(タイトル用・プレイ中用)あり、同時に両方鳴ると不自然なので、
        // どの画面状態でも排他的であることを確認する。
        let labeled_screens: Vec<(&str, Screen)> = vec![
            ("Title", Screen::Title),
            ("Settings", Screen::Settings),
            ("Help", Screen::Help),
            ("Playing", Screen::Playing(Box::new(Game::new(1)))),
        ];
        for (label, screen) in &labeled_screens {
            let title_on = effective_title_bgm_enabled(true, screen);
            let gameplay_on = effective_gameplay_bgm_enabled(true, screen);
            assert!(
                !(title_on && gameplay_on),
                "{label}でタイトル用・プレイ中用の両方が有効になっている"
            );
        }
    }

    #[test]
    fn should_restart_title_bgm_only_on_the_disabled_to_enabled_transition() {
        // 無効→有効に転じた瞬間だけ巻き戻すべきで、有効のまま/無効のままでは巻き戻さない。
        assert!(
            should_restart_title_bgm(false, true),
            "無効→有効の遷移では巻き戻すはず"
        );
        assert!(
            !should_restart_title_bgm(true, true),
            "有効のままなら巻き戻さないはず"
        );
        assert!(
            !should_restart_title_bgm(false, false),
            "無効のままなら巻き戻さないはず"
        );
        assert!(
            !should_restart_title_bgm(true, false),
            "有効→無効の遷移では巻き戻さないはず"
        );
    }
}

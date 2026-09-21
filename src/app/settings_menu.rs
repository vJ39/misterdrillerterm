//! 出現率・速度等の設定値調整ロジック(#262に続くsrc/main.rs分割、段階a)。
//!
//! タイトルのSettings画面と一時停止オーバーレイの両方から同じ調整ロジックが要るため、
//! ここに共通化してまとめる。盤面への反映・`Settings::save`は呼び出し側の責務。

use crate::constants::{
    ATTACK_BLOCKS_PER_BOMB_MAX, ATTACK_BLOCKS_PER_BOMB_MIN, ATTACK_BLOCKS_PER_BOMB_STEP,
    ATTACK_BLOCKS_PER_ROCK_MAX, ATTACK_BLOCKS_PER_ROCK_MIN, ATTACK_BLOCKS_PER_ROCK_STEP,
    ATTACK_BOMB_RATIO_PERCENT_MAX, ATTACK_BOMB_RATIO_PERCENT_MIN, ATTACK_BOMB_RATIO_PERCENT_STEP,
    ATTACK_BOMBS_PER_WAVE_MAX_MAX, ATTACK_BOMBS_PER_WAVE_MAX_MIN, ATTACK_BOMBS_PER_WAVE_MAX_STEP,
    ATTACK_ROCKS_PER_WAVE_MAX_MAX, ATTACK_ROCKS_PER_WAVE_MAX_MIN, ATTACK_ROCKS_PER_WAVE_MAX_STEP,
    BOMB_FUSE_MS_MAX, BOMB_FUSE_MS_MIN, BOMB_FUSE_MS_STEP, BOMB_SPAWN_RATE_PERCENT_MAX,
    BOMB_SPAWN_RATE_PERCENT_MIN, BOMB_SPAWN_RATE_PERCENT_STEP, CHAIN_VANISH_INTERVAL_MS_MAX,
    CHAIN_VANISH_INTERVAL_MS_STEP, COLOR_CLUSTER_RATE_PERCENT_MIN, COLOR_COUNT_MAX,
    COLOR_COUNT_MIN, DEBUG_FALL_TICK_MS_MAX, DEBUG_FALL_TICK_MS_MIN, DEBUG_FALL_TICK_STEP_MS,
    DEBUG_SHAKE_DURATION_MS_MAX, DEBUG_SHAKE_DURATION_STEP_MS, DIAMOND_SPAWN_RATE_PERCENT_MIN,
    DODGE_RECOVERY_MS_MAX, DODGE_RECOVERY_MS_STEP, FIELD_WIDTH_MAX, FIELD_WIDTH_MIN,
    FIELD_WIDTH_STEP, ITEM_SPAWN_RATE_PERCENT_MIN, MOVE_COOLDOWN_MS_MAX, MOVE_COOLDOWN_MS_MIN,
    MOVE_COOLDOWN_MS_STEP, REWIND_STOCK_MAX_SETTING_MAX, REWIND_STOCK_MAX_SETTING_MIN,
    SOUND_VOLUME_PERCENT_MAX, SOUND_VOLUME_PERCENT_MIN, SOUND_VOLUME_PERCENT_STEP,
    SPAWN_RATE_PERCENT_MAX, SPAWN_RATE_PERCENT_MIN, SPAWN_RATE_PERCENT_STEP,
    STAR_SPAWN_RATE_PERCENT_MAX, STAR_SPAWN_RATE_PERCENT_MIN, STAR_SPAWN_RATE_PERCENT_STEP,
};
use crate::settings::Settings;
use crate::ui;

/// Xブロック/AIR等の出現率設定(%)を1ステップぶん増減し、指定した下限〜
/// `SPAWN_RATE_PERCENT_MAX`にクランプする。スターは上限・刻み幅が異なるため
/// `adjust_star_rate_percent`を別に使う。
pub fn adjust_rate_percent(current: u32, increase: bool, min: u32) -> u32 {
    if increase {
        current
            .saturating_add(SPAWN_RATE_PERCENT_STEP)
            .min(SPAWN_RATE_PERCENT_MAX)
    } else {
        current.saturating_sub(SPAWN_RATE_PERCENT_STEP).max(min)
    }
}

/// スターブロックの出現率設定(%)を1ステップぶん増減する。他ブロックと共通の
/// `adjust_rate_percent`とは異なる、スター専用の上限(`STAR_SPAWN_RATE_PERCENT_MAX`)・
/// 刻み幅(`STAR_SPAWN_RATE_PERCENT_STEP`)を使う。
pub fn adjust_star_rate_percent(current: u32, increase: bool) -> u32 {
    if increase {
        current
            .saturating_add(STAR_SPAWN_RATE_PERCENT_STEP)
            .min(STAR_SPAWN_RATE_PERCENT_MAX)
    } else {
        // 現在STAR_SPAWN_RATE_PERCENT_MIN=0のためu32のsaturating_sub結果への
        // .max()は無意味と判定されるが(clippy::unnecessary_min_or_max)、下限を
        // 明示するための記述として意図的に残す(将来0以外に変える場合の安全策)。
        #[allow(clippy::unnecessary_min_or_max)]
        current
            .saturating_sub(STAR_SPAWN_RATE_PERCENT_STEP)
            .max(STAR_SPAWN_RATE_PERCENT_MIN)
    }
}

/// 配分率・色数系の設定項目(岩/AIR/スター/ダイヤ/アイテム3種/色数/色結合率)を1ステップ
/// ぶん調整する。タイトルのSettings画面と一時停止オーバーレイで同じロジックが要るため共通化。
/// `choice`が対象外なら何もせず`false`を返す。盤面への反映・`Settings::save`は呼び出し側の責務。
pub fn adjust_spawn_rate_setting(
    settings: &mut Settings,
    choice: ui::render::SettingsChoice,
    increase: bool,
) -> bool {
    match choice {
        ui::render::SettingsChoice::RockRate => {
            settings.rock_spawn_rate_percent = adjust_rate_percent(
                settings.rock_spawn_rate_percent,
                increase,
                SPAWN_RATE_PERCENT_MIN,
            );
        }
        ui::render::SettingsChoice::AirRate => {
            settings.air_spawn_rate_percent = adjust_rate_percent(
                settings.air_spawn_rate_percent,
                increase,
                SPAWN_RATE_PERCENT_MIN,
            );
        }
        ui::render::SettingsChoice::StarRate => {
            settings.star_spawn_rate_percent =
                adjust_star_rate_percent(settings.star_spawn_rate_percent, increase);
        }
        ui::render::SettingsChoice::DiamondRate => {
            settings.diamond_spawn_rate_percent = adjust_rate_percent(
                settings.diamond_spawn_rate_percent,
                increase,
                DIAMOND_SPAWN_RATE_PERCENT_MIN,
            );
        }
        ui::render::SettingsChoice::ItemClearAboveRate => {
            settings.item_clear_above_rate_percent = adjust_rate_percent(
                settings.item_clear_above_rate_percent,
                increase,
                ITEM_SPAWN_RATE_PERCENT_MIN,
            );
        }
        ui::render::SettingsChoice::ItemUnifyColorsRate => {
            settings.item_unify_colors_rate_percent = adjust_rate_percent(
                settings.item_unify_colors_rate_percent,
                increase,
                ITEM_SPAWN_RATE_PERCENT_MIN,
            );
        }
        ui::render::SettingsChoice::ItemStarifyScreenRate => {
            settings.item_starify_screen_rate_percent = adjust_rate_percent(
                settings.item_starify_screen_rate_percent,
                increase,
                ITEM_SPAWN_RATE_PERCENT_MIN,
            );
        }
        ui::render::SettingsChoice::ColorCount => {
            settings.color_count = adjust_color_count(settings.color_count, increase);
        }
        ui::render::SettingsChoice::ColorClusterRate => {
            settings.color_cluster_rate_percent = adjust_rate_percent(
                settings.color_cluster_rate_percent,
                increase,
                COLOR_CLUSTER_RATE_PERCENT_MIN,
            );
        }
        _ => return false,
    }
    true
}

/// ボム出現頻度設定(%)を1ステップぶん増減する。他の配分率と共通の
/// `adjust_rate_percent`とは異なる、ボム専用の上限(`BOMB_SPAWN_RATE_PERCENT_MAX`)・
/// 刻み幅(`BOMB_SPAWN_RATE_PERCENT_STEP`)を使う。
pub fn adjust_bomb_rate_percent(current: u32, increase: bool) -> u32 {
    if increase {
        current
            .saturating_add(BOMB_SPAWN_RATE_PERCENT_STEP)
            .min(BOMB_SPAWN_RATE_PERCENT_MAX)
    } else {
        // 現在BOMB_SPAWN_RATE_PERCENT_MIN=0のためu32のsaturating_sub結果への
        // .max()は無意味と判定されるが(clippy::unnecessary_min_or_max)、下限を
        // 明示するための記述として意図的に残す(将来0以外に変える場合の安全策)。
        #[allow(clippy::unnecessary_min_or_max)]
        current
            .saturating_sub(BOMB_SPAWN_RATE_PERCENT_STEP)
            .max(BOMB_SPAWN_RATE_PERCENT_MIN)
    }
}

/// ボム爆発までの時間設定(ms)を1ステップぶん増減する。
/// `BOMB_FUSE_MS_MIN`〜`BOMB_FUSE_MS_MAX`の範囲、`BOMB_FUSE_MS_STEP`刻みで調整する。
pub fn adjust_bomb_fuse_ms(current: u32, increase: bool) -> u32 {
    if increase {
        current
            .saturating_add(BOMB_FUSE_MS_STEP)
            .min(BOMB_FUSE_MS_MAX)
    } else {
        current
            .saturating_sub(BOMB_FUSE_MS_STEP)
            .max(BOMB_FUSE_MS_MIN)
    }
}

/// 岩1個に必要な攻撃力(#247)を1ステップぶん増減する。
/// `ATTACK_BLOCKS_PER_ROCK_MIN`〜`MAX`の範囲、`ATTACK_BLOCKS_PER_ROCK_STEP`刻みで調整する。
pub fn adjust_attack_blocks_per_rock(current: u32, increase: bool) -> u32 {
    if increase {
        current
            .saturating_add(ATTACK_BLOCKS_PER_ROCK_STEP)
            .min(ATTACK_BLOCKS_PER_ROCK_MAX)
    } else {
        current
            .saturating_sub(ATTACK_BLOCKS_PER_ROCK_STEP)
            .max(ATTACK_BLOCKS_PER_ROCK_MIN)
    }
}

/// 1ウェーブで降る岩の個数上限(#247)を1ステップぶん増減する。
/// `ATTACK_ROCKS_PER_WAVE_MAX_MIN`〜`MAX`の範囲、`ATTACK_ROCKS_PER_WAVE_MAX_STEP`刻み。
pub fn adjust_attack_rocks_per_wave_max(current: u32, increase: bool) -> u32 {
    if increase {
        current
            .saturating_add(ATTACK_ROCKS_PER_WAVE_MAX_STEP)
            .min(ATTACK_ROCKS_PER_WAVE_MAX_MAX)
    } else {
        current
            .saturating_sub(ATTACK_ROCKS_PER_WAVE_MAX_STEP)
            .max(ATTACK_ROCKS_PER_WAVE_MAX_MIN)
    }
}

/// ボム1個に必要な攻撃力(#304)を1ステップぶん増減する。
/// `ATTACK_BLOCKS_PER_BOMB_MIN`〜`MAX`の範囲、`ATTACK_BLOCKS_PER_BOMB_STEP`刻みで調整する。
pub fn adjust_attack_blocks_per_bomb(current: u32, increase: bool) -> u32 {
    if increase {
        current
            .saturating_add(ATTACK_BLOCKS_PER_BOMB_STEP)
            .min(ATTACK_BLOCKS_PER_BOMB_MAX)
    } else {
        current
            .saturating_sub(ATTACK_BLOCKS_PER_BOMB_STEP)
            .max(ATTACK_BLOCKS_PER_BOMB_MIN)
    }
}

/// 1ウェーブで降るボムの個数上限(#304)を1ステップぶん増減する。
/// `ATTACK_BOMBS_PER_WAVE_MAX_MIN`〜`MAX`の範囲、`ATTACK_BOMBS_PER_WAVE_MAX_STEP`刻み。
pub fn adjust_attack_bombs_per_wave_max(current: u32, increase: bool) -> u32 {
    if increase {
        current
            .saturating_add(ATTACK_BOMBS_PER_WAVE_MAX_STEP)
            .min(ATTACK_BOMBS_PER_WAVE_MAX_MAX)
    } else {
        current
            .saturating_sub(ATTACK_BOMBS_PER_WAVE_MAX_STEP)
            .max(ATTACK_BOMBS_PER_WAVE_MAX_MIN)
    }
}

/// 攻撃力のボム化比率(%、#304)を1ステップぶん増減する。
/// `ATTACK_BOMB_RATIO_PERCENT_MIN`(0=岩だけ)〜`MAX`(100=ボムだけ)の範囲、
/// `ATTACK_BOMB_RATIO_PERCENT_STEP`刻みで調整する。
pub fn adjust_attack_bomb_ratio_percent(current: u32, increase: bool) -> u32 {
    if increase {
        current
            .saturating_add(ATTACK_BOMB_RATIO_PERCENT_STEP)
            .min(ATTACK_BOMB_RATIO_PERCENT_MAX)
    } else {
        // 現在ATTACK_BOMB_RATIO_PERCENT_MIN=0のためu32のsaturating_sub結果への
        // .max()は無意味と判定されるが(clippy::unnecessary_min_or_max)、下限を
        // 明示するための記述として意図的に残す(将来0以外に変える場合の安全策)。
        #[allow(clippy::unnecessary_min_or_max)]
        current
            .saturating_sub(ATTACK_BOMB_RATIO_PERCENT_STEP)
            .max(ATTACK_BOMB_RATIO_PERCENT_MIN)
    }
}

/// 出現する色ブロックの色数(`COLOR_COUNT_MIN`〜`COLOR_COUNT_MAX`)を1ずつ増減する。
pub fn adjust_color_count(current: u8, increase: bool) -> u8 {
    if increase {
        current.saturating_add(1).min(COLOR_COUNT_MAX)
    } else {
        current.saturating_sub(1).max(COLOR_COUNT_MIN)
    }
}

/// ブロック落下速度(tick間隔, ms)を`DEBUG_FALL_TICK_STEP_MS`ぶん増減する。
/// `increase`はms値そのものの増減方向(true=ms増加=遅くなる)を表す。
pub fn adjust_fall_speed_ms(current: u64, increase: bool) -> u64 {
    if increase {
        current
            .saturating_add(DEBUG_FALL_TICK_STEP_MS)
            .min(DEBUG_FALL_TICK_MS_MAX)
    } else {
        current
            .saturating_sub(DEBUG_FALL_TICK_STEP_MS)
            .max(DEBUG_FALL_TICK_MS_MIN)
    }
}

/// 横移動のクールダウン間隔(ms)を`MOVE_COOLDOWN_MS_STEP`ぶん増減する。
/// `increase`はms値そのものの増減方向(true=ms増加=遅くなる)を表す。
pub fn adjust_move_cooldown_ms(current: u64, increase: bool) -> u64 {
    if increase {
        current
            .saturating_add(MOVE_COOLDOWN_MS_STEP)
            .min(MOVE_COOLDOWN_MS_MAX)
    } else {
        current
            .saturating_sub(MOVE_COOLDOWN_MS_STEP)
            .max(MOVE_COOLDOWN_MS_MIN)
    }
}

/// フィールド幅(列数)を`FIELD_WIDTH_STEP`ぶん増減する。新規ゲーム開始時にのみ反映される。
pub fn adjust_field_width(current: usize, increase: bool) -> usize {
    if increase {
        current
            .saturating_add(FIELD_WIDTH_STEP)
            .min(FIELD_WIDTH_MAX)
    } else {
        current
            .saturating_sub(FIELD_WIDTH_STEP)
            .max(FIELD_WIDTH_MIN)
    }
}

/// ヒヤリ回避スライダー後の硬直時間(ms)を`DODGE_RECOVERY_MS_STEP`ぶん増減する。
pub fn adjust_dodge_recovery_ms(current: u64, increase: bool) -> u64 {
    if increase {
        current
            .saturating_add(DODGE_RECOVERY_MS_STEP)
            .min(DODGE_RECOVERY_MS_MAX)
    } else {
        // DODGE_RECOVERY_MS_MINは0固定のため、saturating_subの結果に対する.max()は不要
        // (clippy::unnecessary_min_or_max)。
        current.saturating_sub(DODGE_RECOVERY_MS_STEP)
    }
}

/// 揺れ時間(支えを失ってから実際に落下し始めるまでの猶予, ms)を
/// `DEBUG_SHAKE_DURATION_STEP_MS`ぶん増減する(#243)。
pub fn adjust_shake_duration_ms(current: u64, increase: bool) -> u64 {
    if increase {
        current
            .saturating_add(DEBUG_SHAKE_DURATION_STEP_MS)
            .min(DEBUG_SHAKE_DURATION_MS_MAX)
    } else {
        // DEBUG_SHAKE_DURATION_MS_MINは0固定のため、saturating_subの結果に対する.max()は
        // 不要(clippy::unnecessary_min_or_max)。
        current.saturating_sub(DEBUG_SHAKE_DURATION_STEP_MS)
    }
}

/// 自動消滅の連鎖インターバル(ms)を`CHAIN_VANISH_INTERVAL_MS_STEP`ぶん増減する。
/// 連鎖消滅を0ms連続でなく一定間隔で進めるための設定。
pub fn adjust_chain_vanish_interval_ms(current: u64, increase: bool) -> u64 {
    if increase {
        current
            .saturating_add(CHAIN_VANISH_INTERVAL_MS_STEP)
            .min(CHAIN_VANISH_INTERVAL_MS_MAX)
    } else {
        // CHAIN_VANISH_INTERVAL_MS_MINは0固定のため、saturating_subの結果に対する.max()は
        // 不要(clippy::unnecessary_min_or_max)。
        current.saturating_sub(CHAIN_VANISH_INTERVAL_MS_STEP)
    }
}

/// 巻き戻しストック上限を1ずつ増減する(TERM独自拡張。#233)。
/// `REWIND_STOCK_MAX_SETTING_MIN`(0=機能OFF)〜`REWIND_STOCK_MAX_SETTING_MAX`の範囲。
#[allow(clippy::unnecessary_min_or_max)]
pub fn adjust_rewind_stock_max(current: u8, increase: bool) -> u8 {
    if increase {
        current.saturating_add(1).min(REWIND_STOCK_MAX_SETTING_MAX)
    } else {
        // REWIND_STOCK_MAX_SETTING_MINは0固定のためsaturating_subの結果への.max()は
        // 無意味と判定されるが、下限を明示する記述として意図的に残す。
        current.saturating_sub(1).max(REWIND_STOCK_MAX_SETTING_MIN)
    }
}

/// SE/MUSIC音量(%)を`SOUND_VOLUME_PERCENT_STEP`ぶん増減する(TERM独自拡張。#224)。
#[allow(clippy::unnecessary_min_or_max)]
pub fn adjust_sound_volume_percent(current: u32, increase: bool) -> u32 {
    if increase {
        current
            .saturating_add(SOUND_VOLUME_PERCENT_STEP)
            .min(SOUND_VOLUME_PERCENT_MAX)
    } else {
        current
            .saturating_sub(SOUND_VOLUME_PERCENT_STEP)
            .max(SOUND_VOLUME_PERCENT_MIN)
    }
}

/// AI専用設定(`Settings`の`ai_*`、#312)を1ステップぶん調整する。値の範囲・刻み幅は
/// 人間側と同じ定数をそのまま使い、AIにだけ別の値を持たせるためにフィールドを分けている。
/// 全項目を扱うため対象外の分岐は無い。`Settings::save`は呼び出し側の責務。
pub fn adjust_ai_setting(
    settings: &mut Settings,
    choice: ui::render::AiSettingsChoice,
    increase: bool,
) {
    match choice {
        ui::render::AiSettingsChoice::BlockFallSpeed => {
            settings.ai_block_fall_tick_ms =
                adjust_fall_speed_ms(settings.ai_block_fall_tick_ms, increase);
        }
        ui::render::AiSettingsChoice::PlayerFallSpeed => {
            settings.ai_player_fall_tick_ms =
                adjust_fall_speed_ms(settings.ai_player_fall_tick_ms, increase);
        }
        ui::render::AiSettingsChoice::ShakeDuration => {
            settings.ai_shake_duration_ms =
                adjust_shake_duration_ms(settings.ai_shake_duration_ms, increase);
        }
        ui::render::AiSettingsChoice::RockRate => {
            settings.ai_rock_spawn_rate_percent = adjust_rate_percent(
                settings.ai_rock_spawn_rate_percent,
                increase,
                SPAWN_RATE_PERCENT_MIN,
            );
        }
        ui::render::AiSettingsChoice::AirRate => {
            settings.ai_air_spawn_rate_percent = adjust_rate_percent(
                settings.ai_air_spawn_rate_percent,
                increase,
                SPAWN_RATE_PERCENT_MIN,
            );
        }
        ui::render::AiSettingsChoice::StarRate => {
            settings.ai_star_spawn_rate_percent =
                adjust_star_rate_percent(settings.ai_star_spawn_rate_percent, increase);
        }
        ui::render::AiSettingsChoice::DiamondRate => {
            settings.ai_diamond_spawn_rate_percent = adjust_rate_percent(
                settings.ai_diamond_spawn_rate_percent,
                increase,
                DIAMOND_SPAWN_RATE_PERCENT_MIN,
            );
        }
        ui::render::AiSettingsChoice::ItemClearAboveRate => {
            settings.ai_item_clear_above_rate_percent = adjust_rate_percent(
                settings.ai_item_clear_above_rate_percent,
                increase,
                ITEM_SPAWN_RATE_PERCENT_MIN,
            );
        }
        ui::render::AiSettingsChoice::ItemUnifyColorsRate => {
            settings.ai_item_unify_colors_rate_percent = adjust_rate_percent(
                settings.ai_item_unify_colors_rate_percent,
                increase,
                ITEM_SPAWN_RATE_PERCENT_MIN,
            );
        }
        ui::render::AiSettingsChoice::ItemStarifyScreenRate => {
            settings.ai_item_starify_screen_rate_percent = adjust_rate_percent(
                settings.ai_item_starify_screen_rate_percent,
                increase,
                ITEM_SPAWN_RATE_PERCENT_MIN,
            );
        }
        ui::render::AiSettingsChoice::ColorCount => {
            settings.ai_color_count = adjust_color_count(settings.ai_color_count, increase);
        }
        ui::render::AiSettingsChoice::ColorClusterRate => {
            settings.ai_color_cluster_rate_percent = adjust_rate_percent(
                settings.ai_color_cluster_rate_percent,
                increase,
                COLOR_CLUSTER_RATE_PERCENT_MIN,
            );
        }
        ui::render::AiSettingsChoice::BombRate => {
            settings.ai_bomb_spawn_rate_percent =
                adjust_bomb_rate_percent(settings.ai_bomb_spawn_rate_percent, increase);
        }
        ui::render::AiSettingsChoice::BombFuse => {
            settings.ai_bomb_fuse_ms = adjust_bomb_fuse_ms(settings.ai_bomb_fuse_ms, increase);
        }
        ui::render::AiSettingsChoice::AttackBlocksPerRock => {
            settings.ai_attack_blocks_per_rock =
                adjust_attack_blocks_per_rock(settings.ai_attack_blocks_per_rock, increase);
        }
        ui::render::AiSettingsChoice::AttackRocksPerWaveMax => {
            settings.ai_attack_rocks_per_wave_max =
                adjust_attack_rocks_per_wave_max(settings.ai_attack_rocks_per_wave_max, increase);
        }
        ui::render::AiSettingsChoice::AttackBlocksPerBomb => {
            settings.ai_attack_blocks_per_bomb =
                adjust_attack_blocks_per_bomb(settings.ai_attack_blocks_per_bomb, increase);
        }
        ui::render::AiSettingsChoice::AttackBombsPerWaveMax => {
            settings.ai_attack_bombs_per_wave_max =
                adjust_attack_bombs_per_wave_max(settings.ai_attack_bombs_per_wave_max, increase);
        }
        ui::render::AiSettingsChoice::AttackBombRatioPercent => {
            settings.ai_attack_bomb_ratio_percent =
                adjust_attack_bomb_ratio_percent(settings.ai_attack_bomb_ratio_percent, increase);
        }
        ui::render::AiSettingsChoice::ChainVanishInterval => {
            settings.ai_chain_vanish_interval_ms =
                adjust_chain_vanish_interval_ms(settings.ai_chain_vanish_interval_ms, increase);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjust_rate_percent_saturates_at_max_instead_of_panicking_when_current_is_corrupted() {
        // 破損したsettings.jsonでcurrentがu32::MAX付近になっていても、raw加算での
        // オーバーフローpanicはせず、上限へ飽和するだけのはず。
        assert_eq!(
            adjust_rate_percent(u32::MAX, true, SPAWN_RATE_PERCENT_MIN),
            SPAWN_RATE_PERCENT_MAX
        );
    }

    #[test]
    fn adjust_star_rate_percent_can_reach_the_higher_star_specific_max() {
        // 他ブロックと共通のSPAWN_RATE_PERCENT_MAX(300%)より大きい、
        // スター専用の上限まで増やせるはず。
        assert_eq!(
            adjust_star_rate_percent(u32::MAX, true),
            STAR_SPAWN_RATE_PERCENT_MAX,
            "破損データでのオーバーフローpanicもせず上限へ飽和するはず"
        );
        assert_eq!(
            adjust_star_rate_percent(0, false),
            STAR_SPAWN_RATE_PERCENT_MIN
        );
    }

    #[test]
    fn adjust_bomb_rate_percent_can_reach_the_higher_bomb_specific_max() {
        // 他ブロックと共通のSPAWN_RATE_PERCENT_MAX(300%)より大きい、
        // ボム専用の上限まで増やせるはず。
        assert_eq!(
            adjust_bomb_rate_percent(u32::MAX, true),
            BOMB_SPAWN_RATE_PERCENT_MAX,
            "破損データでのオーバーフローpanicもせず上限へ飽和するはず"
        );
        assert_eq!(
            adjust_bomb_rate_percent(0, false),
            BOMB_SPAWN_RATE_PERCENT_MIN
        );
    }

    #[test]
    fn adjust_bomb_fuse_ms_saturates_at_max_instead_of_panicking_when_current_is_corrupted() {
        assert_eq!(adjust_bomb_fuse_ms(u32::MAX, true), BOMB_FUSE_MS_MAX);
    }

    #[test]
    fn adjust_bomb_fuse_ms_saturates_at_min_instead_of_underflowing_when_current_is_corrupted() {
        assert_eq!(adjust_bomb_fuse_ms(0, false), BOMB_FUSE_MS_MIN);
    }

    #[test]
    fn adjust_attack_blocks_per_rock_saturates_at_both_ends_when_current_is_corrupted() {
        // #247。破損したsettings.jsonの値でもオーバーフロー/アンダーフローせず範囲へ収める。
        assert_eq!(
            adjust_attack_blocks_per_rock(u32::MAX, true),
            ATTACK_BLOCKS_PER_ROCK_MAX
        );
        assert_eq!(
            adjust_attack_blocks_per_rock(0, false),
            ATTACK_BLOCKS_PER_ROCK_MIN
        );
    }

    #[test]
    fn adjust_attack_blocks_per_rock_moves_by_one_step_inside_the_range() {
        let up = adjust_attack_blocks_per_rock(ATTACK_BLOCKS_PER_ROCK_MIN, true);
        assert_eq!(up, ATTACK_BLOCKS_PER_ROCK_MIN + ATTACK_BLOCKS_PER_ROCK_STEP);
        assert_eq!(
            adjust_attack_blocks_per_rock(up, false),
            ATTACK_BLOCKS_PER_ROCK_MIN
        );
    }

    #[test]
    fn adjust_attack_rocks_per_wave_max_saturates_at_both_ends_when_current_is_corrupted() {
        assert_eq!(
            adjust_attack_rocks_per_wave_max(u32::MAX, true),
            ATTACK_ROCKS_PER_WAVE_MAX_MAX
        );
        assert_eq!(
            adjust_attack_rocks_per_wave_max(0, false),
            ATTACK_ROCKS_PER_WAVE_MAX_MIN
        );
    }

    #[test]
    fn adjust_attack_rocks_per_wave_max_moves_by_one_step_inside_the_range() {
        let up = adjust_attack_rocks_per_wave_max(ATTACK_ROCKS_PER_WAVE_MAX_MIN, true);
        assert_eq!(
            up,
            ATTACK_ROCKS_PER_WAVE_MAX_MIN + ATTACK_ROCKS_PER_WAVE_MAX_STEP
        );
        assert_eq!(
            adjust_attack_rocks_per_wave_max(up, false),
            ATTACK_ROCKS_PER_WAVE_MAX_MIN
        );
    }

    #[test]
    fn adjust_attack_blocks_per_bomb_saturates_at_both_ends_when_current_is_corrupted() {
        // #304。岩と同じく、破損した値でも範囲内へ収める。
        assert_eq!(
            adjust_attack_blocks_per_bomb(u32::MAX, true),
            ATTACK_BLOCKS_PER_BOMB_MAX
        );
        assert_eq!(
            adjust_attack_blocks_per_bomb(0, false),
            ATTACK_BLOCKS_PER_BOMB_MIN
        );
    }

    #[test]
    fn adjust_attack_blocks_per_bomb_moves_by_one_step_inside_the_range() {
        let up = adjust_attack_blocks_per_bomb(ATTACK_BLOCKS_PER_BOMB_MIN, true);
        assert_eq!(up, ATTACK_BLOCKS_PER_BOMB_MIN + ATTACK_BLOCKS_PER_BOMB_STEP);
        assert_eq!(
            adjust_attack_blocks_per_bomb(up, false),
            ATTACK_BLOCKS_PER_BOMB_MIN
        );
    }

    #[test]
    fn adjust_attack_bombs_per_wave_max_saturates_at_both_ends_when_current_is_corrupted() {
        assert_eq!(
            adjust_attack_bombs_per_wave_max(u32::MAX, true),
            ATTACK_BOMBS_PER_WAVE_MAX_MAX
        );
        assert_eq!(
            adjust_attack_bombs_per_wave_max(0, false),
            ATTACK_BOMBS_PER_WAVE_MAX_MIN
        );
    }

    #[test]
    fn adjust_attack_bombs_per_wave_max_moves_by_one_step_inside_the_range() {
        let up = adjust_attack_bombs_per_wave_max(ATTACK_BOMBS_PER_WAVE_MAX_MIN, true);
        assert_eq!(
            up,
            ATTACK_BOMBS_PER_WAVE_MAX_MIN + ATTACK_BOMBS_PER_WAVE_MAX_STEP
        );
        assert_eq!(
            adjust_attack_bombs_per_wave_max(up, false),
            ATTACK_BOMBS_PER_WAVE_MAX_MIN
        );
    }

    #[test]
    fn adjust_attack_bomb_ratio_percent_saturates_at_both_ends_when_current_is_corrupted() {
        assert_eq!(
            adjust_attack_bomb_ratio_percent(u32::MAX, true),
            ATTACK_BOMB_RATIO_PERCENT_MAX
        );
        assert_eq!(
            adjust_attack_bomb_ratio_percent(0, false),
            ATTACK_BOMB_RATIO_PERCENT_MIN
        );
    }

    #[test]
    fn adjust_attack_bomb_ratio_percent_moves_by_one_step_inside_the_range() {
        let up = adjust_attack_bomb_ratio_percent(ATTACK_BOMB_RATIO_PERCENT_MIN, true);
        assert_eq!(
            up,
            ATTACK_BOMB_RATIO_PERCENT_MIN + ATTACK_BOMB_RATIO_PERCENT_STEP
        );
        assert_eq!(
            adjust_attack_bomb_ratio_percent(up, false),
            ATTACK_BOMB_RATIO_PERCENT_MIN
        );
    }

    #[test]
    fn adjust_color_count_saturates_at_max_instead_of_panicking_when_current_is_corrupted() {
        assert_eq!(adjust_color_count(u8::MAX, true), COLOR_COUNT_MAX);
    }

    #[test]
    fn adjust_fall_speed_ms_saturates_at_max_instead_of_panicking_when_current_is_corrupted() {
        assert_eq!(adjust_fall_speed_ms(u64::MAX, true), DEBUG_FALL_TICK_MS_MAX);
    }

    #[test]
    fn adjust_move_cooldown_ms_saturates_at_max_instead_of_panicking_when_current_is_corrupted() {
        assert_eq!(
            adjust_move_cooldown_ms(u64::MAX, true),
            MOVE_COOLDOWN_MS_MAX
        );
    }

    #[test]
    fn adjust_field_width_saturates_at_max_instead_of_panicking_when_current_is_corrupted() {
        assert_eq!(adjust_field_width(usize::MAX, true), FIELD_WIDTH_MAX);
    }

    #[test]
    fn adjust_dodge_recovery_ms_saturates_at_max_instead_of_panicking_when_current_is_corrupted() {
        assert_eq!(
            adjust_dodge_recovery_ms(u64::MAX, true),
            DODGE_RECOVERY_MS_MAX
        );
    }

    #[test]
    fn adjust_shake_duration_ms_saturates_at_max_instead_of_panicking_when_current_is_corrupted() {
        assert_eq!(
            adjust_shake_duration_ms(u64::MAX, true),
            DEBUG_SHAKE_DURATION_MS_MAX
        );
    }

    #[test]
    fn adjust_shake_duration_ms_saturates_at_min_instead_of_panicking_when_current_is_corrupted() {
        assert_eq!(adjust_shake_duration_ms(0, false), 0);
    }

    #[test]
    fn adjust_sound_volume_percent_saturates_at_max_instead_of_panicking_when_current_is_corrupted()
    {
        assert_eq!(
            adjust_sound_volume_percent(u32::MAX, true),
            SOUND_VOLUME_PERCENT_MAX
        );
    }

    #[test]
    fn adjust_sound_volume_percent_saturates_at_min() {
        assert_eq!(
            adjust_sound_volume_percent(0, false),
            SOUND_VOLUME_PERCENT_MIN
        );
    }

    #[test]
    fn adjust_sound_volume_percent_decreases_by_one_step() {
        assert_eq!(adjust_sound_volume_percent(100, false), 90);
    }

    #[test]
    fn adjust_sound_volume_percent_increases_by_one_step() {
        assert_eq!(adjust_sound_volume_percent(90, true), 100);
    }

    // --- AI専用設定(#312) ---

    /// AI専用設定の全項目。巡回の対象と調整の対象が食い違わないよう、
    /// `AiSettingsChoice`の宣言順に並べる。
    const ALL_AI_CHOICES: [ui::render::AiSettingsChoice; 20] = [
        ui::render::AiSettingsChoice::BlockFallSpeed,
        ui::render::AiSettingsChoice::PlayerFallSpeed,
        ui::render::AiSettingsChoice::ShakeDuration,
        ui::render::AiSettingsChoice::RockRate,
        ui::render::AiSettingsChoice::AirRate,
        ui::render::AiSettingsChoice::StarRate,
        ui::render::AiSettingsChoice::DiamondRate,
        ui::render::AiSettingsChoice::ItemClearAboveRate,
        ui::render::AiSettingsChoice::ItemUnifyColorsRate,
        ui::render::AiSettingsChoice::ItemStarifyScreenRate,
        ui::render::AiSettingsChoice::ColorCount,
        ui::render::AiSettingsChoice::ColorClusterRate,
        ui::render::AiSettingsChoice::BombRate,
        ui::render::AiSettingsChoice::BombFuse,
        ui::render::AiSettingsChoice::AttackBlocksPerRock,
        ui::render::AiSettingsChoice::AttackRocksPerWaveMax,
        ui::render::AiSettingsChoice::AttackBlocksPerBomb,
        ui::render::AiSettingsChoice::AttackBombsPerWaveMax,
        ui::render::AiSettingsChoice::AttackBombRatioPercent,
        ui::render::AiSettingsChoice::ChainVanishInterval,
    ];

    /// 値が違うAI専用フィールドの名前を返す。どの項目がどのフィールドへ書かれたかを
    /// 名前で突き合わせ、書き先の取り違え・重複を検知する。
    fn changed_ai_field_names(before: &Settings, after: &Settings) -> Vec<&'static str> {
        [
            (
                "ai_block_fall_tick_ms",
                before.ai_block_fall_tick_ms != after.ai_block_fall_tick_ms,
            ),
            (
                "ai_player_fall_tick_ms",
                before.ai_player_fall_tick_ms != after.ai_player_fall_tick_ms,
            ),
            (
                "ai_shake_duration_ms",
                before.ai_shake_duration_ms != after.ai_shake_duration_ms,
            ),
            (
                "ai_rock_spawn_rate_percent",
                before.ai_rock_spawn_rate_percent != after.ai_rock_spawn_rate_percent,
            ),
            (
                "ai_air_spawn_rate_percent",
                before.ai_air_spawn_rate_percent != after.ai_air_spawn_rate_percent,
            ),
            (
                "ai_star_spawn_rate_percent",
                before.ai_star_spawn_rate_percent != after.ai_star_spawn_rate_percent,
            ),
            (
                "ai_diamond_spawn_rate_percent",
                before.ai_diamond_spawn_rate_percent != after.ai_diamond_spawn_rate_percent,
            ),
            (
                "ai_item_clear_above_rate_percent",
                before.ai_item_clear_above_rate_percent != after.ai_item_clear_above_rate_percent,
            ),
            (
                "ai_item_unify_colors_rate_percent",
                before.ai_item_unify_colors_rate_percent != after.ai_item_unify_colors_rate_percent,
            ),
            (
                "ai_item_starify_screen_rate_percent",
                before.ai_item_starify_screen_rate_percent
                    != after.ai_item_starify_screen_rate_percent,
            ),
            (
                "ai_color_count",
                before.ai_color_count != after.ai_color_count,
            ),
            (
                "ai_color_cluster_rate_percent",
                before.ai_color_cluster_rate_percent != after.ai_color_cluster_rate_percent,
            ),
            (
                "ai_bomb_spawn_rate_percent",
                before.ai_bomb_spawn_rate_percent != after.ai_bomb_spawn_rate_percent,
            ),
            (
                "ai_bomb_fuse_ms",
                before.ai_bomb_fuse_ms != after.ai_bomb_fuse_ms,
            ),
            (
                "ai_attack_blocks_per_rock",
                before.ai_attack_blocks_per_rock != after.ai_attack_blocks_per_rock,
            ),
            (
                "ai_attack_rocks_per_wave_max",
                before.ai_attack_rocks_per_wave_max != after.ai_attack_rocks_per_wave_max,
            ),
            (
                "ai_attack_blocks_per_bomb",
                before.ai_attack_blocks_per_bomb != after.ai_attack_blocks_per_bomb,
            ),
            (
                "ai_attack_bombs_per_wave_max",
                before.ai_attack_bombs_per_wave_max != after.ai_attack_bombs_per_wave_max,
            ),
            (
                "ai_attack_bomb_ratio_percent",
                before.ai_attack_bomb_ratio_percent != after.ai_attack_bomb_ratio_percent,
            ),
            (
                "ai_chain_vanish_interval_ms",
                before.ai_chain_vanish_interval_ms != after.ai_chain_vanish_interval_ms,
            ),
        ]
        .into_iter()
        .filter(|(_, changed)| *changed)
        .map(|(name, _)| name)
        .collect()
    }

    /// 既定値から1ステップ動かした結果を返す。既定値が上限に張り付いている項目
    /// (色数)は増やす方向では動かないため、減らす方向で動かす。
    fn adjusted_from_default(choice: ui::render::AiSettingsChoice) -> (Settings, Settings) {
        let before = Settings::default();
        let mut after = before;
        adjust_ai_setting(&mut after, choice, true);
        if after == before {
            adjust_ai_setting(&mut after, choice, false);
        }
        (before, after)
    }

    #[test]
    fn adjust_ai_setting_moves_the_selected_value_by_one_step() {
        let mut settings = Settings::default();
        let block_fall_before = settings.ai_block_fall_tick_ms;
        adjust_ai_setting(
            &mut settings,
            ui::render::AiSettingsChoice::BlockFallSpeed,
            true,
        );
        assert_eq!(
            settings.ai_block_fall_tick_ms,
            block_fall_before + DEBUG_FALL_TICK_STEP_MS
        );

        let rock_before = settings.ai_rock_spawn_rate_percent;
        adjust_ai_setting(&mut settings, ui::render::AiSettingsChoice::RockRate, true);
        assert_eq!(
            settings.ai_rock_spawn_rate_percent,
            rock_before + SPAWN_RATE_PERCENT_STEP
        );

        let color_count_before = settings.ai_color_count;
        adjust_ai_setting(
            &mut settings,
            ui::render::AiSettingsChoice::ColorCount,
            false,
        );
        assert_eq!(settings.ai_color_count, color_count_before - 1);
    }

    #[test]
    fn adjust_ai_setting_writes_only_its_own_field_for_every_item() {
        // 20項目それぞれが、自分のフィールドだけを動かすことを確認する
        // (書き先の取り違え・使い回しはここで表に出る)。
        let mut touched = Vec::new();
        for choice in ALL_AI_CHOICES {
            let (before, after) = adjusted_from_default(choice);
            assert_ne!(
                before, after,
                "{choice:?}はどちらの方向にも動かなかった(調整が未実装)"
            );
            let changed = changed_ai_field_names(&before, &after);
            assert_eq!(
                changed.len(),
                1,
                "{choice:?}で動いたフィールドが1つでない: {changed:?}"
            );
            touched.push(changed[0]);
        }
        let mut unique = touched.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            ALL_AI_CHOICES.len(),
            "複数の項目が同じフィールドへ書いている: {touched:?}"
        );
    }

    #[test]
    fn adjust_ai_setting_never_touches_the_human_side_values() {
        // AI専用設定の調整で人間側の値が動くと対戦の公平性が崩れるため、
        // AI専用フィールド以外が変わらないことを全項目で確認する。
        for choice in ALL_AI_CHOICES {
            let (before, after) = adjusted_from_default(choice);
            let human_side_only = Settings {
                ai_block_fall_tick_ms: before.ai_block_fall_tick_ms,
                ai_player_fall_tick_ms: before.ai_player_fall_tick_ms,
                ai_shake_duration_ms: before.ai_shake_duration_ms,
                ai_rock_spawn_rate_percent: before.ai_rock_spawn_rate_percent,
                ai_air_spawn_rate_percent: before.ai_air_spawn_rate_percent,
                ai_star_spawn_rate_percent: before.ai_star_spawn_rate_percent,
                ai_diamond_spawn_rate_percent: before.ai_diamond_spawn_rate_percent,
                ai_item_clear_above_rate_percent: before.ai_item_clear_above_rate_percent,
                ai_item_unify_colors_rate_percent: before.ai_item_unify_colors_rate_percent,
                ai_item_starify_screen_rate_percent: before.ai_item_starify_screen_rate_percent,
                ai_color_count: before.ai_color_count,
                ai_color_cluster_rate_percent: before.ai_color_cluster_rate_percent,
                ai_bomb_spawn_rate_percent: before.ai_bomb_spawn_rate_percent,
                ai_bomb_fuse_ms: before.ai_bomb_fuse_ms,
                ai_attack_blocks_per_rock: before.ai_attack_blocks_per_rock,
                ai_attack_rocks_per_wave_max: before.ai_attack_rocks_per_wave_max,
                ai_attack_blocks_per_bomb: before.ai_attack_blocks_per_bomb,
                ai_attack_bombs_per_wave_max: before.ai_attack_bombs_per_wave_max,
                ai_attack_bomb_ratio_percent: before.ai_attack_bomb_ratio_percent,
                ai_chain_vanish_interval_ms: before.ai_chain_vanish_interval_ms,
                ..after
            };
            assert_eq!(
                human_side_only, before,
                "{choice:?}の調整で人間側の設定が変わっている"
            );
        }
    }

    #[test]
    fn adjust_ai_setting_clamps_every_item_at_both_ends() {
        // 破損したsettings.jsonの値でも飽和するだけ、という人間側と同じ性質を全項目で確認する。
        for choice in ALL_AI_CHOICES {
            for increase in [true, false] {
                let mut settings = Settings::default();
                for _ in 0..100 {
                    adjust_ai_setting(&mut settings, choice, increase);
                }
                let saturated = settings;
                adjust_ai_setting(&mut settings, choice, increase);
                assert_eq!(
                    settings,
                    saturated,
                    "{choice:?}を{}方向へ動かし続けたのに範囲へ収まっていない",
                    if increase { "増やす" } else { "減らす" }
                );
            }
        }
    }
}

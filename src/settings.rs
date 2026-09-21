//! ユーザー設定(MUSIC/SE ON・OFF・デバッグ速度ショートカットの調整値)の永続化(TERM独自拡張)。
//!
//! `dirs`クレートでOSごとのユーザーデータディレクトリを解決し、
//! `misterdrillerterm/settings.json`としてJSON形式で保存する。保存先が
//! 解決できない・読み書きに失敗する等の場合は、ゲーム自体は継続できるよう
//! 常に既定値へフォールバックし、エラーを呼び出し側へは伝播させない。

use std::io::Write;
use std::path::PathBuf;

use crate::constants::{
    ATTACK_BLOCKS_PER_BOMB_DEFAULT, ATTACK_BLOCKS_PER_ROCK_DEFAULT,
    ATTACK_BOMB_RATIO_PERCENT_DEFAULT, ATTACK_BOMBS_PER_WAVE_MAX_DEFAULT,
    ATTACK_ROCKS_PER_WAVE_MAX_DEFAULT, BOMB_FUSE_MS, CHAIN_VANISH_INTERVAL_MS_DEFAULT,
    COLOR_COUNT_DEFAULT, COURSE_NORMAL_DEPTH_M, DODGE_RECOVERY_MS_DEFAULT, FALL_TICK_MS,
    FIELD_WIDTH_DEFAULT, MOVE_COOLDOWN_MS_DEFAULT, REWIND_STOCK_MAX_DEFAULT,
    REWIND_STOCK_MAX_SETTING_MAX, REWIND_STOCK_MAX_SETTING_MIN, SHAKE_DURATION_MS,
    SOUND_VOLUME_PERCENT_DEFAULT, SOUND_VOLUME_PERCENT_MAX, SPAWN_RATE_PERCENT_DEFAULT,
};

const SETTINGS_DIR_NAME: &str = "misterdrillerterm";
const SETTINGS_FILE_NAME: &str = "settings.json";

/// 永続化するユーザー設定一式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// MUSIC(BGM)のON/OFF。TERM独自拡張。ユーザー指摘により、一括のサウンドON/OFFから
    /// MUSIC/SEの個別トグルへ分離した。
    pub music_enabled: bool,
    /// SE(効果音)のON/OFF。
    pub se_enabled: bool,
    /// MUSIC(BGM)の音量(%、0〜100、10%刻み。TERM独自拡張。#224)。ON/OFFとは別に、
    /// アプリ内部のミックスゲインだけを調整する(OS側のシステム音量には触れない)。
    /// 設定画面から調整する。
    pub music_volume_percent: u32,
    /// SE(効果音)の音量(%、同上)。設定画面から調整する。
    pub se_volume_percent: u32,
    /// 設定画面/デバッグショートカット([ ] キー)のどちらからも調整できるブロック落下速度
    /// (tick間隔, ms)。
    pub block_fall_tick_ms: u64,
    /// 設定画面/デバッグショートカット(- = キー)のどちらからも調整できるプレイヤー自由落下
    /// 速度(tick間隔, ms)。
    pub player_fall_tick_ms: u64,
    /// 設定画面/デバッグショートカット(, . キー)のどちらからも調整できる揺れ時間
    /// (落下開始までの時間, ms)。
    pub shake_duration_ms: u64,
    /// Xブロック(岩)の出現率(%、100=通常のまま。TERM独自拡張)。設定画面から調整する。
    pub rock_spawn_rate_percent: u32,
    /// AIR(酸素カプセル)の出現率(%、100=通常のまま。TERM独自拡張)。設定画面から調整する。
    pub air_spawn_rate_percent: u32,
    /// スターブロックの出現率(%、100=通常のまま。0=完全に出現させない。TERM独自拡張)。
    /// 設定画面から調整する。
    pub star_spawn_rate_percent: u32,
    /// ダイヤブロックの出現率(%、100=通常のまま。0=完全に出現させない。TERM独自拡張)。
    /// 設定画面から調整する。
    pub diamond_spawn_rate_percent: u32,
    /// アイテムブロック(ClearAbove、ショートカットR効果)の出現率(%、100=通常のまま。
    /// 0=完全に出現させない。TERM独自拡張。ユーザー指摘: 「各種アイテムの出現頻度の
    /// 設定項目増やして」)。設定画面から調整する。
    pub item_clear_above_rate_percent: u32,
    /// アイテムブロック(UnifyColors、ショートカットC効果)の出現率(%、同上)。
    pub item_unify_colors_rate_percent: u32,
    /// アイテムブロック(StarifyScreen、ショートカットK効果)の出現率(%、同上)。
    pub item_starify_screen_rate_percent: u32,
    /// 出現する色ブロックの色数(1〜4、TERM独自拡張)。`ColorKind::ALL`の先頭から
    /// この数だけを使う。設定画面から調整する。
    pub color_count: u8,
    /// 色ブロックの結合しやすさ(%、100=通常のまま。0=完全にバラバラ。TERM独自拡張)。
    /// ユーザー指摘: 「ブロック配置の結合関係の割合を設定できるようにして」。
    /// 設定画面から調整する。
    pub color_cluster_rate_percent: u32,
    /// 「わ〜!」スライダー演出後、キャラが起き上がるまでの硬直インターバル(ms、
    /// TERM独自拡張)。設定画面から調整する。
    pub dodge_recovery_ms: u64,
    /// 横移動のクールダウン間隔(ms、小さいほど速い。TERM独自拡張)。ユーザー指摘:
    /// 「横移動のスピードを設定で変えられるように」。設定画面から調整する。
    pub move_cooldown_ms: u64,
    /// フィールド幅(列数、TERM独自拡張)。ユーザー指摘: 「設定値に列の数を変更
    /// できるようにして」。設定画面から調整する。新規ゲーム開始時にのみ反映され、
    /// プレイ中に変更しても次回開始まで見た目には反映しない。
    pub field_width: usize,
    /// ボム出現頻度(%、100=通常のまま。0=完全に出現させない。TERM独自拡張。#96)。
    /// 設定画面から調整する。
    pub bomb_spawn_rate_percent: u32,
    /// ボム設置(Ticking開始)から爆発までの時間(ms。既定`BOMB_FUSE_MS`。TERM独自拡張。
    /// #246)。設定画面から調整する。新規に出現するボムから反映され、設置済みボムの
    /// 残り時間には影響しない。
    pub bomb_fuse_ms: u32,
    /// 対戦の妨害ルール(#247)で、岩1個を降らせるのに必要な攻撃力(消したブロック数)。
    /// TERM独自拡張。設定画面から調整する。
    pub attack_blocks_per_rock: u32,
    /// 対戦の妨害ルール(#247)で、1回(1ウェーブ)に降らせる岩の個数上限。同上。
    pub attack_rocks_per_wave_max: u32,
    /// 対戦の妨害ルール(#304)で、ボム1個を降らせるのに必要な攻撃力。同上。
    pub attack_blocks_per_bomb: u32,
    /// 対戦の妨害ルール(#304)で、1回(1ウェーブ)に降らせるボムの個数上限。同上。
    pub attack_bombs_per_wave_max: u32,
    /// 対戦の妨害ルール(#304)で、送る攻撃力のうちボムへ振り分ける比率(%)。0なら岩だけを
    /// 送る。同上。
    pub attack_bomb_ratio_percent: u32,
    /// #85調査用のブロック状態遷移ログ(SQLite、`debug_log`モジュール)を記録するか
    /// どうか(TERM独自拡張。#167。ユーザー指摘: 「デバッグ用のDB記録するしない
    /// トグル設定に追加」)。設定画面から切り替える。既定は有効(以前の常時記録の
    /// 挙動を変えない)。
    pub debug_log_enabled: bool,
    /// 4連結以上の自動消滅が連鎖するとき、1回消滅するごとに次の重力解決までの
    /// 最小インターバル(ms、TERM独自拡張。#187)。ユーザー指摘: 「ブロックが消えて、
    /// 連鎖的に次ブロックが消えるとき、0msで連続するのではなく一定のインターバルで
    /// 連鎖するように」。既定は0(=従来通り)。設定画面から調整する。
    pub chain_vanish_interval_ms: u64,
    /// 前回モードセレクト画面で選んだコースのゴール深度(m、TERM独自拡張。#112。
    /// ユーザー指摘: 「起動フローにモードセレクト画面を追加」)。次回起動時の
    /// モードセレクト画面の初期選択として引き継ぐ。
    pub last_course_depth_m: usize,
    /// フレーム巻き戻し(TERM独自拡張。#233)で持てるストック(使用回数)の上限。
    /// `REWIND_STOCK_MAX_SETTING_MIN`(0=機能OFF)〜`REWIND_STOCK_MAX_SETTING_MAX`の
    /// 範囲で設定画面から調整する。
    pub rewind_stock_max: u8,

    // 以下はAI専用のミラー値(TERM独自拡張。#312)。対戦でAIの盤面を作るときにだけ使い、
    // 人間の盤面には影響しない。AIに勝てないときのハンディキャップとして人間用と別の値を
    // 持たせるためのもので、取り得る範囲・刻みは人間用と同じ定数を使い回す。既定値も
    // 人間用と同じにしてあり、何も触らなければ従来通り人間と同条件になる。
    /// AI専用のブロック落下間隔(ms)。
    pub ai_block_fall_tick_ms: u64,
    /// AI専用のキャラ落下間隔(ms)。
    pub ai_player_fall_tick_ms: u64,
    /// AI専用の揺れ時間(落下開始までの時間, ms)。
    pub ai_shake_duration_ms: u64,
    /// AI専用のXブロック(岩)の出現率(%)。
    pub ai_rock_spawn_rate_percent: u32,
    /// AI専用のAIR(酸素カプセル)の出現率(%)。
    pub ai_air_spawn_rate_percent: u32,
    /// AI専用のスターブロックの出現率(%)。
    pub ai_star_spawn_rate_percent: u32,
    /// AI専用のダイヤブロックの出現率(%)。
    pub ai_diamond_spawn_rate_percent: u32,
    /// AI専用のアイテムブロック(ClearAbove)の出現率(%)。
    pub ai_item_clear_above_rate_percent: u32,
    /// AI専用のアイテムブロック(UnifyColors)の出現率(%)。
    pub ai_item_unify_colors_rate_percent: u32,
    /// AI専用のアイテムブロック(StarifyScreen)の出現率(%)。
    pub ai_item_starify_screen_rate_percent: u32,
    /// AI専用の色ブロックの色数(1〜4)。
    pub ai_color_count: u8,
    /// AI専用の色ブロックの結合しやすさ(%)。
    pub ai_color_cluster_rate_percent: u32,
    /// AI専用のボム出現頻度(%)。
    pub ai_bomb_spawn_rate_percent: u32,
    /// AI専用のボム設置から爆発までの時間(ms)。
    pub ai_bomb_fuse_ms: u32,
    /// AI専用の「岩1個を降らせるのに必要な攻撃力」。
    pub ai_attack_blocks_per_rock: u32,
    /// AI専用の「1ウェーブに降らせる岩の個数上限」。
    pub ai_attack_rocks_per_wave_max: u32,
    /// AI専用の「ボム1個を降らせるのに必要な攻撃力」。
    pub ai_attack_blocks_per_bomb: u32,
    /// AI専用の「1ウェーブに降らせるボムの個数上限」。
    pub ai_attack_bombs_per_wave_max: u32,
    /// AI専用の「送る攻撃力のうちボムへ振り分ける比率(%)」。
    pub ai_attack_bomb_ratio_percent: u32,
    /// AI専用の自動消滅の連鎖インターバル(ms)。
    pub ai_chain_vanish_interval_ms: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            music_enabled: true,
            se_enabled: true,
            music_volume_percent: SOUND_VOLUME_PERCENT_DEFAULT,
            se_volume_percent: SOUND_VOLUME_PERCENT_DEFAULT,
            block_fall_tick_ms: FALL_TICK_MS,
            player_fall_tick_ms: FALL_TICK_MS,
            shake_duration_ms: SHAKE_DURATION_MS,
            rock_spawn_rate_percent: SPAWN_RATE_PERCENT_DEFAULT,
            air_spawn_rate_percent: SPAWN_RATE_PERCENT_DEFAULT,
            star_spawn_rate_percent: SPAWN_RATE_PERCENT_DEFAULT,
            diamond_spawn_rate_percent: SPAWN_RATE_PERCENT_DEFAULT,
            item_clear_above_rate_percent: SPAWN_RATE_PERCENT_DEFAULT,
            item_unify_colors_rate_percent: SPAWN_RATE_PERCENT_DEFAULT,
            item_starify_screen_rate_percent: SPAWN_RATE_PERCENT_DEFAULT,
            color_count: COLOR_COUNT_DEFAULT,
            color_cluster_rate_percent: SPAWN_RATE_PERCENT_DEFAULT,
            dodge_recovery_ms: DODGE_RECOVERY_MS_DEFAULT,
            move_cooldown_ms: MOVE_COOLDOWN_MS_DEFAULT,
            field_width: FIELD_WIDTH_DEFAULT,
            bomb_spawn_rate_percent: SPAWN_RATE_PERCENT_DEFAULT,
            bomb_fuse_ms: BOMB_FUSE_MS,
            attack_blocks_per_rock: ATTACK_BLOCKS_PER_ROCK_DEFAULT,
            attack_rocks_per_wave_max: ATTACK_ROCKS_PER_WAVE_MAX_DEFAULT,
            attack_blocks_per_bomb: ATTACK_BLOCKS_PER_BOMB_DEFAULT,
            attack_bombs_per_wave_max: ATTACK_BOMBS_PER_WAVE_MAX_DEFAULT,
            attack_bomb_ratio_percent: ATTACK_BOMB_RATIO_PERCENT_DEFAULT,
            debug_log_enabled: true,
            chain_vanish_interval_ms: CHAIN_VANISH_INTERVAL_MS_DEFAULT,
            last_course_depth_m: COURSE_NORMAL_DEPTH_M,
            rewind_stock_max: REWIND_STOCK_MAX_DEFAULT,
            ai_block_fall_tick_ms: FALL_TICK_MS,
            ai_player_fall_tick_ms: FALL_TICK_MS,
            ai_shake_duration_ms: SHAKE_DURATION_MS,
            ai_rock_spawn_rate_percent: SPAWN_RATE_PERCENT_DEFAULT,
            ai_air_spawn_rate_percent: SPAWN_RATE_PERCENT_DEFAULT,
            ai_star_spawn_rate_percent: SPAWN_RATE_PERCENT_DEFAULT,
            ai_diamond_spawn_rate_percent: SPAWN_RATE_PERCENT_DEFAULT,
            ai_item_clear_above_rate_percent: SPAWN_RATE_PERCENT_DEFAULT,
            ai_item_unify_colors_rate_percent: SPAWN_RATE_PERCENT_DEFAULT,
            ai_item_starify_screen_rate_percent: SPAWN_RATE_PERCENT_DEFAULT,
            ai_color_count: COLOR_COUNT_DEFAULT,
            ai_color_cluster_rate_percent: SPAWN_RATE_PERCENT_DEFAULT,
            ai_bomb_spawn_rate_percent: SPAWN_RATE_PERCENT_DEFAULT,
            ai_bomb_fuse_ms: BOMB_FUSE_MS,
            ai_attack_blocks_per_rock: ATTACK_BLOCKS_PER_ROCK_DEFAULT,
            ai_attack_rocks_per_wave_max: ATTACK_ROCKS_PER_WAVE_MAX_DEFAULT,
            ai_attack_blocks_per_bomb: ATTACK_BLOCKS_PER_BOMB_DEFAULT,
            ai_attack_bombs_per_wave_max: ATTACK_BOMBS_PER_WAVE_MAX_DEFAULT,
            ai_attack_bomb_ratio_percent: ATTACK_BOMB_RATIO_PERCENT_DEFAULT,
            ai_chain_vanish_interval_ms: CHAIN_VANISH_INTERVAL_MS_DEFAULT,
        }
    }
}

fn settings_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join(SETTINGS_DIR_NAME).join(SETTINGS_FILE_NAME))
}

impl Settings {
    /// 保存済み設定を読み込む。保存先が無い/ファイルが無い場合は既定値を返す。
    /// 個々のフィールドはファイルの内容に関わらず独立にパースし、壊れている
    /// フィールドがあってもそのフィールドだけ既定値にフォールバックする。
    pub fn load() -> Self {
        let Some(path) = settings_path() else {
            return Self::default();
        };
        Self::load_from(&path)
    }

    /// `path`から設定を読み込む(実体、テストからは実ユーザーディレクトリを介さず
    /// 一時ディレクトリ上のパスで直接呼べる)。ファイルが無い場合は既定値を返す。
    fn load_from(path: &std::path::Path) -> Self {
        let default = Self::default();
        let Ok(text) = std::fs::read_to_string(path) else {
            return default;
        };
        Settings {
            music_enabled: parse_bool_field(&text, "music_enabled")
                .unwrap_or(default.music_enabled),
            se_enabled: parse_bool_field(&text, "se_enabled").unwrap_or(default.se_enabled),
            // 音量は他のフィールドと異なり、読み込み時に意図的にSOUND_VOLUME_PERCENT_MAX
            // でクランプする。手編集や破損データで異常値が入っていると、起動直後から
            // 振幅に直結する音量が爆音になりかねないため(#224)。
            music_volume_percent: parse_u64_field(&text, "music_volume_percent")
                .map(|v| v.min(SOUND_VOLUME_PERCENT_MAX as u64) as u32)
                .unwrap_or(default.music_volume_percent),
            se_volume_percent: parse_u64_field(&text, "se_volume_percent")
                .map(|v| v.min(SOUND_VOLUME_PERCENT_MAX as u64) as u32)
                .unwrap_or(default.se_volume_percent),
            block_fall_tick_ms: parse_u64_field(&text, "block_fall_tick_ms")
                .unwrap_or(default.block_fall_tick_ms),
            player_fall_tick_ms: parse_u64_field(&text, "player_fall_tick_ms")
                .unwrap_or(default.player_fall_tick_ms),
            shake_duration_ms: parse_u64_field(&text, "shake_duration_ms")
                .unwrap_or(default.shake_duration_ms),
            rock_spawn_rate_percent: parse_u64_field(&text, "rock_spawn_rate_percent")
                .map(|v| v as u32)
                .unwrap_or(default.rock_spawn_rate_percent),
            air_spawn_rate_percent: parse_u64_field(&text, "air_spawn_rate_percent")
                .map(|v| v as u32)
                .unwrap_or(default.air_spawn_rate_percent),
            star_spawn_rate_percent: parse_u64_field(&text, "star_spawn_rate_percent")
                .map(|v| v as u32)
                .unwrap_or(default.star_spawn_rate_percent),
            diamond_spawn_rate_percent: parse_u64_field(&text, "diamond_spawn_rate_percent")
                .map(|v| v as u32)
                .unwrap_or(default.diamond_spawn_rate_percent),
            item_clear_above_rate_percent: parse_u64_field(&text, "item_clear_above_rate_percent")
                .map(|v| v as u32)
                .unwrap_or(default.item_clear_above_rate_percent),
            item_unify_colors_rate_percent: parse_u64_field(
                &text,
                "item_unify_colors_rate_percent",
            )
            .map(|v| v as u32)
            .unwrap_or(default.item_unify_colors_rate_percent),
            item_starify_screen_rate_percent: parse_u64_field(
                &text,
                "item_starify_screen_rate_percent",
            )
            .map(|v| v as u32)
            .unwrap_or(default.item_starify_screen_rate_percent),
            color_count: parse_u64_field(&text, "color_count")
                .map(|v| v as u8)
                .unwrap_or(default.color_count),
            color_cluster_rate_percent: parse_u64_field(&text, "color_cluster_rate_percent")
                .map(|v| v as u32)
                .unwrap_or(default.color_cluster_rate_percent),
            dodge_recovery_ms: parse_u64_field(&text, "dodge_recovery_ms")
                .unwrap_or(default.dodge_recovery_ms),
            move_cooldown_ms: parse_u64_field(&text, "move_cooldown_ms")
                .unwrap_or(default.move_cooldown_ms),
            field_width: parse_u64_field(&text, "field_width")
                .map(|v| v as usize)
                .unwrap_or(default.field_width),
            bomb_spawn_rate_percent: parse_u64_field(&text, "bomb_spawn_rate_percent")
                .map(|v| v as u32)
                .unwrap_or(default.bomb_spawn_rate_percent),
            bomb_fuse_ms: parse_u64_field(&text, "bomb_fuse_ms")
                .map(|v| v as u32)
                .unwrap_or(default.bomb_fuse_ms),
            attack_blocks_per_rock: parse_u64_field(&text, "attack_blocks_per_rock")
                .map(|v| v as u32)
                .unwrap_or(default.attack_blocks_per_rock),
            attack_rocks_per_wave_max: parse_u64_field(&text, "attack_rocks_per_wave_max")
                .map(|v| v as u32)
                .unwrap_or(default.attack_rocks_per_wave_max),
            attack_blocks_per_bomb: parse_u64_field(&text, "attack_blocks_per_bomb")
                .map(|v| v as u32)
                .unwrap_or(default.attack_blocks_per_bomb),
            attack_bombs_per_wave_max: parse_u64_field(&text, "attack_bombs_per_wave_max")
                .map(|v| v as u32)
                .unwrap_or(default.attack_bombs_per_wave_max),
            attack_bomb_ratio_percent: parse_u64_field(&text, "attack_bomb_ratio_percent")
                .map(|v| v as u32)
                .unwrap_or(default.attack_bomb_ratio_percent),
            debug_log_enabled: parse_bool_field(&text, "debug_log_enabled")
                .unwrap_or(default.debug_log_enabled),
            chain_vanish_interval_ms: parse_u64_field(&text, "chain_vanish_interval_ms")
                .unwrap_or(default.chain_vanish_interval_ms),
            last_course_depth_m: parse_u64_field(&text, "last_course_depth_m")
                .map(|v| v as usize)
                .unwrap_or(default.last_course_depth_m),
            // 巻き戻しストック上限(#233)は取り得る値が0〜5と狭く、範囲外の値を
            // そのまま受け入れても設定画面の増減で戻せないだけなので、範囲外なら
            // 既定値へフォールバックする。
            rewind_stock_max: parse_u64_field(&text, "rewind_stock_max")
                .filter(|&v| {
                    (REWIND_STOCK_MAX_SETTING_MIN as u64..=REWIND_STOCK_MAX_SETTING_MAX as u64)
                        .contains(&v)
                })
                .map(|v| v as u8)
                .unwrap_or(default.rewind_stock_max),
            // AI専用値(#312)。既存の設定ファイルにはこれらのキーが無いので、
            // 1項目ずつ人間用と同じ既定値へフォールバックする。
            ai_block_fall_tick_ms: parse_u64_field(&text, "ai_block_fall_tick_ms")
                .unwrap_or(default.ai_block_fall_tick_ms),
            ai_player_fall_tick_ms: parse_u64_field(&text, "ai_player_fall_tick_ms")
                .unwrap_or(default.ai_player_fall_tick_ms),
            ai_shake_duration_ms: parse_u64_field(&text, "ai_shake_duration_ms")
                .unwrap_or(default.ai_shake_duration_ms),
            ai_rock_spawn_rate_percent: parse_u64_field(&text, "ai_rock_spawn_rate_percent")
                .map(|v| v as u32)
                .unwrap_or(default.ai_rock_spawn_rate_percent),
            ai_air_spawn_rate_percent: parse_u64_field(&text, "ai_air_spawn_rate_percent")
                .map(|v| v as u32)
                .unwrap_or(default.ai_air_spawn_rate_percent),
            ai_star_spawn_rate_percent: parse_u64_field(&text, "ai_star_spawn_rate_percent")
                .map(|v| v as u32)
                .unwrap_or(default.ai_star_spawn_rate_percent),
            ai_diamond_spawn_rate_percent: parse_u64_field(&text, "ai_diamond_spawn_rate_percent")
                .map(|v| v as u32)
                .unwrap_or(default.ai_diamond_spawn_rate_percent),
            ai_item_clear_above_rate_percent: parse_u64_field(
                &text,
                "ai_item_clear_above_rate_percent",
            )
            .map(|v| v as u32)
            .unwrap_or(default.ai_item_clear_above_rate_percent),
            ai_item_unify_colors_rate_percent: parse_u64_field(
                &text,
                "ai_item_unify_colors_rate_percent",
            )
            .map(|v| v as u32)
            .unwrap_or(default.ai_item_unify_colors_rate_percent),
            ai_item_starify_screen_rate_percent: parse_u64_field(
                &text,
                "ai_item_starify_screen_rate_percent",
            )
            .map(|v| v as u32)
            .unwrap_or(default.ai_item_starify_screen_rate_percent),
            ai_color_count: parse_u64_field(&text, "ai_color_count")
                .map(|v| v as u8)
                .unwrap_or(default.ai_color_count),
            ai_color_cluster_rate_percent: parse_u64_field(&text, "ai_color_cluster_rate_percent")
                .map(|v| v as u32)
                .unwrap_or(default.ai_color_cluster_rate_percent),
            ai_bomb_spawn_rate_percent: parse_u64_field(&text, "ai_bomb_spawn_rate_percent")
                .map(|v| v as u32)
                .unwrap_or(default.ai_bomb_spawn_rate_percent),
            ai_bomb_fuse_ms: parse_u64_field(&text, "ai_bomb_fuse_ms")
                .map(|v| v as u32)
                .unwrap_or(default.ai_bomb_fuse_ms),
            ai_attack_blocks_per_rock: parse_u64_field(&text, "ai_attack_blocks_per_rock")
                .map(|v| v as u32)
                .unwrap_or(default.ai_attack_blocks_per_rock),
            ai_attack_rocks_per_wave_max: parse_u64_field(&text, "ai_attack_rocks_per_wave_max")
                .map(|v| v as u32)
                .unwrap_or(default.ai_attack_rocks_per_wave_max),
            ai_attack_blocks_per_bomb: parse_u64_field(&text, "ai_attack_blocks_per_bomb")
                .map(|v| v as u32)
                .unwrap_or(default.ai_attack_blocks_per_bomb),
            ai_attack_bombs_per_wave_max: parse_u64_field(&text, "ai_attack_bombs_per_wave_max")
                .map(|v| v as u32)
                .unwrap_or(default.ai_attack_bombs_per_wave_max),
            ai_attack_bomb_ratio_percent: parse_u64_field(&text, "ai_attack_bomb_ratio_percent")
                .map(|v| v as u32)
                .unwrap_or(default.ai_attack_bomb_ratio_percent),
            ai_chain_vanish_interval_ms: parse_u64_field(&text, "ai_chain_vanish_interval_ms")
                .unwrap_or(default.ai_chain_vanish_interval_ms),
        }
    }

    /// 設定を保存する。保存先ディレクトリが無ければ作成する。書き込みに失敗しても
    /// (権限が無い等)ゲーム自体は継続できるよう、エラーは無視する。
    pub fn save(self) {
        let Some(path) = settings_path() else {
            return;
        };
        self.save_to(&path);
    }

    /// `path`へ設定を保存する(実体、テストからは実ユーザーディレクトリを介さず
    /// 一時ディレクトリ上のパスで直接呼べる)。保存先ディレクトリが無ければ作成する。
    fn save_to(self, path: &std::path::Path) {
        if let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return;
        }
        let json = format!(
            "{{\n  \"music_enabled\": {},\n  \"se_enabled\": {},\n  \"music_volume_percent\": {},\n  \"se_volume_percent\": {},\n  \"block_fall_tick_ms\": {},\n  \"player_fall_tick_ms\": {},\n  \"shake_duration_ms\": {},\n  \"rock_spawn_rate_percent\": {},\n  \"air_spawn_rate_percent\": {},\n  \"star_spawn_rate_percent\": {},\n  \"diamond_spawn_rate_percent\": {},\n  \"item_clear_above_rate_percent\": {},\n  \"item_unify_colors_rate_percent\": {},\n  \"item_starify_screen_rate_percent\": {},\n  \"color_count\": {},\n  \"color_cluster_rate_percent\": {},\n  \"dodge_recovery_ms\": {},\n  \"move_cooldown_ms\": {},\n  \"field_width\": {},\n  \"bomb_spawn_rate_percent\": {},\n  \"bomb_fuse_ms\": {},\n  \"attack_blocks_per_rock\": {},\n  \"attack_rocks_per_wave_max\": {},\n  \"attack_blocks_per_bomb\": {},\n  \"attack_bombs_per_wave_max\": {},\n  \"attack_bomb_ratio_percent\": {},\n  \"debug_log_enabled\": {},\n  \"chain_vanish_interval_ms\": {},\n  \"last_course_depth_m\": {},\n  \"rewind_stock_max\": {},\n  \"ai_block_fall_tick_ms\": {},\n  \"ai_player_fall_tick_ms\": {},\n  \"ai_shake_duration_ms\": {},\n  \"ai_rock_spawn_rate_percent\": {},\n  \"ai_air_spawn_rate_percent\": {},\n  \"ai_star_spawn_rate_percent\": {},\n  \"ai_diamond_spawn_rate_percent\": {},\n  \"ai_item_clear_above_rate_percent\": {},\n  \"ai_item_unify_colors_rate_percent\": {},\n  \"ai_item_starify_screen_rate_percent\": {},\n  \"ai_color_count\": {},\n  \"ai_color_cluster_rate_percent\": {},\n  \"ai_bomb_spawn_rate_percent\": {},\n  \"ai_bomb_fuse_ms\": {},\n  \"ai_attack_blocks_per_rock\": {},\n  \"ai_attack_rocks_per_wave_max\": {},\n  \"ai_attack_blocks_per_bomb\": {},\n  \"ai_attack_bombs_per_wave_max\": {},\n  \"ai_attack_bomb_ratio_percent\": {},\n  \"ai_chain_vanish_interval_ms\": {}\n}}\n",
            self.music_enabled,
            self.se_enabled,
            self.music_volume_percent,
            self.se_volume_percent,
            self.block_fall_tick_ms,
            self.player_fall_tick_ms,
            self.shake_duration_ms,
            self.rock_spawn_rate_percent,
            self.air_spawn_rate_percent,
            self.star_spawn_rate_percent,
            self.diamond_spawn_rate_percent,
            self.item_clear_above_rate_percent,
            self.item_unify_colors_rate_percent,
            self.item_starify_screen_rate_percent,
            self.color_count,
            self.color_cluster_rate_percent,
            self.dodge_recovery_ms,
            self.move_cooldown_ms,
            self.field_width,
            self.bomb_spawn_rate_percent,
            self.bomb_fuse_ms,
            self.attack_blocks_per_rock,
            self.attack_rocks_per_wave_max,
            self.attack_blocks_per_bomb,
            self.attack_bombs_per_wave_max,
            self.attack_bomb_ratio_percent,
            self.debug_log_enabled,
            self.chain_vanish_interval_ms,
            self.last_course_depth_m,
            self.rewind_stock_max,
            self.ai_block_fall_tick_ms,
            self.ai_player_fall_tick_ms,
            self.ai_shake_duration_ms,
            self.ai_rock_spawn_rate_percent,
            self.ai_air_spawn_rate_percent,
            self.ai_star_spawn_rate_percent,
            self.ai_diamond_spawn_rate_percent,
            self.ai_item_clear_above_rate_percent,
            self.ai_item_unify_colors_rate_percent,
            self.ai_item_starify_screen_rate_percent,
            self.ai_color_count,
            self.ai_color_cluster_rate_percent,
            self.ai_bomb_spawn_rate_percent,
            self.ai_bomb_fuse_ms,
            self.ai_attack_blocks_per_rock,
            self.ai_attack_rocks_per_wave_max,
            self.ai_attack_blocks_per_bomb,
            self.ai_attack_bombs_per_wave_max,
            self.ai_attack_bomb_ratio_percent,
            self.ai_chain_vanish_interval_ms
        );
        // 一時ファイルへ書いてからrenameすることで保存をアトミックにする(TERM独自
        // 拡張。#158)。File::create+write_allをpathへ直接行うと、書き込み途中で
        // プロセスが中断された場合に既存の設定ファイルが不完全な内容のまま残る
        // おそれがあった。同一ディレクトリ内でのrenameはOS側でアトミックに行われる
        // ため、この方式なら途中経過が既存のpathへ反映されることはない。
        let mut tmp_path = path.as_os_str().to_owned();
        tmp_path.push(".tmp");
        let tmp_path = std::path::PathBuf::from(tmp_path);

        let Ok(mut file) = std::fs::File::create(&tmp_path) else {
            return;
        };
        if file.write_all(json.as_bytes()).is_err() {
            let _ = std::fs::remove_file(&tmp_path);
            return;
        }
        drop(file);
        if std::fs::rename(&tmp_path, path).is_err() {
            let _ = std::fs::remove_file(&tmp_path);
        }
    }
}

/// 手書きの最小限JSONパーサ: `"key": true|false`の形の真偽値フィールドを1つ読む。
/// この用途に見合わない`serde`等の依存追加を避けるため、あえて手書きにしている。
fn parse_bool_field(text: &str, key: &str) -> Option<bool> {
    let after_colon = value_after_key(text, key)?;
    if after_colon.starts_with("true") {
        Some(true)
    } else if after_colon.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// 手書きの最小限JSONパーサ: `"key": 123`の形の非負整数フィールドを1つ読む。
fn parse_u64_field(text: &str, key: &str) -> Option<u64> {
    let after_colon = value_after_key(text, key)?;
    let digits_end = after_colon
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after_colon.len());
    after_colon[..digits_end].parse().ok()
}

/// `"key": <値>`の`<値>`より前の空白を読み飛ばした位置から始まる部分文字列を返す。
fn value_after_key<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let quoted_key = format!("\"{key}\"");
    let key_pos = text.find(&quoted_key)?;
    let after_key = &text[key_pos + quoted_key.len()..];
    let colon_pos = after_key.find(':')?;
    Some(after_key[colon_pos + 1..].trim_start())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{
        ATTACK_BLOCKS_PER_BOMB_MAX, ATTACK_BLOCKS_PER_BOMB_MIN, ATTACK_BLOCKS_PER_ROCK_MAX,
        ATTACK_BLOCKS_PER_ROCK_MIN, ATTACK_BOMB_RATIO_PERCENT_MAX, ATTACK_BOMB_RATIO_PERCENT_MIN,
        ATTACK_BOMBS_PER_WAVE_MAX_MAX, ATTACK_BOMBS_PER_WAVE_MAX_MIN,
        ATTACK_ROCKS_PER_WAVE_MAX_MAX, ATTACK_ROCKS_PER_WAVE_MAX_MIN,
    };

    #[test]
    fn default_settings_has_music_and_se_enabled_and_default_fall_speeds() {
        let settings = Settings::default();
        assert!(settings.music_enabled);
        assert!(settings.se_enabled);
        assert_eq!(settings.music_volume_percent, SOUND_VOLUME_PERCENT_DEFAULT);
        assert_eq!(settings.se_volume_percent, SOUND_VOLUME_PERCENT_DEFAULT);
        assert_eq!(settings.block_fall_tick_ms, FALL_TICK_MS);
        assert_eq!(settings.player_fall_tick_ms, FALL_TICK_MS);
        assert_eq!(settings.rock_spawn_rate_percent, SPAWN_RATE_PERCENT_DEFAULT);
        assert_eq!(settings.air_spawn_rate_percent, SPAWN_RATE_PERCENT_DEFAULT);
        assert_eq!(settings.star_spawn_rate_percent, SPAWN_RATE_PERCENT_DEFAULT);
        assert_eq!(
            settings.diamond_spawn_rate_percent,
            SPAWN_RATE_PERCENT_DEFAULT
        );
        assert_eq!(
            settings.item_clear_above_rate_percent,
            SPAWN_RATE_PERCENT_DEFAULT
        );
        assert_eq!(
            settings.item_unify_colors_rate_percent,
            SPAWN_RATE_PERCENT_DEFAULT
        );
        assert_eq!(
            settings.item_starify_screen_rate_percent,
            SPAWN_RATE_PERCENT_DEFAULT
        );
        assert_eq!(settings.color_count, COLOR_COUNT_DEFAULT);
        assert_eq!(
            settings.color_cluster_rate_percent,
            SPAWN_RATE_PERCENT_DEFAULT
        );
        assert_eq!(settings.dodge_recovery_ms, DODGE_RECOVERY_MS_DEFAULT);
        assert_eq!(settings.move_cooldown_ms, MOVE_COOLDOWN_MS_DEFAULT);
        assert_eq!(settings.field_width, FIELD_WIDTH_DEFAULT);
        assert_eq!(settings.bomb_spawn_rate_percent, SPAWN_RATE_PERCENT_DEFAULT);
        assert_eq!(settings.bomb_fuse_ms, BOMB_FUSE_MS);
        assert_eq!(
            settings.chain_vanish_interval_ms,
            CHAIN_VANISH_INTERVAL_MS_DEFAULT
        );
        assert_eq!(settings.last_course_depth_m, COURSE_NORMAL_DEPTH_M);
        assert_eq!(settings.rewind_stock_max, REWIND_STOCK_MAX_DEFAULT);
        assert_eq!(
            settings.attack_blocks_per_rock,
            ATTACK_BLOCKS_PER_ROCK_DEFAULT
        );
        assert_eq!(
            settings.attack_rocks_per_wave_max,
            ATTACK_ROCKS_PER_WAVE_MAX_DEFAULT
        );
        assert_eq!(
            settings.attack_blocks_per_bomb,
            ATTACK_BLOCKS_PER_BOMB_DEFAULT
        );
        assert_eq!(
            settings.attack_bombs_per_wave_max,
            ATTACK_BOMBS_PER_WAVE_MAX_DEFAULT
        );
        assert_eq!(
            settings.attack_bomb_ratio_percent,
            ATTACK_BOMB_RATIO_PERCENT_DEFAULT
        );
    }

    #[test]
    fn load_from_missing_attack_rule_keys_falls_back_to_defaults() {
        // #247・#304を追加する前に保存されたsettings.jsonにはキー自体が無い。
        let path = temp_settings_path("attack-missing-keys");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\"music_enabled\": true}").unwrap();

        let loaded = Settings::load_from(&path);

        assert_eq!(
            loaded.attack_blocks_per_rock,
            ATTACK_BLOCKS_PER_ROCK_DEFAULT
        );
        assert_eq!(
            loaded.attack_rocks_per_wave_max,
            ATTACK_ROCKS_PER_WAVE_MAX_DEFAULT
        );
        assert_eq!(
            loaded.attack_blocks_per_bomb,
            ATTACK_BLOCKS_PER_BOMB_DEFAULT
        );
        assert_eq!(
            loaded.attack_bombs_per_wave_max,
            ATTACK_BOMBS_PER_WAVE_MAX_DEFAULT
        );
        assert_eq!(
            loaded.attack_bomb_ratio_percent,
            ATTACK_BOMB_RATIO_PERCENT_DEFAULT
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn load_from_reads_the_saved_attack_rule_values() {
        let path = temp_settings_path("attack-values");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\"attack_blocks_per_rock\": 7, \"attack_rocks_per_wave_max\": 3, \"attack_blocks_per_bomb\": 17, \"attack_bombs_per_wave_max\": 4, \"attack_bomb_ratio_percent\": 45}",
        )
        .unwrap();

        let loaded = Settings::load_from(&path);

        assert_eq!(loaded.attack_blocks_per_rock, 7);
        assert_eq!(loaded.attack_rocks_per_wave_max, 3);
        assert_eq!(loaded.attack_blocks_per_bomb, 17);
        assert_eq!(loaded.attack_bombs_per_wave_max, 4);
        assert_eq!(loaded.attack_bomb_ratio_percent, 45);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn parse_bool_field_reads_true() {
        assert_eq!(
            parse_bool_field("{\"music_enabled\": true}", "music_enabled"),
            Some(true)
        );
    }

    #[test]
    fn parse_bool_field_reads_false_with_pretty_formatting() {
        assert_eq!(
            parse_bool_field("{\n  \"se_enabled\": false\n}\n", "se_enabled"),
            Some(false)
        );
    }

    #[test]
    fn parse_bool_field_returns_none_for_malformed_or_missing_key() {
        assert_eq!(parse_bool_field("not json", "music_enabled"), None);
        assert_eq!(parse_bool_field("{}", "music_enabled"), None);
    }

    #[test]
    fn parse_u64_field_reads_value() {
        assert_eq!(
            parse_u64_field("{\"block_fall_tick_ms\": 275}", "block_fall_tick_ms"),
            Some(275)
        );
    }

    #[test]
    fn parse_u64_field_returns_none_for_missing_key() {
        assert_eq!(parse_u64_field("{}", "block_fall_tick_ms"), None);
    }

    /// テスト専用: OSの実ユーザーデータディレクトリ(`settings_path()`)を一切
    /// 経由しない、一時ディレクトリ上の使い捨てパスを返す。`tag`はテストごとに
    /// ユニークな名前を渡し、並行実行される他テストのファイルと衝突しないようにする。
    fn temp_settings_path(tag: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "misterdrillerterm-settings-test-{tag}-{}",
                std::process::id()
            ))
            .join(SETTINGS_FILE_NAME)
    }

    #[test]
    fn save_then_load_round_trips_via_temp_dir() {
        let path = temp_settings_path("roundtrip");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        let a = Settings {
            music_enabled: false,
            se_enabled: true,
            music_volume_percent: 70,
            se_volume_percent: 0,
            block_fall_tick_ms: 200,
            player_fall_tick_ms: 100,
            shake_duration_ms: 300,
            rock_spawn_rate_percent: 140,
            air_spawn_rate_percent: 60,
            star_spawn_rate_percent: 0,
            diamond_spawn_rate_percent: 0,
            item_clear_above_rate_percent: 0,
            item_unify_colors_rate_percent: 60,
            item_starify_screen_rate_percent: 140,
            color_count: 1,
            color_cluster_rate_percent: 0,
            dodge_recovery_ms: 500,
            move_cooldown_ms: 40,
            field_width: 8,
            bomb_spawn_rate_percent: 60,
            bomb_fuse_ms: 2500,
            attack_blocks_per_rock: ATTACK_BLOCKS_PER_ROCK_MIN,
            attack_rocks_per_wave_max: ATTACK_ROCKS_PER_WAVE_MAX_MIN,
            attack_blocks_per_bomb: ATTACK_BLOCKS_PER_BOMB_MIN,
            attack_bombs_per_wave_max: ATTACK_BOMBS_PER_WAVE_MAX_MIN,
            attack_bomb_ratio_percent: ATTACK_BOMB_RATIO_PERCENT_MIN,
            debug_log_enabled: false,
            chain_vanish_interval_ms: 150,
            last_course_depth_m: 500,
            rewind_stock_max: REWIND_STOCK_MAX_SETTING_MIN,
            // AI専用値(#312)は対応する人間用の値とわざと別の値にしておく。JSONの
            // 書き出し順とパース順がずれて取り違えられたらここで落ちる。
            ai_block_fall_tick_ms: 500,
            ai_player_fall_tick_ms: 450,
            ai_shake_duration_ms: 800,
            ai_rock_spawn_rate_percent: 11,
            ai_air_spawn_rate_percent: 12,
            ai_star_spawn_rate_percent: 13,
            ai_diamond_spawn_rate_percent: 14,
            ai_item_clear_above_rate_percent: 15,
            ai_item_unify_colors_rate_percent: 16,
            ai_item_starify_screen_rate_percent: 17,
            ai_color_count: 3,
            ai_color_cluster_rate_percent: 18,
            ai_bomb_spawn_rate_percent: 19,
            ai_bomb_fuse_ms: 7000,
            ai_attack_blocks_per_rock: ATTACK_BLOCKS_PER_ROCK_MAX,
            ai_attack_rocks_per_wave_max: ATTACK_ROCKS_PER_WAVE_MAX_MAX,
            ai_attack_blocks_per_bomb: ATTACK_BLOCKS_PER_BOMB_MAX,
            ai_attack_bombs_per_wave_max: ATTACK_BOMBS_PER_WAVE_MAX_MAX,
            ai_attack_bomb_ratio_percent: ATTACK_BOMB_RATIO_PERCENT_MAX,
            ai_chain_vanish_interval_ms: 950,
        };
        a.save_to(&path);
        assert_eq!(Settings::load_from(&path), a);

        let b = Settings {
            music_enabled: true,
            se_enabled: false,
            music_volume_percent: 100,
            se_volume_percent: 30,
            block_fall_tick_ms: 50,
            player_fall_tick_ms: 400,
            shake_duration_ms: 600,
            rock_spawn_rate_percent: 300,
            air_spawn_rate_percent: 20,
            star_spawn_rate_percent: 300,
            diamond_spawn_rate_percent: 300,
            item_clear_above_rate_percent: 300,
            item_unify_colors_rate_percent: 0,
            item_starify_screen_rate_percent: 20,
            color_count: 4,
            color_cluster_rate_percent: 300,
            dodge_recovery_ms: 2000,
            move_cooldown_ms: 300,
            field_width: 20,
            bomb_spawn_rate_percent: 300,
            bomb_fuse_ms: 9000,
            attack_blocks_per_rock: ATTACK_BLOCKS_PER_ROCK_MAX,
            attack_rocks_per_wave_max: ATTACK_ROCKS_PER_WAVE_MAX_MAX,
            attack_blocks_per_bomb: ATTACK_BLOCKS_PER_BOMB_MAX,
            attack_bombs_per_wave_max: ATTACK_BOMBS_PER_WAVE_MAX_MAX,
            attack_bomb_ratio_percent: ATTACK_BOMB_RATIO_PERCENT_MAX,
            debug_log_enabled: true,
            chain_vanish_interval_ms: 1000,
            last_course_depth_m: 1000,
            rewind_stock_max: REWIND_STOCK_MAX_SETTING_MAX,
            ai_block_fall_tick_ms: 120,
            ai_player_fall_tick_ms: 130,
            ai_shake_duration_ms: 140,
            ai_rock_spawn_rate_percent: 21,
            ai_air_spawn_rate_percent: 22,
            ai_star_spawn_rate_percent: 23,
            ai_diamond_spawn_rate_percent: 24,
            ai_item_clear_above_rate_percent: 25,
            ai_item_unify_colors_rate_percent: 26,
            ai_item_starify_screen_rate_percent: 27,
            ai_color_count: 2,
            ai_color_cluster_rate_percent: 28,
            ai_bomb_spawn_rate_percent: 29,
            ai_bomb_fuse_ms: 3000,
            ai_attack_blocks_per_rock: ATTACK_BLOCKS_PER_ROCK_MIN,
            ai_attack_rocks_per_wave_max: ATTACK_ROCKS_PER_WAVE_MAX_MIN,
            ai_attack_blocks_per_bomb: ATTACK_BLOCKS_PER_BOMB_MIN,
            ai_attack_bombs_per_wave_max: ATTACK_BOMBS_PER_WAVE_MAX_MIN,
            ai_attack_bomb_ratio_percent: ATTACK_BOMB_RATIO_PERCENT_MIN,
            ai_chain_vanish_interval_ms: 160,
        };
        b.save_to(&path);
        assert_eq!(Settings::load_from(&path), b);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// テスト専用: AI専用値(#312)20項目を、人間用のミラー元と同じ並びで数値化して返す。
    /// 20項目を1つずつ書き並べると読めなくなるので、配列で一括比較できるようにしている。
    fn ai_only_values(settings: &Settings) -> [u64; 20] {
        [
            settings.ai_block_fall_tick_ms,
            settings.ai_player_fall_tick_ms,
            settings.ai_shake_duration_ms,
            settings.ai_rock_spawn_rate_percent as u64,
            settings.ai_air_spawn_rate_percent as u64,
            settings.ai_star_spawn_rate_percent as u64,
            settings.ai_diamond_spawn_rate_percent as u64,
            settings.ai_item_clear_above_rate_percent as u64,
            settings.ai_item_unify_colors_rate_percent as u64,
            settings.ai_item_starify_screen_rate_percent as u64,
            settings.ai_color_count as u64,
            settings.ai_color_cluster_rate_percent as u64,
            settings.ai_bomb_spawn_rate_percent as u64,
            settings.ai_bomb_fuse_ms as u64,
            settings.ai_attack_blocks_per_rock as u64,
            settings.ai_attack_rocks_per_wave_max as u64,
            settings.ai_attack_blocks_per_bomb as u64,
            settings.ai_attack_bombs_per_wave_max as u64,
            settings.ai_attack_bomb_ratio_percent as u64,
            settings.ai_chain_vanish_interval_ms,
        ]
    }

    /// テスト専用: `ai_only_values`と同じ並びで人間用の値を返す。
    fn human_mirrored_values(settings: &Settings) -> [u64; 20] {
        [
            settings.block_fall_tick_ms,
            settings.player_fall_tick_ms,
            settings.shake_duration_ms,
            settings.rock_spawn_rate_percent as u64,
            settings.air_spawn_rate_percent as u64,
            settings.star_spawn_rate_percent as u64,
            settings.diamond_spawn_rate_percent as u64,
            settings.item_clear_above_rate_percent as u64,
            settings.item_unify_colors_rate_percent as u64,
            settings.item_starify_screen_rate_percent as u64,
            settings.color_count as u64,
            settings.color_cluster_rate_percent as u64,
            settings.bomb_spawn_rate_percent as u64,
            settings.bomb_fuse_ms as u64,
            settings.attack_blocks_per_rock as u64,
            settings.attack_rocks_per_wave_max as u64,
            settings.attack_blocks_per_bomb as u64,
            settings.attack_bombs_per_wave_max as u64,
            settings.attack_bomb_ratio_percent as u64,
            settings.chain_vanish_interval_ms,
        ]
    }

    #[test]
    fn default_settings_starts_the_ai_only_values_at_the_human_values() {
        // #312: AI専用の初期値は用意せず人間用と同じ定数を使う。何も触らなければ
        // 従来通りAIと人間が同条件で対戦する。
        let settings = Settings::default();
        assert_eq!(ai_only_values(&settings), human_mirrored_values(&settings));
    }

    #[test]
    fn load_from_missing_ai_setting_keys_falls_back_to_defaults() {
        // #312を追加する前に保存されたsettings.jsonにはai_*のキー自体が無い。
        // 人間用の値だけが書かれたファイルを読んでも、AI専用値は既定値になる。
        let path = temp_settings_path("ai-missing-keys");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\"block_fall_tick_ms\": 77, \"color_count\": 2, \"attack_blocks_per_rock\": 33}",
        )
        .unwrap();

        let loaded = Settings::load_from(&path);

        assert_eq!(loaded.block_fall_tick_ms, 77);
        assert_eq!(loaded.color_count, 2);
        assert_eq!(loaded.attack_blocks_per_rock, 33);
        assert_eq!(
            ai_only_values(&loaded),
            ai_only_values(&Settings::default())
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn save_then_load_round_trips_the_ai_only_values_without_touching_the_human_ones() {
        // #312: serdeを使わない手書きJSONなので、20項目のうち1つでも書き出しか
        // パースを落とすと静かに既定値へ戻る。人間用と別の値を保存して、両方が
        // 独立に復元されることを確認する。
        let path = temp_settings_path("ai-roundtrip");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        let base = Settings::default();
        let settings = Settings {
            ai_block_fall_tick_ms: base.block_fall_tick_ms + 1,
            ai_player_fall_tick_ms: base.player_fall_tick_ms + 2,
            ai_shake_duration_ms: base.shake_duration_ms + 3,
            ai_rock_spawn_rate_percent: base.rock_spawn_rate_percent + 4,
            ai_air_spawn_rate_percent: base.air_spawn_rate_percent + 5,
            ai_star_spawn_rate_percent: base.star_spawn_rate_percent + 6,
            ai_diamond_spawn_rate_percent: base.diamond_spawn_rate_percent + 7,
            ai_item_clear_above_rate_percent: base.item_clear_above_rate_percent + 8,
            ai_item_unify_colors_rate_percent: base.item_unify_colors_rate_percent + 9,
            ai_item_starify_screen_rate_percent: base.item_starify_screen_rate_percent + 10,
            ai_color_count: base.color_count - 1,
            ai_color_cluster_rate_percent: base.color_cluster_rate_percent + 11,
            ai_bomb_spawn_rate_percent: base.bomb_spawn_rate_percent + 12,
            ai_bomb_fuse_ms: base.bomb_fuse_ms + 13,
            ai_attack_blocks_per_rock: base.attack_blocks_per_rock + 14,
            ai_attack_rocks_per_wave_max: base.attack_rocks_per_wave_max + 15,
            ai_attack_blocks_per_bomb: base.attack_blocks_per_bomb + 16,
            ai_attack_bombs_per_wave_max: base.attack_bombs_per_wave_max + 17,
            ai_attack_bomb_ratio_percent: base.attack_bomb_ratio_percent + 18,
            ai_chain_vanish_interval_ms: base.chain_vanish_interval_ms + 19,
            ..base
        };
        // 20項目すべてが人間用と別の値になっていないと、取り違えを検出できない。
        assert_ne!(ai_only_values(&settings), human_mirrored_values(&settings));
        for (ai, human) in ai_only_values(&settings)
            .iter()
            .zip(human_mirrored_values(&settings).iter())
        {
            assert_ne!(ai, human);
        }

        settings.save_to(&path);
        let loaded = Settings::load_from(&path);

        assert_eq!(loaded, settings);
        assert_eq!(human_mirrored_values(&loaded), human_mirrored_values(&base));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn save_to_creates_missing_parent_directory() {
        let path = temp_settings_path("mkdir");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert!(!path.parent().unwrap().exists());

        Settings::default().save_to(&path);

        assert!(path.exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn save_to_does_not_leave_the_temporary_file_behind() {
        // 一時ファイル+renameでアトミック化した実装が、成功時に`.tmp`ファイルを
        // 残さないことを確認する回帰テスト(TERM独自拡張。#158)。
        let path = temp_settings_path("no-leftover-tmp");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        Settings::default().save_to(&path);

        assert!(path.exists());
        let mut tmp_path = path.as_os_str().to_owned();
        tmp_path.push(".tmp");
        assert!(
            !std::path::Path::new(&tmp_path).exists(),
            "保存成功後は一時ファイルが残っていないはず"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn load_from_missing_file_falls_back_to_default() {
        let path = temp_settings_path("missing");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert_eq!(Settings::load_from(&path), Settings::default());
    }

    #[test]
    fn load_from_corrupted_file_falls_back_to_default() {
        let path = temp_settings_path("corrupted");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not valid json at all").unwrap();

        assert_eq!(Settings::load_from(&path), Settings::default());

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn load_from_partially_corrupted_file_keeps_valid_fields_and_defaults_the_rest() {
        // music_enabledだけ壊れていても、block_fall_tick_msは正しく読み取れる。
        let path = temp_settings_path("partial");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\"music_enabled\": maybe, \"block_fall_tick_ms\": 300}",
        )
        .unwrap();

        let loaded = Settings::load_from(&path);

        assert_eq!(loaded.music_enabled, Settings::default().music_enabled);
        assert_eq!(loaded.block_fall_tick_ms, 300);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn load_from_missing_volume_keys_falls_back_to_100_percent() {
        // 音量キー自体が無い(#224追加前に保存されたsettings.json等)場合は、
        // 既定値の100%へフォールバックする。
        let path = temp_settings_path("volume-missing-keys");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\"music_enabled\": true}").unwrap();

        let loaded = Settings::load_from(&path);

        assert_eq!(loaded.music_volume_percent, SOUND_VOLUME_PERCENT_DEFAULT);
        assert_eq!(loaded.se_volume_percent, SOUND_VOLUME_PERCENT_DEFAULT);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn load_from_out_of_range_volume_clamps_to_max() {
        // 手編集や破損データで範囲外(150%)の値が入っていても、起動直後から爆音に
        // ならないようSOUND_VOLUME_PERCENT_MAX(100%)へクランプする。
        let path = temp_settings_path("volume-out-of-range");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\"music_volume_percent\": 150, \"se_volume_percent\": 150}",
        )
        .unwrap();

        let loaded = Settings::load_from(&path);

        assert_eq!(loaded.music_volume_percent, SOUND_VOLUME_PERCENT_MAX);
        assert_eq!(loaded.se_volume_percent, SOUND_VOLUME_PERCENT_MAX);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn load_from_extremely_large_volume_still_clamps_to_max() {
        // u32の範囲を超える値(4294967296 = 2^32)でもu64のままクランプしてからu32へ
        // キャストするため、キャスト時の折り返りで小さい値へ化けたりしない(#227)。
        let path = temp_settings_path("volume-huge");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\"music_volume_percent\": 4294967296, \"se_volume_percent\": 4294967296}",
        )
        .unwrap();

        let loaded = Settings::load_from(&path);

        assert_eq!(loaded.music_volume_percent, SOUND_VOLUME_PERCENT_MAX);
        assert_eq!(loaded.se_volume_percent, SOUND_VOLUME_PERCENT_MAX);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn load_from_out_of_range_rewind_stock_max_falls_back_to_default() {
        // #233: 設定画面で取り得ない値(範囲外)がファイルに入っていた場合、そのまま
        // 受け入れると設定画面の増減操作だけでは正常な範囲へ戻せなくなるため既定値にする。
        let path = temp_settings_path("rewind-out-of-range");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\"rewind_stock_max\": 99}").unwrap();

        assert_eq!(
            Settings::load_from(&path).rewind_stock_max,
            REWIND_STOCK_MAX_DEFAULT
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn load_from_missing_rewind_stock_max_key_falls_back_to_default() {
        // #233を追加する前に保存されたsettings.jsonにはキー自体が無い。
        let path = temp_settings_path("rewind-missing-key");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\"music_enabled\": true}").unwrap();

        assert_eq!(
            Settings::load_from(&path).rewind_stock_max,
            REWIND_STOCK_MAX_DEFAULT
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn load_from_in_range_rewind_stock_max_is_kept_including_the_off_value() {
        // 範囲内(0=OFFを含む)の値は既定値へ丸めず、そのまま読み取る。
        let path = temp_settings_path("rewind-in-range");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        std::fs::write(&path, "{\"rewind_stock_max\": 0}").unwrap();
        assert_eq!(
            Settings::load_from(&path).rewind_stock_max,
            REWIND_STOCK_MAX_SETTING_MIN
        );

        std::fs::write(&path, "{\"rewind_stock_max\": 5}").unwrap();
        assert_eq!(
            Settings::load_from(&path).rewind_stock_max,
            REWIND_STOCK_MAX_SETTING_MAX
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn load_from_zero_volume_is_not_clamped() {
        // 0%(ミュート相当)は下限として正当な値なのでクランプで消してはいけない。
        let path = temp_settings_path("volume-zero");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\"music_volume_percent\": 0, \"se_volume_percent\": 0}",
        )
        .unwrap();

        let loaded = Settings::load_from(&path);

        assert_eq!(loaded.music_volume_percent, 0);
        assert_eq!(loaded.se_volume_percent, 0);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}

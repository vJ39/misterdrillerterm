//! ユーザー設定(MUSIC/SE ON・OFF・デバッグ速度ショートカットの調整値)の永続化(TERM独自拡張)。
//!
//! `dirs`クレートでOSごとのユーザーデータディレクトリを解決し、
//! `misterdrillerterm/settings.json`としてJSON形式で保存する。保存先が
//! 解決できない・読み書きに失敗する等の場合は、ゲーム自体は継続できるよう
//! 常に既定値へフォールバックし、エラーを呼び出し側へは伝播させない。

use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::constants::{
    ATTACK_BLOCKS_PER_BOMB_DEFAULT, ATTACK_BLOCKS_PER_BOMB_MAX, ATTACK_BLOCKS_PER_BOMB_MIN,
    ATTACK_BLOCKS_PER_ROCK_DEFAULT, ATTACK_BLOCKS_PER_ROCK_MAX, ATTACK_BLOCKS_PER_ROCK_MIN,
    ATTACK_BOMB_RATIO_PERCENT_DEFAULT, ATTACK_BOMB_RATIO_PERCENT_MAX,
    ATTACK_BOMB_RATIO_PERCENT_MIN, ATTACK_BOMBS_PER_WAVE_MAX_DEFAULT,
    ATTACK_BOMBS_PER_WAVE_MAX_MAX, ATTACK_BOMBS_PER_WAVE_MAX_MIN,
    ATTACK_ROCKS_PER_WAVE_MAX_DEFAULT, ATTACK_ROCKS_PER_WAVE_MAX_MAX,
    ATTACK_ROCKS_PER_WAVE_MAX_MIN, BOMB_FUSE_MS, BOMB_FUSE_MS_MAX, BOMB_FUSE_MS_MIN,
    BOMB_SPAWN_RATE_PERCENT_MAX, BOMB_SPAWN_RATE_PERCENT_MIN, CHAIN_VANISH_INTERVAL_MS_DEFAULT,
    CHAIN_VANISH_INTERVAL_MS_MAX, CHAIN_VANISH_INTERVAL_MS_MIN, COLOR_CLUSTER_RATE_PERCENT_MIN,
    COLOR_COUNT_DEFAULT, COLOR_COUNT_MAX, COLOR_COUNT_MIN, COURSE_NORMAL_DEPTH_M,
    DEBUG_FALL_TICK_MS_MAX, DEBUG_FALL_TICK_MS_MIN, DEBUG_SHAKE_DURATION_MS_MAX,
    DEBUG_SHAKE_DURATION_MS_MIN, DIAMOND_SPAWN_RATE_PERCENT_MIN, DODGE_RECOVERY_MS_DEFAULT,
    DODGE_RECOVERY_MS_MAX, DODGE_RECOVERY_MS_MIN, FALL_TICK_MS, FIELD_WIDTH_DEFAULT,
    FIELD_WIDTH_MAX, FIELD_WIDTH_MIN, ITEM_SPAWN_RATE_PERCENT_MIN, MOVE_COOLDOWN_MS_DEFAULT,
    MOVE_COOLDOWN_MS_MAX, MOVE_COOLDOWN_MS_MIN, REWIND_STOCK_MAX_DEFAULT,
    REWIND_STOCK_MAX_SETTING_MAX, REWIND_STOCK_MAX_SETTING_MIN, SHAKE_DURATION_MS,
    SOUND_VOLUME_PERCENT_DEFAULT, SOUND_VOLUME_PERCENT_MAX, SOUND_VOLUME_PERCENT_MIN,
    SPAWN_RATE_PERCENT_DEFAULT, SPAWN_RATE_PERCENT_MAX, SPAWN_RATE_PERCENT_MIN,
    STAR_SPAWN_RATE_PERCENT_MAX, STAR_SPAWN_RATE_PERCENT_MIN,
};

const SETTINGS_DIR_NAME: &str = "misterdrillerterm";
const SETTINGS_FILE_NAME: &str = "settings.json";

/// 永続化するユーザー設定一式。
///
/// JSONのキー名はフィールド名そのままで、`#[serde(rename)]`は使わない(手書きJSONで
/// 保存していた時代のファイルをそのまま読めるようにするため。フィールド名を変えると
/// 既存のsettings.jsonの該当項目が読めなくなる)。
///
/// `#[serde(default)]`はキーが欠けている項目だけを`Default`で補うためのもの。
/// 設定項目を追加した版より前に保存されたファイル(#312のai_*追加前等)を読んでも、
/// 既に保存されていた項目を捨てずに済む。型不一致等でJSONとして解釈できない場合は
/// 項目単位では救わず、`load_from`が全体を既定値へ差し替える。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
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
    pub fn load() -> Self {
        let Some(path) = settings_path() else {
            return Self::default();
        };
        Self::load_from(&path)
    }

    /// `path`から設定を読み込む(実体、テストからは実ユーザーディレクトリを介さず
    /// 一時ディレクトリ上のパスで直接呼べる)。ファイルが無い場合、およびJSONとして
    /// 解釈できない場合(値の型が違う・数値がフィールドの型に収まらない等)は、項目単位で
    /// 救わずに全体を既定値にする。以前は手書きパーサで項目ごとに拾っていたが、u64で
    /// 読んでからu32/u8へasキャストする形だったため、範囲外の値が黙って別の値に化ける
    /// 経路があった(#227で一度踏んでいる)。
    fn load_from(path: &std::path::Path) -> Self {
        let mut settings: Self = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        settings.validate();
        settings
    }

    /// 読み込んだ値を設定画面と同じMIN/MAX範囲へ収める。設定ファイルを直接書き換えれば
    /// 設定画面では選べない値も入ってくるため、ゲームが使う前にここで正す。AI専用項目
    /// (#312)は人間側と同じ範囲定数を共有する。真偽値の項目は範囲の概念が無いので対象外。
    fn validate(&mut self) {
        // 同じ範囲を持つ項目(人間側とAI側のミラー等)はまとめてクランプする。MINが0の
        // 項目も一律clampにしておく(範囲定数を後から変えたときに漏れないようにするため)。
        for v in [&mut self.music_volume_percent, &mut self.se_volume_percent] {
            *v = (*v).clamp(SOUND_VOLUME_PERCENT_MIN, SOUND_VOLUME_PERCENT_MAX);
        }
        for v in [
            &mut self.block_fall_tick_ms,
            &mut self.player_fall_tick_ms,
            &mut self.ai_block_fall_tick_ms,
            &mut self.ai_player_fall_tick_ms,
        ] {
            *v = (*v).clamp(DEBUG_FALL_TICK_MS_MIN, DEBUG_FALL_TICK_MS_MAX);
        }
        for v in [&mut self.shake_duration_ms, &mut self.ai_shake_duration_ms] {
            *v = (*v).clamp(DEBUG_SHAKE_DURATION_MS_MIN, DEBUG_SHAKE_DURATION_MS_MAX);
        }
        for v in [
            &mut self.rock_spawn_rate_percent,
            &mut self.air_spawn_rate_percent,
            &mut self.ai_rock_spawn_rate_percent,
            &mut self.ai_air_spawn_rate_percent,
        ] {
            *v = (*v).clamp(SPAWN_RATE_PERCENT_MIN, SPAWN_RATE_PERCENT_MAX);
        }
        for v in [
            &mut self.star_spawn_rate_percent,
            &mut self.ai_star_spawn_rate_percent,
        ] {
            *v = (*v).clamp(STAR_SPAWN_RATE_PERCENT_MIN, STAR_SPAWN_RATE_PERCENT_MAX);
        }
        // ダイヤ・アイテム・色の結合率は下限だけ0(=出現させない)で、上限は
        // SPAWN_RATE_PERCENT_MAXを共有する(設定画面の増減も同じ組み合わせ)。
        for v in [
            &mut self.diamond_spawn_rate_percent,
            &mut self.ai_diamond_spawn_rate_percent,
        ] {
            *v = (*v).clamp(DIAMOND_SPAWN_RATE_PERCENT_MIN, SPAWN_RATE_PERCENT_MAX);
        }
        for v in [
            &mut self.item_clear_above_rate_percent,
            &mut self.item_unify_colors_rate_percent,
            &mut self.item_starify_screen_rate_percent,
            &mut self.ai_item_clear_above_rate_percent,
            &mut self.ai_item_unify_colors_rate_percent,
            &mut self.ai_item_starify_screen_rate_percent,
        ] {
            *v = (*v).clamp(ITEM_SPAWN_RATE_PERCENT_MIN, SPAWN_RATE_PERCENT_MAX);
        }
        for v in [
            &mut self.color_cluster_rate_percent,
            &mut self.ai_color_cluster_rate_percent,
        ] {
            *v = (*v).clamp(COLOR_CLUSTER_RATE_PERCENT_MIN, SPAWN_RATE_PERCENT_MAX);
        }
        for v in [&mut self.color_count, &mut self.ai_color_count] {
            *v = (*v).clamp(COLOR_COUNT_MIN, COLOR_COUNT_MAX);
        }
        for v in [
            &mut self.bomb_spawn_rate_percent,
            &mut self.ai_bomb_spawn_rate_percent,
        ] {
            *v = (*v).clamp(BOMB_SPAWN_RATE_PERCENT_MIN, BOMB_SPAWN_RATE_PERCENT_MAX);
        }
        for v in [&mut self.bomb_fuse_ms, &mut self.ai_bomb_fuse_ms] {
            *v = (*v).clamp(BOMB_FUSE_MS_MIN, BOMB_FUSE_MS_MAX);
        }
        for v in [
            &mut self.attack_blocks_per_rock,
            &mut self.ai_attack_blocks_per_rock,
        ] {
            *v = (*v).clamp(ATTACK_BLOCKS_PER_ROCK_MIN, ATTACK_BLOCKS_PER_ROCK_MAX);
        }
        for v in [
            &mut self.attack_rocks_per_wave_max,
            &mut self.ai_attack_rocks_per_wave_max,
        ] {
            *v = (*v).clamp(ATTACK_ROCKS_PER_WAVE_MAX_MIN, ATTACK_ROCKS_PER_WAVE_MAX_MAX);
        }
        for v in [
            &mut self.attack_blocks_per_bomb,
            &mut self.ai_attack_blocks_per_bomb,
        ] {
            *v = (*v).clamp(ATTACK_BLOCKS_PER_BOMB_MIN, ATTACK_BLOCKS_PER_BOMB_MAX);
        }
        for v in [
            &mut self.attack_bombs_per_wave_max,
            &mut self.ai_attack_bombs_per_wave_max,
        ] {
            *v = (*v).clamp(ATTACK_BOMBS_PER_WAVE_MAX_MIN, ATTACK_BOMBS_PER_WAVE_MAX_MAX);
        }
        for v in [
            &mut self.attack_bomb_ratio_percent,
            &mut self.ai_attack_bomb_ratio_percent,
        ] {
            *v = (*v).clamp(ATTACK_BOMB_RATIO_PERCENT_MIN, ATTACK_BOMB_RATIO_PERCENT_MAX);
        }
        for v in [
            &mut self.chain_vanish_interval_ms,
            &mut self.ai_chain_vanish_interval_ms,
        ] {
            *v = (*v).clamp(CHAIN_VANISH_INTERVAL_MS_MIN, CHAIN_VANISH_INTERVAL_MS_MAX);
        }
        // AI側を持たない項目。
        self.dodge_recovery_ms = self
            .dodge_recovery_ms
            .clamp(DODGE_RECOVERY_MS_MIN, DODGE_RECOVERY_MS_MAX);
        self.move_cooldown_ms = self
            .move_cooldown_ms
            .clamp(MOVE_COOLDOWN_MS_MIN, MOVE_COOLDOWN_MS_MAX);
        self.field_width = self.field_width.clamp(FIELD_WIDTH_MIN, FIELD_WIDTH_MAX);
        // 巻き戻しストック上限(#233)だけはクランプせず既定値へ戻す。0が「機能OFF」という
        // 意味を持つ有効値なので、範囲外の値を上限/下限へ寄せると別の設定になってしまう。
        if !(REWIND_STOCK_MAX_SETTING_MIN..=REWIND_STOCK_MAX_SETTING_MAX)
            .contains(&self.rewind_stock_max)
        {
            self.rewind_stock_max = REWIND_STOCK_MAX_DEFAULT;
        }
        // last_course_depth_mはMIN/MAX定数を持たない(前回選んだコースの深度をそのまま
        // 引き継ぐだけの値)ため、ここでは触らない。
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
        // キー名も並び順もフィールドの宣言順そのままなので、手書きで組み立てていた頃と
        // 同じ内容になる。to_string_prettyは末尾に改行を付けないため、以前のファイルと
        // 同じく改行で終わるよう足す。
        let Ok(mut json) = serde_json::to_string_pretty(&self) else {
            return;
        };
        json.push('\n');
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

#[cfg(test)]
mod tests {
    use super::*;

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
            // キーと値の対応がずれて取り違えられたらここで落ちる。読み込み時に
            // `validate`が走るため、各値はMIN/MAXの範囲内から選ぶ。
            ai_block_fall_tick_ms: 500,
            ai_player_fall_tick_ms: 450,
            ai_shake_duration_ms: 800,
            ai_rock_spawn_rate_percent: 40,
            ai_air_spawn_rate_percent: 80,
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
        // `Settings`の`#[serde(default)]`(#322)がこの後方互換を担保している部分で、
        // これが外れると古いファイルを読んだ時に人間用の値まで既定値に戻る。
        let path = temp_settings_path("ai-missing-keys");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\"block_fall_tick_ms\": 77, \"color_count\": 2, \"attack_blocks_per_rock\": 23}",
        )
        .unwrap();

        let loaded = Settings::load_from(&path);

        assert_eq!(loaded.block_fall_tick_ms, 77);
        assert_eq!(loaded.color_count, 2);
        assert_eq!(loaded.attack_blocks_per_rock, 23);
        assert_eq!(
            ai_only_values(&loaded),
            ai_only_values(&Settings::default())
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn save_then_load_round_trips_the_ai_only_values_without_touching_the_human_ones() {
        // #312: 20項目のうち1つでもキーの対応を間違えると、静かに既定値へ戻るか
        // 人間用の値と入れ替わる。人間用と別の値を保存して、両方が独立に
        // 復元されることを確認する。
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
            // +16ではATTACK_BLOCKS_PER_BOMB_MAXを超えて`validate`に丸められるため、
            // 人間用と別の値のまま範囲に収まる+9にしている。
            ai_attack_blocks_per_bomb: base.attack_blocks_per_bomb + 9,
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
    fn load_from_partially_corrupted_file_falls_back_to_default_for_every_field() {
        // #322: 1箇所でもJSONとして解釈できない書き方があれば、同じファイルの中で
        // 読める値(block_fall_tick_ms)も採用せず全体を既定値にする。
        let path = temp_settings_path("partial");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\"music_enabled\": maybe, \"block_fall_tick_ms\": 300}",
        )
        .unwrap();

        let loaded = Settings::load_from(&path);

        assert_eq!(loaded, Settings::default());
        assert_ne!(loaded.block_fall_tick_ms, 300);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn load_from_type_mismatched_field_resets_the_other_valid_fields_too() {
        // JSON自体は壊れていないが1項目だけ型が合わない(数値のはずが文字列)場合も、
        // 項目単位では救わず全体を既定値にする(#322)。同じファイルに書かれていた
        // 正しい値(music_enabled=false, color_count=2)も残らない。
        let path = temp_settings_path("type-mismatch");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\"music_enabled\": false, \"color_count\": 2, \"block_fall_tick_ms\": \"300\"}",
        )
        .unwrap();

        let loaded = Settings::load_from(&path);

        assert_eq!(loaded, Settings::default());
        // 既定値と一致するだけでは書いた値が効いていないと言い切れないので、
        // ファイルの値がそのまま残っていないことも確かめる。
        assert!(loaded.music_enabled);
        assert_ne!(loaded.color_count, 2);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn load_from_ignores_unknown_keys() {
        // 設定項目が削除された版で保存したファイルを古い版で読む場合に備えて、
        // 知らないキーがあっても読み込み全体を失敗させない。
        let path = temp_settings_path("unknown-keys");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\"music_enabled\": false, \"removed_setting_from_another_version\": 1}",
        )
        .unwrap();

        assert!(!Settings::load_from(&path).music_enabled);

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
        // ならないようSOUND_VOLUME_PERCENT_MAX(100%)へクランプする。u32には収まる値
        // なので読み込み自体は成功し、その後の`validate`が丸める。
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
    fn load_from_volume_beyond_u32_falls_back_to_default_without_truncating() {
        // u32に収まらない値(1099511627776 = 2^40)は読み込み自体が失敗するため、
        // 全体が既定値になる。以前はu64で読んでu32へasキャストしていたので、下位32bit
        // だけが残って0(無音)のような別の値へ黙って化ける経路があった(#227)。
        let path = temp_settings_path("volume-huge");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\"music_volume_percent\": 1099511627776, \"se_volume_percent\": 1099511627776}",
        )
        .unwrap();

        let loaded = Settings::load_from(&path);

        assert_eq!(loaded, Settings::default());
        assert_ne!(loaded.music_volume_percent, 0);
        assert_ne!(loaded.se_volume_percent, 0);

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

    #[test]
    fn validate_keeps_default_settings_unchanged() {
        // 既定値はすべてMIN/MAXの範囲内にあるはず。ここが落ちるなら定数と既定値の
        // 組み合わせが壊れていて、初回起動時から値が丸められることになる。
        let mut settings = Settings::default();

        settings.validate();

        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn validate_clamps_human_and_ai_values_at_both_ends() {
        // #322: 読み込み後のクランプを`validate`1箇所に集めたので、人間用とAI用の
        // どちらも上下両方の範囲外から丸められることを確認する。
        let mut settings = Settings {
            music_volume_percent: SOUND_VOLUME_PERCENT_MAX + 1,
            block_fall_tick_ms: DEBUG_FALL_TICK_MS_MIN - 1,
            shake_duration_ms: DEBUG_SHAKE_DURATION_MS_MAX + 1,
            rock_spawn_rate_percent: SPAWN_RATE_PERCENT_MIN - 1,
            star_spawn_rate_percent: STAR_SPAWN_RATE_PERCENT_MAX + 1,
            color_count: COLOR_COUNT_MIN - 1,
            dodge_recovery_ms: DODGE_RECOVERY_MS_MAX + 1,
            move_cooldown_ms: MOVE_COOLDOWN_MS_MIN - 1,
            field_width: FIELD_WIDTH_MAX + 1,
            bomb_fuse_ms: BOMB_FUSE_MS_MIN - 1,
            attack_blocks_per_rock: ATTACK_BLOCKS_PER_ROCK_MIN - 1,
            chain_vanish_interval_ms: CHAIN_VANISH_INTERVAL_MS_MAX + 1,
            ai_block_fall_tick_ms: DEBUG_FALL_TICK_MS_MAX + 1,
            ai_rock_spawn_rate_percent: SPAWN_RATE_PERCENT_MAX + 1,
            ai_color_count: COLOR_COUNT_MAX + 1,
            ai_bomb_fuse_ms: BOMB_FUSE_MS_MAX + 1,
            ai_attack_bomb_ratio_percent: ATTACK_BOMB_RATIO_PERCENT_MAX + 1,
            ..Settings::default()
        };

        settings.validate();

        assert_eq!(settings.music_volume_percent, SOUND_VOLUME_PERCENT_MAX);
        assert_eq!(settings.block_fall_tick_ms, DEBUG_FALL_TICK_MS_MIN);
        assert_eq!(settings.shake_duration_ms, DEBUG_SHAKE_DURATION_MS_MAX);
        assert_eq!(settings.rock_spawn_rate_percent, SPAWN_RATE_PERCENT_MIN);
        assert_eq!(
            settings.star_spawn_rate_percent,
            STAR_SPAWN_RATE_PERCENT_MAX
        );
        assert_eq!(settings.color_count, COLOR_COUNT_MIN);
        assert_eq!(settings.dodge_recovery_ms, DODGE_RECOVERY_MS_MAX);
        assert_eq!(settings.move_cooldown_ms, MOVE_COOLDOWN_MS_MIN);
        assert_eq!(settings.field_width, FIELD_WIDTH_MAX);
        assert_eq!(settings.bomb_fuse_ms, BOMB_FUSE_MS_MIN);
        assert_eq!(settings.attack_blocks_per_rock, ATTACK_BLOCKS_PER_ROCK_MIN);
        assert_eq!(
            settings.chain_vanish_interval_ms,
            CHAIN_VANISH_INTERVAL_MS_MAX
        );
        assert_eq!(settings.ai_block_fall_tick_ms, DEBUG_FALL_TICK_MS_MAX);
        assert_eq!(settings.ai_rock_spawn_rate_percent, SPAWN_RATE_PERCENT_MAX);
        assert_eq!(settings.ai_color_count, COLOR_COUNT_MAX);
        assert_eq!(settings.ai_bomb_fuse_ms, BOMB_FUSE_MS_MAX);
        assert_eq!(
            settings.ai_attack_bomb_ratio_percent,
            ATTACK_BOMB_RATIO_PERCENT_MAX
        );
    }

    #[test]
    fn load_from_out_of_range_file_values_are_clamped() {
        // `validate`を通す入口が`load_from`であることの確認。ファイル側が範囲外でも
        // 起動時には範囲内の値になる。
        let path = temp_settings_path("clamp-on-load");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\"field_width\": 1, \"ai_color_count\": 200, \"ai_bomb_fuse_ms\": 999999}",
        )
        .unwrap();

        let loaded = Settings::load_from(&path);

        assert_eq!(loaded.field_width, FIELD_WIDTH_MIN);
        assert_eq!(loaded.ai_color_count, COLOR_COUNT_MAX);
        assert_eq!(loaded.ai_bomb_fuse_ms, BOMB_FUSE_MS_MAX);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn save_to_writes_every_setting_as_a_json_key() {
        // #322でserde_jsonへ移したが、キー名が変わると既存のsettings.jsonが読めなく
        // なる。50項目のキー名をここに書き出して固定し、フィールド名を変えたら
        // このテストで気付けるようにする。
        const EXPECTED_KEYS: [&str; 50] = [
            "music_enabled",
            "se_enabled",
            "music_volume_percent",
            "se_volume_percent",
            "block_fall_tick_ms",
            "player_fall_tick_ms",
            "shake_duration_ms",
            "rock_spawn_rate_percent",
            "air_spawn_rate_percent",
            "star_spawn_rate_percent",
            "diamond_spawn_rate_percent",
            "item_clear_above_rate_percent",
            "item_unify_colors_rate_percent",
            "item_starify_screen_rate_percent",
            "color_count",
            "color_cluster_rate_percent",
            "dodge_recovery_ms",
            "move_cooldown_ms",
            "field_width",
            "bomb_spawn_rate_percent",
            "bomb_fuse_ms",
            "attack_blocks_per_rock",
            "attack_rocks_per_wave_max",
            "attack_blocks_per_bomb",
            "attack_bombs_per_wave_max",
            "attack_bomb_ratio_percent",
            "debug_log_enabled",
            "chain_vanish_interval_ms",
            "last_course_depth_m",
            "rewind_stock_max",
            "ai_block_fall_tick_ms",
            "ai_player_fall_tick_ms",
            "ai_shake_duration_ms",
            "ai_rock_spawn_rate_percent",
            "ai_air_spawn_rate_percent",
            "ai_star_spawn_rate_percent",
            "ai_diamond_spawn_rate_percent",
            "ai_item_clear_above_rate_percent",
            "ai_item_unify_colors_rate_percent",
            "ai_item_starify_screen_rate_percent",
            "ai_color_count",
            "ai_color_cluster_rate_percent",
            "ai_bomb_spawn_rate_percent",
            "ai_bomb_fuse_ms",
            "ai_attack_blocks_per_rock",
            "ai_attack_rocks_per_wave_max",
            "ai_attack_blocks_per_bomb",
            "ai_attack_bombs_per_wave_max",
            "ai_attack_bomb_ratio_percent",
            "ai_chain_vanish_interval_ms",
        ];

        let path = temp_settings_path("json-keys");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        Settings::default().save_to(&path);
        let text = std::fs::read_to_string(&path).unwrap();

        for key in EXPECTED_KEYS {
            assert!(
                text.contains(&format!("\"{key}\":")),
                "キー{key}が書き出されていない"
            );
        }
        // 1行1項目で書き出されるので、行数から項目数も突き合わせる。項目を増やした時に
        // 上の一覧の更新漏れをここで検出する。
        assert_eq!(
            text.lines().filter(|line| line.contains("\": ")).count(),
            EXPECTED_KEYS.len()
        );
        // 手書きで組み立てていた頃と同じく改行で終わる。
        assert!(text.ends_with('\n'));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}

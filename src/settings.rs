//! ユーザー設定(MUSIC/SE ON・OFF・デバッグ速度ショートカットの調整値)の永続化(TERM独自拡張)。
//!
//! `dirs`クレートでOSごとのユーザーデータディレクトリを解決し、
//! `misterdrillerterm/settings.json`としてJSON形式で保存する。保存先が
//! 解決できない・読み書きに失敗する等の場合は、ゲーム自体は継続できるよう
//! 常に既定値へフォールバックし、エラーを呼び出し側へは伝播させない。

use std::io::Write;
use std::path::PathBuf;

use crate::constants::{
    CHAIN_VANISH_INTERVAL_MS_DEFAULT, COLOR_COUNT_DEFAULT, COURSE_NORMAL_DEPTH_M,
    DODGE_RECOVERY_MS_DEFAULT, FALL_TICK_MS, FIELD_WIDTH_DEFAULT, MOVE_COOLDOWN_MS_DEFAULT,
    SHAKE_DURATION_MS, SPAWN_RATE_PERCENT_DEFAULT,
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
    /// デバッグショートカット([ ] キー)で調整するブロック落下速度(tick間隔, ms)。
    pub block_fall_tick_ms: u64,
    /// デバッグショートカット(- = キー)で調整するプレイヤー自由落下速度(tick間隔, ms)。
    pub player_fall_tick_ms: u64,
    /// デバッグショートカット(, . キー)で調整する揺れ時間(落下開始までの時間, ms)。
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
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            music_enabled: true,
            se_enabled: true,
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
            debug_log_enabled: true,
            chain_vanish_interval_ms: CHAIN_VANISH_INTERVAL_MS_DEFAULT,
            last_course_depth_m: COURSE_NORMAL_DEPTH_M,
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
            debug_log_enabled: parse_bool_field(&text, "debug_log_enabled")
                .unwrap_or(default.debug_log_enabled),
            chain_vanish_interval_ms: parse_u64_field(&text, "chain_vanish_interval_ms")
                .unwrap_or(default.chain_vanish_interval_ms),
            last_course_depth_m: parse_u64_field(&text, "last_course_depth_m")
                .map(|v| v as usize)
                .unwrap_or(default.last_course_depth_m),
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
            "{{\n  \"music_enabled\": {},\n  \"se_enabled\": {},\n  \"block_fall_tick_ms\": {},\n  \"player_fall_tick_ms\": {},\n  \"shake_duration_ms\": {},\n  \"rock_spawn_rate_percent\": {},\n  \"air_spawn_rate_percent\": {},\n  \"star_spawn_rate_percent\": {},\n  \"diamond_spawn_rate_percent\": {},\n  \"item_clear_above_rate_percent\": {},\n  \"item_unify_colors_rate_percent\": {},\n  \"item_starify_screen_rate_percent\": {},\n  \"color_count\": {},\n  \"color_cluster_rate_percent\": {},\n  \"dodge_recovery_ms\": {},\n  \"move_cooldown_ms\": {},\n  \"field_width\": {},\n  \"bomb_spawn_rate_percent\": {},\n  \"debug_log_enabled\": {},\n  \"chain_vanish_interval_ms\": {},\n  \"last_course_depth_m\": {}\n}}\n",
            self.music_enabled,
            self.se_enabled,
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
            self.debug_log_enabled,
            self.chain_vanish_interval_ms,
            self.last_course_depth_m
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

    #[test]
    fn default_settings_has_music_and_se_enabled_and_default_fall_speeds() {
        let settings = Settings::default();
        assert!(settings.music_enabled);
        assert!(settings.se_enabled);
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
        assert_eq!(
            settings.chain_vanish_interval_ms,
            CHAIN_VANISH_INTERVAL_MS_DEFAULT
        );
        assert_eq!(settings.last_course_depth_m, COURSE_NORMAL_DEPTH_M);
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
            debug_log_enabled: false,
            chain_vanish_interval_ms: 150,
            last_course_depth_m: 500,
        };
        a.save_to(&path);
        assert_eq!(Settings::load_from(&path), a);

        let b = Settings {
            music_enabled: true,
            se_enabled: false,
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
            debug_log_enabled: true,
            chain_vanish_interval_ms: 1000,
            last_course_depth_m: 1000,
        };
        b.save_to(&path);
        assert_eq!(Settings::load_from(&path), b);

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
}

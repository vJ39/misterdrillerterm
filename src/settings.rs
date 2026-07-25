//! ユーザー設定(サウンドON/OFF)の永続化(TERM独自拡張)。
//!
//! `dirs`クレートでOSごとのユーザーデータディレクトリを解決し、
//! `misterdrillerterm/settings.json`としてJSON形式で保存する。保存先が
//! 解決できない・読み書きに失敗する等の場合は、ゲーム自体は継続できるよう
//! 常に既定値へフォールバックし、エラーを呼び出し側へは伝播させない。

use std::io::Write;
use std::path::PathBuf;

const SETTINGS_DIR_NAME: &str = "misterdrillerterm";
const SETTINGS_FILE_NAME: &str = "settings.json";

/// 永続化するユーザー設定一式。現状はサウンドON/OFFのみ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub sound_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { sound_enabled: true }
    }
}

fn settings_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join(SETTINGS_DIR_NAME).join(SETTINGS_FILE_NAME))
}

impl Settings {
    /// 保存済み設定を読み込む。保存先が無い/ファイルが無い/内容が壊れている場合は
    /// 既定値(サウンドON)を返す。
    pub fn load() -> Self {
        let Some(path) = settings_path() else {
            return Self::default();
        };
        Self::load_from(&path)
    }

    /// `path`から設定を読み込む(実体、テストからは実ユーザーディレクトリを介さず
    /// 一時ディレクトリ上のパスで直接呼べる)。ファイルが無い/内容が壊れている場合は
    /// 既定値(サウンドON)を返す。
    fn load_from(path: &std::path::Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match parse_sound_enabled(&text) {
            Some(sound_enabled) => Settings { sound_enabled },
            None => Self::default(),
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
        let json = format!("{{\n  \"sound_enabled\": {}\n}}\n", self.sound_enabled);
        if let Ok(mut file) = std::fs::File::create(path) {
            let _ = file.write_all(json.as_bytes());
        }
    }
}

/// 手書きの最小限JSONパーサ(`{"sound_enabled": true|false}`の1フィールドのみに対応)。
/// この用途に見合わない`serde`等の依存追加を避けるため、あえて手書きにしている。
fn parse_sound_enabled(text: &str) -> Option<bool> {
    const KEY: &str = "\"sound_enabled\"";
    let key_pos = text.find(KEY)?;
    let after_key = &text[key_pos + KEY.len()..];
    let colon_pos = after_key.find(':')?;
    let after_colon = after_key[colon_pos + 1..].trim_start();
    if after_colon.starts_with("true") {
        Some(true)
    } else if after_colon.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_has_sound_enabled() {
        assert!(Settings::default().sound_enabled);
    }

    #[test]
    fn parse_sound_enabled_reads_true() {
        assert_eq!(parse_sound_enabled("{\"sound_enabled\": true}"), Some(true));
    }

    #[test]
    fn parse_sound_enabled_reads_false_with_pretty_formatting() {
        assert_eq!(parse_sound_enabled("{\n  \"sound_enabled\": false\n}\n"), Some(false));
    }

    #[test]
    fn parse_sound_enabled_returns_none_for_malformed_or_missing_key() {
        assert_eq!(parse_sound_enabled("not json"), None);
        assert_eq!(parse_sound_enabled("{}"), None);
    }

    /// テスト専用: OSの実ユーザーデータディレクトリ(`settings_path()`)を一切
    /// 経由しない、一時ディレクトリ上の使い捨てパスを返す。`tag`はテストごとに
    /// ユニークな名前を渡し、並行実行される他テストのファイルと衝突しないようにする。
    fn temp_settings_path(tag: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("misterdrillerterm-settings-test-{tag}-{}", std::process::id()))
            .join(SETTINGS_FILE_NAME)
    }

    #[test]
    fn save_then_load_round_trips_via_temp_dir() {
        let path = temp_settings_path("roundtrip");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        Settings { sound_enabled: false }.save_to(&path);
        assert_eq!(Settings::load_from(&path), Settings { sound_enabled: false });

        Settings { sound_enabled: true }.save_to(&path);
        assert_eq!(Settings::load_from(&path), Settings { sound_enabled: true });

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn save_to_creates_missing_parent_directory() {
        let path = temp_settings_path("mkdir");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert!(!path.parent().unwrap().exists());

        Settings { sound_enabled: false }.save_to(&path);

        assert!(path.exists());
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
}

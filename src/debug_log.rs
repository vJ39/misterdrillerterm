//! #85(揺れているブロックが浮いたまま落下しない)の調査用に、ブロックの状態遷移
//! (移動・消滅)をフレーム番号つきでSQLiteへ記録する(TERM独自拡張。ユーザー指摘:
//! 「#85のデバッグ情報として、フレームのユニーク番号を取得できるようにしておき、
//! その付近の操作ログ(ブロック状態遷移ログ)を確認し、デバッグしやすくする」)。
//!
//! タイトルからゲームスタートした時点で毎回ファイルを作り直す(古いログを次の
//! プレイに持ち越さない)。書き込み失敗はゲーム進行に影響させず、単に記録を諦める。

use std::path::{Path, PathBuf};

use rusqlite::Connection;

const LOG_DIR_NAME: &str = "misterdrillerterm";
const LOG_FILE_NAME: &str = "debug_log.db";

/// ブロック状態遷移ログの記録先。開けなかった場合は`None`のまま扱われ、
/// `Game`側の記録呼び出しは全て無音のno-opになる。
pub struct DebugLog {
    conn: Connection,
}

fn debug_log_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join(LOG_DIR_NAME).join(LOG_FILE_NAME))
}

impl DebugLog {
    /// 既定の保存先に、前回までの内容を破棄した新しいログDBを作る。
    pub fn open_fresh() -> Option<Self> {
        let path = debug_log_path()?;
        Self::open_fresh_at(&path)
    }

    fn open_fresh_at(path: &Path) -> Option<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        let _ = std::fs::remove_file(path);
        let conn = Connection::open(path).ok()?;
        // 調査用の使い捨てログのため、耐障害性より書き込み速度を優先する
        // (プレイ中の書き込みでフレームレートを落とさないため)。
        conn.execute_batch(
            "PRAGMA synchronous = OFF;
             PRAGMA journal_mode = MEMORY;
             CREATE TABLE block_events (
                 frame INTEGER NOT NULL,
                 kind TEXT NOT NULL,
                 row INTEGER NOT NULL,
                 col INTEGER NOT NULL,
                 from_row INTEGER,
                 from_col INTEGER
             );",
        )
        .ok()?;
        Some(DebugLog { conn })
    }

    /// ブロックが1マス移動した(重力落下・アイテム効果等による着地)ことを記録する。
    pub fn log_move(&self, frame: u64, to: (usize, usize), from: (usize, usize)) {
        let _ = self.conn.execute(
            "INSERT INTO block_events (frame, kind, row, col, from_row, from_col) VALUES (?1, 'move', ?2, ?3, ?4, ?5)",
            rusqlite::params![frame as i64, to.0 as i64, to.1 as i64, from.0 as i64, from.1 as i64],
        );
    }

    /// ブロックが消滅した(自動消滅・スター溶解・アイテム効果等)ことを記録する。
    pub fn log_vanish(&self, frame: u64, pos: (usize, usize)) {
        let _ = self.conn.execute(
            "INSERT INTO block_events (frame, kind, row, col, from_row, from_col) VALUES (?1, 'vanish', ?2, ?3, NULL, NULL)",
            rusqlite::params![frame as i64, pos.0 as i64, pos.1 as i64],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_log_path(tag: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("misterdrillerterm-debug-log-test-{tag}-{}", std::process::id()))
            .join(LOG_FILE_NAME)
    }

    fn count_rows(conn: &Connection, kind: &str) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM block_events WHERE kind = ?1", [kind], |row| row.get(0)).unwrap()
    }

    #[test]
    fn log_move_and_log_vanish_insert_rows_with_the_given_frame() {
        let path = temp_log_path("insert");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        let log = DebugLog::open_fresh_at(&path).expect("一時ディレクトリで開けるはず");
        log.log_move(10, (5, 1), (4, 1));
        log.log_vanish(11, (5, 1));

        assert_eq!(count_rows(&log.conn, "move"), 1);
        assert_eq!(count_rows(&log.conn, "vanish"), 1);
        let frame: i64 = log.conn.query_row("SELECT frame FROM block_events WHERE kind = 'move'", [], |row| row.get(0)).unwrap();
        assert_eq!(frame, 10);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn open_fresh_at_discards_previous_content() {
        // ユーザー指摘: 「タイトルからゲームスタートした時点でログdbは毎回リフレッシュ
        // するものとする」。
        let path = temp_log_path("refresh");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        let log = DebugLog::open_fresh_at(&path).unwrap();
        log.log_move(1, (0, 0), (0, 0));
        assert_eq!(count_rows(&log.conn, "move"), 1);
        drop(log);

        let log = DebugLog::open_fresh_at(&path).expect("既存ファイルがあっても作り直せるはず");
        assert_eq!(count_rows(&log.conn, "move"), 0, "前回の内容は破棄されているはず");

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}

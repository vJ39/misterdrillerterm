//! #85(揺れているブロックが浮いたまま落下しない)の調査用に、ブロックの状態遷移
//! (移動・消滅)をフレーム番号つきでSQLiteへ記録する(TERM独自拡張。ユーザー指摘:
//! 「#85のデバッグ情報として、フレームのユニーク番号を取得できるようにしておき、
//! その付近の操作ログ(ブロック状態遷移ログ)を確認し、デバッグしやすくする」)。
//!
//! タイトルからゲームスタートした時点で毎回ファイルを作り直す(古いログを次の
//! プレイに持ち越さない)。書き込み失敗はゲーム進行に影響させず、単に記録を諦める。

use std::cell::Cell as StdCell;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

const LOG_DIR_NAME: &str = "misterdrillerterm";
const LOG_FILE_NAME: &str = "debug_log.db";

/// ブロック状態遷移ログの記録先。開けなかった場合は`None`のまま扱われ、
/// `Game`側の記録呼び出しは全て無音のno-opになる。
pub struct DebugLog {
    conn: Connection,
    /// `begin_frame`で開いたトランザクションが未コミットかどうか(TERM独自拡張。
    /// ユーザー指摘: 「あとちょっともっさりしてるからinsert高速化したい(cacheとか？)」)。
    /// 1フレームぶんのINSERTをまとめて1トランザクションにすることで、フレームごとに
    /// 多数のブロックが同時に動く/消滅する場面での書き込み負荷を下げる。
    in_transaction: StdCell<bool>,
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
                 cell_kind TEXT NOT NULL,
                 row INTEGER NOT NULL,
                 col INTEGER NOT NULL,
                 from_row INTEGER,
                 from_col INTEGER
             );
             CREATE TABLE player_state (
                 frame INTEGER PRIMARY KEY,
                 row INTEGER NOT NULL,
                 col INTEGER NOT NULL,
                 facing TEXT NOT NULL,
                 status TEXT NOT NULL
             );
             CREATE TABLE render_fallback_events (
                 frame INTEGER NOT NULL,
                 row INTEGER NOT NULL,
                 col INTEGER NOT NULL,
                 from_row INTEGER NOT NULL,
                 from_col INTEGER NOT NULL,
                 resolved_kind TEXT,
                 fallback_used INTEGER NOT NULL,
                 progress REAL NOT NULL,
                 effective_tick_ms INTEGER NOT NULL,
                 flash_remaining_ms INTEGER
             );",
        )
        .ok()?;
        Some(DebugLog {
            conn,
            in_transaction: StdCell::new(false),
        })
    }

    /// フレーム開始時に呼ぶ(TERM独自拡張)。前フレームぶんのトランザクションが
    /// 開いたままならコミットしてから、このフレームぶんの新しいトランザクションを開く
    /// (「次のbeginで前回分をコミットする」遅延コミット方式。呼び出し側`Game::update`
    /// には複数のreturn経路があり、全ての経路でコミットを漏れなく挟むのが煩雑なため)。
    /// 最後に残ったフレームぶんは`Drop`でコミットする。
    pub fn begin_frame(&self) {
        if self.in_transaction.get() {
            let _ = self.conn.execute_batch("COMMIT;");
        }
        if self.conn.execute_batch("BEGIN;").is_ok() {
            self.in_transaction.set(true);
        }
    }

    /// ブロックが1マス移動した(重力落下・アイテム効果等による着地)ことを記録する。
    /// `cell_kind`は移動後のセル内容(`format!("{:?}", cell)`、TERM独自拡張。ユーザー指摘:
    /// 「どういう種類のブロックがっていう情報...残ってないと思うけど大丈夫？」)。
    pub fn log_move(&self, frame: u64, to: (usize, usize), from: (usize, usize), cell_kind: &str) {
        let result = self.conn.prepare_cached(
            "INSERT INTO block_events (frame, kind, cell_kind, row, col, from_row, from_col) VALUES (?1, 'move', ?2, ?3, ?4, ?5, ?6)",
        ).and_then(|mut stmt| {
            stmt.execute(rusqlite::params![frame as i64, cell_kind, to.0 as i64, to.1 as i64, from.0 as i64, from.1 as i64])
        });
        let _ = result;
    }

    /// ブロックが消滅した(自動消滅・スター溶解・アイテム効果等)ことを記録する。
    /// `cell_kind`は消える直前のセル内容(TERM独自拡張)。
    pub fn log_vanish(&self, frame: u64, pos: (usize, usize), cell_kind: &str) {
        let result = self.conn.prepare_cached(
            "INSERT INTO block_events (frame, kind, cell_kind, row, col, from_row, from_col) VALUES (?1, 'vanish', ?2, ?3, ?4, NULL, NULL)",
        ).and_then(|mut stmt| {
            stmt.execute(rusqlite::params![frame as i64, cell_kind, pos.0 as i64, pos.1 as i64])
        });
        let _ = result;
    }

    /// 落下ブロック補間描画(`draw_falling_blocks`)が、着地先セルが盤面上で既に
    /// Emptyになっている(着地と同一tickで自動消滅した等)場面に遭遇したことを記録する
    /// (TERM独自拡張。#172の再発疑い調査用。ユーザー指摘: 「このやり取りが何回か
    /// 続いており解決できてないので...不足要素をロギングしよう」)。
    /// `resolved_kind`は`recently_vanished_kind`で補えた場合の種類(補えなければNone
    /// =描画を丸ごとスキップした)。`progress`は`block_fall_progress()`(0.0〜1.0)、
    /// `flash_remaining_ms`はこの時点で残っていた消滅フラッシュの残り時間(msの整数化。
    /// 対象自体が無ければNone)。
    #[allow(clippy::too_many_arguments)]
    pub fn log_render_fallback(
        &self,
        frame: u64,
        to: (usize, usize),
        from: (usize, usize),
        resolved_kind: Option<&str>,
        progress: f32,
        effective_tick_ms: u64,
        flash_remaining_ms: Option<u64>,
    ) {
        let result = self.conn.prepare_cached(
            "INSERT INTO render_fallback_events (frame, row, col, from_row, from_col, resolved_kind, fallback_used, progress, effective_tick_ms, flash_remaining_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        ).and_then(|mut stmt| {
            stmt.execute(rusqlite::params![
                frame as i64,
                to.0 as i64,
                to.1 as i64,
                from.0 as i64,
                from.1 as i64,
                resolved_kind,
                resolved_kind.is_some() as i64,
                progress as f64,
                effective_tick_ms as i64,
                flash_remaining_ms.map(|ms| ms as i64),
            ])
        });
        let _ = result;
    }

    /// プレイヤーの位置・向き・ステータスをフレームに1回記録する(TERM独自拡張。
    /// ユーザー指摘: 「キャラの向きや位置、ステータスって残ってないと思うけど
    /// 大丈夫？」)。ブロックイベントと違い多発しないため、フレームごと1行のみ。
    pub fn log_player_state(&self, frame: u64, row: usize, col: usize, facing: &str, status: &str) {
        let result = self
            .conn
            .prepare_cached("INSERT INTO player_state (frame, row, col, facing, status) VALUES (?1, ?2, ?3, ?4, ?5)")
            .and_then(|mut stmt| stmt.execute(rusqlite::params![frame as i64, row as i64, col as i64, facing, status]));
        let _ = result;
    }
}

impl Drop for DebugLog {
    fn drop(&mut self) {
        if self.in_transaction.get() {
            let _ = self.conn.execute_batch("COMMIT;");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_log_path(tag: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "misterdrillerterm-debug-log-test-{tag}-{}",
                std::process::id()
            ))
            .join(LOG_FILE_NAME)
    }

    fn count_rows(conn: &Connection, kind: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM block_events WHERE kind = ?1",
            [kind],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn log_move_and_log_vanish_insert_rows_with_the_given_frame_and_kind() {
        let path = temp_log_path("insert");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        let log = DebugLog::open_fresh_at(&path).expect("一時ディレクトリで開けるはず");
        log.log_move(10, (5, 1), (4, 1), "Color(Red)");
        log.log_vanish(11, (5, 1), "Rock { hits: 2 }");

        assert_eq!(count_rows(&log.conn, "move"), 1);
        assert_eq!(count_rows(&log.conn, "vanish"), 1);
        let (frame, cell_kind): (i64, String) = log
            .conn
            .query_row(
                "SELECT frame, cell_kind FROM block_events WHERE kind = 'move'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(frame, 10);
        assert_eq!(cell_kind, "Color(Red)");

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn log_render_fallback_records_resolved_kind_and_progress_when_fallback_succeeds() {
        let path = temp_log_path("render-fallback-hit");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        let log = DebugLog::open_fresh_at(&path).unwrap();
        log.log_render_fallback(42, (5, 1), (4, 1), Some("Color(Red)"), 0.75, 90, Some(120));

        let (resolved_kind, fallback_used, progress, effective_tick_ms, flash_remaining_ms): (
            Option<String>,
            i64,
            f64,
            i64,
            Option<i64>,
        ) = log
            .conn
            .query_row(
                "SELECT resolved_kind, fallback_used, progress, effective_tick_ms, flash_remaining_ms FROM render_fallback_events WHERE frame = 42",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(resolved_kind.as_deref(), Some("Color(Red)"));
        assert_eq!(fallback_used, 1);
        assert!((progress - 0.75).abs() < 1e-6);
        assert_eq!(effective_tick_ms, 90);
        assert_eq!(flash_remaining_ms, Some(120));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn log_render_fallback_records_none_when_the_fallback_could_not_resolve_a_kind() {
        let path = temp_log_path("render-fallback-miss");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        let log = DebugLog::open_fresh_at(&path).unwrap();
        log.log_render_fallback(7, (1, 1), (0, 1), None, 0.1, 150, None);

        let (resolved_kind, fallback_used, flash_remaining_ms): (Option<String>, i64, Option<i64>) = log
            .conn
            .query_row(
                "SELECT resolved_kind, fallback_used, flash_remaining_ms FROM render_fallback_events WHERE frame = 7",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(resolved_kind, None);
        assert_eq!(fallback_used, 0);
        assert_eq!(flash_remaining_ms, None);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn open_fresh_at_discards_previous_content() {
        // ユーザー指摘: 「タイトルからゲームスタートした時点でログdbは毎回リフレッシュ
        // するものとする」。
        let path = temp_log_path("refresh");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        let log = DebugLog::open_fresh_at(&path).unwrap();
        log.log_move(1, (0, 0), (0, 0), "Color(Red)");
        assert_eq!(count_rows(&log.conn, "move"), 1);
        drop(log);

        let log = DebugLog::open_fresh_at(&path).expect("既存ファイルがあっても作り直せるはず");
        assert_eq!(
            count_rows(&log.conn, "move"),
            0,
            "前回の内容は破棄されているはず"
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn log_player_state_records_position_facing_and_status_once_per_frame() {
        let path = temp_log_path("player-state");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        let log = DebugLog::open_fresh_at(&path).unwrap();
        log.log_player_state(3, 10, 2, "Down", "Playing");

        let (row, col, facing, status): (i64, i64, String, String) = log
            .conn
            .query_row(
                "SELECT row, col, facing, status FROM player_state WHERE frame = 3",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (row, col, facing.as_str(), status.as_str()),
            (10, 2, "Down", "Playing")
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn begin_frame_commits_the_previous_frames_pending_transaction() {
        // ユーザー指摘: 「あとちょっともっさりしてるからinsert高速化したい」。
        // 複数フレームぶん連続でbegin_frameを呼んでも、前フレームの書き込みが
        // 失われず全てコミットされていることを確認する(遅延コミット方式の検証)。
        let path = temp_log_path("begin-frame");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        let log = DebugLog::open_fresh_at(&path).unwrap();
        log.begin_frame();
        log.log_move(1, (0, 0), (0, 0), "Color(Red)");
        log.begin_frame();
        log.log_move(2, (0, 0), (0, 0), "Color(Red)");
        drop(log);

        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            count_rows(&conn, "move"),
            2,
            "前フレーム・現フレームどちらの書き込みも残っているはず"
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}

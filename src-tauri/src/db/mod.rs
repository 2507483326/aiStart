use std::path::Path;
use std::sync::{Mutex, OnceLock};

use rusqlite::{Connection, Transaction};

use crate::error::{AppError, AppResult};

/// 建表 DDL 的唯一来源（同目录 schema.sql，人工审核用）。
pub const SCHEMA_SQL: &str = include_str!("schema.sql");

/// 当前 schema 版本号，写入 schema_meta.db_schema_version。
const SCHEMA_VERSION: i64 = 9;

static DB: OnceLock<Mutex<Connection>> = OnceLock::new();

pub fn init(dir: &Path) -> AppResult<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("ai-start.db3");

    let connection = Connection::open(&path)?;
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;\nPRAGMA foreign_keys = OFF;\nPRAGMA busy_timeout = 5000;",
    )?;

    // CREATE TABLE IF NOT EXISTS 只建新表，不会给已存在的旧库补列；schema.sql 里的索引又引用了新列，
    // 所以必须在执行 DDL 之前对「已存在的表」补列（新库由 schema.sql 直接建出带列的表，这里跳过）。
    ensure_column(
        &connection,
        "usage_detail",
        "source_app",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        &connection,
        "usage_detail",
        "upstream_url",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        &connection,
        "usage_detail",
        "upstream_model",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        &connection,
        "usage_detail",
        "proxied",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        &connection,
        "app_model_bindings",
        "token",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(&connection, "usage_payload", "upstream_request", "TEXT")?;
    ensure_column(
        &connection,
        "usage_payload",
        "upstream_request_truncated",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    // v9：入站 HTTP header 原样入库（用户确认不脱敏）。
    ensure_column(&connection, "usage_payload", "inbound_headers", "TEXT")?;

    // v7：app_version_records 由「追加式历史」改为「每个应用一行」。旧表先改名让 schema.sql
    // 建出新结构，数据在 DDL 之后搬运（见 copy_legacy_app_version_records）。
    rename_legacy_app_version_records(&connection)?;

    connection.execute_batch(SCHEMA_SQL)?;

    copy_legacy_app_version_records(&connection)?;

    let now = now_ms();
    connection.execute(
        "INSERT INTO schema_meta (key, value, created_time, update_time) \
         VALUES ('db_schema_version', ?1, ?2, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, update_time = excluded.update_time",
        rusqlite::params![SCHEMA_VERSION.to_string(), now],
    )?;

    let _ = DB.set(Mutex::new(connection));
    Ok(())
}

/// 幂等补列：旧库缺列时执行 ALTER TABLE ADD COLUMN（SQLite 无 ADD COLUMN IF NOT EXISTS）。
/// 表尚不存在（全新库）时直接跳过——由 schema.sql 建出带该列的表。
fn ensure_column(connection: &Connection, table: &str, column: &str, decl: &str) -> AppResult<()> {
    if !table_exists(connection, table)? {
        return Ok(());
    }
    if !table_has_column(connection, table, column)? {
        connection.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
    }
    Ok(())
}

fn table_exists(connection: &Connection, table: &str) -> AppResult<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )?)
}

fn table_has_column(connection: &Connection, table: &str, column: &str) -> AppResult<bool> {
    if !table_exists(connection, table)? {
        return Ok(false);
    }
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(Result::ok)
        .any(|name| name == column);
    Ok(exists)
}

/// v7 迁移第一步：旧版 app_version_records（自增主键、追加式历史）改名为 legacy，
/// 让 schema.sql 建出「每个应用一行」的新表。
fn rename_legacy_app_version_records(connection: &Connection) -> AppResult<()> {
    if table_has_column(connection, "app_version_records", "app_version_record_id")? {
        connection
            .execute_batch("ALTER TABLE app_version_records RENAME TO app_version_records_legacy")?;
    }
    Ok(())
}

/// v7 迁移第二步：把每个应用最新的一条有效 check 搬进新表，然后丢弃旧表。
/// 安装/更新动作行不搬（events 表已留有审计），未确认的 unreachable 行也丢弃。
fn copy_legacy_app_version_records(connection: &Connection) -> AppResult<()> {
    if !table_exists(connection, "app_version_records_legacy")? {
        return Ok(());
    }
    connection.execute_batch(
        "INSERT OR IGNORE INTO app_version_records \
           (app_kind, action, installed_version, target_version, latest_version, update_available, \
            source_url, status, message, event_time, created_time, update_time) \
         SELECT app_kind, 'check', installed_version, target_version, latest_version, update_available, \
                source_url, status, message, event_time, created_time, update_time \
         FROM app_version_records_legacy r \
         WHERE action = 'check' AND status <> 'unreachable' \
           AND r.app_version_record_id = ( \
             SELECT r2.app_version_record_id FROM app_version_records_legacy r2 \
             WHERE r2.app_kind = r.app_kind AND r2.action = 'check' AND r2.status <> 'unreachable' \
             ORDER BY r2.event_time DESC, r2.app_version_record_id DESC LIMIT 1);\
         DROP TABLE app_version_records_legacy",
    )?;
    Ok(())
}

fn connection() -> AppResult<&'static Mutex<Connection>> {
    DB.get()
        .ok_or_else(|| AppError::Message("数据库尚未初始化".into()))
}

pub fn with_conn<T>(f: impl FnOnce(&Connection) -> AppResult<T>) -> AppResult<T> {
    let guard = connection()?.lock().expect("database lock poisoned");
    f(&guard)
}

pub fn with_tx<T>(f: impl FnOnce(&Transaction) -> AppResult<T>) -> AppResult<T> {
    let mut guard = connection()?.lock().expect("database lock poisoned");
    let transaction = guard.transaction()?;
    let value = f(&transaction)?;
    transaction.commit()?;
    Ok(value)
}

pub fn now_ms() -> i64 {
    chrono::Local::now().timestamp_millis()
}

pub fn iso_from_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|value| value.with_timezone(&chrono::Local).to_rfc3339())
        .unwrap_or_default()
}

pub fn ms_from_iso(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|value| value.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v6 之前的 app_version_records：自增主键，每次检查/动作追加一行。
    const LEGACY_DDL: &str = "CREATE TABLE app_version_records (\
        app_version_record_id INTEGER PRIMARY KEY AUTOINCREMENT,\
        app_kind TEXT NOT NULL, action TEXT NOT NULL, installed_version TEXT, target_version TEXT,\
        latest_version TEXT, update_available INTEGER NOT NULL DEFAULT 0, source_url TEXT,\
        status TEXT NOT NULL, message TEXT, event_time INTEGER NOT NULL, created_time INTEGER NOT NULL,\
        update_time INTEGER NOT NULL)";

    fn migrate(connection: &Connection) -> AppResult<()> {
        rename_legacy_app_version_records(connection)?;
        connection.execute_batch(SCHEMA_SQL)?;
        copy_legacy_app_version_records(connection)
    }

    #[test]
    fn collapses_legacy_records_to_one_row_per_app() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(LEGACY_DDL).unwrap();
        connection
            .execute_batch(
                "INSERT INTO app_version_records \
                   (app_kind, action, installed_version, latest_version, status, event_time, created_time, update_time) \
                 VALUES \
                   ('claude-desktop',   'check',  '1.0.0', '2.0.0', 'found',       100, 100, 100), \
                   ('claude-desktop',   'update', '1.0.0', NULL,    'launched',    200, 200, 200), \
                   ('claude-desktop',   'check',  '1.0.0', '2.1.0', 'found',       300, 300, 300), \
                   ('deepseek-desktop', 'check',  NULL,    NULL,    'unreachable', 150, 150, 150)",
            )
            .unwrap();

        migrate(&connection).unwrap();

        let rows: Vec<(String, String, Option<String>, String)> = connection
            .prepare("SELECT app_kind, action, latest_version, status FROM app_version_records ORDER BY app_kind")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .filter_map(Result::ok)
            .collect();

        // 每个应用最多一行；只有有效 check 的应用被迁移，动作行与 unreachable 行丢弃。
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "claude-desktop");
        assert_eq!(rows[0].1, "check");
        assert_eq!(rows[0].2.as_deref(), Some("2.1.0")); // 取最新的一条 check
        assert_eq!(rows[0].3, "found");
    }

    #[test]
    fn migration_is_a_no_op_on_a_fresh_or_already_migrated_database() {
        let fresh = Connection::open_in_memory().unwrap();
        migrate(&fresh).unwrap();
        migrate(&fresh).unwrap();

        let rows: i64 = fresh
            .query_row("SELECT COUNT(*) FROM app_version_records", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }
}

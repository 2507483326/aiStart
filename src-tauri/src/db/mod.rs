use std::path::Path;
use std::sync::{Mutex, OnceLock};

use rusqlite::{Connection, Transaction};

use crate::error::{AppError, AppResult};

/// 建表 DDL 的唯一来源（同目录 schema.sql，人工审核用）。
pub const SCHEMA_SQL: &str = include_str!("schema.sql");

/// 当前 schema 版本号，写入 schema_meta.db_schema_version。
const SCHEMA_VERSION: i64 = 3;

static DB: OnceLock<Mutex<Connection>> = OnceLock::new();

pub fn init(dir: &Path) -> AppResult<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("ai-start.db3");

    let connection = Connection::open(&path)?;
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;\nPRAGMA foreign_keys = OFF;\nPRAGMA busy_timeout = 5000;",
    )?;
    connection.execute_batch(SCHEMA_SQL)?;

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

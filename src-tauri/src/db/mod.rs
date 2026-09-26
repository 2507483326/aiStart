use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Mutex, OnceLock};

use rusqlite::Connection;

use crate::error::{AppError, AppResult};

/// 建表 DDL 的唯一来源（同目录 schema.sql，人工审核用）。
pub const SCHEMA_SQL: &str = include_str!("schema.sql");

/// 当前 schema 版本号，写入 schema_meta.db_schema_version。
const SCHEMA_VERSION: i64 = 11;

/// **读连接**：所有查询都走它（`with_conn`）。迁移跑完后置 `query_only = ON`，此后只能读。
/// 写全部交给下面的写线程——两个连接各司其职，WAL 下读不挡写。
static DB: OnceLock<Mutex<Connection>> = OnceLock::new();

/// 一次写任务：在写线程上执行的一段同步 SQLite 操作。拿到独占的写连接，自己开事务。
type WriteJob = Box<dyn FnOnce(&mut Connection) -> AppResult<()> + Send + 'static>;

enum Msg {
    Write(WriteJob),
    /// 屏障：写线程处理到它时回信，`flush()` 据此确认此前所有写都已落库。
    Flush(SyncSender<()>),
}

/// 写线程的投递端（`db::init` 建立）；所有写库都经过它。
static WRITER: OnceLock<SyncSender<Msg>> = OnceLock::new();

/// 写失败计数与最近一次错误：异步写没有调用方可以返回错误，只能记下来供面板/排查。
static WRITE_FAILURES: AtomicU64 = AtomicU64::new(0);
static LAST_WRITE_ERROR: OnceLock<Mutex<Option<String>>> = OnceLock::new();

/// 队列容量。正常使用永远碰不到——只有 DB 长时间跟不上请求速率、积压超过它时，
/// 投递才会按背压阻塞（「不丢数据」的必然代价）。
const WRITER_QUEUE: usize = 4096;

pub fn init(dir: &Path) -> AppResult<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("ai-start.db3");

    let connection = Connection::open(&path)?;
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;\nPRAGMA foreign_keys = OFF;\nPRAGMA busy_timeout = 5000;",
    )?;

    // CREATE TABLE IF NOT EXISTS 只建新表，不会给已存在的旧库补列；schema.sql 里的索引又引用了新列，
    // 所以必须在执行 DDL 之前对「已存在的表」补列（新库由 schema.sql 直接建出带列的表，这里跳过）。
    // 这些补列的结果不用：只为让旧库跟上当前列集合。
    let _ = ensure_column(
        &connection,
        "usage_detail",
        "source_app",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    let _ = ensure_column(
        &connection,
        "usage_detail",
        "upstream_url",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    let _ = ensure_column(
        &connection,
        "usage_detail",
        "upstream_model",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    let _ = ensure_column(
        &connection,
        "usage_detail",
        "proxied",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    let _ = ensure_column(
        &connection,
        "app_model_bindings",
        "token",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    let _ = ensure_column(&connection, "usage_payload", "upstream_request", "TEXT")?;
    let _ = ensure_column(
        &connection,
        "usage_payload",
        "upstream_request_truncated",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    // v9：入站 HTTP header 原样入库（用户确认不脱敏）。
    let _ = ensure_column(&connection, "usage_payload", "inbound_headers", "TEXT")?;

    // v10：每日汇总表补三列。只要有一列是这次新加的，就把历史回填一次——汇总查询此后只读这张表，
    // 不再依赖「每次重查 usage_detail」，所以旧库必须先补齐它漏掉的历史累计值。
    let added_cache_read = ensure_column(
        &connection,
        "usage_daily_total",
        "cache_read_tokens",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    let added_cache_write = ensure_column(
        &connection,
        "usage_daily_total",
        "cache_write_tokens",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    let added_failovers = ensure_column(
        &connection,
        "usage_daily_total",
        "failovers",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    if added_cache_read || added_cache_write || added_failovers {
        backfill_daily_totals(&connection)?;
    }

    // v7：app_version_records 由「追加式历史」改为「每个应用一行」。旧表先改名让 schema.sql
    // 建出新结构，数据在 DDL 之后搬运（见 copy_legacy_app_version_records）。
    rename_legacy_app_version_records(&connection)?;

    // v11：usage_daily_total 由「day 作主键」改为「自增主键的 usage_total」，每日行与全量行共用一张表。
    // 同样先改名让 schema.sql 建出新表，数据在 DDL 之后搬运（见 copy_legacy_daily_total）。
    rename_legacy_daily_total(&connection)?;

    connection.execute_batch(SCHEMA_SQL)?;

    copy_legacy_app_version_records(&connection)?;

    copy_legacy_daily_total(&connection)?;

    let now = now_ms();
    connection.execute(
        "INSERT INTO schema_meta (key, value, created_time, update_time) \
         VALUES ('db_schema_version', ?1, ?2, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, update_time = excluded.update_time",
        rusqlite::params![SCHEMA_VERSION.to_string(), now],
    )?;

    // 迁移到此结束。把这条连接降为只读：此后任何写都会在这里报错，写全部走下面的写线程。
    // 顺序很重要——迁移与回填都需要写权限，必须跑完再设 query_only。
    connection.execute_batch("PRAGMA query_only = ON")?;
    let _ = DB.set(Mutex::new(connection));

    // 写线程（只建一次）：独占另一条写连接，串行执行所有写任务。
    spawn_writer(&path)?;
    Ok(())
}

/// 建立专用写线程。只有第一次 `db::init` 真正建起来——重复 init 时 `WRITER` 已占用，
/// 新开的 channel 连同 receiver 一起丢弃，不会再起第二个写线程。
fn spawn_writer(path: &Path) -> AppResult<()> {
    let (sender, receiver) = std::sync::mpsc::sync_channel::<Msg>(WRITER_QUEUE);
    if WRITER.set(sender).is_err() {
        return Ok(());
    }

    let connection = Connection::open(path)?;
    connection.execute_batch(
        // WAL 下 synchronous=NORMAL 是通行做法：崩溃最多丢最后几条已提交记录，不损坏库，
        // 省掉每次 commit 的 fsync——落库延迟显著下降。
        "PRAGMA journal_mode = WAL;\nPRAGMA foreign_keys = OFF;\nPRAGMA busy_timeout = 5000;\nPRAGMA synchronous = NORMAL;",
    )?;

    std::thread::Builder::new()
        .name("ai-start-db-writer".into())
        .spawn(move || writer_loop(connection, receiver))
        .map_err(|error| AppError::Message(format!("启动数据库写线程失败: {error}")))?;
    Ok(())
}

/// 写线程主循环：串行执行写任务；`Flush` 任务回信（屏障）。投递端全部消失即退出。
fn writer_loop(mut connection: Connection, receiver: Receiver<Msg>) {
    while let Ok(msg) = receiver.recv() {
        match msg {
            Msg::Write(job) => {
                if let Err(error) = job(&mut connection) {
                    record_write_failure(&error.to_string());
                }
            }
            Msg::Flush(ack) => {
                let _ = ack.send(());
            }
        }
    }
}

/// 把一次写交给写线程。队列未满时**立即返回**，满了才阻塞（背压语义，不丢数据）。
/// 未初始化时静默丢弃——与旧实现「写失败只丢弃、不影响主流程」口径一致。
pub fn submit(job: impl FnOnce(&mut Connection) -> AppResult<()> + Send + 'static) {
    let Some(sender) = WRITER.get() else {
        return;
    };
    if sender.send(Msg::Write(Box::new(job))).is_err() {
        record_write_failure("数据库写线程已退出");
    }
}

/// 屏障：等写线程把此前投递的所有写任务落库后再返回。测试与退出前用。
pub fn flush() {
    let Some(sender) = WRITER.get() else {
        return;
    };
    // 1 容量 channel 做一次性回信；阻塞 send 保证屏障一定会排进队列。
    let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel(1);
    if sender.send(Msg::Flush(ack_tx)).is_ok() {
        let _ = ack_rx.recv();
    }
}

/// 写线程累计的失败次数（异步写没有返回值可传，只能这样暴露给面板/排查）。
pub fn write_failures() -> u64 {
    WRITE_FAILURES.load(Ordering::Relaxed)
}

/// 最近一次写失败的原因。
pub fn last_write_error() -> Option<String> {
    LAST_WRITE_ERROR
        .get_or_init(|| Mutex::new(None))
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
}

fn record_write_failure(message: &str) {
    WRITE_FAILURES.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut guard) = LAST_WRITE_ERROR.get_or_init(|| Mutex::new(None)).lock() {
        *guard = Some(message.to_string());
    }
}

/// 幂等补列：旧库缺列时执行 ALTER TABLE ADD COLUMN（SQLite 无 ADD COLUMN IF NOT EXISTS）。
/// 表尚不存在（全新库）时直接跳过——由 schema.sql 建出带该列的表。
/// 返回**这次是否真的补了列**：迁移需要据此判断要不要顺带回填历史数据（v10）。
fn ensure_column(connection: &Connection, table: &str, column: &str, decl: &str) -> AppResult<bool> {
    if !table_exists(connection, table)? {
        return Ok(false);
    }
    if !table_has_column(connection, table, column)? {
        connection.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
        return Ok(true);
    }
    Ok(false)
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

/// v11 迁移第一步：旧的 usage_daily_total（day 作主键）改名为 legacy，
/// 让 schema.sql 建出自增主键的新表 usage_total。
fn rename_legacy_daily_total(connection: &Connection) -> AppResult<()> {
    if table_exists(connection, "usage_daily_total")? {
        connection.execute_batch("ALTER TABLE usage_daily_total RENAME TO usage_daily_total_legacy")?;
    }
    Ok(())
}

/// v11 迁移第二步：把 legacy 的每日行搬进 usage_total，再补一行全量累计（day = ''），
/// 最后丢弃旧表。整体一个事务：中途失败则旧表还在，下次启动重跑，不会搬出重复行。
fn copy_legacy_daily_total(connection: &Connection) -> AppResult<()> {
    if !table_exists(connection, "usage_daily_total_legacy")? {
        return Ok(());
    }
    let now = now_ms();
    let transaction = connection.unchecked_transaction()?;
    transaction.execute_batch(
        "INSERT INTO usage_total \
           (day, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, total_tokens, \
            calls, failed_calls, failovers, created_time, update_time) \
         SELECT day, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, total_tokens, \
                calls, failed_calls, failovers, created_time, update_time \
         FROM usage_daily_total_legacy",
    )?;
    transaction.execute(
        "INSERT INTO usage_total \
           (day, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, total_tokens, \
            calls, failed_calls, failovers, created_time, update_time) \
         SELECT '', COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0), \
                COALESCE(SUM(cache_read_tokens), 0), COALESCE(SUM(cache_write_tokens), 0), \
                COALESCE(SUM(total_tokens), 0), COALESCE(SUM(calls), 0), \
                COALESCE(SUM(failed_calls), 0), COALESCE(SUM(failovers), 0), ?1, ?1 \
         FROM usage_daily_total_legacy",
        rusqlite::params![now],
    )?;
    transaction.execute_batch("DROP TABLE usage_daily_total_legacy")?;
    transaction.commit()?;
    Ok(())
}

/// v10 回填：把 usage_detail 的历史累计补进 usage_daily_total 的三个新列。
/// （v11 起该表改名为 usage_total，本回填必须在改名之前跑，见 db::init 的顺序。）
/// 只在补列那一次跑（一次性迁移）；`usage_detail(day)` 有索引，逐天子查询走索引。
fn backfill_daily_totals(connection: &Connection) -> AppResult<()> {
    connection.execute_batch(
        "UPDATE usage_daily_total SET \
           cache_read_tokens  = COALESCE((SELECT SUM(cache_read_tokens)  FROM usage_detail d WHERE d.day = usage_daily_total.day), 0), \
           cache_write_tokens = COALESCE((SELECT SUM(cache_write_tokens) FROM usage_detail d WHERE d.day = usage_daily_total.day), 0), \
           failovers          = COALESCE((SELECT SUM(CASE WHEN failover = 1 THEN 1 ELSE 0 END) FROM usage_detail d WHERE d.day = usage_daily_total.day), 0)",
    )?;
    Ok(())
}

fn connection() -> AppResult<&'static Mutex<Connection>> {
    DB.get()
        .ok_or_else(|| AppError::Message("数据库尚未初始化".into()))
}

/// 读：所有查询都走只读的读连接（`PRAGMA query_only = ON`）。
/// 写一律走 [`submit`]——在读连接上写会直接报错，这是刻意的（让漏迁的写点立刻暴露）。
pub fn with_conn<T>(f: impl FnOnce(&Connection) -> AppResult<T>) -> AppResult<T> {
    let guard = connection()?.lock().expect("database lock poisoned");
    f(&guard)
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
        rename_legacy_daily_total(connection)?;
        connection.execute_batch(SCHEMA_SQL)?;
        copy_legacy_app_version_records(connection)?;
        copy_legacy_daily_total(connection)
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

    /// v10：旧库的 usage_daily_total 没有缓存/切换三列，补列后要按 usage_detail 回填一次。
    #[test]
    fn v10_backfill_fills_daily_rollup_from_detail() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE usage_detail (day TEXT, cache_read_tokens INTEGER, cache_write_tokens INTEGER, failover INTEGER);\
                 CREATE TABLE usage_daily_total (\
                   day TEXT PRIMARY KEY, input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,\
                   total_tokens INTEGER NOT NULL DEFAULT 0, calls INTEGER NOT NULL DEFAULT 0, failed_calls INTEGER NOT NULL DEFAULT 0,\
                   created_time INTEGER NOT NULL DEFAULT 0, update_time INTEGER NOT NULL DEFAULT 0);\
                 INSERT INTO usage_detail (day, cache_read_tokens, cache_write_tokens, failover) VALUES \
                   ('2026-01-01', 10, 2, 1), ('2026-01-01', 5, 0, 0), ('2026-01-02', NULL, NULL, 1);\
                 INSERT INTO usage_daily_total (day) VALUES ('2026-01-01'), ('2026-01-02');",
            )
            .unwrap();

        // 三列都是这次新加的 → 应该触发回填。
        assert!(ensure_column(
            &connection,
            "usage_daily_total",
            "cache_read_tokens",
            "INTEGER NOT NULL DEFAULT 0"
        )
        .unwrap());
        assert!(ensure_column(
            &connection,
            "usage_daily_total",
            "cache_write_tokens",
            "INTEGER NOT NULL DEFAULT 0"
        )
        .unwrap());
        assert!(ensure_column(
            &connection,
            "usage_daily_total",
            "failovers",
            "INTEGER NOT NULL DEFAULT 0"
        )
        .unwrap());
        backfill_daily_totals(&connection).unwrap();

        let rollups: Vec<(String, i64, i64, i64)> = connection
            .prepare(
                "SELECT day, cache_read_tokens, cache_write_tokens, failovers FROM usage_daily_total ORDER BY day",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert_eq!(
            rollups,
            vec![
                ("2026-01-01".to_string(), 15, 2, 1),
                // NULL 缓存求和为 NULL → COALESCE 归 0。
                ("2026-01-02".to_string(), 0, 0, 1),
            ]
        );
    }

    /// v11：旧的 usage_daily_total（day 作主键）整表搬进 usage_total，并补一行全量累计（day = ''），
    /// 旧表随后丢弃；重复迁移不产生重复行。
    #[test]
    fn v11_moves_daily_rollup_into_usage_total_and_adds_grand_total() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE usage_daily_total (\
                   day TEXT PRIMARY KEY, input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,\
                   cache_read_tokens INTEGER NOT NULL DEFAULT 0, cache_write_tokens INTEGER NOT NULL DEFAULT 0,\
                   total_tokens INTEGER NOT NULL DEFAULT 0, calls INTEGER NOT NULL DEFAULT 0,\
                   failed_calls INTEGER NOT NULL DEFAULT 0, failovers INTEGER NOT NULL DEFAULT 0,\
                   created_time INTEGER NOT NULL DEFAULT 0, update_time INTEGER NOT NULL DEFAULT 0);\
                 INSERT INTO usage_daily_total \
                   (day, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, total_tokens, calls, failed_calls, failovers) \
                 VALUES ('2026-01-01', 100, 40, 10, 5, 155, 2, 1, 1),\
                        ('2026-01-02', 200, 80, 0, 0, 280, 1, 0, 0);",
            )
            .unwrap();

        migrate(&connection).unwrap();

        assert!(!table_exists(&connection, "usage_daily_total").unwrap());
        assert!(!table_exists(&connection, "usage_daily_total_legacy").unwrap());

        // 每日行原样搬过来（自增主键由新表分配）。
        let days: Vec<(String, i64, i64)> = connection
            .prepare("SELECT day, calls, total_tokens FROM usage_total WHERE day <> '' ORDER BY day")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert_eq!(
            days,
            vec![
                ("2026-01-01".to_string(), 2, 155),
                ("2026-01-02".to_string(), 1, 280),
            ]
        );

        // 全量行 = 每日行之和。
        let grand: (i64, i64, i64, i64, i64, i64) = connection
            .query_row(
                "SELECT calls, failed_calls, input_tokens, output_tokens, total_tokens, failovers \
                 FROM usage_total WHERE day = ''",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(grand, (3, 1, 300, 120, 435, 1));

        // 幂等：旧表已丢弃，再跑一次不新增行。
        migrate(&connection).unwrap();
        let rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM usage_total", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 3);
    }
}

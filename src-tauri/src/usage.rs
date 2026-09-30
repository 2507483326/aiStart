use std::time::Duration;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db;
use crate::error::AppResult;
use crate::settings;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecord {
    pub id: i64,
    pub timestamp: String,
    pub date: String,
    pub model_name: String,
    pub served_by: String,
    /// 来源应用：按请求 token 匹配到的 app_kind；未匹配则原样存该 token；空串=历史数据/未记录。
    #[serde(default)]
    pub source_app: String,
    /// 实际发往上游的接口地址（完整 URL，含路径）；未发起上游请求（如缺 Key、无启用模型）为空。
    #[serde(default)]
    pub upstream_url: String,
    /// 实际发往上游的模型 ID（wire model，与显示名 `served_by` 不同）；未发起上游请求为空。
    #[serde(default)]
    pub upstream_model: String,
    /// 这次请求是否经代理出站（按当时生效的「网络代理」开关 + 地址判定）；
    /// 未发起上游请求的失败（缺 Key、无启用模型）算直连；历史数据为 false。
    #[serde(default)]
    pub proxied: bool,
    pub inbound_protocol: String,
    pub upstream_protocol: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// 缓存读 / 缓存写 token（Anthropic 语义：input_tokens 不含缓存）；未上报为 None。
    #[serde(default)]
    pub cache_read_tokens: Option<u64>,
    #[serde(default)]
    pub cache_write_tokens: Option<u64>,
    /// 思考 token（不计入 total_tokens）；上游未上报为 None。
    #[serde(default)]
    pub reasoning_tokens: Option<u64>,
    pub duration_ms: u64,
    pub ok: bool,
    pub failover: bool,
    #[serde(default)]
    pub error: Option<String>,
}

impl UsageRecord {
    /// 真实消耗 token（口径对齐 cc-switch「真实消耗」）：输入 + 输出 + 缓存读 + 缓存写。
    /// 输入 / 输出各自不含缓存，只有总量把它们合并。
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            + self.output_tokens
            + self.cache_read_tokens.unwrap_or(0)
            + self.cache_write_tokens.unwrap_or(0)
    }
}

/// 写入侧报文：一次调用的入站请求体/HTTP header + 注入后发给上游的请求体 + 上游响应。
/// 报文按原样全量保存，写入侧不做任何截断。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsagePayload {
    pub inbound_request: Option<String>,
    /// 客户端入站 HTTP header（JSON 序列化的 `{ "名": "值" }`，原样保存不脱敏）。
    pub inbound_headers: Option<String>,
    pub upstream_request: Option<String>,
    pub upstream_response: Option<String>,
    pub stream: bool,
}

/// 读取侧报文详情（供「请求明细」详情页展示）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsagePayloadDetail {
    pub id: i64,
    pub time: String,
    pub inbound_request: Option<String>,
    pub inbound_headers: Option<String>,
    pub upstream_request: Option<String>,
    pub upstream_response: Option<String>,
    pub stream: bool,
}

/// 一次调用的完整详情：明细记录 +（可选的）报文快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestDetail {
    pub record: UsageRecord,
    pub payload: Option<UsagePayloadDetail>,
}

/// 分页结果：总条数 + 当前页记录（按行号倒序，最新在前）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsagePage {
    pub total: u64,
    pub items: Vec<UsageRecord>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyUsage {
    pub date: String,
    pub requests: u64,
    pub failed: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
}

/// 全量累计：`usage_total` 里 `day = ''` 的那一行。读 = 单行直读，不做 SUM。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTotals {
    pub requests: u64,
    pub failed: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub failovers: u64,
}

/// `usage_total` 里除 `day` 外的计数列（每日行与全量行共用同一组列）。
const USAGE_TOTAL_COLUMNS: &str = "calls, failed_calls, input_tokens, output_tokens, \
     cache_read_tokens, cache_write_tokens, total_tokens";

/// 全量累计行的 day 取值：空串。每日行用 'yyyy-MM-dd'，全表至多一行空串。
const GRAND_TOTAL_DAY: &str = "";

const SELECT_COLUMNS: &str = "usage_detail_id, day, event_time, model_name, served_by, inbound_protocol, \
     upstream_protocol, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, duration_ms, ok, failover, error, source_app, \
     upstream_url, upstream_model, reasoning_tokens, proxied";

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<UsageRecord> {
    let event_time: i64 = row.get(2)?;
    let ok: i64 = row.get(12)?;
    let failover: i64 = row.get(13)?;
    let proxied: i64 = row.get(19)?;
    Ok(UsageRecord {
        id: row.get(0)?,
        timestamp: db::iso_from_ms(event_time),
        date: row.get(1)?,
        model_name: row.get(3)?,
        served_by: row.get(4)?,
        inbound_protocol: row.get(5)?,
        upstream_protocol: row.get(6)?,
        input_tokens: row.get::<_, i64>(7)? as u64,
        output_tokens: row.get::<_, i64>(8)? as u64,
        cache_read_tokens: row.get::<_, Option<i64>>(9)?.map(|value| value as u64),
        cache_write_tokens: row.get::<_, Option<i64>>(10)?.map(|value| value as u64),
        reasoning_tokens: row.get::<_, Option<i64>>(18)?.map(|value| value as u64),
        duration_ms: row.get::<_, i64>(11)? as u64,
        ok: ok != 0,
        failover: failover != 0,
        error: row.get(14)?,
        source_app: row.get(15)?,
        upstream_url: row.get(16)?,
        upstream_model: row.get(17)?,
        proxied: proxied != 0,
    })
}

/// 投递一条调用记录（明细 + 每日汇总 + 可选报文）给写线程，单事务。
///
/// **非阻塞**（队列未满时立即返回）：网关请求结束只做一次投递，不再等这次 SQLite 事务
/// （含 commit / fsync）。写入失败由写线程记进 [`db::write_failures`] / [`db::last_write_error`]。
pub fn submit(entry: &UsageRecord, payload: Option<&UsagePayload>) {
    let entry = entry.clone();
    let payload = payload.cloned();
    // 保留条数在投递时取一次快照：写线程里不再回读设置，避免和写事务交错。
    let retention = settings::retention_count();
    db::submit(move |connection| {
        let transaction = connection.transaction()?;
        let event_time = db::ms_from_iso(&entry.timestamp).unwrap_or_else(db::now_ms);
        let total = entry.total_tokens() as i64;

        transaction.execute(
            "INSERT INTO usage_detail (day, event_time, model_name, served_by, inbound_protocol, upstream_protocol, \
             input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens, total_tokens, \
             duration_ms, ok, failover, error, source_app, upstream_url, upstream_model, proxied, created_time, update_time) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?21)",
            params![
                entry.date,
                event_time,
                entry.model_name,
                entry.served_by,
                entry.inbound_protocol,
                entry.upstream_protocol,
                entry.input_tokens as i64,
                entry.output_tokens as i64,
                entry.cache_read_tokens.map(|value| value as i64),
                entry.cache_write_tokens.map(|value| value as i64),
                entry.reasoning_tokens.map(|value| value as i64),
                total,
                entry.duration_ms as i64,
                i64::from(entry.ok),
                i64::from(entry.failover),
                entry.error,
                entry.source_app,
                entry.upstream_url,
                entry.upstream_model,
                i64::from(entry.proxied),
                event_time,
            ],
        )?;
        // 必须在 usage_total 的 upsert 之前取：每日行 / 全量行在当天首次写入时走 INSERT，
        // 会把 last_insert_rowid() 覆盖成汇总行的行号（走 UPDATE 分支则不会）。
        let detail_id = transaction.last_insert_rowid();
        upsert_usage_total(&transaction, &entry.date, &entry, event_time)?;
        upsert_usage_total(&transaction, GRAND_TOTAL_DAY, &entry, event_time)?;

        if let Some(payload) = &payload {
            transaction.execute(
                "INSERT INTO usage_payload (usage_detail_id, inbound_request, inbound_headers, upstream_request, upstream_response, \
                 is_stream, size_bytes, created_time, update_time) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                params![
                    detail_id,
                    payload.inbound_request.as_deref(),
                    payload.inbound_headers.as_deref(),
                    payload.upstream_request.as_deref(),
                    payload.upstream_response.as_deref(),
                    i64::from(payload.stream),
                    // 体积在写入时算好：清理要按它排序，现算得读完整张表。
                    payload_size_bytes(payload),
                    event_time,
                ],
            )?;
            // 写入即清理：同事务内把超出上限（条数 + 字节预算）的旧报文删掉，不依赖每日清扫。
            if retention > 0 {
                prune_payloads(&transaction, retention, PAYLOAD_BYTE_BUDGET)?;
            }
        }

        transaction.commit()?;
        Ok(())
    });
}

/// `usage_total` 的增量 upsert：`day` 传 'yyyy-MM-dd' 累加当天行，传 [`GRAND_TOTAL_DAY`] 累加全量行。
/// 累加全在写入侧完成——读取侧只按 day 取行，不做 SUM。
/// 本表按 schema 规范不建 UNIQUE，用不了 `ON CONFLICT`，所以先 UPDATE、没命中再 INSERT；
/// 写线程串行执行，不存在并发竞态。抽出来是为了能对内存库单测写入侧。
fn upsert_usage_total(
    connection: &rusqlite::Connection,
    day: &str,
    entry: &UsageRecord,
    event_time: i64,
) -> AppResult<()> {
    let input = entry.input_tokens as i64;
    let output = entry.output_tokens as i64;
    let cache_read = entry.cache_read_tokens.unwrap_or(0) as i64;
    let cache_write = entry.cache_write_tokens.unwrap_or(0) as i64;
    let total = entry.total_tokens() as i64;
    let failed = i64::from(!entry.ok);
    let failover = i64::from(entry.failover);

    let updated = connection.execute(
        "UPDATE usage_total SET \
           input_tokens = input_tokens + ?2, \
           output_tokens = output_tokens + ?3, \
           cache_read_tokens = cache_read_tokens + ?4, \
           cache_write_tokens = cache_write_tokens + ?5, \
           total_tokens = total_tokens + ?6, \
           calls = calls + 1, \
           failed_calls = failed_calls + ?7, \
           failovers = failovers + ?8, \
           update_time = ?9 \
         WHERE day = ?1",
        params![
            day, input, output, cache_read, cache_write, total, failed, failover, event_time
        ],
    )?;

    if updated == 0 {
        connection.execute(
            "INSERT INTO usage_total (day, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, \
             total_tokens, calls, failed_calls, failovers, created_time, update_time) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8, ?9, ?9)",
            params![
                day, input, output, cache_read, cache_write, total, failed, failover, event_time
            ],
        )?;
    }
    Ok(())
}

/// 按明细行号取报文详情；无报文记录（老数据或已被保留策略清理）返回 None。
pub fn payload_detail(usage_detail_id: i64) -> Option<UsagePayloadDetail> {
    db::with_conn(|connection| {
        let mut statement = connection.prepare(
            "SELECT inbound_request, inbound_headers, upstream_request, upstream_response, is_stream, created_time FROM usage_payload \
             WHERE usage_detail_id = ?1 ORDER BY usage_payload_id DESC LIMIT 1",
        )?;
        let mut rows = statement.query_map(params![usage_detail_id], |row| {
            let created_time: i64 = row.get(5)?;
            Ok(UsagePayloadDetail {
                id: usage_detail_id,
                time: db::iso_from_ms(created_time),
                inbound_request: row.get(0)?,
                inbound_headers: row.get(1)?,
                upstream_request: row.get(2)?,
                upstream_response: row.get(3)?,
                stream: row.get::<_, i64>(4)? != 0,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    })
    .unwrap_or(None)
}

/// 报文快照的总字节预算——「请求保存数量」之外的第二道上限。
///
/// 只按条数限不够：v12 起报文按原样全量保存，而入站报文的上限是 32MB（`gateway::MAX_INBOUND_BODY`），
/// 一条 Codex 长会话就是十几 MB，500 条能把库顶到几个 GB（实测 500 条 = 1.45GB，库文件 2.7GB）。
/// 超出预算时**最旧的先删**：最近那批仍然全量保真，历史窗口按体积自动缩短。典型的小报文
/// （几十 KB）加起来远不到这个数，所以平时条数上限说了算、预算不介入。
const PAYLOAD_BYTE_BUDGET: i64 = 512 * 1024 * 1024;

/// 一行报文的字节数。写入时按与 `db::PAYLOAD_SIZE_SQL` 同一口径算好存进 `size_bytes`，清理时只读
/// 那一列——按四列现算得读完整张表（实测 0.9s，库里有 500MB 报文），而清理挂在每一次写入之后。
fn payload_size_bytes(payload: &UsagePayload) -> i64 {
    let len = |value: &Option<String>| value.as_ref().map_or(0, |text| text.len() as i64);
    len(&payload.inbound_request)
        + len(&payload.inbound_headers)
        + len(&payload.upstream_request)
        + len(&payload.upstream_response)
}

/// 按「请求保存数量」清理超额报文快照：只保留最新的 `retention_count` 条（且总量不超过
/// [`PAYLOAD_BYTE_BUDGET`]），更早的删除。usage_detail 明细与 usage_total 汇总保留；
/// `retention_count <= 0` 不动任何行。
/// 走写线程（读连接是 `query_only`，删除只能交给写线程）；异步投递，行数不再返回。
/// 写入侧每条报文落库时也会即时清理（见 [`submit`]），这里主要用于「用户调小上限后立即生效」
/// 与「把删出来的空洞真正还给文件系统」（见 [`db::compact`]）。
pub fn cleanup_excess_payloads(retention_count: i64) {
    if retention_count <= 0 {
        return;
    }
    db::submit(move |connection| {
        prune_payloads(connection, retention_count, PAYLOAD_BYTE_BUDGET)?;
        db::compact(connection);
        Ok(())
    });
}

/// 只保留最新的 `keep` 条、且总字节不超过 `budget` 的报文行，删除其余。
/// 抽出两条上限是为了能用内存库单测，不必碰进程级 usage 库。
/// 条数用「第 keep+1 新的行号」作水位线一次删干净，避免逐条构造 NOT IN 列表；
/// 字节用窗口函数在 `size_bytes` 上从最新往回累加（只读这一列，几十行整数，微秒级），
/// 一次算出「还在预算内的最旧行号」。
/// 现有条数不足（或最新一条本身就超预算）时子查询为 NULL，`< NULL` 不命中任何行，等于不删
/// ——最新那条永远留着，否则会陷入「刚写就删」。
fn prune_payloads(connection: &rusqlite::Connection, keep: i64, budget: i64) -> AppResult<usize> {
    let by_count = connection.execute(
        "DELETE FROM usage_payload WHERE usage_payload_id <= ( \
             SELECT usage_payload_id FROM usage_payload ORDER BY usage_payload_id DESC LIMIT 1 OFFSET ?1 \
         )",
        params![keep],
    )?;
    let by_bytes = connection.execute(
        "DELETE FROM usage_payload WHERE usage_payload_id < ( \
             SELECT MIN(usage_payload_id) FROM ( \
                 SELECT usage_payload_id, \
                        SUM(size_bytes) OVER ( \
                            ORDER BY usage_payload_id DESC ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW \
                        ) AS running_bytes \
                 FROM usage_payload \
             ) WHERE running_bytes <= ?1 \
         )",
        params![budget],
    )?;
    Ok(by_count + by_bytes)
}

/// 保存窗口清理的节奏：启动后先等一会儿，之后每 24 小时一轮。
const RETENTION_SWEEP_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// 启动后到第一轮清理之间的等待。这一轮可能整库重写（见 [`db::compact`]），几秒的重 I/O
/// 不该和窗口加载、启动后的头几个请求抢——实测卡在启动路径上时，启动后第一个请求要 10s。
const FIRST_SWEEP_DELAY: Duration = Duration::from_secs(30);

/// 每天按当前设置清理一次过期报文。用独立线程而不是 tokio 任务：落库本身是同步的，
/// 清理一天才一次，不值得占用异步运行时；线程在进程退出时随之结束。
pub fn spawn_retention_task() {
    let _ = std::thread::Builder::new()
        .name("usage-retention".into())
        .spawn(|| {
            std::thread::sleep(FIRST_SWEEP_DELAY);
            loop {
                cleanup_excess_payloads(settings::snapshot().request_retention_count);
                std::thread::sleep(RETENTION_SWEEP_INTERVAL);
            }
        });
}

/// 按明细行号取单条记录（详情页深链/刷新用）。
pub fn find(usage_detail_id: i64) -> Option<UsageRecord> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM usage_detail WHERE usage_detail_id = ?1");
    db::with_conn(|connection| {
        let mut statement = connection.prepare(&sql)?;
        let mut rows = statement.query_map(params![usage_detail_id], row_to_record)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    })
    .unwrap_or(None)
}

pub fn recent(limit: usize) -> Vec<UsageRecord> {
    let sql =
        format!("SELECT {SELECT_COLUMNS} FROM usage_detail ORDER BY usage_detail_id DESC LIMIT ?1");
    db::with_conn(|connection| {
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params![limit as i64], row_to_record)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    })
    .unwrap_or_default()
}

/// 分页取全部明细（按行号倒序，最新在前）。查询失败返回空页。
pub fn page(offset: usize, limit: usize) -> UsagePage {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM usage_detail ORDER BY usage_detail_id DESC LIMIT ?1 OFFSET ?2"
    );
    db::with_conn(|connection| {
        let total: i64 =
            connection.query_row("SELECT COUNT(*) FROM usage_detail", [], |row| row.get(0))?;
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params![limit as i64, offset as i64], row_to_record)?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(UsagePage {
            total: total as u64,
            items,
        })
    })
    .unwrap_or(UsagePage {
        total: 0,
        items: Vec::new(),
    })
}

/// 计数列取自当前行（`day` 已占第 0 列）。每日行与全量行共用这套列。
fn row_to_daily(row: &rusqlite::Row<'_>) -> rusqlite::Result<DailyUsage> {
    Ok(DailyUsage {
        date: row.get(0)?,
        requests: row.get::<_, i64>(1)? as u64,
        failed: row.get::<_, i64>(2)? as u64,
        input_tokens: row.get::<_, i64>(3)? as u64,
        output_tokens: row.get::<_, i64>(4)? as u64,
        cache_read_tokens: row.get::<_, i64>(5)? as u64,
        cache_write_tokens: row.get::<_, i64>(6)? as u64,
        total_tokens: row.get::<_, i64>(7)? as u64,
    })
}

/// 读每日汇总行（`day <> ''` 且 `day >= cutoff`，按天升序）。直读行本身，不做 SUM。
/// 抽出来是为了能对内存库单测，不必碰进程级 usage 库。
fn read_daily_usage(connection: &rusqlite::Connection, cutoff: &str) -> AppResult<Vec<DailyUsage>> {
    let sql = format!(
        "SELECT day, {USAGE_TOTAL_COLUMNS} FROM usage_total \
         WHERE day <> '{GRAND_TOTAL_DAY}' AND day >= ?1 ORDER BY day"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params![cutoff], row_to_daily)?;
    let mut collected = Vec::new();
    for row in rows {
        collected.push(row?);
    }
    Ok(collected)
}

/// 天数换算成每日行的起始日（含当天）：`days = 1` 就是今天。
fn daily_cutoff(days: u32) -> String {
    let today = chrono::Local::now().date_naive();
    (today - chrono::Duration::days(i64::from(days.saturating_sub(1))))
        .format("%Y-%m-%d")
        .to_string()
}

/// 每日汇总（直读 `usage_total` 的每日行区间，不做 SUM）。供热力图与连续活跃。
pub fn daily(days: u32) -> Vec<DailyUsage> {
    let cutoff = daily_cutoff(days);
    db::with_conn(|connection| read_daily_usage(connection, &cutoff)).unwrap_or_default()
}

/// 今日一天（直读 `usage_total` 的当日行）。当天还没有记录时返回零值行，便于直接渲染。
pub fn today() -> DailyUsage {
    let date = daily_cutoff(1);
    let sql = format!("SELECT day, {USAGE_TOTAL_COLUMNS} FROM usage_total WHERE day = ?1");
    let found = db::with_conn(|connection| {
        Ok(connection
            .query_row(&sql, params![&date], row_to_daily)
            .optional()?)
    })
    .ok()
    .flatten();
    found.unwrap_or(DailyUsage {
        date,
        ..Default::default()
    })
}

/// 全量累计（直读 `usage_total` 里 `day = ''` 的那一行，不做 SUM）。
/// 供网关启动 [`crate::gateway::hydrate`] 与统计页「总」牌使用。
pub fn totals() -> UsageTotals {
    let sql = format!(
        "SELECT {USAGE_TOTAL_COLUMNS}, failovers FROM usage_total WHERE day = '{GRAND_TOTAL_DAY}'"
    );
    db::with_conn(|connection| {
        Ok(connection
            .query_row(&sql, [], |row| {
                Ok(UsageTotals {
                    requests: row.get::<_, i64>(0)? as u64,
                    failed: row.get::<_, i64>(1)? as u64,
                    input_tokens: row.get::<_, i64>(2)? as u64,
                    output_tokens: row.get::<_, i64>(3)? as u64,
                    cache_read_tokens: row.get::<_, i64>(4)? as u64,
                    cache_write_tokens: row.get::<_, i64>(5)? as u64,
                    total_tokens: row.get::<_, i64>(6)? as u64,
                    failovers: row.get::<_, i64>(7)? as u64,
                })
            })
            .optional()?
            .unwrap_or_default())
    })
    .unwrap_or_default()
}

pub fn current_timestamp() -> (String, String) {
    let now = chrono::Local::now();
    (now.to_rfc3339(), now.format("%Y-%m-%d").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// 现存报文的行号（升序），断言清理结果用。
    fn payload_ids(connection: &Connection) -> Vec<i64> {
        connection
            .prepare("SELECT usage_detail_id FROM usage_payload ORDER BY usage_payload_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect()
    }

    #[test]
    fn pruning_keeps_only_the_newest_rows() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(db::SCHEMA_SQL).unwrap();
        for detail_id in [10_i64, 20, 30] {
            connection
                .execute(
                    "INSERT INTO usage_payload (usage_detail_id, created_time, update_time) \
                     VALUES (?1, 100, 100)",
                    params![detail_id],
                )
                .unwrap();
        }

        // 上限 2 条：最旧的一条删除，保留最新的两条（这些行没有报文，字节预算不介入）。
        assert_eq!(
            prune_payloads(&connection, 2, PAYLOAD_BYTE_BUDGET).unwrap(),
            1
        );
        assert_eq!(payload_ids(&connection), vec![20, 30]);

        // 上限不小于现有条数：一行都不删。
        assert_eq!(
            prune_payloads(&connection, 3, PAYLOAD_BYTE_BUDGET).unwrap(),
            0
        );
    }

    /// 字节预算：条数没超但总量超了 —— 从最旧的开始删，最近的那批仍然全量保真。
    /// 预算是「条数上限」之外的第二道闸，专门对付「一条报文十几 MB」把库顶到几个 GB 的情况。
    #[test]
    fn pruning_drops_the_oldest_payloads_when_the_byte_budget_is_exceeded() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(db::SCHEMA_SQL).unwrap();
        // 每行 100 字节，共 5 行 = 500 字节。
        for detail_id in 1_i64..=5 {
            connection
                .execute(
                    "INSERT INTO usage_payload (usage_detail_id, inbound_request, size_bytes, created_time, update_time) \
                     VALUES (?1, ?2, 100, 100, 100)",
                    params![detail_id, "x".repeat(100)],
                )
                .unwrap();
        }

        // 预算 250 字节 = 最新的两行；第 3 行起累计已超 → 删掉最旧的三行。
        assert_eq!(prune_payloads(&connection, 500, 250).unwrap(), 3);
        assert_eq!(payload_ids(&connection), vec![4, 5]);

        // 预算连最新一条都放不下：一行都不删（否则会陷入「刚写就删」）。
        assert_eq!(prune_payloads(&connection, 500, 10).unwrap(), 0);
        assert_eq!(payload_ids(&connection), vec![4, 5]);
    }

    /// v11：usage_total 由写入侧增量累加（每日行 + 全量行共用一张表），读取侧只按 day 取行。
    /// 用内存库，不碰进程级 usage 库。
    #[test]
    fn usage_total_rollup_tracks_day_and_grand_total_rows() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(db::SCHEMA_SQL).unwrap();

        let entry = |ok: bool, failover: bool, cache_read: u64, cache_write: u64| UsageRecord {
            id: 0,
            timestamp: String::new(),
            date: "2026-01-01".into(),
            model_name: "M".into(),
            served_by: "M".into(),
            source_app: String::new(),
            upstream_url: String::new(),
            upstream_model: String::new(),
            proxied: false,
            inbound_protocol: "anthropic-messages".into(),
            upstream_protocol: "openai-responses".into(),
            input_tokens: 100,
            output_tokens: 40,
            cache_read_tokens: (cache_read > 0).then_some(cache_read),
            cache_write_tokens: (cache_write > 0).then_some(cache_write),
            reasoning_tokens: None,
            duration_ms: 1,
            ok,
            failover,
            error: None,
        };

        // 每次写入都同时落到「当日行」和「全量行」——与 submit 里的双写一致。
        for (entry, event_time) in [
            (entry(false, true, 11, 7), 111),
            (entry(true, false, 0, 0), 222),
        ] {
            upsert_usage_total(&connection, &entry.date, &entry, event_time).unwrap();
            upsert_usage_total(&connection, GRAND_TOTAL_DAY, &entry, event_time).unwrap();
        }

        let rows = read_daily_usage(&connection, "2026-01-01").unwrap();
        assert_eq!(rows.len(), 1, "全量行（day = ''）不能混进每日行");
        let row = &rows[0];
        assert_eq!(row.date, "2026-01-01");
        assert_eq!(row.requests, 2);
        assert_eq!(row.failed, 1, "ok=false 那条计入失败");
        assert_eq!(row.input_tokens, 200);
        assert_eq!(row.output_tokens, 80);
        assert_eq!(row.cache_read_tokens, 11);
        assert_eq!(row.cache_write_tokens, 7);
        assert_eq!(
            row.total_tokens,
            2 * (100 + 40) + 11 + 7,
            "total 累加 input+output+缓存（缺省按 0）"
        );

        // 全量行 = 同一批写入的累计。
        let grand: (i64, i64, i64, i64, i64, i64) = connection
            .query_row(
                &format!(
                    "SELECT calls, failed_calls, input_tokens, output_tokens, total_tokens, failovers \
                     FROM usage_total WHERE day = '{GRAND_TOTAL_DAY}'"
                ),
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
        assert_eq!(grand, (2, 1, 200, 80, 298, 1));

        // 截止日过滤：更晚的 cutoff 排掉它。
        assert!(read_daily_usage(&connection, "2026-01-02").unwrap().is_empty());
    }
}

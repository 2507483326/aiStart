use std::collections::BTreeMap;
use std::time::Duration;

use rusqlite::params;
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
/// 截断与标记由 `record_with_payload` 统一处理。
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
    pub request_truncated: bool,
    pub upstream_request_truncated: bool,
    pub response_truncated: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub model_name: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub total_requests: u64,
    pub failed_requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub today_tokens: u64,
    pub streak_days: u64,
    pub daily: Vec<DailyUsage>,
    pub by_model: Vec<ModelUsage>,
}

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

/// 单条报文保存上限：流式大响应可能极大，超过即截断并置 truncated 标记，避免撑爆本地库。
/// 网关也读它（`gateway::server::inbound_request_text`）：入站报文只按这个上限取前缀，
/// 不再为落库整份复制一遍——上限改了，两边一起跟着走。
pub(crate) const PAYLOAD_MAX_BYTES: usize = 256 * 1024;

/// 按 UTF-8 字节截断文本（不切坏多字节字符），返回 (截断后文本, 是否发生截断)。
fn cap_bytes(text: &str, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text.to_string(), false);
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

/// 明细 INSERT + 每日总和增量 upsert +（可选）报文 INSERT，单事务。
/// 写入失败只丢弃记录，不影响调用方。
pub fn record_with_payload(entry: &UsageRecord, payload: Option<&UsagePayload>) {
    let event_time = db::ms_from_iso(&entry.timestamp).unwrap_or_else(db::now_ms);
    let total = entry.total_tokens() as i64;
    let failed = i64::from(!entry.ok);

    let _ = db::with_tx(|transaction| {
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
        // 必须在 usage_daily_total 的 upsert 之前取：该 upsert 在当天首次插入时会新建行，
        // 从而把 last_insert_rowid() 覆盖成 usage_daily_total 的行号（走 UPDATE 分支则不会）。
        let detail_id = transaction.last_insert_rowid();

        transaction.execute(
            "INSERT INTO usage_daily_total (day, input_tokens, output_tokens, total_tokens, calls, failed_calls, created_time, update_time) \
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?6) \
             ON CONFLICT(day) DO UPDATE SET \
               input_tokens = input_tokens + excluded.input_tokens, \
               output_tokens = output_tokens + excluded.output_tokens, \
               total_tokens = total_tokens + excluded.total_tokens, \
               calls = calls + 1, \
               failed_calls = failed_calls + excluded.failed_calls, \
               update_time = excluded.update_time",
            params![
                entry.date,
                entry.input_tokens as i64,
                entry.output_tokens as i64,
                total,
                failed,
                event_time,
            ],
        )?;

        let Some(payload) = payload else {
            return Ok(());
        };

        let (inbound_request, request_truncated) = cap_optional(payload.inbound_request.as_deref());
        // header 通常很小，同一上限截断即可，不单设 truncated 标记列。
        let (inbound_headers, _) = cap_optional(payload.inbound_headers.as_deref());
        let (upstream_request, upstream_request_truncated) =
            cap_optional(payload.upstream_request.as_deref());
        let (upstream_response, response_truncated) =
            cap_optional(payload.upstream_response.as_deref());

        transaction.execute(
            "INSERT INTO usage_payload (usage_detail_id, inbound_request, inbound_headers, upstream_request, upstream_response, \
             request_truncated, upstream_request_truncated, response_truncated, is_stream, created_time, update_time) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
            params![
                detail_id,
                inbound_request,
                inbound_headers,
                upstream_request,
                upstream_response,
                i64::from(request_truncated),
                i64::from(upstream_request_truncated),
                i64::from(response_truncated),
                i64::from(payload.stream),
                event_time,
            ],
        )?;

        Ok(())
    });
}

fn cap_optional(text: Option<&str>) -> (Option<String>, bool) {
    match text {
        Some(text) => {
            let (capped, truncated) = cap_bytes(text, PAYLOAD_MAX_BYTES);
            (Some(capped), truncated)
        }
        None => (None, false),
    }
}

/// 按明细行号取报文详情；无报文记录（老数据或已被保留策略清理）返回 None。
pub fn payload_detail(usage_detail_id: i64) -> Option<UsagePayloadDetail> {
    db::with_conn(|connection| {
        let mut statement = connection.prepare(
            "SELECT inbound_request, inbound_headers, upstream_request, upstream_response, request_truncated, \
             upstream_request_truncated, response_truncated, is_stream, created_time FROM usage_payload \
             WHERE usage_detail_id = ?1 ORDER BY usage_payload_id DESC LIMIT 1",
        )?;
        let mut rows = statement.query_map(params![usage_detail_id], |row| {
            let created_time: i64 = row.get(8)?;
            Ok(UsagePayloadDetail {
                id: usage_detail_id,
                time: db::iso_from_ms(created_time),
                inbound_request: row.get(0)?,
                inbound_headers: row.get(1)?,
                upstream_request: row.get(2)?,
                upstream_response: row.get(3)?,
                request_truncated: row.get::<_, i64>(4)? != 0,
                upstream_request_truncated: row.get::<_, i64>(5)? != 0,
                response_truncated: row.get::<_, i64>(6)? != 0,
                stream: row.get::<_, i64>(7)? != 0,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    })
    .unwrap_or(None)
}

/// 按「请求保存时间」清理过期报文快照。只删 usage_payload（入站/上游请求与响应），
/// usage_detail 明细与 usage_daily_total 汇总保留；`retention_days <= 0`（永久保留）不动任何行。
/// 返回删除行数，失败按 0 计（清理失败不该影响调用方）。
pub fn cleanup_expired_payloads(retention_days: i64) -> usize {
    let Some(cutoff) = retention_cutoff(retention_days, db::now_ms()) else {
        return 0;
    };
    db::with_conn(|connection| prune_payloads(connection, cutoff)).unwrap_or(0)
}

/// 保留天数换算成「早于它即过期」的截止时刻；`retention_days <= 0`（永久保留）返回 None。
fn retention_cutoff(retention_days: i64, now_ms: i64) -> Option<i64> {
    if retention_days <= 0 {
        return None;
    }
    Some(now_ms - retention_days * 24 * 60 * 60 * 1000)
}

/// 删除早于 `cutoff` 的报文行。抽出来是为了能用内存库单测，不必碰进程级 usage 库。
fn prune_payloads(connection: &rusqlite::Connection, cutoff: i64) -> AppResult<usize> {
    Ok(connection.execute(
        "DELETE FROM usage_payload WHERE created_time < ?1",
        params![cutoff],
    )?)
}

/// 保存窗口清理的节奏：启动先跑一次，之后每 24 小时一轮。
const RETENTION_SWEEP_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// 每天按当前设置清理一次过期报文。用独立线程而不是 tokio 任务：落库本身是同步的，
/// 清理一天才一次，不值得占用异步运行时；线程在进程退出时随之结束。
pub fn spawn_retention_task() {
    let _ = std::thread::Builder::new()
        .name("usage-retention".into())
        .spawn(|| loop {
            cleanup_expired_payloads(settings::snapshot().request_retention_days);
            std::thread::sleep(RETENTION_SWEEP_INTERVAL);
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

fn read_since(cutoff: &str) -> Vec<UsageRecord> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM usage_detail WHERE day >= ?1 ORDER BY usage_detail_id"
    );
    db::with_conn(|connection| {
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params![cutoff], row_to_record)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    })
    .unwrap_or_default()
}

pub fn summary(days: u32) -> UsageSummary {
    let today = chrono::Local::now().date_naive();
    let cutoff = today - chrono::Duration::days(i64::from(days.saturating_sub(1)));
    let records = read_since(&cutoff.format("%Y-%m-%d").to_string());

    let mut daily: BTreeMap<String, DailyUsage> = BTreeMap::new();
    let mut by_model: BTreeMap<String, ModelUsage> = BTreeMap::new();
    let mut summary = UsageSummary {
        total_requests: 0,
        failed_requests: 0,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        total_tokens: 0,
        today_tokens: 0,
        streak_days: 0,
        daily: Vec::new(),
        by_model: Vec::new(),
    };

    for entry in records {
        let Ok(date) = chrono::NaiveDate::parse_from_str(&entry.date, "%Y-%m-%d") else {
            continue;
        };
        if date < cutoff {
            continue;
        }

        summary.total_requests += 1;
        if !entry.ok {
            summary.failed_requests += 1;
        }
        summary.input_tokens += entry.input_tokens;
        summary.output_tokens += entry.output_tokens;
        summary.cache_read_tokens += entry.cache_read_tokens.unwrap_or(0);
        summary.cache_write_tokens += entry.cache_write_tokens.unwrap_or(0);
        if date == today {
            summary.today_tokens += entry.total_tokens();
        }

        let bucket = daily
            .entry(entry.date.clone())
            .or_insert_with(|| DailyUsage {
                date: entry.date.clone(),
                requests: 0,
                failed: 0,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                total_tokens: 0,
            });
        bucket.requests += 1;
        if !entry.ok {
            bucket.failed += 1;
        }
        bucket.input_tokens += entry.input_tokens;
        bucket.output_tokens += entry.output_tokens;
        bucket.cache_read_tokens += entry.cache_read_tokens.unwrap_or(0);
        bucket.cache_write_tokens += entry.cache_write_tokens.unwrap_or(0);
        bucket.total_tokens += entry.total_tokens();

        let model = by_model
            .entry(entry.served_by.clone())
            .or_insert_with(|| ModelUsage {
                model_name: entry.served_by.clone(),
                requests: 0,
                input_tokens: 0,
                output_tokens: 0,
            });
        model.requests += 1;
        model.input_tokens += entry.input_tokens;
        model.output_tokens += entry.output_tokens;
    }

    summary.total_tokens = summary.input_tokens
        + summary.output_tokens
        + summary.cache_read_tokens
        + summary.cache_write_tokens;
    summary.daily = daily.into_values().collect();
    summary.by_model = by_model.into_values().collect();
    summary.by_model.sort_by(|a, b| b.requests.cmp(&a.requests));

    let active: std::collections::BTreeSet<&str> = summary
        .daily
        .iter()
        .filter(|day| day.requests > 0)
        .map(|day| day.date.as_str())
        .collect();
    let mut streak = 0;
    let mut cursor = today;
    while active.contains(cursor.format("%Y-%m-%d").to_string().as_str()) {
        streak += 1;
        cursor -= chrono::Duration::days(1);
    }
    summary.streak_days = streak;

    summary
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UsageTotals {
    pub requests: u64,
    pub failed: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub failovers: u64,
}

/// 全量累计口径（不限日期），供网关面板等需要跨重启保留的累计值使用。
pub fn totals() -> UsageTotals {
    db::with_conn(|connection| {
        let totals = connection.query_row(
            "SELECT COUNT(*), \
                    COALESCE(SUM(CASE WHEN ok = 0 THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(input_tokens), 0), \
                    COALESCE(SUM(output_tokens), 0), \
                    COALESCE(SUM(CASE WHEN failover = 1 THEN 1 ELSE 0 END), 0) \
             FROM usage_detail",
            [],
            |row| {
                Ok(UsageTotals {
                    requests: row.get::<_, i64>(0)? as u64,
                    failed: row.get::<_, i64>(1)? as u64,
                    input_tokens: row.get::<_, i64>(2)? as u64,
                    output_tokens: row.get::<_, i64>(3)? as u64,
                    failovers: row.get::<_, i64>(4)? as u64,
                })
            },
        )?;
        Ok(totals)
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

    const DAY_MS: i64 = 24 * 60 * 60 * 1000;

    #[test]
    fn retention_cutoff_is_permanent_only_when_disabled() {
        // 0 / 负数 = 永久保留：没有截止时刻，也就是不清理。
        assert_eq!(retention_cutoff(0, 1_000), None);
        assert_eq!(retention_cutoff(-30, 1_000), None);
        // 正数 = 现在往前推 N 天；边界取「恰好 N 天前」，更早的才算过期。
        assert_eq!(retention_cutoff(7, 1_000), Some(1_000 - 7 * DAY_MS));
    }

    #[test]
    fn pruning_removes_only_rows_older_than_the_cutoff() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(db::SCHEMA_SQL).unwrap();
        for created in [100_i64, 200, 300] {
            connection
                .execute(
                    "INSERT INTO usage_payload (usage_detail_id, created_time, update_time) \
                     VALUES (1, ?1, ?1)",
                    params![created],
                )
                .unwrap();
        }

        // 250 之前的两条删除，250 之后的保留（边界不算过期）。
        assert_eq!(prune_payloads(&connection, 250).unwrap(), 2);
        let remaining: Vec<i64> = connection
            .prepare("SELECT created_time FROM usage_payload ORDER BY created_time")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert_eq!(remaining, vec![300]);
    }
}

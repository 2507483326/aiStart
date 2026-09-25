use std::sync::{OnceLock, RwLock};

use rusqlite::params;
use serde_json::{json, Value};

use crate::db;
use crate::domain::canonical::CanonicalRequest;
use crate::domain::filter::{FilterRule, PromptMode, RequestFilter};
use crate::error::{AppError, AppResult};

/// 过滤器进程内缓存：网关每个请求读一次快照（与 settings::snapshot 一致），
/// 变更走 mutate() 全量落库。
static STORE: OnceLock<RwLock<Vec<RequestFilter>>> = OnceLock::new();

fn store() -> &'static RwLock<Vec<RequestFilter>> {
    STORE.get_or_init(|| RwLock::new(Vec::new()))
}

/// 启动时从 SQLite 载入全部过滤器（需在 db::init 之后调用）。
pub fn load() -> AppResult<()> {
    let filters = db::with_conn(|connection| {
        let mut statement = connection.prepare(
            "SELECT request_filter_id, name, enabled, sort_order, rule_config, created_time, update_time \
             FROM request_filters ORDER BY sort_order, request_filter_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })?;

        let mut filters = Vec::new();
        for row in rows {
            let (id, name, enabled, order, raw, created_time, update_time) = row?;
            let rule: FilterRule = serde_json::from_str(&raw)
                .map_err(|error| AppError::Message(format!("过滤器 {id} 规则解析失败: {error}")))?;
            filters.push(RequestFilter {
                id,
                name,
                enabled: enabled != 0,
                order,
                rule,
                created_at: db::iso_from_ms(created_time),
                updated_at: db::iso_from_ms(update_time),
            });
        }
        Ok(filters)
    })?;

    *store().write().expect("filters lock poisoned") = filters;
    Ok(())
}

pub fn snapshot() -> Vec<RequestFilter> {
    store().read().expect("filters lock poisoned").clone()
}

fn persist(filters: &[RequestFilter]) -> AppResult<()> {
    db::with_tx(|transaction| {
        let now = db::now_ms();
        transaction.execute("DELETE FROM request_filters", [])?;
        for (index, filter) in filters.iter().enumerate() {
            let rule_config = serde_json::to_string(&filter.rule)?;
            transaction.execute(
                "INSERT INTO request_filters \
                 (request_filter_id, name, enabled, sort_order, rule_kind, rule_config, created_time, update_time) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    filter.id,
                    filter.name,
                    i64::from(filter.enabled),
                    index as i64,
                    filter.rule.kind(),
                    rule_config,
                    db::ms_from_iso(&filter.created_at).unwrap_or(now),
                    db::ms_from_iso(&filter.updated_at).unwrap_or(now),
                ],
            )?;
        }
        Ok(())
    })
}

/// 在写锁内变更缓存并整体落库（落库用变更后的快照）。
pub fn mutate<T>(f: impl FnOnce(&mut Vec<RequestFilter>) -> T) -> AppResult<T> {
    let (value, snapshot) = {
        let mut guard = store().write().expect("filters lock poisoned");
        let value = f(&mut guard);
        (value, guard.clone())
    };
    persist(&snapshot)?;
    Ok(value)
}

/// 把启用的过滤器按顺序依次套用到规范请求上；跳过停用的规则。
pub fn apply(rules: &[RequestFilter], request: CanonicalRequest) -> AppResult<CanonicalRequest> {
    let mut request = request;
    for rule in rules.iter().filter(|rule| rule.enabled) {
        request = apply_rule(request, &rule.rule)?;
    }
    Ok(request)
}

fn apply_rule(request: CanonicalRequest, rule: &FilterRule) -> AppResult<CanonicalRequest> {
    let FilterRule::SystemPrompt { mode, text } = rule;
    let mut request = request.map_raw(|raw| {
        apply_system_prompt(raw, *mode, text);
        Ok(())
    })?;
    // 记下「system 被改过」：同协议直通以客户端原文为底，只覆盖改过的规范字段，
    // 不标记的话注入的提示词会被客户端原文盖回去。
    request.mark_dirty("system");
    Ok(request)
}

fn apply_system_prompt(raw: &mut Value, mode: PromptMode, text: &str) {
    let Some(object) = raw.as_object_mut() else {
        return;
    };
    let current = object.remove("system");
    let next = match (mode, current) {
        (PromptMode::Append | PromptMode::Prepend, None) => Value::String(text.to_string()),
        (PromptMode::Append, Some(Value::String(existing))) => {
            Value::String(join_text(&existing, text, true))
        }
        (PromptMode::Prepend, Some(Value::String(existing))) => {
            Value::String(join_text(text, &existing, true))
        }
        (PromptMode::Append, Some(Value::Array(mut blocks))) => {
            blocks.push(json!({ "type": "text", "text": text }));
            Value::Array(blocks)
        }
        (PromptMode::Prepend, Some(Value::Array(mut blocks))) => {
            blocks.insert(0, json!({ "type": "text", "text": text }));
            Value::Array(blocks)
        }
        // 异常形态（既非字符串也非块数组）：包成块数组后再按位置插入。
        (PromptMode::Append, Some(other)) => {
            Value::Array(vec![other, json!({ "type": "text", "text": text })])
        }
        (PromptMode::Prepend, Some(other)) => {
            Value::Array(vec![json!({ "type": "text", "text": text }), other])
        }
    };
    object.insert("system".into(), next);
}

fn join_text(left: &str, right: &str, newline: bool) -> String {
    let mut result = left.to_string();
    if newline && !result.is_empty() && !right.is_empty() {
        result.push('\n');
    }
    result.push_str(right);
    result
}

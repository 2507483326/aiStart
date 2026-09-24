use std::sync::{OnceLock, RwLock};

use rusqlite::params;
use serde_json::{json, Value};

use crate::db;
use crate::domain::canonical::CanonicalRequest;
use crate::domain::filter::{FilterRule, PromptMode, ReplaceTarget, RequestFilter};
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
    match rule {
        FilterRule::SystemPrompt { mode, text } => request.map_raw(|raw| {
            apply_system_prompt(raw, *mode, text);
            Ok(())
        }),
        FilterRule::RequestParams {
            temperature,
            max_tokens,
            top_p,
            stop_sequences,
        } => request.map_raw(|raw| {
            let Some(object) = raw.as_object_mut() else {
                return Ok(());
            };
            if let Some(value) = temperature {
                object.insert("temperature".into(), json!(value));
            }
            if let Some(value) = max_tokens {
                object.insert("max_tokens".into(), json!(value));
            }
            if let Some(value) = top_p {
                object.insert("top_p".into(), json!(value));
            }
            if let Some(value) = stop_sequences {
                object.insert("stop_sequences".into(), json!(value));
            }
            Ok(())
        }),
        FilterRule::TextReplace {
            find,
            replace,
            target,
        } => {
            // 空查找串没有意义，且字符串 replace("") 会到处插入，直接跳过。
            if find.is_empty() {
                return Ok(request);
            }
            request.map_raw(|raw| {
                apply_text_replace(raw, find, replace, *target);
                Ok(())
            })
        }
    }
}

fn apply_system_prompt(raw: &mut Value, mode: PromptMode, text: &str) {
    let Some(object) = raw.as_object_mut() else {
        return;
    };
    let current = object.remove("system");
    let next = match (mode, current) {
        (PromptMode::Replace, _) => Value::String(text.to_string()),
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

fn apply_text_replace(raw: &mut Value, find: &str, replace: &str, target: ReplaceTarget) {
    let Some(object) = raw.as_object_mut() else {
        return;
    };
    if matches!(target, ReplaceTarget::System | ReplaceTarget::All) {
        if let Some(system) = object.get_mut("system") {
            replace_in_text_value(system, find, replace);
        }
    }
    if matches!(target, ReplaceTarget::Messages | ReplaceTarget::All) {
        if let Some(Value::Array(messages)) = object.get_mut("messages") {
            for message in messages.iter_mut() {
                if let Some(content) = message.get_mut("content") {
                    replace_in_text_value(content, find, replace);
                }
            }
        }
    }
}

/// 只在文本字段里做字面量替换：字符串整体、块数组里带 `text` 字段的块、
/// 以及 tool_result 的 `content`（可能是字符串或块数组）；绝不改动 tool_use.input
/// 之类结构化字段。
fn replace_in_text_value(value: &mut Value, find: &str, replace: &str) {
    match value {
        Value::String(text) => replace_in_string(text, find, replace),
        Value::Array(items) => {
            for item in items.iter_mut() {
                replace_in_text_value(item, find, replace);
            }
        }
        Value::Object(map) => {
            if let Some(Value::String(text)) = map.get_mut("text") {
                replace_in_string(text, find, replace);
            }
            if let Some(content) = map.get_mut("content") {
                replace_in_text_value(content, find, replace);
            }
        }
        _ => {}
    }
}

fn replace_in_string(text: &mut String, find: &str, replace: &str) {
    if text.contains(find) {
        *text = text.replace(find, replace);
    }
}

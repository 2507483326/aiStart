use serde::Deserialize;
use serde_json::json;

use crate::db;
use crate::domain::filter::{FilterRule, RequestFilter};
use crate::error::{AppError, AppResult};
use crate::events;
use crate::filters;

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterInput {
    #[serde(default)]
    pub id: Option<i64>,
    pub name: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub rule: FilterRule,
}

fn validate(input: &FilterInput) -> AppResult<()> {
    if input.name.trim().is_empty() {
        return Err(AppError::InvalidConfig("过滤器名称不能为空".into()));
    }
    match &input.rule {
        FilterRule::SystemPrompt { text, .. } if text.trim().is_empty() => {
            Err(AppError::InvalidConfig("系统提示词不能为空".into()))
        }
        _ => Ok(()),
    }
}

#[tauri::command]
pub fn list_filters() -> AppResult<Vec<RequestFilter>> {
    Ok(filters::snapshot())
}

#[tauri::command]
pub fn save_filter(input: FilterInput) -> AppResult<RequestFilter> {
    validate(&input)?;
    let FilterInput {
        id,
        name,
        enabled,
        rule,
    } = input;
    let name = name.trim().to_string();

    let saved = filters::mutate(|list| {
        let now = db::iso_from_ms(db::now_ms());
        if let Some(id) = id {
            if let Some(existing) = list.iter_mut().find(|filter| filter.id == id) {
                existing.name = name.clone();
                existing.enabled = enabled;
                existing.rule = rule.clone();
                existing.updated_at = now.clone();
                return existing.clone();
            }
        }
        let order = list.len() as i64;
        let filter = RequestFilter {
            id: list.iter().map(|filter| filter.id).max().unwrap_or(0) + 1,
            name: name.clone(),
            enabled,
            order,
            rule: rule.clone(),
            created_at: now.clone(),
            updated_at: now,
        };
        list.push(filter.clone());
        filter
    })?;

    events::log(
        "user",
        None,
        "request_filter.saved",
        Some("request_filter"),
        Some(&saved.id.to_string()),
        Some(json!({ "name": saved.name, "kind": saved.rule.kind() })),
    );
    Ok(saved)
}

#[tauri::command]
pub fn set_filter_enabled(id: i64, enabled: bool) -> AppResult<RequestFilter> {
    let exists = filters::snapshot().iter().any(|filter| filter.id == id);
    if !exists {
        return Err(AppError::NotFound(format!("过滤器 {id} 不存在")));
    }
    let saved = filters::mutate(|list| {
        let now = db::iso_from_ms(db::now_ms());
        let filter = list
            .iter_mut()
            .find(|filter| filter.id == id)
            .expect("过滤器存在性已校验");
        filter.enabled = enabled;
        filter.updated_at = now;
        filter.clone()
    })?;

    events::log(
        "user",
        None,
        "request_filter.toggled",
        Some("request_filter"),
        Some(&id.to_string()),
        Some(json!({ "enabled": enabled })),
    );
    Ok(saved)
}

#[tauri::command]
pub fn delete_filter(id: i64) -> AppResult<Vec<RequestFilter>> {
    let remaining = filters::mutate(|list| {
        list.retain(|filter| filter.id != id);
        list.clone()
    })?;
    events::log(
        "user",
        None,
        "request_filter.deleted",
        Some("request_filter"),
        Some(&id.to_string()),
        None,
    );
    Ok(remaining)
}

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::domain::canonical::CanonicalRequest;
use crate::domain::catalog;
use crate::domain::model::{ModelConfig, ModelFormat, ModelInput, ModelPreset};
use crate::error::{AppError, AppResult};
use crate::gateway;
use crate::providers::{http_client, provider_for};
use crate::settings;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatInfo {
    pub format: ModelFormat,
    pub display_name: String,
    pub description: String,
    pub default_base_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub ok: bool,
    pub latency_ms: u64,
    pub message: String,
    pub preview: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[tauri::command]
pub fn list_model_formats() -> Vec<FormatInfo> {
    ModelFormat::ALL
        .iter()
        .map(|format| FormatInfo {
            format: *format,
            display_name: format.display_name().to_string(),
            description: format.description().to_string(),
            default_base_url: format.default_base_url().to_string(),
        })
        .collect()
}

#[tauri::command]
pub fn list_models() -> AppResult<Vec<ModelConfig>> {
    Ok(settings::snapshot().models)
}

#[tauri::command]
pub fn list_model_presets() -> Vec<ModelPreset> {
    catalog::builtin_model_presets()
}

#[tauri::command]
pub fn save_model(input: ModelInput) -> AppResult<ModelConfig> {
    if input.name.trim().is_empty() {
        return Err(AppError::InvalidConfig("模型名称不能为空".into()));
    }
    if input.base_url.trim().is_empty() {
        return Err(AppError::InvalidConfig("Base URL 不能为空".into()));
    }
    if input.model.trim().is_empty() {
        return Err(AppError::InvalidConfig("上游模型 ID 不能为空".into()));
    }
    settings::mutate(|settings| settings.upsert(input))
}

#[tauri::command]
pub fn delete_model(id: String) -> AppResult<Vec<ModelConfig>> {
    settings::mutate(|settings| {
        settings.remove(&id);
        settings.models.clone()
    })
}

#[tauri::command]
pub fn activate_model(id: String) -> AppResult<gateway::GatewayStatus> {
    let exists = settings::snapshot().models.iter().any(|model| model.id == id);
    if !exists {
        return Err(AppError::NotFound(format!("模型 {id} 不存在")));
    }
    settings::mutate(|settings| {
        settings.active_model_id = Some(id.clone());
    })?;

    if gateway::status().running {
        gateway::restart()
    } else {
        Ok(gateway::status())
    }
}

fn probe_config(base_url: &str, api_key: &str, format: ModelFormat) -> ModelConfig {
    ModelConfig {
        id: "probe".into(),
        name: "probe".into(),
        format,
        base_url: base_url.to_string(),
        api_key: api_key.to_string(),
        model: String::new(),
        supports_1m: false,
        created_at: String::new(),
        updated_at: String::new(),
    }
}

pub(crate) fn parse_model_ids(value: &Value) -> Vec<String> {
    let mut ids: Vec<String> = value
        .get("data")
        .or_else(|| value.get("models"))
        .or_else(|| value.get("result"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match item {
                    Value::String(id) => Some(id.clone()),
                    _ => item
                        .get("id")
                        .or_else(|| item.get("name"))
                        .or_else(|| item.get("model"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default();

    if ids.is_empty() {
        if let Some(items) = value.as_array() {
            ids = items
                .iter()
                .filter_map(|item| match item {
                    Value::String(id) => Some(id.clone()),
                    _ => item.get("id").and_then(Value::as_str).map(str::to_string),
                })
                .collect();
        }
    }

    ids.retain(|id| !id.trim().is_empty());
    ids.sort();
    ids.dedup();
    ids
}

#[tauri::command]
pub async fn fetch_upstream_models(
    base_url: String,
    api_key: String,
    format: ModelFormat,
) -> AppResult<Vec<String>> {
    if base_url.trim().is_empty() {
        return Err(AppError::InvalidConfig("请先填写 Base URL".into()));
    }

    let config = probe_config(&base_url, &api_key, format);
    let provider = provider_for(format);

    let mut builder = http_client().get(config.models_url());
    for (name, value) in provider.headers(&config) {
        builder = builder.header(name, value);
    }

    let response = builder.send().await?;
    let status = response.status();
    let raw = response.text().await.unwrap_or_default();

    if !status.is_success() {
        return Err(AppError::Message(format!(
            "上游返回 HTTP {}: {}",
            status.as_u16(),
            truncate(&raw, 300)
        )));
    }

    let value: Value = serde_json::from_str(&raw)
        .map_err(|error| AppError::Message(format!("模型列表不是合法 JSON: {error}")))?;

    let ids = parse_model_ids(&value);

    if ids.is_empty() {
        return Err(AppError::Message(
            "上游没有返回任何模型，可能是该端点不支持模型列表接口".into(),
        ));
    }

    Ok(ids)
}

fn preview_from_response(response: &Value) -> Option<String> {
    let content = response.get("content")?.as_array()?;
    let text = content
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    if text.trim().is_empty() {
        None
    } else {
        Some(text.chars().take(200).collect())
    }
}

#[tauri::command]
pub async fn test_model(id: String) -> AppResult<TestResult> {
    let config = settings::require_model(&id)?;
    let provider = provider_for(config.format);

    let request = CanonicalRequest::parse(json!({
        "model": config.model,
        "max_tokens": 64,
        "messages": [{ "role": "user", "content": "回复 pong 一个词即可，不要多余内容。" }],
        "stream": false
    }))?;

    let payload = provider.encode_request(&config, &request)?;
    let mut builder = http_client().post(provider.endpoint(&config)).json(&payload);
    for (name, value) in provider.headers(&config) {
        builder = builder.header(name, value);
    }

    let started = std::time::Instant::now();
    let response = builder.send().await?;
    let elapsed = started.elapsed().as_millis() as u64;
    let status = response.status();
    let raw = response.text().await.unwrap_or_default();

    if !status.is_success() {
        return Ok(TestResult {
            ok: false,
            latency_ms: elapsed,
            message: format!("上游返回 HTTP {}: {}", status.as_u16(), truncate(&raw, 400)),
            preview: None,
            input_tokens: 0,
            output_tokens: 0,
        });
    }

    let value: Value = serde_json::from_str(&raw)
        .map_err(|error| AppError::Message(format!("上游响应不是合法 JSON: {error}")))?;
    let decoded = provider.decode_response(&config, &value)?;

    Ok(TestResult {
        ok: true,
        latency_ms: elapsed,
        message: format!("{} 连接正常", config.name),
        preview: preview_from_response(&decoded),
        input_tokens: decoded
            .pointer("/usage/input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: decoded
            .pointer("/usage/output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

fn truncate(input: &str, limit: usize) -> String {
    if input.chars().count() <= limit {
        return input.to_string();
    }
    let head: String = input.chars().take(limit).collect();
    format!("{head}…")
}

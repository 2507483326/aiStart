use std::time::Duration;

use serde_json::{json, Value};

use crate::domain::canonical::CanonicalRequest;
use crate::error::{AppError, AppResult};
use crate::providers::{http_client, provider_for};
use crate::settings;

/// 翻译可能整段报文，比探测请求长得多，给一个宽松但有限的硬超时。
const TRANSLATE_TIMEOUT: Duration = Duration::from_secs(120);

const SYSTEM_PROMPT: &str = "你是一个翻译引擎。把用户提供的文本翻译成简体中文。\
只输出译文本身，不要添加解释、标题、引号或任何额外说明。\
保留原文的换行、Markdown 结构、代码块，以及 API、JSON 字段名、命令等专有名词与技术术语。\
如果原文已经是简体中文，则原样返回。";

/// 从规范（Anthropic 形状）响应里取出正文，供翻译结果返回。
fn output_text(canonical: &Value) -> String {
    canonical
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

#[tauri::command]
pub async fn translate_text(text: String) -> AppResult<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidConfig("没有可翻译的文本".into()));
    }

    let config = settings::snapshot().active_model().cloned().ok_or_else(|| {
        AppError::InvalidConfig("请先在「模型」页面启用一个模型，翻译会复用它".into())
    })?;
    let provider = provider_for(config.format);

    // 用字符数粗略估算输出长度，避免长报文被 max_tokens 截断。
    let estimate = trimmed.chars().count() * 2 + 512;
    let max_tokens = estimate.clamp(1024, 16000) as u32;

    let request = CanonicalRequest::parse(json!({
        "model": config.model,
        "max_tokens": max_tokens,
        "temperature": 0.0,
        "system": SYSTEM_PROMPT,
        "messages": [{ "role": "user", "content": trimmed }],
        "stream": false
    }))?;

    let payload = provider.encode_request(&config, &request)?;
    let mut builder = http_client()
        .post(provider.endpoint(&config))
        .timeout(TRANSLATE_TIMEOUT)
        .json(&payload);
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
        .map_err(|error| AppError::Message(format!("上游响应不是合法 JSON: {error}")))?;
    let decoded = provider.decode_response(&config, &value)?;
    let translated = output_text(&decoded).trim().to_string();

    if translated.is_empty() {
        return Err(AppError::Message("翻译结果为空，请稍后重试".into()));
    }
    Ok(translated)
}

fn truncate(input: &str, limit: usize) -> String {
    if input.chars().count() <= limit {
        return input.to_string();
    }
    let head: String = input.chars().take(limit).collect();
    format!("{head}…")
}

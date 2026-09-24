use serde_json::Value;

use crate::domain::canonical::CanonicalRequest;
use crate::domain::model::ModelConfig;
use crate::error::{AppError, AppResult};

use super::{ModelProvider, SseEvent, StreamState, ANTHROPIC_VERSION};

pub struct AnthropicMessagesProvider;

impl ModelProvider for AnthropicMessagesProvider {
    fn is_passthrough(&self) -> bool {
        true
    }

    fn endpoint(&self, cfg: &ModelConfig) -> String {
        let base = cfg.base_url.trim_end_matches('/');
        if base.ends_with("/v1/messages") {
            base.to_string()
        } else if base.ends_with("/v1") {
            format!("{base}/messages")
        } else {
            format!("{base}/v1/messages")
        }
    }

    fn headers(&self, cfg: &ModelConfig) -> Vec<(String, String)> {
        let mut headers = vec![
            ("content-type".to_string(), "application/json".to_string()),
            (
                "anthropic-version".to_string(),
                ANTHROPIC_VERSION.to_string(),
            ),
        ];
        if !cfg.api_key.is_empty() {
            headers.push(("x-api-key".to_string(), cfg.api_key.clone()));
            if !cfg.base_url.contains("api.anthropic.com") {
                headers.push((
                    "authorization".to_string(),
                    format!("Bearer {}", cfg.api_key),
                ));
            }
        }
        headers
    }

    fn encode_request(&self, cfg: &ModelConfig, req: &CanonicalRequest) -> AppResult<Value> {
        let mut body = req.raw().clone();
        let object = body
            .as_object_mut()
            .ok_or_else(|| AppError::InvalidConfig("请求体必须是 JSON 对象".into()))?;
        object.insert("model".into(), Value::String(cfg.model.clone()));
        if !object.contains_key("max_tokens") {
            object.insert(
                "max_tokens".into(),
                serde_json::json!(crate::domain::model::DEFAULT_MAX_TOKENS),
            );
        }
        Ok(body)
    }

    fn decode_response(&self, _cfg: &ModelConfig, raw: &Value) -> AppResult<Value> {
        Ok(raw.clone())
    }

    fn decode_stream_event(
        &self,
        _cfg: &ModelConfig,
        event: &str,
        data: &Value,
        state: &mut StreamState,
    ) -> AppResult<Vec<SseEvent>> {
        if data.get("type").and_then(Value::as_str) == Some("ping") && event.is_empty() {
            return Ok(vec![SseEvent::new("ping", data.clone())]);
        }
        let name = if event.is_empty() {
            data.get("type")
                .and_then(Value::as_str)
                .unwrap_or("message")
                .to_string()
        } else {
            event.to_string()
        };

        match name.as_str() {
            "message_start" => {
                state.message_started = true;
                if let Some(model) = data
                    .pointer("/message/model")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                {
                    state.upstream_model = model;
                }
                if let Some(id) = data.pointer("/message/id").and_then(Value::as_str) {
                    state.message_id = id.to_string();
                }
                if let Some(tokens) = data
                    .pointer("/message/usage/input_tokens")
                    .and_then(Value::as_u64)
                {
                    state.input_tokens = tokens;
                }
                if let Some(tokens) = data
                    .pointer("/message/usage/cache_read_input_tokens")
                    .and_then(Value::as_u64)
                {
                    state.cache_read_tokens = tokens;
                }
                if let Some(tokens) = data
                    .pointer("/message/usage/cache_creation_input_tokens")
                    .and_then(Value::as_u64)
                {
                    state.cache_write_tokens = tokens;
                }
            }
            "message_stop" => state.finished = true,
            "error" => state.finished = true,
            _ => {}
        }

        Ok(vec![SseEvent::new(name, data.clone())])
    }

    fn decode_stream_done(
        &self,
        _cfg: &ModelConfig,
        state: &mut StreamState,
    ) -> AppResult<Vec<SseEvent>> {
        if state.finished {
            return Ok(Vec::new());
        }
        Ok(state.finish("end_turn"))
    }
}

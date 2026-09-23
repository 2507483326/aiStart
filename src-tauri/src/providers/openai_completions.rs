use serde_json::{json, Map, Value};

use crate::domain::canonical::{blocks_to_text, content_to_text, CanonicalRequest, ContentBlock};
use crate::domain::model::ModelConfig;
use crate::error::AppResult;

use super::{BlockKind, DeltaKind, ModelProvider, SseEvent, StreamState};

pub struct OpenaiCompletionsProvider;

fn image_url_from_block(block: &ContentBlock) -> Option<String> {
    let source = block.field("source")?;
    match source.get("type").and_then(Value::as_str) {
        Some("base64") => {
            let media_type = source
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("image/png");
            let data = source.get("data").and_then(Value::as_str)?;
            Some(format!("data:{media_type};base64,{data}"))
        }
        Some("url") => source
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

fn encode_tools(tools: &[crate::domain::canonical::ToolDef]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|tool| {
                let mut function = Map::new();
                function.insert("name".into(), Value::String(tool.name.clone()));
                if let Some(description) = &tool.description {
                    function.insert("description".into(), Value::String(description.clone()));
                }
                function.insert("parameters".into(), tool.input_schema.clone());
                json!({ "type": "function", "function": Value::Object(function) })
            })
            .collect(),
    )
}

fn encode_tool_choice(choice: &Value) -> Option<Value> {
    let kind = choice.get("type").and_then(Value::as_str)?;
    Some(match kind {
        "auto" => Value::String("auto".into()),
        "any" => Value::String("required".into()),
        "none" => Value::String("none".into()),
        "tool" => {
            let name = choice.get("name").and_then(Value::as_str).unwrap_or_default();
            json!({ "type": "function", "function": { "name": name } })
        }
        _ => Value::String("auto".into()),
    })
}

impl ModelProvider for OpenaiCompletionsProvider {
    fn headers(&self, cfg: &ModelConfig) -> Vec<(String, String)> {
        let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
        if !cfg.api_key.is_empty() {
            headers.push((
                "authorization".to_string(),
                format!("Bearer {}", cfg.api_key),
            ));
        }
        headers
    }

    fn encode_request(&self, cfg: &ModelConfig, req: &CanonicalRequest) -> AppResult<Value> {
        let body = req.body();
        let mut messages: Vec<Value> = Vec::new();

        if let Some(system) = &body.system {
            let text = system.plain_text();
            if !text.is_empty() {
                messages.push(json!({ "role": "system", "content": text }));
            }
        }

        for message in &body.messages {
            let blocks = message.content.blocks();
            if message.role == "assistant" {
                let text = blocks_to_text(&blocks);
                let tool_calls: Vec<Value> = blocks
                    .iter()
                    .filter_map(|block| block.tool_use())
                    .map(|tool| {
                        json!({
                            "id": tool.id,
                            "type": "function",
                            "function": {
                                "name": tool.name,
                                "arguments": serde_json::to_string(&tool.input).unwrap_or_else(|_| "{}".into())
                            }
                        })
                    })
                    .collect();

                let mut entry = Map::new();
                entry.insert("role".into(), Value::String("assistant".into()));
                entry.insert(
                    "content".into(),
                    if text.is_empty() {
                        Value::Null
                    } else {
                        Value::String(text)
                    },
                );
                if !tool_calls.is_empty() {
                    entry.insert("tool_calls".into(), Value::Array(tool_calls));
                }
                if entry.contains_key("tool_calls") || !entry["content"].is_null() {
                    messages.push(Value::Object(entry));
                }
                continue;
            }

            let mut pending_parts: Vec<Value> = Vec::new();
            let flush = |parts: &mut Vec<Value>, messages: &mut Vec<Value>| {
                if parts.is_empty() {
                    return;
                }
                let content = std::mem::take(parts);
                messages.push(json!({ "role": "user", "content": content }));
            };

            for block in &blocks {
                if block.is("text") {
                    pending_parts.push(json!({ "type": "text", "text": block.text_value() }));
                } else if block.is("image") {
                    if let Some(url) = image_url_from_block(block) {
                        pending_parts.push(json!({ "type": "image_url", "image_url": { "url": url } }));
                    }
                } else if let Some(result) = block.tool_result() {
                    flush(&mut pending_parts, &mut messages);
                    let text = content_to_text(&result.content);
                    let payload = if result.is_error {
                        format!("Error: {text}")
                    } else {
                        text
                    };
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": result.tool_use_id,
                        "content": payload
                    }));
                }
            }

            if pending_parts.len() == 1 && pending_parts[0]["type"] == "text" {
                let text = pending_parts[0]["text"].clone();
                messages.push(json!({ "role": "user", "content": text }));
            } else {
                flush(&mut pending_parts, &mut messages);
            }
        }

        let mut payload = Map::new();
        payload.insert("model".into(), Value::String(cfg.model.clone()));
        payload.insert("messages".into(), Value::Array(messages));
        payload.insert(
            "max_tokens".into(),
            json!(body
                .max_tokens
                .unwrap_or(crate::domain::model::DEFAULT_MAX_TOKENS)),
        );
        payload.insert("stream".into(), Value::Bool(body.stream));

        if let Some(top_p) = body.top_p {
            payload.insert("top_p".into(), json!(top_p));
        }
        if let Some(stop) = &body.stop_sequences {
            payload.insert("stop".into(), json!(stop));
        }
        if let Some(tools) = &body.tools {
            if !tools.is_empty() {
                payload.insert("tools".into(), encode_tools(tools));
            }
        }
        if let Some(choice) = &body.tool_choice {
            if let Some(encoded) = encode_tool_choice(choice) {
                payload.insert("tool_choice".into(), encoded);
            }
        }

        Ok(Value::Object(payload))
    }

    fn decode_response(&self, cfg: &ModelConfig, raw: &Value) -> AppResult<Value> {
        let message = raw
            .pointer("/choices/0/message")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let finish_reason = raw
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str);

        let mut content: Vec<Value> = Vec::new();
        if let Some(reasoning) = message
            .get("reasoning_content")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            content.push(json!({ "type": "thinking", "thinking": reasoning }));
        }
        if let Some(text) = message.get("content").and_then(Value::as_str) {
            if !text.is_empty() {
                content.push(json!({ "type": "text", "text": text }));
            }
        }
        if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
            for call in tool_calls {
                let arguments = call
                    .pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                content.push(json!({
                    "type": "tool_use",
                    "id": call.get("id").and_then(Value::as_str).unwrap_or_default(),
                    "name": call.pointer("/function/name").and_then(Value::as_str).unwrap_or_default(),
                    "input": serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!({}))
                }));
            }
        }

        let stop_reason = super::resolve_stop_reason(finish_reason);

        Ok(json!({
            "id": raw.get("id").and_then(Value::as_str).map(str::to_string)
                .unwrap_or_else(|| format!("msg_{}", uuid::Uuid::new_v4().simple())),
            "type": "message",
            "role": "assistant",
            "model": raw.get("model").and_then(Value::as_str).unwrap_or(&cfg.model),
            "content": content,
            "stop_reason": stop_reason,
            "stop_sequence": null,
            "usage": {
                "input_tokens": raw.pointer("/usage/prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
                "output_tokens": raw.pointer("/usage/completion_tokens").and_then(Value::as_u64).unwrap_or(0)
            }
        }))
    }

    fn decode_stream_event(
        &self,
        _cfg: &ModelConfig,
        _event: &str,
        data: &Value,
        state: &mut StreamState,
    ) -> AppResult<Vec<SseEvent>> {
        if let Some(usage) = data.get("usage").filter(|value| !value.is_null()) {
            if let Some(prompt) = usage.get("prompt_tokens").and_then(Value::as_u64) {
                state.input_tokens = prompt;
            }
            if let Some(completion) = usage.get("completion_tokens").and_then(Value::as_u64) {
                state.output_tokens = completion;
            }
        }

        let Some(choice) = data.pointer("/choices/0") else {
            return Ok(Vec::new());
        };

        let mut events: Vec<SseEvent> = Vec::new();

        if let Some(delta) = choice.get("delta") {
            if let Some(reasoning) = delta
                .get("reasoning_content")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                events.extend(state.delta(DeltaKind::Thinking, reasoning));
            }

            if let Some(text) = delta
                .get("content")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                if state.open_block == Some(BlockKind::Thinking) {
                    if let Some(event) = state.close_block() {
                        events.push(event);
                    }
                }
                events.extend(state.delta(DeltaKind::Text, text));
                state.output_tokens += 1;
            }

            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for call in tool_calls {
                    let index = call.get("index").and_then(Value::as_i64).unwrap_or(0);

                    if state.tool_calls.insert(index) {
                        let id = call
                            .get("id")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("toolu_{}", uuid::Uuid::new_v4().simple()));
                        let name = call
                            .pointer("/function/name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        events.extend(state.open_tool(&id, &name));
                    }

                    if let Some(partial) = call
                        .pointer("/function/arguments")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                    {
                        events.extend(state.delta(DeltaKind::Json, partial));
                    }
                }
            }
        }

        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            let stop_reason = super::resolve_stop_reason(Some(reason));
            events.extend(state.finish(&stop_reason));
        }

        Ok(events)
    }

    fn decode_stream_done(&self, _cfg: &ModelConfig, state: &mut StreamState) -> AppResult<Vec<SseEvent>> {
        if state.finished {
            return Ok(Vec::new());
        }
        let stop_reason = if state.tool_calls.is_empty() {
            "end_turn"
        } else {
            "tool_use"
        };
        Ok(state.finish(stop_reason))
    }
}

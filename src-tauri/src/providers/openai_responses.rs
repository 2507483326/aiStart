use serde_json::{json, Map, Value};

use crate::domain::canonical::{blocks_to_text, content_to_text, CanonicalRequest, ContentBlock};
use crate::domain::model::ModelConfig;
use crate::error::AppResult;

use super::{DeltaKind, ModelProvider, SseEvent, StreamState};

pub struct OpenaiResponsesProvider;

fn encode_content_parts(blocks: &[ContentBlock], role: &str) -> Vec<Value> {
    let text_type = if role == "assistant" {
        "output_text"
    } else {
        "input_text"
    };
    let mut parts = Vec::new();
    for block in blocks {
        if block.is("text") {
            parts.push(json!({ "type": text_type, "text": block.text_value() }));
        } else if block.is("image") {
            if let Some(source) = block.field("source") {
                let url = match source.get("type").and_then(Value::as_str) {
                    Some("base64") => {
                        let media_type = source
                            .get("media_type")
                            .and_then(Value::as_str)
                            .unwrap_or("image/png");
                        source
                            .get("data")
                            .and_then(Value::as_str)
                            .map(|data| format!("data:{media_type};base64,{data}"))
                    }
                    Some("url") => source
                        .get("url")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    _ => None,
                };
                if let Some(url) = url {
                    parts.push(json!({ "type": "input_image", "image_url": url }));
                }
            }
        }
    }
    parts
}

fn encode_tools(tools: &[crate::domain::canonical::ToolDef]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|tool| {
                let mut item = Map::new();
                item.insert("type".into(), Value::String("function".into()));
                item.insert("name".into(), Value::String(tool.name.clone()));
                if let Some(description) = &tool.description {
                    item.insert("description".into(), Value::String(description.clone()));
                }
                item.insert("parameters".into(), tool.input_schema.clone());
                Value::Object(item)
            })
            .collect(),
    )
}

impl ModelProvider for OpenaiResponsesProvider {
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
        let mut input: Vec<Value> = Vec::new();

        for message in &body.messages {
            let blocks = message.content.blocks();
            let is_assistant = message.role == "assistant";
            let mut pending: Vec<Value> = Vec::new();

            for block in &blocks {
                if block.is("text") || block.is("image") {
                    continue;
                }
                if let Some(tool) = block.tool_use() {
                    if !pending.is_empty() {
                        let parts = std::mem::take(&mut pending);
                        input.push(json!({ "role": "assistant", "content": parts }));
                    }
                    input.push(json!({
                        "type": "function_call",
                        "call_id": tool.id,
                        "name": tool.name,
                        "arguments": serde_json::to_string(&tool.input).unwrap_or_else(|_| "{}".into())
                    }));
                } else if let Some(result) = block.tool_result() {
                    if !pending.is_empty() {
                        let parts = std::mem::take(&mut pending);
                        input.push(json!({ "role": "assistant", "content": parts }));
                    }
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": result.tool_use_id,
                        "output": content_to_text(&result.content)
                    }));
                }
            }

            let parts = encode_content_parts(&blocks, &message.role);
            if !parts.is_empty() && !(is_assistant && blocks_to_text(&blocks).is_empty()) {
                input.push(json!({ "role": message.role, "content": parts }));
            }
        }

        let mut payload = Map::new();
        payload.insert("model".into(), Value::String(cfg.model.clone()));
        payload.insert("input".into(), Value::Array(input));
        payload.insert("stream".into(), Value::Bool(body.stream));
        payload.insert(
            "max_output_tokens".into(),
            json!(body
                .max_tokens
                .unwrap_or(crate::domain::model::DEFAULT_MAX_TOKENS)),
        );

        if let Some(system) = &body.system {
            let text = system.plain_text();
            if !text.is_empty() {
                payload.insert("instructions".into(), Value::String(text));
            }
        }
        if let Some(top_p) = body.top_p {
            payload.insert("top_p".into(), json!(top_p));
        }
        if let Some(tools) = &body.tools {
            if !tools.is_empty() {
                payload.insert("tools".into(), encode_tools(tools));
            }
        }

        Ok(Value::Object(payload))
    }

    fn decode_response(&self, cfg: &ModelConfig, raw: &Value) -> AppResult<Value> {
        let mut content: Vec<Value> = Vec::new();
        let status = raw.get("status").and_then(Value::as_str);

        if let Some(output) = raw.get("output").and_then(Value::as_array) {
            for item in output {
                match item.get("type").and_then(Value::as_str) {
                    Some("reasoning") => {
                        if let Some(summaries) = item.get("summary").and_then(Value::as_array) {
                            let text = summaries
                                .iter()
                                .filter_map(|summary| summary.get("text").and_then(Value::as_str))
                                .collect::<Vec<_>>()
                                .join("\n");
                            if !text.is_empty() {
                                content.push(json!({ "type": "thinking", "thinking": text }));
                            }
                        }
                    }
                    Some("message") => {
                        if let Some(parts) = item.get("content").and_then(Value::as_array) {
                            for part in parts {
                                if let Some(text) = part.get("text").and_then(Value::as_str) {
                                    if !text.is_empty() {
                                        content.push(json!({ "type": "text", "text": text }));
                                    }
                                }
                            }
                        }
                    }
                    Some("function_call") => {
                        let arguments = item.get("arguments").and_then(Value::as_str).unwrap_or("{}");
                        content.push(json!({
                            "type": "tool_use",
                            "id": item.get("call_id").and_then(Value::as_str).unwrap_or_default(),
                            "name": item.get("name").and_then(Value::as_str).unwrap_or_default(),
                            "input": serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!({}))
                        }));
                    }
                    _ => {}
                }
            }
        }

        let has_tools = content.iter().any(|block| block["type"] == "tool_use");
        let stop_reason = match status {
            Some("incomplete") => "max_tokens",
            _ if has_tools => "tool_use",
            _ => "end_turn",
        };

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
                "input_tokens": raw.pointer("/usage/input_tokens").and_then(Value::as_u64).unwrap_or(0),
                "output_tokens": raw.pointer("/usage/output_tokens").and_then(Value::as_u64).unwrap_or(0)
            }
        }))
    }

    fn decode_stream_event(
        &self,
        _cfg: &ModelConfig,
        event: &str,
        data: &Value,
        state: &mut StreamState,
    ) -> AppResult<Vec<SseEvent>> {
        let name = if event.is_empty() {
            data.get("type").and_then(Value::as_str).unwrap_or("")
        } else {
            event
        };
        let mut events: Vec<SseEvent> = Vec::new();

        match name {
            "response.created" => {
                if let Some(id) = data.pointer("/response/id").and_then(Value::as_str) {
                    state.message_id = id.to_string();
                }
                if let Some(model) = data.pointer("/response/model").and_then(Value::as_str) {
                    state.upstream_model = model.to_string();
                }
                events.extend(state.begin());
            }
            "response.output_text.delta" => {
                if let Some(delta) = data.get("delta").and_then(Value::as_str) {
                    events.extend(state.delta(DeltaKind::Text, delta));
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(delta) = data.get("delta").and_then(Value::as_str) {
                    events.extend(state.delta(DeltaKind::Thinking, delta));
                }
            }
            "response.output_item.added" => {
                if let Some(item) = data.get("item") {
                    if item.get("type").and_then(Value::as_str) == Some("function_call") {
                        let index = data
                            .get("output_index")
                            .and_then(Value::as_i64)
                            .unwrap_or(state.next_index);
                        let call_id = item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let call_name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        state.tool_calls.insert(index);
                        events.extend(state.open_tool(&call_id, &call_name));
                    }
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(delta) = data.get("delta").and_then(Value::as_str) {
                    events.extend(state.delta(DeltaKind::Json, delta));
                }
            }
            "response.completed" | "response.incomplete" => {
                let has_tools = !state.tool_calls.is_empty();
                let incomplete = name == "response.incomplete";
                if let Some(usage) = data.pointer("/response/usage") {
                    if let Some(input) = usage.get("input_tokens").and_then(Value::as_u64) {
                        state.input_tokens = input;
                    }
                    if let Some(output) = usage.get("output_tokens").and_then(Value::as_u64) {
                        state.output_tokens = output;
                    }
                }
                let stop_reason = if incomplete {
                    "max_tokens"
                } else if has_tools {
                    "tool_use"
                } else {
                    "end_turn"
                };
                events.extend(state.finish(stop_reason));
            }
            "response.failed" | "error" => {
                let message = data
                    .pointer("/response/error/message")
                    .or_else(|| data.pointer("/error/message"))
                    .and_then(Value::as_str)
                    .unwrap_or("上游返回错误");
                events.extend(state.error("api_error", message));
            }
            _ => {}
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

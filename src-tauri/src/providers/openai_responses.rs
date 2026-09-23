use serde_json::{json, Map, Value};

use crate::domain::canonical::{blocks_to_text, content_to_text, CanonicalRequest, ContentBlock};
use crate::domain::model::ModelConfig;
use crate::error::{AppError, AppResult};

use super::openai_completions::{data_url_to_source, parse_arguments};
use super::{DeltaKind, ModelProvider, SseEvent, StreamState, WireState};

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

    fn decode_request(&self, raw: Value) -> AppResult<CanonicalRequest> {
        let object = raw
            .as_object()
            .ok_or_else(|| AppError::InvalidConfig("请求体必须是 JSON 对象".into()))?;

        let mut system_parts: Vec<String> = Vec::new();
        if let Some(text) = object
            .get("instructions")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
        {
            system_parts.push(text.to_string());
        }

        let items: Vec<Value> = match object.get("input") {
            Some(Value::String(text)) => {
                vec![json!({ "role": "user", "content": [{ "type": "input_text", "text": text }] })]
            }
            Some(Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        };

        let mut messages: Vec<Value> = Vec::new();
        for item in &items {
            match item.get("type").and_then(Value::as_str) {
                Some("function_call") => messages.push(json!({
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": item.get("call_id").and_then(Value::as_str).unwrap_or_default(),
                        "name": item.get("name").and_then(Value::as_str).unwrap_or_default(),
                        "input": parse_arguments(item.get("arguments"))
                    }]
                })),
                Some("function_call_output") => messages.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": item.get("call_id").and_then(Value::as_str).unwrap_or_default(),
                        "content": content_to_text(item.get("output").unwrap_or(&Value::Null))
                    }]
                })),
                _ => {
                    let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
                    let mut blocks: Vec<Value> = Vec::new();
                    match item.get("content") {
                        Some(Value::String(text)) => {
                            if !text.is_empty() {
                                blocks.push(json!({ "type": "text", "text": text }));
                            }
                        }
                        Some(Value::Array(parts)) => {
                            for part in parts {
                                match part.get("type").and_then(Value::as_str) {
                                    Some("input_text")
                                    | Some("output_text")
                                    | Some("text")
                                    | Some("summary_text") => blocks.push(json!({
                                        "type": "text",
                                        "text": part.get("text").and_then(Value::as_str).unwrap_or_default()
                                    })),
                                    Some("input_image") => {
                                        let url = part
                                            .get("image_url")
                                            .and_then(Value::as_str)
                                            .unwrap_or_default();
                                        if let Some(source) = data_url_to_source(url) {
                                            blocks.push(json!({ "type": "image", "source": source }));
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                    if !blocks.is_empty() {
                        messages.push(json!({ "role": role, "content": blocks }));
                    }
                }
            }
        }

        let mut canonical = Map::new();
        canonical.insert(
            "model".into(),
            object
                .get("model")
                .cloned()
                .unwrap_or_else(|| Value::String(String::new())),
        );
        canonical.insert("messages".into(), Value::Array(messages));
        if !system_parts.is_empty() {
            canonical.insert("system".into(), Value::String(system_parts.join("\n")));
        }
        if let Some(max) = object.get("max_output_tokens").and_then(Value::as_u64) {
            canonical.insert("max_tokens".into(), json!(max));
        }
        for key in ["temperature", "top_p", "stream"] {
            if let Some(value) = object.get(key) {
                canonical.insert(key.into(), value.clone());
            }
        }
        if let Some(tools) = object.get("tools").and_then(Value::as_array) {
            let converted: Vec<Value> = tools
                .iter()
                .filter(|tool| tool.get("name").is_some())
                .map(|tool| {
                    json!({
                        "name": tool.get("name").and_then(Value::as_str).unwrap_or_default(),
                        "description": tool.get("description").cloned().unwrap_or(Value::Null),
                        "input_schema": tool.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object" }))
                    })
                })
                .collect();
            if !converted.is_empty() {
                canonical.insert("tools".into(), Value::Array(converted));
            }
        }

        CanonicalRequest::parse(Value::Object(canonical))
    }

    fn encode_response(&self, cfg: &ModelConfig, canonical: &Value) -> AppResult<Value> {
        let mut output: Vec<Value> = Vec::new();
        let mut text = String::new();
        let mut reasoning = String::new();

        for block in canonical
            .get("content")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => text
                    .push_str(block.get("text").and_then(Value::as_str).unwrap_or_default()),
                Some("thinking") => reasoning.push_str(
                    block
                        .get("thinking")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                ),
                Some("tool_use") => output.push(json!({
                    "type": "function_call",
                    "id": format!("fc_{}", uuid::Uuid::new_v4().simple()),
                    "call_id": block.get("id").cloned().unwrap_or_else(|| json!("")),
                    "name": block.get("name").cloned().unwrap_or_else(|| json!("")),
                    "arguments": serde_json::to_string(block.get("input").unwrap_or(&json!({})))
                        .unwrap_or_else(|_| "{}".into()),
                    "status": "completed"
                })),
                _ => {}
            }
        }

        if !reasoning.is_empty() {
            output.insert(
                0,
                json!({
                    "type": "reasoning",
                    "id": format!("rs_{}", uuid::Uuid::new_v4().simple()),
                    "summary": [{ "type": "summary_text", "text": reasoning }]
                }),
            );
        }
        if !text.is_empty() {
            let index = output
                .iter()
                .position(|item| item["type"] == "reasoning")
                .map(|position| position + 1)
                .unwrap_or(0);
            output.insert(
                index,
                json!({
                    "type": "message",
                    "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
                    "role": "assistant",
                    "status": "completed",
                    "content": [{ "type": "output_text", "text": text, "annotations": [] }]
                }),
            );
        }

        let input_tokens = canonical
            .pointer("/usage/input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let output_tokens = canonical
            .pointer("/usage/output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);

        Ok(json!({
            "id": format!("resp_{}", uuid::Uuid::new_v4().simple()),
            "object": "response",
            "created_at": chrono::Utc::now().timestamp(),
            "status": "completed",
            "model": cfg.model,
            "output": output,
            "usage": {
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "total_tokens": input_tokens + output_tokens
            }
        }))
    }

    fn encode_stream_event(
        &self,
        cfg: &ModelConfig,
        canonical: &SseEvent,
        state: &mut WireState,
    ) -> Vec<SseEvent> {
        let data = &canonical.data;
        match data.get("type").and_then(Value::as_str).unwrap_or_default() {
            "message_start" => {
                state.started = true;
                state.model = data
                    .pointer("/message/model")
                    .and_then(Value::as_str)
                    .unwrap_or(&cfg.model)
                    .to_string();
                state.response_id = data
                    .pointer("/message/id")
                    .and_then(Value::as_str)
                    .map(|id| format!("resp_{}", id.trim_start_matches("msg_")))
                    .unwrap_or_else(|| format!("resp_{}", uuid::Uuid::new_v4().simple()));
                state.input_tokens = data
                    .pointer("/message/usage/input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                vec![responses_event(
                    "response.created",
                    json!({ "response": response_stub(state) }),
                )]
            }
            "content_block_start" => {
                let Some(block) = data.get("content_block") else {
                    return Vec::new();
                };
                let index = data.get("index").and_then(Value::as_i64).unwrap_or(0);

                if block.get("type").and_then(Value::as_str) == Some("text") {
                    state.text_block_index = Some(index);
                    return Vec::new();
                }
                if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                    return Vec::new();
                }

                let mut events = close_text_item(state);
                let output_index = state.next_output_index;
                state.next_output_index += 1;
                state.tool_indices.insert(index, output_index);

                let call_id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                state
                    .tool_meta
                    .insert(output_index, (call_id.clone(), name.clone()));

                events.push(responses_event(
                    "response.output_item.added",
                    json!({
                        "output_index": output_index,
                        "item": {
                            "type": "function_call",
                            "id": format!("fc_{}", uuid::Uuid::new_v4().simple()),
                            "call_id": call_id,
                            "name": name,
                            "arguments": "",
                            "status": "in_progress"
                        }
                    }),
                ));
                events
            }
            "content_block_delta" => {
                let Some(delta) = data.get("delta") else {
                    return Vec::new();
                };
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let mut events = open_text_item(state);
                        let text = delta.get("text").and_then(Value::as_str).unwrap_or_default();
                        state.text_buffer.push_str(text);
                        events.push(responses_event(
                            "response.output_text.delta",
                            json!({
                                "item_id": state.text_item_id,
                                "output_index": state.text_output_index,
                                "content_index": 0,
                                "delta": text
                            }),
                        ));
                        events
                    }
                    Some("thinking_delta") => vec![responses_event(
                        "response.reasoning_summary_text.delta",
                        json!({
                            "delta": delta.get("thinking").and_then(Value::as_str).unwrap_or_default()
                        }),
                    )],
                    Some("input_json_delta") => {
                        let index = data.get("index").and_then(Value::as_i64).unwrap_or(0);
                        let Some(output_index) = state.tool_indices.get(&index).copied() else {
                            return Vec::new();
                        };
                        let partial = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        state
                            .tool_args
                            .entry(output_index)
                            .or_default()
                            .push_str(partial);
                        vec![responses_event(
                            "response.function_call_arguments.delta",
                            json!({ "output_index": output_index, "delta": partial }),
                        )]
                    }
                    _ => Vec::new(),
                }
            }
            "content_block_stop" => {
                let index = data.get("index").and_then(Value::as_i64).unwrap_or(-1);
                if state.text_block_index == Some(index) {
                    return close_text_item(state);
                }
                let Some(output_index) = state.tool_indices.get(&index).copied() else {
                    return Vec::new();
                };
                let (call_id, name) = state
                    .tool_meta
                    .get(&output_index)
                    .cloned()
                    .unwrap_or_default();
                vec![responses_event(
                    "response.output_item.done",
                    json!({
                        "output_index": output_index,
                        "item": {
                            "type": "function_call",
                            "call_id": call_id,
                            "name": name,
                            "arguments": state.tool_args.get(&output_index).cloned().unwrap_or_default(),
                            "status": "completed"
                        }
                    }),
                )]
            }
            "message_delta" => {
                if let Some(output) = data
                    .pointer("/usage/output_tokens")
                    .and_then(Value::as_u64)
                {
                    state.output_tokens = output;
                }
                Vec::new()
            }
            "error" => vec![canonical.clone()],
            _ => Vec::new(),
        }
    }

    fn encode_stream_done(&self, _cfg: &ModelConfig, state: &mut WireState) -> Vec<SseEvent> {
        if state.done_sent {
            return Vec::new();
        }
        state.done_sent = true;
        let mut events = close_text_item(state);
        events.push(responses_event(
            "response.completed",
            json!({
                "response": {
                    "id": state.response_id,
                    "object": "response",
                    "status": "completed",
                    "model": state.model,
                    "output": [],
                    "usage": {
                        "input_tokens": state.input_tokens,
                        "output_tokens": state.output_tokens,
                        "total_tokens": state.input_tokens + state.output_tokens
                    }
                }
            }),
        ));
        events
    }
}

fn responses_event(kind: &str, mut payload: Value) -> SseEvent {
    if let Some(map) = payload.as_object_mut() {
        map.insert("type".into(), Value::String(kind.to_string()));
    }
    SseEvent::new(kind, payload)
}

fn response_stub(state: &WireState) -> Value {
    json!({
        "id": state.response_id,
        "object": "response",
        "status": "in_progress",
        "model": state.model,
        "output": []
    })
}

fn open_text_item(state: &mut WireState) -> Vec<SseEvent> {
    if state.text_item_open {
        return Vec::new();
    }
    state.text_item_open = true;
    if state.text_item_id.is_empty() {
        state.text_item_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
    }
    let output_index = state.next_output_index;
    state.next_output_index += 1;
    state.text_output_index = output_index;

    vec![
        responses_event(
            "response.output_item.added",
            json!({
                "output_index": output_index,
                "item": {
                    "type": "message",
                    "id": state.text_item_id,
                    "status": "in_progress",
                    "role": "assistant",
                    "content": []
                }
            }),
        ),
        responses_event(
            "response.content_part.added",
            json!({
                "item_id": state.text_item_id,
                "output_index": output_index,
                "content_index": 0,
                "part": { "type": "output_text", "text": "", "annotations": [] }
            }),
        ),
    ]
}

fn close_text_item(state: &mut WireState) -> Vec<SseEvent> {
    if !state.text_item_open {
        return Vec::new();
    }
    state.text_item_open = false;
    let text = std::mem::take(&mut state.text_buffer);

    vec![
        responses_event(
            "response.output_text.done",
            json!({
                "item_id": state.text_item_id,
                "output_index": state.text_output_index,
                "content_index": 0,
                "text": text
            }),
        ),
        responses_event(
            "response.output_item.done",
            json!({
                "output_index": state.text_output_index,
                "item": {
                    "type": "message",
                    "id": state.text_item_id,
                    "status": "completed",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": text, "annotations": [] }]
                }
            }),
        ),
    ]
}

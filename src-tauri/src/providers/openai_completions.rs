use serde_json::{json, Map, Value};

use crate::domain::canonical::{blocks_to_text, content_to_text, CanonicalRequest, ContentBlock};
use crate::domain::model::ModelConfig;
use crate::error::{AppError, AppResult};

use super::{BlockKind, DeltaKind, ModelProvider, SseEvent, StreamState, WireState};

pub struct OpenaiCompletionsProvider;

/// 思考内容的字段名各家不同：DeepSeek 原生用 `reasoning_content`，OpenRouter 系（含部分网关上游）用 `reasoning`。
fn reasoning_text(value: &Value) -> Option<&str> {
    value
        .get("reasoning_content")
        .or_else(|| value.get("reasoning"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

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
        if let Some(reasoning) = reasoning_text(&message) {
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

        // OpenAI 的 prompt_tokens 已包含缓存命中，canonical 采用 Anthropic 语义（input 不含缓存），
        // 故拆成「未命中输入 + 缓存读」；两者相加等于上游 prompt_tokens，不会重复计数。
        let usage = raw.get("usage");
        let prompt_tokens = usage
            .and_then(|value| value.get("prompt_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cache_read_tokens = usage
            .and_then(|value| value.pointer("/prompt_tokens_details/cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let input_tokens = prompt_tokens.saturating_sub(cache_read_tokens);
        let output_tokens = usage
            .and_then(|value| value.get("completion_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);

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
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "cache_read_input_tokens": cache_read_tokens,
                "cache_creation_input_tokens": 0
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
            let cache_read = usage
                .pointer("/prompt_tokens_details/cached_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if let Some(prompt) = usage.get("prompt_tokens").and_then(Value::as_u64) {
                // prompt_tokens 含缓存命中，canonical 只留未命中部分（与 Anthropic 同口径）。
                state.input_tokens = prompt.saturating_sub(cache_read);
            }
            state.cache_read_tokens = cache_read;
            if let Some(completion) = usage.get("completion_tokens").and_then(Value::as_u64) {
                state.output_tokens = completion;
            }
        }

        let Some(choice) = data.pointer("/choices/0") else {
            return Ok(Vec::new());
        };

        let mut events: Vec<SseEvent> = Vec::new();

        if let Some(delta) = choice.get("delta") {
            if let Some(reasoning) = reasoning_text(delta) {
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

    fn decode_request(&self, raw: Value) -> AppResult<CanonicalRequest> {
        let object = raw
            .as_object()
            .ok_or_else(|| AppError::InvalidConfig("请求体必须是 JSON 对象".into()))?;

        let mut system_parts: Vec<String> = Vec::new();
        let mut messages: Vec<Value> = Vec::new();

        for message in object
            .get("messages")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let role = message.get("role").and_then(Value::as_str).unwrap_or("user");
            match role {
                "system" | "developer" => system_parts.push(content_to_text(
                    message.get("content").unwrap_or(&Value::Null),
                )),
                "tool" | "function" => messages.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": message.get("tool_call_id").and_then(Value::as_str).unwrap_or_default(),
                        "content": content_to_text(message.get("content").unwrap_or(&Value::Null))
                    }]
                })),
                "assistant" => {
                    let mut blocks: Vec<Value> = Vec::new();
                    if let Some(text) = message
                        .get("content")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                    {
                        blocks.push(json!({ "type": "text", "text": text }));
                    }
                    for call in message
                        .get("tool_calls")
                        .and_then(Value::as_array)
                        .map(Vec::as_slice)
                        .unwrap_or_default()
                    {
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": call.get("id").and_then(Value::as_str).unwrap_or_default(),
                            "name": call.pointer("/function/name").and_then(Value::as_str).unwrap_or_default(),
                            "input": parse_arguments(call.pointer("/function/arguments"))
                        }));
                    }
                    if !blocks.is_empty() {
                        messages.push(json!({ "role": "assistant", "content": blocks }));
                    }
                }
                _ => {
                    let mut blocks: Vec<Value> = Vec::new();
                    match message.get("content") {
                        Some(Value::String(text)) => {
                            if !text.is_empty() {
                                blocks.push(json!({ "type": "text", "text": text }));
                            }
                        }
                        Some(Value::Array(parts)) => {
                            for part in parts {
                                match part.get("type").and_then(Value::as_str) {
                                    Some("text") => blocks.push(json!({
                                        "type": "text",
                                        "text": part.get("text").and_then(Value::as_str).unwrap_or_default()
                                    })),
                                    Some("image_url") => {
                                        let url = part
                                            .pointer("/image_url/url")
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
                        messages.push(json!({ "role": "user", "content": blocks }));
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
        for key in ["max_tokens", "max_completion_tokens"] {
            if let Some(value) = object.get(key).and_then(Value::as_u64) {
                canonical.insert("max_tokens".into(), json!(value));
                break;
            }
        }
        for key in ["temperature", "top_p", "stream"] {
            if let Some(value) = object.get(key) {
                canonical.insert(key.into(), value.clone());
            }
        }
        match object.get("stop") {
            Some(Value::String(text)) => {
                canonical.insert("stop_sequences".into(), json!([text]));
            }
            Some(Value::Array(items)) => {
                canonical.insert("stop_sequences".into(), Value::Array(items.clone()));
            }
            _ => {}
        }
        if let Some(tools) = object.get("tools").and_then(Value::as_array) {
            let converted: Vec<Value> = tools
                .iter()
                .filter_map(|tool| {
                    let function = tool.get("function")?;
                    Some(json!({
                        "name": function.get("name").and_then(Value::as_str).unwrap_or_default(),
                        "description": function.get("description").cloned().unwrap_or(Value::Null),
                        "input_schema": function.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object" }))
                    }))
                })
                .collect();
            if !converted.is_empty() {
                canonical.insert("tools".into(), Value::Array(converted));
            }
        }

        CanonicalRequest::parse(Value::Object(canonical))
    }

    fn encode_response(&self, cfg: &ModelConfig, canonical: &Value) -> AppResult<Value> {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_calls: Vec<Value> = Vec::new();

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
                Some("tool_use") => tool_calls.push(json!({
                    "id": block.get("id").cloned().unwrap_or_else(|| json!("")),
                    "type": "function",
                    "function": {
                        "name": block.get("name").cloned().unwrap_or_else(|| json!("")),
                        "arguments": serde_json::to_string(block.get("input").unwrap_or(&json!({})))
                            .unwrap_or_else(|_| "{}".into())
                    }
                })),
                _ => {}
            }
        }

        let mut message = Map::new();
        message.insert("role".into(), Value::String("assistant".into()));
        message.insert(
            "content".into(),
            if text.is_empty() {
                Value::Null
            } else {
                Value::String(text)
            },
        );
        if !reasoning.is_empty() {
            message.insert("reasoning_content".into(), Value::String(reasoning));
        }
        if !tool_calls.is_empty() {
            message.insert("tool_calls".into(), Value::Array(tool_calls));
        }

        let finish = map_finish_reason(canonical.get("stop_reason").and_then(Value::as_str));
        let input_tokens = canonical
            .pointer("/usage/input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cache_read_tokens = canonical
            .pointer("/usage/cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let output_tokens = canonical
            .pointer("/usage/output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        // 回写给 OpenAI 客户端时把缓存读并回 prompt_tokens（OpenAI 语义：prompt_tokens 含缓存）。
        let prompt_tokens = input_tokens + cache_read_tokens;

        Ok(json!({
            "id": canonical.get("id").cloned()
                .unwrap_or_else(|| json!(format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()))),
            "object": "chat.completion",
            "created": chrono::Utc::now().timestamp(),
            "model": cfg.model,
            "choices": [{
                "index": 0,
                "message": Value::Object(message),
                "finish_reason": finish
            }],
            "usage": {
                "prompt_tokens": prompt_tokens,
                "completion_tokens": output_tokens,
                "total_tokens": prompt_tokens + output_tokens,
                "prompt_tokens_details": { "cached_tokens": cache_read_tokens }
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
                    .unwrap_or_default()
                    .to_string();
                vec![chunk(state, json!({ "role": "assistant" }), None)]
            }
            "content_block_start" => {
                let Some(block) = data.get("content_block") else {
                    return Vec::new();
                };
                if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                    return Vec::new();
                }
                let index = data.get("index").and_then(Value::as_i64).unwrap_or(0);
                let wire = state.next_output_index;
                state.next_output_index += 1;
                state.tool_indices.insert(index, wire);
                vec![chunk(
                    state,
                    json!({ "tool_calls": [{
                        "index": wire,
                        "id": block.get("id").and_then(Value::as_str).unwrap_or_default(),
                        "type": "function",
                        "function": {
                            "name": block.get("name").and_then(Value::as_str).unwrap_or_default(),
                            "arguments": ""
                        }
                    }] }),
                    None,
                )]
            }
            "content_block_delta" => {
                let Some(delta) = data.get("delta") else {
                    return Vec::new();
                };
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let text = delta.get("text").and_then(Value::as_str).unwrap_or_default();
                        state.text_buffer.push_str(text);
                        vec![chunk(state, json!({ "content": text }), None)]
                    }
                    Some("thinking_delta") => vec![chunk(
                        state,
                        json!({
                            "reasoning_content": delta.get("thinking").and_then(Value::as_str).unwrap_or_default()
                        }),
                        None,
                    )],
                    Some("input_json_delta") => {
                        let index = data.get("index").and_then(Value::as_i64).unwrap_or(0);
                        let wire = state.tool_indices.get(&index).copied().unwrap_or(0);
                        let partial = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        state.tool_args.entry(wire).or_default().push_str(partial);
                        vec![chunk(
                            state,
                            json!({ "tool_calls": [{ "index": wire, "function": { "arguments": partial } }] }),
                            None,
                        )]
                    }
                    _ => Vec::new(),
                }
            }
            "message_delta" => {
                let reason = data
                    .pointer("/delta/stop_reason")
                    .and_then(Value::as_str);
                vec![chunk(state, json!({}), Some(map_finish_reason(reason)))]
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
        vec![SseEvent::raw("[DONE]")]
    }
}

pub(crate) fn parse_arguments(value: Option<&Value>) -> Value {
    match value.and_then(Value::as_str) {
        Some(text) => serde_json::from_str::<Value>(text).unwrap_or_else(|_| json!({})),
        None => json!({}),
    }
}

pub(crate) fn data_url_to_source(url: &str) -> Option<Value> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    if !meta.contains("base64") {
        return None;
    }
    let media_type = meta.split(';').next().unwrap_or("image/png");
    Some(json!({ "type": "base64", "media_type": media_type, "data": data }))
}

fn map_finish_reason(reason: Option<&str>) -> &'static str {
    match reason {
        Some("tool_use") => "tool_calls",
        Some("max_tokens") => "length",
        _ => "stop",
    }
}

fn chunk(state: &WireState, delta: Value, finish_reason: Option<&str>) -> SseEvent {
    let id = if state.response_id.is_empty() {
        format!("chatcmpl-{}", uuid::Uuid::new_v4().simple())
    } else {
        state.response_id.clone()
    };
    SseEvent::new(
        "",
        json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": state.model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }]
        }),
    )
}

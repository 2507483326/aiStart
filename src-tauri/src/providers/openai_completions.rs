use serde_json::{json, Map, Value};

use crate::domain::canonical::{
    blocks_to_text, content_to_text, CanonicalRequest, ContentBlock, MaxTokensField, SystemPrompt,
};
use crate::domain::model::{ModelConfig, ModelFormat};
use crate::error::{AppError, AppResult};

use super::wire::{self, Fill};
use super::{ModelProvider, SseEvent, StreamState, WireState};

pub struct OpenaiCompletionsProvider;

/// 上游承载思考的字段按协议表依次尝试（DeepSeek 原生用 `reasoning_content`，OpenRouter 系用 `reasoning`）。
fn reasoning_text(value: &Value) -> Option<&str> {
    wire::profile(ModelFormat::OpenaiCompletions)
        .reasoning_fields
        .iter()
        .find_map(|field| value.get(*field).and_then(Value::as_str))
        .filter(|text| !text.is_empty())
}

/// 结构化思考（OpenRouter 系还会给 `reasoning_details[]`）：只有前面那些字符串字段都没有时才兜底，
/// 免得同一段思考被算两遍。
fn reasoning_details_text(value: &Value) -> Option<String> {
    let items = value.get("reasoning_details")?.as_array()?;
    let text: String = items
        .iter()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect();
    (!text.is_empty()).then_some(text)
}

/// 只取思考：字符串字段优先，结构化字段兜底。
fn thinking_text(value: &Value) -> Option<String> {
    reasoning_text(value)
        .map(str::to_string)
        .or_else(|| reasoning_details_text(value))
}

/// 用量口径：OpenAI 的 `prompt_tokens` 含缓存命中，规范采用 Anthropic 语义（input 不含缓存），
/// 所以拆成「未命中输入 + 缓存读」；思考 token 单独记，不计入 output。
fn apply_usage(state: &mut StreamState, usage: &Value) {
    let cache_read = usage
        .pointer("/prompt_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if let Some(prompt) = usage.get("prompt_tokens").and_then(Value::as_u64) {
        state.input_tokens = prompt.saturating_sub(cache_read);
    }
    state.cache_read_tokens = cache_read;
    if let Some(completion) = usage.get("completion_tokens").and_then(Value::as_u64) {
        state.output_tokens = completion;
    }
    if let Some(reasoning) = usage
        .pointer("/completion_tokens_details/reasoning_tokens")
        .and_then(Value::as_u64)
    {
        state.reasoning_tokens = reasoning;
    }
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
            let name = choice
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            json!({ "type": "function", "function": { "name": name } })
        }
        _ => Value::String("auto".into()),
    })
}

/// 客户端有没有给出输出上限（`max_tokens` / `max_completion_tokens`；`null` 或非法值都算没给）。
fn has_output_limit(payload: &Map<String, Value>) -> bool {
    ["max_tokens", "max_completion_tokens"]
        .iter()
        .any(|key| payload.get(*key).is_some_and(|value| value.as_u64().is_some()))
}

/// 把规范里的 system 写成 completions 的 system 消息：清掉客户端原有的 system/developer 消息，
/// 在队首放一条（位置与重建路径一致）。只在过滤器改写过后调用——此时 system 是网关接管的字段，
/// 与重建路径一样按纯文本落地。
fn write_system_message(payload: &mut Map<String, Value>, system: Option<&SystemPrompt>) {
    let text = system.map(SystemPrompt::plain_text).unwrap_or_default();
    let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    messages.retain(|message| {
        !matches!(
            message.get("role").and_then(Value::as_str),
            Some("system" | "developer")
        )
    });
    if !text.is_empty() {
        messages.insert(0, json!({ "role": "system", "content": text }));
    }
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
                        pending_parts
                            .push(json!({ "type": "image_url", "image_url": { "url": url } }));
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
        // 输出上限：客户端原本用 max_completion_tokens 就原样回它——OpenAI 的 max_tokens 已弃用，
        // 且与 o 系列不兼容（上游会直接报错）。
        let max_tokens = body
            .max_tokens
            .unwrap_or(crate::domain::model::DEFAULT_MAX_TOKENS);
        let max_tokens_field = match body.canonical.max_tokens_field {
            Some(MaxTokensField::MaxCompletionTokens) => "max_completion_tokens",
            _ => "max_tokens",
        };
        payload.insert(max_tokens_field.into(), json!(max_tokens));

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

        // 其余标量字段按协议表落地（同名写入 / 改名写入 / 显式丢弃）。
        let canonical = serde_json::to_value(body)
            .map_err(|error| AppError::Message(format!("规范请求序列化失败: {error}")))?;
        wire::apply_common_fields(
            &mut payload,
            &canonical,
            wire::profile(ModelFormat::OpenaiCompletions),
            Fill::Overwrite,
        );

        // 上游只有收到 include_usage 才会在流末尾上报 usage（OpenAI 官方接口如此），否则本地与客户端都拿不到。
        if body.stream && body.canonical.include_usage {
            payload.insert("stream_options".into(), json!({ "include_usage": true }));
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
        if let Some(reasoning) = thinking_text(&message) {
            content.push(json!({ "type": "thinking", "thinking": reasoning }));
        }
        if let Some(text) = message.get("content").and_then(Value::as_str) {
            if !text.is_empty() {
                content.push(json!({ "type": "text", "text": text }));
            }
        }
        // 拒答内容按正文给出：规范里没有 refusal 块，丢掉会让客户端只看到一片空白。
        if let Some(refusal) = message.get("refusal").and_then(Value::as_str) {
            if !refusal.is_empty() {
                content.push(json!({ "type": "text", "text": refusal }));
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

        let stop_reason = wire::stop_reason_from_finish_reason(finish_reason);
        let usage = raw.get("usage");
        let input_tokens = usage
            .and_then(|value| value.get("prompt_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .saturating_sub(
                usage
                    .and_then(|value| value.pointer("/prompt_tokens_details/cached_tokens"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            );
        let cache_read_tokens = usage
            .and_then(|value| value.pointer("/prompt_tokens_details/cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let output_tokens = usage
            .and_then(|value| value.get("completion_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let reasoning_tokens = usage
            .and_then(|value| value.pointer("/completion_tokens_details/reasoning_tokens"))
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
                "cache_creation_input_tokens": 0,
                "output_tokens_details": { "thinking_tokens": reasoning_tokens }
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
            apply_usage(state, usage);
        }

        // OpenAI 系的流内错误是「没有 choices、只有一个 error 对象」的分片：
        // 以前会被当成空分片丢掉，客户端什么都看不到、明细还记成成功。
        if let Some(error) = data.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("上游返回错误");
            state.upstream_error = Some(message.to_string());
            return Ok(state.error("api_error", message));
        }

        let Some(choice) = data.pointer("/choices/0") else {
            return Ok(Vec::new());
        };

        let mut events: Vec<SseEvent> = Vec::new();

        if let Some(delta) = choice.get("delta") {
            if let Some(reasoning) = thinking_text(delta) {
                events.extend(state.thinking_delta(&reasoning));
            }

            if let Some(text) = delta
                .get("content")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                events.extend(state.text_delta(text));
                state.output_tokens += 1;
            }

            // 拒答（content_filter 场景）按正文转发，否则客户端只看到一段空白。
            if let Some(refusal) = delta
                .get("refusal")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                events.extend(state.text_delta(refusal));
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
                        events.extend(state.tool_start(index, &id, &name));
                    }

                    if let Some(partial) = call
                        .pointer("/function/arguments")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                    {
                        events.extend(state.tool_args(index, partial));
                    }
                }
            }
        }

        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            state.upstream_ended = true;
            let stop_reason = wire::stop_reason_from_finish_reason(Some(reason));
            events.extend(state.finish(&stop_reason));
        }

        Ok(events)
    }

    fn decode_stream_done(
        &self,
        _cfg: &ModelConfig,
        state: &mut StreamState,
    ) -> AppResult<Vec<SseEvent>> {
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
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user");
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
                // 记住客户端用的是哪个字段：二者语义不同（max_completion_tokens 才兼容 o 系列），
                // 上游编码时按原字段回写。
                if key == "max_completion_tokens" {
                    canonical.insert(
                        "_canonical".into(),
                        json!({ "max_tokens_field": "max_completion_tokens" }),
                    );
                }
                break;
            }
        }
        for key in [
            "temperature",
            "top_p",
            "stream",
            "store",
            "metadata",
            "response_format",
            "parallel_tool_calls",
            "reasoning_effort",
            "service_tier",
            "n",
        ] {
            if let Some(value) = object.get(key) {
                canonical.insert(key.into(), value.clone());
            }
        }
        // stream_options.include_usage 决定流式响应末尾是否要带 usage 事件，需要透传到上游与本端出站。
        if object
            .get("stream_options")
            .and_then(|options| options.get("include_usage"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let canonical_only = canonical
                .entry("_canonical".to_string())
                .or_insert_with(|| json!({}));
            canonical_only["include_usage"] = Value::Bool(true);
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

    /// 同协议（Completions → Completions）免转换快路：以客户端原文为底，只改必须改的三处——
    /// 上游模型名、被过滤器改写的 system、规范内部字段。客户端的扩展键（OpenRouter 的
    /// `provider`/`route`、message 的 `name`、`logit_bias`、`stop` 的原字段名……）原样带给上游。
    fn encode_request_passthrough(
        &self,
        cfg: &ModelConfig,
        req: &CanonicalRequest,
    ) -> AppResult<Option<Value>> {
        let Some(client) = req.client_raw().and_then(Value::as_object) else {
            return Ok(None);
        };
        let mut payload = client.clone();

        payload.insert("model".into(), Value::String(cfg.model.clone()));
        // 客户端两个上限字段都没给时补默认值（与重建路径一致，否则同协议请求会变成不限长）。
        if !has_output_limit(&payload) {
            payload.insert(
                "max_tokens".into(),
                json!(crate::domain::model::DEFAULT_MAX_TOKENS),
            );
        }
        // 只有过滤器注入过系统提示词才重写 system 消息；没注入过就完全保留客户端的原有写法
        // （多条 system 消息、developer 角色、块数组里的 cache_control 都不动）。
        if req.is_dirty("system") {
            write_system_message(&mut payload, req.body().system.as_ref());
        }
        // 规范内部字段不属于任何线上协议：客户端碰巧带了同名键也删掉。
        payload.remove(wire::CANONICAL_ONLY_KEY);
        Ok(Some(Value::Object(payload)))
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
                Some("text") => text.push_str(
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                ),
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

        let finish = wire::finish_reason(canonical.get("stop_reason").and_then(Value::as_str));
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
        let reasoning_tokens = canonical
            .pointer("/usage/output_tokens_details/thinking_tokens")
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
                "prompt_tokens_details": { "cached_tokens": cache_read_tokens },
                "completion_tokens_details": { "reasoning_tokens": reasoning_tokens }
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
                        let text = delta
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
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
                let reason = data.pointer("/delta/stop_reason").and_then(Value::as_str);
                // 记下规范 stop_reason：流末尾的 usage 分片与落库都按它取口径。
                state.stop_reason = reason.map(str::to_string);
                vec![chunk(state, json!({}), Some(wire::finish_reason(reason)))]
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
        let mut events = Vec::new();
        // 客户端声明了 stream_options.include_usage：按 OpenAI 约定在 [DONE] 之前补一个只带 usage 的分片。
        if state.include_usage {
            events.push(usage_chunk(state));
        }
        events.push(SseEvent::raw("[DONE]"));
        events
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

fn chunk(state: &WireState, delta: Value, finish_reason: Option<&str>) -> SseEvent {
    SseEvent::new(
        "",
        json!({
            "id": chunk_id(state),
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": state.model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }]
        }),
    )
}

/// 流末尾的 usage 分片：choices 为空数组，只带整次调用的 token 统计（OpenAI 口径，prompt_tokens 含缓存命中）。
fn usage_chunk(state: &WireState) -> SseEvent {
    let prompt_tokens = state.input_tokens + state.cache_read_tokens;
    SseEvent::new(
        "",
        json!({
            "id": chunk_id(state),
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": state.model,
            "choices": [],
            "usage": {
                "prompt_tokens": prompt_tokens,
                "completion_tokens": state.output_tokens,
                "total_tokens": prompt_tokens + state.output_tokens,
                "prompt_tokens_details": { "cached_tokens": state.cache_read_tokens },
                "completion_tokens_details": { "reasoning_tokens": state.reasoning_tokens }
            }
        }),
    )
}

fn chunk_id(state: &WireState) -> String {
    if state.response_id.is_empty() {
        format!("chatcmpl-{}", uuid::Uuid::new_v4().simple())
    } else {
        state.response_id.clone()
    }
}

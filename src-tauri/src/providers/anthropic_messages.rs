use serde_json::{json, Map, Value};

use crate::domain::canonical::CanonicalRequest;
use crate::domain::model::ModelConfig;
use crate::error::{AppError, AppResult};

use super::wire::{self, Fill};
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

    /// 透传 = 客户端原始报文（`raw`，保住 `tools` 上的 cache_control 等协议扩展字段）
    /// 加上规范字段的显式覆盖：
    ///
    /// * Anthropic 不认识的键（`_canonical`、`store`、`n` 等）删掉；
    /// * 与规范形状不同的字段（`response_format` / `reasoning_effort` / `parallel_tool_calls` / `metadata`）
    ///   按 Anthropic 写法重新落一遍（`fill = IfAbsent`：报文里已有的原样字段不动）。
    fn encode_request(&self, cfg: &ModelConfig, req: &CanonicalRequest) -> AppResult<Value> {
        let body = req.body();
        let canonical = serde_json::to_value(body)
            .map_err(|error| AppError::Message(format!("规范请求序列化失败: {error}")))?;

        let mut payload = req.raw().clone();
        let object = payload
            .as_object_mut()
            .ok_or_else(|| AppError::InvalidConfig("请求体必须是 JSON 对象".into()))?;

        wire::apply_common_fields(
            object,
            &canonical,
            wire::profile(crate::domain::model::ModelFormat::AnthropicMessages),
            Fill::IfAbsent,
        );

        object.insert("model".into(), Value::String(cfg.model.clone()));
        if !object.contains_key("max_tokens") {
            object.insert(
                "max_tokens".into(),
                json!(crate::domain::model::DEFAULT_MAX_TOKENS),
            );
        }

        // metadata / response_format / reasoning_effort / parallel_tool_calls 的规范名已由
        // `apply_common_fields` 从报文里删掉（Anthropic 没有这些键），这里按 Anthropic 写法补回去。
        if let Some(metadata) = metadata(body.metadata.as_ref()) {
            object.insert("metadata".into(), metadata);
        }
        match tool_choice(body.tool_choice.as_ref(), body.parallel_tool_calls) {
            Some(choice) => {
                object.insert("tool_choice".into(), choice);
            }
            None => {
                if tool_choice_is_openai_shaped(object.get("tool_choice")) {
                    object.remove("tool_choice");
                }
            }
        }

        // response_format 与 reasoning_effort 在 Anthropic 里都归到 output_config 下。
        let format = body.response_format.as_ref().and_then(anthropic_format);
        let effort = body.reasoning_effort.clone();
        if format.is_some() || effort.is_some() {
            let config = output_config(object);
            if let Some(format) = format {
                config.insert("format".into(), format);
            }
            if let Some(effort) = effort {
                config.insert("effort".into(), Value::String(effort));
            }
        }

        Ok(payload)
    }

    fn decode_response(&self, _cfg: &ModelConfig, raw: &Value) -> AppResult<Value> {
        Ok(raw.clone())
    }

    /// 客户端原始报文就是规范形状，只需要把两个「藏起来的」字段抬到规范字段上，
    /// 跨协议转发（Anthropic 入站 → OpenAI 出站）时才不会丢掉思考档位与并行开关：
    /// `output_config.effort` → `reasoning_effort`，`tool_choice.disable_parallel_tool_use`
    /// → `parallel_tool_calls`（取反）。
    fn decode_request(&self, raw: Value) -> AppResult<CanonicalRequest> {
        CanonicalRequest::parse(raw)?.map_raw(|value| {
            let effort = value
                .pointer("/output_config/effort")
                .and_then(Value::as_str)
                .map(str::to_string);
            let parallel = value
                .pointer("/tool_choice/disable_parallel_tool_use")
                .and_then(Value::as_bool);
            let Some(object) = value.as_object_mut() else {
                return Ok(());
            };
            if let Some(effort) = effort {
                object.insert("reasoning_effort".into(), Value::String(effort));
            }
            if parallel == Some(true) {
                object.insert("parallel_tool_calls".into(), Value::Bool(false));
            }
            Ok(())
        })
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
                read_usage(state, data.pointer("/message/usage"));
            }
            "message_delta" => {
                // 文档口径：message_delta 的 usage 是累计值，且同样带 input / 缓存字段。
                read_usage(state, data.get("usage"));
                if let Some(stop) = data.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    state.stop_reason = Some(wire::stop_reason_from_anthropic(Some(stop)));
                }
            }
            "message_stop" | "error" => {
                state.finished = true;
                state.upstream_ended = true;
                // 上游在流里报的错误以前只当成一个事件转发，明细却记成成功。
                if name == "error" {
                    state.upstream_error = Some(
                        data.pointer("/error/message")
                            .and_then(Value::as_str)
                            .unwrap_or("上游返回错误")
                            .to_string(),
                    );
                }
            }
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

/// 累计口径：只在字段出现时覆盖（message_start 给 input/缓存，message_delta 给 output/累计值）。
fn read_usage(state: &mut StreamState, usage: Option<&Value>) {
    let Some(usage) = usage else {
        return;
    };
    if let Some(tokens) = usage.get("input_tokens").and_then(Value::as_u64) {
        state.input_tokens = tokens;
    }
    if let Some(tokens) = usage.get("output_tokens").and_then(Value::as_u64) {
        state.output_tokens = tokens;
    }
    if let Some(tokens) = usage
        .get("cache_read_input_tokens")
        .and_then(Value::as_u64)
    {
        state.cache_read_tokens = tokens;
    }
    if let Some(tokens) = usage
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64)
    {
        state.cache_write_tokens = tokens;
    }
    if let Some(tokens) = usage
        .pointer("/output_tokens_details/thinking_tokens")
        .and_then(Value::as_u64)
    {
        state.reasoning_tokens = tokens;
    }
}

/// Anthropic 的 `metadata` 只接受 `user_id` 一个字段。
fn metadata(value: Option<&Value>) -> Option<Value> {
    let user_id = value?.get("user_id")?.as_str()?;
    Some(json!({ "user_id": user_id }))
}

/// 规范（OpenAI 口径）的 `tool_choice` → Anthropic 写法。
/// `parallel_tool_calls == false` 在 Anthropic 里是 `tool_choice.disable_parallel_tool_use`（语义相反）。
fn tool_choice(choice: Option<&Value>, parallel: Option<bool>) -> Option<Value> {
    let mut converted = match choice {
        Some(Value::String(kind)) => match kind.as_str() {
            "auto" => json!({ "type": "auto" }),
            "none" => json!({ "type": "none" }),
            "required" => json!({ "type": "any" }),
            _ => return None,
        },
        Some(Value::Object(source)) => match source.get("type").and_then(Value::as_str) {
            // OpenAI 写法的 `{type:function,function:{name}}` → Anthropic 的 `{type:tool,name}`。
            Some("function") => {
                let name = source
                    .get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                json!({ "type": "tool", "name": name })
            }
            // OpenAI 的 forced 叫 required，Anthropic 叫 any（其余扩展字段原样保留）。
            Some("required") => {
                let mut converted = source.clone();
                converted.insert("type".into(), Value::String("any".into()));
                Value::Object(converted)
            }
            // 已经是 Anthropic 写法（auto / any / none / tool），原样保留。
            _ => Value::Object(source.clone()),
        },
        _ => {
            if parallel == Some(false) {
                json!({ "type": "auto" })
            } else {
                return None;
            }
        }
    };

    if parallel == Some(false) {
        if let Some(object) = converted.as_object_mut() {
            if object.get("type").and_then(Value::as_str) != Some("none") {
                object.insert("disable_parallel_tool_use".into(), Value::Bool(true));
            }
        }
    }
    Some(converted)
}

/// 报文里残留的 `tool_choice` 是不是 OpenAI 写法（转换不出来时不该把它发给 Anthropic）。
fn tool_choice_is_openai_shaped(value: Option<&Value>) -> bool {
    match value {
        Some(Value::String(_)) => true,
        Some(Value::Object(object)) => object.contains_key("function"),
        _ => false,
    }
}

/// 取（必要时新建）`output_config` 对象。
fn output_config(object: &mut Map<String, Value>) -> &mut Map<String, Value> {
    let entry = object
        .entry("output_config".to_string())
        .or_insert_with(|| json!({}));
    if !entry.is_object() {
        *entry = json!({});
    }
    entry
        .as_object_mut()
        .expect("output_config 刚被规范成对象")
}

/// 规范的 `response_format` → Anthropic 的 `output_config.format`。
/// Anthropic 只支持 json_schema 一种结构化输出；text / json_object 没有对应语义。
fn anthropic_format(value: &Value) -> Option<Value> {
    let schema = value.pointer("/json_schema/schema")?;
    Some(json!({ "type": "json_schema", "schema": schema }))
}

use serde_json::{json, Map, Value};

use crate::domain::canonical::{
    CanonicalResponseFormat, CanonicalToolChoice, CanonicalRequest,
};
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
        match body.tool_choice.as_ref() {
            Some(choice) => {
                object.insert("tool_choice".into(), tool_choice(choice, body.parallel_tool_calls).expect("类型化枚举必能转出 Anthropic 写法"));
            }
            None => {
                // 没有指定策略但禁用了并行工具调用：合成 auto + 开关，语义才能落到 Anthropic。
                // （body.tool_choice 为 None 时报文里不会有残留的 tool_choice：
                // 非法形状在 decode_request 已 400，合法形状都会归一成 Some。）
                if body.parallel_tool_calls == Some(false) {
                    object.insert(
                        "tool_choice".into(),
                        json!({ "type": "auto", "disable_parallel_tool_use": true }),
                    );
                }
            }
        }

        // response_format 与 reasoning_effort 在 Anthropic 里都归到 output_config 下。
        let format = body.response_format.as_ref().and_then(anthropic_format);
        // 客户端原文带 thinking（预算式思考）时不再叠档位：reasoning_effort 是从预算
        // 有损映射来的（见 decode_request），两个思考开关同时下发会被上游拒。
        // 先算好要写什么再碰报文——没有可写的就不建 output_config 空对象。
        let has_thinking_budget = object
            .get("thinking")
            .is_some_and(|thinking| thinking.get("budget_tokens").is_some());
        let effort = if has_thinking_budget {
            None
        } else {
            body.reasoning_effort.clone()
        };
        if let Some(format) = format {
            let config = output_config(object);
            config.insert("format".into(), format);
            if let Some(effort) = effort {
                config.insert("effort".into(), Value::String(effort));
            }
        } else if let Some(effort) = effort {
            let config = output_config(object);
            config.insert("effort".into(), Value::String(effort));
        }

        Ok(payload)
    }

    fn decode_response(&self, _cfg: &ModelConfig, raw: &Value) -> AppResult<Value> {
        Ok(raw.clone())
    }

    /// Anthropic 报文里藏着几个跨协议字段，先归一成规范形状再 parse（类型化字段只认
    /// 规范形状，parse 之后再 map_raw 就来不及了）：
    ///
    /// * `tool_choice`（`{type:"auto"/"any"/"none"/"tool",…}`）→ OpenAI 口径规范形；
    /// * `output_config.format` → `response_format`（A2：以前跨协议转发时静默丢失）；
    /// * `output_config.effort` → `reasoning_effort`；
    /// * `thinking`（预算式）→ `reasoning_effort` 的有损映射（档位按预算粗分），
    ///   原键删除——它不属于其他协议，留着会在跨协议时原样漏出去；
    /// * `tool_choice.disable_parallel_tool_use` → `parallel_tool_calls`（取反）。
    fn decode_request(&self, raw: Value) -> AppResult<CanonicalRequest> {
        let mut value = raw;
        normalize_anthropic_request(&mut value)?;
        CanonicalRequest::parse(value)
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
        /// 按事件名推进流状态。返回「本协议认不认识这个名字」——不认识时**一动 `state` 都没动**，
        /// 调用方因此可以拿另一个候选名重试（L1）。
        fn apply(name: &str, data: &Value, state: &mut StreamState) -> bool {
            match name {
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
                _ => return false,
            }

            true
        }

        // 事件名有两处来源：SSE 的 `event:` 行，与正文里的 `type` 字段。信封优先，但信封写的
        // 名字本协议不认识、而正文的 `type` 认识时，采用正文的名字（L1）——有些中转会把每一帧的
        // 信封统一写成别的名字，正文的 `type` 才是权威。事件名也跟着改成认出来的那个：
        // Anthropic 客户端就是按事件名解析的，把不认识的信封名原样发过去等于丢帧。
        let data_type = data.get("type").and_then(Value::as_str).unwrap_or("");
        let mut name = if event.is_empty() {
            if data_type.is_empty() {
                "message"
            } else {
                data_type
            }
        } else {
            event
        };
        // 两个候选都不认识时，仍按信封名原样转发：Anthropic 的帧总是要发给客户端的，
        // 网关照旧只做能理解的那部分状态推进（与改动前一致）。
        if !apply(name, data, state) && !data_type.is_empty() && data_type != name {
            if apply(data_type, data, state) {
                name = data_type;
            }
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

/// 规范 `tool_choice`（类型化）→ Anthropic 写法。
/// `parallel_tool_calls == false` 在 Anthropic 里是 `tool_choice.disable_parallel_tool_use`（语义相反）。
fn tool_choice(choice: &CanonicalToolChoice, parallel: Option<bool>) -> Option<Value> {
    let mut converted = match choice {
        CanonicalToolChoice::Auto => json!({ "type": "auto" }),
        CanonicalToolChoice::None => json!({ "type": "none" }),
        CanonicalToolChoice::Required => json!({ "type": "any" }),
        CanonicalToolChoice::Tool { name } => json!({ "type": "tool", "name": name }),
    };

    if parallel == Some(false) && converted["type"] != "none" {
        if let Some(object) = converted.as_object_mut() {
            object.insert("disable_parallel_tool_use".into(), Value::Bool(true));
        }
    }
    Some(converted)
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

/// 规范 `response_format`（类型化）→ Anthropic 的 `output_config.format`。
/// Anthropic 只支持 json_schema 一种结构化输出；text / json_object 没有对应语义。
fn anthropic_format(value: &CanonicalResponseFormat) -> Option<Value> {
    let CanonicalResponseFormat::JsonSchema { schema, .. } = value else {
        return None;
    };
    Some(json!({ "type": "json_schema", "schema": schema }))
}

/// 把 Anthropic 入站报文归一成规范形状（原地改写，详见 `decode_request` 的注释）。
/// 只做「抬升」：把藏在嵌套里的跨协议字段补到规范字段上，不删 `thinking` / `output_config`
/// 本身——同协议转发时客户端原文（raw）仍是报文底稿，这两个键必须原样保留。
fn normalize_anthropic_request(value: &mut Value) -> AppResult<()> {
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };

    // tool_choice：Anthropic 形（{type:any/tool/...}，可带 disable_parallel_tool_use）→ 规范形。
    if let Some(choice) = object.get("tool_choice").cloned() {
        let parallel_disabled = choice
            .get("disable_parallel_tool_use")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if let Some(kind) = choice.get("type").and_then(Value::as_str) {
            let canonical = match kind {
                "auto" => CanonicalToolChoice::Auto,
                "none" => CanonicalToolChoice::None,
                // Anthropic 的 any == OpenAI 的 required（扩展字段不保真，语义口径统一）。
                "any" | "required" => CanonicalToolChoice::Required,
                "tool" => CanonicalToolChoice::Tool {
                    name: choice
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                },
                other => {
                    return Err(AppError::InvalidConfig(format!(
                        "tool_choice 形状非法（未知的 type {other:?}）"
                    )));
                }
            };
            object.insert("tool_choice".into(), canonical.to_json());
            if parallel_disabled {
                object.insert("parallel_tool_calls".into(), Value::Bool(false));
            }
        }
    }

    // output_config：format → response_format；effort / thinking → reasoning_effort。
    // effort 是显式档位，优先于预算式的 thinking（二选一时）。
    let effort = object
        .get("output_config")
        .and_then(|config| config.get("effort"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let thinking_budget = object
        .get("thinking")
        .and_then(|thinking| thinking.get("budget_tokens"))
        .and_then(Value::as_u64);
    if let Some(format) = object
        .get("output_config")
        .and_then(|config| config.get("format"))
        .cloned()
    {
        let schema = format.get("schema").cloned().ok_or_else(|| {
            AppError::InvalidConfig("output_config.format 形状非法: 缺少 schema".into())
        })?;
        object.insert(
            "response_format".into(),
            CanonicalResponseFormat::JsonSchema {
                // Anthropic 的 format 只有 type+schema，没有 name；跨协议到 OpenAI 系时补个缺省名。
                name: format
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("response")
                    .to_string(),
                description: None,
                schema,
                strict: None,
            }
            .to_json(),
        );
    }
    let effort = match (effort, thinking_budget) {
        (Some(effort), _) => Some(effort),
        (None, Some(budget)) => Some(effort_from_budget(budget).to_string()),
        (None, None) => None,
    };
    if let Some(effort) = effort {
        object.insert("reasoning_effort".into(), Value::String(effort));
    }

    Ok(())
}

/// thinking 预算 → 档位的有损映射：预算是连续值，档位只有三档，按官方参考值粗分。
/// （32k 是 effort=high 的典型预算，8k 左右对应 medium，之下归 low。）
fn effort_from_budget(budget: u64) -> &'static str {
    if budget >= 32_000 {
        "high"
    } else if budget >= 8_000 {
        "medium"
    } else {
        "low"
    }
}

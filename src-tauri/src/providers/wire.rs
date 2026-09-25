//! 协议能力表：把「每个上游协议独有的知识」集中到一张表里。
//!
//! 规范请求（`domain::canonical::RequestBody`）是三个协议的并集。各协议能表达哪些字段、
//! 用什么名字表达、哪些字段根本没有对应语义，全部写在下面的 profile 里。好处：
//!
//! 1. 编码器不再逐字段手写 `if let Some(..) { insert }`，「这个字段漏了」只会出现在需要
//!    协议专属变换的位置，其余由 `apply_common_fields` 统一落地；
//! 2. `tests::protocol_profiles_cover_every_canonical_field` 盯着「规范字段 = 已映射 ∪ 显式丢弃」，
//!    以后往 `RequestBody` 加字段忘了同步这张表，测试立刻失败——静默丢字段变成结构上不可能。
//!
//! 字段名与语义口径来自官方文档：OpenAI Chat Completions、OpenAI Responses、Anthropic Messages。

use serde_json::{json, Map, Value};

use crate::domain::model::ModelFormat;

use super::SseEvent;

/// 规范字段落到线上报文上的方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// 同名直接写。
    Same,
    /// 换个名字写（形状一致，只是键名不同）。
    Renamed(&'static str),
    /// 名字就是本协议的字段名，但形状/结构要 provider 自己拼（消息、工具、tool_choice 等）。
    Custom,
    /// 名字在本协议里不存在，provider 会把值变形后放到别处（如 `response_format` → `text.format`）。
    /// 规范名必须从报文里删掉，绝不能原样发出去。
    Transformed,
    /// 该协议没有对应语义，显式丢弃。
    Dropped,
}

pub struct FieldRule {
    /// 规范字段名（= `RequestBody` 的 JSON 键）。
    pub field: &'static str,
    pub slot: Slot,
}

pub struct ProtocolProfile {
    /// 规范字段 → 该协议的落地方式，必须覆盖 `RequestBody` 的全部顶层字段。
    pub rules: &'static [FieldRule],
}

/// 规范内部字段所在的键（任何线上协议都没有这一层）。
pub const CANONICAL_ONLY_KEY: &str = "_canonical";

/// 写入策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    /// 从规范体重建的报文（OpenAI 系）：直接覆盖。
    Overwrite,
    /// 透传的报文里已经是客户端原样字段（`tools` 之类还带协议扩展字段）：只在缺失时补写。
    IfAbsent,
}

const ANTHROPIC_RULES: &[FieldRule] = &[
    FieldRule { field: "model", slot: Slot::Custom },
    FieldRule { field: "max_tokens", slot: Slot::Custom },
    FieldRule { field: "system", slot: Slot::Same },
    FieldRule { field: "messages", slot: Slot::Same },
    FieldRule { field: "tools", slot: Slot::Same },
    FieldRule { field: "tool_choice", slot: Slot::Custom },
    FieldRule { field: "temperature", slot: Slot::Same },
    FieldRule { field: "top_p", slot: Slot::Same },
    FieldRule { field: "top_k", slot: Slot::Same },
    FieldRule { field: "stop_sequences", slot: Slot::Same },
    FieldRule { field: "stream", slot: Slot::Same },
    FieldRule { field: "store", slot: Slot::Dropped },
    FieldRule { field: "metadata", slot: Slot::Transformed },
    FieldRule { field: "response_format", slot: Slot::Transformed },
    FieldRule { field: "parallel_tool_calls", slot: Slot::Transformed },
    FieldRule { field: "reasoning_effort", slot: Slot::Transformed },
    // A5：`service_tier` 的取值词表是各家自己的（OpenAI 的 flex/priority/scale/default
    // ↔ Anthropic 的 auto/standard_only），跨协议转发等于把 A 家的词丢进 B 家的枚举，
    // 上游会按非法取值 400 掉整个请求。Anthropic 侧改为显式丢弃——丢的至多是一个
    // 容量/优先级偏好（而 Anthropic 的默认档正是 `auto`），换来的是不会 400。
    //
    // 代价说清：`Dropped` 在**本协议的编码路径上同样生效**，所以 Anthropic 客户端自己
    // 带的 `service_tier` 也会被这条规则删掉（这条路径与跨协议重建共用同一个
    // `encode_request`，不像两个 OpenAI 协议那样有独立的 `encode_request_passthrough`）。
    // 见 `docs/forwarding.md` §8 批 4 的取舍说明。
    FieldRule { field: "service_tier", slot: Slot::Dropped },
    FieldRule { field: "n", slot: Slot::Dropped },
    FieldRule { field: CANONICAL_ONLY_KEY, slot: Slot::Dropped },
];

const COMPLETIONS_RULES: &[FieldRule] = &[
    FieldRule { field: "model", slot: Slot::Custom },
    FieldRule { field: "max_tokens", slot: Slot::Custom },
    FieldRule { field: "system", slot: Slot::Custom },
    FieldRule { field: "messages", slot: Slot::Custom },
    FieldRule { field: "tools", slot: Slot::Custom },
    FieldRule { field: "tool_choice", slot: Slot::Custom },
    FieldRule { field: "temperature", slot: Slot::Same },
    FieldRule { field: "top_p", slot: Slot::Same },
    FieldRule { field: "top_k", slot: Slot::Dropped },
    FieldRule { field: "stop_sequences", slot: Slot::Renamed("stop") },
    FieldRule { field: "stream", slot: Slot::Same },
    FieldRule { field: "store", slot: Slot::Same },
    FieldRule { field: "metadata", slot: Slot::Same },
    FieldRule { field: "response_format", slot: Slot::Same },
    FieldRule { field: "parallel_tool_calls", slot: Slot::Same },
    FieldRule { field: "reasoning_effort", slot: Slot::Same },
    FieldRule { field: "service_tier", slot: Slot::Same },
    FieldRule { field: "n", slot: Slot::Same },
    FieldRule { field: CANONICAL_ONLY_KEY, slot: Slot::Dropped },
];

const RESPONSES_RULES: &[FieldRule] = &[
    FieldRule { field: "model", slot: Slot::Custom },
    FieldRule { field: "max_tokens", slot: Slot::Custom },
    FieldRule { field: "system", slot: Slot::Custom },
    FieldRule { field: "messages", slot: Slot::Custom },
    FieldRule { field: "tools", slot: Slot::Custom },
    FieldRule { field: "tool_choice", slot: Slot::Custom },
    FieldRule { field: "temperature", slot: Slot::Same },
    FieldRule { field: "top_p", slot: Slot::Same },
    FieldRule { field: "top_k", slot: Slot::Dropped },
    FieldRule { field: "stop_sequences", slot: Slot::Dropped },
    FieldRule { field: "stream", slot: Slot::Same },
    FieldRule { field: "store", slot: Slot::Same },
    FieldRule { field: "metadata", slot: Slot::Same },
    FieldRule { field: "response_format", slot: Slot::Transformed },
    FieldRule { field: "parallel_tool_calls", slot: Slot::Same },
    FieldRule { field: "reasoning_effort", slot: Slot::Transformed },
    FieldRule { field: "service_tier", slot: Slot::Same },
    FieldRule { field: "n", slot: Slot::Dropped },
    FieldRule { field: CANONICAL_ONLY_KEY, slot: Slot::Dropped },
];

const ANTHROPIC: ProtocolProfile = ProtocolProfile {
    rules: ANTHROPIC_RULES,
};

const OPENAI_COMPLETIONS: ProtocolProfile = ProtocolProfile {
    rules: COMPLETIONS_RULES,
};

const OPENAI_RESPONSES: ProtocolProfile = ProtocolProfile {
    rules: RESPONSES_RULES,
};

pub fn profile(format: ModelFormat) -> &'static ProtocolProfile {
    match format {
        ModelFormat::AnthropicMessages => &ANTHROPIC,
        ModelFormat::OpenaiCompletions => &OPENAI_COMPLETIONS,
        ModelFormat::OpenaiResponses => &OPENAI_RESPONSES,
    }
}

/// 按 profile 把规范字段落到报文上：`Same`/`Renamed` 写入，
/// `Transformed`（本协议没有这个键名）与 `Dropped` 一律删除，`Custom` 留给 provider
/// （它自己按本协议的字段名拼结构）。
///
/// `canonical` 是 `serde_json::to_value(RequestBody)` 的结果。
pub fn apply_common_fields(
    payload: &mut Map<String, Value>,
    canonical: &Value,
    profile: &ProtocolProfile,
    fill: Fill,
) {
    for rule in profile.rules {
        match rule.slot {
            Slot::Same => write(payload, rule.field, canonical.get(rule.field), fill),
            Slot::Renamed(name) => write(payload, name, canonical.get(rule.field), fill),
            Slot::Transformed | Slot::Dropped => {
                payload.remove(rule.field);
            }
            Slot::Custom => {}
        }
    }
}

fn write(payload: &mut Map<String, Value>, key: &str, value: Option<&Value>, fill: Fill) {
    let Some(value) = value else {
        return;
    };
    if fill == Fill::IfAbsent && payload.contains_key(key) {
        return;
    }
    payload.insert(key.to_string(), value.clone());
}

/// 流内错误事件的形状（A7）：**一份来源，三个协议共用**。
///
/// 上游在流里报错时，解码侧产出的规范错误事件是 Anthropic 形状
/// （`{"type":"error","error":{…}}`——规范层的不变式）。原样转发给 OpenAI 客户端，
/// 等于给它们发了一个自己协议里不存在的形状；而网关自产的错误（读到断流、解帧失败）
/// 又是按入站协议出形状的——同一个客户端会碰上两种形状。这里把形状定义收成一处，
/// 网关自产与上游流内错误都从它出。
///
/// Anthropic 客户端本来就是这个形状，`error_events` 与上游原文一致；两个 OpenAI 协议
/// 用它们自己 HTTP 错误体的信封（`{"error":{message,type}}`），客户端对 4xx/5xx 响应体
/// 已经在按这个形状解析了。
pub fn stream_error_event(format: ModelFormat, kind: &str, message: &str) -> SseEvent {
    match format {
        ModelFormat::AnthropicMessages => SseEvent::new(
            "error",
            json!({ "type": "error", "error": { "type": kind, "message": message } }),
        ),
        _ => SseEvent::new(
            "error",
            json!({ "error": { "message": message, "type": kind } }),
        ),
    }
}

/// 从规范错误事件里取 `(kind, message)`，供各 provider 的 `encode_stream_event` 复用。
pub fn stream_error_parts(canonical: &Value) -> (&str, &str) {
    (
        canonical
            .pointer("/error/type")
            .and_then(Value::as_str)
            .unwrap_or("api_error"),
        canonical
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("上游返回错误"),
    )
}

/// 规范 stop_reason → OpenAI Chat Completions 的 `finish_reason`。
/// OpenAI 的取值集合只有 stop / length / tool_calls / content_filter /（弃用的 function_call）。
pub fn finish_reason(stop_reason: Option<&str>) -> &'static str {
    match stop_reason {
        Some("max_tokens") | Some("model_context_window_exceeded") => "length",
        Some("tool_use") => "tool_calls",
        Some("refusal") => "content_filter",
        // stop_sequence / pause_turn / end_turn 等 OpenAI 没有对应取值，都归一到自然结束。
        _ => "stop",
    }
}

/// OpenAI Chat Completions 的 `finish_reason` → 规范 stop_reason。
pub fn stop_reason_from_finish_reason(reason: Option<&str>) -> String {
    match reason {
        Some("tool_calls") | Some("function_call") => "tool_use",
        Some("length") => "max_tokens",
        Some("content_filter") => "refusal",
        // stop 是自然结束；Anthropic 风格的 end_turn / Responses 的 completed 也归一到这里
        // （有些 OpenAI 兼容上游会直接回 end_turn）。
        Some("stop") | Some("end_turn") | Some("completed") | None => "end_turn",
        Some(other) => other,
    }
    .to_string()
}

/// Anthropic Messages 的 `stop_reason` → 规范 stop_reason（取值基本同名，只做归一）。
pub fn stop_reason_from_anthropic(reason: Option<&str>) -> String {
    match reason {
        Some("max_output_tokens") => "max_tokens",
        Some(other) => other,
        None => "end_turn",
    }
    .to_string()
}

/// Responses 的响应 `status` → 规范 stop_reason。
pub fn stop_reason_from_response(status: Option<&str>, has_tool_calls: bool) -> String {
    match status {
        Some("incomplete") => "max_tokens".to_string(),
        Some("completed") | None => {
            if has_tool_calls {
                "tool_use".to_string()
            } else {
                "end_turn".to_string()
            }
        }
        Some(other) => other.to_string(),
    }
}

/// 规范 stop_reason → Responses 的响应 `status` 与 `incomplete_details`。
pub fn response_status(stop_reason: Option<&str>) -> (&'static str, Option<&'static str>) {
    match stop_reason {
        Some("max_tokens") | Some("model_context_window_exceeded") => {
            ("incomplete", Some("max_tokens"))
        }
        _ => ("completed", None),
    }
}

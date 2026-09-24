pub mod anthropic_messages;
pub mod openai_completions;
pub mod openai_responses;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use serde_json::{json, Value};

use crate::domain::canonical::CanonicalRequest;
use crate::domain::model::{ModelConfig, ModelFormat};
use crate::error::AppResult;

pub const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone)]
pub struct SseEvent {
    pub event: String,
    pub data: Value,
    pub raw: Option<String>,
}

impl SseEvent {
    pub fn new(event: impl Into<String>, data: Value) -> Self {
        Self {
            event: event.into(),
            data,
            raw: None,
        }
    }

    /// An event whose payload is not JSON (for example OpenAI's `[DONE]` marker).
    pub fn raw(payload: impl Into<String>) -> Self {
        Self {
            event: String::new(),
            data: Value::Null,
            raw: Some(payload.into()),
        }
    }
}

/// State used when re-encoding canonical (Anthropic) events into another wire format.
#[derive(Debug, Default)]
pub struct WireState {
    pub started: bool,
    pub done_sent: bool,
    pub model: String,
    pub response_id: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub next_output_index: i64,
    pub text_block_index: Option<i64>,
    pub text_output_index: i64,
    pub text_item_open: bool,
    pub text_item_id: String,
    pub text_buffer: String,
    pub tool_indices: BTreeMap<i64, i64>,
    pub tool_meta: BTreeMap<i64, (String, String)>,
    pub tool_args: BTreeMap<i64, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Text,
    Thinking,
    ToolUse,
}

impl BlockKind {
    fn start_block(&self) -> Value {
        match self {
            BlockKind::Text => json!({ "type": "text", "text": "" }),
            BlockKind::Thinking => json!({ "type": "thinking", "thinking": "" }),
            BlockKind::ToolUse => json!({ "type": "tool_use", "id": "", "name": "", "input": {} }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaKind {
    Text,
    Thinking,
    Json,
}

#[derive(Debug)]
pub struct StreamState {
    pub message_id: String,
    pub upstream_model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// 缓存读 / 缓存写 token（Anthropic 语义：input_tokens 不含缓存，两者单独计）。
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub message_started: bool,
    pub open_block: Option<BlockKind>,
    pub next_index: i64,
    pub stop_reason: Option<String>,
    pub finished: bool,
    pub tool_calls: BTreeSet<i64>,
}

impl StreamState {
    pub fn new(requested_model: impl Into<String>) -> Self {
        let requested_model = requested_model.into();
        Self {
            message_id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
            upstream_model: requested_model,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            message_started: false,
            open_block: None,
            next_index: 0,
            stop_reason: None,
            finished: false,
            tool_calls: BTreeSet::new(),
        }
    }

    pub fn begin(&mut self) -> Vec<SseEvent> {
        if self.message_started {
            return Vec::new();
        }
        self.message_started = true;
        vec![SseEvent::new(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id,
                    "type": "message",
                    "role": "assistant",
                    "model": self.upstream_model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": self.input_tokens,
                        "output_tokens": 0,
                        "cache_read_input_tokens": self.cache_read_tokens,
                        "cache_creation_input_tokens": self.cache_write_tokens
                    }
                }
            }),
        )]
    }

    pub fn close_block(&mut self) -> Option<SseEvent> {
        self.open_block.take().map(|_| {
            let index = self.next_index - 1;
            SseEvent::new(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": index }),
            )
        })
    }

    pub fn open_tool(&mut self, tool_id: &str, name: &str) -> Vec<SseEvent> {
        let mut events = self.begin();
        if let Some(event) = self.close_block() {
            events.push(event);
        }
        self.open_block = Some(BlockKind::ToolUse);
        let index = self.next_index;
        self.next_index += 1;
        events.push(SseEvent::new(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": { "type": "tool_use", "id": tool_id, "name": name, "input": {} }
            }),
        ));
        events
    }

    pub fn delta(&mut self, kind: DeltaKind, payload: &str) -> Vec<SseEvent> {
        if payload.is_empty() {
            return Vec::new();
        }
        let mut events = self.begin();
        if self.open_block.is_none() {
            let block = match kind {
                DeltaKind::Thinking => BlockKind::Thinking,
                _ => BlockKind::Text,
            };
            if let Some(event) = self.close_block() {
                events.push(event);
            }
            self.open_block = Some(block);
            let index = self.next_index;
            self.next_index += 1;
            events.push(SseEvent::new(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": block.start_block()
                }),
            ));
        }
        let index = self.next_index - 1;
        let delta = match kind {
            DeltaKind::Text => json!({ "type": "text_delta", "text": payload }),
            DeltaKind::Thinking => json!({ "type": "thinking_delta", "thinking": payload }),
            DeltaKind::Json => json!({ "type": "input_json_delta", "partial_json": payload }),
        };
        events.push(SseEvent::new(
            "content_block_delta",
            json!({ "type": "content_block_delta", "index": index, "delta": delta }),
        ));
        events
    }

    pub fn finish(&mut self, stop_reason: &str) -> Vec<SseEvent> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        self.stop_reason = Some(stop_reason.to_string());
        let mut events = self.begin();
        if let Some(event) = self.close_block() {
            events.push(event);
        }
        events.push(SseEvent::new(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": stop_reason, "stop_sequence": null },
                "usage": { "output_tokens": self.output_tokens }
            }),
        ));
        events.push(SseEvent::new(
            "message_stop",
            json!({ "type": "message_stop" }),
        ));
        events
    }

    pub fn error(&mut self, kind: &str, message: &str) -> Vec<SseEvent> {
        self.finished = true;
        vec![SseEvent::new(
            "error",
            json!({ "type": "error", "error": { "type": kind, "message": message } }),
        )]
    }
}

#[derive(Debug, Default)]
struct AssembledToolCall {
    id: String,
    name: String,
    arguments: String,
}

/// 把 canonical（Anthropic 形状）流事件拼装成 canonical message，供报文落库使用。
/// 落库前会再经 provider 的 `encode_response` 转成上游协议的原生形状，前端按协议解析。
#[derive(Debug, Default)]
pub struct ResponseAssembler {
    message_id: String,
    model: String,
    text: String,
    thinking: String,
    tool_calls: Vec<AssembledToolCall>,
    tool_index_by_block: BTreeMap<i64, usize>,
    stop_reason: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
}

impl ResponseAssembler {
    pub fn apply(&mut self, event: &SseEvent) {
        let data = &event.data;
        match event.event.as_str() {
            "message_start" => {
                if let Some(id) = data.pointer("/message/id").and_then(Value::as_str) {
                    self.message_id = id.to_string();
                }
                if let Some(model) = data.pointer("/message/model").and_then(Value::as_str) {
                    self.model = model.to_string();
                }
                if let Some(tokens) = data
                    .pointer("/message/usage/input_tokens")
                    .and_then(Value::as_u64)
                {
                    self.input_tokens = tokens;
                }
                if let Some(tokens) = data
                    .pointer("/message/usage/cache_read_input_tokens")
                    .and_then(Value::as_u64)
                {
                    self.cache_read_tokens = tokens;
                }
                if let Some(tokens) = data
                    .pointer("/message/usage/cache_creation_input_tokens")
                    .and_then(Value::as_u64)
                {
                    self.cache_write_tokens = tokens;
                }
            }
            "content_block_start" => {
                if data.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use") {
                    let index = data.get("index").and_then(Value::as_i64).unwrap_or(0);
                    self.tool_calls.push(AssembledToolCall {
                        id: data
                            .pointer("/content_block/id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        name: data
                            .pointer("/content_block/name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        arguments: String::new(),
                    });
                    self.tool_index_by_block
                        .insert(index, self.tool_calls.len() - 1);
                }
            }
            "content_block_delta" => {
                let index = data.get("index").and_then(Value::as_i64).unwrap_or(0);
                match data.pointer("/delta/type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(text) = data.pointer("/delta/text").and_then(Value::as_str) {
                            self.text.push_str(text);
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(text) = data.pointer("/delta/thinking").and_then(Value::as_str)
                        {
                            self.thinking.push_str(text);
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(partial) =
                            data.pointer("/delta/partial_json").and_then(Value::as_str)
                        {
                            if let Some(&tool) = self.tool_index_by_block.get(&index) {
                                self.tool_calls[tool].arguments.push_str(partial);
                            }
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(stop) = data.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop_reason = Some(stop.to_string());
                }
                if let Some(tokens) = data.pointer("/usage/output_tokens").and_then(Value::as_u64) {
                    self.output_tokens = tokens;
                }
            }
            _ => {}
        }
    }

    /// 拼装成 canonical（Anthropic Messages 形状）的响应消息。
    pub fn to_value(&self) -> Value {
        let mut content: Vec<Value> = Vec::new();
        if !self.thinking.is_empty() {
            content.push(json!({ "type": "thinking", "thinking": self.thinking }));
        }
        if !self.text.is_empty() {
            content.push(json!({ "type": "text", "text": self.text }));
        }
        for tool in &self.tool_calls {
            let input = serde_json::from_str::<Value>(&tool.arguments)
                .unwrap_or_else(|_| Value::String(tool.arguments.clone()));
            content.push(json!({
                "type": "tool_use",
                "id": tool.id,
                "name": tool.name,
                "input": input
            }));
        }

        let id = if self.message_id.is_empty() {
            format!("msg_{}", uuid::Uuid::new_v4().simple())
        } else {
            self.message_id.clone()
        };

        json!({
            "id": id,
            "type": "message",
            "role": "assistant",
            "model": self.model,
            "content": content,
            "stop_reason": self.stop_reason,
            "stop_sequence": null,
            "usage": {
                "input_tokens": self.input_tokens,
                "output_tokens": self.output_tokens,
                "cache_read_input_tokens": self.cache_read_tokens,
                "cache_creation_input_tokens": self.cache_write_tokens
            }
        })
    }
}

pub trait ModelProvider: Send + Sync {
    fn is_passthrough(&self) -> bool {
        false
    }

    fn endpoint(&self, cfg: &ModelConfig) -> String {
        cfg.completion_url()
    }

    fn headers(&self, cfg: &ModelConfig) -> Vec<(String, String)>;

    fn encode_request(&self, cfg: &ModelConfig, req: &CanonicalRequest) -> AppResult<Value>;

    fn decode_response(&self, cfg: &ModelConfig, raw: &Value) -> AppResult<Value>;

    fn decode_stream_event(
        &self,
        cfg: &ModelConfig,
        event: &str,
        data: &Value,
        state: &mut StreamState,
    ) -> AppResult<Vec<SseEvent>>;

    fn decode_stream_done(
        &self,
        _cfg: &ModelConfig,
        state: &mut StreamState,
    ) -> AppResult<Vec<SseEvent>> {
        Ok(state.finish("end_turn"))
    }

    /// Client wire request -> canonical request. Default assumes the canonical (Anthropic) shape.
    fn decode_request(&self, raw: Value) -> AppResult<CanonicalRequest> {
        CanonicalRequest::parse(raw)
    }

    /// Canonical response -> client wire response. Default passes the canonical shape through.
    fn encode_response(&self, _cfg: &ModelConfig, canonical: &Value) -> AppResult<Value> {
        Ok(canonical.clone())
    }

    /// Canonical stream event -> client wire stream events.
    fn encode_stream_event(
        &self,
        _cfg: &ModelConfig,
        canonical: &SseEvent,
        _state: &mut WireState,
    ) -> Vec<SseEvent> {
        vec![canonical.clone()]
    }

    /// Final client wire events once the upstream stream ends.
    fn encode_stream_done(&self, _cfg: &ModelConfig, _state: &mut WireState) -> Vec<SseEvent> {
        Vec::new()
    }
}

static ANTHROPIC: anthropic_messages::AnthropicMessagesProvider =
    anthropic_messages::AnthropicMessagesProvider;
static OPENAI_COMPLETIONS: openai_completions::OpenaiCompletionsProvider =
    openai_completions::OpenaiCompletionsProvider;
static OPENAI_RESPONSES: openai_responses::OpenaiResponsesProvider =
    openai_responses::OpenaiResponsesProvider;

pub fn provider_for(format: ModelFormat) -> &'static dyn ModelProvider {
    match format {
        ModelFormat::AnthropicMessages => &ANTHROPIC,
        ModelFormat::OpenaiCompletions => &OPENAI_COMPLETIONS,
        ModelFormat::OpenaiResponses => &OPENAI_RESPONSES,
    }
}

pub fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(20))
            .build()
            .expect("failed to build reqwest client")
    })
}

pub fn resolve_stop_reason(value: Option<&str>) -> String {
    match value {
        Some("tool_calls") | Some("function_call") => "tool_use",
        Some("length") | Some("max_tokens") | Some("max_output_tokens") => "max_tokens",
        Some("stop") | Some("end_turn") | Some("completed") | None => "end_turn",
        Some(other) => other,
    }
    .to_string()
}

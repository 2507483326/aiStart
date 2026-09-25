pub mod anthropic_messages;
pub mod normalizer;
pub mod openai_completions;
pub mod openai_responses;
pub mod wire;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use serde_json::{json, Value};

use crate::domain::canonical::CanonicalRequest;
use crate::domain::model::{ModelConfig, ModelFormat};
use crate::error::AppResult;

use normalizer::{BlockNormalizer, StreamVerdict};

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
    /// 入站客户端是否要求流式 usage（OpenAI `stream_options.include_usage`），只有 OpenAI completions 出站用。
    pub include_usage: bool,
    pub model: String,
    pub response_id: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub reasoning_tokens: u64,
    /// 已经看到的规范 stop_reason（Responses 出站要据此决定 completed / incomplete）。
    pub stop_reason: Option<String>,
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

#[derive(Debug)]
pub struct StreamState {
    pub message_id: String,
    pub upstream_model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// 缓存读 / 缓存写 token（Anthropic 语义：input_tokens 不含缓存，两者单独计）。
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// 思考 token：completions 的 `completion_tokens_details.reasoning_tokens`、
    /// Responses 的 `output_tokens_details.reasoning_tokens`、Anthropic 的 `output_tokens_details.thinking_tokens`。
    pub reasoning_tokens: u64,
    pub message_started: bool,
    pub stop_reason: Option<String>,
    pub finished: bool,
    /// 上游有没有发过真正的结束信号（`finish_reason` / `message_stop` / `[DONE]`）。
    /// 用来区分「上游正常收尾」和「连接断了」——后者以前会被静默补成 end_turn。
    pub upstream_ended: bool,
    /// 上游在流里报的错误（Anthropic 的 `error` 事件、OpenAI 的 `{"error":..}` 分片等）。
    /// 这类流以前会被记成成功，因为错误是作为事件转发出去、不进 `stream_error`。
    pub upstream_error: Option<String>,
    /// 已经见过的上游工具序号（解码侧用它判断「这是某个工具的第一个分片」）。
    pub tool_calls: BTreeSet<i64>,
    blocks: BlockNormalizer,
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
            reasoning_tokens: 0,
            message_started: false,
            stop_reason: None,
            finished: false,
            upstream_ended: false,
            upstream_error: None,
            tool_calls: BTreeSet::new(),
            blocks: BlockNormalizer::default(),
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

    pub fn text_delta(&mut self, payload: &str) -> Vec<SseEvent> {
        let mut events = self.begin();
        events.extend(self.blocks.text(payload));
        events
    }

    pub fn thinking_delta(&mut self, payload: &str) -> Vec<SseEvent> {
        let mut events = self.begin();
        events.extend(self.blocks.thinking(payload));
        events
    }

    /// 声明一个工具调用（块在第一个参数分片或流结束时才开）。
    pub fn tool_start(&mut self, upstream_index: i64, id: &str, name: &str) -> Vec<SseEvent> {
        self.blocks.tool_start(upstream_index, id, name);
        self.begin()
    }

    pub fn tool_args(&mut self, upstream_index: i64, partial: &str) -> Vec<SseEvent> {
        let mut events = self.begin();
        events.extend(self.blocks.tool_args(upstream_index, partial));
        events
    }

    pub fn finish(&mut self, stop_reason: &str) -> Vec<SseEvent> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        self.stop_reason = Some(stop_reason.to_string());
        let mut events = self.begin();
        events.extend(self.blocks.close());
        events.push(SseEvent::new(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": stop_reason, "stop_sequence": null },
                // Anthropic 文档口径：message_delta 的 usage 是累计值，且带 input / 缓存字段。
                // 上游同一条消息里还没报过的值写 null（类型允许 null），不把「未知」写成 0。
                "usage": {
                    "input_tokens": reported(self.input_tokens),
                    "output_tokens": self.output_tokens,
                    "cache_read_input_tokens": reported(self.cache_read_tokens),
                    "cache_creation_input_tokens": reported(self.cache_write_tokens),
                    "output_tokens_details": { "thinking_tokens": reported(self.reasoning_tokens) }
                }
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

    /// 流结束后的形态判定，决定这条明细算不算成功（`server.rs` 只负责上报）。
    pub fn verdict(&self, error: Option<String>) -> StreamVerdict {
        if let Some(message) = error {
            return StreamVerdict::UpstreamError(message);
        }
        if let Some(message) = &self.upstream_error {
            return StreamVerdict::UpstreamError(message.clone());
        }
        if !self.upstream_ended {
            return StreamVerdict::Truncated;
        }
        if self.blocks.text_chars() == 0
            && self.blocks.tool_count() == 0
            && self.blocks.thinking_chars() > 0
        {
            return StreamVerdict::EmptyReasoningOnly;
        }
        StreamVerdict::Ok
    }
}

/// 0 视为「上游还没报」→ null（Anthropic 的 usage 字段允许 null）。
fn reported(tokens: u64) -> Value {
    if tokens == 0 {
        Value::Null
    } else {
        json!(tokens)
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

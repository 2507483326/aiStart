use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{AppError, AppResult};

pub type JsonMap = Map<String, Value>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentBlock {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(flatten)]
    pub data: JsonMap,
}

impl ContentBlock {
    pub fn new(kind: &str, data: JsonMap) -> Self {
        Self {
            kind: kind.to_string(),
            data,
        }
    }

    pub fn text(text: impl Into<String>) -> Self {
        let mut data = JsonMap::new();
        data.insert("text".into(), Value::String(text.into()));
        Self::new("text", data)
    }

    pub fn is(&self, kind: &str) -> bool {
        self.kind == kind
    }

    pub fn field(&self, key: &str) -> Option<&Value> {
        self.data.get(key)
    }

    pub fn str_field(&self, key: &str) -> Option<&str> {
        self.data.get(key).and_then(Value::as_str)
    }

    pub fn text_value(&self) -> String {
        self.str_field("text").unwrap_or_default().to_string()
    }

    pub fn tool_use(&self) -> Option<ToolUse> {
        if !self.is("tool_use") {
            return None;
        }
        Some(ToolUse {
            id: self.str_field("id").unwrap_or_default().to_string(),
            name: self.str_field("name").unwrap_or_default().to_string(),
            input: self
                .field("input")
                .cloned()
                .unwrap_or_else(|| Value::Object(JsonMap::new())),
        })
    }

    pub fn tool_result(&self) -> Option<ToolResult> {
        if !self.is("tool_result") {
            return None;
        }
        Some(ToolResult {
            tool_use_id: self
                .str_field("tool_use_id")
                .unwrap_or_default()
                .to_string(),
            content: self.field("content").cloned().unwrap_or(Value::Null),
            is_error: self
                .field("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

#[derive(Debug, Clone)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: Value,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl MessageContent {
    pub fn blocks(&self) -> Vec<ContentBlock> {
        match self {
            MessageContent::Text(text) => vec![ContentBlock::text(text.clone())],
            MessageContent::Blocks(blocks) => blocks.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemPrompt {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl SystemPrompt {
    pub fn plain_text(&self) -> String {
        match self {
            SystemPrompt::Text(text) => text.clone(),
            SystemPrompt::Blocks(blocks) => blocks_to_text(blocks),
        }
    }
}

pub fn blocks_to_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter(|block| block.is("text"))
        .map(|block| block.text_value())
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn content_to_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(content_to_text)
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(map) => map
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: MessageContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, rename = "input_schema")]
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestBody {
    #[serde(default)]
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemPrompt>,
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDef>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(default)]
    pub stream: bool,
    /// Anthropic 专有：核采样候选集大小（另两个协议没有对应参数）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<f64>,
    /// 是否让上游留存这次请求（OpenAI / Responses 同名；Anthropic 无此参数）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
    /// 结构化元数据：OpenAI / Responses 是任意字符串映射，Anthropic 只接受 `user_id`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    /// 输出格式，统一按 OpenAI Chat Completions 的形状表达
    /// （`{type:"text"|"json_object"|"json_schema", json_schema:{name,schema,strict,description}}`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<Value>,
    /// 是否允许并行工具调用：OpenAI / Responses 同名，Anthropic 要取反写进 `tool_choice.disable_parallel_tool_use`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    /// 思考档位：completions 是 `reasoning_effort`，Responses 是 `reasoning.effort`，
    /// Anthropic 是 `output_config.effort`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// 处理档位（completions / Responses / Anthropic 都有同名参数，取值集合略有差异）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    /// 一次请求生成几个候选。网关只承载单候选，> 1 由 `validate()` 挡掉（见下）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    /// 只在规范内部存在的字段（任何线上协议都没有的键），统一收进 `_canonical`：
    /// 透传前整体删掉这一层，以后新增同类字段也不会漏到上游。
    #[serde(default, rename = "_canonical")]
    pub canonical: CanonicalOnly,
}

/// 规范内部字段（不属于任何线上协议）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CanonicalOnly {
    /// 客户端是否要求流式响应末尾附带 usage（OpenAI 的 `stream_options.include_usage`）。
    #[serde(default)]
    pub include_usage: bool,
    /// 客户端原本用哪个字段表达输出上限。OpenAI 的 `max_tokens` 已弃用且与 o 系列不兼容，
    /// 而 `max_completion_tokens` 才是现行字段；上游编码时按客户端的用法回同一个字段名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_field: Option<MaxTokensField>,
}

/// 输出上限在 OpenAI Chat Completions 请求里的字段名。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaxTokensField {
    MaxTokens,
    MaxCompletionTokens,
}

#[derive(Debug, Clone)]
pub struct CanonicalRequest {
    raw: Value,
    body: RequestBody,
}

impl CanonicalRequest {
    pub fn parse(raw: Value) -> crate::error::AppResult<Self> {
        let body: RequestBody = serde_json::from_value(raw.clone()).map_err(|err| {
            crate::error::AppError::InvalidConfig(format!("请求体格式非法: {err}"))
        })?;
        Ok(Self { raw, body })
    }

    pub fn raw(&self) -> &Value {
        &self.raw
    }

    pub fn body(&self) -> &RequestBody {
        &self.body
    }

    pub fn stream(&self) -> bool {
        self.body.stream
    }

    /// 规范级校验：无法保真转换的请求直接拒绝，避免「只转了一半」的静默行为。
    /// 目前只有多候选：网关与规范形状都只承载一条 assistant 消息。
    pub fn validate(&self) -> AppResult<()> {
        if self.body.n.is_some_and(|count| count > 1) {
            return Err(AppError::InvalidConfig(
                "一次请求多个候选（n > 1）无法保真转发，请把 n 设为 1 或去掉该字段".into(),
            ));
        }
        Ok(())
    }

    /// 在规范 JSON 上做改写，并据此重建类型化的 body，保证两者始终一致。
    /// 过滤器（请求转发前的改写）通过它修改请求。
    pub fn map_raw(
        mut self,
        f: impl FnOnce(&mut Value) -> crate::error::AppResult<()>,
    ) -> crate::error::AppResult<Self> {
        f(&mut self.raw)?;
        self.body = serde_json::from_value(self.raw.clone()).map_err(|err| {
            crate::error::AppError::InvalidConfig(format!("请求体格式非法: {err}"))
        })?;
        Ok(self)
    }
}

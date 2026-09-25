use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{json, Map, Value};

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

/// 工具调用策略的规范形状（§7-A3：形状分歧字段必须类型化，decode 侧统一收进这里，
/// encode 侧按本协议写出——形状解析只发生在入站协议自己的转换函数里，不再靠 JSON 猜臂）。
#[derive(Debug, Clone, PartialEq)]
pub enum CanonicalToolChoice {
    Auto,
    None,
    Required,
    /// 强制调用某个具体工具。
    Tool { name: String },
}

impl CanonicalToolChoice {
    /// 规范 JSON（OpenAI Chat Completions 口径）：字符串同名字段 + `{type:function,function:{name}}`。
    pub fn to_json(&self) -> Value {
        match self {
            Self::Auto => Value::String("auto".into()),
            Self::None => Value::String("none".into()),
            Self::Required => Value::String("required".into()),
            Self::Tool { name } => json!({ "type": "function", "function": { "name": name } }),
        }
    }

    /// 解析规范 JSON；不属于任何已知臂时报错（而不是静默降级成 auto）。
    pub fn from_json(value: &Value) -> AppResult<Self> {
        match value {
            Value::String(kind) => match kind.as_str() {
                "auto" => Ok(Self::Auto),
                "none" => Ok(Self::None),
                "required" => Ok(Self::Required),
                other => Err(tool_choice_error(value, &format!("字符串 {other:?}"))),
            },
            Value::Object(object) => match object.get("type").and_then(Value::as_str) {
                Some("function") => {
                    let name = object
                        .get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    Ok(Self::Tool { name: name.to_string() })
                }
                Some(other) => Err(tool_choice_error(value, other)),
                None => Err(tool_choice_error(value, "缺少 type 字段")),
            },
            _ => Err(tool_choice_error(value, "既不是字符串也不是对象")),
        }
    }
}

fn tool_choice_error(value: &Value, detail: &str) -> AppError {
    AppError::InvalidConfig(format!(
        "tool_choice 形状非法（{detail}）: {}",
        serde_json::to_string(value).unwrap_or_default()
    ))
}

/// 输出格式的规范形状（OpenAI Chat Completions 口径的嵌套结构）。
#[derive(Debug, Clone, PartialEq)]
pub enum CanonicalResponseFormat {
    Text,
    JsonObject,
    JsonSchema {
        name: String,
        description: Option<String>,
        schema: Value,
        strict: Option<bool>,
    },
}

impl CanonicalResponseFormat {
    /// 规范 JSON：`{type:"json_schema", json_schema:{name,schema,strict}}`。
    pub fn to_json(&self) -> Value {
        match self {
            Self::Text => json!({ "type": "text" }),
            Self::JsonObject => json!({ "type": "json_object" }),
            Self::JsonSchema { name, description, schema, strict } => {
                let mut inner = Map::new();
                inner.insert("name".into(), Value::String(name.clone()));
                if let Some(description) = description {
                    inner.insert("description".into(), Value::String(description.clone()));
                }
                inner.insert("schema".into(), schema.clone());
                if let Some(strict) = strict {
                    inner.insert("strict".into(), Value::Bool(*strict));
                }
                json!({ "type": "json_schema", "json_schema": Value::Object(inner) })
            }
        }
    }

    /// 解析规范 JSON；`text` / `json_object` 之外的 type 一律报错。
    pub fn from_json(value: &Value) -> AppResult<Self> {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::InvalidConfig(
                "response_format 形状非法: 缺少 type 字段".into(),
            ))?;
        match kind {
            "text" => Ok(Self::Text),
            "json_object" => Ok(Self::JsonObject),
            "json_schema" => {
                let inner = value.get("json_schema").ok_or_else(|| {
                    AppError::InvalidConfig(
                        "response_format 形状非法: json_schema 缺少 json_schema 对象".into(),
                    )
                })?;
                Ok(Self::JsonSchema {
                    name: inner
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    description: inner
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    schema: inner
                        .get("schema")
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object" })),
                    strict: inner.get("strict").and_then(Value::as_bool),
                })
            }
            other => Err(AppError::InvalidConfig(format!(
                "response_format 形状非法: 未知的 type {other:?}"
            ))),
        }
    }
}

/// `CanonicalToolChoice` 的 serde：按规范 JSON 存取，形状非法直接 400。
impl Serialize for CanonicalToolChoice {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CanonicalToolChoice {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Self::from_json(&value).map_err(serde::de::Error::custom)
    }
}

/// `CanonicalResponseFormat` 的 serde：按规范 JSON 存取。
impl Serialize for CanonicalResponseFormat {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CanonicalResponseFormat {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Self::from_json(&value).map_err(serde::de::Error::custom)
    }
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
    pub tool_choice: Option<CanonicalToolChoice>,
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
    /// 输出格式：类型化的规范形状（Text / JsonObject / JsonSchema），
    /// serde 反序列化在入站就拒绝非法形状（见 `CanonicalResponseFormat`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<CanonicalResponseFormat>,
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
    /// 客户端入站报文的**原文**（入站协议的本来形状）。
    ///
    /// 规范层为了统一，`raw` 是重建后的规范形状（OpenAI 入站时由 `decode_request` 重拼），
    /// 客户端的原文会在入站处留一份：同协议转发（入站协议 == 上游协议）时以它为准做免转换直通，
    /// 客户端自己的字段名、消息结构、协议扩展键才能原样带到上游。
    client_raw: Option<Value>,
    /// 被过滤器改写过的规范字段名。同协议直通只覆盖这些字段，其余以客户端原文为准
    /// ——否则注入的提示词会被客户端原文盖回去。
    dirty: BTreeSet<String>,
}

impl CanonicalRequest {
    pub fn parse(raw: Value) -> crate::error::AppResult<Self> {
        let body: RequestBody = serde_json::from_value(raw.clone()).map_err(|err| {
            crate::error::AppError::InvalidConfig(format!("请求体格式非法: {err}"))
        })?;
        Ok(Self {
            raw,
            body,
            client_raw: None,
            dirty: BTreeSet::new(),
        })
    }

    /// 记下客户端原始报文（网关在入站处调用，`decode_request` 之前的那份 JSON）。
    pub fn retain_client_raw(mut self, raw: Value) -> Self {
        self.client_raw = Some(raw);
        self
    }

    /// 客户端原始报文；没有保留过（内部构造的请求如翻译命令）时为 None。
    pub fn client_raw(&self) -> Option<&Value> {
        self.client_raw.as_ref()
    }

    /// 标记某个规范字段已被改写（过滤器用）。
    pub fn mark_dirty(&mut self, field: &str) {
        self.dirty.insert(field.to_string());
    }

    /// 该规范字段是否被过滤器改写过。
    pub fn is_dirty(&self, field: &str) -> bool {
        self.dirty.contains(field)
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
        match self.body.n {
            Some(0) => {
                return Err(AppError::InvalidConfig(
                    "n = 0 语义非法（一个候选都不生成），请去掉该字段或设为 1".into(),
                ));
            }
            Some(count) if count > 1 => {
                return Err(AppError::InvalidConfig(
                    "一次请求多个候选（n > 1）无法保真转发，请把 n 设为 1 或去掉该字段".into(),
                ));
            }
            _ => {}
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

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{json, Map, Value};

use crate::error::{AppError, AppResult};

pub type JsonMap = Map<String, Value>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

    /// 规范 JSON 块 → 类型化块。没建模的种类收进 `Unmodeled`（形状不丢）。
    pub fn canonical(&self) -> CanonicalBlock {
        match self.kind.as_str() {
            "text" => CanonicalBlock::Text(self.text_value()),
            "image" => CanonicalBlock::Image {
                source: self.field("source").cloned().unwrap_or(Value::Null),
            },
            "document" => {
                let mut extra = self.data.clone();
                let source = extra.remove("source").unwrap_or(Value::Null);
                CanonicalBlock::Document { source, extra }
            }
            "tool_use" => match self.tool_use() {
                Some(tool) => CanonicalBlock::ToolUse {
                    id: tool.id,
                    name: tool.name,
                    input: tool.input,
                },
                None => CanonicalBlock::Unmodeled(self.clone()),
            },
            "tool_result" => match self.tool_result() {
                Some(result) => CanonicalBlock::ToolResult {
                    tool_use_id: result.tool_use_id,
                    content: result.content,
                    is_error: result.is_error,
                },
                None => CanonicalBlock::Unmodeled(self.clone()),
            },
            "thinking" => CanonicalBlock::Thinking {
                thinking: self.str_field("thinking").unwrap_or_default().to_string(),
                signature: self.str_field("signature").map(str::to_string),
                redacted: None,
            },
            "redacted_thinking" => CanonicalBlock::Thinking {
                thinking: String::new(),
                signature: None,
                redacted: self.str_field("data").map(str::to_string),
            },
            _ => CanonicalBlock::Unmodeled(self.clone()),
        }
    }
}

impl MessageContent {
    /// 规范内容块（类型化）。字符串 content 视作单个 text 块。
    pub fn canonical_blocks(&self) -> Vec<CanonicalBlock> {
        self.blocks().iter().map(ContentBlock::canonical).collect()
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

/// 规范内容块的**种类**（`CanonicalBlock` 的判别式）。
///
/// 单独列出来是为了让「能力表 + 守卫测试」能遍历全集：往 `CanonicalBlock` 加变体却忘了
/// 在 `providers::wire` 的能力表里登记，`block_rules_cover_every_kind` 会直接失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // 判别式目前由能力表守卫测试消费
pub enum BlockKind {
    Text,
    Image,
    Document,
    ToolUse,
    ToolResult,
    Thinking,
    Unmodeled,
}

#[allow(dead_code)]
impl BlockKind {
    pub const ALL: &'static [BlockKind] = &[
        BlockKind::Text,
        BlockKind::Image,
        BlockKind::Document,
        BlockKind::ToolUse,
        BlockKind::ToolResult,
        BlockKind::Thinking,
        BlockKind::Unmodeled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            BlockKind::Text => "text",
            BlockKind::Image => "image",
            BlockKind::Document => "document",
            BlockKind::ToolUse => "tool_use",
            BlockKind::ToolResult => "tool_result",
            BlockKind::Thinking => "thinking",
            BlockKind::Unmodeled => "unmodeled",
        }
    }
}

/// 规范的**类型化内容块**：C 侧内容词汇的唯一真源。
///
/// 规范要表达的是三个协议的并集，而各家内容块形状各不相同。每个 provider 的编解码都按
/// 这个全集**穷举**匹配——新增一个块类型，编译器会把每一处没跟上的地方指出来；协议确实
/// 没有的形状则走各协议显式的「不支持」规则（见 `providers::wire`），而不是 `_ => {}` 落空。
#[derive(Debug, Clone, PartialEq)]
pub enum CanonicalBlock {
    Text(String),
    Image {
        source: Value,
    },
    /// 文档/文件（PDF、文本、表格…）。
    ///
    /// `source` 用 Anthropic 的 document source 形状（`base64` / `url` / `file` / `text` / `content`），
    /// `extra` 原样带走同一块上的其余字段（`title` / `context` / `cache_control` / `citations`），
    /// 保证规范 → Anthropic 的往返不丢东西。
    Document {
        source: Value,
        extra: JsonMap,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Value,
        is_error: bool,
    },
    Thinking {
        thinking: String,
        signature: Option<String>,
        /// `redacted_thinking` 的密文（Anthropic 专有），有值时按它还原。
        redacted: Option<String>,
    },
    /// C 未建模的块：形状原样保留（Anthropic 同协议直通与未知块 round-trip 依赖它），
    /// 编码到别的协议时按各协议的显式规则处理。
    Unmodeled(ContentBlock),
}

impl CanonicalBlock {
    /// 判别式，供能力表与守卫测试使用。
    #[allow(dead_code)]
    pub fn kind(&self) -> BlockKind {
        match self {
            Self::Text(_) => BlockKind::Text,
            Self::Image { .. } => BlockKind::Image,
            Self::Document { .. } => BlockKind::Document,
            Self::ToolUse { .. } => BlockKind::ToolUse,
            Self::ToolResult { .. } => BlockKind::ToolResult,
            Self::Thinking { .. } => BlockKind::Thinking,
            Self::Unmodeled(_) => BlockKind::Unmodeled,
        }
    }

    /// 类型化块 → 规范 JSON 值（`to_content_block` 的 serde 形式，供解码侧落 JSON 用）。
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self.to_content_block()).unwrap_or(Value::Null)
    }

    /// 类型化块 → 规范 JSON 块（也就是 C 的序列化形状，Anthropic 口径）。
    pub fn to_content_block(&self) -> ContentBlock {
        match self {
            Self::Text(text) => ContentBlock::text(text.clone()),
            Self::Image { source } => {
                let mut data = JsonMap::new();
                data.insert("source".into(), source.clone());
                ContentBlock::new("image", data)
            }
            Self::Document { source, extra } => {
                let mut data = extra.clone();
                data.insert("source".into(), source.clone());
                ContentBlock::new("document", data)
            }
            Self::ToolUse { id, name, input } => {
                let mut data = JsonMap::new();
                data.insert("id".into(), Value::String(id.clone()));
                data.insert("name".into(), Value::String(name.clone()));
                data.insert("input".into(), input.clone());
                ContentBlock::new("tool_use", data)
            }
            Self::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let mut data = JsonMap::new();
                data.insert("tool_use_id".into(), Value::String(tool_use_id.clone()));
                data.insert("content".into(), content.clone());
                // `is_error` 只在真出错时写：Anthropic 允许省略，缺省即 false。
                if *is_error {
                    data.insert("is_error".into(), Value::Bool(true));
                }
                ContentBlock::new("tool_result", data)
            }
            Self::Thinking {
                thinking,
                signature,
                redacted,
            } => {
                if let Some(redacted) = redacted {
                    let mut data = JsonMap::new();
                    data.insert("data".into(), Value::String(redacted.clone()));
                    return ContentBlock::new("redacted_thinking", data);
                }
                let mut data = JsonMap::new();
                data.insert("thinking".into(), Value::String(thinking.clone()));
                if let Some(signature) = signature {
                    data.insert("signature".into(), Value::String(signature.clone()));
                }
                ContentBlock::new("thinking", data)
            }
            Self::Unmodeled(block) => block.clone(),
        }
    }
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

/// 规范不变式：`messages` 里角色必须交替。
///
/// 消息边界不是任何协议的语义，而是各家对「一个模型回合」的不同切法：Responses 把一次回合的
/// 每个工具调用/结果各作为一个 `input` item，Completions 把每个结果各作为一条 `tool` 消息。
/// 原样转进规范就是若干条相邻同角色消息，而下游协议都要求交替——Anthropic 直接拒非交替的
/// user/assistant，OpenAI 系要求 `tool` 消息紧跟声明它的 assistant。所以在入规范时就并成一条，
/// 跨协议转换才拿得到 A、B 都支持的那部分。
fn merge_adjacent_messages(raw: &mut Value) {
    let Some(messages) = raw.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    let mut merged: Vec<Value> = Vec::with_capacity(messages.len());
    for message in messages.drain(..) {
        let role = message.get("role").and_then(Value::as_str);
        let same_role = role.is_some()
            && merged
                .last()
                .and_then(|last| last.get("role"))
                .and_then(Value::as_str)
                == role;
        if !same_role {
            merged.push(message);
            continue;
        }
        // 只在真的合并时才把 content 统一成块数组：没合并的消息保持客户端原样（字符串就是字符串）。
        let mut blocks = content_blocks(merged.last().and_then(|last| last.get("content")));
        blocks.extend(content_blocks(message.get("content")));
        if let Some(last) = merged.last_mut().and_then(Value::as_object_mut) {
            last.insert("content".into(), Value::Array(blocks));
        }
    }
    *messages = merged;
}

/// 消息 content 看成块数组：字符串包成一个 text 块（空串算没有），其余形状按空处理。
fn content_blocks(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(text)) if !text.is_empty() => {
            vec![json!({ "type": "text", "text": text })]
        }
        Some(Value::Array(blocks)) => blocks.clone(),
        _ => Vec::new(),
    }
}

fn body_from(raw: &Value) -> AppResult<RequestBody> {
    serde_json::from_value(raw.clone())
        .map_err(|err| AppError::InvalidConfig(format!("请求体格式非法: {err}")))
}

impl CanonicalRequest {
    pub fn parse(mut raw: Value) -> crate::error::AppResult<Self> {
        merge_adjacent_messages(&mut raw);
        let body = body_from(&raw)?;
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

    /// 规范 JSON 原文（过滤器改写的对象；出站编码不再读它——同协议直通用 `client_raw`，
    /// 跨协议用类型化 body）。
    #[allow(dead_code)]
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
        // 过滤器只改 system 这类标量，但改写后同样要保证消息形状，避免又出现相邻同角色。
        merge_adjacent_messages(&mut self.raw);
        self.body = body_from(&self.raw)?;
        Ok(self)
    }
}

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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
            tool_use_id: self.str_field("tool_use_id").unwrap_or_default().to_string(),
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
}

#[derive(Debug, Clone)]
pub struct CanonicalRequest {
    raw: Value,
    body: RequestBody,
}

impl CanonicalRequest {
    pub fn parse(raw: Value) -> crate::error::AppResult<Self> {
        let body: RequestBody = serde_json::from_value(raw.clone())
            .map_err(|err| crate::error::AppError::InvalidConfig(format!("请求体格式非法: {err}")))?;
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

    /// 在规范 JSON 上做改写，并据此重建类型化的 body，保证两者始终一致。
    /// 过滤器（请求转发前的改写）通过它修改请求。
    pub fn map_raw(
        mut self,
        f: impl FnOnce(&mut Value) -> crate::error::AppResult<()>,
    ) -> crate::error::AppResult<Self> {
        f(&mut self.raw)?;
        self.body = serde_json::from_value(self.raw.clone())
            .map_err(|err| crate::error::AppError::InvalidConfig(format!("请求体格式非法: {err}")))?;
        Ok(self)
    }
}

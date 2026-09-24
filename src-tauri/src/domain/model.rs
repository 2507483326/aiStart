use serde::{Deserialize, Serialize};

pub const DEFAULT_MAX_TOKENS: u32 = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelFormat {
    AnthropicMessages,
    OpenaiCompletions,
    OpenaiResponses,
}

impl ModelFormat {
    pub const ALL: [ModelFormat; 3] = [
        ModelFormat::AnthropicMessages,
        ModelFormat::OpenaiCompletions,
        ModelFormat::OpenaiResponses,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            ModelFormat::AnthropicMessages => "anthropic-messages",
            ModelFormat::OpenaiCompletions => "openai-completions",
            ModelFormat::OpenaiResponses => "openai-responses",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            ModelFormat::AnthropicMessages => "Anthropic Messages",
            ModelFormat::OpenaiCompletions => "OpenAI Chat Completions",
            ModelFormat::OpenaiResponses => "OpenAI Responses",
        }
    }

    pub fn default_base_url(&self) -> &'static str {
        match self {
            ModelFormat::AnthropicMessages => "https://api.anthropic.com",
            ModelFormat::OpenaiCompletions => "https://api.openai.com/v1",
            ModelFormat::OpenaiResponses => "https://api.openai.com/v1",
        }
    }

    pub fn parse(value: &str) -> ModelFormat {
        match value {
            "anthropic-messages" => ModelFormat::AnthropicMessages,
            "openai-responses" => ModelFormat::OpenaiResponses,
            _ => ModelFormat::OpenaiCompletions,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelConfig {
    pub id: i64,
    pub name: String,
    pub format: ModelFormat,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    pub model: String,
    #[serde(default)]
    pub supports_1m: bool,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

impl ModelConfig {
    pub fn completion_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        let path = match self.format {
            ModelFormat::AnthropicMessages => "/v1/messages",
            ModelFormat::OpenaiCompletions => "/chat/completions",
            ModelFormat::OpenaiResponses => "/responses",
        };
        if base.ends_with("/v1") && path.starts_with("/v1/") {
            format!("{}{}", base, &path[3..])
        } else {
            format!("{}{}", base, path)
        }
    }

    pub fn models_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{base}/models")
        } else {
            format!("{base}/v1/models")
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInput {
    #[serde(default)]
    pub id: Option<i64>,
    pub name: String,
    pub format: ModelFormat,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    pub model: String,
    #[serde(default)]
    pub supports_1m: bool,
}

use serde::{Deserialize, Serialize};

/// 客户端没给输出上限、而上游协议又必填时的兜底值（三个协议里只有 Anthropic Messages 的
/// `max_tokens` 是必填）。取值保守：几乎被所有模型与网关接受；更高的默认会被输出上限较低的
/// 网关直接 400，而那种失败不可重试。
pub const DEFAULT_MAX_TOKENS: u32 = 8192;

/// 勾选「支持 1M 上下文」的模型用的兜底输出上限——上下文够大，输出也放宽到 64k 量级。
pub const LARGE_MAX_TOKENS: u32 = 64000;

/// 模型没有显式输出上限时的兜底值：勾了「支持 1M 上下文」→ 64000，否则 8192。
/// 保存模型时按这个口径写进 `models.max_output_tokens`（用户改 1M 开关即同步更新）。
pub fn default_max_output_tokens(supports_1m: bool) -> u32 {
    if supports_1m {
        LARGE_MAX_TOKENS
    } else {
        DEFAULT_MAX_TOKENS
    }
}

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
    /// 客户端没给输出上限时的兜底值：勾了「支持 1M 上下文」→ 64000，否则 8192。
    /// 保存在 `models.max_output_tokens`，随 1M 开关同步更新；另两个协议（输出上限可选）不用它。
    #[serde(default = "default_max_output_tokens_value")]
    pub max_output_tokens: u32,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

fn default_max_output_tokens_value() -> u32 {
    DEFAULT_MAX_TOKENS
}

impl ModelConfig {
    /// Anthropic 上游的 `max_tokens` 是必填：客户端给了就用客户端的，没给才用模型自己的兜底值
    ///（`models.max_output_tokens`，随「支持 1M 上下文」开关同步更新）。另两个协议的输出上限
    /// 是可选的，一律不兜——替客户端设上限会把长回答 / 思考量大的回答悄悄截断。
    pub fn anthropic_max_tokens(&self, requested: Option<u32>) -> u32 {
        requested.unwrap_or(self.max_output_tokens)
    }

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
    /// 显式指定输出上限兜底值；不填（前端不传）时按 1M 开关推导。
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
}

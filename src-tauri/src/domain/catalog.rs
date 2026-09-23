use crate::domain::app::{AppDescriptor, AppKind, ApplyMode};
use crate::domain::model::{ModelFormat, ModelPreset};

pub fn builtin_apps() -> Vec<AppDescriptor> {
    vec![
        AppDescriptor {
            kind: AppKind::ClaudeDesktop,
            name: "Claude Desktop".into(),
            publisher: "Anthropic".into(),
            description: "Anthropic 官方桌面客户端。通过第三方推理（3P）网关接入任意上游模型。"
                .into(),
            download_page: "https://claude.ai/download".into(),
            requires_gateway: true,
            apply_mode: ApplyMode::Gateway,
            config_target: "HKCU\\SOFTWARE\\Policies\\Claude".into(),
            installer_url: None,
            installer_sha256: None,
            latest_version: None,
        },
        AppDescriptor {
            kind: AppKind::DeepseekDesktop,
            name: "DeepSeek Desktop".into(),
            publisher: "DeepSeek".into(),
            description: "DeepSeek 桌面客户端。通过写入配置文件接入 OpenAI 兼容上游，目标路径可在设置中调整。"
                .into(),
            download_page: "https://www.deepseek.com/".into(),
            requires_gateway: false,
            apply_mode: ApplyMode::DirectConfig,
            config_target: "%APPDATA%\\DeepSeek\\config.json".into(),
            installer_url: None,
            installer_sha256: None,
            latest_version: None,
        },
    ]
}

pub fn builtin_app(kind: AppKind) -> AppDescriptor {
    builtin_apps()
        .into_iter()
        .find(|app| app.kind == kind)
        .expect("builtin catalog always contains every AppKind")
}

pub fn builtin_model_presets() -> Vec<ModelPreset> {
    vec![
        ModelPreset {
            name: "Claude Sonnet (Anthropic)".into(),
            format: ModelFormat::AnthropicMessages,
            base_url: "https://api.anthropic.com".into(),
            model: "claude-sonnet-4-5".into(),
            note: "Anthropic 官方 Messages 协议，网关直通转发".into(),
        },
        ModelPreset {
            name: "DeepSeek Chat (OpenAI 兼容)".into(),
            format: ModelFormat::OpenaiCompletions,
            base_url: "https://api.deepseek.com/v1".into(),
            model: "deepseek-chat".into(),
            note: "OpenAI Chat Completions 协议，网关自动与 Anthropic 互转".into(),
        },
        ModelPreset {
            name: "DeepSeek Reasoner (OpenAI 兼容)".into(),
            format: ModelFormat::OpenaiCompletions,
            base_url: "https://api.deepseek.com/v1".into(),
            model: "deepseek-reasoner".into(),
            note: "推理模型，思维链内容会被翻译为 Anthropic thinking 块".into(),
        },
        ModelPreset {
            name: "OpenAI GPT (Responses)".into(),
            format: ModelFormat::OpenaiResponses,
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-5".into(),
            note: "OpenAI 新一代 Responses 协议，事件流会翻译为 Anthropic SSE".into(),
        },
        ModelPreset {
            name: "本地 Ollama (OpenAI 兼容)".into(),
            format: ModelFormat::OpenaiCompletions,
            base_url: "http://127.0.0.1:11434/v1".into(),
            model: "qwen3:32b".into(),
            note: "本地推理，无需 API Key".into(),
        },
        ModelPreset {
            name: "LiteLLM 代理 (Anthropic 入口)".into(),
            format: ModelFormat::AnthropicMessages,
            base_url: "http://127.0.0.1:4000".into(),
            model: "claude-sonnet-4-5".into(),
            note: "已有 LiteLLM / Portkey 网关时，直接串联转发".into(),
        },
    ]
}

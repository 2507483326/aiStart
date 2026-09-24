use crate::domain::app::{AppDescriptor, AppKind, ApplyMode};

pub fn builtin_apps() -> Vec<AppDescriptor> {
    vec![
        AppDescriptor {
            kind: AppKind::ClaudeDesktop,
            name: "Claude Desktop".into(),
            publisher: "Anthropic".into(),
            description: "Anthropic 官方桌面客户端。通过第三方推理（3P）网关接入任意上游模型。"
                .into(),
            download_page: "https://claude.ai/download".into(),
            homepage: "https://claude.ai/".into(),
            requires_gateway: true,
            apply_mode: ApplyMode::Gateway,
            config_target: r"%LOCALAPPDATA%\Claude-3p\configLibrary".into(),
            installer_url: None,
            installer_sha256: None,
            latest_version_urls: vec![
                "https://downloads.claude.ai/releases/win32/x64/RELEASES".into(),
                "https://api.github.com/repos/Wangnov/claude-app-mirror/releases/latest".into(),
            ],
        },
        AppDescriptor {
            kind: AppKind::DeepseekDesktop,
            name: "DeepSeek Desktop".into(),
            publisher: "DeepSeek".into(),
            description: "DeepSeek 桌面客户端。通过写入配置文件接入 OpenAI 兼容上游，目标路径可在设置中调整。"
                .into(),
            download_page: "https://www.deepseek.com/".into(),
            homepage: "https://www.deepseek.com/".into(),
            requires_gateway: false,
            apply_mode: ApplyMode::DirectConfig,
            config_target: "%APPDATA%\\DeepSeek\\config.json".into(),
            installer_url: None,
            installer_sha256: None,
            latest_version_urls: vec![
                "https://api.github.com/repos/anywhere-labs/deepseek-harness-desktop/releases/latest"
                    .into(),
            ],
        },
    ]
}

pub fn builtin_app(kind: AppKind) -> AppDescriptor {
    builtin_apps()
        .into_iter()
        .find(|app| app.kind == kind)
        .expect("builtin catalog always contains every AppKind")
}

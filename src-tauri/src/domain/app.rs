use serde::{Deserialize, Serialize};

use crate::domain::release::UpgradeSpec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppKind {
    ClaudeDesktop,
    DeepseekDesktop,
    Codex,
    // `rename_all = "kebab-case"` 会把 `ZCode` 变成 `z-code`、`WorkBuddy` 变成 `work-buddy`，
    // 与前端 AppKind 联合类型要对上的 "zcode" / "workbuddy" 不符，故显式覆写。
    #[serde(rename = "zcode")]
    ZCode,
    #[serde(rename = "workbuddy")]
    WorkBuddy,
}

impl AppKind {
    pub const ALL: [AppKind; 5] = [
        AppKind::ClaudeDesktop,
        AppKind::DeepseekDesktop,
        AppKind::Codex,
        AppKind::ZCode,
        AppKind::WorkBuddy,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            AppKind::ClaudeDesktop => "claude-desktop",
            AppKind::DeepseekDesktop => "deepseek-desktop",
            AppKind::Codex => "codex",
            AppKind::ZCode => "zcode",
            AppKind::WorkBuddy => "workbuddy",
        }
    }

    /// 该应用接入网关时使用的专属 Key：固定可读、不加前缀，直接取 app_kind。
    /// 网关据此把入站请求匹配回来源应用。
    pub fn gateway_token(&self) -> &'static str {
        self.as_str()
    }

    pub fn parse(value: &str) -> Option<AppKind> {
        AppKind::ALL
            .iter()
            .copied()
            .find(|kind| kind.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApplyMode {
    Gateway,
    DirectConfig,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppDescriptor {
    pub kind: AppKind,
    pub name: String,
    pub publisher: String,
    pub description: String,
    pub homepage: String,
    pub download_page: String,
    pub requires_gateway: bool,
    pub apply_mode: ApplyMode,
    pub config_target: String,
    /// 版本探测源（有序：官方优先、镜像兜底）。只用来回答「有没有新版本」。
    pub latest_version_urls: Vec<String>,
    /// 安装/升级规格：去哪找安装包、怎么静默装、怎么诊断锚定。
    ///
    /// 与 `latest_version_urls` 是**两件事**：官方下载地址常常只是在线引导器
    /// （Claude 的 `ClaudeSetup.exe` 约 7MB，装的时候还会回 GCS 重下真正的 MSIX），
    /// 所以「探测版本」和「拿安装包」必须分开配置。
    pub upgrade: UpgradeSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdate {
    pub kind: AppKind,
    pub latest_version: Option<String>,
    pub update_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolApp {
    pub kind: AppKind,
    pub name: String,
    pub publisher: String,
    pub description: String,
    pub homepage: String,
    pub download_page: String,
    pub requires_gateway: bool,
    pub apply_mode: ApplyMode,
    pub config_target: String,
    pub api_key: String,
    pub installed: bool,
    pub version: Option<String>,
    pub install_location: Option<String>,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub applied_model_id: Option<i64>,
    pub applied_model_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyReport {
    pub kind: AppKind,
    pub model_id: i64,
    pub model_name: String,
    pub apply_mode: ApplyMode,
    pub target: String,
    pub restart_required: bool,
    pub steps: Vec<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallReport {
    pub kind: AppKind,
    pub action: String,
    pub target: String,
    pub launched: bool,
    pub steps: Vec<String>,
}

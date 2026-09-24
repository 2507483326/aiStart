use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppKind {
    ClaudeDesktop,
    DeepseekDesktop,
}

impl AppKind {
    pub const ALL: [AppKind; 2] = [AppKind::ClaudeDesktop, AppKind::DeepseekDesktop];

    pub fn as_str(&self) -> &'static str {
        match self {
            AppKind::ClaudeDesktop => "claude-desktop",
            AppKind::DeepseekDesktop => "deepseek-desktop",
        }
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
    pub installer_url: Option<String>,
    pub installer_sha256: Option<String>,
    pub latest_version_urls: Vec<String>,
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

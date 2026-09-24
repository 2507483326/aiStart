#[cfg(windows)]
pub mod dsh;

pub mod manual;

pub mod workbuddy;

#[cfg(windows)]
pub mod windows;

#[cfg(not(windows))]
pub mod fallback;

use crate::domain::app::{AppDescriptor, AppKind, ApplyReport};
use crate::domain::model::ModelConfig;
use crate::error::AppResult;

/// A route a client can be pointed at: the id it sends, plus the label its
/// model picker shows.
pub struct ModelChoice {
    pub id: String,
    pub label: String,
}

/// 网关不按请求里的 model 路由，所以只认单个模型入口的客户端统一用这个名字。
pub const GATEWAY_ALIAS: &str = "aiStart";

/// 大多数客户端只需要一个网关入口，不用逐个认识网关的路由。
pub fn gateway_alias_choice() -> ModelChoice {
    ModelChoice {
        id: GATEWAY_ALIAS.to_string(),
        label: GATEWAY_ALIAS.to_string(),
    }
}

pub struct ApplyContext {
    pub model: ModelConfig,
    pub gateway_base_url: String,
    pub gateway_token: String,
    /// 由 [`AppConfigurator::exposed_models`] 按客户端类型填好。
    pub model_choices: Vec<ModelChoice>,
}

impl ApplyContext {
    /// Model id for clients that only take a single name.
    pub fn gateway_model_id(&self) -> &str {
        self.model_choices
            .first()
            .map(|choice| choice.id.as_str())
            .unwrap_or(GATEWAY_ALIAS)
    }
}

pub struct DetectResult {
    pub installed: bool,
    pub version: Option<String>,
    pub location: Option<String>,
}

impl DetectResult {
    pub fn missing() -> Self {
        Self {
            installed: false,
            version: None,
            location: None,
        }
    }

    pub fn found(location: impl Into<String>, version: Option<String>) -> Self {
        Self {
            installed: true,
            version,
            location: Some(location.into()),
        }
    }
}

pub trait AppConfigurator: Send + Sync {
    fn descriptor(&self) -> AppDescriptor;
    fn detect(&self) -> AppResult<DetectResult>;
    fn is_configured(&self) -> AppResult<bool>;

    /// 该客户端在模型选择器里应当看到的条目。暴露几条由客户端自己决定：
    /// Claude Desktop 只认 Anthropic 形状的档位路由，其余客户端一个入口就够。
    fn exposed_models(&self) -> Vec<ModelChoice>;

    fn apply(&self, ctx: &ApplyContext) -> AppResult<ApplyReport>;
    fn clear(&self) -> AppResult<()>;
}

#[cfg(windows)]
pub fn configurator_for(kind: AppKind) -> Box<dyn AppConfigurator> {
    match kind {
        AppKind::ClaudeDesktop => Box::new(windows::ClaudeDesktopConfigurator),
        AppKind::DeepseekDesktop => Box::new(windows::DeepseekDesktopConfigurator),
        AppKind::WorkBuddy => Box::new(workbuddy::WorkBuddyConfigurator),
        AppKind::Codex | AppKind::ZCode => Box::new(manual::ManualConfigurator::new(kind)),
    }
}

#[cfg(not(windows))]
pub fn configurator_for(kind: AppKind) -> Box<dyn AppConfigurator> {
    match kind {
        AppKind::WorkBuddy => Box::new(workbuddy::WorkBuddyConfigurator),
        AppKind::Codex | AppKind::ZCode => Box::new(manual::ManualConfigurator::new(kind)),
        _ => Box::new(fallback::UnsupportedConfigurator::new(kind)),
    }
}

pub fn expand_env(raw: &str) -> String {
    let mut result = raw.to_string();
    let mut rest = result.clone();
    while let Some(start) = rest.find('%') {
        let Some(end_rel) = rest[start + 1..].find('%') else {
            break;
        };
        let key = &rest[start + 1..start + 1 + end_rel];
        let value = std::env::var(key).unwrap_or_default();
        result = result.replacen(&format!("%{key}%"), &value, 1);
        rest = result.clone();
    }
    result
}

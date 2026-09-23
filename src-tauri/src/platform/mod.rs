#[cfg(windows)]
pub mod windows;

#[cfg(not(windows))]
pub mod fallback;

use crate::domain::app::{AppDescriptor, AppKind, ApplyReport};
use crate::domain::model::ModelConfig;
use crate::error::AppResult;

pub struct ApplyContext {
    pub model: ModelConfig,
    pub gateway_base_url: String,
    pub gateway_token: String,
    pub model_alias: String,
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
    fn apply(&self, ctx: &ApplyContext) -> AppResult<ApplyReport>;
    fn clear(&self) -> AppResult<()>;
}

#[cfg(windows)]
pub fn configurator_for(kind: AppKind) -> Box<dyn AppConfigurator> {
    match kind {
        AppKind::ClaudeDesktop => Box::new(windows::ClaudeDesktopConfigurator),
        AppKind::DeepseekDesktop => Box::new(windows::DeepseekDesktopConfigurator),
    }
}

#[cfg(not(windows))]
pub fn configurator_for(kind: AppKind) -> Box<dyn AppConfigurator> {
    Box::new(fallback::UnsupportedConfigurator::new(kind))
}

fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

pub fn model_alias(model: &ModelConfig) -> String {
    let slug = slugify(&model.name);
    if slug.is_empty() {
        let short = &model.id[..model.id.len().min(8)];
        format!("ai-start-{short}")
    } else {
        format!("ai-start-{slug}")
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

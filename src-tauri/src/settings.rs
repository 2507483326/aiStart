use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::domain::app::AppKind;
use crate::domain::model::{ModelConfig, ModelFormat, ModelInput};
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub models: Vec<ModelConfig>,
    #[serde(default)]
    pub active_model_id: Option<String>,
    #[serde(default = "default_port")]
    pub gateway_port: u16,
    #[serde(default)]
    pub deepseek_config_path: Option<String>,
    #[serde(default)]
    pub auto_failover: bool,
    #[serde(default)]
    pub applied: BTreeMap<String, String>,
}

fn default_port() -> u16 {
    8931
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            models: Vec::new(),
            active_model_id: None,
            gateway_port: default_port(),
            deepseek_config_path: None,
            auto_failover: false,
            applied: BTreeMap::new(),
        }
    }
}

impl Settings {
    pub fn active_model(&self) -> Option<&ModelConfig> {
        let id = self.active_model_id.as_ref()?;
        self.models.iter().find(|model| &model.id == id)
    }

    pub fn applied_model(&self, kind: AppKind) -> Option<&ModelConfig> {
        let id = self.applied.get(kind.as_str())?;
        self.models.iter().find(|model| &model.id == id)
    }

    pub fn candidate_models(&self) -> Vec<ModelConfig> {
        let mut list: Vec<ModelConfig> = Vec::new();
        if let Some(active) = self.active_model() {
            list.push(active.clone());
        }
        if !self.auto_failover {
            return list;
        }
        for model in &self.models {
            if self.active_model_id.as_deref() == Some(model.id.as_str()) {
                continue;
            }
            list.push(model.clone());
        }
        list
    }

    pub fn upsert(&mut self, input: ModelInput) -> ModelConfig {
        let now = chrono::Local::now().to_rfc3339();
        let existing = input
            .id
            .as_ref()
            .and_then(|id| self.models.iter().position(|model| &model.id == id));

        let id = input
            .id
            .clone()
            .unwrap_or_else(|| format!("mdl_{}", uuid::Uuid::new_v4().simple()));

        let created_at = existing
            .map(|index| self.models[index].created_at.clone())
            .unwrap_or_else(|| now.clone());

        let config = ModelConfig {
            id: id.clone(),
            name: input.name,
            format: input.format,
            base_url: input.base_url,
            api_key: input.api_key,
            model: input.model,
            supports_1m: input.supports_1m,
            created_at,
            updated_at: now,
        };

        match existing {
            Some(index) => self.models[index] = config.clone(),
            None => self.models.push(config.clone()),
        }

        if self.active_model_id.is_none() {
            self.active_model_id = Some(id);
        }

        config
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.models.len();
        self.models.retain(|model| model.id != id);
        if self.active_model_id.as_deref() == Some(id) {
            self.active_model_id = self.models.first().map(|model| model.id.clone());
        }
        self.applied.retain(|_, value| value != id);
        self.models.len() != before
    }
}

static DIR: OnceLock<PathBuf> = OnceLock::new();
static STORE: OnceLock<RwLock<Settings>> = OnceLock::new();

fn store() -> &'static RwLock<Settings> {
    STORE.get_or_init(|| RwLock::new(Settings::default()))
}

fn settings_file() -> Option<PathBuf> {
    DIR.get().map(|dir| dir.join("settings.json"))
}

pub fn data_dir() -> Option<PathBuf> {
    DIR.get().cloned()
}

pub fn init(dir: PathBuf) -> AppResult<()> {
    std::fs::create_dir_all(&dir)?;
    let _ = DIR.set(dir.clone());

    let path = dir.join("settings.json");
    let loaded = if path.exists() {
        let raw = std::fs::read_to_string(&path)?;
        serde_json::from_str::<Settings>(&raw).unwrap_or_default()
    } else {
        Settings::default()
    };

    let mut settings = loaded;
    seed_default_models(&mut settings);

    *store().write().expect("settings lock poisoned") = settings;
    persist()
}

fn seed_default_models(settings: &mut Settings) {
    if !settings.models.is_empty() {
        return;
    }
    let now = chrono::Local::now().to_rfc3339();
    let seeds = [
        (
            "DeepSeek Chat（示例）",
            ModelFormat::OpenaiCompletions,
            "https://api.deepseek.com/v1",
            "deepseek-chat",
        ),
        (
            "Claude Sonnet（示例）",
            ModelFormat::AnthropicMessages,
            "https://api.anthropic.com",
            "claude-sonnet-4-5",
        ),
    ];

    for (name, format, base_url, model) in seeds {
        let id = format!("mdl_{}", uuid::Uuid::new_v4().simple());
        settings.models.push(ModelConfig {
            id: id.clone(),
            name: name.into(),
            format,
            base_url: base_url.into(),
            api_key: String::new(),
            model: model.into(),
            supports_1m: false,
            created_at: now.clone(),
            updated_at: now.clone(),
        });
        if settings.active_model_id.is_none() {
            settings.active_model_id = Some(id);
        }
    }
}

pub fn persist() -> AppResult<()> {
    let Some(path) = settings_file() else {
        return Ok(());
    };
    let snapshot = store().read().expect("settings lock poisoned").clone();
    let raw = serde_json::to_string_pretty(&snapshot)?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, raw)?;
    std::fs::rename(&temp, &path)?;
    Ok(())
}

pub fn snapshot() -> Settings {
    store().read().expect("settings lock poisoned").clone()
}

pub fn mutate<T>(f: impl FnOnce(&mut Settings) -> T) -> AppResult<T> {
    let value = {
        let mut guard = store().write().expect("settings lock poisoned");
        f(&mut guard)
    };
    persist()?;
    Ok(value)
}

pub fn deepseek_config_path() -> String {
    let settings = snapshot();
    if let Some(path) = settings.deepseek_config_path.filter(|path| !path.trim().is_empty()) {
        return path;
    }
    let default = format!(
        r"{}\DeepSeek\config.json",
        std::env::var("APPDATA").unwrap_or_else(|_| ".".into())
    );
    crate::platform::expand_env(&default)
}

pub fn require_model(id: &str) -> AppResult<ModelConfig> {
    snapshot()
        .models
        .iter()
        .find(|model| model.id == id)
        .cloned()
        .ok_or_else(|| AppError::NotFound(format!("模型 {id} 不存在")))
}

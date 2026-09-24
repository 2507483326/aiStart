use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::error::AppResult;
use crate::gateway;
use crate::settings;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub gateway_port: u16,
    pub gateway_token: String,
    pub deepseek_config_path: String,
    pub auto_failover: bool,
    pub active_model_id: Option<i64>,
    pub applied: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsInput {
    #[serde(default)]
    pub gateway_port: Option<u16>,
    #[serde(default)]
    pub deepseek_config_path: Option<String>,
    #[serde(default)]
    pub auto_failover: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub arch: String,
    pub config_dir: String,
}

fn view() -> SettingsView {
    let snapshot = settings::snapshot();
    SettingsView {
        gateway_port: snapshot.gateway_port,
        gateway_token: gateway::GATEWAY_TOKEN.to_string(),
        deepseek_config_path: settings::deepseek_config_path(),
        auto_failover: snapshot.auto_failover,
        active_model_id: snapshot.active_model_id,
        applied: snapshot.applied,
    }
}

#[tauri::command]
pub fn get_settings() -> SettingsView {
    view()
}

#[tauri::command]
pub fn update_settings(input: SettingsInput) -> AppResult<SettingsView> {
    let was_running = gateway::status().running;
    let port_changed = input
        .gateway_port
        .map(|port| port != settings::snapshot().gateway_port)
        .unwrap_or(false);

    settings::mutate(|store| {
        if let Some(port) = input.gateway_port {
            store.gateway_port = port;
        }
        if let Some(path) = &input.deepseek_config_path {
            store.deepseek_config_path = Some(path.clone());
        }
        if let Some(enabled) = input.auto_failover {
            store.auto_failover = enabled;
        }
    })?;

    if was_running && port_changed {
        gateway::restart()?;
    }

    Ok(view())
}

#[tauri::command]
pub fn app_info(app: AppHandle) -> AppInfo {
    let config_dir = app
        .path()
        .app_config_dir()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();

    AppInfo {
        name: "AI Start".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        platform: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        config_dir,
    }
}

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;

use crate::autostart;
use crate::error::{AppError, AppResult};
use crate::gateway;
use crate::providers;
use crate::settings;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub gateway_port: u16,
    pub auto_failover: bool,
    pub launch_at_login: bool,
    pub proxy_enabled: bool,
    pub proxy_url: String,
    pub request_retention_days: i64,
    pub active_model_id: Option<i64>,
    pub applied: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsInput {
    #[serde(default)]
    pub gateway_port: Option<u16>,
    #[serde(default)]
    pub auto_failover: Option<bool>,
    #[serde(default)]
    pub launch_at_login: Option<bool>,
    /// 代理开关：关掉即直连（地址仍保留，下次打开接着用）。
    #[serde(default)]
    pub proxy_enabled: Option<bool>,
    /// 空串 = 清空代理地址；不带这个字段则保持原样。
    #[serde(default)]
    pub proxy_url: Option<String>,
    /// 请求报文保留天数：7 / 30 / 100，0 = 永久保留。
    #[serde(default)]
    pub request_retention_days: Option<i64>,
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
        auto_failover: snapshot.auto_failover,
        launch_at_login: snapshot.launch_at_login,
        proxy_enabled: snapshot.proxy_enabled,
        proxy_url: snapshot.proxy_url,
        request_retention_days: snapshot.request_retention_days,
        active_model_id: snapshot.active_model_id,
        applied: snapshot.applied,
    }
}

#[tauri::command]
pub fn get_settings() -> SettingsView {
    view()
}

#[tauri::command]
pub async fn update_settings(input: SettingsInput) -> AppResult<SettingsView> {
    let was_running = gateway::state() == gateway::GatewayState::Running;
    let port_changed = input
        .gateway_port
        .map(|port| port != settings::snapshot().gateway_port)
        .unwrap_or(false);

    // 代理地址先校验：写错了当场告诉用户，也不让脏值落库。
    // `Some(None)` = 显式清空成直连，`None` = 这次没动它。
    let proxy_url = match input.proxy_url.as_deref() {
        Some(raw) => Some(providers::validate_proxy(raw)?),
        None => None,
    };

    // 开关和地址是两个字段，但要合起来看：开着却没有地址等于在直连。
    let current = settings::snapshot();
    let proxy_enabled = resolve_proxy(
        input.proxy_enabled,
        input.proxy_url.as_deref(),
        current.proxy_enabled,
        &current.proxy_url,
    )?;

    // 开机启动先写注册表，再落库：写不进去就别把开关记成「已开启」。
    // 值没变就不碰注册表（每次启动时的同步会兜住「注册表被外部清掉」的情况）。
    if let Some(enabled) = input.launch_at_login {
        if enabled != settings::snapshot().launch_at_login {
            autostart::apply(enabled)?;
        }
    }

    // 只认下拉框里的四个值：写进来别的天数没有对应的 UI，不如当场拒绝。
    if let Some(days) = input.request_retention_days {
        if !settings::RETENTION_DAY_OPTIONS.contains(&days) {
            return Err(AppError::InvalidConfig("请求保存时间取值非法".into()));
        }
    }

    settings::mutate(|store| {
        if let Some(port) = input.gateway_port {
            store.gateway_port = port;
        }
        if let Some(enabled) = input.auto_failover {
            store.auto_failover = enabled;
        }
        if let Some(enabled) = input.launch_at_login {
            store.launch_at_login = enabled;
        }
        if let Some(days) = input.request_retention_days {
            store.request_retention_days = days;
        }
        store.proxy_enabled = proxy_enabled;
        if let Some(url) = proxy_url {
            store.proxy_url = url.unwrap_or_default();
        }
    })?;

    // 代理开关/地址都不用重启网关：出站客户端按请求重建（providers::http_client）。
    if was_running && port_changed {
        gateway::restart_async().await?;
    }

    Ok(view())
}

/// 开关 + 地址合起来该怎么落库，返回这次保存之后开关的状态。
///
/// 只有一种组合要拦：合完之后「开着但没地址」。那时出站流量其实在走直连，
/// 用户却以为配好了代理——这种静默不生效比报错难查得多，所以宁可保存时就失败。
/// 「开着但这次没带地址字段、库里已有」不算数；「关掉」则地址可留可清。
pub(crate) fn resolve_proxy(
    enabled: Option<bool>,
    next_url: Option<&str>,
    current_enabled: bool,
    current_url: &str,
) -> AppResult<bool> {
    let next_enabled = enabled.unwrap_or(current_enabled);
    let has_url = match next_url {
        Some(raw) => !raw.trim().is_empty(),
        None => !current_url.trim().is_empty(),
    };
    if next_enabled && !has_url {
        return Err(AppError::InvalidConfig("请先填写代理地址再打开代理".into()));
    }
    Ok(next_enabled)
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

/// 在资源管理器里打开应用数据目录（配置与数据库都放在这里）。
#[tauri::command]
pub fn open_data_dir(app: AppHandle) -> AppResult<()> {
    let directory = app
        .path()
        .app_config_dir()
        .map_err(|error| AppError::Message(format!("无法定位数据目录: {error}")))?;
    // 首次运行时目录还没建出来：先建出来，否则打开会失败。
    std::fs::create_dir_all(&directory)?;
    app.opener()
        .open_path(directory.to_string_lossy().to_string(), None::<&str>)
        .map_err(|error| AppError::Message(format!("打开数据目录失败: {error}")))
}

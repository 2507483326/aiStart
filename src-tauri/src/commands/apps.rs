use futures_util::StreamExt;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_opener::OpenerExt;
use tokio::io::AsyncWriteExt;

use crate::domain::app::{AppKind, ApplyMode, ApplyReport, InstallReport, ToolApp};
use crate::domain::catalog;
use crate::error::{AppError, AppResult};
use crate::gateway;
use crate::platform::{self, ApplyContext, DetectResult};
use crate::settings;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    kind: AppKind,
    action: String,
    phase: String,
    received: u64,
    total: Option<u64>,
    percent: Option<f64>,
}

fn build_app_view(kind: AppKind, applied: Option<&crate::domain::model::ModelConfig>) -> ToolApp {
    let configurator = platform::configurator_for(kind);
    let descriptor = configurator.descriptor();
    let detect = configurator.detect().unwrap_or_else(|_| DetectResult::missing());
    let applied = applied.filter(|_| configurator.is_configured().unwrap_or(false));

    ToolApp {
        kind,
        name: descriptor.name,
        publisher: descriptor.publisher,
        description: descriptor.description,
        download_page: descriptor.download_page,
        requires_gateway: descriptor.requires_gateway,
        apply_mode: descriptor.apply_mode,
        config_target: descriptor.config_target,
        installed: detect.installed,
        version: detect.version,
        install_location: detect.location,
        latest_version: descriptor.latest_version,
        update_available: false,
        applied_model_id: applied.map(|model| model.id.clone()),
        applied_model_name: applied.map(|model| model.name.clone()),
    }
}

#[tauri::command]
pub fn list_apps() -> AppResult<Vec<ToolApp>> {
    let settings = settings::snapshot();
    Ok(AppKind::ALL
        .iter()
        .map(|kind| build_app_view(*kind, settings.applied_model(*kind)))
        .collect())
}

#[tauri::command]
pub fn apply_model(kind: AppKind, model_id: Option<String>) -> AppResult<ApplyReport> {
    let settings = settings::snapshot();
    let model = match model_id {
        Some(id) => crate::settings::require_model(&id)?,
        None => settings
            .active_model()
            .cloned()
            .ok_or_else(|| AppError::NotFound("请先添加并启用一个模型".into()))?,
    };

    let configurator = platform::configurator_for(kind);
    if configurator.descriptor().requires_gateway {
        gateway::ensure_running()?;
    }
    let status = gateway::status();

    let context = ApplyContext {
        model: model.clone(),
        gateway_base_url: status.base_url.clone(),
        gateway_token: status.token.clone(),
        model_alias: platform::model_alias(&model),
    };

    let report = configurator.apply(&context)?;

    settings::mutate(|settings| {
        settings.applied.insert(kind.as_str().to_string(), model.id.clone());
    })?;

    Ok(report)
}

#[tauri::command]
pub fn clear_app_model(kind: AppKind) -> AppResult<()> {
    let configurator = platform::configurator_for(kind);
    configurator.clear()?;
    settings::mutate(|settings| {
        settings.applied.remove(kind.as_str());
    })?;
    Ok(())
}

async fn download_installer(
    app: &AppHandle,
    kind: AppKind,
    action: &str,
    url: &str,
) -> AppResult<String> {
    let directory = app
        .path()
        .app_cache_dir()
        .map_err(|error| AppError::Message(format!("无法定位缓存目录: {error}")))?
        .join("installers");
    std::fs::create_dir_all(&directory)?;

    let file_name = url
        .rsplit('/')
        .next()
        .filter(|name| name.contains('.'))
        .unwrap_or("installer.exe")
        .to_string();
    let destination = directory.join(file_name);

    let response = crate::providers::http_client().get(url).send().await?;
    if !response.status().is_success() {
        return Err(AppError::Message(format!(
            "下载安装包失败: HTTP {}",
            response.status().as_u16()
        )));
    }

    let total = response.content_length();
    let mut file = tokio::fs::File::create(&destination).await?;
    let mut stream = response.bytes_stream();
    let mut received: u64 = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        received += chunk.len() as u64;
        file.write_all(&chunk).await?;
        let _ = app.emit(
            "install://progress",
            DownloadProgress {
                kind,
                action: action.to_string(),
                phase: "downloading".into(),
                received,
                total,
                percent: total.map(|total| received as f64 / total as f64 * 100.0),
            },
        );
    }
    file.flush().await?;

    Ok(destination.to_string_lossy().to_string())
}

fn launch_installer(path: &str) -> AppResult<()> {
    let lower = path.to_lowercase();
    if lower.ends_with(".msi") {
        std::process::Command::new("msiexec")
            .args(["/i", path])
            .spawn()
            .map_err(|error| AppError::Message(format!("启动安装程序失败: {error}")))?;
        return Ok(());
    }
    std::process::Command::new(path)
        .spawn()
        .map_err(|error| AppError::Message(format!("启动安装程序失败: {error}")))?;
    Ok(())
}

async fn run_install(app: &AppHandle, kind: AppKind, action: &str) -> AppResult<InstallReport> {
    let descriptor = catalog::builtin_app(kind);

    if let Some(url) = descriptor.installer_url.clone() {
        let file = download_installer(app, kind, action, &url).await?;
        launch_installer(&file)?;
        let _ = app.emit(
            "install://progress",
            DownloadProgress {
                kind,
                action: action.to_string(),
                phase: "launched".into(),
                received: 0,
                total: None,
                percent: Some(100.0),
            },
        );
        return Ok(InstallReport {
            kind,
            action: action.to_string(),
            target: file.clone(),
            launched: true,
            steps: vec![
                format!("已下载安装包到 {file}"),
                "已启动安装程序，请按提示完成安装".into(),
                "安装完成后回到本应用执行「一键应用模型」".into(),
            ],
        });
    }

    app.opener()
        .open_url(descriptor.download_page.clone(), None::<&str>)
        .map_err(|error| AppError::Message(format!("打开下载页失败: {error}")))?;

    Ok(InstallReport {
        kind,
        action: action.to_string(),
        target: descriptor.download_page.clone(),
        launched: true,
        steps: vec![
            format!("已在浏览器打开官方下载页 {}", descriptor.download_page),
            "下载并完成安装".into(),
            format!(
                "本应用会在注册表中自动识别 {}，安装完成后回到「应用」标签页刷新即可",
                descriptor.name
            ),
            "如需全自动下载安装，可在应用目录中配置 installerUrl 指向直链".into(),
        ],
    })
}

#[tauri::command]
pub async fn install_app(app: AppHandle, kind: AppKind) -> AppResult<InstallReport> {
    run_install(&app, kind, "install").await
}

#[tauri::command]
pub async fn update_app(app: AppHandle, kind: AppKind) -> AppResult<InstallReport> {
    run_install(&app, kind, "update").await
}

#[tauri::command]
pub fn app_apply_mode_label(mode: ApplyMode) -> String {
    match mode {
        ApplyMode::Gateway => "网关接管".into(),
        ApplyMode::DirectConfig => "写入配置".into(),
        ApplyMode::Manual => "手动应用".into(),
    }
}

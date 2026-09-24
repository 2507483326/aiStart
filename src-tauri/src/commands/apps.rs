use std::path::PathBuf;

use futures_util::future::join_all;
use futures_util::StreamExt;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_opener::OpenerExt;
use tokio::io::AsyncWriteExt;

use crate::domain::app::{AppKind, AppUpdate, ApplyMode, ApplyReport, InstallReport, ToolApp};
use crate::domain::catalog;
use crate::error::{AppError, AppResult};
use crate::events;
use crate::gateway;
use crate::install;
use crate::platform::{self, ApplyContext, DetectResult};
use crate::settings;
use crate::updates;

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

/// 用「当前检测到的已安装版本」重新判定是否有新版本：库里存下的 update_available
/// 可能已经过期（用户升级过应用），不能直接信任；只有在最新版本或已安装版本读不到、
/// 无从比较时，才退回库里存下的标记。
fn resolve_update(
    latest_version: Option<String>,
    installed: Option<&str>,
    stored: bool,
) -> (Option<String>, bool) {
    match (latest_version.as_deref(), installed) {
        (Some(latest), Some(installed)) => (
            Some(latest.to_string()),
            updates::is_newer(latest, installed),
        ),
        _ => (latest_version, stored),
    }
}

fn build_app_view(
    kind: AppKind,
    applied: Option<&crate::domain::model::ModelConfig>,
    known: Option<&updates::CheckSnapshot>,
    api_key: String,
) -> ToolApp {
    let configurator = platform::configurator_for(kind);
    let descriptor = configurator.descriptor();
    let detect = configurator
        .detect()
        .unwrap_or_else(|_| DetectResult::missing());
    let applied = applied.filter(|_| configurator.is_configured().unwrap_or(false));
    let (latest_version, update_available) = resolve_update(
        known.and_then(|snapshot| snapshot.latest_version.clone()),
        detect.version.as_deref(),
        known
            .map(|snapshot| snapshot.update_available)
            .unwrap_or(false),
    );

    ToolApp {
        kind,
        name: descriptor.name,
        publisher: descriptor.publisher,
        description: descriptor.description,
        homepage: descriptor.homepage,
        download_page: descriptor.download_page,
        requires_gateway: descriptor.requires_gateway,
        apply_mode: descriptor.apply_mode,
        config_target: descriptor.config_target,
        api_key,
        installed: detect.installed,
        version: detect.version,
        install_location: detect.location,
        latest_version,
        update_available,
        applied_model_id: applied.map(|model| model.id),
        applied_model_name: applied.map(|model| model.name.clone()),
    }
}

#[tauri::command]
pub fn list_apps() -> AppResult<Vec<ToolApp>> {
    let settings = settings::snapshot();
    let known = updates::latest_checks();
    Ok(AppKind::ALL
        .iter()
        .map(|kind| {
            let api_key = settings
                .app_token(*kind)
                .unwrap_or_else(|| kind.gateway_token())
                .to_string();
            build_app_view(
                *kind,
                settings.applied_model(*kind),
                known.get(kind),
                api_key,
            )
        })
        .collect())
}

#[tauri::command]
pub async fn check_app_updates() -> AppResult<Vec<AppUpdate>> {
    // 先探测本机已安装版本：版本感知的探测源（如 WorkBuddy 的 `v2/update`）要带上
    // 当前版本才会返回「本机该升到的目标版本」，探测和比对共用同一次探测结果。
    let installed: Vec<Option<String>> = AppKind::ALL
        .iter()
        .map(|kind| installed_version(*kind))
        .collect();
    let latest = join_all(
        AppKind::ALL
            .iter()
            .zip(&installed)
            .map(|(kind, installed)| updates::latest_version(*kind, installed.as_deref())),
    )
    .await;
    let known = updates::latest_checks();

    Ok(AppKind::ALL
        .iter()
        .zip(latest)
        .enumerate()
        .map(|(index, (kind, found))| {
            let installed = installed[index].as_deref();
            let latest_version = found.as_ref().map(|found| found.version.clone());

            // 只有同时拿到「官方最新版本」和「已安装版本」才算一次有效检查：
            // 此时才比较并落库。查不到（离线 / 超时 / 版本号读不出）时既不落库也不清零，
            // 直接沿用上一次已知结果，避免把「有新版本」误抹成「已是最新」。
            match (latest_version.as_deref(), installed) {
                (Some(latest), Some(installed)) => {
                    let update_available = updates::is_newer(latest, installed);
                    updates::record_check(
                        *kind,
                        Some(installed),
                        Some(latest),
                        found.as_ref().map(|found| found.source_url.as_str()),
                        update_available,
                        if update_available {
                            "found"
                        } else {
                            "up-to-date"
                        },
                        None,
                    );
                    AppUpdate {
                        kind: *kind,
                        latest_version: Some(latest.to_string()),
                        update_available,
                    }
                }
                _ => {
                    // 本次没能确认（离线 / 超时 / 版本号读不出）：不落库、不清零。
                    // 但仍要用「已知最新版本 vs 当前已安装版本」重算，避免把库里过期的
                    // update_available=1 原样透出（例如应用已升级、新旧版本号已一致）。
                    let previous = known.get(kind);
                    let latest_version = latest_version
                        .or_else(|| previous.and_then(|snapshot| snapshot.latest_version.clone()));
                    let (latest_version, update_available) = resolve_update(
                        latest_version,
                        installed,
                        previous
                            .map(|snapshot| snapshot.update_available)
                            .unwrap_or(false),
                    );
                    AppUpdate {
                        kind: *kind,
                        latest_version,
                        update_available,
                    }
                }
            }
        })
        .collect())
}

#[tauri::command]
pub fn apply_model(kind: AppKind, model_id: Option<i64>) -> AppResult<ApplyReport> {
    let settings = settings::snapshot();
    let model = match model_id {
        Some(id) => crate::settings::require_model(id)?,
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
    let token = settings
        .app_token(kind)
        .map(str::to_string)
        .unwrap_or_else(|| kind.gateway_token().to_string());

    let context = ApplyContext {
        model: model.clone(),
        gateway_base_url: status.base_url.clone(),
        gateway_token: token.clone(),
        model_choices: configurator.exposed_models(),
    };

    let report = configurator.apply(&context)?;

    settings::mutate(|settings| {
        settings.applied.insert(kind.as_str().to_string(), model.id);
        settings
            .app_tokens
            .insert(kind.as_str().to_string(), token.clone());
    })?;

    events::log(
        "user",
        None,
        "app.applied",
        Some("app"),
        Some(kind.as_str()),
        Some(serde_json::json!({ "modelId": model.id, "modelName": model.name })),
    );

    Ok(report)
}

#[tauri::command]
pub fn clear_app_model(kind: AppKind) -> AppResult<()> {
    let configurator = platform::configurator_for(kind);
    configurator.clear()?;
    settings::mutate(|settings| {
        settings.applied.remove(kind.as_str());
        settings.app_tokens.remove(kind.as_str());
    })?;
    events::log(
        "user",
        None,
        "app.cleared",
        Some("app"),
        Some(kind.as_str()),
        None,
    );
    Ok(())
}

/// 安装包下载目录（`<app_cache_dir>/installers`）。
///
/// 真实落盘位置与设置里的「打开下载文件夹」共用这一个函数，避免两处各算一次而漂移。
pub(crate) fn installer_dir(app: &AppHandle) -> AppResult<PathBuf> {
    app.path()
        .app_cache_dir()
        .map(|dir| dir.join("installers"))
        .map_err(|error| AppError::Message(format!("无法定位缓存目录: {error}")))
}

/// 在资源管理器里打开下载文件夹。
#[tauri::command]
pub fn open_download_dir(app: AppHandle) -> AppResult<()> {
    let directory = installer_dir(&app)?;
    // 还没下载过任何安装包时目录并不存在：先建出来，否则打开会失败。
    std::fs::create_dir_all(&directory)?;
    app.opener()
        .open_path(directory.to_string_lossy().to_string(), None::<&str>)
        .map_err(|error| AppError::Message(format!("打开下载文件夹失败: {error}")))
}

/// 解析「这次会从哪个地址下载安装包」，供卡片菜单的「复制下载地址」使用。
///
/// 走的是与安装完全相同的候选源解析（同样的镜像校验与兜底），只是不下载，
/// 也不因「已是最新」短路——用户要的就是直链本身。
#[tauri::command]
pub async fn installer_url(kind: AppKind) -> AppResult<String> {
    let descriptor = catalog::builtin_app(kind);
    let asset = install::sources::resolve_asset(kind, &descriptor.upgrade).await?;
    Ok(asset.url)
}

async fn download_installer(
    app: &AppHandle,
    kind: AppKind,
    action: &str,
    url: &str,
) -> AppResult<String> {
    let directory = installer_dir(app)?;
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

    match install::sources::resolve(kind, &descriptor.upgrade).await {
        Ok(install::sources::ResolveOutcome::Ready(asset)) => {
            let file = download_installer(app, kind, action, &asset.url).await?;
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
            let version = asset.version.unwrap_or_else(|| "未知".into());
            Ok(InstallReport {
                kind,
                action: action.to_string(),
                target: file.clone(),
                launched: true,
                steps: vec![
                    format!("已从「{}」解析到版本 {version}", asset.source.as_str()),
                    format!("已下载安装包到 {file}"),
                    "已启动安装程序，请按提示完成安装".into(),
                    "安装完成后回到本应用执行「应用」接入模型".into(),
                ],
            })
        }

        Ok(install::sources::ResolveOutcome::UpToDate { installed, latest }) => {
            Err(AppError::Message(format!(
                "已是最新版本（{}），无需安装",
                installed.or(latest).unwrap_or_else(|| "未知".into())
            )))
        }

        // 自动链路的全部候选源都不可用 → 降级到「打开下载页人工安装」，
        // 并把失败原因一并告诉用户，而不是静默退回下载页。
        Err(error) => {
            app.opener()
                .open_url(descriptor.download_page.clone(), None::<&str>)
                .map_err(|open_error| AppError::Message(format!("打开下载页失败: {open_error}")))?;

            Ok(InstallReport {
                kind,
                action: action.to_string(),
                target: descriptor.download_page.clone(),
                launched: true,
                steps: vec![
                    format!("自动升级不可用：{error}"),
                    format!(
                        "已在浏览器打开 {} 的官方下载页 {}",
                        descriptor.name, descriptor.download_page
                    ),
                    format!(
                        "本应用会在注册表中自动识别 {}，安装完成后回到「应用」标签页刷新即可",
                        descriptor.name
                    ),
                ],
            })
        }
    }
}

fn installed_version(kind: AppKind) -> Option<String> {
    platform::configurator_for(kind)
        .detect()
        .ok()
        .filter(|detect| detect.installed)
        .and_then(|detect| detect.version)
}

fn log_install_outcome(kind: AppKind, action: &str, outcome: &AppResult<InstallReport>) {
    let installed = installed_version(kind);
    match outcome {
        Ok(report) => {
            updates::record_action(kind, action, installed.as_deref(), None, "launched", None);
            events::log(
                "user",
                None,
                &format!("app.{action}.launched"),
                Some("app"),
                Some(kind.as_str()),
                Some(serde_json::json!({ "target": report.target })),
            );
        }
        Err(error) => {
            let message = error.to_string();
            updates::record_action(
                kind,
                action,
                installed.as_deref(),
                None,
                "failed",
                Some(&message),
            );
            events::log(
                "user",
                None,
                &format!("app.{action}.failed"),
                Some("app"),
                Some(kind.as_str()),
                Some(serde_json::json!({ "message": message })),
            );
        }
    }
}

#[tauri::command]
pub async fn install_app(app: AppHandle, kind: AppKind) -> AppResult<InstallReport> {
    let outcome = run_install(&app, kind, "install").await;
    log_install_outcome(kind, "install", &outcome);
    outcome
}

#[tauri::command]
pub async fn update_app(app: AppHandle, kind: AppKind) -> AppResult<InstallReport> {
    let outcome = run_install(&app, kind, "update").await;
    log_install_outcome(kind, "update", &outcome);
    outcome
}

#[tauri::command]
pub fn app_apply_mode_label(mode: ApplyMode) -> String {
    match mode {
        ApplyMode::Gateway => "网关接管".into(),
        ApplyMode::DirectConfig => "写入配置".into(),
        ApplyMode::Manual => "手动应用".into(),
    }
}

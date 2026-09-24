use std::path::PathBuf;

use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, KEY_WRITE};
use winreg::RegKey;

use crate::domain::app::{AppDescriptor, AppKind, ApplyMode, ApplyReport};
use crate::domain::catalog;
use crate::error::{AppError, AppResult};

use super::{
    dsh, expand_env, gateway_alias_choice, AppConfigurator, ApplyContext, DetectResult, ModelChoice,
};

const CLAUDE_POLICY_PATH: &str = r"SOFTWARE\Policies\Claude";

/// Registry values this app wrote before it moved to the user-level profile.
/// Managed policy outranks the user-level profile, so any of these left behind
/// would shadow the file we now write.
const LEGACY_POLICY_VALUES: &[&str] = &[
    "inferenceProvider",
    "inferenceGatewayBaseUrl",
    "inferenceGatewayApiKey",
    "inferenceGatewayAuthScheme",
    "inferenceModels",
    "modelDiscoveryEnabled",
    "disableDeploymentModeChooser",
];

/// Claude Desktop's user-level (non-managed) profile directory. The in-app
/// "Configure Third-Party Inference" window writes here too, so this stays
/// compatible with other tools managing the same profile.
const CLAUDE_CONFIG_LIBRARY: &str = r"%LOCALAPPDATA%\Claude-3p\configLibrary";

/// Fixed profile id, so re-applying overwrites our own file instead of
/// accumulating entries in `_meta.json`.
const CLAUDE_PROFILE_ID: &str = "00000000-0000-4000-8000-000000008931";
const CLAUDE_PROFILE_NAME: &str = "AI Start";

const UNINSTALL_PATHS: &[&str] = &[
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
    r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
];

const MSIX_PACKAGES_PATH: &str = r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages";

struct InstalledApp {
    name: String,
    version: Option<String>,
    location: Option<String>,
}

fn installed_apps() -> Vec<InstalledApp> {
    let mut apps = Vec::new();
    for hive in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        let root = RegKey::predef(hive);
        for path in UNINSTALL_PATHS {
            let flags = KEY_READ | KEY_WOW64_64KEY;
            let Ok(key) = root.open_subkey_with_flags(path, flags) else {
                continue;
            };
            for sub_name in key.enum_keys().flatten() {
                let Ok(sub) = key.open_subkey_with_flags(&sub_name, flags) else {
                    continue;
                };
                let display: String = sub.get_value("DisplayName").unwrap_or_default();
                if display.trim().is_empty() {
                    continue;
                }
                let version: String = sub.get_value("DisplayVersion").unwrap_or_default();
                let location: String = sub
                    .get_value::<String, _>("InstallLocation")
                    .unwrap_or_default();
                apps.push(InstalledApp {
                    name: display,
                    version: (!version.trim().is_empty()).then_some(version),
                    location: (!location.trim().is_empty()).then_some(location),
                });
            }
        }
    }
    apps
}

fn find_app(needles: &[&str]) -> Option<InstalledApp> {
    let apps = installed_apps();
    for needle in needles {
        let needle = needle.to_lowercase();
        if let Some(app) = apps
            .iter()
            .find(|app| app.name.to_lowercase().contains(&needle))
        {
            return Some(InstalledApp {
                name: app.name.clone(),
                version: app.version.clone(),
                location: app.location.clone(),
            });
        }
    }
    None
}

struct MsixPackage {
    name: String,
    version: Option<String>,
    location: Option<String>,
}

fn parse_package_id(package_id: &str) -> Option<(String, Option<String>)> {
    let mut parts = package_id.split('_');
    let name = parts.next()?.trim();
    if name.is_empty() {
        return None;
    }
    let version = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    Some((name.to_string(), version))
}

fn installed_msix_packages() -> Vec<MsixPackage> {
    let mut packages = Vec::new();
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(key) = hkcu.open_subkey_with_flags(MSIX_PACKAGES_PATH, KEY_READ) else {
        return packages;
    };
    for full_name in key.enum_keys().flatten() {
        let Ok(sub) = key.open_subkey_with_flags(&full_name, KEY_READ) else {
            continue;
        };
        let display: String = sub.get_value("DisplayName").unwrap_or_default();
        if display.trim().is_empty() {
            continue;
        }
        let package_id: String = sub
            .get_value("PackageID")
            .unwrap_or_else(|_| full_name.clone());
        let Some((_, version)) = parse_package_id(&package_id) else {
            continue;
        };
        let location: String = sub.get_value("PackageRootFolder").unwrap_or_default();
        packages.push(MsixPackage {
            name: display,
            version,
            location: (!location.trim().is_empty()).then_some(location),
        });
    }
    packages
}

fn find_msix(needles: &[&str]) -> Option<MsixPackage> {
    let packages = installed_msix_packages();
    for needle in needles {
        let needle = needle.to_lowercase();
        if let Some(package) = packages
            .iter()
            .find(|package| package.name.to_lowercase().contains(&needle))
        {
            return Some(MsixPackage {
                name: package.name.clone(),
                version: package.version.clone(),
                location: package.location.clone(),
            });
        }
    }
    None
}

fn existing_dir(candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .map(|path| expand_env(path))
        .find(|path| PathBuf::from(path).exists())
}

fn claude_binary() -> Option<String> {
    let candidates = [
        r"%LOCALAPPDATA%\AnthropicClaude\claude.exe",
        r"%LOCALAPPDATA%\Programs\Claude\Claude.exe",
        r"%LOCALAPPDATA%\Claude\Claude.exe",
        r"%PROGRAMFILES%\Claude\Claude.exe",
    ];
    candidates
        .iter()
        .map(|path| expand_env(path))
        .find(|path| PathBuf::from(path).exists())
}

fn policy_key() -> AppResult<RegKey> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    hkcu.open_subkey_with_flags(CLAUDE_POLICY_PATH, KEY_READ | KEY_WRITE)
        .map_err(|_| AppError::NotFound("Claude 策略注册表项不存在".into()))
}

fn delete_policy(name: &str) -> AppResult<()> {
    let Ok(key) = policy_key() else {
        return Ok(());
    };
    match key.delete_value(name) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Managed policy outranks the user-level profile, so the values this app used
/// to write there have to go for the profile to take effect. Failing here is
/// fatal on purpose — a leftover policy would silently shadow the profile.
fn clear_legacy_policy() -> AppResult<()> {
    for name in LEGACY_POLICY_VALUES {
        delete_policy(name)?;
    }
    Ok(())
}

fn config_library_dir() -> PathBuf {
    PathBuf::from(expand_env(CLAUDE_CONFIG_LIBRARY))
}

fn profile_path() -> PathBuf {
    config_library_dir().join(format!("{CLAUDE_PROFILE_ID}.json"))
}

fn meta_path() -> PathBuf {
    config_library_dir().join("_meta.json")
}

fn read_meta() -> serde_json::Value {
    std::fs::read_to_string(meta_path())
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({ "appliedId": null, "entries": [] }))
}

fn write_meta(meta: &serde_json::Value) -> AppResult<()> {
    std::fs::write(meta_path(), serde_json::to_string_pretty(meta)?)?;
    Ok(())
}

/// Registers our profile in `_meta.json` and makes it the applied one.
fn adopt_meta() -> AppResult<()> {
    let mut meta = read_meta();
    if !meta.is_object() {
        meta = serde_json::json!({ "appliedId": null, "entries": [] });
    }
    let object = meta.as_object_mut().expect("reset to an object above");
    if !object
        .get("entries")
        .map(serde_json::Value::is_array)
        .unwrap_or(false)
    {
        object.insert("entries".into(), serde_json::Value::Array(Vec::new()));
    }
    let entries = object
        .get_mut("entries")
        .and_then(serde_json::Value::as_array_mut)
        .expect("an array was just ensured");
    entries.retain(|entry| {
        entry.get("id").and_then(serde_json::Value::as_str) != Some(CLAUDE_PROFILE_ID)
    });
    entries.push(serde_json::json!({ "id": CLAUDE_PROFILE_ID, "name": CLAUDE_PROFILE_NAME }));
    object.insert("appliedId".into(), serde_json::json!(CLAUDE_PROFILE_ID));
    write_meta(&meta)
}

/// Drops our profile from `_meta.json`, handing the applied slot to whatever
/// other profile is still registered.
fn release_meta() -> AppResult<()> {
    if !meta_path().exists() {
        return Ok(());
    }
    let mut meta = read_meta();
    let Some(object) = meta.as_object_mut() else {
        return Ok(());
    };
    if let Some(entries) = object
        .get_mut("entries")
        .and_then(serde_json::Value::as_array_mut)
    {
        entries.retain(|entry| {
            entry.get("id").and_then(serde_json::Value::as_str) != Some(CLAUDE_PROFILE_ID)
        });
    }
    if object.get("appliedId").and_then(serde_json::Value::as_str) == Some(CLAUDE_PROFILE_ID) {
        let next = object
            .get("entries")
            .and_then(serde_json::Value::as_array)
            .and_then(|entries| entries.first())
            .and_then(|entry| entry.get("id"))
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        object.insert("appliedId".into(), next);
    }
    write_meta(&meta)
}

/// 描述符驱动的通用探测：新增应用只要在目录里填好锚点字段，探测就自动可用，
/// 不必为每个应用写一份 `detect`（Claude / DSH 的历史实现保留各自的特例逻辑）。
///
/// 依次尝试 MSIX 包名前缀 → 注册表 `DisplayName` 前缀 → 期望安装位置，命中即止。
/// 这些锚点在应用未安装时都只是「待实测」的推测值，命中不了就诚实地报未安装。
pub fn detect_by_descriptor(descriptor: &AppDescriptor) -> DetectResult {
    if let Some(prefix) = descriptor.upgrade.msix_name_prefix.as_deref() {
        if let Some(package) = find_msix(&[prefix]) {
            return DetectResult::found(package.location.unwrap_or(package.name), package.version);
        }
    }

    if let Some(needle) = descriptor.upgrade.display_name_match.as_deref() {
        if let Some(app) = find_app(&[needle]) {
            return DetectResult::found(
                app.location.unwrap_or_else(|| app.name.clone()),
                app.version,
            );
        }
    }

    if let Some(location) = descriptor.upgrade.expected_location.as_deref() {
        if let Some(path) = existing_dir(&[location]) {
            return DetectResult::found(path, None);
        }
    }

    DetectResult::missing()
}

pub struct ClaudeDesktopConfigurator;

impl AppConfigurator for ClaudeDesktopConfigurator {
    fn descriptor(&self) -> AppDescriptor {
        catalog::builtin_app(AppKind::ClaudeDesktop)
    }

    fn detect(&self) -> AppResult<DetectResult> {
        if let Some(package) = find_msix(&["claude"]) {
            return Ok(DetectResult::found(
                package.location.unwrap_or(package.name),
                package.version,
            ));
        }
        if let Some(path) = claude_binary() {
            let version = find_app(&["claude desktop", "claude for windows"])
                .and_then(|app| app.version)
                .or_else(|| find_app(&["claude"]).and_then(|app| app.version));
            return Ok(DetectResult::found(path, version));
        }
        match find_app(&["claude desktop", "claude"]) {
            Some(app) => Ok(DetectResult::found(
                app.location.unwrap_or_else(|| app.name.clone()),
                app.version,
            )),
            None => Ok(DetectResult::missing()),
        }
    }

    fn is_configured(&self) -> AppResult<bool> {
        let applied = read_meta()
            .get("appliedId")
            .and_then(serde_json::Value::as_str)
            == Some(CLAUDE_PROFILE_ID);
        Ok(applied && profile_path().exists())
    }

    /// Claude Desktop 会丢弃名字认不出是 Anthropic 模型的条目，因此这里必须逐个
    /// 暴露网关的档位路由，不能像其他客户端那样只给一个别名。
    fn exposed_models(&self) -> Vec<ModelChoice> {
        crate::gateway::MODEL_ROLES
            .iter()
            .map(|role| ModelChoice {
                id: role.id.to_string(),
                label: role.picker_label(),
            })
            .collect()
    }

    fn apply(&self, ctx: &ApplyContext) -> AppResult<ApplyReport> {
        let models = serde_json::Value::Array(
            ctx.model_choices
                .iter()
                .enumerate()
                .map(|(index, choice)| {
                    let mut entry = serde_json::Map::new();
                    entry.insert("name".into(), serde_json::json!(choice.id));
                    entry.insert("labelOverride".into(), serde_json::json!(choice.label));
                    if ctx.model.supports_1m {
                        entry.insert("supports1m".into(), serde_json::json!(true));
                        if index == 0 {
                            entry.insert("prefer1m".into(), serde_json::json!(true));
                        }
                    }
                    serde_json::Value::Object(entry)
                })
                .collect(),
        );

        let profile = serde_json::json!({
            "inferenceProvider": "gateway",
            "inferenceGatewayBaseUrl": ctx.gateway_base_url,
            "inferenceGatewayApiKey": ctx.gateway_token,
            "inferenceGatewayAuthScheme": "bearer",
            "inferenceModels": models,
            "modelDiscoveryEnabled": false,
            "disableDeploymentModeChooser": true,
        });

        std::fs::create_dir_all(config_library_dir())?;
        std::fs::write(profile_path(), serde_json::to_string_pretty(&profile)?)?;
        adopt_meta()?;
        clear_legacy_policy()?;

        let routes = ctx
            .model_choices
            .iter()
            .map(|choice| choice.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let mut steps = vec![
            format!("写入用户级配置 {}", profile_path().display()),
            format!("通过本地网关 {} 接管推理请求", ctx.gateway_base_url),
            format!(
                "上游模型: {} ({})",
                ctx.model.model,
                ctx.model.format.display_name()
            ),
            format!("暴露 {} 个 Claude 路由: {routes}", ctx.model_choices.len()),
        ];
        if ctx.model.supports_1m {
            steps.push("已标记支持 1M 上下文窗口，Claude 模型选择器会额外提供 1M 变体".into());
        }
        steps.push("已清除旧的 HKCU 托管策略，避免其覆盖用户级配置".into());
        steps.push("完全退出并重新打开 Claude Desktop 后生效".into());

        Ok(ApplyReport {
            kind: AppKind::ClaudeDesktop,
            model_id: ctx.model.id,
            model_name: ctx.model.name.clone(),
            apply_mode: ApplyMode::Gateway,
            target: format!(
                r"{} → inferenceProvider=gateway, inferenceGatewayBaseUrl={}",
                profile_path().display(),
                ctx.gateway_base_url
            ),
            restart_required: true,
            steps,
            note: Some(
                "写入的是 Claude Desktop 的用户级配置目录（与应用内「Configure Third-Party Inference」同一位置），无需管理员权限，也不会覆盖其他工具写的配置。注意：托管策略优先于用户级文件 —— 若机器级 HKLM 策略存在，Claude Desktop 会完全忽略本配置。"
                    .into(),
            ),
        })
    }

    fn clear(&self) -> AppResult<()> {
        let path = profile_path();
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        release_meta()?;
        clear_legacy_policy()?;
        Ok(())
    }
}

pub struct DeepseekDesktopConfigurator;

impl AppConfigurator for DeepseekDesktopConfigurator {
    fn descriptor(&self) -> AppDescriptor {
        catalog::builtin_app(AppKind::DeepseekDesktop)
    }

    fn detect(&self) -> AppResult<DetectResult> {
        if let Some(package) = find_msix(&["deepseek", "dsh desktop"]) {
            return Ok(DetectResult::found(
                package.location.unwrap_or(package.name),
                package.version,
            ));
        }
        match find_app(&["dsh desktop", "deepseek harness", "deepseek"]) {
            Some(app) => Ok(DetectResult::found(
                app.location.unwrap_or_else(|| app.name.clone()),
                app.version,
            )),
            None => match existing_dir(&[
                r"%LOCALAPPDATA%\Programs\DSH Desktop",
                r"%APPDATA%\DSH Desktop",
                r"%USERPROFILE%\.dsh",
                r"%APPDATA%\DeepSeek",
                r"%LOCALAPPDATA%\DeepSeek",
            ]) {
                Some(path) => Ok(DetectResult::found(path, None)),
                None => Ok(DetectResult::missing()),
            },
        }
    }

    fn is_configured(&self) -> AppResult<bool> {
        Ok(dsh::is_configured())
    }

    /// DSH 的模型列表是我们直接写进 settings.yaml 的，一个网关入口就够。
    fn exposed_models(&self) -> Vec<ModelChoice> {
        vec![gateway_alias_choice()]
    }

    fn apply(&self, ctx: &ApplyContext) -> AppResult<ApplyReport> {
        dsh::apply(ctx)
    }

    fn clear(&self) -> AppResult<()> {
        dsh::clear()
    }
}

#[cfg(test)]
mod tests {
    use super::parse_package_id;

    #[test]
    fn parses_version_out_of_msix_package_id() {
        let (name, version) = parse_package_id("Claude_2.2553.1.0_x64__pzs8sxrjxfjjc").unwrap();
        assert_eq!(name, "Claude");
        assert_eq!(version.as_deref(), Some("2.2553.1.0"));

        let (name, version) =
            parse_package_id("Microsoft.WindowsTerminal_1.24.11911.0_x64__8wekyb3d8bbwe").unwrap();
        assert_eq!(name, "Microsoft.WindowsTerminal");
        assert_eq!(version.as_deref(), Some("1.24.11911.0"));

        assert!(parse_package_id("").is_none());
    }
}

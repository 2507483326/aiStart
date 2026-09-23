use std::path::PathBuf;

use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY};
use winreg::RegKey;

use crate::domain::app::{AppDescriptor, AppKind, ApplyMode, ApplyReport};
use crate::domain::catalog;
use crate::error::{AppError, AppResult};

use super::{expand_env, AppConfigurator, ApplyContext, DetectResult};

const CLAUDE_POLICY_PATH: &str = r"SOFTWARE\Policies\Claude";

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

fn policy_key(create: bool) -> AppResult<RegKey> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if create {
        let (key, _) = hkcu.create_subkey(CLAUDE_POLICY_PATH)?;
        Ok(key)
    } else {
        hkcu.open_subkey_with_flags(CLAUDE_POLICY_PATH, KEY_READ)
            .map_err(|_| AppError::NotFound("Claude 策略注册表项不存在".into()))
    }
}

fn read_policy(name: &str) -> Option<String> {
    policy_key(false)
        .ok()
        .and_then(|key| key.get_value::<String, _>(name).ok())
}

fn write_policy(name: &str, value: &str) -> AppResult<()> {
    let key = policy_key(true)?;
    key.set_value(name, &value.to_string())?;
    Ok(())
}

fn delete_policy(name: &str) -> AppResult<()> {
    if let Ok(key) = policy_key(false) {
        let _ = key.delete_value(name);
    }
    Ok(())
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
        Ok(read_policy("inferenceProvider").as_deref() == Some("gateway")
            && read_policy("inferenceGatewayBaseUrl").is_some())
    }

    fn apply(&self, ctx: &ApplyContext) -> AppResult<ApplyReport> {
        let mut entry = serde_json::Map::new();
        entry.insert("name".into(), serde_json::json!(ctx.model_alias));
        entry.insert(
            "labelOverride".into(),
            serde_json::json!(ctx.model.name),
        );
        if ctx.model.supports_1m {
            entry.insert("supports1m".into(), serde_json::json!(true));
            entry.insert("prefer1m".into(), serde_json::json!(true));
        }
        let models = serde_json::Value::Array(vec![serde_json::Value::Object(entry)]);

        write_policy("inferenceProvider", "gateway")?;
        write_policy("inferenceGatewayBaseUrl", &ctx.gateway_base_url)?;
        write_policy("inferenceGatewayApiKey", &ctx.gateway_token)?;
        write_policy("inferenceGatewayAuthScheme", "bearer")?;
        write_policy("inferenceModels", &models.to_string())?;
        write_policy("modelDiscoveryEnabled", "false")?;
        write_policy("disableDeploymentModeChooser", "true")?;

        let mut steps = vec![
            format!("通过本地网关 {} 接管推理请求", ctx.gateway_base_url),
            format!(
                "上游模型: {} ({})",
                ctx.model.model,
                ctx.model.format.display_name()
            ),
            "已关闭模型自动发现，改为使用显式模型列表".into(),
        ];
        if ctx.model.supports_1m {
            steps.push("已标记支持 1M 上下文窗口，Claude 模型选择器会额外提供 1M 变体".into());
        }
        steps.push("完全退出并重新打开 Claude Desktop 后生效".into());

        Ok(ApplyReport {
            kind: AppKind::ClaudeDesktop,
            model_id: ctx.model.id.clone(),
            model_name: ctx.model.name.clone(),
            apply_mode: ApplyMode::Gateway,
            target: format!(
                r"HKEY_CURRENT_USER\{CLAUDE_POLICY_PATH} → inferenceProvider=gateway, inferenceGatewayBaseUrl={}",
                ctx.gateway_base_url
            ),
            restart_required: true,
            steps,
            note: Some(
                "写入的是 HKCU 用户级策略，无需管理员权限，也不会覆盖机器级 HKLM 策略；若机器级策略已存在，Claude Desktop 会完全忽略本配置。"
                    .into(),
            ),
        })
    }

    fn clear(&self) -> AppResult<()> {
        for name in [
            "inferenceProvider",
            "inferenceGatewayBaseUrl",
            "inferenceGatewayApiKey",
            "inferenceGatewayAuthScheme",
            "inferenceModels",
            "modelDiscoveryEnabled",
            "disableDeploymentModeChooser",
        ] {
            delete_policy(name)?;
        }
        Ok(())
    }
}

pub struct DeepseekDesktopConfigurator;

impl DeepseekDesktopConfigurator {
    fn config_path(&self) -> String {
        crate::settings::deepseek_config_path()
    }
}

impl AppConfigurator for DeepseekDesktopConfigurator {
    fn descriptor(&self) -> AppDescriptor {
        catalog::builtin_app(AppKind::DeepseekDesktop)
    }

    fn detect(&self) -> AppResult<DetectResult> {
        if let Some(package) = find_msix(&["deepseek"]) {
            return Ok(DetectResult::found(
                package.location.unwrap_or(package.name),
                package.version,
            ));
        }
        match find_app(&["deepseek"]) {
            Some(app) => Ok(DetectResult::found(
                app.location.unwrap_or_else(|| app.name.clone()),
                app.version,
            )),
            None => match existing_dir(&[r"%APPDATA%\DeepSeek", r"%LOCALAPPDATA%\DeepSeek"]) {
                Some(path) => Ok(DetectResult::found(path, None)),
                None => Ok(DetectResult::missing()),
            },
        }
    }

    fn is_configured(&self) -> AppResult<bool> {
        Ok(PathBuf::from(self.config_path()).exists())
    }

    fn apply(&self, ctx: &ApplyContext) -> AppResult<ApplyReport> {
        let path = self.config_path();
        let payload = serde_json::json!({
            "provider": "openai",
            "openai": {
                "baseURL": ctx.gateway_base_url,
                "apiKey": ctx.gateway_token,
                "model": ctx.model_alias,
            },
            "upstream": {
                "format": ctx.model.format.as_str(),
                "model": ctx.model.model,
                "baseURL": ctx.model.base_url,
            }
        });

        if let Some(parent) = PathBuf::from(&path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(&payload)?)?;

        Ok(ApplyReport {
            kind: AppKind::DeepseekDesktop,
            model_id: ctx.model.id.clone(),
            model_name: ctx.model.name.clone(),
            apply_mode: ApplyMode::DirectConfig,
            target: path.clone(),
            restart_required: true,
            steps: vec![
                format!("写入配置文件 {path}"),
                format!("指向本地网关 {}", ctx.gateway_base_url),
                format!("上游模型: {} ({})", ctx.model.model, ctx.model.format.display_name()),
                "重启 DeepSeek Desktop 后生效".into(),
            ],
            note: Some(
                "DeepSeek Desktop 未公开程序化配置格式，这里按最通用的 OpenAI 兼容结构写出参考配置；若目标路径与你的安装版本不符，可在设置中改为实际配置路径。"
                    .into(),
            ),
        })
    }

    fn clear(&self) -> AppResult<()> {
        let path = PathBuf::from(self.config_path());
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
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

//! 被管理应用的内置目录。
//!
//! **新增一个应用要改的地方**（全部在这里，流程代码不动）：
//!   1. `AppKind` 加一个变体（`domain/app.rs`：`as_str` / `ALL` / `gateway_token`）；
//!   2. 在下表加一个 `AppDescriptor`，填好 `latest_version_urls` 与 `upgrade`；
//!   3. 若该应用的「接入方式」是新形态（非网关、非直接写配置），再加一个
//!      `platform::AppConfigurator` 实现。
//!
//! 只要它用的安装器类型已在 `InstallerKind` 里，升级链路一行都不用改；
//! 只有出现全新安装器类型时才加一个枚举分支 + 一条静默参数默认值。
//!
//! 注意：`latest_version_urls`（探测版本）与 `upgrade.sources`（拿安装包）是
//! **两件独立的事**，不要合并——官方下载地址经常只是在线引导器。

use crate::domain::app::{AppDescriptor, AppKind, ApplyMode};
use crate::domain::release::{InstallerKind, ReleaseSource, UpgradeSpec};

pub fn builtin_apps() -> Vec<AppDescriptor> {
    vec![
        AppDescriptor {
            kind: AppKind::ClaudeDesktop,
            name: "Claude Desktop".into(),
            publisher: "Anthropic".into(),
            description: "Anthropic 官方桌面客户端。通过第三方推理（3P）网关接入任意上游模型。"
                .into(),
            download_page: "https://claude.ai/download".into(),
            homepage: "https://claude.ai/".into(),
            requires_gateway: true,
            apply_mode: ApplyMode::Gateway,
            config_target: r"%LOCALAPPDATA%\Claude-3p\configLibrary".into(),
            latest_version_urls: vec![
                // Squirrel RELEASES feed：只需要它报版本号，不从这里下载。
                "https://downloads.claude.ai/releases/win32/x64/RELEASES".into(),
                "https://api.github.com/repos/Wangnov/claude-app-mirror/releases/latest".into(),
            ],
            upgrade: UpgradeSpec {
                sources: vec![
                    // 1) 国内镜像短链（R2），永远指向最新版。
                    //    实测：无重定向、文件名 `Claude-win-x64.msix` 不含版本、
                    //    ETag 是分片值而非 sha256 → 版本与校验值都只能靠外部来源，
                    //    所以这条源必须配合 `updates::latest_version()` 做自洽校验。
                    ReleaseSource::Mirror {
                        url: "https://claudeapp.agentsmirror.com/latest/win-x64".into(),
                        checksums: Some(
                            "https://claudeapp.agentsmirror.com/latest/checksums".into(),
                        ),
                    },
                    // 2) GitHub 镜像仓库：资产自带 `digest`，可直接拿到 sha256。
                    //    注意它镜像是**自包含离线 MSIX**，不是官方的在线引导器。
                    ReleaseSource::Github {
                        repo: "Wangnov/claude-app-mirror".into(),
                        asset_match: "Claude-win-x64.msix".into(),
                    },
                    // 3) 兜底：打开官方下载页，人工安装。
                    ReleaseSource::Manual {
                        page: "https://claude.ai/download".into(),
                    },
                ],
                // 官方 `downloads.claude.ai` 的 `ClaudeSetup.exe`（约 7MB）**只是在线
                // 引导器**，安装时仍会回 GCS 重下真正的 MSIX，所以它只能当版本探测源，
                // 不能当安装包源——这里刻意不配 Official 源。
                installer: Some(InstallerKind::Msix),
                requires_admin: false,
                // 实测：进程镜像名 `claude.exe`（Chromium 多进程，会同时有多个）。
                process_names: vec!["claude.exe".into()],
                // 实测：`PackageFullName` 形如 `Claude_2.7032.0.0_x64__pzs8sxrjxfjjc`。
                msix_name_prefix: Some("Claude".into()),
                manual_page: "https://claude.ai/download".into(),
                ..Default::default()
            },
        },
        AppDescriptor {
            kind: AppKind::DeepseekDesktop,
            name: "DeepSeek Desktop".into(),
            publisher: "DeepSeek".into(),
            description: "DeepSeek 桌面客户端（DSH）。把 aiStart 网关作为 provider 合并进 ~/.dsh/settings.yaml，并设为默认模型。"
                .into(),
            download_page: "https://www.deepseek.com/".into(),
            homepage: "https://www.deepseek.com/".into(),
            requires_gateway: true,
            apply_mode: ApplyMode::DirectConfig,
            config_target: r"%USERPROFILE%\.dsh\settings.yaml".into(),
            latest_version_urls: vec![
                // 注意：`deepseek-harness-desktop` 会 302 到 `dsh-desktop`，
                // 直接写最终仓库名，少一跳、也避免被重定向策略影响。
                "https://api.github.com/repos/anywhere-labs/dsh-desktop/releases/latest".into(),
            ],
            upgrade: UpgradeSpec {
                sources: vec![
                    // 1) ModelScope 国内镜像（DSH 发行说明里官方给的镜像）。
                    //    URL 带版本，`{version}` 由探测结果填充。
                    ReleaseSource::Direct {
                        url: "https://modelscope.cn/models/t4wefan/deepseek-harness-desktop/resolve/master/DSH-Desktop-{version}-x64-Setup.exe".into(),
                    },
                    // 2) GitHub Releases：digest 提供 sha256。
                    ReleaseSource::Github {
                        repo: "anywhere-labs/dsh-desktop".into(),
                        asset_match: "-x64-Setup.exe".into(),
                    },
                    ReleaseSource::Manual {
                        page: "https://github.com/anywhere-labs/dsh-desktop/releases".into(),
                    },
                ],
                // DSH Desktop 是 Tauri 2 shell，`-Setup.exe` 即 Tauri 的 NSIS 安装器：
                // `/S` 静默，默认 currentUser（免管理员）。
                // 若实测发现它其实是 Inno，改这一个字段即可。
                installer: Some(InstallerKind::Nsis),
                requires_admin: false,
                // 实测：进程镜像名含空格 —— `DSH Desktop.exe`
                // （路径 `%LOCALAPPDATA%\Programs\DSH Desktop\DSH Desktop.exe`）。
                process_names: vec!["DSH Desktop.exe".into()],
                // 实测：注册表 `DisplayName = "DSH Desktop 2.0.5"`，而 `InstallLocation`
                // 为**空**，所以位置锚定不可用，只能靠 DisplayName 前缀。
                display_name_match: Some("DSH Desktop".into()),
                manual_page: "https://github.com/anywhere-labs/dsh-desktop/releases".into(),
                ..Default::default()
            },
        },
        AppDescriptor {
            kind: AppKind::Codex,
            name: "Codex".into(),
            publisher: "OpenAI".into(),
            description: "OpenAI Codex 桌面版（即 ChatGPT 桌面版）。在应用内添加自定义模型供应商，把推理指向本地网关的 Responses 入口。"
                .into(),
            homepage: "https://chatgpt.com/codex".into(),
            download_page: "https://chatgpt.com/download/".into(),
            requires_gateway: true,
            apply_mode: ApplyMode::Manual,
            // 桌面版与 Codex CLI 共用 Codex home（`CODEX_HOME`），自定义 provider 就写在这里的
            // `[model_providers.*]`（`wire_api` 目前只支持 `responses`）。本期**不写入**，只作为
            // 「手动接入」的目标告知用户；待装后实测桌面版是否读取该文件，再决定是否升级为 DirectConfig。
            config_target: r"%USERPROFILE%\.codex\config.toml".into(),
            // 桌面版经 Microsoft Store（MSIX）分发，没有可解析的公开「最新版本」源；
            // 留空即不做版本探测，卡片只显示已安装版本。
            latest_version_urls: vec![],
            upgrade: UpgradeSpec {
                // 无权威下载源（桌面版走 Store）：`sources` 只给 Manual。
                // `install::sources::resolve` 会硬失败，`run_install` 随即降级为「打开官方下载页」。
                sources: vec![ReleaseSource::Manual {
                    page: "https://chatgpt.com/download/".into(),
                }],
                installer: None,
                requires_admin: false,
                // 待实测：桌面版进程镜像名（推测为 ChatGPT.exe）。
                process_names: vec!["ChatGPT.exe".into()],
                // 待实测：MSIX `PackageID` / `DisplayName` 前缀（推测为 ChatGPT）。
                msix_name_prefix: Some("ChatGPT".into()),
                // 待实测：注册表 `DisplayName`。
                display_name_match: Some("ChatGPT".into()),
                manual_page: "https://chatgpt.com/download/".into(),
                ..Default::default()
            },
        },
        AppDescriptor {
            kind: AppKind::ZCode,
            name: "ZCode".into(),
            publisher: "Z.ai（智谱）".into(),
            description: "智谱 ZCode（GLM 官方 ADE 桌面端）。添加自定义供应商时可选择 Chat Completions / Responses / Anthropic Messages 三种协议。"
                .into(),
            homepage: "https://zcode.z.ai/".into(),
            download_page: "https://zcode.z.ai/cn".into(),
            requires_gateway: true,
            apply_mode: ApplyMode::Manual,
            // 自定义供应商在 GUI 里配置，官方未公开落盘格式；待实测后再定文件路径。
            config_target: "ZCode 设置 → 模型供应商（自定义）".into(),
            // 待实测：`https://zcode.z.ai/changelog` 是 HTML，现有 `updates::parse_latest` 解析不了，
            // 暂不做版本探测。CDN 已是版本化直链（见 upgrade 注释），补上版本源即可开启自动安装。
            latest_version_urls: vec![],
            upgrade: UpgradeSpec {
                // 官方下载页已有各平台的版本化 CDN 直链，但没有可解析的「最新版本」源，
                // 无法填 `{version}` 占位，故本期只保留 Manual 兜底（打开下载页）。
                // follow-up: 找到版本源后加 `Direct{ url: "https://cdn-zcode.z.ai/zcode/electron/releases/{version}/windows-x64/ZCode-{version}-win-x64.exe" }`（installer = Nsis）。
                sources: vec![ReleaseSource::Manual {
                    page: "https://zcode.z.ai/cn".into(),
                }],
                installer: None,
                requires_admin: false,
                // 待实测：Electron 应用进程镜像名（推测为 ZCode.exe）。
                process_names: vec!["ZCode.exe".into()],
                // 待实测：注册表 `DisplayName` / 安装位置。
                display_name_match: Some("ZCode".into()),
                expected_location: Some(r"%LOCALAPPDATA%\Programs\ZCode".into()),
                manual_page: "https://zcode.z.ai/cn".into(),
                ..Default::default()
            },
        },
        AppDescriptor {
            kind: AppKind::WorkBuddy,
            name: "WorkBuddy".into(),
            publisher: "Tencent".into(),
            description: "腾讯 WorkBuddy AI Agent 办公工作台。直接写入它的本地自定义模型配置（models.json），把网关地址与 Key 交给它。"
                .into(),
            homepage: "https://www.workbuddy.ai/".into(),
            download_page: "https://www.workbuddy.ai/".into(),
            requires_gateway: true,
            apply_mode: ApplyMode::DirectConfig,
            // 实测：`CustomModelsJSON` 特性已开启，WorkBuddy 会监听该文件并自动同步，
            // 写入后无需重启。条目形如 { id, name, vendor, url, apiKey, ... }，url 必须是
            // 以 /chat/completions 结尾的完整地址（见 `platform/workbuddy.rs`）。
            config_target: r"%USERPROFILE%\.workbuddy\models.json".into(),
            // 实测：官方 `v2/update` feed（应用自身的更新检查接口）。它是**版本感知**的
            // ——必须带上当前已安装版本，服务端才按灰度返回「本机该升到的目标版本」；
            // 传 `0.0.0` 只会拿到一个旧目标。故 URL 用 `{version}` 占位，由
            // `updates::latest_version()` 填已安装版本。已是该目标版本时返回 204（空体）。
            //
            // 平台串 `win32-x64-user` 里的 `-user` 只是选择「NSIS 用户安装产物」这一
            // 安装形态（另有 `-archive`）；两种产物的版本号一致，取最常见的即可。
            latest_version_urls: vec![
                "https://copilot.tencent.com/v2/update?platform=workbuddy-win32-x64-user&version={version}"
                    .into(),
            ],
            upgrade: UpgradeSpec {
                // 官方下载页按系统分发安装包（WorkBuddySetup.exe），无稳定直链与校验值，
                // 故本期只保留 Manual 兜底（打开下载页）。
                sources: vec![ReleaseSource::Manual {
                    page: "https://www.workbuddy.ai/".into(),
                }],
                installer: None,
                requires_admin: false,
                // 待实测：进程镜像名（推测为 WorkBuddy.exe）。
                process_names: vec!["WorkBuddy.exe".into()],
                // 待实测：注册表 `DisplayName` / 安装位置。
                display_name_match: Some("WorkBuddy".into()),
                expected_location: Some(r"%LOCALAPPDATA%\Programs\WorkBuddy".into()),
                manual_page: "https://www.workbuddy.ai/".into(),
                ..Default::default()
            },
        },
    ]
}

pub fn builtin_app(kind: AppKind) -> AppDescriptor {
    builtin_apps()
        .into_iter()
        .find(|app| app.kind == kind)
        .expect("builtin catalog always contains every AppKind")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_app_has_an_upgrade_spec_ending_in_manual() {
        for app in builtin_apps() {
            assert!(
                !app.upgrade.sources.is_empty(),
                "{} 没有配置任何下载源",
                app.name
            );
            assert!(
                matches!(
                    app.upgrade.sources.last(),
                    Some(ReleaseSource::Manual { .. })
                ),
                "{} 的候选源必须以 Manual 收尾，否则全部失败时无法降级到人工安装",
                app.name
            );
            assert!(
                !app.upgrade.manual_page.is_empty(),
                "{} 缺 manual_page",
                app.name
            );
        }
    }

    #[test]
    fn apps_that_claim_auto_install_have_a_real_source() {
        // 声明了 `installer`（即声称能自动安装）时，只有 Manual 等于「永远自动不了」，
        // 那是配置错误而不是降级。没声明 `installer` 的应用（如在 Store 分发、官方无直链的
        // 桌面版）就是刻意的「只能人工安装」，允许只有 Manual。
        for app in builtin_apps() {
            if app.upgrade.installer.is_none() {
                continue;
            }
            assert!(
                app.upgrade.sources.len() > 1,
                "{} 声明了 installer 却只配了人工安装，等于没有自动升级能力",
                app.name
            );
        }
    }

    #[test]
    fn every_app_kind_has_exactly_one_descriptor() {
        // 防止加了 AppKind 变体却忘了在目录里补描述符（`builtin_app` 会在运行期 panic）。
        let apps = builtin_apps();
        assert_eq!(apps.len(), AppKind::ALL.len());
        for kind in AppKind::ALL {
            assert_eq!(
                apps.iter().filter(|app| app.kind == kind).count(),
                1,
                "{} 的描述符缺失或重复",
                kind.as_str()
            );
        }
    }

    #[test]
    fn app_kind_str_round_trips() {
        // `as_str` 同时是网关 token 与 settings 里的 app_kind 键，必须能被 `parse` 还原。
        for kind in AppKind::ALL {
            assert_eq!(AppKind::parse(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn workbuddy_probes_the_version_aware_official_feed() {
        // WorkBuddy 的新版本探测依赖官方 `v2/update`：它是版本感知的，URL 必须带
        // `{version}` 占位符，否则探测会拿到一个与「已安装版本」无关的旧目标。
        let workbuddy = builtin_app(AppKind::WorkBuddy);
        let url = workbuddy
            .latest_version_urls
            .first()
            .expect("WorkBuddy 应配置版本探测源");
        assert!(url.contains("copilot.tencent.com/v2/update"), "{url}");
        assert!(url.contains("{version}"), "{url}");
    }

    #[test]
    fn workbuddy_writes_its_models_json_instead_of_going_manual() {
        // WorkBuddy 的落盘格式已实测（`CustomModelsJSON` 特性开启，文件会被监听），
        // 因此它必须走 `DirectConfig`；退回 `Manual` 说明有人把写入逻辑删了。
        let workbuddy = builtin_app(AppKind::WorkBuddy);
        assert_eq!(workbuddy.apply_mode, ApplyMode::DirectConfig);
        assert!(workbuddy.config_target.contains("models.json"), "{}", workbuddy.config_target);
    }

    #[test]
    fn claude_never_uses_the_official_bootstrapper_as_a_source() {
        // 官方 ClaudeSetup.exe 只是在线引导器，拿它当安装包源会下到一个装不起来的
        // 7MB 引导器。这条断言防止以后有人"顺手"把官方地址加回来。
        let claude = builtin_app(AppKind::ClaudeDesktop);
        let official_like = claude.upgrade.sources.iter().any(|source| match source {
            ReleaseSource::Direct { url } => url.contains("downloads.claude.ai"),
            ReleaseSource::Squirrel { feed, .. } => feed.contains("downloads.claude.ai"),
            _ => false,
        });
        assert!(!official_like, "Claude 的安装包源里不应出现官方引导器地址");
    }
}

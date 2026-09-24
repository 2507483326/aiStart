//! 应用升级链路的中间模型（canonical model）。
//!
//! 这一层只描述「一次升级是什么」，不含任何 IO 与平台分支：
//! - 配置侧：`UpgradeSpec` / `ReleaseSource` 声明「去哪找安装包、怎么静默装」；
//! - 运行侧：`ReleaseAsset` 是解析阶段的唯一产物，`InstallOutcome` 是安装阶段的结论。
//!
//! 安装器类型与源类型都是**封闭枚举**（不是插件系统）：新增应用通常只需在
//! `catalog.rs` 加一份 `UpgradeSpec`；只有出现全新安装器类型时才加一个枚举分支。

// 这是升级链路的中间模型：各字段会被 diagnose / download / installers / reverify
// 等阶段陆续消费。尚未落地的阶段所对应的类型此刻看起来「未被使用」，属于
// 「先把模型定下来，再逐阶段接线」，不是遗留代码——故整模块放行 dead_code。
// 等 §6 各阶段接完后可移除此行，让它重新变回有效信号。
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// 安装器类型。决定用哪条静默安装命令，以及诊断时按哪种形态识别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallerKind {
    /// MSIX/APPX 包：`Add-AppxPackage`，per-user、免管理员、天然静默。
    Msix,
    /// NSIS 安装器：`/S` 静默。Tauri 的 `-setup.exe` 属于这一类。
    Nsis,
    /// Windows Installer：`msiexec /i … /qn /norestart`。
    Msi,
    /// Inno Setup：`/VERYSILENT`。**必须显式指定**——`.exe` 无法与 NSIS 区分。
    Inno,
    /// 绿色版压缩包：解压到目标目录（本期只留签名，不做实现）。
    Zip,
}

impl InstallerKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            InstallerKind::Msix => "msix",
            InstallerKind::Nsis => "nsis",
            InstallerKind::Msi => "msi",
            InstallerKind::Inno => "inno",
            InstallerKind::Zip => "zip",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            InstallerKind::Msix => "MSIX 包",
            InstallerKind::Nsis => "NSIS 安装器",
            InstallerKind::Msi => "Windows Installer (MSI)",
            InstallerKind::Inno => "Inno Setup 安装器",
            InstallerKind::Zip => "绿色版压缩包",
        }
    }

    /// 按下载文件名推断安装器类型。配置里显式指定时优先用配置值。
    ///
    /// `.exe` 一律推断为 NSIS：`.exe` 在文件层面无法区分 NSIS 与 Inno，
    /// 而 NSIS 是这两者里更常见的（Tauri 默认产物就是 NSIS），
    /// 因此 Inno 必须由配置显式声明，不靠猜。
    pub fn from_file_name(file_name: &str) -> Option<InstallerKind> {
        let lower = file_name.to_ascii_lowercase();
        if lower.ends_with(".msix") || lower.ends_with(".appx") {
            return Some(InstallerKind::Msix);
        }
        if lower.ends_with(".msi") {
            return Some(InstallerKind::Msi);
        }
        if lower.ends_with(".zip") {
            return Some(InstallerKind::Zip);
        }
        if lower.ends_with(".exe") {
            return Some(InstallerKind::Nsis);
        }
        None
    }

    /// 诊断用的反向映射：本地已安装形态 → 对应的安装器类型。
    pub fn from_install_form(form: InstallForm) -> InstallerKind {
        match form {
            InstallForm::Msix => InstallerKind::Msix,
            InstallForm::Squirrel => InstallerKind::Nsis,
            InstallForm::Msi => InstallerKind::Msi,
            InstallForm::Nsis => InstallerKind::Nsis,
            InstallForm::Portable => InstallerKind::Zip,
        }
    }
}

/// 本机已存在的安装形态。用于诊断「这台机器上有几处安装、这次会动哪一处」。
///
/// `Squirrel` 单列出来，是因为 Claude Desktop 从 Squirrel 迁移到了 MSIX：
/// 同一台机器上两种形态可能并存，而旧的那处仍可能被快捷方式指向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallForm {
    Msix,
    Squirrel,
    Msi,
    Nsis,
    Portable,
}

impl InstallForm {
    pub fn as_str(&self) -> &'static str {
        match self {
            InstallForm::Msix => "msix",
            InstallForm::Squirrel => "squirrel",
            InstallForm::Msi => "msi",
            InstallForm::Nsis => "nsis",
            InstallForm::Portable => "portable",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            InstallForm::Msix => "MSIX 包",
            InstallForm::Squirrel => "Squirrel（旧版安装器）",
            InstallForm::Msi => "MSI",
            InstallForm::Nsis => "NSIS",
            InstallForm::Portable => "绿色版",
        }
    }
}

/// 来源标记。落库与展示用，便于回答「这次到底是从哪儿下的」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceTag {
    /// 国内镜像短链（R2 / ModelScope 之类）。
    Mirror,
    /// 厂商官方地址。
    Official,
    /// GitHub Releases。
    Github,
    /// 配置里写死的直链。
    Direct,
    /// 兜底：打开官方下载页，由用户手动安装。
    Manual,
}

impl SourceTag {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceTag::Mirror => "mirror",
            SourceTag::Official => "official",
            SourceTag::Github => "github",
            SourceTag::Direct => "direct",
            SourceTag::Manual => "manual",
        }
    }
}

/// 一条候选下载源。`UpgradeSpec.sources` 是**有序**数组，逐个尝试，
/// 第一个「通过自洽校验」（见 `install::sources`）的胜出。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ReleaseSource {
    /// 国内镜像直链/短链：URL 永远指向最新版，**自身不带版本信息**。
    ///
    /// 实测 `claudeapp.agentsmirror.com/latest/win-x64`：无重定向、文件名
    /// `Claude-win-x64.msix` 不含版本、`ETag` 是分片值而非 sha256 → 版本只能靠
    /// `updates::latest_version()` 的探测结果比对，校验值只能靠 companion checksums。
    Mirror {
        url: String,
        /// 伴随的校验清单地址（`sha256  filename` 逐行）。缺省则该源无法校验。
        checksums: Option<String>,
    },
    /// GitHub Releases：取 `releases/latest`，按 `asset_match` 挑资产。
    /// 版本取 `tag_name`，sha256 取该资产的 `digest`（去掉 `sha256:` 前缀）。
    Github { repo: String, asset_match: String },
    /// Squirrel `RELEASES` feed：取最新一行的资产名，下载地址 = `base` + 资产名。
    Squirrel { feed: String, base: String },
    /// 配置里写死的直链，支持 `{version}` 占位（版本来自探测结果）。
    Direct { url: String },
    /// 兜底：打开官方下载页，人工安装。必须是 `sources` 的最后一项。
    Manual { page: String },
}

impl ReleaseSource {
    /// 该源对应的来源标记（落库用）。
    pub fn tag(&self) -> SourceTag {
        match self {
            ReleaseSource::Mirror { .. } => SourceTag::Mirror,
            ReleaseSource::Github { .. } => SourceTag::Github,
            ReleaseSource::Squirrel { .. } => SourceTag::Official,
            ReleaseSource::Direct { .. } => SourceTag::Direct,
            ReleaseSource::Manual { .. } => SourceTag::Manual,
        }
    }
}

/// 一次升级的声明式规格。新增应用只改这里（外加 `AppKind` 一个变体）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpgradeSpec {
    /// 有序候选下载源；第一个通过自洽校验的胜出。最后一项通常是 `Manual`。
    pub sources: Vec<ReleaseSource>,
    /// 安装器类型；`None` = 按下载文件名推断（见 `InstallerKind::from_file_name`）。
    pub installer: Option<InstallerKind>,
    /// 覆盖默认静默参数（`None` = 用 `install::installers` 的默认表）。
    pub silent_args: Option<Vec<String>>,
    /// 是否必然需要管理员：`true` → 直接走 UAC，不做免提权尝试。
    pub requires_admin: bool,
    /// 安装前需要结束的进程名（**镜像名**，如 `DSH Desktop.exe`）。
    pub process_names: Vec<String>,
    /// 各源都没给校验值时的兜底 sha256。
    pub sha256: Option<String>,
    /// 诊断锚定：MSIX 包名前缀（实测 `Claude` 与 `PackageFullName` 前缀一致）。
    pub msix_name_prefix: Option<String>,
    /// 诊断锚定：注册表 `DisplayName` 前缀。
    ///
    /// 实测 DSH Desktop 的 `InstallLocation` 为空字符串，位置锚定在它身上不可用，
    /// 只能靠 `DisplayName`（`"DSH Desktop 2.0.5"`）匹配。
    pub display_name_match: Option<String>,
    /// 诊断锚定：期望安装位置。仅作可选补充，不作为唯一依据。
    pub expected_location: Option<String>,
    /// 手动安装时的官方下载页。
    pub manual_page: String,
}

/// 解析阶段的唯一产物：这次要装哪个包、从哪来、怎么校验、用哪种安装器。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseAsset {
    pub version: Option<String>,
    /// 实际命中的下载地址。
    pub url: String,
    pub file_name: String,
    pub size: Option<u64>,
    pub sha256: Option<String>,
    pub installer: InstallerKind,
    pub source: SourceTag,
}

/// 本机的一处安装。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedInstall {
    pub form: InstallForm,
    pub version: Option<String>,
    pub location: Option<String>,
    /// 本次升级将作用的那一处。
    pub is_target: bool,
}

/// 安装形态诊断结果。
///
/// 与源解析的取向**相反**：诊断是本地探测，失败只降级（`is_conflict = false`、
/// `target = None`）而**不阻断**升级；源解析处在信任边界上，坏数据必须硬失败。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallDiagnosis {
    pub installs: Vec<DetectedInstall>,
    /// 是否检测到多处安装（`installs.len() > 1`）。
    pub is_conflict: bool,
    pub target: Option<InstallForm>,
}

/// 安装结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallVerdict {
    /// 装完复验：版本已变（或已达到探测到的最新版）。
    Succeeded,
    /// 命令退出码为 0，但复验发现版本没变 —— 软失败。
    ///
    /// 上游 updater 有时在未实际改动版本时仍返回 0；也可能被另一处安装遮蔽。
    /// 必须与 `Succeeded` 区分，否则会给用户误报升级成功。
    Unchanged,
    Failed,
    /// 用户取消（含拒绝 UAC 提权）。
    Cancelled,
}

impl InstallVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            InstallVerdict::Succeeded => "succeeded",
            InstallVerdict::Unchanged => "unchanged",
            InstallVerdict::Failed => "failed",
            InstallVerdict::Cancelled => "cancelled",
        }
    }

    /// 是否属于「确定结果」——只有确定结果才落 `app_version_records`。
    pub fn is_conclusive(&self) -> bool {
        matches!(
            self,
            InstallVerdict::Succeeded
                | InstallVerdict::Unchanged
                | InstallVerdict::Failed
                | InstallVerdict::Cancelled
        )
    }
}

/// 安装阶段的结果：真实退出码 + 复验结论。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallOutcome {
    pub verdict: InstallVerdict,
    /// 安装进程的真实退出码；提权失败或未执行时为 `None`。
    pub exit_code: Option<i32>,
    pub elevated: bool,
    pub version_before: Option<String>,
    pub version_after: Option<String>,
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_installer_kind_from_file_name() {
        assert_eq!(
            InstallerKind::from_file_name("Claude-win-x64.msix"),
            Some(InstallerKind::Msix)
        );
        assert_eq!(
            InstallerKind::from_file_name("Claude-win-arm64.MSIX"),
            Some(InstallerKind::Msix)
        );
        assert_eq!(
            InstallerKind::from_file_name("DSH-Desktop-2.0.13-x64-Setup.exe"),
            Some(InstallerKind::Nsis)
        );
        assert_eq!(
            InstallerKind::from_file_name("node-v24.21.0-win-x64.zip"),
            Some(InstallerKind::Zip)
        );
        assert_eq!(InstallerKind::from_file_name("RELEASES"), None);
    }

    #[test]
    fn exe_is_never_inferred_as_inno() {
        // `.exe` 在文件层面无法区分 NSIS 与 Inno；Inno 必须由配置显式声明。
        for name in ["setup.exe", "installer.exe", "DSH Desktop.exe"] {
            assert_eq!(
                InstallerKind::from_file_name(name),
                Some(InstallerKind::Nsis)
            );
        }
    }

    #[test]
    fn source_tags_match_their_variants() {
        assert_eq!(
            ReleaseSource::Mirror {
                url: "u".into(),
                checksums: None
            }
            .tag(),
            SourceTag::Mirror
        );
        assert_eq!(
            ReleaseSource::Github {
                repo: "r".into(),
                asset_match: "a".into()
            }
            .tag(),
            SourceTag::Github
        );
        assert_eq!(
            ReleaseSource::Squirrel {
                feed: "f".into(),
                base: "b".into()
            }
            .tag(),
            SourceTag::Official
        );
        assert_eq!(
            ReleaseSource::Manual { page: "p".into() }.tag(),
            SourceTag::Manual
        );
    }

    #[test]
    fn squirrel_form_is_a_nsis_installer() {
        assert_eq!(
            InstallerKind::from_install_form(InstallForm::Squirrel),
            InstallerKind::Nsis
        );
        assert_eq!(
            InstallerKind::from_install_form(InstallForm::Msix),
            InstallerKind::Msix
        );
    }
}

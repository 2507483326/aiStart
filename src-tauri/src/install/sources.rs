//! 下载源解析 + 自洽校验。
//!
//! ## 为什么不是简单的「按顺序取第一个能解析的源」
//!
//! 镜像短链（如 `claudeapp.agentsmirror.com/latest/win-x64`）**自身不带版本信息**。
//! 实测：无重定向、文件名 `Claude-win-x64.msix` 不含版本号、`ETag` 是分片值
//! （`51322f53…-35`）而非 sha256。若按「第一个能解析的源就短路」，一个格式完全
//! 合法但内容过期的镜像（R2 上的旧对象 / CDN 缓存）会被静默采用——用户装到旧版
//! 却以为升级成功，且没有任何报错。
//!
//! 所以这里把源分成两种**角色**：
//!
//! - **权威源**（`Github` / `Squirrel` / `Direct`）：能给出「版本 + sha256」，
//!   可能还给出包大小。按 `sources` 顺序尝试。
//! - **加速源**（`Mirror`）：只提供更快的下载通道。**只有当它的可观测特征
//!   （包大小、或 sha256 与伴随清单）与权威资产对得上时才会被采用**，
//!   否则退回权威源下载。
//!
//! 这与 CC Switch 对自家 R2 镜像的处理同构：镜像被视为**不可信**，
//! 安全性由客户端拿权威侧的校验值兜底。
//!
//! ## 两类失败的取向相反（刻意如此）
//!
//! - 源解析处在**信任边界**上：校验值缺失、版本落后、清单对不上 → **硬失败**，
//!   宁可让用户走人工安装，也不装一个无法验证的 288MB 二进制。
//! - 本机诊断（`diagnose`）是**本地探测**：失败只降级，绝不阻断升级。

use std::time::Duration;

use serde_json::Value;

use crate::domain::app::AppKind;
use crate::domain::release::{InstallerKind, ReleaseAsset, ReleaseSource, SourceTag, UpgradeSpec};
use crate::error::{AppError, AppResult};
use crate::events;
use crate::platform;
use crate::providers::http_client;
use crate::updates::{self, FoundVersion};

const USER_AGENT: &str = "ai-start";
/// 元数据请求超时。GitHub API 在国内常慢，比 `updates.rs` 的 6s 放宽一些。
const METADATA_TIMEOUT: Duration = Duration::from_secs(20);
/// 镜像 HEAD 探测：只取响应头，不该久等。
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// 解析结果。
#[derive(Debug, Clone)]
pub enum ResolveOutcome {
    /// 拿到了可安装、且已通过自洽校验的资产。
    Ready(ReleaseAsset),
    /// 已经是最新，无需下载。
    UpToDate {
        installed: Option<String>,
        latest: Option<String>,
    },
}

/// 走一遍配置的候选源，产出这次要装的包。
pub async fn resolve(kind: AppKind, spec: &UpgradeSpec) -> AppResult<ResolveOutcome> {
    // 先拿已安装版本：版本感知的探测源需要它（见 `updates::latest_version`）。
    let installed = installed_version(kind);
    let probed = updates::latest_version(kind, installed.as_deref()).await;

    // 已是最新就不下载。口径与版本徽标一致（`is_newer` 会把尾随 0 段归一化，
    // 这正是 MSIX 的 `2.7032.0.0` 与探测源的 `2.7032.0` 能对上的原因）。
    if let (Some(latest), Some(installed)) = (probed.as_ref(), installed.as_deref()) {
        if !updates::is_newer(&latest.version, installed) {
            return Ok(ResolveOutcome::UpToDate {
                installed: Some(installed.to_string()),
                latest: Some(latest.version.clone()),
            });
        }
    }

    let mirror = spec
        .sources
        .iter()
        .find(|source| matches!(source, ReleaseSource::Mirror { .. }));

    let mut problems: Vec<String> = Vec::new();
    for source in &spec.sources {
        // 加速源与人工兜底不在这一轮参与「权威元数据」解析。
        if matches!(
            source,
            ReleaseSource::Mirror { .. } | ReleaseSource::Manual { .. }
        ) {
            continue;
        }

        let asset = match resolve_authoritative(source, probed.as_ref(), spec).await {
            Ok(asset) => asset,
            Err(error) => {
                problems.push(format!("{}（{error}）", source_label(source)));
                continue;
            }
        };

        // 权威源也可能落后于探测到的最新版：探测按 `latest_version_urls` 顺序走
        // （官方优先），而下载源可能是某个同步延迟的镜像仓库。此时**不能采用**——
        // 否则会「升级」到比当前更旧的版本。
        if is_stale_authoritative(
            probed.as_ref().map(|found| found.version.as_str()),
            asset.version.as_deref(),
        ) {
            let detail = format!(
                "{} 的版本 {} 落后于探测到的最新版 {}",
                source_label(source),
                asset.version.as_deref().unwrap_or("未知"),
                probed.as_ref().map(|f| f.version.as_str()).unwrap_or("未知"),
            );
            record_stale(kind, &detail);
            problems.push(detail);
            continue;
        }

        let asset = match mirror {
            Some(mirror) => prefer_mirror(kind, asset, mirror).await,
            None => asset,
        };
        return Ok(ResolveOutcome::Ready(asset));
    }

    // 没有任何权威源可用 → 硬失败。绝不「只剩镜像就放行」：那等于装一个
    // 无法验证的大体积二进制，且我们连它的版本都无从确认。
    Err(AppError::Message(format!(
        "所有候选下载源都不可用（{}）。可点「打开官方下载页」手动安装。",
        if problems.is_empty() {
            "未配置可用的权威下载源".to_string()
        } else {
            problems.join("；")
        }
    )))
}

/// 解析一个权威源，产出「版本 + sha256 + 大小」都尽量完整的资产。
async fn resolve_authoritative(
    source: &ReleaseSource,
    probed: Option<&FoundVersion>,
    spec: &UpgradeSpec,
) -> AppResult<ReleaseAsset> {
    match source {
        ReleaseSource::Github { repo, asset_match } => {
            let url = format!("https://api.github.com/repos/{repo}/releases/latest");
            let body = fetch_text(&url, METADATA_TIMEOUT).await?;
            let json: Value = serde_json::from_str(&body)?;
            let release = GithubRelease::parse(&json, asset_match)?;
            Ok(release.into_asset(spec)?)
        }

        ReleaseSource::Squirrel { feed, base } => {
            let body = fetch_text(feed, METADATA_TIMEOUT).await?;
            // Squirrel feed 每行 `<sha1> <asset> <size>`，最新一行在最末。
            let asset_name = body
                .lines()
                .rev()
                .find_map(|line| line.split_whitespace().nth(1))
                .ok_or_else(|| AppError::Message("RELEASES feed 里读不到资产名".into()))?
                .to_string();
            // feed 里的 sha1 与我们需要的 sha256 不是一回事，不能拿来当校验值。
            let sha256 = require_sha256(spec)?;
            let installer = installer_for(spec, &asset_name)?;
            Ok(ReleaseAsset {
                version: updates::extract_version(&asset_name),
                url: format!("{}{}", base.trim_end_matches('/'), format!("/{asset_name}")),
                file_name: asset_name,
                size: None,
                sha256: Some(sha256),
                installer,
                source: SourceTag::Official,
            })
        }

        ReleaseSource::Direct { url } => {
            // 直链可以带 `{version}` 占位，版本号来自探测结果。
            let version = probed
                .map(|found| found.version.clone())
                .ok_or_else(|| AppError::Message("无法确认最新版本，无法确定下载地址".into()))?;
            let resolved = url.replace("{version}", &version);
            let file_name = file_name_from_url(&resolved)
                .ok_or_else(|| AppError::Message(format!("无法从地址推断文件名: {resolved}")))?;
            let installer = installer_for(spec, &file_name)?;
            Ok(ReleaseAsset {
                version: Some(version),
                url: resolved,
                file_name,
                size: None,
                sha256: Some(require_sha256(spec)?),
                installer,
                source: SourceTag::Direct,
            })
        }

        ReleaseSource::Mirror { .. } | ReleaseSource::Manual { .. } => Err(AppError::Message(
            "该源不提供权威元数据（版本与校验值）".into(),
        )),
    }
}

/// 镜像加速：只有镜像的可观测特征与权威资产一致时才改用镜像下载。
///
/// **任何一步不确定都直接退回权威源**——不失败、不阻断，只是用得慢一点。
async fn prefer_mirror(kind: AppKind, asset: ReleaseAsset, mirror: &ReleaseSource) -> ReleaseAsset {
    let ReleaseSource::Mirror { url, checksums } = mirror else {
        return asset;
    };

    let Some(head) = probe_mirror(url).await else {
        // 探测不到（不支持 HEAD、网络不可达…）→ 用权威源，不冒险。
        return asset;
    };

    // 判定为可信的依据至少要有其一，否则不敢用镜像。
    let mut validated = false;

    // 依据一：包大小。最便宜，实测可用（镜像与 GitHub 都是 287766830）。
    if let (Some(expected), Some(actual)) = (asset.size, head.size) {
        if expected != actual {
            let detail = format!(
                "镜像包大小 {actual} 与权威源 {expected} 不一致，判为过期",
            );
            record_stale(kind, &detail);
            return asset;
        }
        validated = true;
    }

    // 依据二：伴随清单里的 sha256。
    if let (Some(endpoint), Some(expected)) = (checksums.as_deref(), asset.sha256.as_deref()) {
        match fetch_checksums(endpoint, &head.file_name).await {
            Some(actual) if actual.eq_ignore_ascii_case(expected) => validated = true,
            Some(actual) => {
                let detail = format!(
                    "镜像校验值 {actual} 与权威源 {expected} 不一致，判为过期",
                );
                record_stale(kind, &detail);
                return asset;
            }
            None => {
                // 清单里没有这个文件：可能是镜像换了命名，也不可信。
                let detail = format!("镜像校验清单里找不到 {}", head.file_name);
                record_stale(kind, &detail);
                return asset;
            }
        }
    }

    if !validated {
        return asset;
    }

    ReleaseAsset {
        url: url.clone(),
        source: SourceTag::Mirror,
        ..asset
    }
}

/// 镜像 HEAD 探测结果。
#[derive(Debug, Clone)]
struct MirrorHead {
    size: Option<u64>,
    file_name: String,
}

async fn probe_mirror(url: &str) -> Option<MirrorHead> {
    let response = http_client()
        .head(url)
        .header("User-Agent", USER_AGENT)
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }

    let size = response.content_length();
    let file_name = response
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_content_disposition_filename)
        .or_else(|| file_name_from_url(url))
        .unwrap_or_default();

    (size.is_some() || !file_name.is_empty()).then_some(MirrorHead { size, file_name })
}

async fn fetch_checksums(endpoint: &str, file_name: &str) -> Option<String> {
    let body = fetch_text(endpoint, METADATA_TIMEOUT).await.ok()?;
    parse_checksums(&body, file_name)
}

async fn fetch_text(url: &str, timeout: Duration) -> AppResult<String> {
    let response = http_client()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .timeout(timeout)
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(AppError::Message(format!(
            "HTTP {}",
            response.status().as_u16()
        )));
    }
    Ok(response.text().await?)
}

// ---------------------------------------------------------------------------
// 纯函数（可直接单测，不碰网络）
// ---------------------------------------------------------------------------

/// 权威源是否落后于探测到的最新版。
///
/// 只有两边都读得出具体版本时才比较；任一侧未知一律**不判 stale**
/// （宁可继续用，也不要因为读不出版本就把可用源全部否掉）。
fn is_stale_authoritative(probed_latest: Option<&str>, candidate: Option<&str>) -> bool {
    match (probed_latest, candidate) {
        (Some(latest), Some(candidate)) => updates::is_newer(latest, candidate),
        _ => false,
    }
}

/// 解析 `sha256sum` 风格的清单：`<hex>  <filename>`，逐行。
/// 兼容二进制标记（`*filename`）与大小写差异；按文件名精确匹配。
fn parse_checksums(text: &str, file_name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?;
        let name = name.strip_prefix('*').unwrap_or(name);
        name.eq_ignore_ascii_case(file_name)
            .then(|| hash.to_string())
    })
}

/// 从 `Content-Disposition` 里取文件名，如 `attachment; filename="Claude-win-x64.msix"`。
fn parse_content_disposition_filename(header: &str) -> Option<String> {
    let marker = "filename=";
    let start = header.to_ascii_lowercase().find(marker)? + marker.len();
    let value = header[start..].trim();
    let value = value.trim_matches('"').trim_matches('\'');
    let value = value.split(';').next()?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn file_name_from_url(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    path.rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty() && segment.contains('.'))
        .map(str::to_string)
}

/// 安装器类型：配置优先，其次按扩展名推断。两者都拿不到就报错，
/// **不默认猜 NSIS**——猜错会静默跑出一个带 UI 的安装器。
fn installer_for(spec: &UpgradeSpec, file_name: &str) -> AppResult<InstallerKind> {
    if let Some(kind) = spec.installer {
        return Ok(kind);
    }
    InstallerKind::from_file_name(file_name).ok_or_else(|| {
        AppError::Message(format!(
            "无法从文件名「{file_name}」判断安装器类型，请在配置里指定 installer"
        ))
    })
}

fn require_sha256(spec: &UpgradeSpec) -> AppResult<String> {
    spec.sha256.clone().ok_or_else(|| {
        AppError::Message(
            "该下载源不带校验值，且配置里也没有兜底 sha256；拒绝安装无法验证的安装包".into(),
        )
    })
}

fn source_label(source: &ReleaseSource) -> String {
    match source {
        ReleaseSource::Mirror { url, .. } => format!("镜像 {url}"),
        ReleaseSource::Github { repo, .. } => format!("GitHub {repo}"),
        ReleaseSource::Squirrel { feed, .. } => format!("Squirrel {feed}"),
        ReleaseSource::Direct { url } => format!("直链 {url}"),
        ReleaseSource::Manual { page } => format!("人工 {page}"),
    }
}

fn installed_version(kind: AppKind) -> Option<String> {
    platform::configurator_for(kind)
        .detect()
        .ok()
        .filter(|detect| detect.installed)
        .and_then(|detect| detect.version)
}

fn record_stale(kind: AppKind, detail: &str) {
    // 只写 events，不写 app_version_records：这不是一次安装动作，
    // 不该影响「上次检查结果」与徽标口径。
    events::log(
        "system",
        None,
        "app.install.source.stale",
        Some("app"),
        Some(kind.as_str()),
        Some(serde_json::json!({ "detail": detail })),
    );
}

/// GitHub `releases/latest` 的解析产物。
#[derive(Debug, Clone, PartialEq, Eq)]
struct GithubRelease {
    version: Option<String>,
    asset_name: String,
    asset_url: String,
    asset_size: Option<u64>,
    asset_sha256: Option<String>,
}

impl GithubRelease {
    fn parse(json: &Value, asset_match: &str) -> AppResult<GithubRelease> {
        let version = json
            .get("tag_name")
            .and_then(Value::as_str)
            .and_then(updates::extract_version);

        let assets = json
            .get("assets")
            .and_then(Value::as_array)
            .ok_or_else(|| AppError::Message("release 响应里没有 assets 数组".into()))?;

        let needle = asset_match.to_ascii_lowercase();
        let asset = assets
            .iter()
            .find(|asset| {
                asset
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.to_ascii_lowercase().contains(&needle))
            })
            .ok_or_else(|| {
                AppError::Message(format!("该 release 里没有匹配「{asset_match}」的资产"))
            })?;

        let asset_name = asset
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::Message("资产缺 name".into()))?
            .to_string();
        let asset_url = asset
            .get("browser_download_url")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::Message("资产缺 browser_download_url".into()))?
            .to_string();
        let asset_size = asset.get("size").and_then(Value::as_u64);
        // GitHub 自 2025 起在 release 资产上提供 `digest`（`sha256:...`）。
        let asset_sha256 = asset
            .get("digest")
            .and_then(Value::as_str)
            .map(|digest| digest.trim_start_matches("sha256:").to_string())
            .filter(|digest| !digest.is_empty());

        Ok(GithubRelease {
            version,
            asset_name,
            asset_url,
            asset_size,
            asset_sha256,
        })
    }

    fn into_asset(self, spec: &UpgradeSpec) -> AppResult<ReleaseAsset> {
        let installer = installer_for(spec, &self.asset_name)?;
        let sha256 = self
            .asset_sha256
            .or_else(|| spec.sha256.clone())
            .ok_or_else(|| {
                AppError::Message(format!(
                    "GitHub 资产「{}」没有 digest，配置里也没有兜底 sha256；拒绝安装无法验证的包",
                    self.asset_name
                ))
            })?;
        Ok(ReleaseAsset {
            version: self.version,
            url: self.asset_url,
            file_name: self.asset_name,
            size: self.asset_size,
            sha256: Some(sha256),
            installer,
            source: SourceTag::Github,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 取自 `Wangnov/claude-app-mirror` release 的真实响应形状（已裁剪）。
    fn claude_release_json() -> Value {
        json!({
            "tag_name": "claude-app-v2.7032.0",
            "assets": [
                { "name": "Claude-mac-universal.dmg", "browser_download_url": "https://example.com/Claude-mac-universal.dmg",
                  "size": 373393800u64, "digest": "sha256:32c25f2e4ae17f97e3996f95f07a9494a90ceab52311e3eec3cf438f7e748f0d" },
                { "name": "Claude-win-arm64.msix", "browser_download_url": "https://example.com/Claude-win-arm64.msix",
                  "size": 281326865u64, "digest": "sha256:0ca30c5e4d0d58a05090c3b0f417bd53b22990863a7cd383bc99c06cf951c3f1" },
                { "name": "Claude-win-x64.msix", "browser_download_url": "https://example.com/Claude-win-x64.msix",
                  "size": 287766830u64, "digest": "sha256:f11b7d5dc5c969f19248cf598f23d35c552311a0ed703e106542c057c4016827" }
            ]
        })
    }

    #[test]
    fn picks_the_x64_asset_and_reads_its_digest() {
        let release = GithubRelease::parse(&claude_release_json(), "Claude-win-x64.msix").unwrap();
        assert_eq!(release.asset_name, "Claude-win-x64.msix");
        assert_eq!(release.asset_size, Some(287_766_830));
        assert_eq!(release.version.as_deref(), Some("2.7032.0"));
        assert_eq!(
            release.asset_sha256.as_deref(),
            Some("f11b7d5dc5c969f19248cf598f23d35c552311a0ed703e106542c057c4016827")
        );
    }

    #[test]
    fn x64_match_does_not_grab_the_arm64_asset() {
        let release = GithubRelease::parse(&claude_release_json(), "Claude-win-x64.msix").unwrap();
        assert_ne!(release.asset_name, "Claude-win-arm64.msix");
    }

    #[test]
    fn matches_dsh_setup_asset_by_suffix() {
        let json = json!({
            "tag_name": "v2.0.13",
            "assets": [
                { "name": "DSH.Desktop-2.0.13-universal.dmg", "browser_download_url": "https://example.com/a.dmg", "size": 1u64,
                  "digest": "sha256:aa" },
                { "name": "DSH-Desktop-2.0.13-x64-Setup.exe", "browser_download_url": "https://example.com/DSH-Desktop-2.0.13-x64-Setup.exe",
                  "size": 156056465u64, "digest": "sha256:3aa0c75b891470d1621f5589574530c7509b68d854859b059164fbfb7cd93490" }
            ]
        });
        let release = GithubRelease::parse(&json, "-x64-Setup.exe").unwrap();
        assert_eq!(release.asset_name, "DSH-Desktop-2.0.13-x64-Setup.exe");
        assert_eq!(release.version.as_deref(), Some("2.0.13"));
    }

    #[test]
    fn rejects_a_release_without_a_matching_asset() {
        let error = GithubRelease::parse(&claude_release_json(), "Claude-win-x86.msix").unwrap_err();
        assert!(error.to_string().contains("没有匹配"));
    }

    #[test]
    fn parses_checksums_list() {
        // 实测 `/latest/checksums` 的真实内容。
        let body = "32c25f2e4ae17f97e3996f95f07a9494a90ceab52311e3eec3cf438f7e748f0d  Claude-mac-universal.dmg\n\
                    0ca30c5e4d0d58a05090c3b0f417bd53b22990863a7cd383bc99c06cf951c3f1  Claude-win-arm64.msix\n\
                    f11b7d5dc5c969f19248cf598f23d35c552311a0ed703e106542c057c4016827  Claude-win-x64.msix\n";
        assert_eq!(
            parse_checksums(body, "Claude-win-x64.msix").as_deref(),
            Some("f11b7d5dc5c969f19248cf598f23d35c552311a0ed703e106542c057c4016827")
        );
        // 找不到就是找不到，不能退化成「随便给一个」。
        assert_eq!(parse_checksums(body, "Claude-win-x86.msix"), None);
    }

    #[test]
    fn checksums_parser_tolerates_binary_marker_and_case() {
        let body = "ABC123  *Claude-Win-X64.MSIX\n";
        assert_eq!(
            parse_checksums(body, "Claude-win-x64.msix").as_deref(),
            Some("ABC123")
        );
    }

    #[test]
    fn parses_filename_from_content_disposition() {
        assert_eq!(
            parse_content_disposition_filename("attachment; filename=\"Claude-win-x64.msix\"").as_deref(),
            Some("Claude-win-x64.msix")
        );
        assert_eq!(
            parse_content_disposition_filename("attachment; filename=plain.msix").as_deref(),
            Some("plain.msix")
        );
        assert_eq!(
            parse_content_disposition_filename("inline").as_deref(),
            None
        );
    }

    #[test]
    fn extracts_file_name_from_url() {
        assert_eq!(
            file_name_from_url("https://example.com/a/b/Claude-win-x64.msix?x=1").as_deref(),
            Some("Claude-win-x64.msix")
        );
        // 没有扩展名的路径段不算文件名（如 `.../latest/win-x64`）。
        assert_eq!(file_name_from_url("https://example.com/latest/win-x64"), None);
        assert_eq!(file_name_from_url("https://example.com/"), None);
    }

    #[test]
    fn stale_detection_ignores_unknown_versions() {
        // 这是 §6.3 规则 1 的核心：权威源落后于探测结果 → 不能采用。
        assert!(is_stale_authoritative(Some("2.7032.0"), Some("2.7031.0")));
        // 相等（含尾随 0 段差异）不算落后。
        assert!(!is_stale_authoritative(Some("2.7032.0"), Some("2.7032.0")));
        assert!(!is_stale_authoritative(Some("2.7032.0"), Some("2.7032.0.0")));
        // 更高不算落后。
        assert!(!is_stale_authoritative(Some("2.7031.0"), Some("2.7032.0")));
        // 任一侧未知 → 一律不判 stale，避免把可用源全部否掉。
        assert!(!is_stale_authoritative(None, Some("2.7031.0")));
        assert!(!is_stale_authoritative(Some("2.7032.0"), None));
    }

    #[test]
    fn installer_prefers_config_then_extension() {
        let mut spec = UpgradeSpec::default();

        // 未配置 → 按扩展名推断。
        assert_eq!(
            installer_for(&spec, "DSH-Desktop-2.0.13-x64-Setup.exe").unwrap(),
            InstallerKind::Nsis
        );

        // 配置优先：同一个 .exe 被显式声明为 Inno。
        spec.installer = Some(InstallerKind::Inno);
        assert_eq!(
            installer_for(&spec, "DSH-Desktop-2.0.13-x64-Setup.exe").unwrap(),
            InstallerKind::Inno
        );

        // 认不出来且没配置 → 报错，而不是猜一个。
        let bare = UpgradeSpec::default();
        assert!(installer_for(&bare, "RELEASES").is_err());
    }

    #[test]
    fn missing_checksum_is_a_hard_error() {
        let spec = UpgradeSpec::default();
        let error = require_sha256(&spec).unwrap_err();
        assert!(error.to_string().contains("拒绝安装无法验证"));

        let spec = UpgradeSpec {
            sha256: Some("abc".into()),
            ..Default::default()
        };
        assert_eq!(require_sha256(&spec).unwrap(), "abc");
    }

    #[test]
    fn github_asset_without_digest_falls_back_to_config_sha256() {
        let json = json!({
            "tag_name": "v1.0.0",
            "assets": [{ "name": "Tool-x64-Setup.exe", "browser_download_url": "https://example.com/t.exe", "size": 10u64 }]
        });
        let release = GithubRelease::parse(&json, "-x64-Setup.exe").unwrap();

        // 没有兜底 → 硬失败。
        assert!(release.clone().into_asset(&UpgradeSpec::default()).is_err());

        // 有兜底 → 采用兜底值。
        let spec = UpgradeSpec {
            sha256: Some("fallback".into()),
            ..Default::default()
        };
        let asset = release.into_asset(&spec).unwrap();
        assert_eq!(asset.sha256.as_deref(), Some("fallback"));
        assert_eq!(asset.installer, InstallerKind::Nsis);
    }
}

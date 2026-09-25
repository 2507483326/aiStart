use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::time::Duration;

use rusqlite::params;
use serde::Serialize;

use crate::db;
use crate::domain::app::AppKind;
use crate::domain::catalog;
use crate::providers::http_client;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);
const USER_AGENT: &str = "ai-start";

#[derive(Debug, Clone)]
pub struct FoundVersion {
    pub version: String,
    pub source_url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckSnapshot {
    pub latest_version: Option<String>,
    pub update_available: bool,
}

/// Walks the descriptor's sources in order (official first, mirror as fallback)
/// and returns the first version that can be read off the response.
///
/// `installed` 供**版本感知**的探测源使用：少数官方 feed 必须带上当前版本才会
/// 返回「这台机器该升到的目标版本」（WorkBuddy 的 `v2/update` 就是如此，服务端按
/// 版本灰度，给 `0.0.0` 只会返回一个旧目标）。URL 里的 `{version}` 会被替换成
/// 已安装版本；不含占位符的源（Claude RELEASES、GitHub release…）不受影响。
pub async fn latest_version(kind: AppKind, installed: Option<&str>) -> Option<FoundVersion> {
    for template in catalog::builtin_app(kind).latest_version_urls {
        let Some(url) = resolve_probe_url(&template, installed) else {
            continue;
        };
        let Some(body) = fetch(&url).await else {
            continue;
        };
        if let Some(version) = parse_latest(&body, &url) {
            return Some(FoundVersion {
                version,
                source_url: url,
            });
        }
    }
    None
}

/// 替换探测 URL 里的 `{version}` 占位符。
///
/// 缺已安装版本时返回 `None`（调用方跳过这条源）：与其拿一个填不出占位符的地址
/// 去猜，不如老实放弃——这条源本就回答不了「该升到哪个版本」。
fn resolve_probe_url(template: &str, installed: Option<&str>) -> Option<String> {
    if !template.contains("{version}") {
        return Some(template.to_string());
    }
    installed.map(|version| template.replace("{version}", version))
}

async fn fetch(url: &str) -> Option<String> {
    let response = http_client()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.text().await.ok()
}

fn parse_latest(body: &str, url: &str) -> Option<String> {
    if url.ends_with("RELEASES") {
        // Squirrel feed: "<sha1> <asset> <size>" per line, newest last.
        return body
            .lines()
            .rev()
            .find_map(|line| line.split_whitespace().nth(1))
            .and_then(extract_version);
    }
    if url.contains("api.github.com") {
        let value: serde_json::Value = serde_json::from_str(body).ok()?;
        return extract_version(value.get("tag_name")?.as_str()?);
    }
    if url.contains("copilot.tencent.com") {
        // WorkBuddy 的 `/v2/update`：JSON，目标版本在 `productVersion`（旧版回退 `version`）。
        // 已是最新时服务端返回 204 空体，`from_str` 失败 → `None`，即「本次读不出目标版本」。
        let value: serde_json::Value = serde_json::from_str(body).ok()?;
        let raw = value
            .get("productVersion")
            .or_else(|| value.get("version"))?
            .as_str()?;
        return extract_version(raw);
    }
    if url.contains("zcode.z.ai") {
        // ZCode 更新日志页（Next.js SSR 的 HTML）：版本按新→旧排列，正文里第一条
        // `Release vX.Y.Z` 即当前最新版本。页面没有可用的 JSON 版本接口，
        // electron-builder 的 latest.yml 也只有带版本号的路径（`.../releases/{version}/...`），
        // 拿不到版本号就拼不出地址，故只能解析这一页。
        return extract_version(body.split("Release v").nth(1)?);
    }
    extract_version(body.trim().lines().next()?)
}

/// Pulls the first dotted numeric token out of arbitrary text,
/// e.g. `AnthropicClaude-2.7032.0-full.nupkg` or `claude-app-v2.7032.0` -> `2.7032.0`.
///
/// 复用给 `install::sources`：资产文件名里的版本号是同一套解析规则，
/// 不在那边重写一份，避免两处对「什么算版本号」的判断漂移。
pub(crate) fn extract_version(text: &str) -> Option<String> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find(|token| token.starts_with(|c: char| c.is_ascii_digit()) && token.contains('.'))
        .map(str::to_string)
}

pub fn is_newer(latest: &str, installed: &str) -> bool {
    compare(latest, installed) == Ordering::Greater
}

fn compare(left: &str, right: &str) -> Ordering {
    let left = segments(left);
    let right = segments(right);
    for index in 0..left.len().max(right.len()) {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        match a.cmp(&b) {
            Ordering::Equal => continue,
            other => return other,
        }
    }
    Ordering::Equal
}

fn segments(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map(|part| part.trim().parse().unwrap_or(0))
        .collect()
}

/// 写入/更新某个应用的版本检查快照（每个应用一行；重复刷新只覆盖这一行）。
#[allow(clippy::too_many_arguments)]
pub fn record_check(
    kind: AppKind,
    installed: Option<&str>,
    latest_version: Option<&str>,
    source_url: Option<&str>,
    update_available: bool,
    status: &str,
    message: Option<&str>,
) {
    let now = db::now_ms();
    let _ = db::with_conn(|connection| {
        connection.execute(
            "INSERT INTO app_version_records (app_kind, action, installed_version, target_version, latest_version, \
             update_available, source_url, status, message, event_time, created_time, update_time) \
             VALUES (?1, 'check', ?2, ?3, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?8) \
             ON CONFLICT(app_kind) DO UPDATE SET \
             action = excluded.action, installed_version = excluded.installed_version, \
             target_version = excluded.target_version, latest_version = excluded.latest_version, \
             update_available = excluded.update_available, source_url = excluded.source_url, \
             status = excluded.status, message = excluded.message, \
             event_time = excluded.event_time, update_time = excluded.update_time",
            params![
                kind.as_str(),
                installed,
                latest_version,
                i64::from(update_available),
                source_url,
                status,
                message,
                now,
            ],
        )?;
        Ok(())
    });
}

/// 写入/更新某个应用的安装更新动作记录。与 check 共用该应用唯一的那一行：
/// 只覆盖动作相关字段，保留最近一次检查得到的 latest_version / target_version /
/// source_url / update_available（一次安装不改变「是否有新版本」的判定，等下次刷新再更新）。
pub fn record_action(
    kind: AppKind,
    action: &str,
    installed: Option<&str>,
    target_version: Option<&str>,
    status: &str,
    message: Option<&str>,
) {
    let now = db::now_ms();
    let _ = db::with_conn(|connection| {
        connection.execute(
            "INSERT INTO app_version_records (app_kind, action, installed_version, target_version, \
             update_available, status, message, event_time, created_time, update_time) \
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, ?7, ?7) \
             ON CONFLICT(app_kind) DO UPDATE SET \
             action = excluded.action, installed_version = excluded.installed_version, \
             status = excluded.status, message = excluded.message, \
             event_time = excluded.event_time, update_time = excluded.update_time",
            params![
                kind.as_str(),
                action,
                installed,
                target_version,
                status,
                message,
                now
            ],
        )?;
        Ok(())
    });
}

/// 每个应用最近一次的检查快照（一行 = 一个应用），用于重启后立刻显示徽标而不必等联网。
pub fn latest_checks() -> BTreeMap<AppKind, CheckSnapshot> {
    db::with_conn(|connection| {
        let mut statement = connection
            .prepare("SELECT app_kind, latest_version, update_available FROM app_version_records")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;

        let mut checks = BTreeMap::new();
        for row in rows {
            let (kind, latest_version, update_available) = row?;
            if let Some(app_kind) = AppKind::parse(&kind) {
                checks.insert(
                    app_kind,
                    CheckSnapshot {
                        latest_version,
                        update_available: update_available != 0,
                    },
                );
            }
        }
        Ok(checks)
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{is_newer, parse_latest, resolve_probe_url};

    const RELEASES_URL: &str = "https://downloads.claude.ai/releases/win32/x64/RELEASES";
    const MIRROR_URL: &str =
        "https://api.github.com/repos/Wangnov/claude-app-mirror/releases/latest";
    const WORKBUDDY_TEMPLATE: &str =
        "https://copilot.tencent.com/v2/update?platform=workbuddy-win32-x64-user&version={version}";
    const WORKBUDDY_URL: &str =
        "https://copilot.tencent.com/v2/update?platform=workbuddy-win32-x64-user&version=5.3.5";
    const ZCODE_URL: &str = "https://zcode.z.ai/cn/changelog";

    #[test]
    fn reads_newest_asset_from_squirrel_releases() {
        let body = "AAA111 AnthropicClaude-2.2449.0-full.nupkg 254988501\n\
                    BBB222 AnthropicClaude-2.7032.0-full.nupkg 254988501\n";
        assert_eq!(
            parse_latest(body, RELEASES_URL).as_deref(),
            Some("2.7032.0")
        );
    }

    #[test]
    fn reads_tag_from_github_release() {
        let body =
            r#"{ "tag_name": "claude-app-v2.7032.0", "name": "Claude App Mirror 2.7032.0" }"#;
        assert_eq!(parse_latest(body, MIRROR_URL).as_deref(), Some("2.7032.0"));
    }

    #[test]
    fn reads_latest_version_from_zcode_changelog() {
        // 更新日志页把最新版本排在最前：正文里第一条 `Release vX.Y.Z` 就是当前最新版本。
        // 前面故意放一个带点号的 CSS 哈希，证明这条 URL 走的是专用分支而不是通用兜底
        // （通用兜底只看首行，会从 `95d975...css` 里读出一段垃圾）。
        let body = "<!DOCTYPE html><html><link href=\"/_next/static/css/95d975bdaa94e289.css\">\
                    <h2 class=\"text-3xl\">Release v3.14.3</h2>\
                    <h2 class=\"text-3xl\">Release v3.14.1</h2>";
        assert_eq!(parse_latest(body, ZCODE_URL).as_deref(), Some("3.14.3"));
    }

    #[test]
    fn treats_plain_body_as_bare_version() {
        assert_eq!(
            parse_latest(" 3.1.4 \n", "https://example.com/version").as_deref(),
            Some("3.1.4")
        );
        assert_eq!(parse_latest("   \n", "https://example.com/version"), None);
    }

    #[test]
    fn reads_target_version_from_workbuddy_update_feed() {
        // 实测形状：目标版本在 `productVersion`，`version` 同值，另有安装包直链。
        let body = r#"{"version":"5.6.2.39298511","url":"https://download.codebuddy.cn/workbuddy/saas/win32-x64-user/WorkBuddy-win32-x64-user-5.6.2.39298511-37a65c0b.exe","productVersion":"5.6.2.39298511","sha256hash":"","timestamp":1790021711}"#;
        assert_eq!(
            parse_latest(body, WORKBUDDY_URL).as_deref(),
            Some("5.6.2.39298511")
        );
    }

    #[test]
    fn empty_workbuddy_body_means_no_target_version() {
        // 已是该目标版本时 feed 返回 204（空体）→ 读不出新版本，而不是解析出错。
        assert_eq!(parse_latest("", WORKBUDDY_URL), None);
    }

    #[test]
    fn probe_url_substitutes_version_only_when_installed_is_known() {
        assert_eq!(
            resolve_probe_url(WORKBUDDY_TEMPLATE, Some("5.3.5")).as_deref(),
            Some(WORKBUDDY_URL)
        );
        // 没有已安装版本 → 跳过这条源，而不是发一个还带占位符的地址。
        assert_eq!(resolve_probe_url(WORKBUDDY_TEMPLATE, None), None);
        // 不含占位符的源原样返回，不受已安装版本有无的影响。
        assert_eq!(
            resolve_probe_url(RELEASES_URL, None).as_deref(),
            Some(RELEASES_URL)
        );
        assert_eq!(
            resolve_probe_url(MIRROR_URL, Some("1.0.0")).as_deref(),
            Some(MIRROR_URL)
        );
    }

    #[test]
    fn compares_versions_by_padded_numeric_segments() {
        assert!(is_newer("2.7032.0", "2.2553.1.0"));
        assert!(is_newer("2.2553.2", "2.2553.1"));
        assert!(!is_newer("2.2553.1.0", "2.2553.1.0"));
        assert!(!is_newer("2.1000.0", "2.2553.1.0"));
        // 尾随的 0 段不算更新：RELEASES 给 2.7032.0，MSIX 已安装版常是 2.7032.0.0
        assert!(!is_newer("2.7032.0", "2.7032.0.0"));
    }
}

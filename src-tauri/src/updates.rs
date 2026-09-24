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
pub async fn latest_version(kind: AppKind) -> Option<FoundVersion> {
    for url in catalog::builtin_app(kind).latest_version_urls {
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
    extract_version(body.trim().lines().next()?)
}

/// Pulls the first dotted numeric token out of arbitrary text,
/// e.g. `AnthropicClaude-2.7032.0-full.nupkg` or `claude-app-v2.7032.0` -> `2.7032.0`.
fn extract_version(text: &str) -> Option<String> {
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
             VALUES (?1, 'check', ?2, ?3, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?8)",
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
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, ?7, ?7)",
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

/// 最近一次 check 结果（按应用），用于重启后立刻显示徽标而不必等联网。
pub fn latest_checks() -> BTreeMap<AppKind, CheckSnapshot> {
    db::with_conn(|connection| {
        let mut statement = connection.prepare(
            "SELECT r.app_kind, r.latest_version, r.update_available \
             FROM app_version_records r \
             JOIN (SELECT app_kind, MAX(event_time) AS newest FROM app_version_records \
                   WHERE action = 'check' GROUP BY app_kind) m \
               ON r.app_kind = m.app_kind AND r.event_time = m.newest \
             WHERE r.action = 'check'",
        )?;
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
    use super::{is_newer, parse_latest};

    const RELEASES_URL: &str = "https://downloads.claude.ai/releases/win32/x64/RELEASES";
    const MIRROR_URL: &str = "https://api.github.com/repos/Wangnov/claude-app-mirror/releases/latest";

    #[test]
    fn reads_newest_asset_from_squirrel_releases() {
        let body = "AAA111 AnthropicClaude-2.2449.0-full.nupkg 254988501\n\
                    BBB222 AnthropicClaude-2.7032.0-full.nupkg 254988501\n";
        assert_eq!(parse_latest(body, RELEASES_URL).as_deref(), Some("2.7032.0"));
    }

    #[test]
    fn reads_tag_from_github_release() {
        let body = r#"{ "tag_name": "claude-app-v2.7032.0", "name": "Claude App Mirror 2.7032.0" }"#;
        assert_eq!(parse_latest(body, MIRROR_URL).as_deref(), Some("2.7032.0"));
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
    fn compares_versions_by_padded_numeric_segments() {
        assert!(is_newer("2.7032.0", "2.2553.1.0"));
        assert!(is_newer("2.2553.2", "2.2553.1"));
        assert!(!is_newer("2.2553.1.0", "2.2553.1.0"));
        assert!(!is_newer("2.1000.0", "2.2553.1.0"));
    }
}

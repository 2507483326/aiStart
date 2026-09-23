use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};

const MAX_RECORDS: usize = 20_000;
const TRIM_TO: usize = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecord {
    pub timestamp: String,
    pub date: String,
    pub model_name: String,
    pub served_by: String,
    pub inbound_protocol: String,
    pub upstream_protocol: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub duration_ms: u64,
    pub ok: bool,
    pub failover: bool,
    #[serde(default)]
    pub error: Option<String>,
}

impl UsageRecord {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyUsage {
    pub date: String,
    pub requests: u64,
    pub failed: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub model_name: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub total_requests: u64,
    pub failed_requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub today_tokens: u64,
    pub streak_days: u64,
    pub daily: Vec<DailyUsage>,
    pub by_model: Vec<ModelUsage>,
}

fn file_path() -> Option<PathBuf> {
    crate::settings::data_dir().map(|dir| dir.join("usage.jsonl"))
}

static COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn record(entry: &UsageRecord) {
    let Some(path) = file_path() else {
        return;
    };
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    if let Ok(line) = serde_json::to_string(entry) {
        if writeln!(file, "{line}").is_ok() {
            let count = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
            if count > MAX_RECORDS {
                drop(file);
                trim(&path);
            }
        }
    }
}

fn trim(path: &PathBuf) {
    let records = read_all();
    let keep = records.len().saturating_sub(TRIM_TO);
    let tail = records.into_iter().skip(keep);
    let mut buffer = String::new();
    for entry in tail {
        if let Ok(line) = serde_json::to_string(&entry) {
            buffer.push_str(&line);
            buffer.push('\n');
        }
    }
    if std::fs::write(path, buffer).is_ok() {
        COUNT.store(TRIM_TO, Ordering::Relaxed);
    }
}

fn read_all() -> Vec<UsageRecord> {
    let Some(path) = file_path() else {
        return Vec::new();
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<UsageRecord>(line).ok())
        .collect()
}

pub fn recent(limit: usize) -> Vec<UsageRecord> {
    let mut records = read_all();
    records.reverse();
    records.truncate(limit);
    records
}

pub fn summary(days: u32) -> UsageSummary {
    let records = read_all();
    let today = chrono::Local::now().date_naive();
    let cutoff = today - chrono::Duration::days(i64::from(days.saturating_sub(1)));

    let mut daily: BTreeMap<String, DailyUsage> = BTreeMap::new();
    let mut by_model: BTreeMap<String, ModelUsage> = BTreeMap::new();
    let mut summary = UsageSummary {
        total_requests: 0,
        failed_requests: 0,
        input_tokens: 0,
        output_tokens: 0,
        total_tokens: 0,
        today_tokens: 0,
        streak_days: 0,
        daily: Vec::new(),
        by_model: Vec::new(),
    };

    for entry in records {
        let Ok(date) = chrono::NaiveDate::parse_from_str(&entry.date, "%Y-%m-%d") else {
            continue;
        };
        if date < cutoff {
            continue;
        }

        summary.total_requests += 1;
        if !entry.ok {
            summary.failed_requests += 1;
        }
        summary.input_tokens += entry.input_tokens;
        summary.output_tokens += entry.output_tokens;
        if date == today {
            summary.today_tokens += entry.total_tokens();
        }

        let bucket = daily.entry(entry.date.clone()).or_insert_with(|| DailyUsage {
            date: entry.date.clone(),
            requests: 0,
            failed: 0,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
        });
        bucket.requests += 1;
        if !entry.ok {
            bucket.failed += 1;
        }
        bucket.input_tokens += entry.input_tokens;
        bucket.output_tokens += entry.output_tokens;
        bucket.total_tokens += entry.total_tokens();

        let model = by_model
            .entry(entry.served_by.clone())
            .or_insert_with(|| ModelUsage {
                model_name: entry.served_by.clone(),
                requests: 0,
                input_tokens: 0,
                output_tokens: 0,
            });
        model.requests += 1;
        model.input_tokens += entry.input_tokens;
        model.output_tokens += entry.output_tokens;
    }

    summary.total_tokens = summary.input_tokens + summary.output_tokens;
    summary.daily = daily.into_values().collect();
    summary.by_model = by_model.into_values().collect();
    summary.by_model.sort_by(|a, b| b.requests.cmp(&a.requests));

    let active: std::collections::BTreeSet<&str> = summary
        .daily
        .iter()
        .filter(|day| day.requests > 0)
        .map(|day| day.date.as_str())
        .collect();
    let mut streak = 0;
    let mut cursor = today;
    while active.contains(cursor.format("%Y-%m-%d").to_string().as_str()) {
        streak += 1;
        cursor -= chrono::Duration::days(1);
    }
    summary.streak_days = streak;

    summary
}

pub fn current_timestamp() -> (String, String) {
    let now = chrono::Local::now();
    (
        now.to_rfc3339(),
        now.format("%Y-%m-%d").to_string(),
    )
}

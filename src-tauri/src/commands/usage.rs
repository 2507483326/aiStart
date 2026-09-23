use crate::error::AppResult;
use crate::usage::{self, UsageRecord, UsageSummary};

#[tauri::command]
pub fn usage_summary(days: u32) -> UsageSummary {
    usage::summary(days)
}

#[tauri::command]
pub fn usage_records(limit: usize) -> AppResult<Vec<UsageRecord>> {
    Ok(usage::recent(limit.clamp(1, 2000)))
}

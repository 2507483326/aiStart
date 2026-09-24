use crate::error::AppResult;
use crate::usage::{self, RequestDetail, UsagePage, UsageRecord, UsageSummary};

#[tauri::command]
pub fn usage_summary(days: u32) -> UsageSummary {
    usage::summary(days)
}

#[tauri::command]
pub fn usage_records(limit: usize) -> AppResult<Vec<UsageRecord>> {
    Ok(usage::recent(limit.clamp(1, 2000)))
}

#[tauri::command]
pub fn usage_page(offset: usize, limit: usize) -> AppResult<UsagePage> {
    Ok(usage::page(offset, limit.clamp(1, 200)))
}

#[tauri::command]
pub fn usage_detail(id: i64) -> AppResult<Option<RequestDetail>> {
    Ok(usage::find(id).map(|record| RequestDetail {
        payload: usage::payload_detail(id),
        record,
    }))
}

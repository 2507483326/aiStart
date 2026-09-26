use crate::error::AppResult;
use crate::usage::{self, DailyUsage, RequestDetail, UsagePage, UsageRecord, UsageTotals};

#[tauri::command]
pub fn usage_daily(days: u32) -> Vec<DailyUsage> {
    usage::daily(days)
}

#[tauri::command]
pub fn usage_today() -> DailyUsage {
    usage::today()
}

#[tauri::command]
pub fn usage_total() -> UsageTotals {
    usage::totals()
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

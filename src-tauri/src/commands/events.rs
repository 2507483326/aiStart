use crate::error::AppResult;
use crate::events::{self, EventView};

#[tauri::command]
pub fn list_events(limit: Option<usize>) -> AppResult<Vec<EventView>> {
    events::list(limit.unwrap_or(200).clamp(1, 2000))
}

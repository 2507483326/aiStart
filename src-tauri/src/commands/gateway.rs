use crate::error::AppResult;
use crate::gateway::{self, GatewayStatus};

#[tauri::command]
pub fn gateway_status() -> GatewayStatus {
    gateway::status()
}

#[tauri::command]
pub fn restart_gateway() -> AppResult<GatewayStatus> {
    gateway::restart()
}

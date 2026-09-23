use crate::error::AppResult;
use crate::gateway::{self, GatewayStatus};

#[tauri::command]
pub fn gateway_status() -> GatewayStatus {
    gateway::status()
}

#[tauri::command]
pub fn start_gateway() -> AppResult<GatewayStatus> {
    gateway::start()
}

#[tauri::command]
pub fn stop_gateway() -> AppResult<GatewayStatus> {
    gateway::stop()
}

#[tauri::command]
pub fn restart_gateway() -> AppResult<GatewayStatus> {
    gateway::restart()
}

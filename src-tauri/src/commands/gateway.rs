use crate::error::AppResult;
use crate::gateway::{self, GatewayStatus};

#[tauri::command]
pub fn gateway_status() -> GatewayStatus {
    gateway::status()
}

/// 停 + 起最长会阻塞几秒，所以走 async 命令交给运行时，UI 才不会在重启期间卡住，
/// 也才能实时收到 `gateway://state` 事件看到「停止中 → 已停止 → 启动中 → 运行中」。
#[tauri::command]
pub async fn restart_gateway() -> AppResult<GatewayStatus> {
    gateway::restart_async().await
}

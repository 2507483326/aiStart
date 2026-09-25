//! 事后切换：请求结束后由后台连接测试驱动的模型切换。
//!
//! 目标流程里一次请求只打一个上游、失败立即返回客户端；本模块在请求落库之后
//! 异步做「连接测试 → 第一个通过的就任当前模型」，服务的是**下一次**请求。
//! 多个请求同时失败只跑一轮探测：进程级哨兵保证同一时刻最多一轮，后来的触发
//! 直接合并进正在跑的那轮（见 `start_probe`）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::json;

use crate::domain::model::ModelConfig;
use crate::events;
use crate::settings;

use super::GatewayStats;

/// 探测进行中的进程级哨兵：并发失败的请求只跑一轮探测，后来的触发合并。
/// 网关每次启动时重置一次，兜底「探测任务随旧运行时被丢弃」的场景。
static PROBING: AtomicBool = AtomicBool::new(false);

/// 一次失败触发的切换上下文：刚失败的模型。探测时跳过它；切换前用它复查
/// 「当前模型没有被用户手动换掉」。
#[derive(Debug, Clone)]
pub struct FailoverContext {
    pub failed_model_id: i64,
    pub failed_model_name: String,
}

/// 触发条件判定表（纯函数，docs/forwarding.md §8 批 1c）：
/// 自动切换开启、本次未点名模型、且失败形态是换模型可能救得了的
/// （5xx / 401 / 403 / 404 / 408 / 429 / 网络错误——即 `dispatch` 判定的 retryable）。
pub fn qualifies(auto_failover: bool, named: bool, retryable: bool) -> bool {
    auto_failover && !named && retryable
}

/// 探测顺序（纯函数）：模型列表原顺序，跳过刚失败的模型。
pub fn probe_order(models: &[ModelConfig], failed_model_id: i64) -> Vec<i64> {
    models
        .iter()
        .map(|model| model.id)
        .filter(|id| *id != failed_model_id)
        .collect()
}

/// 允许切换的前提（纯函数）：当前模型仍是刚失败的那个——探测期间用户手动
/// 切过模型就不覆盖。
pub fn may_switch(current_active: Option<i64>, failed_model_id: i64) -> bool {
    current_active == Some(failed_model_id)
}

/// 网关启动时重置哨兵：上一轮探测任务可能随旧运行时一起被丢弃，标志不能永久留在 true。
pub fn reset() {
    PROBING.store(false, Ordering::Release);
}

/// 请求失败落库后的入口（在网关运行时内调用，内部 `tokio::spawn` 后台探测）。
/// 哨兵已被占用时直接返回：本次触发合并进正在跑的那轮。
pub(super) fn start_probe(stats: Arc<GatewayStats>, context: FailoverContext) {
    if PROBING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    tokio::spawn(async move {
        struct ProbeGuard;
        impl Drop for ProbeGuard {
            fn drop(&mut self) {
                PROBING.store(false, Ordering::Release);
            }
        }
        // 无论探测怎么退出（包括网关停止时任务被 drop）都释放哨兵。
        let _guard = ProbeGuard;
        run(&stats, &context).await;
    });
}

/// 一轮探测：按模型列表顺序逐个发最小补全请求（跳过刚失败的模型），第一个
/// 通过的就任当前模型；全部不通过则原地不动，等下一次请求失败再触发。
async fn run(stats: &GatewayStats, context: &FailoverContext) {
    let models = settings::snapshot().models;
    for id in probe_order(&models, context.failed_model_id) {
        let Some(model) = models.iter().find(|model| model.id == id) else {
            continue;
        };
        // probe_completion 自带 15s 硬超时；构造请求失败等 Err 一律视同不通过。
        let passed = crate::commands::models::probe_completion(model)
            .await
            .map(|result| result.ok)
            .unwrap_or(false);
        if !passed {
            continue;
        }

        // 切换前复查：探测期间用户手动换过模型就放弃，别覆盖用户的选择。
        if !may_switch(settings::snapshot().active_model_id, context.failed_model_id) {
            events::log(
                "system",
                Some("网关"),
                "model.failover.skipped",
                Some("model"),
                Some(&context.failed_model_id.to_string()),
                Some(json!({
                    "reason": "当前模型已被手动切换，放弃自动切换",
                    "probePassed": &model.name,
                })),
            );
            return;
        }

        match settings::mutate(|settings| settings.active_model_id = Some(model.id)) {
            Ok(()) => {
                events::log(
                    "system",
                    Some("网关"),
                    "model.failover",
                    Some("model"),
                    Some(&model.id.to_string()),
                    Some(json!({ "from": &context.failed_model_name, "to": &model.name })),
                );
                stats.record_failover(&context.failed_model_name, &model.name);
                super::publish();
            }
            Err(error) => {
                stats.record_error(&format!("自动切换写入当前模型失败: {error}"));
            }
        }
        return;
    }

    events::log(
        "system",
        Some("网关"),
        "model.failover.exhausted",
        Some("model"),
        Some(&context.failed_model_id.to_string()),
        Some(json!({
            "failed": &context.failed_model_name,
            "probed": probe_order(&models, context.failed_model_id).len(),
        })),
    );
}

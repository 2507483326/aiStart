pub mod server;
pub mod sse;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::sync::oneshot;

use crate::error::{AppError, AppResult};
use crate::events;

/// 收到停止信号后留给在途请求的收尾时间；超过就直接结束服务，确保监听端口一定被释放。
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// `stop()` 等服务线程退出的上限。正常情况下就是 `SHUTDOWN_GRACE` 加建运行时/绑端口的
/// 时间，这里只是兜底，避免万一卡住时把调用方（Tauri 命令）永久挂住。
const STOP_WAIT: Duration = Duration::from_secs(10);

/// Brand shown in the Claude Desktop model picker.
pub const MODEL_LABEL_PREFIX: &str = "aiStart";

/// One route the gateway advertises and Claude Desktop accepts.
#[derive(Debug, Clone, Copy)]
pub struct ModelRole {
    /// Advertised on `/v1/models` and written into `inferenceModels[].name`.
    /// Claude Desktop drops any entry whose name is not recognisably an
    /// Anthropic model route, so this has to stay Claude-shaped rather than an
    /// opaque alias.
    pub id: &'static str,
    /// Appended to [`MODEL_LABEL_PREFIX`] to build the picker's `labelOverride`.
    pub suffix: &'static str,
}

impl ModelRole {
    pub fn picker_label(&self) -> String {
        format!("{MODEL_LABEL_PREFIX} · {}", self.suffix)
    }
}

/// One route per tier, so bare aliases (e.g. `sonnet` in Code sessions) resolve.
/// The first entry is Claude's default model. The real upstream model is
/// substituted on the way out, so the picker label comes from `labelOverride`.
pub const MODEL_ROLES: [ModelRole; 4] = [
    ModelRole {
        id: "claude-sonnet-5",
        suffix: "Sonnet",
    },
    ModelRole {
        id: "claude-opus-5",
        suffix: "Opus",
    },
    ModelRole {
        id: "claude-haiku-4-5",
        suffix: "Haiku",
    },
    ModelRole {
        id: "claude-fable-5",
        suffix: "Fable",
    },
];

#[derive(Debug, Default)]
pub struct GatewayStats {
    pub requests: AtomicU64,
    pub errors: AtomicU64,
    pub input_tokens: AtomicU64,
    pub output_tokens: AtomicU64,
    pub failovers: AtomicU64,
    pub last_error: RwLock<Option<String>>,
    pub last_failover: RwLock<Option<String>>,
}

impl GatewayStats {
    pub fn record_error(&self, message: &str) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut guard) = self.last_error.write() {
            *guard = Some(message.to_string());
        }
    }

    pub fn record_failover(&self, from: &str, to: &str) {
        self.failovers.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut guard) = self.last_failover.write() {
            *guard = Some(format!("{from} → {to}"));
        }
    }

    pub fn record_tokens(&self, input: u64, output: u64) {
        self.input_tokens.fetch_add(input, Ordering::Relaxed);
        self.output_tokens.fetch_add(output, Ordering::Relaxed);
    }

    pub fn last_failover(&self) -> Option<String> {
        self.last_failover
            .read()
            .ok()
            .and_then(|value| value.clone())
    }

    pub fn snapshot(&self) -> (u64, u64, u64, u64, u64, Option<String>) {
        (
            self.requests.load(Ordering::Relaxed),
            self.errors.load(Ordering::Relaxed),
            self.input_tokens.load(Ordering::Relaxed),
            self.output_tokens.load(Ordering::Relaxed),
            self.failovers.load(Ordering::Relaxed),
            self.last_error.read().ok().and_then(|value| value.clone()),
        )
    }
}

static STATS: OnceLock<Arc<GatewayStats>> = OnceLock::new();

pub fn stats() -> Arc<GatewayStats> {
    STATS
        .get_or_init(|| Arc::new(GatewayStats::default()))
        .clone()
}

/// 用数据库中的全量累计值初始化计数器，使面板数据跨重启保留。
/// 启动时调用一次即可；此后进程内的自增会继续叠加历史值。
pub fn hydrate() {
    let totals = crate::usage::totals();
    let stats = stats();
    stats.requests.store(totals.requests, Ordering::Relaxed);
    stats.errors.store(totals.failed, Ordering::Relaxed);
    stats
        .input_tokens
        .store(totals.input_tokens, Ordering::Relaxed);
    stats
        .output_tokens
        .store(totals.output_tokens, Ordering::Relaxed);
    stats.failovers.store(totals.failovers, Ordering::Relaxed);
}

/// 网关生命周期。`Starting` / `Stopping` 是真实会停留的状态：停止要等 axum
/// 真正退出（监听端口被释放）才会落到 `Stopped`，前端据此展示过程变化。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GatewayState {
    #[default]
    Stopped,
    Starting,
    Running,
    Stopping,
}

static LIFECYCLE: OnceLock<RwLock<GatewayState>> = OnceLock::new();
static APP: OnceLock<AppHandle> = OnceLock::new();
static OPERATION: OnceLock<Mutex<()>> = OnceLock::new();

fn lifecycle() -> &'static RwLock<GatewayState> {
    LIFECYCLE.get_or_init(|| RwLock::new(GatewayState::default()))
}

fn operation_lock() -> &'static Mutex<()> {
    OPERATION.get_or_init(|| Mutex::new(()))
}

/// 保存 AppHandle：状态一变就推事件给前端，5 秒轮询只是兜底。
pub fn attach(app: AppHandle) {
    let _ = APP.set(app);
}

pub fn state() -> GatewayState {
    *lifecycle().read().expect("gateway lifecycle poisoned")
}

fn publish() {
    if let Some(app) = APP.get() {
        let _ = app.emit("gateway://state", status());
    }
}

fn set_state(next: GatewayState) {
    let changed = {
        let mut guard = lifecycle().write().expect("gateway lifecycle poisoned");
        if *guard == next {
            false
        } else {
            *guard = next;
            true
        }
    };
    if changed {
        publish();
    }
}

struct RunningGateway {
    port: u16,
    shutdown: oneshot::Sender<()>,
    /// 服务线程退出后发送。`stop()` 靠它确认端口已释放，重启时才不会抢端口。
    done: std::sync::mpsc::Receiver<()>,
}

static RUNNING: OnceLock<Mutex<Option<RunningGateway>>> = OnceLock::new();

fn running() -> &'static Mutex<Option<RunningGateway>> {
    RUNNING.get_or_init(|| Mutex::new(None))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    pub running: bool,
    pub state: GatewayState,
    pub port: u16,
    pub base_url: String,
    pub requests: u64,
    pub errors: u64,
    pub failovers: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub last_error: Option<String>,
    pub last_failover: Option<String>,
    pub auto_failover: bool,
    pub active_model_name: Option<String>,
    pub active_model_format: Option<String>,
    pub active_model_id: Option<i64>,
}

pub fn base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

pub fn status() -> GatewayStatus {
    let settings = crate::settings::snapshot();
    let (requests, errors, input_tokens, output_tokens, failovers, last_error) = stats().snapshot();
    // 先读生命周期、再读 RUNNING，两把锁不嵌套，避免与 start() 的加锁顺序互相等待。
    let state = state();
    let running_port = running()
        .lock()
        .expect("gateway lock poisoned")
        .as_ref()
        .map(|gateway| gateway.port);
    let port = running_port.unwrap_or(settings.gateway_port);
    let active = settings.active_model();

    GatewayStatus {
        running: running_port.is_some(),
        state,
        port,
        base_url: base_url(port),
        requests,
        errors,
        failovers,
        input_tokens,
        output_tokens,
        last_error,
        last_failover: stats().last_failover(),
        auto_failover: settings.auto_failover,
        active_model_name: active.map(|model| model.name.clone()),
        active_model_format: active.map(|model| model.format.as_str().to_string()),
        active_model_id: active.map(|model| model.id),
    }
}

pub fn start() -> AppResult<GatewayStatus> {
    {
        let guard = running().lock().expect("gateway lock poisoned");
        if guard.is_some() {
            drop(guard);
            return Ok(status());
        }
    }
    if state() == GatewayState::Starting {
        return Err(AppError::Message("网关正在启动中，请稍候再试".into()));
    }

    let settings = crate::settings::snapshot();
    if settings.active_model().is_none() {
        return Err(AppError::NotFound("请先添加并启用一个模型".into()));
    }
    let port = settings.gateway_port;
    let (shutdown_tx, shutdown_rx) = sse::shutdown_channel();

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();

    set_state(GatewayState::Starting);

    let spawned = std::thread::Builder::new()
        .name("ai-start-gateway".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready_tx.send(Err(format!("创建运行时失败: {error}")));
                    return;
                }
            };
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
                    Ok(listener) => {
                        let _ = ready_tx.send(Ok(()));
                        listener
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(format!("端口 {port} 绑定失败: {error}")));
                        return;
                    }
                };
                let app = server::router();
                let (hard_tx, hard_rx) = tokio::sync::oneshot::channel::<()>();
                // 注意：axum 以「这个 future 完成」作为开始优雅关闭的信号，所以它必须
                // 在收到停止信号后立刻返回；宽限期交给独立的定时任务去触发硬关闭。
                let shutdown = async move {
                    let _ = shutdown_rx.await;
                    tokio::spawn(async move {
                        tokio::time::sleep(SHUTDOWN_GRACE).await;
                        let _ = hard_tx.send(());
                    });
                };
                let graceful = axum::serve(listener, app).with_graceful_shutdown(shutdown);
                // 正常情况 graceful 先完成（在途请求跑完）；超过宽限期则由 hard_rx 结束
                // future，运行时随之丢弃、端口随之释放。
                tokio::select! {
                    _ = graceful => {}
                    _ = hard_rx => {}
                }
            });
            let _ = done_tx.send(());
        });

    if let Err(error) = spawned {
        fail_start();
        return Err(AppError::Message(format!("启动网关线程失败: {error}")));
    }

    match ready_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(message)) => {
            fail_start();
            return Err(AppError::Message(message));
        }
        Err(_) => {
            fail_start();
            return Err(AppError::Message("网关启动失败".into()));
        }
    }

    let mut guard = running().lock().expect("gateway lock poisoned");
    *guard = Some(RunningGateway {
        port,
        shutdown: shutdown_tx,
        done: done_rx,
    });
    drop(guard);

    set_state(GatewayState::Running);

    events::log(
        "system",
        Some("网关"),
        "gateway.started",
        Some("gateway"),
        Some(&port.to_string()),
        None,
    );

    Ok(status())
}

/// 启动失败时把状态退回 `Stopped`。只在还停留在 `Starting` 时才退，
/// 免得覆盖另一次已经成功的启动写下的 `Running`。
fn fail_start() {
    let reverted = {
        let mut guard = lifecycle().write().expect("gateway lifecycle poisoned");
        if *guard == GatewayState::Starting {
            *guard = GatewayState::Stopped;
            true
        } else {
            false
        }
    };
    if reverted {
        publish();
    }
}

pub fn stop() -> AppResult<GatewayStatus> {
    let taken = {
        let mut guard = running().lock().expect("gateway lock poisoned");
        guard.take()
    };

    if let Some(RunningGateway {
        port,
        shutdown,
        done,
    }) = taken
    {
        set_state(GatewayState::Stopping);
        let _ = shutdown.send(());
        // 等 axum 真正结束（连带释放监听端口）再返回：上一个监听还占着端口就开始下一次，
        // bind 会报「端口绑定失败」，那正是「重启像没重启」的根源。
        let _ = done.recv_timeout(STOP_WAIT);
        set_state(GatewayState::Stopped);
        events::log(
            "system",
            Some("网关"),
            "gateway.stopped",
            Some("gateway"),
            Some(&port.to_string()),
            None,
        );
    }

    Ok(status())
}

pub fn ensure_running() -> AppResult<GatewayStatus> {
    {
        let guard = running().lock().expect("gateway lock poisoned");
        if guard.is_some() {
            drop(guard);
            return Ok(status());
        }
    }
    start()
}

pub fn restart() -> AppResult<GatewayStatus> {
    // 连点重启时把「停 + 起」串起来，避免两次重启交错。
    let _serialized = operation_lock()
        .lock()
        .expect("gateway operation lock poisoned");
    stop()?;
    start()
}

/// 供 Tauri 命令使用：`stop()` 最长等 5 秒，扔到阻塞线程池里，别卡住 UI。
pub async fn restart_async() -> AppResult<GatewayStatus> {
    tokio::task::spawn_blocking(restart)
        .await
        .map_err(|error| AppError::Message(format!("重启网关失败: {error}")))?
}

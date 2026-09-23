pub mod server;
pub mod sse;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::error::{AppError, AppResult};

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
    STATS.get_or_init(|| Arc::new(GatewayStats::default())).clone()
}

struct RunningGateway {
    port: u16,
    shutdown: oneshot::Sender<()>,
}

static RUNNING: OnceLock<Mutex<Option<RunningGateway>>> = OnceLock::new();

fn running() -> &'static Mutex<Option<RunningGateway>> {
    RUNNING.get_or_init(|| Mutex::new(None))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    pub running: bool,
    pub port: u16,
    pub base_url: String,
    pub token: String,
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
}

pub fn base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

pub fn status() -> GatewayStatus {
    let settings = crate::settings::snapshot();
    let (requests, errors, input_tokens, output_tokens, failovers, last_error) = stats().snapshot();
    let guard = running().lock().expect("gateway lock poisoned");
    let running_port = guard.as_ref().map(|gateway| gateway.port);
    let active = settings.active_model();

    GatewayStatus {
        running: running_port.is_some(),
        port: running_port.unwrap_or(settings.gateway_port),
        base_url: base_url(running_port.unwrap_or(settings.gateway_port)),
        token: settings.gateway_token.clone(),
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

    let settings = crate::settings::snapshot();
    if settings.active_model().is_none() {
        return Err(AppError::NotFound("请先添加并启用一个模型".into()));
    }
    let port = settings.gateway_port;
    let (shutdown_tx, shutdown_rx) = sse::shutdown_channel();

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    std::thread::Builder::new()
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
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = shutdown_rx.await;
                    })
                    .await;
            });
        })
        .map_err(|error| AppError::Message(format!("启动网关线程失败: {error}")))?;

    match ready_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(message)) => return Err(AppError::Message(message)),
        Err(_) => return Err(AppError::Message("网关启动失败".into())),
    }

    let mut guard = running().lock().expect("gateway lock poisoned");
    *guard = Some(RunningGateway {
        port,
        shutdown: shutdown_tx,
    });
    drop(guard);

    Ok(status())
}

pub fn stop() -> AppResult<GatewayStatus> {
    let taken = {
        let mut guard = running().lock().expect("gateway lock poisoned");
        guard.take()
    };
    if let Some(gateway) = taken {
        let _ = gateway.shutdown.send(());
    }
    Ok(status())
}

pub fn ensure_running() -> AppResult<GatewayStatus> {
    if status().running {
        Ok(status())
    } else {
        start()
    }
}

pub fn restart() -> AppResult<GatewayStatus> {
    stop()?;
    std::thread::sleep(std::time::Duration::from_millis(150));
    start()
}

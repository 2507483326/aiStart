use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tower_http::cors::CorsLayer;

use crate::domain::canonical::CanonicalRequest;
use crate::domain::model::{ModelConfig, ModelFormat};
use crate::error::{AppError, AppResult};
use crate::providers::{
    http_client, provider_for, request_proxies_through, ResponseAssembler, SseEvent, StreamState,
    WireState,
};

use super::sse::{encode_channel_event, first_frame_timeout, parse_sse_stream};
use super::{GatewayStats, MODEL_ROLES};

/// 转发链路的超时（B1）：主链路此前只有 `connect_timeout(20s)`，上游建连后不响应会永久挂起
/// （客户端干等、明细永不落库）。非流式用它限整个请求；流式不设总超时——长生成合法，
/// 只限「等响应头」与「等首帧」两程（首帧见 `first_frame_timeout`）。
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(300);

/// 入站请求体的逻辑上限（H2）：超过它就进 handler 自检，按**入站协议的形状**回 413 并落一条
/// 失败明细。取 32MB 是因为「几张 base64 图片 + 长提示词」也到不了——A4 支持 `data:` URL 图片，
/// 而 axum 的默认上限 2MB 连一张 1.5MB 的图（base64 后约 2MB）都放不下，超限时更是连明细都不落。
const MAX_INBOUND_BODY: usize = 32 * MEGABYTE;

/// axum 那一层的硬上限（`DefaultBodyLimit`）。比逻辑上限高出一截，好处是「略微超限」的请求
/// 能进到 handler：拿到协议形状的 413，并留下一条可查的失败明细。只有真离谱的（超过这里）
/// 才由 axum 在 handler 之前用纯文本 413 挡掉——那种请求不值得为它准备一份 JSON 形状和一条库记录。
const INBOUND_BODY_HARD_LIMIT: usize = 64 * MEGABYTE;

const MEGABYTE: usize = 1024 * 1024;

pub fn router() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/messages", post(messages))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses))
        // 硬上限写在 CORS 之前（后加的层在外层），这样硬上限自己回的 413 也带着 CORS 头：
        // 浏览器里看到的是「请求太大」，而不是一个伪装成跨域问题的 413。
        .layer(DefaultBodyLimit::max(INBOUND_BODY_HARD_LIMIT))
        .layer(CorsLayer::permissive())
        .with_state(super::stats())
}

fn truncate(input: &str, limit: usize) -> String {
    if input.chars().count() <= limit {
        return input.to_string();
    }
    let head: String = input.chars().take(limit).collect();
    format!("{head}…")
}

fn json_response(status: StatusCode, body: Value) -> Response {
    (status, axum::Json(body)).into_response()
}

fn api_error(inbound: ModelFormat, status: StatusCode, kind: &str, message: &str) -> Response {
    let body = match inbound {
        ModelFormat::AnthropicMessages => {
            json!({ "type": "error", "error": { "type": kind, "message": message } })
        }
        _ => json!({ "error": { "message": message, "type": kind, "code": kind } }),
    };
    json_response(status, body)
}

fn extract_token(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
    {
        return Some(value.to_string());
    }
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim_start_matches("Bearer ").trim().to_string())
}

/// 把请求携带的 token 匹配回来源应用；匹配不到时原样返回该 token（空则返回空串）。
fn source_app_for(token: &str) -> String {
    crate::settings::snapshot()
        .app_for_token(token)
        .map(|kind| kind.as_str().to_string())
        .unwrap_or_else(|| token.to_string())
}

/// 入站 HTTP header 序列化成 `{ "名": "值" }`（原样保存不脱敏，决策见 docs/forwarding.md §2）。
/// 同名多值用逗号合并；非 ASCII 值按 lossy 转换保留，不静默丢 header。
fn serialize_headers(headers: &HeaderMap) -> String {
    let mut map = serde_json::Map::new();
    for (name, value) in headers {
        let key = name.as_str().to_string();
        let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
        match map.entry(key) {
            serde_json::map::Entry::Occupied(mut slot) => {
                let existing = slot.get_mut().as_str().unwrap_or_default().to_string();
                slot.insert(json!(format!("{existing}, {value}")));
            }
            serde_json::map::Entry::Vacant(slot) => {
                slot.insert(json!(value));
            }
        }
    }
    json!(map).to_string()
}

fn encode_event(event: &SseEvent) -> String {
    let data = match &event.raw {
        Some(raw) => raw.clone(),
        None => serde_json::to_string(&event.data).unwrap_or_else(|_| "{}".into()),
    };
    encode_channel_event(&event.event, &data)
}

/// 网关自产的流内错误（断流、解帧失败、首帧超时）→ 按入站协议出形状。
/// 形状定义在 `wire::stream_error_event`（A7）：上游流内错误走的是同一份，
/// 客户端不会因为错误来自哪一侧而收到两种形状。
fn error_events(inbound: ModelFormat, message: &str) -> Vec<SseEvent> {
    vec![crate::providers::wire::stream_error_event(
        inbound,
        "api_error",
        message,
    )]
}

fn sse_response(
    stream: impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| {
            api_error(
                ModelFormat::AnthropicMessages,
                StatusCode::INTERNAL_SERVER_ERROR,
                "api_error",
                "构建流式响应失败",
            )
        })
}

/// 构造一条明细记录（不含报文）。落库与非流式/流式两条路径共用；流式的断连兜底（B3）
/// 也要用它——把「决定记什么」和「写库」分开，判定部分才能不碰真库地单测。
#[allow(clippy::too_many_arguments)]
fn build_usage_record(
    active_model_name: &str,
    config: &ModelConfig,
    source_app: &str,
    inbound: ModelFormat,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    duration_ms: u64,
    ok: bool,
    failover: bool,
    error: Option<String>,
) -> crate::usage::UsageRecord {
    let (timestamp, date) = crate::usage::current_timestamp();
    crate::usage::UsageRecord {
        id: 0,
        timestamp,
        date,
        model_name: active_model_name.to_string(),
        served_by: config.name.clone(),
        source_app: source_app.to_string(),
        upstream_url: provider_for(config.format).endpoint(config),
        upstream_model: config.model.clone(),
        // 能走到这里说明上游请求已经发出去了。代理开着、但目标是本机地址时仍算直连
        // （绕行规则见 providers::is_loopback_target），所以按目标地址判定。
        proxied: request_proxies_through(&provider_for(config.format).endpoint(config)),
        inbound_protocol: inbound.as_str().to_string(),
        upstream_protocol: config.format.as_str().to_string(),
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
        duration_ms,
        ok,
        failover,
        error,
    }
}

#[allow(clippy::too_many_arguments)]
fn record_usage(
    active_model_name: &str,
    config: &ModelConfig,
    source_app: &str,
    inbound: ModelFormat,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    duration_ms: u64,
    ok: bool,
    failover: bool,
    error: Option<String>,
    payload: crate::usage::UsagePayload,
) {
    crate::usage::submit(
        &build_usage_record(
            active_model_name,
            config,
            source_app,
            inbound,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
            duration_ms,
            ok,
            failover,
            error,
        ),
        Some(&payload),
    );
}

async fn health(State(stats): State<Arc<GatewayStats>>) -> Response {
    let settings = crate::settings::snapshot();
    let (requests, errors, _, _, failovers, _) = stats.snapshot();
    json_response(
        StatusCode::OK,
        json!({
            "status": "ok",
            "service": "ai-start-gateway",
            "protocols": ["anthropic-messages", "openai-completions", "openai-responses"],
            "activeModel": settings.active_model().map(|model| model.name.clone()),
            "autoFailover": settings.auto_failover,
            "requests": requests,
            "errors": errors,
            "failovers": failovers,
            // 落库是异步投递：写失败没有调用方可返回，只能在这里暴露给面板/排查。
            "dbWriteFailures": crate::db::write_failures(),
            "lastDbWriteError": crate::db::last_write_error()
        }),
    )
}

async fn models() -> Response {
    let data: Vec<Value> = MODEL_ROLES
        .iter()
        .map(|role| {
            json!({
                "id": role.id,
                "type": "model",
                "object": "model",
                "created": 0,
                "display_name": role.id,
                "owned_by": "ai-start"
            })
        })
        .collect();
    let first = MODEL_ROLES[0].id;
    let last = MODEL_ROLES[MODEL_ROLES.len() - 1].id;
    json_response(
        StatusCode::OK,
        json!({
            "object": "list",
            "data": data,
            "has_more": false,
            "first_id": first,
            "last_id": last
        }),
    )
}

async fn messages(
    State(stats): State<Arc<GatewayStats>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    route(ModelFormat::AnthropicMessages, stats, headers, body).await
}

async fn chat_completions(
    State(stats): State<Arc<GatewayStats>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    route(ModelFormat::OpenaiCompletions, stats, headers, body).await
}

async fn responses(
    State(stats): State<Arc<GatewayStats>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    route(ModelFormat::OpenaiResponses, stats, headers, body).await
}

/// 落库用的入站报文文本：只取入库上限**多一个字节**的前缀。
///
/// 多出来的那 1 个字节是给 `cap_bytes` 用的——它靠「长度是否超过上限」判定「确实被截断过」，
/// 少了它，正好超出一个字节的请求在库里会显示成完整报文。整份报文本来就要被 `serde_json`
/// 解析一遍，这里不再为落库多复制一份全集（32MB 的请求复制成 String 就是又多 32MB）。
fn inbound_request_text(body: &[u8]) -> String {
    let head = &body[..body.len().min(crate::usage::PAYLOAD_MAX_BYTES + 1)];
    String::from_utf8_lossy(head).into_owned()
}

async fn route(
    inbound: ModelFormat,
    stats: Arc<GatewayStats>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    stats.requests.fetch_add(1, Ordering::Relaxed);
    let started = std::time::Instant::now();
    let inbound_request = inbound_request_text(&body);
    let token = extract_token(&headers);
    let inbound_headers = serialize_headers(&headers);

    match handle(
        inbound,
        stats.clone(),
        headers,
        token.clone(),
        body,
        started,
        &inbound_request,
        &inbound_headers,
    )
    .await
    {
        Ok(response) => response,
        Err(failure) => {
            let RouteFailure {
                error,
                upstream_response,
                upstream_url,
                upstream_model,
                failover_trigger,
            } = failure;
            stats.record_error(&error.to_string());
            let message = error.to_string();
            let settings = crate::settings::snapshot();
            let active_name = settings
                .active_model()
                .map(|model| model.name.clone())
                .unwrap_or_default();
            let source_app = source_app_for(token.as_deref().unwrap_or_default());
            // 只有真发起了上游请求才谈得上「走了代理」：缺 Key、没启用模型这类失败压根没出网；
            // 本机地址即使代理开着也走的是直连。
            let proxied = !upstream_url.is_empty() && request_proxies_through(&upstream_url);
            // 触发判定（写库时即可判定）：自动切换开 + 未点名 + 失败形态值得探测。
            let probe_fired = failover_trigger
                .as_ref()
                .is_some_and(|trigger| {
                    super::failover::qualifies(settings.auto_failover, trigger.named, trigger.retryable)
                });
            let (timestamp, date) = crate::usage::current_timestamp();
            crate::usage::submit(
                &crate::usage::UsageRecord {
                    id: 0,
                    timestamp,
                    date,
                    model_name: active_name,
                    served_by: String::new(),
                    source_app,
                    upstream_url,
                    upstream_model,
                    proxied,
                    inbound_protocol: inbound.as_str().to_string(),
                    upstream_protocol: String::new(),
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                    duration_ms: started.elapsed().as_millis() as u64,
                    ok: false,
                    // 明细 failover 字段语义：这次请求触发了切换探测（写库时即可判定）；
                    // 切换结果由事件与统计呈现。
                    failover: probe_fired,
                    error: Some(message.clone()),
                },
                Some(&crate::usage::UsagePayload {
                    inbound_request: Some(inbound_request),
                    inbound_headers: Some(inbound_headers),
                    upstream_request: None,
                    upstream_response,
                    stream: false,
                }),
            );

            // 事后切换：落库完成后异步探测（不拖慢本次错误响应）。
            if probe_fired {
                if let Some(trigger) = failover_trigger {
                    super::failover::start_probe(
                        stats,
                        super::failover::FailoverContext {
                            failed_model_id: trigger.failed_model_id,
                            failed_model_name: trigger.failed_model_name,
                        },
                    );
                }
            }

            let (status, kind) = match error {
                AppError::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "authentication_error"),
                AppError::NotFound(_) => (StatusCode::NOT_FOUND, "api_error"),
                AppError::InvalidConfig(_) => (StatusCode::BAD_REQUEST, "api_error"),
                // `request_too_large` 是 Anthropic 的正式错误类型；OpenAI 那两个协议把同一个词
                // 放进 `type` / `code`，客户端本来就是按状态码 + message 处置，形状对得上。
                AppError::PayloadTooLarge(_) => (StatusCode::PAYLOAD_TOO_LARGE, "request_too_large"),
                _ => (StatusCode::BAD_GATEWAY, "api_error"),
            };
            api_error(inbound, status, kind, &message)
        }
    }
}

struct UpstreamFailure {
    error: AppError,
    retryable: bool,
    /// 上游返回的原始响应体（网络错误/超时没有响应体时为 None）。
    raw_response: Option<String>,
}

/// 一次上游失败里与「事后切换」相关的信息：失败的模型 + 失败形态是否值得探测。
struct FailoverTrigger {
    failed_model_id: i64,
    failed_model_name: String,
    /// dispatch 判定的 retryable（5xx / 401 / 403 / 404 / 408 / 429 / 网络错误）。
    retryable: bool,
    /// 本次是否点名模型：点名失败不触发切换。
    named: bool,
}

/// 透传给 `route()` 的失败信息：错误本身 + 上游原始报文 + 最后尝试的地址与模型，供失败记录落库展示；
/// `failover_trigger` 供 `route()` 落库后判定是否触发事后切换。
struct RouteFailure {
    error: AppError,
    upstream_response: Option<String>,
    upstream_url: String,
    upstream_model: String,
    failover_trigger: Option<FailoverTrigger>,
}

impl From<AppError> for RouteFailure {
    fn from(error: AppError) -> Self {
        Self {
            error,
            upstream_response: None,
            upstream_url: String::new(),
            upstream_model: String::new(),
            failover_trigger: None,
        }
    }
}

/// 同协议直通：入站协议与上游协议相同。
///
/// 请求侧据此选免转换快路；响应与流侧据此**原样转发上游字节**——不做转换、不合成起始事件、
/// 不补结束信号，客户端拿到的与直连上游一致。记账与判罚不受影响：那一份始终走旁路
/// （`StreamState`/`ResponseAssembler`）落库，与客户端出口无关。
pub(crate) fn same_protocol(inbound: ModelFormat, config: &ModelConfig) -> bool {
    config.format == inbound
}

/// 选定真正发往上游的报文：入站协议与上游协议相同时走免转换快路（以客户端原文为底，
/// 只改模型名与被过滤字段），不同协议才经规范层重建。provider 没有快路时（返回 None）回退重建。
pub(crate) fn encode_upstream_request(
    inbound: ModelFormat,
    config: &ModelConfig,
    request: &CanonicalRequest,
) -> AppResult<Value> {
    let provider = provider_for(config.format);
    if same_protocol(inbound, config) {
        if let Some(payload) = provider.encode_request_passthrough(config, request)? {
            return Ok(payload);
        }
    }
    provider.encode_request(config, request)
}

/// 选定回给客户端的响应报文：同协议直通时就是上游原文（解码只用来记账），
/// 跨协议才把规范响应重建成客户端协议的形状。
///
/// Anthropic 上游的 `decode_response` 本就是恒等，所以这道分叉只对两个 OpenAI 协议有实际差别
/// ——它们此前的重建会丢 `system_fingerprint`/`logprobs`、把 `created` 重生成当前时间、
/// 把 `refusal` 压成纯文本。
pub(crate) fn encode_client_response(
    inbound: ModelFormat,
    config: &ModelConfig,
    raw_response: &Value,
    canonical: &Value,
) -> AppResult<Value> {
    if same_protocol(inbound, config) {
        return Ok(raw_response.clone());
    }
    provider_for(inbound).encode_response(config, canonical)
}

/// 流式请求「等响应头」超时的错误：文案带模型名与秒数，明细与事件里能直接看出是超时。
fn upstream_timeout_error(config: &ModelConfig) -> AppError {
    AppError::Message(format!(
        "上游 {} 超过 {} 秒未返回响应（超时）",
        config.name,
        UPSTREAM_TIMEOUT.as_secs()
    ))
}

/// 把规范请求编码成上游协议原生报文并发起请求；成功时一并返回编码后的请求体（供落库展示）。
async fn dispatch(
    inbound: ModelFormat,
    request: &CanonicalRequest,
    headers: &HeaderMap,
    config: &ModelConfig,
) -> Result<(reqwest::Response, Value), UpstreamFailure> {
    let provider = provider_for(config.format);
    let payload =
        encode_upstream_request(inbound, config, request).map_err(|error| UpstreamFailure {
            error,
            retryable: false,
            raw_response: None,
        })?;

    let mut builder = http_client().post(provider.endpoint(config)).json(&payload);
    for (name, value) in provider.headers(config) {
        builder = builder.header(name, value);
    }
    if config.format == ModelFormat::AnthropicMessages {
        if let Some(beta) = headers
            .get("anthropic-beta")
            .and_then(|value| value.to_str().ok())
        {
            builder = builder.header("anthropic-beta", beta.to_string());
        }
    }

    // 超时（B1）：非流式限整个请求（reqwest 的请求级超时覆盖建连到响应体读完）；
    // 流式不能设它——长生成合法，请求级超时会把流中途掐死，改为只限「等响应头」
    //（响应体的首帧等待在 `first_frame_timeout` 里）。
    let streaming = request.stream();
    if !streaming {
        builder = builder.timeout(UPSTREAM_TIMEOUT);
    }

    let send = if streaming {
        match tokio::time::timeout(UPSTREAM_TIMEOUT, builder.send()).await {
            Ok(result) => result.map_err(AppError::from),
            Err(_elapsed) => Err(upstream_timeout_error(config)),
        }
    } else {
        builder.send().await.map_err(AppError::from)
    };
    let response = send.map_err(|error| UpstreamFailure {
        error,
        // 超时与网络错误同类：换一个模型确实可能通，值得事后探测。
        retryable: true,
        raw_response: None,
    })?;

    let status = response.status();
    if !status.is_success() {
        // 上游错误响应体完整保留（截断交给落库时的统一上限），错误文案只取前 400 字。
        let detail = response.text().await.unwrap_or_default();
        let retryable =
            status.is_server_error() || matches!(status.as_u16(), 401 | 403 | 404 | 408 | 429);
        let error = AppError::Message(format!(
            "上游 {} 返回 {}: {}",
            config.name,
            status.as_u16(),
            truncate(&detail, 400)
        ));
        return Err(UpstreamFailure {
            error,
            retryable,
            raw_response: Some(detail),
        });
    }

    Ok((response, payload))
}

/// 流式请求的记账快照（B3）。
///
/// 落库原先只放在流式生成器的尾部：客户端中途断开（Esc、崩溃、连接超时）时 hyper 会把响应流
/// 连同生成器一起丢弃，尾部代码永远跑不到——用量统计因此系统性漏记长生成的真实消耗。
/// 快照收进 `Arc<Mutex<…>>`：生成器边跑边推进它，生成器里那个 `DisconnectGuard` 被中途销毁
/// 时据此补一条「客户端断开」的失败记录。
struct StreamAccounting {
    /// 落库用的固定上下文（发起请求时就定下来了）
    primary: String,
    config: ModelConfig,
    source_app: String,
    inbound: ModelFormat,
    started: std::time::Instant,
    inbound_request: String,
    inbound_headers: String,
    upstream_request: String,
    /// 边跑边推进的记账状态
    upstream_state: StreamState,
    assembler: ResponseAssembler,
    stream_error: Option<String>,
    /// 已认领收尾（常规落库或守卫补记）。守卫只在 false 时补记，不双记。
    finished: bool,
}

impl StreamAccounting {
    fn new(
        primary: String,
        config: ModelConfig,
        source_app: String,
        inbound: ModelFormat,
        started: std::time::Instant,
        inbound_request: String,
        inbound_headers: String,
        upstream_request: String,
    ) -> Self {
        let upstream_state = StreamState::new(config.name.clone());
        Self {
            primary,
            config,
            source_app,
            inbound,
            started,
            inbound_request,
            inbound_headers,
            upstream_request,
            upstream_state,
            assembler: ResponseAssembler::default(),
            stream_error: None,
            finished: false,
        }
    }

    /// 落库前构造记录：把判定与写库分开，判定部分才能不碰真库地单测。
    fn usage_record(&self, ok: bool, error: Option<String>) -> crate::usage::UsageRecord {
        build_usage_record(
            &self.primary,
            &self.config,
            &self.source_app,
            self.inbound,
            self.upstream_state.input_tokens,
            self.upstream_state.output_tokens,
            (self.upstream_state.cache_read_tokens > 0)
                .then_some(self.upstream_state.cache_read_tokens),
            (self.upstream_state.cache_write_tokens > 0)
                .then_some(self.upstream_state.cache_write_tokens),
            (self.upstream_state.reasoning_tokens > 0)
                .then_some(self.upstream_state.reasoning_tokens),
            self.started.elapsed().as_millis() as u64,
            ok,
            false,
            error,
        )
    }

    /// 拼装结果 → 上游协议原生形状的报文文本（常规收尾与断连兜底共用）。
    fn upstream_response_text(&self) -> String {
        let mut canonical = self.assembler.to_value();
        // 输入/缓存 token 只在流末尾的 usage 事件里出现，message_start 时还没有，
        // 用流状态的最终值补齐，否则落库报文会显示输入 0。
        if let Some(usage) = canonical.get_mut("usage").and_then(Value::as_object_mut) {
            usage.insert("input_tokens".into(), json!(self.upstream_state.input_tokens));
            usage.insert(
                "cache_read_input_tokens".into(),
                json!(self.upstream_state.cache_read_tokens),
            );
            usage.insert(
                "cache_creation_input_tokens".into(),
                json!(self.upstream_state.cache_write_tokens),
            );
            usage.insert(
                "output_tokens_details".into(),
                json!({ "thinking_tokens": self.upstream_state.reasoning_tokens }),
            );
        }
        let native = provider_for(self.config.format)
            .encode_response(&self.config, &canonical)
            .unwrap_or(canonical);
        serde_json::to_string_pretty(&native).unwrap_or_else(|_| "{}".to_string())
    }

    fn payload(&self) -> crate::usage::UsagePayload {
        crate::usage::UsagePayload {
            inbound_request: Some(self.inbound_request.clone()),
            inbound_headers: Some(self.inbound_headers.clone()),
            upstream_request: Some(self.upstream_request.clone()),
            upstream_response: Some(self.upstream_response_text()),
            stream: true,
        }
    }

    /// 统计 + 明细落库（常规收尾与断连兜底共用）。先置 `finished` 认领——两种收尾只会有一个跑。
    fn record(&mut self, ok: bool, error: Option<String>, stats: &GatewayStats) {
        self.finished = true;
        stats.record_tokens(
            self.upstream_state.input_tokens,
            self.upstream_state.output_tokens,
        );
        if let Some(message) = error.as_deref() {
            stats.record_error(message);
        }
        crate::usage::submit(&self.usage_record(ok, error), Some(&self.payload()));
    }
}

/// 断连兜底的写入口：生产用 `StreamAccounting::record`，单测注入捕获实现（免得碰真库）。
type StreamWriter = fn(&mut StreamAccounting, &GatewayStats, bool, Option<String>);

/// 断连兜底守卫（B3）：随生成器一起被丢弃时，补记一条「客户端断开」。
/// 正常收尾会先把 `finished` 置位，守卫随即让位，不双记。
struct DisconnectGuard {
    accounting: Arc<Mutex<StreamAccounting>>,
    stats: Arc<GatewayStats>,
    writer: StreamWriter,
}

impl DisconnectGuard {
    fn new(accounting: Arc<Mutex<StreamAccounting>>, stats: Arc<GatewayStats>) -> Self {
        Self {
            accounting,
            stats,
            writer: |account, stats, ok, error| account.record(ok, error, stats),
        }
    }
}

impl Drop for DisconnectGuard {
    fn drop(&mut self) {
        // 锁即使被 poison（生成器持锁时 panic）也要能补记：快照本身仍是可读的数据。
        let mut account = self
            .accounting
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if account.finished {
            return;
        }
        account.finished = true;
        (self.writer)(
            &mut account,
            &self.stats,
            false,
            Some("客户端断开".to_string()),
        );
    }
}

/// 入站请求体的体积自检（H2）。报错就走 `route()` 那条统一的失败路径：按入站协议的形状回 413、
/// 记一条失败明细、计入错误统计。到了这里 body 已经全在内存里，`body_len` 就是实际长度，
/// 不看客户端报的 Content-Length——那个可以撒谎。
fn check_inbound_body(body_len: usize) -> AppResult<()> {
    if body_len <= MAX_INBOUND_BODY {
        return Ok(());
    }
    Err(AppError::PayloadTooLarge(format!(
        "请求体 {:.1} MB 超过网关上限 {} MB，请减少输入内容或图片体积后重试",
        body_len as f64 / MEGABYTE as f64,
        MAX_INBOUND_BODY / MEGABYTE,
    )))
}

async fn handle(
    inbound: ModelFormat,
    stats: Arc<GatewayStats>,
    headers: HeaderMap,
    token: Option<String>,
    body: Bytes,
    started: std::time::Instant,
    inbound_request: &str,
    inbound_headers: &str,
) -> Result<Response, RouteFailure> {
    let settings = crate::settings::snapshot();

    // 只校验 Key 非空；具体来源靠 token 匹配应用（匹配不到则原样记录）。
    let Some(token) = token.filter(|value| !value.trim().is_empty()) else {
        return Err(RouteFailure::from(AppError::Unauthorized(
            "缺少网关 API Key".into(),
        )));
    };
    let source_app = source_app_for(&token);

    // H2：体积自检。放在「有 Key」之后，超限就走统一的失败落库与形状映射。
    check_inbound_body(body.len()).map_err(RouteFailure::from)?;

    let raw: Value = serde_json::from_slice(&body)
        .map_err(|error| AppError::InvalidConfig(format!("请求体不是合法 JSON: {error}")))?;
    let inbound_provider = provider_for(inbound);
    // 保留客户端原文：OpenAI 系的 decode_request 会把报文重建成规范形状，原文只留在这里，
    // 同协议转发（入站协议 == 上游协议）才能免转换直通，不丢客户端的字段名与扩展键。
    let request = inbound_provider
        .decode_request(raw.clone())?
        .retain_client_raw(raw);
    // 规范级校验：无法保真转换的请求直接拒掉，别转一半。
    request.validate()?;
    // 过滤器：在转发前按规则改写规范请求，只跑一次。
    let request = crate::filters::apply(&crate::filters::snapshot(), request)?;

    // 一次请求只打一个上游：请求模型名命中显示名 → 该模型（锁定，失败不触发切换）；
    // 否则（别名 aiStart/auto、未指定、未命中）→ 当前模型。没有可用模型立即报错。
    let resolved = settings
        .resolve_target(Some(request.body().model.as_str()))
        .ok_or_else(|| RouteFailure::from(AppError::NotFound("网关没有启用中的模型".into())))?;
    let named = resolved.is_named();
    let config = resolved.into_config();
    // 落库的 model_name：点名时是被点名的模型，否则就是请求到达时生效的当前模型
    //（Active 分支拿到的 config 即当前模型）；事后切换事件登记「X → Y」的 X 也取它。
    let primary = config.name.clone();

    // 最后一次尝试的上游地址与模型，供失败记录展示。
    let last_attempt = (
        provider_for(config.format).endpoint(&config),
        config.model.clone(),
    );

    let (upstream, upstream_payload) = match dispatch(inbound, &request, &headers, &config).await {
        Ok(success) => success,
        Err(failure) => {
            let UpstreamFailure {
                error,
                retryable,
                raw_response,
            } = failure;
            // 第一个上游失败立即返回客户端；是否事后探测切换由 route() 落库后判定。
            return Err(RouteFailure {
                error,
                upstream_response: raw_response,
                upstream_url: last_attempt.0,
                upstream_model: last_attempt.1,
                failover_trigger: Some(FailoverTrigger {
                    failed_model_id: config.id,
                    failed_model_name: config.name.clone(),
                    retryable,
                    named,
                }),
            });
        }
    };

    // 记录实际发往上游的请求体（已套用提示词注入），供详情页核对注入结果。
    let upstream_request_text = serde_json::to_string_pretty(&upstream_payload)
        .unwrap_or_else(|_| upstream_payload.to_string());

    let upstream_provider = provider_for(config.format);

    if !request.stream() {
        let raw_response = upstream.json::<Value>().await.map_err(AppError::from)?;
        let canonical = upstream_provider.decode_response(&config, &raw_response)?;
        let input_tokens = canonical
            .pointer("/usage/input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let output_tokens = canonical
            .pointer("/usage/output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cache_read_tokens = canonical
            .pointer("/usage/cache_read_input_tokens")
            .and_then(Value::as_u64)
            .filter(|tokens| *tokens > 0);
        let cache_write_tokens = canonical
            .pointer("/usage/cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .filter(|tokens| *tokens > 0);
        let reasoning_tokens = canonical
            .pointer("/usage/output_tokens_details/thinking_tokens")
            .and_then(Value::as_u64)
            .filter(|tokens| *tokens > 0);
        stats.record_tokens(input_tokens, output_tokens);
        record_usage(
            &primary,
            &config,
            &source_app,
            inbound,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
            started.elapsed().as_millis() as u64,
            true,
            false,
            None,
            crate::usage::UsagePayload {
                inbound_request: Some(inbound_request.to_string()),
                inbound_headers: Some(inbound_headers.to_string()),
                upstream_request: Some(upstream_request_text),
                upstream_response: Some(
                    serde_json::to_string_pretty(&raw_response)
                        .unwrap_or_else(|_| raw_response.to_string()),
                ),
                stream: false,
            },
        );

        // 同协议直通：解码只用来记账，回给客户端的就是上游原文——重建会丢
        // `system_fingerprint`/`logprobs`、把 `created` 重生、把 `refusal` 压成纯文本。
        let wire = encode_client_response(inbound, &config, &raw_response, &canonical)?;
        return Ok(json_response(StatusCode::OK, wire));
    }

    // include_usage（OpenAI 入站的 stream_options）决定出站流末尾要不要补 usage 分片。
    let mut wire_state = WireState {
        include_usage: request.body().canonical.include_usage,
        ..WireState::default()
    };
    // 同协议直通：客户端那条出口直接吐上游原文。既不合成起始事件，也不补结束信号——
    // 上游自己的起始与结束就是客户端协议的（旧行为里合成的 `message_start` 会把网关的
    // 随机 uuid 塞进客户端分片的 id；断流时还会替上游补一条"正常结束"，等于对客户端撒谎）。
    let passthrough = same_protocol(inbound, &config);
    // 上游不会自己发 message_start 时才需要补（Anthropic 上游会发，两个 OpenAI 协议不会）。
    let emit_initial = !passthrough && !upstream_provider.is_passthrough();
    // 记账快照（B3）：生成器边跑边推进，连同一个 Drop 守卫一起进生成器——
    // 客户端中途断开时生成器被丢弃，守卫据快照补一条「客户端断开」，不再整条漏记。
    let accounting = Arc::new(Mutex::new(StreamAccounting::new(
        primary.clone(),
        config.clone(),
        source_app.clone(),
        inbound,
        started,
        inbound_request.to_string(),
        inbound_headers.to_string(),
        upstream_request_text,
    )));
    // 首帧限时（B1）：上游回了响应头却不吐数据时，流不能永久悬住。
    let events = first_frame_timeout(parse_sse_stream(upstream.bytes_stream()), UPSTREAM_TIMEOUT);

    let stream = async_stream::stream! {
        futures_util::pin_mut!(events);
        // 守卫随生成器一起被丢弃：正常收尾会先落库并置位 finished，守卫随即让位（不双记）。
        let _disconnect_guard = DisconnectGuard::new(accounting.clone(), stats.clone());

        // 取记账快照锁。加锁一律收在单个语句或单个块里——MutexGuard 一旦跨 yield，
        // 生成器就不再是 Send，而且守卫 drop 时可能撞上自己持有的锁。
        let snapshot = || {
            accounting
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        };

        // 规范事件 → 客户端字节。直通时返回空：客户端那条出口走上游原文，
        // 解码出来的规范事件只喂记账（`ResponseAssembler` + `StreamState`）。
        let encode_for_client = |canonical: &SseEvent, wire_state: &mut WireState| -> Vec<Bytes> {
            if passthrough {
                return Vec::new();
            }
            inbound_provider
                .encode_stream_event(&config, canonical, wire_state)
                .iter()
                .map(|event| Bytes::from(encode_event(event)))
                .collect()
        };

        if emit_initial {
            // 锁内推进记账、锁外发送（见 `snapshot` 的注释）。
            let startup = {
                let mut account = snapshot();
                let startup_events = account.upstream_state.begin();
                for canonical in &startup_events {
                    account.assembler.apply(canonical);
                }
                startup_events
            };
            for canonical in &startup {
                for bytes in encode_for_client(canonical, &mut wire_state) {
                    yield Ok::<Bytes, std::io::Error>(bytes);
                }
            }
        }

        while let Some(item) = events.next().await {
            match item {
                Ok(frame) => {
                    // 直通：这一帧原样发给客户端——注释行（心跳）、多行 data、分帧格式都保真。
                    if passthrough {
                        yield Ok(Bytes::from(frame.raw));
                    }
                    if frame.data.trim() == "[DONE]" {
                        // [DONE] 是上游真正的结束信号，记下来（没有它的流算被截断）。
                        // 收尾统一到流末（B4）：这里不再提前调 decode_stream_done——原来
                        // 「[DONE] 与流末各调一次、靠幂等兜底」是设计债。
                        snapshot().upstream_state.upstream_ended = true;
                        continue;
                    }
                    let Ok(value) = serde_json::from_str::<Value>(&frame.data) else {
                        continue;
                    };
                    // 锁内解码记账、锁外发送（见 `snapshot` 的注释）。
                    let outgoing = {
                        let mut account = snapshot();
                        match upstream_provider.decode_stream_event(
                            &config,
                            &frame.event,
                            &value,
                            &mut account.upstream_state,
                        ) {
                            Ok(canonical_events) => {
                                for canonical in &canonical_events {
                                    account.assembler.apply(canonical);
                                }
                                canonical_events
                                    .iter()
                                    .flat_map(|canonical| {
                                        encode_for_client(canonical, &mut wire_state)
                                    })
                                    .collect::<Vec<_>>()
                            }
                            Err(error) => {
                                let message = error.to_string();
                                account.stream_error = Some(message.clone());
                                // 直通时不能再补一条网关的错误事件：上游原文已经发出去了，
                                // 补上去等于在客户端的流里伪造内容。只记账，不发。
                                if passthrough {
                                    Vec::new()
                                } else {
                                    error_events(inbound, &message)
                                        .iter()
                                        .map(|event| Bytes::from(encode_event(event)))
                                        .collect()
                                }
                            }
                        }
                    };
                    for bytes in outgoing {
                        yield Ok(bytes);
                    }
                }
                Err(error) => {
                    let message = error.to_string();
                    snapshot().stream_error = Some(message.clone());
                    // 直通时同样不补网关的错误事件（上游原文已经发出去了）。
                    if !passthrough {
                        for event in error_events(inbound, &message) {
                            yield Ok(Bytes::from(encode_event(&event)));
                        }
                    }
                    break;
                }
            }
        }

        // 流末收尾：decode_stream_done 只在这里调一次（B4），没等到上游结束事件的流
        // 在这里补终态（判罚为 Truncated）。锁内构造、锁外发送。
        let tail = {
            let mut account = snapshot();
            let mut bytes_out = Vec::new();
            for canonical in upstream_provider
                .decode_stream_done(&config, &mut account.upstream_state)
                .unwrap_or_default()
            {
                account.assembler.apply(&canonical);
                bytes_out.extend(encode_for_client(&canonical, &mut wire_state));
            }
            // usage 只在流的末尾事件里出现，message_start 时还没有；出站前按流状态的最终值补齐，
            // 否则 include_usage 的客户端会收到一份输入 token 为 0 的 usage 分片。
            wire_state.input_tokens = account.upstream_state.input_tokens;
            wire_state.output_tokens = account.upstream_state.output_tokens;
            wire_state.cache_read_tokens = account.upstream_state.cache_read_tokens;
            wire_state.reasoning_tokens = account.upstream_state.reasoning_tokens;
            // 直通不补尾巴：上游的 usage 尾片与结束标记就是客户端该收到的那一份
            //（请求侧直通后 `stream_options.include_usage` 会原样发给上游，它自己会带）。
            if !passthrough {
                for event in inbound_provider.encode_stream_done(&config, &mut wire_state) {
                    bytes_out.push(Bytes::from(encode_event(&event)));
                }
            }
            bytes_out
        };
        for bytes in tail {
            yield Ok(bytes);
        }

        // 常规收尾：形态判定（上游「只给了思考没给正文」/「没发结束事件就断了」以前都被记成
        // 成功，明细里看不出异常）+ 落库。`record` 会置位 finished，断连守卫随即让位，不双记。
        let mut account = snapshot();
        let verdict = account.upstream_state.verdict(account.stream_error.clone());
        let (ok, error) = verdict.outcome();
        account.record(ok, error, &stats);
    };

    Ok(sse_response(stream))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 单测里替掉真库写入：`StreamAccounting::record` 会写进程级的 usage 库，
    // 而 `sqlite_persistence_round_trips` 断言的是记录条数——这里只捕获参数。
    thread_local! {
        static CAPTURED: std::cell::RefCell<Vec<(bool, Option<String>, u64, u64)>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    fn capture(
        account: &mut StreamAccounting,
        stats: &GatewayStats,
        ok: bool,
        error: Option<String>,
    ) {
        let (input, output) = (
            account.upstream_state.input_tokens,
            account.upstream_state.output_tokens,
        );
        // 与生产实现同款统计口径：兜底落库同样要计入面板与错误计数。
        stats.record_tokens(input, output);
        if let Some(message) = error.as_deref() {
            stats.record_error(message);
        }
        CAPTURED.with(|captured| captured.borrow_mut().push((ok, error, input, output)));
    }

    fn taken() -> Vec<(bool, Option<String>, u64, u64)> {
        CAPTURED.with(|captured| captured.borrow_mut().drain(..).collect())
    }

    fn config(format: ModelFormat) -> ModelConfig {
        ModelConfig {
            id: 1,
            name: "Test".into(),
            format,
            base_url: "https://example.test/v1".into(),
            api_key: "sk-test".into(),
            model: "upstream-model".into(),
            supports_1m: false,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn accounting() -> StreamAccounting {
        StreamAccounting::new(
            "Primary".into(),
            config(ModelFormat::OpenaiCompletions),
            "claude-desktop".into(),
            ModelFormat::OpenaiCompletions,
            std::time::Instant::now(),
            "{}".into(),
            "{}".into(),
            "{}".into(),
        )
    }

    /// B3 的核心承诺：客户端中途断开时，流已经被丢弃、生成器尾部跑不到了，
    /// 那条「已经消耗掉的 token」仍要落库，并标明原因。
    #[test]
    fn disconnect_guard_records_what_the_stream_had_already_accounted_for() {
        let stats = Arc::new(GatewayStats::default());
        let mut account = accounting();
        account.upstream_state.input_tokens = 7;
        account.upstream_state.output_tokens = 3;
        let shared = Arc::new(Mutex::new(account));

        drop(DisconnectGuard {
            accounting: shared.clone(),
            stats: stats.clone(),
            writer: capture,
        });

        assert_eq!(
            taken(),
            vec![(false, Some("客户端断开".to_string()), 7, 3)],
            "断连兜底该恰好补一条，带上已记账的用量"
        );
        let (_, errors, input, output, _, last) = stats.snapshot();
        assert_eq!((errors, input, output), (1, 7, 3), "统计不能被兜底路径漏掉");
        assert_eq!(last.as_deref(), Some("客户端断开"));
        assert!(
            shared.lock().expect("lock").finished,
            "兜底与常规收尾共用一个认领位"
        );
    }

    /// 正常收尾先认领，随后生成器销毁守卫——不能再补一条，否则每次成功请求都双记。
    #[test]
    fn disconnect_guard_stays_silent_after_the_regular_hand_off() {
        let stats = Arc::new(GatewayStats::default());
        let mut account = accounting();
        account.finished = true;
        let shared = Arc::new(Mutex::new(account));

        drop(DisconnectGuard {
            accounting: shared.clone(),
            stats: stats.clone(),
            writer: capture,
        });

        assert!(taken().is_empty(), "常规收尾落库后守卫必须让位");
        assert_eq!(stats.snapshot().1, 0, "让位的守卫也不该记错误");
    }

    /// A7：流内错误事件的形状只有一份来源。上游在流里报错时解码侧产出的是 Anthropic 形状的
    /// 规范事件，不能原样发给 OpenAI 客户端；而网关自产错误（断流、解帧失败、首帧超时）
    /// 走的是 `error_events`。两侧必须给同一个客户端同样的形状，否则它会碰到两种形状。
    #[test]
    fn stream_errors_have_one_shape_per_inbound_protocol() {
        let message = "上游返回错误";
        for inbound in [
            ModelFormat::AnthropicMessages,
            ModelFormat::OpenaiCompletions,
            ModelFormat::OpenaiResponses,
        ] {
            let mut state = StreamState::new("upstream-model");
            let canonical = state.error("api_error", message);
            let encoded = provider_for(inbound).encode_stream_event(
                &config(inbound),
                &canonical[0],
                &mut WireState::default(),
            );
            let upstream_side = error_events(inbound, message);
            assert_eq!(encoded.len(), 1, "{inbound:?} 应产出恰好一个错误事件");
            assert_eq!(encoded[0].event, upstream_side[0].event, "{inbound:?} 事件名");
            assert_eq!(
                encoded[0].data, upstream_side[0].data,
                "{inbound:?}：上游流内错误与网关自产错误必须是同一形状"
            );
        }
    }

    /// H2：体积自检的两条边界——上限之内放行，超一个字节就报 413 口径的错误，
    /// 文案里同时给出「这次多大」和「上限多少」，用户才知道差多少。
    #[test]
    fn inbound_body_check_rejects_only_above_the_cap() {
        assert!(check_inbound_body(MAX_INBOUND_BODY).is_ok());

        let message = check_inbound_body(MAX_INBOUND_BODY + 1)
            .expect_err("超过上限就该报错")
            .to_string();
        assert!(message.contains("32 MB"), "要说清上限是多少：{message}");
        assert!(message.contains("32.0 MB"), "也要说清这次是多少：{message}");

        // 硬上限必须真的比逻辑上限高：不然「略微超限的请求也能拿到协议形状的 413 和一条明细」
        // 就成了空话——所有超限请求都会被 axum 在 handler 之前用纯文本挡掉。
        assert!(MAX_INBOUND_BODY < INBOUND_BODY_HARD_LIMIT);
    }

    /// H2：超限的 413 要按**入站协议**的形状出——客户端 SDK 解析的是自己那套错误信封，
    /// 一个纯文本的 413 在它眼里就是「响应体解析失败」，用户看不到「请求太大」。
    #[tokio::test]
    async fn oversize_error_keeps_the_inbound_protocol_shape() {
        let message = check_inbound_body(MAX_INBOUND_BODY + 1)
            .expect_err("构造一个超限错误")
            .to_string();

        let anthropic = api_error(
            ModelFormat::AnthropicMessages,
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_too_large",
            &message,
        );
        assert_eq!(anthropic.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(anthropic.into_body(), usize::MAX)
                .await
                .expect("body"),
        )
        .expect("JSON");
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], "request_too_large");
        assert_eq!(body["error"]["message"], message.as_str());

        let openai = api_error(
            ModelFormat::OpenaiCompletions,
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_too_large",
            &message,
        );
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(openai.into_body(), usize::MAX)
                .await
                .expect("body"),
        )
        .expect("JSON");
        assert_eq!(body["error"]["type"], "request_too_large");
        assert_eq!(body["error"]["code"], "request_too_large");
        assert_eq!(body["error"]["message"], message.as_str());
    }

    /// H2 的端到端验证：40MB 的请求真的走到 handler 自检、拿到协议形状的 413，
    /// 并在明细里留下一条失败记录；入站报文按入库上限截断（不是把 40MB 整份写进库里）。
    ///
    /// 默认跳过：这条会往**进程级**的 usage 库里写一条记录，全量跑会把
    /// `sqlite_persistence_round_trips` 的「库里只有我这一条」断言顶掉（同一进程共用一个库）。
    /// 单独跑：`cargo test h2_oversize_request_is_rejected_and_recorded -- --ignored`
    #[tokio::test]
    #[ignore = "要写进程级 usage 库；会和 sqlite_persistence_round_trips 的计数断言打架"]
    async fn h2_oversize_request_is_rejected_and_recorded() {
        let dir = std::env::temp_dir().join(format!("ai-start-h2-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // 同进程里 `db::init` 只会生效一次：单独跑由本用例初始化；万一和别的用例一起跑，
        // 库已经在那儿了，沿用即可（要找的是「有没有我这条」，不是「库里一共几条」）。
        if let Err(error) = crate::settings::init(&dir) {
            println!("沿用已初始化的库：{error}");
        }

        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", "claude-desktop".parse().expect("header"));
        let body = Bytes::from(vec![b' '; MAX_INBOUND_BODY + 8 * MEGABYTE]);

        let response = route(
            ModelFormat::AnthropicMessages,
            crate::gateway::stats(),
            headers,
            body,
        )
        .await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let raw = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: Value = serde_json::from_slice(&raw).expect("JSON");
        assert_eq!(body["error"]["type"], "request_too_large");

        crate::db::flush();
        let record = crate::usage::recent(20)
            .into_iter()
            .find(|record| {
                record
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("超过网关上限"))
            })
            .expect("超限请求要落一条失败明细");
        assert!(!record.ok);
        assert_eq!(record.inbound_protocol, "anthropic-messages");
        let payload = crate::usage::payload_detail(record.id).expect("报文详情");
        assert!(payload.request_truncated, "40MB 的报文只该存下前缀");
        assert!(
            payload.inbound_request.expect("入站报文").len() <= crate::usage::PAYLOAD_MAX_BYTES,
            "入库的报文不许超过保存上限"
        );
    }
}

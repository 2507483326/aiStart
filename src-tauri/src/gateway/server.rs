use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
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
use crate::events;
use crate::providers::{
    http_client, provider_for, request_proxies_through, ResponseAssembler, SseEvent, StreamState,
    WireState,
};

use super::sse::{encode_channel_event, parse_sse_stream};
use super::{GatewayStats, MODEL_ROLES};

pub fn router() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/messages", post(messages))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses))
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

fn encode_event(event: &SseEvent) -> String {
    let data = match &event.raw {
        Some(raw) => raw.clone(),
        None => serde_json::to_string(&event.data).unwrap_or_else(|_| "{}".into()),
    };
    encode_channel_event(&event.event, &data)
}

fn error_events(inbound: ModelFormat, message: &str) -> Vec<SseEvent> {
    match inbound {
        ModelFormat::AnthropicMessages => vec![SseEvent::new(
            "error",
            json!({ "type": "error", "error": { "type": "api_error", "message": message } }),
        )],
        _ => vec![SseEvent::new(
            "error",
            json!({ "error": { "message": message, "type": "api_error" } }),
        )],
    }
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
    let (timestamp, date) = crate::usage::current_timestamp();
    crate::usage::record_with_payload(
        &crate::usage::UsageRecord {
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
        },
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
            "failovers": failovers
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

async fn route(
    inbound: ModelFormat,
    stats: Arc<GatewayStats>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    stats.requests.fetch_add(1, Ordering::Relaxed);
    let started = std::time::Instant::now();
    let inbound_request = String::from_utf8_lossy(&body).into_owned();
    let token = extract_token(&headers);

    match handle(
        inbound,
        stats.clone(),
        headers,
        token.clone(),
        body,
        started,
        &inbound_request,
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
            let (timestamp, date) = crate::usage::current_timestamp();
            crate::usage::record_with_payload(
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
                    failover: false,
                    error: Some(message.clone()),
                },
                Some(&crate::usage::UsagePayload {
                    inbound_request: Some(inbound_request),
                    upstream_request: None,
                    upstream_response,
                    stream: false,
                }),
            );

            let (status, kind) = match error {
                AppError::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "authentication_error"),
                AppError::NotFound(_) => (StatusCode::NOT_FOUND, "api_error"),
                AppError::InvalidConfig(_) => (StatusCode::BAD_REQUEST, "api_error"),
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

/// 透传给 `route()` 的失败信息：错误本身 + 上游原始报文 + 最后尝试的地址与模型，供失败记录落库展示。
struct RouteFailure {
    error: AppError,
    upstream_response: Option<String>,
    upstream_url: String,
    upstream_model: String,
}

impl From<AppError> for RouteFailure {
    fn from(error: AppError) -> Self {
        Self {
            error,
            upstream_response: None,
            upstream_url: String::new(),
            upstream_model: String::new(),
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

    let response = builder.send().await.map_err(|error| UpstreamFailure {
        error: error.into(),
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

async fn handle(
    inbound: ModelFormat,
    stats: Arc<GatewayStats>,
    headers: HeaderMap,
    token: Option<String>,
    body: Bytes,
    started: std::time::Instant,
    inbound_request: &str,
) -> Result<Response, RouteFailure> {
    let settings = crate::settings::snapshot();

    // 只校验 Key 非空；具体来源靠 token 匹配应用（匹配不到则原样记录）。
    let Some(token) = token.filter(|value| !value.trim().is_empty()) else {
        return Err(RouteFailure::from(AppError::Unauthorized(
            "缺少网关 API Key".into(),
        )));
    };
    let source_app = source_app_for(&token);

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
    // 过滤器：在转发前按规则改写规范请求。放在自动切换循环之外，
    // 保证重试多个上游时规则只套用一次。
    let request = crate::filters::apply(&crate::filters::snapshot(), request)?;

    // 请求里的模型名决定候选上游：别名（aiStart / auto）或未命中时走现有逻辑，
    // 命中模型列表里的显示名时只调用那一个模型（自动切换对它无效）。
    let candidates = settings.candidates_for(Some(request.body().model.as_str()));
    if candidates.is_empty() {
        return Err(RouteFailure::from(AppError::NotFound(
            "网关没有启用中的模型".into(),
        )));
    }

    // 首选模型：自动切换登记「X → Y」时用它，落库的 model_name 也用它
    // （现有逻辑下它就是当前模型；指定模型名时就是被指定的那个）。
    let primary = candidates[0].name.clone();
    let mut chosen: Option<(ModelConfig, reqwest::Response, Value)> = None;
    let mut failover_used = false;
    let mut last_error: Option<UpstreamFailure> = None;
    // 最后一次真正发起（或尝试发起）的上游地址与模型，供失败记录展示。
    let mut last_attempt: Option<(String, String)> = None;

    for (index, candidate) in candidates.iter().enumerate() {
        last_attempt = Some((
            provider_for(candidate.format).endpoint(candidate),
            candidate.model.clone(),
        ));
        match dispatch(inbound, &request, &headers, candidate).await {
            Ok((response, payload)) => {
                if index > 0 {
                    stats.record_failover(&primary, &candidate.name);
                    // 切换成功后把接手方记为当前模型：模型列表的「使用中」随之移动，
                    // 后续请求也直接以它为首选，不必每次都先撞一遍已失败的主模型。
                    match crate::settings::mutate(|settings| {
                        settings.active_model_id = Some(candidate.id);
                    }) {
                        Ok(()) => {
                            events::log(
                                "system",
                                Some("网关"),
                                "model.failover",
                                Some("model"),
                                Some(&candidate.id.to_string()),
                                Some(json!({ "from": &primary, "to": &candidate.name })),
                            );
                            super::publish();
                        }
                        Err(error) => {
                            stats.record_error(&format!("自动切换后更新当前模型失败: {error}"));
                        }
                    }
                    failover_used = true;
                }
                chosen = Some((candidate.clone(), response, payload));
                break;
            }
            Err(failure) => {
                last_error = Some(failure);
                if !last_error.as_ref().is_some_and(|failure| failure.retryable) {
                    break;
                }
            }
        }
    }

    let Some((config, upstream, upstream_payload)) = chosen else {
        let (upstream_url, upstream_model) = last_attempt.unwrap_or_default();
        return Err(match last_error {
            Some(failure) => RouteFailure {
                error: failure.error,
                upstream_response: failure.raw_response,
                upstream_url,
                upstream_model,
            },
            None => RouteFailure::from(AppError::Message("没有可用的上游模型".into())),
        });
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
            failover_used,
            None,
            crate::usage::UsagePayload {
                inbound_request: Some(inbound_request.to_string()),
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

    let mut upstream_state = StreamState::new(config.name.clone());
    // include_usage（OpenAI 入站的 stream_options）决定出站流末尾要不要补 usage 分片。
    let mut wire_state = WireState {
        include_usage: request.body().canonical.include_usage,
        ..WireState::default()
    };
    let mut assembler = ResponseAssembler::default();
    let inbound_request_owned = inbound_request.to_string();
    // 同协议直通：客户端那条出口直接吐上游原文。既不合成起始事件，也不补结束信号——
    // 上游自己的起始与结束就是客户端协议的（旧行为里合成的 `message_start` 会把网关的
    // 随机 uuid 塞进客户端分片的 id；断流时还会替上游补一条"正常结束"，等于对客户端撒谎）。
    let passthrough = same_protocol(inbound, &config);
    // 上游不会自己发 message_start 时才需要补（Anthropic 上游会发，两个 OpenAI 协议不会）。
    let emit_initial = !passthrough && !upstream_provider.is_passthrough();
    let events = parse_sse_stream(upstream.bytes_stream());

    let stream = async_stream::stream! {
        futures_util::pin_mut!(events);
        let mut stream_error: Option<String> = None;

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
            for canonical in upstream_state.begin() {
                assembler.apply(&canonical);
                for bytes in encode_for_client(&canonical, &mut wire_state) {
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
                        upstream_state.upstream_ended = true;
                        for canonical in upstream_provider.decode_stream_done(&config, &mut upstream_state).unwrap_or_default() {
                            assembler.apply(&canonical);
                            for bytes in encode_for_client(&canonical, &mut wire_state) {
                                yield Ok(bytes);
                            }
                        }
                        continue;
                    }
                    let Ok(value) = serde_json::from_str::<Value>(&frame.data) else {
                        continue;
                    };
                    match upstream_provider.decode_stream_event(&config, &frame.event, &value, &mut upstream_state) {
                        Ok(canonical_events) => {
                            for canonical in canonical_events {
                                assembler.apply(&canonical);
                                for bytes in encode_for_client(&canonical, &mut wire_state) {
                                    yield Ok(bytes);
                                }
                            }
                        }
                        Err(error) => {
                            let message = error.to_string();
                            stream_error = Some(message.clone());
                            // 直通时不能再补一条网关的错误事件：上游原文已经发出去了，
                            // 补上去等于在客户端的流里伪造内容。只记账，不发。
                            if !passthrough {
                                for event in error_events(inbound, &message) {
                                    yield Ok(Bytes::from(encode_event(&event)));
                                }
                            }
                        }
                    }
                }
                Err(error) => {
                    let message = error.to_string();
                    stream_error = Some(message.clone());
                    if !passthrough {
                        for event in error_events(inbound, &message) {
                            yield Ok(Bytes::from(encode_event(&event)));
                        }
                    }
                    break;
                }
            }
        }

        for canonical in upstream_provider.decode_stream_done(&config, &mut upstream_state).unwrap_or_default() {
            assembler.apply(&canonical);
            for bytes in encode_for_client(&canonical, &mut wire_state) {
                yield Ok(bytes);
            }
        }
        // usage 只在流的末尾事件里出现，message_start 时还没有；出站前按流状态的最终值补齐，
        // 否则 include_usage 的客户端会收到一份输入 token 为 0 的 usage 分片。
        wire_state.input_tokens = upstream_state.input_tokens;
        wire_state.output_tokens = upstream_state.output_tokens;
        wire_state.cache_read_tokens = upstream_state.cache_read_tokens;
        wire_state.reasoning_tokens = upstream_state.reasoning_tokens;
        // 直通不补尾巴：上游的 usage 尾片与结束标记就是客户端该收到的那一份
        //（请求侧直通后 `stream_options.include_usage` 会原样发给上游，它自己会带）。
        if !passthrough {
            for event in inbound_provider.encode_stream_done(&config, &mut wire_state) {
                yield Ok(Bytes::from(encode_event(&event)));
            }
        }

        // 形态判定：上游「只给了思考没给正文」或「没发结束事件就断了」以前都被记成成功，
        // 明细里看不出异常。线上报文保持不变，只把这条记录标成失败并写清原因。
        let verdict = upstream_state.verdict(stream_error.clone());
        let (ok, error) = verdict.outcome();
        stats.record_tokens(upstream_state.input_tokens, upstream_state.output_tokens);
        if let Some(message) = error.as_deref() {
            stats.record_error(message);
        }
        record_usage(
            &primary,
            &config,
            &source_app,
            inbound,
            upstream_state.input_tokens,
            upstream_state.output_tokens,
            (upstream_state.cache_read_tokens > 0).then_some(upstream_state.cache_read_tokens),
            (upstream_state.cache_write_tokens > 0).then_some(upstream_state.cache_write_tokens),
            (upstream_state.reasoning_tokens > 0).then_some(upstream_state.reasoning_tokens),
            started.elapsed().as_millis() as u64,
            ok,
            failover_used,
            error,
            crate::usage::UsagePayload {
                inbound_request: Some(inbound_request_owned),
                upstream_request: Some(upstream_request_text),
                upstream_response: Some({
                    // 拼装成 canonical 后转成上游协议的原生形状再落库，前端按协议解析（与非流式一致）
                    let mut canonical = assembler.to_value();
                    // 输入/缓存 token 只在流末尾的 usage 事件里出现，message_start 时还没有，
                    // 用流状态的最终值补齐，否则落库报文会显示输入 0。
                    if let Some(usage) = canonical.get_mut("usage").and_then(Value::as_object_mut) {
                        usage.insert("input_tokens".into(), json!(upstream_state.input_tokens));
                        usage.insert(
                            "cache_read_input_tokens".into(),
                            json!(upstream_state.cache_read_tokens),
                        );
                        usage.insert(
                            "cache_creation_input_tokens".into(),
                            json!(upstream_state.cache_write_tokens),
                        );
                        usage.insert(
                            "output_tokens_details".into(),
                            json!({ "thinking_tokens": upstream_state.reasoning_tokens }),
                        );
                    }
                    let native = upstream_provider
                        .encode_response(&config, &canonical)
                        .unwrap_or(canonical);
                    serde_json::to_string_pretty(&native).unwrap_or_else(|_| "{}".to_string())
                }),
                stream: true,
            },
        );
    };

    Ok(sse_response(stream))
}

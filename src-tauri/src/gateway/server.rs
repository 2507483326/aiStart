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
use crate::providers::{http_client, provider_for, ResponseAssembler, SseEvent, StreamState, WireState};

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
    if let Some(value) = headers.get("x-api-key").and_then(|value| value.to_str().ok()) {
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
            inbound_protocol: inbound.as_str().to_string(),
            upstream_protocol: config.format.as_str().to_string(),
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
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

async fn messages(State(stats): State<Arc<GatewayStats>>, headers: HeaderMap, body: Bytes) -> Response {
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
        Err(error) => {
            stats.record_error(&error.to_string());
            let message = error.to_string();
            let settings = crate::settings::snapshot();
            let active_name = settings
                .active_model()
                .map(|model| model.name.clone())
                .unwrap_or_default();
            let source_app = source_app_for(token.as_deref().unwrap_or_default());
            let (timestamp, date) = crate::usage::current_timestamp();
            crate::usage::record_with_payload(
                &crate::usage::UsageRecord {
                    id: 0,
                    timestamp,
                    date,
                    model_name: active_name,
                    served_by: String::new(),
                    source_app,
                    inbound_protocol: inbound.as_str().to_string(),
                    upstream_protocol: String::new(),
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    duration_ms: started.elapsed().as_millis() as u64,
                    ok: false,
                    failover: false,
                    error: Some(message.clone()),
                },
                Some(&crate::usage::UsagePayload {
                    inbound_request: Some(inbound_request),
                    upstream_response: None,
                    stream: false,
                }),
            );

            let status = match error {
                AppError::NotFound(_) => StatusCode::NOT_FOUND,
                AppError::InvalidConfig(_) => StatusCode::BAD_REQUEST,
                _ => StatusCode::BAD_GATEWAY,
            };
            api_error(inbound, status, "api_error", &message)
        }
    }
}

struct UpstreamFailure {
    error: AppError,
    retryable: bool,
}

async fn dispatch(
    request: &CanonicalRequest,
    headers: &HeaderMap,
    config: &ModelConfig,
) -> Result<reqwest::Response, UpstreamFailure> {
    let provider = provider_for(config.format);
    let payload = provider
        .encode_request(config, request)
        .map_err(|error| UpstreamFailure {
            error,
            retryable: false,
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
    })?;

    let status = response.status();
    if !status.is_success() {
        let detail = response.text().await.unwrap_or_default();
        let retryable =
            status.is_server_error() || matches!(status.as_u16(), 401 | 403 | 404 | 408 | 429);
        return Err(UpstreamFailure {
            error: AppError::Message(format!(
                "上游 {} 返回 {}: {}",
                config.name,
                status.as_u16(),
                truncate(&detail, 400)
            )),
            retryable,
        });
    }

    Ok(response)
}

async fn handle(
    inbound: ModelFormat,
    stats: Arc<GatewayStats>,
    headers: HeaderMap,
    token: Option<String>,
    body: Bytes,
    started: std::time::Instant,
    inbound_request: &str,
) -> AppResult<Response> {
    let settings = crate::settings::snapshot();

    // 只校验 Key 非空；具体来源靠 token 匹配应用（匹配不到则原样记录）。
    let Some(token) = token.filter(|value| !value.trim().is_empty()) else {
        return Ok(api_error(
            inbound,
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "缺少网关 API Key",
        ));
    };
    let source_app = source_app_for(&token);

    let raw: Value = serde_json::from_slice(&body)
        .map_err(|error| AppError::InvalidConfig(format!("请求体不是合法 JSON: {error}")))?;
    let inbound_provider = provider_for(inbound);
    let request = inbound_provider.decode_request(raw)?;
    // 过滤器：在转发前按规则改写规范请求。放在自动切换循环之外，
    // 保证重试多个上游时规则只套用一次。
    let request = crate::filters::apply(&crate::filters::snapshot(), request)?;

    let candidates = settings.candidate_models();
    if candidates.is_empty() {
        return Err(AppError::NotFound("网关没有启用中的模型".into()));
    }

    let primary = candidates[0].name.clone();
    let active_name = settings
        .active_model()
        .map(|model| model.name.clone())
        .unwrap_or_default();
    let mut chosen: Option<(ModelConfig, reqwest::Response)> = None;
    let mut failover_used = false;
    let mut last_error: Option<AppError> = None;

    for (index, candidate) in candidates.iter().enumerate() {
        match dispatch(&request, &headers, candidate).await {
            Ok(response) => {
                if index > 0 {
                    stats.record_failover(&primary, &candidate.name);
                    failover_used = true;
                }
                chosen = Some((candidate.clone(), response));
                break;
            }
            Err(failure) => {
                last_error = Some(failure.error);
                if !failure.retryable {
                    break;
                }
            }
        }
    }

    let Some((config, upstream)) = chosen else {
        return Err(last_error.unwrap_or_else(|| AppError::Message("没有可用的上游模型".into())));
    };

    let upstream_provider = provider_for(config.format);

    if !request.stream() {
        let raw_response = upstream.json::<Value>().await?;
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
        stats.record_tokens(input_tokens, output_tokens);
        record_usage(
            &active_name,
            &config,
            &source_app,
            inbound,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            started.elapsed().as_millis() as u64,
            true,
            failover_used,
            None,
            crate::usage::UsagePayload {
                inbound_request: Some(inbound_request.to_string()),
                upstream_response: Some(
                    serde_json::to_string_pretty(&raw_response)
                        .unwrap_or_else(|_| raw_response.to_string()),
                ),
                stream: false,
            },
        );

        let wire = inbound_provider.encode_response(&config, &canonical)?;
        return Ok(json_response(StatusCode::OK, wire));
    }

    let mut upstream_state = StreamState::new(config.name.clone());
    let mut wire_state = WireState::default();
    let mut assembler = ResponseAssembler::default();
    let inbound_request_owned = inbound_request.to_string();
    let emit_initial = !upstream_provider.is_passthrough();
    let events = parse_sse_stream(upstream.bytes_stream());

    let stream = async_stream::stream! {
        futures_util::pin_mut!(events);
        let mut stream_error: Option<String> = None;

        if emit_initial {
            for canonical in upstream_state.begin() {
                assembler.apply(&canonical);
                for event in inbound_provider.encode_stream_event(&config, &canonical, &mut wire_state) {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from(encode_event(&event)));
                }
            }
        }

        while let Some(item) = events.next().await {
            match item {
                Ok((event_name, data)) => {
                    if data.trim() == "[DONE]" {
                        for canonical in upstream_provider.decode_stream_done(&config, &mut upstream_state).unwrap_or_default() {
                            assembler.apply(&canonical);
                            for event in inbound_provider.encode_stream_event(&config, &canonical, &mut wire_state) {
                                yield Ok(Bytes::from(encode_event(&event)));
                            }
                        }
                        continue;
                    }
                    let Ok(value) = serde_json::from_str::<Value>(&data) else {
                        continue;
                    };
                    match upstream_provider.decode_stream_event(&config, &event_name, &value, &mut upstream_state) {
                        Ok(canonical_events) => {
                            for canonical in canonical_events {
                                assembler.apply(&canonical);
                                for event in inbound_provider.encode_stream_event(&config, &canonical, &mut wire_state) {
                                    yield Ok(Bytes::from(encode_event(&event)));
                                }
                            }
                        }
                        Err(error) => {
                            let message = error.to_string();
                            stream_error = Some(message.clone());
                            for event in error_events(inbound, &message) {
                                yield Ok(Bytes::from(encode_event(&event)));
                            }
                        }
                    }
                }
                Err(error) => {
                    let message = error.to_string();
                    stream_error = Some(message.clone());
                    for event in error_events(inbound, &message) {
                        yield Ok(Bytes::from(encode_event(&event)));
                    }
                    break;
                }
            }
        }

        for canonical in upstream_provider.decode_stream_done(&config, &mut upstream_state).unwrap_or_default() {
            assembler.apply(&canonical);
            for event in inbound_provider.encode_stream_event(&config, &canonical, &mut wire_state) {
                yield Ok(Bytes::from(encode_event(&event)));
            }
        }
        for event in inbound_provider.encode_stream_done(&config, &mut wire_state) {
            yield Ok(Bytes::from(encode_event(&event)));
        }

        stats.record_tokens(upstream_state.input_tokens, upstream_state.output_tokens);
        if let Some(message) = stream_error.as_deref() {
            stats.record_error(message);
        }
        record_usage(
            &active_name,
            &config,
            &source_app,
            inbound,
            upstream_state.input_tokens,
            upstream_state.output_tokens,
            (upstream_state.cache_read_tokens > 0).then_some(upstream_state.cache_read_tokens),
            (upstream_state.cache_write_tokens > 0).then_some(upstream_state.cache_write_tokens),
            started.elapsed().as_millis() as u64,
            stream_error.is_none(),
            failover_used,
            stream_error,
            crate::usage::UsagePayload {
                inbound_request: Some(inbound_request_owned),
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

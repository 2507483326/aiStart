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
use crate::providers::{http_client, provider_for, SseEvent, StreamState};

use super::sse::{encode_channel_event, parse_sse_stream};
use super::GatewayStats;

pub fn router() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/messages", post(messages))
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

fn api_error(status: StatusCode, kind: &str, message: &str) -> Response {
    json_response(
        status,
        json!({ "type": "error", "error": { "type": kind, "message": message } }),
    )
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

fn encode_event(event: &SseEvent) -> String {
    let data = serde_json::to_string(&event.data).unwrap_or_else(|_| "{}".into());
    encode_channel_event(&event.event, &data)
}

fn encode_error(message: &str) -> String {
    encode_event(&SseEvent::new(
        "error",
        json!({ "type": "error", "error": { "type": "api_error", "message": message } }),
    ))
}

fn sse_response(stream: impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "api_error", "构建流式响应失败"))
}

async fn health(State(stats): State<Arc<GatewayStats>>) -> Response {
    let settings = crate::settings::snapshot();
    let (requests, errors, _, _, failovers, _) = stats.snapshot();
    json_response(
        StatusCode::OK,
        json!({
            "status": "ok",
            "service": "ai-start-gateway",
            "activeModel": settings.active_model().map(|model| model.name.clone()),
            "autoFailover": settings.auto_failover,
            "requests": requests,
            "errors": errors,
            "failovers": failovers
        }),
    )
}

async fn models() -> Response {
    let settings = crate::settings::snapshot();
    let entries: Vec<Value> = settings
        .models
        .iter()
        .map(|model| {
            json!({
                "type": "model",
                "id": alias_for(model),
                "display_name": model.name,
                "created_at": model.created_at
            })
        })
        .collect();

    json_response(
        StatusCode::OK,
        json!({
            "data": entries,
            "has_more": false,
            "first_id": entries.first().and_then(|entry| entry.get("id")).cloned(),
            "last_id": entries.last().and_then(|entry| entry.get("id")).cloned()
        }),
    )
}

fn alias_for(model: &ModelConfig) -> String {
    crate::platform::model_alias(model)
}

async fn messages(State(stats): State<Arc<GatewayStats>>, headers: HeaderMap, body: Bytes) -> Response {
    stats.requests.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    match handle_messages(stats.clone(), headers, body).await {
        Ok(response) => response,
        Err(error) => {
            stats.record_error(&error.to_string());
            let status = match error {
                AppError::NotFound(_) => StatusCode::NOT_FOUND,
                AppError::InvalidConfig(_) => StatusCode::BAD_REQUEST,
                _ => StatusCode::BAD_GATEWAY,
            };
            api_error(status, "api_error", &error.to_string())
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

async fn handle_messages(
    stats: Arc<GatewayStats>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Response> {
    let settings = crate::settings::snapshot();

    let provided = extract_token(&headers);
    if provided.as_deref() != Some(settings.gateway_token.as_str()) {
        return Ok(api_error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "网关 API Key 不匹配，请在应用详情中重新执行「一键应用模型」",
        ));
    }

    let raw: Value = serde_json::from_slice(&body)
        .map_err(|error| AppError::InvalidConfig(format!("请求体不是合法 JSON: {error}")))?;
    let request = CanonicalRequest::parse(raw)?;

    let candidates = settings.candidate_models();
    if candidates.is_empty() {
        return Err(AppError::NotFound("网关没有启用中的模型".into()));
    }

    let primary = candidates[0].name.clone();
    let mut chosen: Option<(ModelConfig, reqwest::Response)> = None;
    let mut last_error: Option<AppError> = None;

    for (index, candidate) in candidates.iter().enumerate() {
        match dispatch(&request, &headers, candidate).await {
            Ok(response) => {
                if index > 0 {
                    stats.record_failover(&primary, &candidate.name);
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

    let provider = provider_for(config.format);

    if !request.stream() {
        let raw = upstream.json::<Value>().await?;
        let response = provider.decode_response(&config, &raw)?;
        if let Some(usage) = response.get("usage") {
            stats.record_tokens(
                usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
                usage.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
            );
        }
        return Ok(json_response(StatusCode::OK, response));
    }

    let mut state = StreamState::new(config.name.clone());
    let passthrough = provider.is_passthrough();
    let events = parse_sse_stream(upstream.bytes_stream());

    let stream = async_stream::stream! {
        futures_util::pin_mut!(events);

        if !passthrough {
            for event in state.begin() {
                yield Ok::<Bytes, std::io::Error>(Bytes::from(encode_event(&event)));
            }
        }

        while let Some(item) = events.next().await {
            match item {
                Ok((event_name, data)) => {
                    if data.trim() == "[DONE]" {
                        for event in provider.decode_stream_done(&config, &mut state).unwrap_or_default() {
                            yield Ok(Bytes::from(encode_event(&event)));
                        }
                        continue;
                    }
                    let Ok(value) = serde_json::from_str::<Value>(&data) else {
                        continue;
                    };
                    match provider.decode_stream_event(&config, &event_name, &value, &mut state) {
                        Ok(list) => {
                            for event in list {
                                yield Ok(Bytes::from(encode_event(&event)));
                            }
                        }
                        Err(error) => {
                            yield Ok(Bytes::from(encode_error(&error.to_string())));
                        }
                    }
                }
                Err(error) => {
                    yield Ok(Bytes::from(encode_error(&error.to_string())));
                    break;
                }
            }
        }

        for event in provider.decode_stream_done(&config, &mut state).unwrap_or_default() {
            yield Ok(Bytes::from(encode_event(&event)));
        }

        stats.record_tokens(state.input_tokens, state.output_tokens);
    };

    Ok(sse_response(stream))
}

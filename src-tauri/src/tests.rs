use serde_json::{json, Value};

use crate::commands::models::parse_model_ids;
use crate::domain::canonical::{
    CanonicalOnly, CanonicalRequest, CanonicalResponseFormat, CanonicalToolChoice, MaxTokensField,
    RequestBody, SystemPrompt,
};
use crate::domain::model::{ModelConfig, ModelFormat};
use crate::providers::normalizer::{BlockNormalizer, StreamVerdict};
use crate::providers::{provider_for, wire, SseEvent, StreamState, WireState};
use crate::settings::Settings;

fn model(format: ModelFormat, base_url: &str) -> ModelConfig {
    ModelConfig {
        id: 1,
        name: "Test".into(),
        format,
        base_url: base_url.into(),
        api_key: "sk-test".into(),
        model: "upstream-model".into(),
        supports_1m: false,
        created_at: String::new(),
        updated_at: String::new(),
    }
}

fn request(body: serde_json::Value) -> CanonicalRequest {
    CanonicalRequest::parse(body).expect("request should parse")
}

#[test]
fn urls_handle_base_with_and_without_v1_suffix() {
    let bare = model(ModelFormat::AnthropicMessages, "https://api.anthropic.com/");
    assert_eq!(
        bare.completion_url(),
        "https://api.anthropic.com/v1/messages"
    );
    assert_eq!(bare.models_url(), "https://api.anthropic.com/v1/models");

    let with_v1 = model(ModelFormat::OpenaiCompletions, "https://api.openai.com/v1");
    assert_eq!(
        with_v1.completion_url(),
        "https://api.openai.com/v1/chat/completions"
    );
    assert_eq!(with_v1.models_url(), "https://api.openai.com/v1/models");

    let responses = model(ModelFormat::OpenaiResponses, "http://127.0.0.1:11434/v1/");
    assert_eq!(
        responses.completion_url(),
        "http://127.0.0.1:11434/v1/responses"
    );
}

fn resolve_target_settings() -> Settings {
    let mut settings = Settings::default();
    for name in ["A", "B", "C"] {
        settings.upsert(crate::domain::model::ModelInput {
            id: None,
            name: name.into(),
            format: ModelFormat::OpenaiCompletions,
            base_url: "https://example.com/v1".into(),
            api_key: String::new(),
            model: name.into(),
            supports_1m: false,
        });
    }
    settings.active_model_id = Some(2);
    settings
}

#[test]
fn resolve_target_sends_aliases_and_misses_to_the_active_model() {
    let mut settings = resolve_target_settings();
    settings.auto_failover = true;

    let label = |resolved: &crate::settings::ResolvedTarget| match resolved {
        crate::settings::ResolvedTarget::Named(model) => format!("named:{}", model.name),
        crate::settings::ResolvedTarget::Active(model) => format!("active:{}", model.name),
    };

    // 网关别名与 auto（任意大小写、带空白）、未指定、空白、未命中 → 当前模型，可参与切换。
    for requested in [
        Some("aiStart"),
        Some("aistart"),
        Some("AISTART"),
        Some("auto"),
        Some("Auto"),
        Some("AUTO"),
        Some("  auto  "),
        Some("  aiStart "),
        None,
        Some("   "),
        Some("不存在"),
    ] {
        let resolved = settings
            .resolve_target(requested)
            .expect("应解析到当前模型");
        assert_eq!(
            label(&resolved),
            "active:B",
            "{requested:?} 应落到当前模型"
        );
    }
}

#[test]
fn resolve_target_locks_named_models_and_requires_an_enabled_model() {
    let settings = resolve_target_settings();

    // 命中显示名（不区分大小写、带空白）→ 锁定该模型：只调用它，失败不触发切换。
    for name in ["C", "c", "  C  ", "A", "  a  "] {
        let resolved = settings.resolve_target(Some(name)).expect("点名应命中");
        assert!(resolved.is_named(), "{name:?} 应识别为点名");
        match resolved {
            crate::settings::ResolvedTarget::Named(model) => {
                let expected = name.trim().to_uppercase();
                assert_eq!(model.name, expected, "{name:?} 应锁定到 {expected}");
            }
            _ => unreachable!(),
        }
    }

    // 没有任何模型 → None，调用方报错。
    let empty = Settings::default();
    assert!(empty.resolve_target(Some("auto")).is_none());
    assert!(empty.resolve_target(Some("A")).is_none());

    // 点名命中时 auto_failover 开关无关紧要：锁定语义不受影响。
    let mut locked = resolve_target_settings();
    locked.auto_failover = false;
    assert!(locked.resolve_target(Some("C")).is_some());
    assert!(locked.resolve_target(Some("auto")).is_some());
}

#[test]
fn failover_qualifies_only_when_auto_on_unnamed_and_retryable() {
    use crate::gateway::failover::qualifies;

    // 全部满足才触发。
    assert!(qualifies(true, false, true));
    // 关闭自动切换 / 点名模型 / 不可重试的失败（4xx 中换模型救不了的）都不触发。
    assert!(!qualifies(false, false, true));
    assert!(!qualifies(true, true, true));
    assert!(!qualifies(true, false, false));
    // 关闭自动切换时，其余条件再齐也不触发。
    assert!(!qualifies(false, true, false));
}

#[test]
fn probe_order_skips_the_failed_model_and_may_switch_guards_manual_changes() {
    use crate::gateway::failover::{may_switch, probe_order};

    let mut settings = resolve_target_settings(); // A(1) B(2) C(3)，当前 B
    settings.auto_failover = true;

    // 探测顺序 = 模型列表原顺序，跳过刚失败的模型。
    assert_eq!(probe_order(&settings.models, 2), vec![1, 3]);
    assert_eq!(probe_order(&settings.models, 1), vec![2, 3]);
    assert_eq!(probe_order(&settings.models, 3), vec![1, 2]);
    // 失败的模型不在列表里（如已删除）→ 不跳过任何模型，全部可探测。
    assert_eq!(probe_order(&settings.models, 99), vec![1, 2, 3]);

    // 切换守卫：当前模型仍是刚失败的那个才允许切；用户手动换过就放弃。
    assert!(may_switch(Some(2), 2));
    assert!(!may_switch(Some(3), 2));
    assert!(!may_switch(None, 2));
}

#[test]
fn auto_alias_detection_is_case_insensitive() {
    use crate::gateway::is_auto_alias;
    for name in ["aiStart", "aistart", "AISTART", "aiSTART", "auto", "AUTO"] {
        assert!(is_auto_alias(name), "{name}");
    }
    for name in ["", " autos", "autoo", "claude-sonnet-5", "我的模型"] {
        assert!(!is_auto_alias(name), "{name}");
    }
}

#[test]
fn anthropic_passthrough_replaces_model_and_keeps_body() {
    let config = model(ModelFormat::AnthropicMessages, "https://api.anthropic.com");
    let provider = provider_for(ModelFormat::AnthropicMessages);
    assert!(provider.is_passthrough());

    let canonical = request(json!({
        "model": "claude-from-desktop",
        "max_tokens": 128,
        "messages": [{ "role": "user", "content": "hi" }]
    }));

    let encoded = provider.encode_request(&config, &canonical).unwrap();
    assert_eq!(encoded["model"], "upstream-model");
    assert_eq!(encoded["max_tokens"], 128);
}

#[test]
fn openai_completions_lifts_system_and_converts_tools() {
    let config = model(ModelFormat::OpenaiCompletions, "https://api.openai.com/v1");
    let provider = provider_for(ModelFormat::OpenaiCompletions);

    let canonical = request(json!({
        "model": "ignored",
        "max_tokens": 64,
        "system": "Be terse.",
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": "weather?" }] }],
        "tools": [{
            "name": "get_weather",
            "description": "lookup",
            "input_schema": { "type": "object", "properties": { "city": { "type": "string" } } }
        }]
    }));

    let encoded = provider.encode_request(&config, &canonical).unwrap();
    assert_eq!(encoded["model"], "upstream-model");
    assert_eq!(encoded["messages"][0]["role"], "system");
    assert_eq!(encoded["messages"][0]["content"], "Be terse.");
    assert_eq!(encoded["messages"][1]["role"], "user");
    assert_eq!(encoded["messages"][1]["content"], "weather?");
    assert_eq!(encoded["tools"][0]["type"], "function");
    assert_eq!(encoded["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(
        encoded["tools"][0]["function"]["parameters"]["type"],
        "object"
    );
}

#[test]
fn openai_completions_splits_tool_results_into_tool_messages() {
    let config = model(ModelFormat::OpenaiCompletions, "https://api.openai.com/v1");
    let provider = provider_for(ModelFormat::OpenaiCompletions);

    let canonical = request(json!({
        "model": "ignored",
        "messages": [
            { "role": "assistant", "content": [
                { "type": "text", "text": "let me check" },
                { "type": "tool_use", "id": "call_1", "name": "get_weather", "input": { "city": "Beijing" } }
            ]},
            { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "call_1", "content": "sunny" }
            ]}
        ]
    }));

    let encoded = provider.encode_request(&config, &canonical).unwrap();
    let messages = encoded["messages"].as_array().unwrap();

    assert_eq!(messages[0]["role"], "assistant");
    assert_eq!(messages[0]["tool_calls"][0]["id"], "call_1");
    assert_eq!(
        messages[0]["tool_calls"][0]["function"]["arguments"],
        "{\"city\":\"Beijing\"}"
    );
    assert_eq!(messages[1]["role"], "tool");
    assert_eq!(messages[1]["tool_call_id"], "call_1");
    assert_eq!(messages[1]["content"], "sunny");
}

#[test]
fn openai_completions_decodes_tool_calls_into_tool_use_blocks() {
    let config = model(ModelFormat::OpenaiCompletions, "https://api.openai.com/v1");
    let provider = provider_for(ModelFormat::OpenaiCompletions);

    let decoded = provider
        .decode_response(
            &config,
            &json!({
                "id": "chatcmpl-1",
                "model": "gpt",
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "done",
                        "tool_calls": [{
                            "id": "call_9",
                            "type": "function",
                            "function": { "name": "get_weather", "arguments": "{\"city\":\"Beijing\"}" }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": { "prompt_tokens": 3, "completion_tokens": 4 }
            }),
        )
        .unwrap();

    assert_eq!(decoded["type"], "message");
    assert_eq!(decoded["content"][0]["type"], "text");
    assert_eq!(decoded["content"][1]["type"], "tool_use");
    assert_eq!(decoded["content"][1]["input"]["city"], "Beijing");
    assert_eq!(decoded["stop_reason"], "tool_use");
    assert_eq!(decoded["usage"]["input_tokens"], 3);
}

#[test]
fn openai_completions_splits_cached_prompt_tokens() {
    let config = model(ModelFormat::OpenaiCompletions, "https://api.openai.com/v1");
    let provider = provider_for(ModelFormat::OpenaiCompletions);

    // OpenAI 的 prompt_tokens 已含缓存命中：canonical 拆成「未命中输入 + 缓存读」，两者之和不变。
    let decoded = provider
        .decode_response(
            &config,
            &json!({
                "id": "chatcmpl-1",
                "model": "gpt",
                "choices": [{ "message": { "role": "assistant", "content": "ok" }, "finish_reason": "stop" }],
                "usage": {
                    "prompt_tokens": 1000,
                    "completion_tokens": 20,
                    "prompt_tokens_details": { "cached_tokens": 900 }
                }
            }),
        )
        .unwrap();

    assert_eq!(decoded["usage"]["input_tokens"], 100);
    assert_eq!(decoded["usage"]["cache_read_input_tokens"], 900);
    assert_eq!(decoded["usage"]["output_tokens"], 20);

    // 回写给 OpenAI 客户端时缓存读并回 prompt_tokens（OpenAI 语义：prompt_tokens 含缓存）
    let encoded = provider.encode_response(&config, &decoded).unwrap();
    assert_eq!(encoded["usage"]["prompt_tokens"], 1000);
    assert_eq!(
        encoded["usage"]["prompt_tokens_details"]["cached_tokens"],
        900
    );
}

#[test]
fn openai_responses_splits_cached_input_tokens() {
    let config = model(ModelFormat::OpenaiResponses, "https://api.openai.com/v1");
    let provider = provider_for(ModelFormat::OpenaiResponses);

    let decoded = provider
        .decode_response(
            &config,
            &json!({
                "id": "resp_1",
                "model": "gpt",
                "status": "completed",
                "output": [],
                "usage": {
                    "input_tokens": 500,
                    "output_tokens": 10,
                    "input_tokens_details": { "cached_tokens": 400 }
                }
            }),
        )
        .unwrap();

    assert_eq!(decoded["usage"]["input_tokens"], 100);
    assert_eq!(decoded["usage"]["cache_read_input_tokens"], 400);
    assert_eq!(decoded["usage"]["output_tokens"], 10);
}

#[test]
fn anthropic_stream_records_cache_tokens() {
    let config = model(ModelFormat::AnthropicMessages, "https://api.anthropic.com");
    let provider = provider_for(ModelFormat::AnthropicMessages);
    let mut state = StreamState::new("claude");

    provider
        .decode_stream_event(
            &config,
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": "msg_1",
                    "model": "claude",
                    "usage": {
                        "input_tokens": 50,
                        "output_tokens": 0,
                        "cache_read_input_tokens": 1000,
                        "cache_creation_input_tokens": 200
                    }
                }
            }),
            &mut state,
        )
        .unwrap();

    assert_eq!(state.input_tokens, 50);
    assert_eq!(state.cache_read_tokens, 1000);
    assert_eq!(state.cache_write_tokens, 200);
}

#[test]
fn openai_responses_encodes_instructions_and_function_items() {
    let config = model(ModelFormat::OpenaiResponses, "https://api.openai.com/v1");
    let provider = provider_for(ModelFormat::OpenaiResponses);

    let canonical = request(json!({
        "model": "ignored",
        "system": "Be terse.",
        "messages": [
            { "role": "assistant", "content": [
                { "type": "tool_use", "id": "call_1", "name": "get_weather", "input": { "city": "Beijing" } }
            ]},
            { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "call_1", "content": "sunny" }
            ]}
        ]
    }));

    let encoded = provider.encode_request(&config, &canonical).unwrap();
    assert_eq!(encoded["instructions"], "Be terse.");
    assert_eq!(encoded["input"][0]["type"], "function_call");
    assert_eq!(encoded["input"][0]["call_id"], "call_1");
    assert_eq!(encoded["input"][1]["type"], "function_call_output");
    assert_eq!(encoded["input"][1]["output"], "sunny");
}

#[test]
fn model_id_parsing_covers_common_endpoint_shapes() {
    let openai = json!({ "object": "list", "data": [{ "id": "gpt-4o" }, { "id": "gpt-4o-mini" }] });
    assert_eq!(parse_model_ids(&openai), vec!["gpt-4o", "gpt-4o-mini"]);

    let anthropic = json!({ "data": [{ "id": "claude-sonnet-4-5", "display_name": "Sonnet" }] });
    assert_eq!(parse_model_ids(&anthropic), vec!["claude-sonnet-4-5"]);

    let ollama = json!({ "models": [{ "name": "qwen3:32b" }] });
    assert_eq!(parse_model_ids(&ollama), vec!["qwen3:32b"]);

    let bare = json!([{ "id": "a" }, { "id": "b" }]);
    assert_eq!(parse_model_ids(&bare), vec!["a", "b"]);

    let deduped = json!({ "data": [{ "id": "x" }, { "id": "x" }] });
    assert_eq!(parse_model_ids(&deduped), vec!["x"]);

    assert!(parse_model_ids(&json!({ "unexpected": true })).is_empty());
}

fn canonical_events() -> Vec<SseEvent> {
    vec![
        SseEvent::new(
            "message_start",
            json!({ "type": "message_start", "message": { "id": "msg_1", "model": "gpt-5", "usage": { "input_tokens": 3, "output_tokens": 0 } } }),
        ),
        SseEvent::new(
            "content_block_start",
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        ),
        SseEvent::new(
            "content_block_delta",
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Hello" } }),
        ),
        SseEvent::new(
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }),
        ),
        SseEvent::new(
            "content_block_start",
            json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "tool_use", "id": "call_1", "name": "get_weather", "input": {} } }),
        ),
        SseEvent::new(
            "content_block_delta",
            json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "input_json_delta", "partial_json": "{\"city\":\"Beijing\"}" } }),
        ),
        SseEvent::new(
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 1 }),
        ),
        SseEvent::new(
            "message_delta",
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" }, "usage": { "output_tokens": 4 } }),
        ),
    ]
}

fn encode_all(format: ModelFormat) -> Vec<serde_json::Value> {
    let cfg = model(format, "https://api.openai.com/v1");
    let provider = provider_for(format);
    let mut state = WireState::default();
    let mut out: Vec<SseEvent> = Vec::new();
    for event in canonical_events() {
        out.extend(provider.encode_stream_event(&cfg, &event, &mut state));
    }
    out.extend(provider.encode_stream_done(&cfg, &mut state));
    out.into_iter()
        .map(|event| {
            if let Some(raw) = event.raw {
                json!({ "raw": raw })
            } else {
                event.data
            }
        })
        .collect()
}

#[test]
fn openai_completions_reencodes_canonical_stream_into_chunks() {
    let events = encode_all(ModelFormat::OpenaiCompletions);
    let types: Vec<&str> = events
        .iter()
        .map(|event| {
            event
                .get("object")
                .and_then(|v| v.as_str())
                .unwrap_or("done")
        })
        .collect();

    assert!(types
        .iter()
        .all(|kind| *kind == "chat.completion.chunk" || *kind == "done"));

    assert_eq!(events[0]["choices"][0]["delta"]["role"], "assistant");
    assert_eq!(events[1]["choices"][0]["delta"]["content"], "Hello");
    assert_eq!(
        events[2]["choices"][0]["delta"]["tool_calls"][0]["id"],
        "call_1"
    );
    assert_eq!(
        events[2]["choices"][0]["delta"]["tool_calls"][0]["index"],
        0
    );
    assert_eq!(
        events[3]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
        "{\"city\":\"Beijing\"}"
    );
    assert_eq!(events[4]["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(events[5]["raw"], "[DONE]");
}

#[test]
fn openai_completions_decodes_reasoning_field_as_thinking() {
    let config = model(ModelFormat::OpenaiCompletions, "https://api.openai.com/v1");
    let provider = provider_for(ModelFormat::OpenaiCompletions);
    let mut state = StreamState::new("upstream-model");

    // 上游用 `reasoning` 承载思考（OpenRouter 口径），此前只认 `reasoning_content` 会整段丢掉
    let thinking = provider
        .decode_stream_event(
            &config,
            "",
            &json!({ "choices": [{ "index": 0, "delta": { "reasoning": "let me think" }, "finish_reason": null }] }),
            &mut state,
        )
        .unwrap();
    assert_eq!(thinking[1].data["type"], "content_block_start");
    assert_eq!(thinking[1].data["content_block"]["type"], "thinking");
    assert_eq!(thinking[2].data["delta"]["type"], "thinking_delta");
    assert_eq!(thinking[2].data["delta"]["thinking"], "let me think");

    // 正文到来时收尾思考块、另起文本块
    let text = provider
        .decode_stream_event(
            &config,
            "",
            &json!({ "choices": [{ "index": 0, "delta": { "content": "pong" }, "finish_reason": "stop" }] }),
            &mut state,
        )
        .unwrap();
    let kinds: Vec<&str> = text
        .iter()
        .map(|event| event.data["type"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        kinds,
        vec![
            "content_block_stop",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop"
        ]
    );
    assert_eq!(text[2].data["delta"]["text"], "pong");

    // 非流式同样识别 `reasoning`
    let decoded = provider
        .decode_response(
            &config,
            &json!({
                "id": "gen_1",
                "model": "deepseek/deepseek-v4-flash",
                "choices": [{ "index": 0, "message": { "role": "assistant", "content": "pong", "reasoning": "thinking" }, "finish_reason": "stop" }],
                "usage": { "prompt_tokens": 10, "completion_tokens": 5 }
            }),
        )
        .unwrap();
    assert_eq!(decoded["content"][0]["type"], "thinking");
    assert_eq!(decoded["content"][0]["thinking"], "thinking");
    assert_eq!(decoded["content"][1]["type"], "text");
    assert_eq!(decoded["content"][1]["text"], "pong");
}

#[test]
fn openai_inbound_forwards_usage_opt_in_and_temperature() {
    let provider = provider_for(ModelFormat::OpenaiCompletions);
    let config = model(ModelFormat::OpenaiCompletions, "https://api.openai.com/v1");

    let opted_in = provider
        .decode_request(json!({
            "model": "gpt-4o",
            "messages": [{ "role": "user", "content": "hi" }],
            "stream": true,
            "temperature": 0.3,
            "stream_options": { "include_usage": true }
        }))
        .unwrap();
    assert!(opted_in.body().canonical.include_usage);
    assert_eq!(opted_in.body().temperature, Some(0.3));

    let encoded = provider.encode_request(&config, &opted_in).unwrap();
    assert_eq!(encoded["temperature"], 0.3);
    assert_eq!(encoded["stream_options"]["include_usage"], true);

    // 客户端没要 usage 时不要主动带上，有的上游对多余字段很严格
    let plain = provider
        .decode_request(json!({
            "model": "gpt-4o",
            "messages": [{ "role": "user", "content": "hi" }],
            "stream": true
        }))
        .unwrap();
    assert!(!plain.body().canonical.include_usage);
    let encoded = provider.encode_request(&config, &plain).unwrap();
    assert!(encoded.get("stream_options").is_none());
}

#[test]
fn openai_stream_tail_carries_usage_when_client_opted_in() {
    let provider = provider_for(ModelFormat::OpenaiCompletions);
    let config = model(ModelFormat::OpenaiCompletions, "https://api.openai.com/v1");

    let mut state = WireState {
        include_usage: true,
        response_id: "chatcmpl-1".into(),
        model: "gpt-4o".into(),
        input_tokens: 100,
        output_tokens: 7,
        cache_read_tokens: 40,
        ..WireState::default()
    };
    let events = provider.encode_stream_done(&config, &mut state);
    assert_eq!(events.len(), 2);

    // OpenAI 口径：prompt_tokens 含缓存命中
    let usage = &events[0].data;
    assert_eq!(usage["object"], "chat.completion.chunk");
    assert_eq!(usage["choices"].as_array().unwrap().len(), 0);
    assert_eq!(usage["usage"]["prompt_tokens"], 140);
    assert_eq!(usage["usage"]["completion_tokens"], 7);
    assert_eq!(usage["usage"]["total_tokens"], 147);
    assert_eq!(usage["usage"]["prompt_tokens_details"]["cached_tokens"], 40);
    assert_eq!(events[1].raw.as_deref(), Some("[DONE]"));

    // 没声明 include_usage 时维持原样，只有结束标记
    let mut plain = WireState::default();
    let events = provider.encode_stream_done(&config, &mut plain);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].raw.as_deref(), Some("[DONE]"));
}

#[test]
fn anthropic_passthrough_drops_openai_only_canonical_fields() {
    let canonical = provider_for(ModelFormat::OpenaiCompletions)
        .decode_request(json!({
            "model": "gpt-4o",
            "messages": [{ "role": "user", "content": "hi" }],
            "stream": true,
            "store": false,
            "stream_options": { "include_usage": true }
        }))
        .unwrap();
    assert!(canonical.body().canonical.include_usage);
    assert_eq!(canonical.body().store, Some(false));

    let encoded = provider_for(ModelFormat::AnthropicMessages)
        .encode_request(
            &model(ModelFormat::AnthropicMessages, "https://api.anthropic.com"),
            &canonical,
        )
        .unwrap();
    assert_eq!(encoded["model"], "upstream-model");
    // 规范内部字段与 Anthropic 没有的字段都不能漏出去（Anthropic 会拒收未知键）。
    for key in ["_canonical", "store", "n"] {
        assert!(encoded.get(key).is_none(), "{key} 不该出现在 Anthropic 报文里");
    }
}

/// 完备性守卫：规范请求的每个字段都必须在每个协议的 profile 里出现（映射或显式丢弃）。
///
/// 这里刻意用**不写** `..Default::default()` 的完整字面量构造 `RequestBody`：以后往规范请求里
/// 加字段，这个测试会先因为缺字段编译不过，逼着人回来把协议表补上——「新字段被静默丢掉」不再可能。
#[test]
fn protocol_profiles_cover_every_canonical_field() {
    let body = RequestBody {
        model: "m".into(),
        max_tokens: Some(1),
        // 每个字段都必须给出 Some/非空，否则 serde 的 skip_serializing_if 会把它从字段集合里抹掉。
        system: Some(SystemPrompt::Text("s".into())),
        messages: Vec::new(),
        tools: Some(Vec::new()),
        tool_choice: Some(CanonicalToolChoice::Auto),
        temperature: Some(0.0),
        top_p: Some(0.0),
        stop_sequences: Some(vec!["s".into()]),
        stream: true,
        top_k: Some(1.0),
        store: Some(true),
        metadata: Some(json!({})),
        response_format: Some(CanonicalResponseFormat::Text),
        parallel_tool_calls: Some(true),
        reasoning_effort: Some("low".into()),
        service_tier: Some("auto".into()),
        n: Some(1),
        canonical: CanonicalOnly {
            include_usage: true,
            max_tokens_field: Some(MaxTokensField::MaxTokens),
        },
    };
    let value = serde_json::to_value(&body).expect("规范请求应该能序列化");
    let fields: Vec<&str> = value
        .as_object()
        .expect("规范请求是对象")
        .keys()
        .map(String::as_str)
        .collect();

    for format in ModelFormat::ALL {
        let profile = wire::profile(format);
        for field in &fields {
            assert!(
                profile.rules.iter().any(|rule| rule.field == *field),
                "{} 的协议表漏了规范字段 {field}",
                format.as_str()
            );
        }
        for rule in profile.rules {
            assert!(
                fields.contains(&rule.field),
                "{} 的协议表里有规范里不存在的字段 {}",
                format.as_str(),
                rule.field
            );
        }
    }
}

/// 块状态机不变式：载荷只进匹配的块，索引严格递增，块收尾用它自己的索引。
#[test]
fn block_normalizer_keeps_payloads_in_their_own_block() {
    let mut normalizer = BlockNormalizer::default();
    let mut events = normalizer.thinking("先想一下");
    events.extend(normalizer.text("然后正文"));
    normalizer.tool_start(0, "toolu_1", "lookup");
    events.extend(normalizer.tool_args(0, "{\"a\":1}"));
    // 工具块之后又来思考：必须新开一个 thinking 块，不能写进工具块
    events.extend(normalizer.thinking("再想一下"));
    events.extend(normalizer.close());

    let mut opens: Vec<i64> = Vec::new();
    let mut deltas: Vec<(i64, &str)> = Vec::new();
    for event in &events {
        match event.data.get("type").and_then(Value::as_str) {
            Some("content_block_start") => opens.push(event.data["index"].as_i64().unwrap()),
            Some("content_block_delta") => deltas.push((
                event.data["index"].as_i64().unwrap(),
                event.data["delta"]["type"].as_str().unwrap(),
            )),
            _ => {}
        }
    }
    assert_eq!(opens, vec![0, 1, 2, 3]);
    assert_eq!(
        deltas,
        vec![
            (0, "thinking_delta"),
            (1, "text_delta"),
            (2, "input_json_delta"),
            (3, "thinking_delta"),
        ]
    );
    let stops = events
        .iter()
        .filter(|event| event.data["type"] == "content_block_stop")
        .count();
    assert_eq!(stops, 4, "每个块都要收尾");
}

/// 并行工具调用：参数按序号缓冲，绝不串到别的工具块上（I4）。
#[test]
fn block_normalizer_serializes_parallel_tool_arguments() {
    let mut normalizer = BlockNormalizer::default();
    normalizer.tool_start(0, "toolu_a", "first");
    normalizer.tool_start(1, "toolu_b", "second");

    let mut events = normalizer.tool_args(0, "{\"one\"");
    events.extend(normalizer.tool_args(1, "{\"two\""));
    events.extend(normalizer.tool_args(0, ":1}"));
    events.extend(normalizer.tool_args(1, ":2}"));
    events.extend(normalizer.close());

    let mut calls: Vec<(String, String)> = Vec::new();
    for event in &events {
        match (
            event.data.get("type").and_then(Value::as_str),
            event.data.pointer("/delta/type").and_then(Value::as_str),
        ) {
            (Some("content_block_start"), _) => calls.push((
                event.data["content_block"]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                String::new(),
            )),
            (Some("content_block_delta"), Some("input_json_delta")) => calls
                .last_mut()
                .expect("参数前面一定有块")
                .1
                .push_str(event.data["delta"]["partial_json"].as_str().unwrap()),
            _ => {}
        }
    }
    assert_eq!(
        calls,
        vec![
            ("toolu_a".to_string(), "{\"one\":1}".to_string()),
            ("toolu_b".to_string(), "{\"two\":2}".to_string()),
        ]
    );

    // 上游没先声明就直接给参数：也只开一个块，参数不丢
    let mut normalizer = BlockNormalizer::default();
    let mut events = normalizer.tool_args(0, "{\"a\"");
    events.extend(normalizer.tool_args(0, ":1}"));
    events.extend(normalizer.close());
    let starts = events
        .iter()
        .filter(|event| event.data["type"] == "content_block_start")
        .count();
    let partial: String = events
        .iter()
        .filter_map(|event| event.data.pointer("/delta/partial_json"))
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(starts, 1, "同一个工具不能开两个块");
    assert_eq!(partial, "{\"a\":1}");
}

/// 流结束判定：只有思考、以及没发结束事件的流，都不该再被记成成功。
#[test]
fn stream_verdict_flags_reasoning_only_and_truncated() {
    let config = model(ModelFormat::OpenaiCompletions, "https://api.openai.com/v1");
    let provider = provider_for(ModelFormat::OpenaiCompletions);

    // 实测形状（usage_detail_id=8）：上游只发 reasoning，finish_reason 仍是 stop
    let mut state = StreamState::new("upstream-model");
    provider
        .decode_stream_event(
            &config,
            "",
            &json!({ "choices": [{ "index": 0, "delta": { "reasoning": "想完就停了" }, "finish_reason": null }] }),
            &mut state,
        )
        .unwrap();
    provider
        .decode_stream_event(
            &config,
            "",
            &json!({ "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }] }),
            &mut state,
        )
        .unwrap();
    assert_eq!(state.verdict(None), StreamVerdict::EmptyReasoningOnly);
    assert!(!state.verdict(None).outcome().0, "这种收尾应记成失败");

    // 有正文 → 正常
    let mut state = StreamState::new("upstream-model");
    provider
        .decode_stream_event(
            &config,
            "",
            &json!({ "choices": [{ "index": 0, "delta": { "content": "正文" }, "finish_reason": "stop" }] }),
            &mut state,
        )
        .unwrap();
    assert_eq!(state.verdict(None), StreamVerdict::Ok);

    // 上游没发结束事件就断了：网关补的收尾不算上游结束 → 截断
    let mut state = StreamState::new("upstream-model");
    provider
        .decode_stream_event(
            &config,
            "",
            &json!({ "choices": [{ "index": 0, "delta": { "content": "半句" }, "finish_reason": null }] }),
            &mut state,
        )
        .unwrap();
    state.finish("end_turn");
    assert_eq!(state.verdict(None), StreamVerdict::Truncated);

    // 只有工具调用的流是正常的
    let mut state = StreamState::new("upstream-model");
    state.tool_start(0, "toolu_1", "f");
    state.tool_args(0, "{}");
    state.upstream_ended = true;
    state.finish("tool_use");
    assert_eq!(state.verdict(None), StreamVerdict::Ok);

    // 传输/解码错误优先
    assert_eq!(
        state.verdict(Some("上游断开".into())),
        StreamVerdict::UpstreamError("上游断开".into())
    );

    // 流内错误分片（OpenAI 系没有 choices、只有一个 error 对象）→ 转发给客户端并记失败
    let mut state = StreamState::new("upstream-model");
    let events = provider
        .decode_stream_event(
            &config,
            "",
            &json!({ "error": { "message": "rate limited", "type": "server_error" } }),
            &mut state,
        )
        .unwrap();
    assert_eq!(events[0].event, "error");
    assert_eq!(
        state.verdict(None),
        StreamVerdict::UpstreamError("rate limited".into())
    );
}

/// 一张规范请求在三个协议上的落地（对照官方文档的字段口径）。
#[test]
fn canonical_fields_are_forwarded_per_protocol() {
    let inbound = provider_for(ModelFormat::OpenaiCompletions)
        .decode_request(json!({
            "model": "gpt-4o",
            "messages": [{ "role": "user", "content": "hi" }],
            "stream": true,
            "temperature": 0.2,
            "top_p": 0.9,
            "service_tier": "priority",
            "store": true,
            "metadata": { "user_id": "u1" },
            "response_format": {
                "type": "json_schema",
                "json_schema": { "name": "r", "schema": { "type": "object" }, "strict": true }
            },
            "tool_choice": { "type": "function", "function": { "name": "lookup" } },
            "tools": [{
                "type": "function",
                "function": { "name": "lookup", "parameters": { "type": "object" } }
            }],
            "parallel_tool_calls": false,
            "reasoning_effort": "high",
            "stop": ["END"],
            "max_completion_tokens": 256,
            "stream_options": { "include_usage": true }
        }))
        .unwrap();

    // Chat Completions 上游：同名透传，且 max_completion_tokens 保真（不是改写成弃用的 max_tokens）
    let encoded = inbound.raw();
    assert_eq!(encoded["max_tokens"], 256);
    let encoded = provider_for(ModelFormat::OpenaiCompletions)
        .encode_request(
            &model(ModelFormat::OpenaiCompletions, "https://api.openai.com/v1"),
            &inbound,
        )
        .unwrap();
    assert_eq!(encoded["temperature"], 0.2);
    assert_eq!(encoded["top_p"], 0.9);
    assert_eq!(encoded["service_tier"], "priority");
    assert_eq!(encoded["store"], true);
    assert_eq!(encoded["metadata"]["user_id"], "u1");
    assert_eq!(encoded["parallel_tool_calls"], false);
    assert_eq!(encoded["reasoning_effort"], "high");
    assert_eq!(encoded["response_format"]["type"], "json_schema");
    assert_eq!(encoded["tool_choice"]["function"]["name"], "lookup");
    assert_eq!(encoded["stop"][0], "END");
    assert_eq!(encoded["max_completion_tokens"], 256);
    assert!(encoded.get("max_tokens").is_none());
    assert_eq!(encoded["stream_options"]["include_usage"], true);
    assert!(encoded.get("_canonical").is_none());

    // Responses 上游：effort / 输出格式变形到 reasoning、text 里；stop 与 top_k 没有对应参数
    let encoded = provider_for(ModelFormat::OpenaiResponses)
        .encode_request(
            &model(ModelFormat::OpenaiResponses, "https://api.openai.com/v1"),
            &inbound,
        )
        .unwrap();
    assert_eq!(encoded["max_output_tokens"], 256);
    assert_eq!(encoded["reasoning"]["effort"], "high");
    assert_eq!(encoded["text"]["format"]["type"], "json_schema");
    assert_eq!(encoded["text"]["format"]["name"], "r");
    assert_eq!(encoded["text"]["format"]["strict"], true);
    assert_eq!(encoded["tool_choice"], json!({ "type": "function", "name": "lookup" }));
    assert_eq!(encoded["store"], true);
    assert_eq!(encoded["parallel_tool_calls"], false);
    assert_eq!(encoded["temperature"], 0.2);
    // A5 的另一半：OpenAI 系协议之间词表相同，照旧原样带过去（只有 Anthropic 上游丢弃）。
    assert_eq!(encoded["service_tier"], "priority");
    for key in [
        "response_format",
        "reasoning_effort",
        "stop",
        "stop_sequences",
        "top_k",
        "_canonical",
    ] {
        assert!(encoded.get(key).is_none(), "{key} 不该出现在 Responses 报文里");
    }

    // Anthropic 上游：effort / format 归到 output_config，并行开关取反，metadata 只留 user_id
    let encoded = provider_for(ModelFormat::AnthropicMessages)
        .encode_request(
            &model(ModelFormat::AnthropicMessages, "https://api.anthropic.com"),
            &inbound,
        )
        .unwrap();
    assert_eq!(encoded["output_config"]["effort"], "high");
    assert_eq!(encoded["output_config"]["format"]["type"], "json_schema");
    assert_eq!(encoded["output_config"]["format"]["schema"]["type"], "object");
    assert_eq!(encoded["tool_choice"]["type"], "tool");
    assert_eq!(encoded["tool_choice"]["name"], "lookup");
    assert_eq!(encoded["tool_choice"]["disable_parallel_tool_use"], true);
    assert_eq!(encoded["metadata"]["user_id"], "u1");
    assert_eq!(encoded["stop_sequences"][0], "END");
    for key in [
        "response_format",
        "reasoning_effort",
        "parallel_tool_calls",
        "store",
        "top_k",
        // A5：`service_tier` 的取值是各家自己的词表（这里入站是 OpenAI 的 "priority"），
        // 原样发给 Anthropic 会被按非法取值 400 掉整个请求。
        "service_tier",
        "_canonical",
    ] {
        assert!(encoded.get(key).is_none(), "{key} 不该出现在 Anthropic 报文里");
    }
}

/// A5 的代价写成测试，免得日后被当 bug 查：`Dropped` 在 Anthropic **本协议**的编码路径上
/// 同样生效，所以 Anthropic 客户端自己带的 `service_tier` 也会被删（跨协议重建与同协议补写
/// 共用同一个 `encode_request`，不像两个 OpenAI 协议有独立的直通路径）。只要该参数在
/// Anthropic 侧的存在性与取值词表还没被实测确认，丢它换「绝不 400」就是认下的取舍
/// （见 `docs/forwarding.md` §8 批 4）。日后实测确认后，改法是把 `Slot::Dropped` 换成
/// 值映射并改写本测试——**不要**改回 `Same`。
#[test]
fn anthropic_clients_lose_their_own_service_tier_until_the_parameter_is_verified() {
    let inbound = provider_for(ModelFormat::AnthropicMessages)
        .decode_request(json!({
            "model": "claude-sonnet-5",
            "max_tokens": 64,
            "messages": [{ "role": "user", "content": "hi" }],
            "service_tier": "auto",
        }))
        .unwrap();
    let encoded = provider_for(ModelFormat::AnthropicMessages)
        .encode_request(
            &model(ModelFormat::AnthropicMessages, "https://api.anthropic.com"),
            &inbound,
        )
        .unwrap();
    assert!(
        encoded.get("service_tier").is_none(),
        "A5 之下连 Anthropic 自带的 service_tier 也会被丢弃"
    );
    // 代价范围就这一个键：同一条报文里的其余字段照常落地。
    assert_eq!(encoded["max_tokens"], 64);
    assert_eq!(encoded["messages"].as_array().map(Vec::len), Some(1));
}

/// Anthropic 入站的隐藏字段跨协议不丢（A2）：output_config.format → response_format、
/// thinking 预算 → reasoning_effort（有损映射）、`{type:"any"}` 的 tool_choice → Responses
/// 不再被 `_ => None` 静默丢弃（A3）。
#[test]
fn anthropic_inbound_hidden_fields_survive_cross_protocol() {
    // Anthropic 入站：结构化输出 + thinking 预算 + 强制任意工具
    let inbound = provider_for(ModelFormat::AnthropicMessages)
        .decode_request(json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 1024,
            "messages": [{ "role": "user", "content": "hi" }],
            "output_config": {
                "effort": "medium",
                "format": { "type": "json_schema", "schema": { "type": "object" } }
            },
            "thinking": { "type": "enabled", "budget_tokens": 4096 },
            "tool_choice": { "type": "any" }
        }))
        .unwrap();

    // 抬升结果先验一遍：format/effort/thinking 都在规范字段上
    assert_eq!(inbound.body().reasoning_effort.as_deref(), Some("medium"));
    match &inbound.body().response_format {
        Some(CanonicalResponseFormat::JsonSchema { schema, .. }) => {
            assert_eq!(schema["type"], "object");
        }
        other => panic!("response_format 应该是 JsonSchema，实际 {other:?}"),
    }
    // effort 显式档位优先于 thinking 预算（避免两套口径打架）

    // → Responses 出站：format 进 text.format，tool_choice 的 any → required
    let encoded = provider_for(ModelFormat::OpenaiResponses)
        .encode_request(
            &model(ModelFormat::OpenaiResponses, "https://api.openai.com/v1"),
            &inbound,
        )
        .unwrap();
    assert_eq!(encoded["text"]["format"]["type"], "json_schema");
    assert_eq!(encoded["text"]["format"]["schema"]["type"], "object");
    assert_eq!(encoded["tool_choice"], json!("required"));

    // → Completions 出站：any → required，format 原形状透传
    let encoded = provider_for(ModelFormat::OpenaiCompletions)
        .encode_request(
            &model(ModelFormat::OpenaiCompletions, "https://api.openai.com/v1"),
            &inbound,
        )
        .unwrap();
    assert_eq!(encoded["tool_choice"], "required");
    assert_eq!(encoded["response_format"]["type"], "json_schema");

    // thinking 预算式（没有 output_config.effort）→ 档位映射：4k → low
    let inbound = provider_for(ModelFormat::AnthropicMessages)
        .decode_request(json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 1024,
            "messages": [{ "role": "user", "content": "hi" }],
            "thinking": { "type": "enabled", "budget_tokens": 4096 }
        }))
        .unwrap();
    assert_eq!(inbound.body().reasoning_effort.as_deref(), Some("low"));
    // 跨协议到 Responses 时档位落在 reasoning.effort
    let encoded = provider_for(ModelFormat::OpenaiResponses)
        .encode_request(
            &model(ModelFormat::OpenaiResponses, "https://api.openai.com/v1"),
            &inbound,
        )
        .unwrap();
    assert_eq!(encoded["reasoning"]["effort"], "low");

    // 同协议（Anthropic → Anthropic）：thinking / output_config 原样保留（不删、不叠档位）
    let client = json!({
        "model": "claude-sonnet-4-5",
        "max_tokens": 1024,
        "messages": [{ "role": "user", "content": "hi" }],
        "thinking": { "type": "enabled", "budget_tokens": 4096 },
        "tool_choice": { "type": "auto", "disable_parallel_tool_use": true }
    });
    let inbound = provider_for(ModelFormat::AnthropicMessages)
        .decode_request(client.clone())
        .unwrap();
    let encoded = provider_for(ModelFormat::AnthropicMessages)
        .encode_request(
            &model(ModelFormat::AnthropicMessages, "https://api.anthropic.com"),
            &inbound,
        )
        .unwrap();
    assert_eq!(
        encoded["thinking"],
        json!({ "type": "enabled", "budget_tokens": 4096 })
    );
    assert!(encoded.get("output_config").is_none());
    assert_eq!(
        encoded["tool_choice"],
        json!({ "type": "auto", "disable_parallel_tool_use": true })
    );
}

/// http(s) 图片地址跨协议不再静默丢块（A4）：Completions / Responses 入站的 image_url
/// 收成 url source，两条出站路径都能还原。
#[test]
fn http_image_urls_survive_cross_protocol() {
    let url = "https://example.com/cat.png";
    let inbound = provider_for(ModelFormat::OpenaiCompletions)
        .decode_request(json!({
            "model": "gpt-4o",
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "看图" },
                    { "type": "image_url", "image_url": { "url": url } }
                ]
            }]
        }))
        .unwrap();
    assert_eq!(
        inbound.raw()["messages"][0]["content"][1]["source"],
        json!({ "type": "url", "url": url })
    );

    // → Anthropic 出站：url source 原样落到 image 块
    let encoded = provider_for(ModelFormat::AnthropicMessages)
        .encode_request(
            &model(ModelFormat::AnthropicMessages, "https://api.anthropic.com"),
            &inbound,
        )
        .unwrap();
    assert_eq!(encoded["messages"][0]["content"][1]["source"]["type"], "url");
    assert_eq!(encoded["messages"][0]["content"][1]["source"]["url"], url);

    // → Responses 出站：url source 还原成 input_image
    let encoded = provider_for(ModelFormat::OpenaiResponses)
        .encode_request(
            &model(ModelFormat::OpenaiResponses, "https://api.openai.com/v1"),
            &inbound,
        )
        .unwrap();
    assert_eq!(encoded["input"][0]["content"][1]["type"], "input_image");
    assert_eq!(encoded["input"][0]["content"][1]["image_url"], url);

    // Responses 入站同样收 url source
    let inbound = provider_for(ModelFormat::OpenaiResponses)
        .decode_request(json!({
            "model": "gpt-4o",
            "input": [{
                "role": "user",
                "content": [{ "type": "input_image", "image_url": url }]
            }]
        }))
        .unwrap();
    assert_eq!(
        inbound.raw()["messages"][0]["content"][0]["source"],
        json!({ "type": "url", "url": url })
    );
}

/// 规范级校验（A6）：n = 0 与 n > 1 都拒绝；非法的 tool_choice / response_format 形状
/// 在入站反序列化时就报错，而不是转发到上游才炸（A3 的类型化收口）。
#[test]
fn invalid_n_and_malformed_shapes_are_rejected_at_inbound() {
    // n = 0 语义非法
    let parsed = request(json!({
        "model": "m",
        "messages": [{ "role": "user", "content": "hi" }],
        "n": 0
    }));
    assert!(parsed.validate().is_err());

    // n > 1 多候选
    let parsed = request(json!({
        "model": "m",
        "messages": [{ "role": "user", "content": "hi" }],
        "n": 2
    }));
    assert!(parsed.validate().is_err());

    // 显式 null 视为「未指定」，不能 400
    let parsed = request(json!({
        "model": "m",
        "messages": [{ "role": "user", "content": "hi" }],
        "tool_choice": null,
        "response_format": null
    }));
    assert!(parsed.validate().is_ok());

    // tool_choice 未知臂 / 嵌套残缺
    assert!(CanonicalRequest::parse(json!({
        "model": "m",
        "messages": [{ "role": "user", "content": "hi" }],
        "tool_choice": { "type": "any" }
    }))
    .is_err());
    assert!(provider_for(ModelFormat::OpenaiCompletions)
        .decode_request(json!({
            "model": "gpt-4o",
            "messages": [{ "role": "user", "content": "hi" }],
            "tool_choice": { "type": "function" }
        }))
        .is_err());

    // response_format 未知 type
    assert!(provider_for(ModelFormat::OpenaiCompletions)
        .decode_request(json!({
            "model": "gpt-4o",
            "messages": [{ "role": "user", "content": "hi" }],
            "response_format": { "type": "yaml" }
        }))
        .is_err());
}

/// 取某协议的同协议直通报文（测试辅助）。
fn passthrough(format: ModelFormat, request: &CanonicalRequest) -> Value {
    provider_for(format)
        .encode_request_passthrough(&model(format, "https://example.com/v1"), request)
        .expect("直通编码不该失败")
        .expect("该协议应该有直通快路")
}

/// 同协议免转换直通：客户端原文为底，协议扩展键、字段名、消息结构原样带给上游。
#[test]
fn same_protocol_passthrough_keeps_the_client_payload_intact() {
    let client = json!({
        "model": "gpt-4o",
        "messages": [
            { "role": "system", "content": "be brief" },
            { "role": "user", "name": "alice", "content": "hi" }
        ],
        "stop": "END",
        "logit_bias": { "50256": -100 },
        "max_completion_tokens": 256,
        "provider": { "order": ["openai"] },
        "stream_options": { "include_usage": true },
        "stream": false
    });
    let request = provider_for(ModelFormat::OpenaiCompletions)
        .decode_request(client.clone())
        .expect("Completions 请求应该能解析")
        .retain_client_raw(client);

    let encoded = passthrough(ModelFormat::OpenaiCompletions, &request);

    // 只换模型名
    assert_eq!(encoded["model"], "upstream-model");
    // 重建路径会丢的键、会被改写的字段名，直通全部保真
    assert_eq!(encoded["stop"], "END");
    assert_eq!(encoded["logit_bias"]["50256"], json!(-100));
    assert_eq!(encoded["provider"]["order"][0], "openai");
    assert_eq!(encoded["max_completion_tokens"], json!(256));
    assert!(encoded.get("max_tokens").is_none());
    assert_eq!(encoded["stream_options"]["include_usage"], true);
    assert_eq!(encoded["messages"][0]["role"], "system");
    assert_eq!(encoded["messages"][1]["name"], "alice");
    assert!(encoded.get("_canonical").is_none());
}

/// 网关选报文：同协议走免转换快路（客户端原文保真），跨协议仍走规范层重建。
#[test]
fn gateway_keeps_same_protocol_payloads_and_rebuilds_across_protocols() {
    use crate::gateway::server::encode_upstream_request;

    let client = json!({
        "model": "gpt-4o",
        "messages": [{ "role": "user", "content": "hi" }],
        "logit_bias": { "50256": -100 }
    });
    let request = provider_for(ModelFormat::OpenaiCompletions)
        .decode_request(client.clone())
        .expect("Completions 请求应该能解析")
        .retain_client_raw(client);
    let config = model(ModelFormat::OpenaiCompletions, "https://api.openai.com/v1");

    // 入站 Completions → 上游 Completions：客户端原文 + 换模型名
    let same = encode_upstream_request(ModelFormat::OpenaiCompletions, &config, &request).unwrap();
    assert_eq!(same["model"], "upstream-model");
    assert_eq!(same["logit_bias"]["50256"], json!(-100));

    // 入站 Responses → 上游 Completions：跨协议，仍走规范层重建（私有扩展键落在规范之外，带不过去）
    let cross = encode_upstream_request(ModelFormat::OpenaiResponses, &config, &request).unwrap();
    assert_eq!(cross["model"], "upstream-model");
    assert_eq!(cross["messages"][0]["content"], "hi");
    assert!(cross.get("logit_bias").is_none());
}

/// 系统提示词注入在直通路径上也要落地：过滤器改过 system 才重写，没改过一字不动。
#[test]
fn passthrough_rewrites_system_only_after_a_filter_changed_it() {
    let client = json!({
        "model": "gpt-4o",
        "messages": [
            { "role": "system", "content": "旧提示" },
            { "role": "developer", "content": "老规矩" },
            { "role": "user", "content": "hi" }
        ]
    });
    let request = provider_for(ModelFormat::OpenaiCompletions)
        .decode_request(client.clone())
        .expect("Completions 请求应该能解析")
        .retain_client_raw(client);

    // 没有过滤器：客户端的多条 system / developer 消息原样保留
    // （重建路径会把它们并成一条并挪到队首）。
    let untouched = passthrough(ModelFormat::OpenaiCompletions, &request);
    let messages = untouched["messages"].as_array().expect("messages 是数组");
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["content"], "旧提示");
    assert_eq!(messages[1]["role"], "developer");

    // 有过滤器注入：system 由网关接管，合并成一条放在队首。
    let filtered = crate::filters::apply(
        &[filter(
            true,
            FilterRule::SystemPrompt {
                mode: PromptMode::Append,
                text: "新提示".into(),
            },
        )],
        request,
    )
    .expect("过滤器套用不该失败");
    assert!(filtered.is_dirty("system"));

    let payload = passthrough(ModelFormat::OpenaiCompletions, &filtered);
    let messages = payload["messages"].as_array().expect("messages 是数组");
    assert_eq!(messages.len(), 2, "system / developer 合并成一条 system");
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "旧提示\n老规矩\n新提示");
    assert_eq!(messages[1]["role"], "user");
}

/// Responses 同协议直通：多轮会话指针与客户端扩展键不再被丢掉。
#[test]
fn responses_passthrough_keeps_client_extensions() {
    let client = json!({
        "model": "gpt-5",
        "input": "hi",
        "previous_response_id": "resp_abc",
        "include": ["reasoning.encrypted_content"],
        "truncation": "auto",
        "store": false
    });
    let request = provider_for(ModelFormat::OpenaiResponses)
        .decode_request(client.clone())
        .expect("Responses 请求应该能解析")
        .retain_client_raw(client);

    let encoded = passthrough(ModelFormat::OpenaiResponses, &request);
    assert_eq!(encoded["model"], "upstream-model");
    assert_eq!(encoded["previous_response_id"], "resp_abc");
    assert_eq!(encoded["include"][0], "reasoning.encrypted_content");
    assert_eq!(encoded["truncation"], "auto");
    assert_eq!(encoded["input"], "hi");
    assert_eq!(
        encoded["max_output_tokens"],
        json!(crate::domain::model::DEFAULT_MAX_TOKENS)
    );
    assert!(encoded.get("_canonical").is_none());
}

/// 没有保留客户端原文（内部构造的请求）时快路不参与，也不会有协议提供快路给非本协议形状的报文。
#[test]
fn passthrough_falls_back_without_a_retained_client_payload() {
    let request = request(json!({ "model": "m", "messages": [], "max_tokens": 8 }));
    for format in ModelFormat::ALL {
        assert!(
            provider_for(format)
                .encode_request_passthrough(&model(format, "https://example.com/v1"), &request)
                .expect("快路判定不该失败")
                .is_none(),
            "{} 在没有客户端原文时不该给出直通报文",
            format.as_str()
        );
    }
}

/// 停止原因双向表（文档里的取值都要能对上）。
#[test]
fn stop_reasons_round_trip_per_protocol() {
    assert_eq!(wire::stop_reason_from_finish_reason(Some("stop")), "end_turn");
    assert_eq!(
        wire::stop_reason_from_finish_reason(Some("length")),
        "max_tokens"
    );
    assert_eq!(
        wire::stop_reason_from_finish_reason(Some("tool_calls")),
        "tool_use"
    );
    assert_eq!(
        wire::stop_reason_from_finish_reason(Some("content_filter")),
        "refusal"
    );
    assert_eq!(wire::stop_reason_from_finish_reason(None), "end_turn");

    assert_eq!(wire::finish_reason(Some("end_turn")), "stop");
    assert_eq!(wire::finish_reason(Some("max_tokens")), "length");
    assert_eq!(wire::finish_reason(Some("tool_use")), "tool_calls");
    assert_eq!(wire::finish_reason(Some("refusal")), "content_filter");
    assert_eq!(wire::finish_reason(Some("stop_sequence")), "stop");
    assert_eq!(wire::finish_reason(Some("pause_turn")), "stop");

    assert_eq!(
        wire::stop_reason_from_response(Some("completed"), true),
        "tool_use"
    );
    assert_eq!(
        wire::stop_reason_from_response(Some("completed"), false),
        "end_turn"
    );
    assert_eq!(
        wire::stop_reason_from_response(Some("incomplete"), false),
        "max_tokens"
    );
    assert_eq!(
        wire::response_status(Some("max_tokens")),
        ("incomplete", Some("max_tokens"))
    );
    assert_eq!(wire::response_status(Some("end_turn")), ("completed", None));

    assert_eq!(
        wire::stop_reason_from_anthropic(Some("pause_turn")),
        "pause_turn"
    );
    assert_eq!(wire::stop_reason_from_anthropic(Some("refusal")), "refusal");
    assert_eq!(wire::stop_reason_from_anthropic(None), "end_turn");
}

#[test]
fn openai_responses_reencodes_canonical_stream_into_events() {
    let events = encode_all(ModelFormat::OpenaiResponses);
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|event| event.get("type").and_then(|v| v.as_str()))
        .collect();

    assert!(kinds.contains(&"response.created"));
    assert!(kinds.contains(&"response.output_item.added"));
    assert!(kinds.contains(&"response.content_part.added"));
    assert!(kinds.contains(&"response.output_text.delta"));
    assert!(kinds.contains(&"response.output_text.done"));
    assert!(kinds.contains(&"response.function_call_arguments.delta"));
    assert!(kinds.contains(&"response.completed"));

    let created = events
        .iter()
        .find(|event| event["type"] == "response.created")
        .unwrap();
    assert_eq!(created["response"]["model"], "gpt-5");

    let delta = events
        .iter()
        .find(|event| event["type"] == "response.output_text.delta")
        .unwrap();
    assert_eq!(delta["delta"], "Hello");

    let completed = events
        .iter()
        .find(|event| event["type"] == "response.completed")
        .unwrap();
    assert_eq!(completed["response"]["usage"]["input_tokens"], 3);
    assert_eq!(completed["response"]["usage"]["output_tokens"], 4);
}

#[test]
fn openai_inbound_requests_are_lifted_to_canonical() {
    let chat = provider_for(ModelFormat::OpenaiCompletions)
        .decode_request(json!({
            "model": "gpt-4o",
            "max_tokens": 100,
            "messages": [
                { "role": "system", "content": "Be terse." },
                { "role": "user", "content": "hi" },
                { "role": "assistant", "tool_calls": [{ "id": "call_1", "type": "function", "function": { "name": "f", "arguments": "{\"a\":1}" } }] },
                { "role": "tool", "tool_call_id": "call_1", "content": "ok" }
            ],
            "tools": [{ "type": "function", "function": { "name": "f", "parameters": { "type": "object" } } }]
        }))
        .unwrap();

    let body = chat.body();
    assert_eq!(body.max_tokens, Some(100));
    assert_eq!(body.system.as_ref().unwrap().plain_text(), "Be terse.");
    assert_eq!(body.messages.len(), 3);
    assert_eq!(body.messages[0].role, "user");
    assert_eq!(body.messages[1].role, "assistant");
    assert_eq!(body.messages[2].role, "user");
    assert_eq!(body.messages[2].content.blocks()[0].kind, "tool_result");
    assert_eq!(body.tools.as_ref().unwrap()[0].name, "f");

    let responses = provider_for(ModelFormat::OpenaiResponses)
        .decode_request(json!({
            "model": "gpt-5",
            "instructions": "Be terse.",
            "input": [
                { "role": "user", "content": [{ "type": "input_text", "text": "hi" }] },
                { "type": "function_call", "call_id": "call_9", "name": "f", "arguments": "{}" },
                { "type": "function_call_output", "call_id": "call_9", "output": "done" }
            ]
        }))
        .unwrap();

    let body = responses.body();
    assert_eq!(body.system.as_ref().unwrap().plain_text(), "Be terse.");
    assert_eq!(body.messages.len(), 3);
    assert_eq!(body.messages[1].content.blocks()[0].kind, "tool_use");
    assert_eq!(body.messages[2].content.blocks()[0].kind, "tool_result");
}

#[test]
fn canonical_responses_are_re_encoded_for_openai_clients() {
    let canonical = json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "model": "gpt-4o",
        "content": [
            { "type": "text", "text": "hi" },
            { "type": "tool_use", "id": "call_1", "name": "f", "input": { "a": 1 } }
        ],
        "stop_reason": "tool_use",
        "usage": { "input_tokens": 7, "output_tokens": 2 }
    });

    let chat = provider_for(ModelFormat::OpenaiCompletions)
        .encode_response(
            &model(ModelFormat::OpenaiCompletions, "https://api.openai.com/v1"),
            &canonical,
        )
        .unwrap();
    assert_eq!(chat["choices"][0]["message"]["content"], "hi");
    assert_eq!(
        chat["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
        "{\"a\":1}"
    );
    assert_eq!(chat["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(chat["usage"]["total_tokens"], 9);

    let responses = provider_for(ModelFormat::OpenaiResponses)
        .encode_response(
            &model(ModelFormat::OpenaiResponses, "https://api.openai.com/v1"),
            &canonical,
        )
        .unwrap();
    let types: Vec<&str> = responses["output"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["type"].as_str().unwrap())
        .collect();
    assert_eq!(types, vec!["message", "function_call"]);
    assert_eq!(responses["status"], "completed");
}

/// 全局设置库整个测试进程只有一份（`settings::init` 的 OnceLock），而 `init` 是
/// 「读盘 → 覆盖内存里的设置 → 落盘」。两个用例并发跑时，B 的读盘可能落在 A 落盘之前，
/// B 随后的落盘就把 A 的写入抹掉——实测表现为 `applied` 绑定凭空消失、`sqlite_persistence_round_trips`
/// 偶发失败。凡是 init/mutate 全局设置的用例都拿这把锁串起来（纯测试隔离，
/// 生产侧 `settings::init` 只在启动时调一次，不存在这个交错）。
static SETTINGS_DB_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_settings_db() -> std::sync::MutexGuard<'static, ()> {
    SETTINGS_DB_GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn sqlite_persistence_round_trips() {
    use crate::domain::app::AppKind;
    use crate::domain::model::ModelInput;
    use crate::usage::{self, UsageRecord};
    use crate::{events, settings, updates};

    let _settings_db = lock_settings_db();
    let dir = std::env::temp_dir().join(format!("ai-start-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    settings::init(&dir).expect("database should initialize");

    let seeded = settings::snapshot();
    // 空库启动：示例模型（seed_default_models）已在 1a28cb0 移除，模型由用户自己添加。
    assert!(seeded.models.is_empty());
    assert_eq!(seeded.active_model_id, None);
    // 三个新设置项的默认值：开机启动默认开，代理默认关、地址为空（直连）。
    assert!(seeded.launch_at_login);
    assert!(!seeded.proxy_enabled);
    assert!(seeded.proxy_url.is_empty());

    let saved = settings::mutate(|store| {
        store.upsert(ModelInput {
            id: None,
            name: "Temp".into(),
            format: ModelFormat::OpenaiResponses,
            base_url: "https://example.com/v1".into(),
            api_key: "k".into(),
            model: "temp-model".into(),
            supports_1m: true,
        })
    })
    .expect("model should save");
    // 空库里的第一个模型就是 1 号（不再有预置模型占位）
    assert_eq!(saved.id, 1);

    settings::mutate(|store| {
        store.applied.insert("claude-desktop".into(), saved.id);
    })
    .expect("binding should save");

    let reloaded = settings::snapshot();
    assert_eq!(reloaded.applied.get("claude-desktop"), Some(&saved.id));
    assert_eq!(
        reloaded
            .applied_model(AppKind::ClaudeDesktop)
            .map(|model| model.model.as_str()),
        Some("temp-model")
    );

    let (timestamp, date) = usage::current_timestamp();
    usage::submit(
        &UsageRecord {
            id: 0,
            timestamp: timestamp.clone(),
            date: date.clone(),
            model_name: "Temp".into(),
            served_by: "Temp".into(),
            source_app: "claude-desktop".into(),
            upstream_url: "https://example.com/v1/responses".into(),
            upstream_model: "temp-model".into(),
            proxied: true,
            inbound_protocol: "anthropic-messages".into(),
            upstream_protocol: "openai-responses".into(),
            input_tokens: 10,
            output_tokens: 5,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            duration_ms: 12,
            ok: true,
            failover: false,
            error: None,
        },
        None,
    );

    // 落库已改为异步投递：读之前先等写线程排空。
    crate::db::flush();
    let records = usage::recent(10);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].total_tokens(), 15);
    assert_eq!(records[0].source_app, "claude-desktop");
    assert_eq!(
        records[0].upstream_url,
        "https://example.com/v1/responses"
    );
    assert_eq!(records[0].upstream_model, "temp-model");
    assert!(records[0].proxied, "经代理出站的标记要能落库读回");
    let summary = usage::summary(365);
    assert_eq!(summary.total_requests, 1);
    assert_eq!(summary.today_tokens, 15);

    // 详情页按 id 单独取记录
    assert_eq!(
        usage::find(records[0].id).expect("record should load").id,
        records[0].id
    );
    assert!(usage::find(999_999).is_none());

    // 列表页分页：总数 + 倒序 + 越界空页
    let first_page = usage::page(0, 10);
    assert_eq!(first_page.total, 1);
    assert_eq!(first_page.items.len(), 1);
    assert_eq!(first_page.items[0].id, records[0].id);
    let offset_page = usage::page(10, 10);
    assert_eq!(offset_page.total, 1);
    assert!(offset_page.items.is_empty());

    // 报文捕获：入站请求 + 上游响应可回读
    usage::submit(
        &UsageRecord {
            id: 0,
            timestamp: timestamp.clone(),
            date: date.clone(),
            model_name: "Temp".into(),
            served_by: "Temp".into(),
            source_app: "claude-desktop".into(),
            upstream_url: "https://example.com/v1/responses".into(),
            upstream_model: "temp-model".into(),
            proxied: true,
            inbound_protocol: "anthropic-messages".into(),
            upstream_protocol: "openai-responses".into(),
            input_tokens: 3,
            output_tokens: 4,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            duration_ms: 5,
            ok: true,
            failover: false,
            error: None,
        },
        Some(&usage::UsagePayload {
            inbound_request: Some("{\"hello\":1}".into()),
            inbound_headers: Some("{\"x-api-key\":[\"claude-desktop\"]}".into()),
            upstream_request: Some("{\"system\":\"injected\"}".into()),
            upstream_response: Some("{\"ok\":true}".into()),
            stream: false,
        }),
    );

    crate::db::flush();
    let latest = usage::recent(1);
    let detail = usage::payload_detail(latest[0].id).expect("payload should load");
    assert_eq!(detail.inbound_request.as_deref(), Some("{\"hello\":1}"));
    assert_eq!(
        detail.upstream_request.as_deref(),
        Some("{\"system\":\"injected\"}")
    );
    assert_eq!(detail.upstream_response.as_deref(), Some("{\"ok\":true}"));
    assert!(
        !detail.stream
            && !detail.request_truncated
            && !detail.upstream_request_truncated
            && !detail.response_truncated
    );

    // 某天首次写入时 usage_daily_total 会新建行，报文仍须挂到正确的明细行
    // （回归：last_insert_rowid 若在 daily upsert 之后取，会被覆盖成 daily 的行号）
    usage::submit(
        &UsageRecord {
            id: 0,
            timestamp: timestamp.clone(),
            date: "2001-01-01".into(),
            model_name: "Temp".into(),
            served_by: "Temp".into(),
            source_app: "claude-desktop".into(),
            upstream_url: String::new(),
            upstream_model: String::new(),
            proxied: false,
            inbound_protocol: "anthropic-messages".into(),
            upstream_protocol: "openai-responses".into(),
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            duration_ms: 1,
            ok: true,
            failover: false,
            error: None,
        },
        Some(&usage::UsagePayload {
            inbound_request: Some("{\"day\":\"first\"}".into()),
            inbound_headers: None,
            upstream_request: None,
            upstream_response: None,
            stream: false,
        }),
    );
    crate::db::flush();
    let latest = usage::recent(1);
    let detail =
        usage::payload_detail(latest[0].id).expect("payload should link to its own detail");
    assert_eq!(
        detail.inbound_request.as_deref(),
        Some("{\"day\":\"first\"}")
    );

    // 超限报文被截断并置标记
    usage::submit(
        &UsageRecord {
            id: 0,
            timestamp: timestamp.clone(),
            date: date.clone(),
            model_name: "Temp".into(),
            served_by: "Temp".into(),
            source_app: "claude-desktop".into(),
            upstream_url: String::new(),
            upstream_model: String::new(),
            proxied: false,
            inbound_protocol: "anthropic-messages".into(),
            upstream_protocol: "openai-responses".into(),
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            duration_ms: 1,
            ok: true,
            failover: false,
            error: None,
        },
        Some(&usage::UsagePayload {
            inbound_request: Some("x".repeat(300 * 1024)),
            inbound_headers: None,
            upstream_request: None,
            upstream_response: None,
            stream: true,
        }),
    );

    crate::db::flush();
    let latest = usage::recent(1);
    let detail = usage::payload_detail(latest[0].id).expect("payload should load");
    assert!(detail.request_truncated);
    assert!(detail.stream);
    assert_eq!(detail.inbound_request.as_ref().unwrap().len(), 256 * 1024);

    // 报文不再做条数保留清理：写入多少就留多少（此时已有 3 条，再补 1001 条后应为 1004 条）
    for _ in 0..1001 {
        usage::submit(
            &UsageRecord {
                id: 0,
                timestamp: timestamp.clone(),
                date: date.clone(),
                model_name: "Temp".into(),
                served_by: "Temp".into(),
                source_app: "claude-desktop".into(),
                upstream_url: String::new(),
                upstream_model: String::new(),
                proxied: false,
                inbound_protocol: "anthropic-messages".into(),
                upstream_protocol: "openai-responses".into(),
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
                duration_ms: 1,
                ok: true,
                failover: false,
                error: None,
            },
            Some(&usage::UsagePayload {
                inbound_request: Some("{}".into()),
                inbound_headers: None,
                upstream_request: None,
                upstream_response: None,
                stream: false,
            }),
        );
    }
    crate::db::flush();
    let payload_count: i64 = crate::db::with_conn(|connection| {
        Ok(connection.query_row("SELECT COUNT(*) FROM usage_payload", [], |row| row.get(0))?)
    })
    .expect("count should load");
    assert_eq!(payload_count, 1004);

    events::log(
        "user",
        None,
        "test.event",
        Some("app"),
        Some("claude-desktop"),
        None,
    );
    crate::db::flush();
    let logged = events::list(10).expect("events should load");
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0].event_type, "test.event");

    updates::record_check(
        AppKind::ClaudeDesktop,
        Some("1.0.0"),
        Some("2.0.0"),
        Some("https://example.com/RELEASES"),
        true,
        "found",
        None,
    );
    crate::db::flush();
    let checks = updates::latest_checks();
    let snapshot = checks
        .get(&AppKind::ClaudeDesktop)
        .expect("check should load");
    assert!(snapshot.update_available);
    assert_eq!(snapshot.latest_version.as_deref(), Some("2.0.0"));

    // 过滤器：新增 → 落库 → 重新 load 后仍能读回，顺序与启用状态保持
    crate::filters::mutate(|list| {
        list.push(crate::domain::filter::RequestFilter {
            id: 1,
            name: "注入".into(),
            enabled: true,
            order: 0,
            rule: crate::domain::filter::FilterRule::SystemPrompt {
                mode: crate::domain::filter::PromptMode::Append,
                text: "be terse".into(),
            },
            created_at: String::new(),
            updated_at: String::new(),
        });
    })
    .expect("filter should save");

    crate::db::flush();
    crate::filters::load().expect("filters should load");
    let loaded = crate::filters::snapshot();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].name, "注入");
    assert!(loaded[0].enabled);
    assert_eq!(loaded[0].rule.kind(), "system-prompt");

    // 设置项：改过的三个键落库后，重新从库里读回来仍在
    settings::mutate(|store| {
        store.launch_at_login = false;
        store.proxy_enabled = true;
        store.proxy_url = "http://127.0.0.1:7890".into();
    })
    .expect("settings should save");

    crate::db::flush();
    settings::init(&dir).expect("settings should reload");
    let restored = settings::snapshot();
    assert!(!restored.launch_at_login);
    assert!(restored.proxy_enabled);
    assert_eq!(restored.proxy_url, "http://127.0.0.1:7890");

    // 关掉代理不该丢地址：下次打开开关还是它（settings 层只管存，不参与判断）
    settings::mutate(|store| store.proxy_enabled = false).expect("settings should save");
    crate::db::flush();
    settings::init(&dir).expect("settings should reload");
    let off = settings::snapshot();
    assert!(!off.proxy_enabled);
    assert_eq!(off.proxy_url, "http://127.0.0.1:7890");

    // 分页：总数一致、倒序、翻页与越界（此时明细已很多）
    crate::db::flush();
    let all = usage::page(0, 100_000);
    assert_eq!(all.total as usize, all.items.len());
    assert!(all.total > 50, "expected many rows, got {}", all.total);
    assert!(all.items.windows(2).all(|pair| pair[0].id > pair[1].id));
    let second = usage::page(50, 50);
    assert_eq!(second.items.len(), 50);
    assert_eq!(second.items[0].id, all.items[50].id);
    let past_end = usage::page(all.total as usize, 50);
    assert!(past_end.items.is_empty());
    assert_eq!(past_end.total, all.total);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 落库已改为异步投递：写失败没有调用方可以返回错误，必须能被观测到（面板/排查用）。
#[test]
fn database_write_failures_are_observable() {
    let _settings_db = lock_settings_db();
    let dir = std::env::temp_dir().join(format!(
        "ai-start-test-write-fail-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    crate::settings::init(&dir).expect("database should initialize");

    let before = crate::db::write_failures();
    crate::db::submit(|_connection| Err(crate::error::AppError::Message("write-boom".into())));
    crate::db::flush();

    assert!(crate::db::write_failures() > before, "失败的写要被计数");
    assert_eq!(crate::db::last_write_error().as_deref(), Some("write-boom"));
}

#[test]
fn response_assembler_builds_canonical_message_then_native_shape() {
    use crate::providers::ResponseAssembler;

    let mut assembler = ResponseAssembler::default();
    for event in canonical_events() {
        assembler.apply(&event);
    }

    let canonical = assembler.to_value();
    assert_eq!(canonical["id"], "msg_1");
    assert_eq!(canonical["type"], "message");
    assert_eq!(canonical["role"], "assistant");
    assert_eq!(canonical["model"], "gpt-5");
    assert_eq!(canonical["content"][0]["type"], "text");
    assert_eq!(canonical["content"][0]["text"], "Hello");
    assert_eq!(canonical["content"][1]["type"], "tool_use");
    assert_eq!(canonical["content"][1]["name"], "get_weather");
    assert_eq!(canonical["content"][1]["input"]["city"], "Beijing");
    assert_eq!(canonical["stop_reason"], "tool_use");
    assert_eq!(canonical["usage"]["input_tokens"], 3);
    assert_eq!(canonical["usage"]["output_tokens"], 4);

    // 落库前转成上游协议的原生形状（与非流式一致），供前端按协议解析
    let native = provider_for(ModelFormat::OpenaiCompletions)
        .encode_response(
            &model(ModelFormat::OpenaiCompletions, "https://api.openai.com/v1"),
            &canonical,
        )
        .unwrap();
    assert_eq!(native["choices"][0]["message"]["content"], "Hello");
    assert_eq!(
        native["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "get_weather"
    );
    assert_eq!(native["usage"]["prompt_tokens"], 3);
    assert_eq!(native["usage"]["completion_tokens"], 4);
}

// ── 请求过滤器（filters::apply）──────────────────────────────────────

use crate::domain::filter::{FilterRule, PromptMode, RequestFilter};
use crate::filters;

fn filter(enabled: bool, rule: FilterRule) -> RequestFilter {
    RequestFilter {
        id: 1,
        name: "test".into(),
        enabled,
        order: 0,
        rule,
        created_at: String::new(),
        updated_at: String::new(),
    }
}

#[test]
fn filter_injects_system_prompt() {
    // system 缺失 → 直接写入
    let out = filters::apply(
        &[filter(
            true,
            FilterRule::SystemPrompt {
                mode: PromptMode::Append,
                text: "B".into(),
            },
        )],
        request(json!({ "messages": [] })),
    )
    .unwrap();
    assert_eq!(out.raw()["system"], "B");

    // 已有字符串 → 追加 / 前置
    let append = filters::apply(
        &[filter(
            true,
            FilterRule::SystemPrompt {
                mode: PromptMode::Append,
                text: "B".into(),
            },
        )],
        request(json!({ "system": "A", "messages": [] })),
    )
    .unwrap();
    assert_eq!(append.raw()["system"], "A\nB");

    let prepend = filters::apply(
        &[filter(
            true,
            FilterRule::SystemPrompt {
                mode: PromptMode::Prepend,
                text: "B".into(),
            },
        )],
        request(json!({ "system": "A", "messages": [] })),
    )
    .unwrap();
    assert_eq!(prepend.raw()["system"], "B\nA");

    // 块数组 → 追加一个 text 块
    let blocks = filters::apply(
        &[filter(
            true,
            FilterRule::SystemPrompt {
                mode: PromptMode::Append,
                text: "B".into(),
            },
        )],
        request(json!({ "system": [{ "type": "text", "text": "A" }], "messages": [] })),
    )
    .unwrap();
    assert_eq!(blocks.raw()["system"][0]["text"], "A");
    assert_eq!(blocks.raw()["system"][1]["text"], "B");
}

#[test]
fn filter_skips_disabled_and_stacks_in_order() {
    // 停用的规则不生效
    let disabled = filters::apply(
        &[filter(
            false,
            FilterRule::SystemPrompt {
                mode: PromptMode::Append,
                text: "X".into(),
            },
        )],
        request(json!({ "system": "A", "messages": [] })),
    )
    .unwrap();
    assert_eq!(disabled.raw()["system"], "A");

    // 按列表顺序叠加：先追加 B，再前置 C
    let stacked = filters::apply(
        &[
            filter(
                true,
                FilterRule::SystemPrompt {
                    mode: PromptMode::Append,
                    text: "B".into(),
                },
            ),
            filter(
                true,
                FilterRule::SystemPrompt {
                    mode: PromptMode::Prepend,
                    text: "C".into(),
                },
            ),
        ],
        request(json!({ "system": "A", "messages": [] })),
    )
    .unwrap();
    assert_eq!(stacked.raw()["system"], "C\nA\nB");
}

#[test]
fn proxy_url_is_validated_before_it_reaches_the_network() {
    use crate::providers::validate_proxy;

    // 留空 = 直连：用户得能把代理清掉
    assert_eq!(validate_proxy("").unwrap(), None);
    assert_eq!(validate_proxy("   ").unwrap(), None);

    // 带协议头、不带协议头（补 http://）、带账号密码：都归一化后再存库
    assert_eq!(
        validate_proxy("http://127.0.0.1:7890").unwrap().as_deref(),
        Some("http://127.0.0.1:7890")
    );
    assert_eq!(
        validate_proxy(" 127.0.0.1:7890 ").unwrap().as_deref(),
        Some("http://127.0.0.1:7890")
    );
    assert!(validate_proxy("http://user:pw@127.0.0.1:7890").is_ok());

    // socks 要 reqwest 的 socks 特性（本项目没开）：保存时就拦住，别等发请求才失败
    assert!(validate_proxy("socks5://127.0.0.1:1080").is_err());
    assert!(validate_proxy("ftp://127.0.0.1:21").is_err());
    assert!(validate_proxy("http://").is_err());
    assert!(validate_proxy("not a url").is_err());
}

#[test]
fn loopback_targets_bypass_the_proxy_and_requests_say_so() {
    use crate::providers::request_proxies_through;

    let _settings_db = lock_settings_db();
    // 独立临时库：别的用例可能已经动过全局设置，这里从头初始化一份干净的
    let dir = std::env::temp_dir().join(format!("ai-start-test-proxy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    crate::settings::init(&dir).expect("database should initialize");

    // 代理没开时，谁都不算「走了代理」
    assert!(!request_proxies_through("https://api.anthropic.com/v1/messages"));

    // 开着代理
    crate::settings::mutate(|store| {
        store.proxy_enabled = true;
        store.proxy_url = "http://127.0.0.1:7890".into();
    })
    .expect("settings should save");

    // 远端地址走代理
    assert!(request_proxies_through("https://api.anthropic.com/v1/messages"));
    assert!(request_proxies_through("https://open.example.com/v1"));
    // 本机地址绕行：显式名单里的、环回网段里的其他地址、IPv6 环回、localhost 变体
    assert!(!request_proxies_through("http://127.0.0.1:11434/v1/chat/completions"));
    assert!(!request_proxies_through("http://127.1.2.3:8080/v1"));
    assert!(!request_proxies_through("http://[::1]:11434/v1"));
    assert!(!request_proxies_through("http://localhost:11434/v1"));
    assert!(!request_proxies_through("http://api.localhost/v1"));
    // 带账号信息、端口号不影响判定
    assert!(!request_proxies_through("http://user:pw@localhost:11434/v1"));

    // 收尾：把代理关回去、地址清掉。全局 DB 连接（OnceLock）整个测试进程只有一份，
    // 后面的用例「重新 init」也仍读这一个库，留下的状态会串场。
    crate::settings::mutate(|store| {
        store.proxy_enabled = false;
        store.proxy_url = String::new();
    })
    .expect("settings should save");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn turning_the_proxy_on_requires_an_address() {
    use crate::commands::system::resolve_proxy;

    // 开着却没地址 = 实际在直连，用户以为走了代理：保存就该拦下来
    assert!(resolve_proxy(Some(true), None, false, "").is_err());
    assert!(resolve_proxy(Some(true), Some("   "), false, "").is_err());
    // 已经开着、这次把地址清掉（没带开关字段）：同样是偷偷变直连，一样拦
    assert!(resolve_proxy(None, Some(""), true, "http://127.0.0.1:7890").is_err());

    // 开着且这次填了地址 / 库里已有地址：放行
    assert!(resolve_proxy(Some(true), Some("http://127.0.0.1:7890"), false, "").is_ok());
    assert!(resolve_proxy(Some(true), None, false, "http://127.0.0.1:7890").is_ok());

    // 关掉：地址留着下次用，清掉也行，两种都放行
    assert!(!resolve_proxy(Some(false), Some(""), true, "http://127.0.0.1:7890").unwrap());
    assert!(!resolve_proxy(None, None, false, "").unwrap());
}

#[test]
#[cfg(windows)]
fn autostart_quotes_the_executable_path() {
    use crate::autostart::command_line;
    use std::path::Path;

    // 带空格的路径必须加引号，否则 Windows 只认第一个空格之前的那一段。
    assert_eq!(
        command_line(Path::new(r"C:\Program Files\AI Start\ai-start.exe")),
        r#""C:\Program Files\AI Start\ai-start.exe""#
    );
}

/// 直通判定只有一条依据：入站协议 == 上游协议。请求侧、响应侧、流侧共用它。
#[test]
fn same_protocol_holds_exactly_when_the_formats_match() {
    use crate::gateway::server::same_protocol;

    for inbound in ModelFormat::ALL {
        for upstream in ModelFormat::ALL {
            assert_eq!(
                same_protocol(inbound, &model(upstream, "https://example.com")),
                inbound == upstream,
                "入站 {inbound:?} / 上游 {upstream:?}"
            );
        }
    }
}

/// 同协议直通（响应侧）：回给客户端的就是上游原文，`system_fingerprint`/`logprobs` 这类
/// 只有上游知道的字段不再被重建吃掉；跨协议才按客户端协议重建。
#[test]
fn same_protocol_response_is_forwarded_verbatim() {
    use crate::gateway::server::encode_client_response;

    let upstream = json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "upstream-model",
        "system_fingerprint": "fp_abc",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "hi", "refusal": null },
            "finish_reason": "stop",
            "logprobs": { "content": [] }
        }],
        "usage": { "prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4 }
    });
    let config = model(ModelFormat::OpenaiCompletions, "https://api.openai.com/v1");
    let canonical = provider_for(ModelFormat::OpenaiCompletions)
        .decode_response(&config, &upstream)
        .expect("upstream body should decode");

    // 同协议：一字不改
    let same = encode_client_response(
        ModelFormat::OpenaiCompletions,
        &config,
        &upstream,
        &canonical,
    )
    .expect("same protocol response");
    assert_eq!(same, upstream);

    // 跨协议：重建成客户端（Anthropic）形状，上游专有字段不复存在
    let crossed = encode_client_response(
        ModelFormat::AnthropicMessages,
        &config,
        &upstream,
        &canonical,
    )
    .expect("cross protocol response");
    assert!(crossed.get("system_fingerprint").is_none());
    assert!(crossed.get("created").is_none());
    assert_eq!(crossed["content"][0]["type"], "text");
    assert_eq!(crossed["content"][0]["text"], "hi");
}

/// 同协议直通（流侧）靠的就是这一份原文：解析器必须把上游那一帧原样带出来。
///
/// 顺带守住 `data` 的语义不变——记账旁路（`StreamState`/`ResponseAssembler`）读的是它，
/// 解析器改动不能让记账跟着变。多行 `data:` 那条还钉住了「不能拿 `data` 重排回 SSE」的原因：
/// 重排会把换行塞进 `data:` 行里，客户端的 SSE 解析器读到的是半截 JSON。
#[tokio::test]
async fn sse_frames_keep_the_upstream_bytes_intact() {
    use crate::gateway::sse::{encode_channel_event, parse_sse_stream};
    use futures_util::StreamExt;

    let upstream = concat!(
        ": ping\n\n",
        "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
        "data: {\n",
        "data:   \"id\": \"chatcmpl-1\",\n",
        "data:   \"object\": \"chat.completion.chunk\"\n",
        "data: }\n\n",
        "data: [DONE]\n\n",
    );
    let frames: Vec<_> =
        parse_sse_stream(futures_util::stream::iter(vec![Ok::<_, reqwest::Error>(
            bytes::Bytes::from(upstream),
        )]))
        .map(|frame| frame.expect("frame should parse"))
        .collect()
        .await;

    assert_eq!(frames.len(), 4);

    // 客户端拿到的字节 === 上游字节（心跳注释也没被吞掉）
    let forwarded: String = frames.iter().map(|frame| frame.raw.as_str()).collect();
    assert_eq!(forwarded, upstream);
    assert_eq!(frames[0].raw, ": ping\n\n");
    assert!(frames[0].data.is_empty());

    // 多行 data 拼出来的副本仍是合法 JSON……
    let multiline: Value = serde_json::from_str(&frames[2].data).expect("joined data is JSON");
    assert_eq!(multiline["object"], "chat.completion.chunk");
    // ……但重排回 SSE 会把它压成一行、把换行塞进 data 行里，所以直通必须发 raw
    assert_ne!(
        encode_channel_event(&frames[2].event, &frames[2].data),
        frames[2].raw
    );
}

/// 多字节字符（CJK）被 TCP 分帧从中间切开时不能变成乱码：按 chunk 解码会吞掉半个字符，
/// 按「凑齐一整行再解码」才能保真（B2）。
#[tokio::test]
async fn sse_frames_decode_whole_lines_so_cjk_survives_chunk_boundaries() {
    use crate::gateway::sse::parse_sse_stream;
    use futures_util::StreamExt;

    let line = "data: {\"id\":1,\"choices\":[{\"delta\":{\"content\":\"你好世界，测试\"}}]}\n\n";
    let bytes = line.as_bytes();
    // 每次只给 3 个字节：多个 UTF-8 字符与 JSON 结构都被切开。
    let chunks: Vec<Result<bytes::Bytes, reqwest::Error>> = bytes
        .chunks(3)
        .map(|chunk| Ok::<_, reqwest::Error>(bytes::Bytes::copy_from_slice(chunk)))
        .collect();
    let frames: Vec<_> = parse_sse_stream(futures_util::stream::iter(chunks))
        .map(|frame| frame.expect("frame should parse"))
        .collect()
        .await;

    assert_eq!(frames.len(), 1);
    // 正文一字不差
    let payload: Value = serde_json::from_str(&frames[0].data).expect("joined data is JSON");
    assert_eq!(
        payload["choices"][0]["delta"]["content"],
        "你好世界，测试"
    );
    // 直通原文同样保真
    assert_eq!(frames[0].raw, line);

    // \r\n 行尾跨 chunk 也要归一（\r 在上一个 chunk 结尾、\n 在下一个开头）
    let crlf_chunks: Vec<Result<bytes::Bytes, reqwest::Error>> = vec![
        Ok(bytes::Bytes::from_static(b"data: {\"a\":1}\r")),
        Ok(bytes::Bytes::from_static(b"\n\r\ndata: [DONE]\r\n")),
    ];
    let frames: Vec<_> = parse_sse_stream(futures_util::stream::iter(crlf_chunks))
        .map(|frame| frame.expect("frame should parse"))
        .collect()
        .await;
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].data, "{\"a\":1}");
    assert_eq!(frames[0].raw, "data: {\"a\":1}\n\n");
    assert_eq!(frames[1].data, "[DONE]");
}

/// 首帧限时（B1）：上游接受了请求、回了响应头，却一直不吐第一个字节时，等待必须被掐断——
/// 否则流永久悬住，客户端干等、明细永远落不了库。超时以 `Err` 项进流（不是直接中断），
/// 才能和上游断流走同一条处理路径：记账 + 跨协议时报错给客户端。
#[tokio::test]
async fn first_frame_timeout_gives_up_when_the_upstream_never_speaks() {
    use crate::gateway::sse::{first_frame_timeout, SseFrame};
    use futures_util::StreamExt;
    use std::time::Duration;

    let silent = futures_util::stream::pending::<crate::error::AppResult<SseFrame>>();
    let items: Vec<_> = first_frame_timeout(silent, Duration::from_millis(30))
        .collect()
        .await;

    assert_eq!(items.len(), 1, "超时只该产出一个错误项然后收流");
    let message = items[0].as_ref().expect_err("超时必须是 Err").to_string();
    assert!(message.contains("首帧"), "{message}");
}

/// 限时只针对首帧：第一帧到了就撤掉（长生成合法，加总超时会把正常输出长文的流掐断）。
#[tokio::test]
async fn first_frame_timeout_stops_policing_after_the_first_frame() {
    use crate::error::AppError;
    use crate::gateway::sse::{first_frame_timeout, SseFrame};
    use futures_util::StreamExt;
    use std::time::Duration;

    // 首帧立刻到，第二帧拖过限时之后才到。
    let slow_but_alive = async_stream::stream! {
        yield Ok::<_, AppError>(SseFrame {
            event: String::new(),
            data: "{\"a\":1}".into(),
            raw: "data: {\"a\":1}\n\n".into(),
        });
        tokio::time::sleep(Duration::from_millis(120)).await;
        yield Ok::<_, AppError>(SseFrame {
            event: String::new(),
            data: "{\"a\":2}".into(),
            raw: "data: {\"a\":2}\n\n".into(),
        });
    };
    let items: Vec<_> = first_frame_timeout(slow_but_alive, Duration::from_millis(30))
        .collect()
        .await;

    assert_eq!(items.len(), 2, "首帧之后不再限时");
    assert_eq!(items[1].as_ref().expect("frame 应通过").data, "{\"a\":2}");
    assert_eq!(items[1].as_ref().expect("frame 应通过").raw, "data: {\"a\":2}\n\n");
}

/// C1：Completions 上游每个分片都带同一条补全的 `id`（`chatcmpl-…`），要捕获进记账状态。
/// 不读它的话，落库的 `upstream_response.id` 永远是网关合成的 `msg_<uuid>`——对不上上游侧的日志，
/// 而跨协议（Anthropic / Responses 入站）时这个 id 还会直接发给客户端。
#[test]
fn completions_stream_captures_the_upstream_chunk_id() {
    let provider = provider_for(ModelFormat::OpenaiCompletions);
    let config = model(ModelFormat::OpenaiCompletions, "https://api.openai.com/v1");
    let mut state = StreamState::new("gpt-4o");
    let synthesized = state.message_id.clone();

    let events = provider
        .decode_stream_event(
            &config,
            "",
            &json!({
                "id": "chatcmpl-abc123",
                "object": "chat.completion.chunk",
                "choices": [{ "delta": { "content": "hi" } }]
            }),
            &mut state,
        )
        .unwrap();

    assert_ne!(state.message_id, synthesized, "id 该换成上游分片的 id");
    assert_eq!(state.message_id, "chatcmpl-abc123");
    // 第一个正文分片会带出规范 message_start，客户端与落库读的都是这里的 id
    let start = events
        .iter()
        .find(|event| event.data["type"] == "message_start")
        .expect("首个正文分片应带出 message_start");
    assert_eq!(start.data["message"]["id"], "chatcmpl-abc123");

    // 分片没带 id（或带空串）时保留既有值，不要写成空
    provider
        .decode_stream_event(
            &config,
            "",
            &json!({ "id": "", "choices": [{ "delta": { "content": "!" } }] }),
            &mut state,
        )
        .unwrap();
    assert_eq!(state.message_id, "chatcmpl-abc123");
}

/// L1：`event:` 信封名不认识时，拿正文的 `type` 回落。不守规矩的中转会把每一帧的信封
/// 统一写成别的名字（这里用 `message` 当例子），正文里的 `type` 才是权威。
#[test]
fn responses_frames_fall_back_to_the_body_type_when_the_envelope_is_unknown() {
    let config = model(ModelFormat::OpenaiResponses, "https://api.openai.com/v1");
    let provider = provider_for(ModelFormat::OpenaiResponses);
    let mut state = StreamState::new("gpt-4o");

    let events = provider
        .decode_stream_event(
            &config,
            "message",
            &json!({ "type": "response.output_text.delta", "delta": "你好" }),
            &mut state,
        )
        .unwrap();
    let text = events
        .iter()
        .find_map(|event| event.data.pointer("/delta/text").and_then(Value::as_str))
        .expect("正文分片不能因为信封名不认识就整帧丢掉");
    assert_eq!(text, "你好");

    // 终止帧同样要认出来：`upstream_ended` 不置位，一条答完的流会被记成 Truncated
    provider
        .decode_stream_event(
            &config,
            "message",
            &json!({
                "type": "response.completed",
                "response": { "usage": { "input_tokens": 7, "output_tokens": 3 } }
            }),
            &mut state,
        )
        .unwrap();
    assert!(state.upstream_ended, "终止帧也要按正文的 type 认出来");
    assert_eq!(state.output_tokens, 3);

    // 两个名字都不认识：维持改动前的行为——丢掉，且不许碰状态
    let mut untouched = StreamState::new("gpt-4o");
    let events = provider
        .decode_stream_event(
            &config,
            "message",
            &json!({ "type": "message" }),
            &mut untouched,
        )
        .unwrap();
    assert!(events.is_empty());
    assert!(!untouched.message_started && !untouched.upstream_ended);
}

/// L1：Anthropic 侧同样用正文的 `type` 回落。它的每一帧本来就要发给客户端，所以认出来之后
/// 连事件名一起改成认出来的那个——把不认识的信封名原样发过去，客户端一样是丢帧。
#[test]
fn anthropic_frames_fall_back_to_the_body_type_when_the_envelope_is_unknown() {
    let config = model(ModelFormat::AnthropicMessages, "https://api.anthropic.com");
    let provider = provider_for(ModelFormat::AnthropicMessages);
    let mut state = StreamState::new("claude");

    let events = provider
        .decode_stream_event(
            &config,
            "message",
            &json!({
                "type": "message_delta",
                "delta": { "stop_reason": "max_tokens" },
                "usage": { "output_tokens": 12 }
            }),
            &mut state,
        )
        .unwrap();
    assert_eq!(
        state.stop_reason.as_deref(),
        Some("max_tokens"),
        "回落之后状态照样要推进"
    );
    assert_eq!(state.output_tokens, 12);
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].event, "message_delta",
        "事件名要跟着认出来的那个走，别把不认识的信封名发出去"
    );

    // 两个名字都不认识：原样转发（维持改动前的行为），状态不动
    let mut untouched = StreamState::new("claude");
    let events = provider
        .decode_stream_event(
            &config,
            "message",
            &json!({ "type": "message" }),
            &mut untouched,
        )
        .unwrap();
    assert_eq!(events[0].event, "message");
    assert!(!untouched.message_started && !untouched.upstream_ended && !untouched.finished);
}

/// L3：代理绕行的判定只有一份名单（`NO_PROXY_LIST`），规则照着 reqwest 内部那份抄。
/// 这张表按 hyper-util 自己的用例（reqwest 真正用的 matcher）整理，保证两边不各说各话。
#[test]
fn loopback_bypass_matches_reqwest_rules() {
    use crate::providers::no_proxy_matches;

    // 本机地址：走直连（清单里 `localhost` 也覆盖它的全部子域，`127.0.0.0/8` 覆盖整个网段）
    for host in [
        "localhost",
        "LOCALHOST",
        "api.localhost",
        "127.0.0.1",
        "127.9.9.9",
        "[::1]",
    ] {
        assert!(
            no_proxy_matches(host),
            "{host} 是本机地址，展示口径不该写成「走了代理」"
        );
    }

    // 非本机地址：代理开着就走代理
    for host in [
        "example.com",
        "localhost.example.com",
        "notlocalhost",
        "8.8.8.8",
        "128.0.0.1",
        "[2001:db8::1]",
    ] {
        assert!(!no_proxy_matches(host), "{host} 不是本机地址，不该白名单放行");
    }
}

/// L3 的守门测试：名单里只许有回环地址。这条不是为了现在，是为了以后——想把 `10.0.0.0/8`
/// 顺手加进去时会红，而那不是「加一行」：`is_loopback_target` / `request_proxies_through`
/// 的命名、注释、明细里的展示口径都得跟着重新想。
#[test]
fn no_proxy_list_only_covers_loopback() {
    for entry in crate::providers::NO_PROXY_LIST.split(',').map(str::trim) {
        let address = entry.split('/').next().unwrap_or(entry);
        let loopback = match address.parse::<std::net::IpAddr>() {
            Ok(ip) => ip.is_loopback(),
            // 域名条目：只认 `localhost` 与它的子域（RFC 6761 把 `.localhost` 整个留给了回环）。
            Err(_) => {
                let domain = address.trim_start_matches('.').to_ascii_lowercase();
                domain == "localhost" || domain.ends_with(".localhost")
            }
        };
        assert!(
            loopback,
            "`{entry}` 不是回环地址：{}",
            crate::providers::NO_PROXY_LIST
        );
    }
}

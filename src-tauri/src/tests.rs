use serde_json::{json, Value};

use crate::commands::models::parse_model_ids;
use crate::domain::canonical::{
    CanonicalOnly, CanonicalRequest, MaxTokensField, RequestBody, SystemPrompt,
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

#[test]
fn candidate_models_puts_active_first_and_only_expands_when_failover_is_on() {
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

    settings.auto_failover = false;
    let single = settings.candidate_models();
    assert_eq!(single.len(), 1);
    assert_eq!(single[0].name, "B");

    settings.auto_failover = true;
    let all = settings.candidate_models();
    let names: Vec<&str> = all.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, vec!["B", "A", "C"]);
}

#[test]
fn candidates_for_routes_aliases_to_the_usual_logic_and_named_models_to_themselves() {
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
    settings.auto_failover = true;

    let names = |list: Vec<ModelConfig>| {
        list.into_iter()
            .map(|model| model.name)
            .collect::<Vec<String>>()
    };

    // 网关别名与 auto（任意大小写、带空白）→ 现有逻辑：当前模型优先，其后依次是其余模型。
    for alias in [
        "aiStart", "aistart", "AISTART", "auto", "Auto", "AUTO", "  auto  ", "  aiStart ",
    ] {
        assert_eq!(
            names(settings.candidates_for(Some(alias))),
            vec!["B", "A", "C"],
            "{alias:?} 应走现有逻辑"
        );
    }
    // 未指定 / 空 → 现有逻辑。
    assert_eq!(names(settings.candidates_for(None)), vec!["B", "A", "C"]);
    assert_eq!(names(settings.candidates_for(Some("   "))), vec!["B", "A", "C"]);
    // 未命中模型列表 → 现有逻辑。
    assert_eq!(
        names(settings.candidates_for(Some("不存在"))),
        vec!["B", "A", "C"]
    );

    // 命中模型列表里的显示名（不区分大小写）→ 只调用该模型，自动切换对它无效。
    for name in ["C", "c", "  C  "] {
        assert_eq!(
            names(settings.candidates_for(Some(name))),
            vec!["C"],
            "{name:?} 应只调用 C"
        );
    }

    // 关掉自动切换同样成立（本来就是单模型）。
    settings.auto_failover = false;
    assert_eq!(names(settings.candidates_for(Some("A"))), vec!["A"]);
    assert_eq!(names(settings.candidates_for(Some("auto"))), vec!["B"]);
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
        tool_choice: Some(json!("auto")),
        temperature: Some(0.0),
        top_p: Some(0.0),
        stop_sequences: Some(vec!["s".into()]),
        stream: true,
        top_k: Some(1.0),
        store: Some(true),
        metadata: Some(json!({})),
        response_format: Some(json!({ "type": "text" })),
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
    assert_eq!(encoded["store"], true);
    assert_eq!(encoded["parallel_tool_calls"], false);
    assert_eq!(encoded["temperature"], 0.2);
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
    assert_eq!(encoded["tool_choice"]["disable_parallel_tool_use"], true);
    assert_eq!(encoded["metadata"]["user_id"], "u1");
    assert_eq!(encoded["stop_sequences"][0], "END");
    for key in [
        "response_format",
        "reasoning_effort",
        "parallel_tool_calls",
        "store",
        "top_k",
        "_canonical",
    ] {
        assert!(encoded.get(key).is_none(), "{key} 不该出现在 Anthropic 报文里");
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

#[test]
fn sqlite_persistence_round_trips() {
    use crate::domain::app::AppKind;
    use crate::domain::model::ModelInput;
    use crate::usage::{self, UsageRecord};
    use crate::{events, settings, updates};

    let dir = std::env::temp_dir().join(format!("ai-start-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    settings::init(&dir).expect("database should initialize");

    let seeded = settings::snapshot();
    assert_eq!(seeded.models.len(), 2);
    assert!(seeded.active_model_id.is_some());

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
    assert_eq!(saved.id, 3);

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
    usage::record_with_payload(
        &UsageRecord {
            id: 0,
            timestamp: timestamp.clone(),
            date: date.clone(),
            model_name: "Temp".into(),
            served_by: "Temp".into(),
            source_app: "claude-desktop".into(),
            upstream_url: "https://example.com/v1/responses".into(),
            upstream_model: "temp-model".into(),
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

    let records = usage::recent(10);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].total_tokens(), 15);
    assert_eq!(records[0].source_app, "claude-desktop");
    assert_eq!(
        records[0].upstream_url,
        "https://example.com/v1/responses"
    );
    assert_eq!(records[0].upstream_model, "temp-model");
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
    usage::record_with_payload(
        &UsageRecord {
            id: 0,
            timestamp: timestamp.clone(),
            date: date.clone(),
            model_name: "Temp".into(),
            served_by: "Temp".into(),
            source_app: "claude-desktop".into(),
            upstream_url: "https://example.com/v1/responses".into(),
            upstream_model: "temp-model".into(),
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
            upstream_request: Some("{\"system\":\"injected\"}".into()),
            upstream_response: Some("{\"ok\":true}".into()),
            stream: false,
        }),
    );

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
    usage::record_with_payload(
        &UsageRecord {
            id: 0,
            timestamp: timestamp.clone(),
            date: "2001-01-01".into(),
            model_name: "Temp".into(),
            served_by: "Temp".into(),
            source_app: "claude-desktop".into(),
            upstream_url: String::new(),
            upstream_model: String::new(),
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
            upstream_request: None,
            upstream_response: None,
            stream: false,
        }),
    );
    let latest = usage::recent(1);
    let detail =
        usage::payload_detail(latest[0].id).expect("payload should link to its own detail");
    assert_eq!(
        detail.inbound_request.as_deref(),
        Some("{\"day\":\"first\"}")
    );

    // 超限报文被截断并置标记
    usage::record_with_payload(
        &UsageRecord {
            id: 0,
            timestamp: timestamp.clone(),
            date: date.clone(),
            model_name: "Temp".into(),
            served_by: "Temp".into(),
            source_app: "claude-desktop".into(),
            upstream_url: String::new(),
            upstream_model: String::new(),
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
            upstream_request: None,
            upstream_response: None,
            stream: true,
        }),
    );

    let latest = usage::recent(1);
    let detail = usage::payload_detail(latest[0].id).expect("payload should load");
    assert!(detail.request_truncated);
    assert!(detail.stream);
    assert_eq!(detail.inbound_request.as_ref().unwrap().len(), 256 * 1024);

    // 报文不再做条数保留清理：写入多少就留多少（此时已有 3 条，再补 1001 条后应为 1004 条）
    for _ in 0..1001 {
        usage::record_with_payload(
            &UsageRecord {
                id: 0,
                timestamp: timestamp.clone(),
                date: date.clone(),
                model_name: "Temp".into(),
                served_by: "Temp".into(),
                source_app: "claude-desktop".into(),
                upstream_url: String::new(),
                upstream_model: String::new(),
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
                upstream_request: None,
                upstream_response: None,
                stream: false,
            }),
        );
    }
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

    crate::filters::load().expect("filters should load");
    let loaded = crate::filters::snapshot();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].name, "注入");
    assert!(loaded[0].enabled);
    assert_eq!(loaded[0].rule.kind(), "system-prompt");

    // 分页：总数一致、倒序、翻页与越界（此时明细已很多）
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

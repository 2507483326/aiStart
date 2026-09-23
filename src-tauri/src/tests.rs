use serde_json::json;

use crate::commands::models::parse_model_ids;
use crate::domain::canonical::CanonicalRequest;
use crate::domain::model::{ModelConfig, ModelFormat};
use crate::providers::provider_for;
use crate::settings::Settings;

fn model(format: ModelFormat, base_url: &str) -> ModelConfig {
    ModelConfig {
        id: "test".into(),
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
    assert_eq!(bare.completion_url(), "https://api.anthropic.com/v1/messages");
    assert_eq!(bare.models_url(), "https://api.anthropic.com/v1/models");

    let with_v1 = model(ModelFormat::OpenaiCompletions, "https://api.openai.com/v1");
    assert_eq!(with_v1.completion_url(), "https://api.openai.com/v1/chat/completions");
    assert_eq!(with_v1.models_url(), "https://api.openai.com/v1/models");

    let responses = model(ModelFormat::OpenaiResponses, "http://127.0.0.1:11434/v1/");
    assert_eq!(responses.completion_url(), "http://127.0.0.1:11434/v1/responses");
}

#[test]
fn candidate_models_puts_active_first_and_only_expands_when_failover_is_on() {
    let mut settings = Settings::default();
    for name in ["A", "B", "C"] {
        settings.upsert(crate::domain::model::ModelInput {
            id: Some(name.into()),
            name: name.into(),
            format: ModelFormat::OpenaiCompletions,
            base_url: "https://example.com/v1".into(),
            api_key: String::new(),
            model: name.into(),
            supports_1m: false,
        });
    }
    settings.active_model_id = Some("B".into());

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
    assert_eq!(encoded["tools"][0]["function"]["parameters"]["type"], "object");
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

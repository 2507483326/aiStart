//! DeepSeek Harness（DSH）配置读写。
//!
//! DSH 把自定义模型接入写在 `~/.dsh/settings.yaml` 的 `llm-pi-ai.providers` 下，
//! provider 的 Key 通过 `apiKeyEnv` 间接指向 `~/.dsh/.credentials.yaml` 的 `refs`。
//! 这里只做「合并」：新增或更新 aiStart 自己的条目，文件里其余内容原样保留。

use std::path::{Path, PathBuf};

use serde_yaml::{Mapping, Value};

use crate::domain::app::{AppKind, ApplyMode, ApplyReport};
use crate::error::{AppError, AppResult};
use crate::platform::ApplyContext;

/// aiStart 在 `llm-pi-ai.providers` 下使用的 provider 名。
pub const PROVIDER_KEY: &str = "aistart";
/// 存放网关 Key 的凭据名，真实值写在 `.credentials.yaml` 的 `refs` 里。
pub const API_KEY_ENV: &str = "AISTART_API_KEY";

const API_KIND: &str = "openai-completions";
const PLUGIN_KEY: &str = "llm-pi-ai";
const PROVIDERS_KEY: &str = "providers";
const DEFAULT_MODEL_KEY: &str = "agent-default-model";
const REFS_KEY: &str = "refs";

pub struct Paths {
    pub settings: PathBuf,
    pub credentials: PathBuf,
}

/// 目标文件默认是 `~/.dsh/settings.yaml`（可在设置里覆盖），
/// 凭据固定取同目录下的 `.credentials.yaml`。
pub fn paths() -> Paths {
    let settings = PathBuf::from(crate::settings::deepseek_config_path());
    let credentials = settings
        .parent()
        .map(|dir| dir.join(".credentials.yaml"))
        .unwrap_or_else(|| PathBuf::from(".credentials.yaml"));
    Paths {
        settings,
        credentials,
    }
}

/// 网关的 OpenAI 兼容入口：调用方会自行拼上 `/chat/completions`。
fn openai_base(ctx: &ApplyContext) -> String {
    format!("{}/v1", ctx.gateway_base_url.trim_end_matches('/'))
}

/// 读一份 YAML；文件不存在或为空时当作空映射。解析失败会报错而不是返回空，
/// 避免把看不懂的内容当成「空文件」覆盖掉。
fn load(path: &Path) -> AppResult<Value> {
    if !path.exists() {
        return Ok(Value::Mapping(Mapping::new()));
    }
    let text = std::fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(Value::Mapping(Mapping::new()));
    }
    let value: Value = serde_yaml::from_str(&text).map_err(|error| {
        AppError::Message(format!(
            "{} 不是合法 YAML（{error}），已放弃写入以免破坏现有配置",
            path.display()
        ))
    })?;
    Ok(match value {
        Value::Null => Value::Mapping(Mapping::new()),
        other => other,
    })
}

fn save(path: &Path, root: &Value) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_yaml::to_string(root)
        .map_err(|error| AppError::Message(format!("生成 YAML 失败: {error}")))?;
    std::fs::write(path, text)?;
    Ok(())
}

fn root_mapping(root: &mut Value) -> AppResult<&mut Mapping> {
    root.as_mapping_mut().ok_or_else(|| {
        AppError::Message("settings.yaml 顶层不是映射，已放弃写入以免覆盖现有配置".into())
    })
}

/// 取 `parent[key]`，不存在就建一个映射；已存在但不是映射时报错而不是覆盖。
fn child_mapping<'a>(parent: &'a mut Mapping, key: &str) -> AppResult<&'a mut Mapping> {
    parent
        .entry(Value::from(key))
        .or_insert(Value::Mapping(Mapping::new()))
        .as_mapping_mut()
        .ok_or_else(|| AppError::Message(format!("{key} 不是映射，已放弃写入以免覆盖现有配置")))
}

fn providers_mut(root: &mut Value) -> AppResult<&mut Mapping> {
    let plugin = child_mapping(root_mapping(root)?, PLUGIN_KEY)?;
    child_mapping(plugin, PROVIDERS_KEY)
}

fn string_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// aiStart 写入的条目都带这个凭据名，用它认领已有配置。
fn is_ours(value: &Value) -> bool {
    string_at(value, "apiKeyEnv") == Some(API_KEY_ENV)
}

/// 找出这份配置里已经属于 aiStart 的 provider：先认固定名，再认凭据名，
/// 最后认 baseURL 已指向本地网关的条目，避免重复添加。
fn owned_provider(providers: &Mapping, gateway_base_url: Option<&str>) -> Option<String> {
    if providers.contains_key(PROVIDER_KEY) {
        return Some(PROVIDER_KEY.to_string());
    }
    let host = gateway_base_url.map(|url| url.trim_end_matches('/').to_string());
    for (key, value) in providers.iter() {
        let Some(name) = key.as_str() else { continue };
        if is_ours(value) {
            return Some(name.to_string());
        }
        if let Some(base) = string_at(value, "baseURL") {
            let matches_gateway = host
                .as_deref()
                .is_some_and(|host| !host.is_empty() && base.starts_with(host));
            if matches_gateway {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// 把 aiStart 的 provider 合并进 `providers[key]`，未知字段保留。
fn merge_provider(providers: &mut Mapping, key: &str, ctx: &ApplyContext) -> AppResult<()> {
    let entry = providers
        .entry(Value::from(key))
        .or_insert(Value::Mapping(Mapping::new()))
        .as_mapping_mut()
        .ok_or_else(|| {
            AppError::Message(format!(
                "providers.{key} 不是映射，已放弃写入以免覆盖现有配置"
            ))
        })?;

    entry.insert(Value::from("apiKeyEnv"), Value::from(API_KEY_ENV));
    entry.insert(Value::from("api"), Value::from(API_KIND));
    entry.insert(Value::from("baseURL"), Value::from(openai_base(ctx)));

    let models: Vec<Value> = ctx
        .model_choices
        .iter()
        .map(|choice| {
            let mut item = Mapping::new();
            item.insert(Value::from("id"), Value::from(choice.id.as_str()));
            item.insert(Value::from("name"), Value::from(choice.label.as_str()));
            Value::Mapping(item)
        })
        .collect();
    entry.insert(Value::from("models"), Value::Sequence(models));
    Ok(())
}

/// 合并 `settings.yaml`：provider 条目 + 默认模型。返回实际使用的 provider 名。
fn merge_settings(root: &mut Value, ctx: &ApplyContext) -> AppResult<String> {
    let key = {
        let providers = providers_mut(root)?;
        let key = owned_provider(providers, Some(&ctx.gateway_base_url))
            .unwrap_or_else(|| PROVIDER_KEY.to_string());
        merge_provider(providers, &key, ctx)?;
        key
    };

    let default = child_mapping(root_mapping(root)?, DEFAULT_MODEL_KEY)?;
    default.insert(Value::from("provider"), Value::from(key.as_str()));
    default.insert(Value::from("model"), Value::from(ctx.gateway_model_id()));
    Ok(key)
}

fn merge_credentials(root: &mut Value, token: &str) -> AppResult<()> {
    let root = root_mapping(root)?;
    // 凭据库顶层带 schema 版本；文件不存在时补齐，避免 DSH 读不懂我们新建的文件。
    if !root.contains_key("version") {
        root.insert(Value::from("version"), Value::from(1));
    }
    let refs = child_mapping(root, REFS_KEY)?;
    refs.insert(Value::from(API_KEY_ENV), Value::from(token));
    Ok(())
}

/// 移除 aiStart 的 provider；若默认模型也指向它则一并清掉。
/// 返回被移除的 provider 名，没有我们的条目时返回 None。
fn strip_settings(root: &mut Value) -> Option<String> {
    let key = {
        let providers = root
            .as_mapping_mut()?
            .get_mut(PLUGIN_KEY)?
            .as_mapping_mut()?
            .get_mut(PROVIDERS_KEY)?
            .as_mapping_mut()?;
        let key = owned_provider(providers, None)?;
        providers.remove(Value::from(key.as_str()));
        key
    };

    let root = root.as_mapping_mut()?;
    let default_points_at_us = root
        .get(DEFAULT_MODEL_KEY)
        .and_then(|value| string_at(value, "provider"))
        .is_some_and(|provider| provider == key);
    if default_points_at_us {
        root.remove(Value::from(DEFAULT_MODEL_KEY));
    }
    Some(key)
}

fn strip_credentials(root: &mut Value) -> bool {
    root.as_mapping_mut()
        .and_then(|root| root.get_mut(REFS_KEY))
        .and_then(Value::as_mapping_mut)
        .and_then(|refs| refs.remove(Value::from(API_KEY_ENV)))
        .is_some()
}

pub fn is_configured() -> bool {
    let Ok(root) = load(&paths().settings) else {
        return false;
    };
    root.get(PLUGIN_KEY)
        .and_then(|plugin| plugin.get(PROVIDERS_KEY))
        .and_then(Value::as_mapping)
        .is_some_and(|providers| owned_provider(providers, None).is_some())
}

pub fn apply(ctx: &ApplyContext) -> AppResult<ApplyReport> {
    let paths = paths();

    let mut settings = load(&paths.settings)?;
    let provider = merge_settings(&mut settings, ctx)?;
    save(&paths.settings, &settings)?;

    let mut credentials = load(&paths.credentials)?;
    merge_credentials(&mut credentials, &ctx.gateway_token)?;
    save(&paths.credentials, &credentials)?;

    let model = ctx.gateway_model_id().to_string();
    let base = openai_base(ctx);
    Ok(ApplyReport {
        kind: AppKind::DeepseekDesktop,
        model_id: ctx.model.id,
        model_name: ctx.model.name.clone(),
        apply_mode: ApplyMode::DirectConfig,
        target: format!(
            "{} → llm-pi-ai.providers.{provider}",
            paths.settings.display()
        ),
        restart_required: true,
        steps: vec![
            format!(
                "合并写入 {}（其他 provider 与设置保持不变）",
                paths.settings.display()
            ),
            format!("provider「{provider}」指向本地网关 {base}"),
            format!(
                "网关 Key 写入 {} 的 refs.{API_KEY_ENV}",
                paths.credentials.display()
            ),
            format!("agent-default-model 指向 {provider}/{model}"),
            format!(
                "上游模型: {} ({})",
                ctx.model.model,
                ctx.model.format.display_name()
            ),
            "重启 DeepSeek Desktop 后生效".into(),
        ],
        note: Some(
            "只合并 aiStart 自己的 provider 与 agent-default-model，不动文件里的其他内容；需要还原时用「移除模型配置」。"
                .into(),
        ),
    })
}

pub fn clear() -> AppResult<()> {
    let paths = paths();

    if paths.settings.exists() {
        let mut settings = load(&paths.settings)?;
        if strip_settings(&mut settings).is_some() {
            save(&paths.settings, &settings)?;
        }
    }

    if paths.credentials.exists() {
        let mut credentials = load(&paths.credentials)?;
        if strip_credentials(&mut credentials) {
            save(&paths.credentials, &credentials)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::{ModelConfig, ModelFormat};
    use crate::platform::{gateway_alias_choice, GATEWAY_ALIAS};

    fn parse(text: &str) -> Value {
        serde_yaml::from_str(text).expect("test fixture is valid yaml")
    }

    fn context() -> ApplyContext {
        ApplyContext {
            model: ModelConfig {
                id: 7,
                name: "command".into(),
                format: ModelFormat::OpenaiCompletions,
                base_url: "https://api.commandcode.ai/provider/v1".into(),
                api_key: "sk-test".into(),
                model: "deepseek/deepseek-v4-flash".into(),
                supports_1m: false,
                created_at: String::new(),
                updated_at: String::new(),
            },
            gateway_base_url: "http://127.0.0.1:8931".into(),
            gateway_token: "deepseek-desktop".into(),
            model_choices: vec![gateway_alias_choice()],
        }
    }

    #[test]
    fn exposes_a_single_gateway_alias_not_claudes_tiered_routes() {
        let models = crate::platform::configurator_for(AppKind::DeepseekDesktop).exposed_models();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, GATEWAY_ALIAS);
        assert_eq!(models[0].label, GATEWAY_ALIAS);
    }

    fn providers_of(root: &Value) -> &Mapping {
        root.get(PLUGIN_KEY)
            .and_then(|plugin| plugin.get(PROVIDERS_KEY))
            .and_then(Value::as_mapping)
            .expect("providers mapping")
    }

    #[test]
    fn merges_into_existing_settings_without_touching_other_entries() {
        let mut root = parse(
            "llm-pi-ai:\n  providers:\n    cmd:\n      apiKeyEnv: CMD_API_KEY\n      api: openai-completions\n      baseURL: https://api.commandcode.ai/provider/v1/\nagent-default-model:\n  provider: cmd\n  model: deepseek/deepseek-v4.1-flash\nui-onboarding:\n  welcomeNoticeVersion: 2026-08-13.1\n",
        );

        let key = merge_settings(&mut root, &context()).expect("merge succeeds");
        assert_eq!(key, PROVIDER_KEY);

        let providers = providers_of(&root);
        assert_eq!(providers.len(), 2, "existing provider must survive");
        let cmd = providers.get("cmd").expect("cmd kept");
        assert_eq!(
            string_at(cmd, "baseURL"),
            Some("https://api.commandcode.ai/provider/v1/")
        );

        let ours = providers.get(PROVIDER_KEY).expect("aistart added");
        assert_eq!(string_at(ours, "apiKeyEnv"), Some(API_KEY_ENV));
        assert_eq!(string_at(ours, "api"), Some("openai-completions"));
        assert_eq!(string_at(ours, "baseURL"), Some("http://127.0.0.1:8931/v1"));
        let models = ours.get("models").and_then(Value::as_sequence).unwrap();
        assert_eq!(models.len(), 1, "DSH 只该看到网关的一个入口");
        assert_eq!(string_at(&models[0], "id"), Some(GATEWAY_ALIAS));
        assert_eq!(string_at(&models[0], "name"), Some(GATEWAY_ALIAS));

        let default = root.get(DEFAULT_MODEL_KEY).expect("default model set");
        assert_eq!(string_at(default, "provider"), Some(PROVIDER_KEY));
        assert_eq!(string_at(default, "model"), Some(GATEWAY_ALIAS));

        assert!(
            root.get("ui-onboarding").is_some(),
            "unrelated top-level keys must survive"
        );
    }

    #[test]
    fn reuses_an_existing_ai_start_provider_instead_of_adding_a_second() {
        let mut root = parse(
            "llm-pi-ai:\n  providers:\n    cmd:\n      apiKeyEnv: CMD_API_KEY\n    homesh:\n      apiKeyEnv: AISTART_API_KEY\n      baseURL: http://127.0.0.1:8931/v1\n",
        );

        let key = merge_settings(&mut root, &context()).expect("merge succeeds");
        assert_eq!(key, "homesh");

        let providers = providers_of(&root);
        assert_eq!(providers.len(), 2);
        assert!(!providers.contains_key(PROVIDER_KEY));
    }

    #[test]
    fn strip_removes_only_our_entries_and_leaves_others_alone() {
        let mut root = parse(
            "llm-pi-ai:\n  providers:\n    cmd:\n      apiKeyEnv: CMD_API_KEY\n    aistart:\n      apiKeyEnv: AISTART_API_KEY\n      baseURL: http://127.0.0.1:8931/v1\nagent-default-model:\n  provider: aistart\n  model: claude-sonnet-5\n",
        );

        assert_eq!(strip_settings(&mut root), Some(PROVIDER_KEY.to_string()));

        let providers = providers_of(&root);
        assert_eq!(providers.len(), 1);
        assert!(providers.contains_key("cmd"));
        assert!(root.get(DEFAULT_MODEL_KEY).is_none());
    }

    #[test]
    fn strip_keeps_a_default_model_that_points_somewhere_else() {
        let mut root = parse(
            "llm-pi-ai:\n  providers:\n    aistart:\n      apiKeyEnv: AISTART_API_KEY\nagent-default-model:\n  provider: cmd\n  model: k3\n",
        );

        assert_eq!(strip_settings(&mut root), Some(PROVIDER_KEY.to_string()));
        assert!(root.get(DEFAULT_MODEL_KEY).is_some());
    }

    #[test]
    fn strip_is_a_no_op_when_nothing_is_ours() {
        let mut root = parse("llm-pi-ai:\n  providers:\n    cmd:\n      apiKeyEnv: CMD_API_KEY\n");
        assert_eq!(strip_settings(&mut root), None);
        assert_eq!(providers_of(&root).len(), 1);
    }

    #[test]
    fn round_trips_a_realistic_dsh_config() {
        let mut root = parse(
            "ui-onboarding:\n  welcomeNoticeVersion: 2026-08-13.1\nllm-pi-ai:\n  providers:\n    cmd:\n      apiKeyEnv: CMD_API_KEY\n      api: openai-completions\n      baseURL: https://api.commandcode.ai/provider/v1/\n      models:\n        - id: deepseek/deepseek-v4.1-flash\n          name: DeepSeek V4.1 Flash\n          contextWindow: 1000000\n    kimi-coding:\n      apiKeyEnv: KIMI_CODING_API_KEY\n      api: anthropic-messages\n      baseURL: https://api.kimi.com/coding\n      models:\n        - id: k3\n          name: Kimi K3\n          maxTokens: 131072\nagent-default-model:\n  provider: cmd\n  model: deepseek/deepseek-v4.1-flash\ndsh-desktop:\n  mode: compatibility\n  networkExposure: loopback\n",
        );

        merge_settings(&mut root, &context()).expect("merge succeeds");
        let rendered = serde_yaml::to_string(&root).expect("serialises");
        let reparsed: Value = serde_yaml::from_str(&rendered).expect("round-trips");

        let providers = providers_of(&reparsed);
        assert_eq!(providers.len(), 3);
        let kimi = providers.get("kimi-coding").expect("kimi kept");
        assert_eq!(
            string_at(kimi, "baseURL"),
            Some("https://api.kimi.com/coding")
        );
        assert_eq!(string_at(kimi, "api"), Some("anthropic-messages"));
        let kimi_models = kimi.get("models").and_then(Value::as_sequence).unwrap();
        assert_eq!(
            kimi_models[0].get("name").and_then(Value::as_str),
            Some("Kimi K3")
        );
        assert_eq!(
            kimi_models[0].get("maxTokens").and_then(Value::as_i64),
            Some(131072)
        );

        let cmd_models = providers
            .get("cmd")
            .and_then(|cmd| cmd.get("models"))
            .and_then(Value::as_sequence)
            .unwrap();
        assert_eq!(
            cmd_models[0].get("contextWindow").and_then(Value::as_i64),
            Some(1_000_000)
        );

        assert_eq!(
            reparsed
                .get("dsh-desktop")
                .and_then(|item| item.get("networkExposure"))
                .and_then(Value::as_str),
            Some("loopback")
        );
        assert_eq!(
            reparsed
                .get("ui-onboarding")
                .and_then(|item| item.get("welcomeNoticeVersion"))
                .and_then(Value::as_str),
            Some("2026-08-13.1")
        );
    }

    #[test]
    fn credentials_merge_preserves_existing_refs() {
        let mut root = parse(
            "version: 1\nrecords:\n  client-connection/browser-session:\n    kind: grant\n    payload:\n      version: 1\n      secret: abc123\nrefs:\n  CMD_API_KEY: user_x\n",
        );

        merge_credentials(&mut root, "deepseek-desktop").expect("merge succeeds");

        let refs = root.get(REFS_KEY).and_then(Value::as_mapping).unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(
            string_at(root.get(REFS_KEY).unwrap(), "CMD_API_KEY"),
            Some("user_x")
        );
        assert_eq!(
            string_at(root.get(REFS_KEY).unwrap(), API_KEY_ENV),
            Some("deepseek-desktop")
        );

        let record = root
            .get("records")
            .and_then(|records| records.get("client-connection/browser-session"))
            .expect("record with a slash in its key survives");
        assert_eq!(string_at(record, "kind"), Some("grant"));
        assert_eq!(
            record
                .get("payload")
                .and_then(|payload| payload.get("secret"))
                .and_then(Value::as_str),
            Some("abc123")
        );

        assert!(strip_credentials(&mut root));
        let refs = root.get(REFS_KEY).and_then(Value::as_mapping).unwrap();
        assert_eq!(refs.len(), 1);
        assert!(refs.contains_key("CMD_API_KEY"));
        assert!(root.get("records").is_some());
    }
}

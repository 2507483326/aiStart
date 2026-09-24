//! ZCode（智谱 GLM 官方 ADE 桌面端）配置读写。
//!
//! ZCode 把模型供应商落盘在数据目录的 `v2/config.json` 里。数据根由 `ZCODE_DATA_BASE_DIR`
//! 决定，其次 `HOME`，最后回落到用户主目录（与它自己的 `getDataBaseDir` 顺序一致）——
//! 默认即 `%USERPROFILE%\.zcode\v2\config.json`。
//!
//! 顶层 `provider` 是一张 `id → 供应商` 的表：内置供应商用 `builtin:<name>` 作 key，
//! 在 GUI 里加的「自定义供应商」用 UUID 作 key。每条供应商用 `kind` 决定线协议，凭据与
//! 地址放在 `options` 里。`kind` 的取值与 ZCode 会自行拼上的路径（写 baseURL 时按这个来填）：
//!
//! - `anthropic`         → `/v1/messages`
//! - `openai`            → `/responses`
//! - `openai-compatible` → `/chat/completions`
//!
//! 这里用 `kind = "anthropic"`：ZCode 自带的供应商全是这个档，`baseURL` 只填网关源地址，
//! 它会自己接上 `/v1/messages`（网关的 Messages 入口），与「手动对接」提示里的地址一致。
//!
//! 只做「合并」：新增或更新 aiStart 自己的那一条供应商，文件里其余的供应商、`$schema`
//! 与未知字段原样保留。刻意**不碰** `credentials.json`：那是 `enc:v1:` 加密的官方登录缓存。
//! 绑定关系仍记在 `settings` 里，网关据此把入站请求归到来源应用。

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::domain::app::{AppDescriptor, AppKind, ApplyMode, ApplyReport};
use crate::domain::catalog;
use crate::error::{AppError, AppResult};
use crate::platform::{
    gateway_alias_choice, AppConfigurator, ApplyContext, DetectResult, ModelChoice, GATEWAY_ALIAS,
};

/// aiStart 在 `provider` 表里使用的 id。可读且固定，重复应用时覆盖自己而不新增。
pub const PROVIDER_ID: &str = "aistart";
/// 供应商表里的显示名。
const PROVIDER_NAME: &str = "aiStart";
/// 线协议：Anthropic Messages。ZCode 会按 kind 自行接上 `/v1/messages`。
const PROVIDER_KIND: &str = "anthropic";

/// 顶层那张 id → 供应商的表。
const PROVIDERS_KEY: &str = "provider";

/// 数据根：`ZCODE_DATA_BASE_DIR` 优先，其次 `HOME`，最后回落到用户主目录。
fn resolve_base(env_dir: Option<String>, env_home: Option<String>) -> PathBuf {
    if let Some(dir) = env_dir.filter(|dir| !dir.trim().is_empty()) {
        return PathBuf::from(dir.trim());
    }
    if let Some(home) = env_home.filter(|home| !home.trim().is_empty()) {
        return PathBuf::from(home.trim());
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// ZCode 的数据目录（桌面端的 `v2` 状态目录）。
pub fn zcode_home() -> PathBuf {
    resolve_base(
        std::env::var("ZCODE_DATA_BASE_DIR").ok(),
        std::env::var("HOME").ok(),
    )
    .join(".zcode")
    .join("v2")
}

pub fn config_path() -> PathBuf {
    zcode_home().join("config.json")
}

/// 网关源地址；ZCode 按 kind 自己接上协议路径，所以这里不带 `/v1`。
fn gateway_origin(ctx: &ApplyContext) -> String {
    ctx.gateway_base_url.trim_end_matches('/').to_string()
}

/// 读一份配置；文件不存在或为空时当作空对象。解析失败会报错而不是返回空，
/// 避免把看不懂的内容当成「空文件」覆盖掉。
fn load(path: &Path) -> AppResult<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let text = std::fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    let value: Value = serde_json::from_str(&text).map_err(|error| {
        AppError::Message(format!(
            "{} 不是合法 JSON（{error}），已放弃写入以免破坏现有配置",
            path.display()
        ))
    })?;
    if !value.is_object() {
        return Err(AppError::Message(format!(
            "{} 顶层不是对象，已放弃写入以免破坏现有配置",
            path.display()
        )));
    }
    Ok(value)
}

fn save(path: &Path, root: &Value) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = serde_json::to_string_pretty(root)?;
    text.push('\n');
    std::fs::write(path, text)?;
    Ok(())
}

/// 取顶层 `provider` 表，不存在就建。已存在但不是对象时报错而不是覆盖。
fn provider_slot(root: &mut Value) -> AppResult<&mut Map<String, Value>> {
    let object = root
        .as_object_mut()
        .ok_or_else(|| AppError::Message("配置顶层不是对象，已放弃写入以免覆盖现有配置".into()))?;
    if object.get(PROVIDERS_KEY).is_none() {
        object.insert(PROVIDERS_KEY.into(), Value::Object(Map::new()));
    }
    object
        .get_mut(PROVIDERS_KEY)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| AppError::Message("provider 不是对象，已放弃写入以免覆盖现有配置".into()))
}

/// 一条自定义供应商条目：kind 决定协议，options 带凭据与源地址，models 列出可见模型。
fn entry(ctx: &ApplyContext) -> Value {
    let context_window = if ctx.model.supports_1m {
        1_000_000
    } else {
        200_000
    };

    let mut model = Map::new();
    model.insert("name".into(), json!(GATEWAY_ALIAS));
    model.insert("limit".into(), json!({ "context": context_window }));
    model.insert(
        "modalities".into(),
        json!({ "input": ["text", "image"], "output": ["text"] }),
    );
    model.insert("zcode".into(), json!({ "modified": false }));

    let mut models = Map::new();
    models.insert(GATEWAY_ALIAS.to_string(), Value::Object(model));

    json!({
        "name": PROVIDER_NAME,
        "kind": PROVIDER_KIND,
        "options": {
            "apiKey": ctx.gateway_token,
            "baseURL": gateway_origin(ctx),
            "apiKeyRequired": true,
        },
        "enabled": true,
        "source": "custom",
        "models": Value::Object(models),
    })
}

/// 把 aiStart 的供应商合并进 `provider`：固定 key，重复应用只覆盖自己。
fn merge_provider(root: &mut Value, ctx: &ApplyContext) -> AppResult<()> {
    provider_slot(root)?.insert(PROVIDER_ID.into(), entry(ctx));
    Ok(())
}

/// 摘掉 aiStart 的供应商；`provider` 因此空了就把这张表也收干净。
/// 返回是否真的摘掉了。
fn strip(root: &mut Value) -> bool {
    let Some(object) = root.as_object_mut() else {
        return false;
    };
    let Some(providers) = object.get_mut(PROVIDERS_KEY).and_then(Value::as_object_mut) else {
        return false;
    };
    let removed = providers.remove(PROVIDER_ID).is_some();
    let empty = providers.is_empty();
    if removed && empty {
        object.remove(PROVIDERS_KEY);
    }
    removed
}

/// 除了条目还在，还要求它没被停用 —— 用户可能在 GUI 里把它关掉了。
fn configured_in(root: &Value) -> bool {
    root.get(PROVIDERS_KEY)
        .and_then(|providers| providers.get(PROVIDER_ID))
        .is_some_and(|entry| {
            entry
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true)
        })
}

pub fn is_configured() -> bool {
    let Ok(root) = load(&config_path()) else {
        return false;
    };
    configured_in(&root)
}

pub fn apply(ctx: &ApplyContext) -> AppResult<ApplyReport> {
    let path = config_path();
    let mut root = load(&path)?;
    merge_provider(&mut root, ctx)?;
    save(&path, &root)?;

    let mut steps = vec![
        format!(
            "合并写入 {}（其它供应商与未知字段保持不变）",
            path.display()
        ),
        format!(
            "provider.{PROVIDER_ID}: kind = \"{PROVIDER_KIND}\"，地址 {}（ZCode 自接 /v1/messages）",
            gateway_origin(ctx)
        ),
        format!(
            "网关 Key 写在 options.apiKey（{} 专属）",
            ctx.gateway_token
        ),
        format!("model = {GATEWAY_ALIAS}（网关别名）"),
        format!(
            "上游模型: {} ({})",
            ctx.model.model,
            ctx.model.format.display_name()
        ),
        format!("在 ZCode 的模型选择器里选一次 {GATEWAY_ALIAS}，用量会归到「zcode」来源"),
    ];
    if ctx.model.supports_1m {
        steps.push("已把条目上下文窗口标到 1M".into());
    }
    steps.push("完全退出并重新打开 ZCode 后生效".into());

    Ok(ApplyReport {
        kind: AppKind::ZCode,
        model_id: ctx.model.id,
        model_name: ctx.model.name.clone(),
        apply_mode: ApplyMode::DirectConfig,
        target: format!("{} → provider.{PROVIDER_ID}", path.display()),
        restart_required: true,
        steps,
        note: Some(
            "只合并 aiStart 自己的那一条供应商，不改动文件里的其它供应商与未知字段；需要还原时用「移除模型配置」。ZCode 的自定义供应商要在它的模型选择器里选一次才成为当前模型。credentials.json 是官方登录缓存，本应用不会写它。"
                .into(),
        ),
    })
}

pub fn clear() -> AppResult<()> {
    let path = config_path();
    if !path.exists() {
        return Ok(());
    }
    let mut root = load(&path)?;
    if !strip(&mut root) {
        return Ok(());
    }
    // 表都清空后如果文件只剩空白，说明是我们建的：删掉，还原成「从未配置」。
    if root.as_object().is_some_and(|object| object.is_empty()) {
        std::fs::remove_file(&path)?;
    } else {
        save(&path, &root)?;
    }
    Ok(())
}

pub struct ZCodeConfigurator;

impl AppConfigurator for ZCodeConfigurator {
    fn descriptor(&self) -> AppDescriptor {
        catalog::builtin_app(AppKind::ZCode)
    }

    fn detect(&self) -> AppResult<DetectResult> {
        #[cfg(windows)]
        return Ok(super::windows::detect_by_descriptor(&self.descriptor()));
        #[cfg(not(windows))]
        return Ok(DetectResult::missing());
    }

    fn is_configured(&self) -> AppResult<bool> {
        Ok(is_configured())
    }

    /// ZCode 只认单个模型入口，给网关别名一个就够。
    fn exposed_models(&self) -> Vec<ModelChoice> {
        vec![gateway_alias_choice()]
    }

    fn apply(&self, ctx: &ApplyContext) -> AppResult<ApplyReport> {
        apply(ctx)
    }

    fn clear(&self) -> AppResult<()> {
        clear()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::{ModelConfig, ModelFormat};

    fn context() -> ApplyContext {
        ApplyContext {
            model: ModelConfig {
                id: 5,
                name: "command".into(),
                format: ModelFormat::OpenaiCompletions,
                base_url: "https://api.commandcode.ai/provider/v1".into(),
                api_key: "sk-test".into(),
                model: "deepseek/deepseek-v4-flash".into(),
                supports_1m: false,
                created_at: String::new(),
                updated_at: String::new(),
            },
            gateway_base_url: "http://127.0.0.1:8931/".into(),
            gateway_token: "zcode".into(),
            model_choices: vec![gateway_alias_choice()],
        }
    }

    fn ours(root: &Value) -> &Value {
        &root[PROVIDERS_KEY][PROVIDER_ID]
    }

    #[test]
    fn writes_the_shape_zcode_actually_reads() {
        let mut root = json!({});
        merge_provider(&mut root, &context()).expect("merge succeeds");

        let entry = ours(&root);
        assert_eq!(entry["name"], "aiStart");
        assert_eq!(entry["kind"], PROVIDER_KIND);
        assert_eq!(entry["enabled"], json!(true));
        assert_eq!(entry["source"], "custom");
        // baseURL 只填源地址，ZCode 自己接 /v1/messages。
        assert_eq!(entry["options"]["baseURL"], "http://127.0.0.1:8931");
        assert_eq!(entry["options"]["apiKey"], "zcode");
        assert_eq!(entry["options"]["apiKeyRequired"], json!(true));
        // 模型列表里必须有这个别名，否则选择器看不到。
        assert_eq!(entry["models"][GATEWAY_ALIAS]["name"], GATEWAY_ALIAS);
    }

    #[test]
    fn merges_into_an_existing_config_without_touching_other_providers() {
        let mut root = json!({
            "$schema": "zcode.config.v1",
            "provider": {
                "builtin:bigmodel": {
                    "name": "Bigmodel - API Key",
                    "kind": "anthropic",
                    "options": { "apiKey": "keep-me", "baseURL": "https://open.bigmodel.cn/api/anthropic" },
                    "models": { "GLM-5.3": { "limit": { "context": 1000000 } } }
                },
                "12d4a066-7743-4255-a8dd-6df95f8787e0": {
                    "name": "opengo",
                    "kind": "anthropic",
                    "options": { "apiKey": "sk-opengo", "baseURL": "https://opencode.ai/zen/go/v1" }
                }
            }
        });

        merge_provider(&mut root, &context()).expect("merge succeeds");

        // 别的供应商原样保留。
        assert_eq!(
            root[PROVIDERS_KEY]["builtin:bigmodel"]["options"]["apiKey"],
            "keep-me"
        );
        assert_eq!(
            root[PROVIDERS_KEY]["12d4a066-7743-4255-a8dd-6df95f8787e0"]["options"]["baseURL"],
            "https://opencode.ai/zen/go/v1"
        );
        assert_eq!(root["$schema"], "zcode.config.v1");
        assert_eq!(root[PROVIDERS_KEY].as_object().unwrap().len(), 3);
    }

    #[test]
    fn rewrites_a_stale_entry_instead_of_adding_a_second_provider() {
        let mut root = json!({
            "provider": {
                PROVIDER_ID: {
                    "name": "old",
                    "kind": "anthropic",
                    "options": { "apiKey": "stale", "baseURL": "http://127.0.0.1:9999" }
                }
            }
        });

        merge_provider(&mut root, &context()).expect("merge succeeds");

        let providers = root[PROVIDERS_KEY].as_object().expect("providers table");
        assert_eq!(providers.len(), 1, "不该新增第二条");
        assert_eq!(ours(&root)["options"]["apiKey"], "zcode");
        assert_eq!(ours(&root)["options"]["baseURL"], "http://127.0.0.1:8931");
    }

    #[test]
    fn supports_1m_bumps_the_context_window() {
        let mut small = json!({});
        merge_provider(&mut small, &context()).expect("merge succeeds");
        assert_eq!(
            ours(&small)["models"][GATEWAY_ALIAS]["limit"]["context"],
            json!(200_000)
        );

        let mut big = json!({});
        let mut with_1m = context();
        with_1m.model.supports_1m = true;
        merge_provider(&mut big, &with_1m).expect("merge succeeds");
        assert_eq!(
            ours(&big)["models"][GATEWAY_ALIAS]["limit"]["context"],
            json!(1_000_000)
        );
    }

    #[test]
    fn strip_removes_only_our_provider() {
        let mut root = json!({
            "$schema": "zcode.config.v1",
            "provider": {
                PROVIDER_ID: { "name": PROVIDER_NAME, "kind": PROVIDER_KIND },
                "builtin:bigmodel": { "name": "Bigmodel - API Key" }
            }
        });

        assert!(strip(&mut root));

        assert!(root[PROVIDERS_KEY].get(PROVIDER_ID).is_none());
        assert!(root[PROVIDERS_KEY].get("builtin:bigmodel").is_some());
        assert_eq!(root["$schema"], "zcode.config.v1");
    }

    #[test]
    fn strip_cleans_up_the_table_it_created() {
        let mut root = json!({});
        merge_provider(&mut root, &context()).expect("merge succeeds");
        assert!(strip(&mut root));
        assert!(root.as_object().expect("object").is_empty());
    }

    #[test]
    fn strip_is_a_no_op_when_nothing_is_ours() {
        let mut root = json!({ "provider": { "builtin:bigmodel": { "name": "x" } } });
        assert!(!strip(&mut root));
        assert!(root[PROVIDERS_KEY].get("builtin:bigmodel").is_some());
    }

    #[test]
    fn configured_only_while_the_entry_is_present_and_enabled() {
        let mut applied = json!({});
        merge_provider(&mut applied, &context()).expect("merge succeeds");
        assert!(configured_in(&applied));

        let mut disabled = applied.clone();
        disabled[PROVIDERS_KEY][PROVIDER_ID]["enabled"] = json!(false);
        assert!(!configured_in(&disabled), "被停用就不算「使用中」");

        assert!(!configured_in(&json!({ "provider": {} })));
    }

    #[test]
    fn refuses_shapes_it_cannot_merge_into() {
        let mut wrong_type = json!({ "provider": 42 });
        assert!(merge_provider(&mut wrong_type, &context()).is_err());
    }

    #[test]
    fn round_trips_a_real_file_on_disk() {
        let dir = std::env::temp_dir().join(format!("aistart-zcode-{}", std::process::id()));
        let path = dir.join("config.json");
        let _ = std::fs::remove_dir_all(&dir);

        let mut root = load(&path).expect("missing file counts as an empty document");
        merge_provider(&mut root, &context()).expect("merge succeeds");
        save(&path, &root).expect("save creates the directory");

        let written = std::fs::read_to_string(&path).expect("file written");
        assert!(written.contains("\"aistart\""), "{written}");

        let mut reloaded = load(&path).expect("our own output must be readable");
        assert!(configured_in(&reloaded));
        assert!(strip(&mut reloaded));
        assert!(
            reloaded.as_object().expect("object").is_empty(),
            "provider 清掉后文件应为空，clear 会据此删文件"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn base_dir_prefers_the_env_var_then_home() {
        assert_eq!(
            resolve_base(
                Some(r"D:\zcode-data".into()),
                Some(r"C:\Users\someone".into())
            ),
            PathBuf::from(r"D:\zcode-data")
        );
        // 空串等同于没设置，回落到 HOME。
        assert_eq!(
            resolve_base(Some("   ".into()), Some(r"C:\Users\someone".into())),
            PathBuf::from(r"C:\Users\someone")
        );
        assert!(resolve_base(None, None).is_absolute() || resolve_base(None, None).exists());
    }

    #[test]
    fn config_path_hangs_off_the_data_dir() {
        assert!(
            config_path().ends_with(r".zcode\v2\config.json")
                || config_path().ends_with(".zcode/v2/config.json")
        );
    }
}

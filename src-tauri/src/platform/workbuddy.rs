//! WorkBuddy（腾讯 AI Agent 办公工作台）自定义模型配置读写。
//!
//! WorkBuddy 把「本地自定义模型」放在用户目录的 `models.json` 里，由内置的
//! `CustomModelsProvider` 读取并**监听文件变更**（约 1s 去抖后触发 product 同步），
//! 因此写完后无需重启。文件有两种形状：GUI 保存的顶级数组，或
//! `{ models: [...], availableModels: [...] }` 对象包裹。这里只做「合并」：
//! 新增或更新 aiStart 自己的条目，其余条目与整体形状原样保留。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::domain::app::{AppDescriptor, AppKind, ApplyMode, ApplyReport};
use crate::domain::catalog;
use crate::error::{AppError, AppResult};
use crate::platform::{
    gateway_alias_choice, AppConfigurator, ApplyContext, DetectResult, ModelChoice, GATEWAY_ALIAS,
};

/// 配置目录名。WorkBuddy 与 CodeBuddy 系产品共用同一套读取逻辑，只有目录名不同
/// （WorkBuddy 产品名含 "workbuddy" 时用 `.workbuddy`，否则 `.codebuddy`）。
const WORKBUDDY_DIR: &str = ".workbuddy";
const CODEBUDDY_DIR: &str = ".codebuddy";

/// 配置目录：环境变量优先，其次 `~/.workbuddy`，再退到 `~/.codebuddy`。
fn config_dir() -> PathBuf {
    for key in ["WORKBUDDY_CONFIG_DIR", "CODEBUDDY_CONFIG_DIR"] {
        if let Ok(dir) = std::env::var(key) {
            if !dir.trim().is_empty() {
                return PathBuf::from(dir.trim());
            }
        }
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let workbuddy = home.join(WORKBUDDY_DIR);
    if workbuddy.exists() {
        return workbuddy;
    }
    let codebuddy = home.join(CODEBUDDY_DIR);
    if codebuddy.exists() {
        return codebuddy;
    }
    workbuddy
}

pub fn models_path() -> PathBuf {
    config_dir().join("models.json")
}

/// 网关的 OpenAI 兼容入口。WorkBuddy 要求这里填**完整**地址，校验也要求以
/// `/chat/completions` 结尾；运行时见该后缀就不再自动补全。
fn chat_endpoint(ctx: &ApplyContext) -> String {
    format!(
        "{}/v1/chat/completions",
        ctx.gateway_base_url.trim_end_matches('/')
    )
}

/// 读一份配置；文件不存在或为空时当作空数组。解析失败会报错而不是返回空，
/// 避免把看不懂的内容当成「空文件」覆盖掉。
fn load(path: &Path) -> AppResult<Value> {
    if !path.exists() {
        return Ok(Value::Array(Vec::new()));
    }
    let text = std::fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    let value: Value = serde_json::from_str(&text).map_err(|error| {
        AppError::Message(format!(
            "{} 不是合法 JSON（{error}），已放弃写入以免破坏现有配置",
            path.display()
        ))
    })?;
    match value {
        Value::Array(_) | Value::Object(_) => Ok(value),
        _ => Err(AppError::Message(format!(
            "{} 顶层不是数组或对象，已放弃写入以免覆盖现有配置",
            path.display()
        ))),
    }
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

/// 只读地取出模型列表，兼容数组与对象两种形状。
fn models(root: &Value) -> Option<&Vec<Value>> {
    match root {
        Value::Array(items) => Some(items),
        Value::Object(_) => root.get("models").and_then(Value::as_array),
        _ => None,
    }
}

/// 可写地取出模型列表；对象形状缺少 `models` 时补一个空数组，但**不覆盖**其它键。
fn models_slot(root: &mut Value) -> AppResult<&mut Vec<Value>> {
    match root {
        Value::Array(items) => Ok(items),
        Value::Object(map) => map
            .entry("models".to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| {
                AppError::Message("models 字段不是数组，已放弃写入以免覆盖现有配置".into())
            }),
        _ => Err(AppError::Message(
            "models.json 顶层不是数组或对象，已放弃写入以免覆盖现有配置".into(),
        )),
    }
}

fn is_ours(entry: &Value) -> bool {
    entry.get("id").and_then(Value::as_str) == Some(GATEWAY_ALIAS)
}

fn points_at_gateway(entry: &Value, gateway_base_url: Option<&str>) -> bool {
    let Some(base) = gateway_base_url
        .map(|url| url.trim_end_matches('/'))
        .filter(|url| !url.is_empty())
    else {
        return false;
    };
    entry
        .get("url")
        .and_then(Value::as_str)
        .is_some_and(|url| url.starts_with(base))
}

/// 找出这份配置里已经属于 aiStart 的条目：先认固定 id，再认 url 已指向本地网关的，
/// 避免在 GUI 里手加过一次后再自动写入时重复添加。
fn owned_index(models: &[Value], gateway_base_url: Option<&str>) -> Option<usize> {
    if let Some(index) = models.iter().position(is_ours) {
        return Some(index);
    }
    models
        .iter()
        .position(|entry| points_at_gateway(entry, gateway_base_url))
}

/// 生成我们的条目；已有条目里的其他字段（用户手改的 token 上限、reasoning 等）保留。
fn entry_value(ctx: &ApplyContext, existing: Option<&Value>) -> Value {
    let mut map = existing
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    map.insert("id".into(), json!(GATEWAY_ALIAS));
    map.insert("name".into(), json!(GATEWAY_ALIAS));
    map.insert("vendor".into(), json!("aiStart"));
    map.insert("url".into(), json!(chat_endpoint(ctx)));
    map.insert("apiKey".into(), json!(ctx.gateway_token));
    map.insert("supportsToolCall".into(), json!(true));
    map.insert("supportsImages".into(), json!(false));
    map.insert("supportsReasoning".into(), json!(false));
    map.insert("useCustomProtocol".into(), json!(false));
    Value::Object(map)
}

/// 合并 aiStart 的条目。注意**不写 `availableModels`**：WorkBuddy 一旦读到该字段就
/// 用它**替换**可用模型集合（不与内置模型合并），写了会把自带模型全部隐藏。
fn merge_models(root: &mut Value, ctx: &ApplyContext) -> AppResult<()> {
    let base = ctx.gateway_base_url.trim_end_matches('/').to_string();
    let slot = models_slot(root)?;
    let index = owned_index(slot, Some(&base));
    let existing = index.map(|index| slot[index].clone());
    let entry = entry_value(ctx, existing.as_ref());
    match index {
        Some(index) => slot[index] = entry,
        None => slot.push(entry),
    }
    Ok(())
}

/// 移除 aiStart 的条目，其余条目保留。返回是否真的删掉了东西。
fn strip_models(root: &mut Value) -> AppResult<bool> {
    let Ok(slot) = models_slot(root) else {
        return Ok(false);
    };
    let Some(index) = owned_index(slot, None) else {
        return Ok(false);
    };
    slot.remove(index);
    Ok(true)
}

pub fn is_configured() -> bool {
    let Ok(root) = load(&models_path()) else {
        return false;
    };
    models(&root).is_some_and(|list| owned_index(list, None).is_some())
}

pub fn apply(ctx: &ApplyContext) -> AppResult<ApplyReport> {
    let path = models_path();
    let mut root = load(&path)?;
    merge_models(&mut root, ctx)?;
    save(&path, &root)?;

    let endpoint = chat_endpoint(ctx);
    Ok(ApplyReport {
        kind: AppKind::WorkBuddy,
        model_id: ctx.model.id,
        model_name: ctx.model.name.clone(),
        apply_mode: ApplyMode::DirectConfig,
        target: format!("{} → models[{}]", path.display(), GATEWAY_ALIAS),
        restart_required: false,
        steps: vec![
            format!(
                "合并写入 {}（GUI 里添加的其它模型保持不变）",
                path.display()
            ),
            format!("接口地址 {endpoint} 指向本地网关"),
            format!("网关 Key 写入该条目的 apiKey（{} 专属）", ctx.gateway_token),
            format!(
                "上游模型: {} ({})",
                ctx.model.model,
                ctx.model.format.display_name()
            ),
            "WorkBuddy 会监听该文件并自动同步，无需重启；若模型选择器未刷新，新建一个对话即可".into(),
        ],
        note: Some(
            "只合并 aiStart 自己的条目，不动 models.json 里的其他模型；需要还原时用「移除模型配置」。注意：企业管理员若禁用了「个人自定义模型」，该条目会被 WorkBuddy 忽略。"
                .into(),
        ),
    })
}

pub fn clear() -> AppResult<()> {
    let path = models_path();
    if !path.exists() {
        return Ok(());
    }
    let mut root = load(&path)?;
    if !strip_models(&mut root)? {
        return Ok(());
    }
    // 数组形状是我们自己建的：空了就整个删掉，还原成「从未配置」。
    // 对象形状可能还挂着用户手写的 availableModels，只把 models 清空、保留其余内容。
    let empty_array = root.as_array().is_some_and(Vec::is_empty);
    if empty_array {
        std::fs::remove_file(&path)?;
    } else {
        save(&path, &root)?;
    }
    Ok(())
}

pub struct WorkBuddyConfigurator;

impl AppConfigurator for WorkBuddyConfigurator {
    fn descriptor(&self) -> AppDescriptor {
        catalog::builtin_app(AppKind::WorkBuddy)
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

    /// WorkBuddy 只认单个模型入口，给网关别名一个就够。
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
    use crate::platform::{gateway_alias_choice, GATEWAY_ALIAS};

    fn parse(text: &str) -> Value {
        serde_json::from_str(text).expect("test fixture is valid json")
    }

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
            gateway_base_url: "http://127.0.0.1:8931".into(),
            gateway_token: "workbuddy".into(),
            model_choices: vec![gateway_alias_choice()],
        }
    }

    fn entry(root: &Value) -> &Value {
        let list = models(root).expect("array-shaped model list");
        let index = list
            .iter()
            .position(is_ours)
            .expect("aiStart entry present");
        &list[index]
    }

    fn string_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
        value.get(key).and_then(Value::as_str)
    }

    #[test]
    fn merges_into_an_existing_array_without_touching_other_models() {
        let mut root = parse(
            r#"[
                { "id": "glm-5.0", "name": "GLM 5.0", "vendor": "Zhipu", "url": "https://open.bigmodel.cn/api/paas/v4/chat/completions", "apiKey": "user-key" }
            ]"#,
        );

        merge_models(&mut root, &context()).expect("merge succeeds");

        let list = models(&root).expect("still an array");
        assert_eq!(list.len(), 2, "existing model must survive");
        let other = &list[0];
        assert_eq!(string_at(other, "id"), Some("glm-5.0"));
        assert_eq!(string_at(other, "apiKey"), Some("user-key"));

        let ours = entry(&root);
        assert_eq!(string_at(ours, "id"), Some(GATEWAY_ALIAS));
        assert_eq!(string_at(ours, "name"), Some(GATEWAY_ALIAS));
        assert_eq!(string_at(ours, "apiKey"), Some("workbuddy"));
        assert_eq!(
            string_at(ours, "url"),
            Some("http://127.0.0.1:8931/v1/chat/completions")
        );
        assert_eq!(ours.get("supportsToolCall"), Some(&json!(true)));
        assert_eq!(ours.get("useCustomProtocol"), Some(&json!(false)));
        assert!(
            ours.get("availableModels").is_none(),
            "不得写 availableModels：会隐藏 WorkBuddy 自带模型"
        );
    }

    #[test]
    fn keeps_the_object_shape_and_available_models() {
        let mut root = parse(
            r#"{
                "models": [ { "id": "old", "url": "https://example.com/v1/chat/completions" } ],
                "availableModels": ["glm-5.0"]
            }"#,
        );

        merge_models(&mut root, &context()).expect("merge succeeds");

        assert!(!root.is_array(), "对象形状必须保留");
        let list = models(&root).expect("models array");
        assert_eq!(list.len(), 2);
        assert_eq!(
            root.get("availableModels"),
            Some(&json!(["glm-5.0"])),
            "availableModels 原样保留"
        );
    }

    #[test]
    fn reuses_an_entry_that_already_points_at_the_gateway() {
        let mut root = parse(
            r#"[
                { "id": "my-gateway", "name": "本机网关", "url": "http://127.0.0.1:8931/v1/chat/completions", "apiKey": "stale", "maxOutputTokens": 4096 }
            ]"#,
        );

        merge_models(&mut root, &context()).expect("merge succeeds");

        let list = models(&root).expect("models array");
        assert_eq!(list.len(), 1, "不该新增第二条");
        let ours = &list[0];
        assert_eq!(
            string_at(ours, "id"),
            Some(GATEWAY_ALIAS),
            "认领后 id 归一为网关别名"
        );
        assert_eq!(string_at(ours, "apiKey"), Some("workbuddy"));
        assert_eq!(
            ours.get("maxOutputTokens"),
            Some(&json!(4096)),
            "用户手改的字段保留"
        );
    }

    #[test]
    fn strip_removes_only_our_entry() {
        let mut root = parse(
            r#"[
                { "id": "glm-5.0" },
                { "id": "aiStart", "url": "http://127.0.0.1:8931/v1/chat/completions" }
            ]"#,
        );

        assert!(strip_models(&mut root).expect("strip succeeds"));
        let list = models(&root).expect("models array");
        assert_eq!(list.len(), 1);
        assert_eq!(string_at(&list[0], "id"), Some("glm-5.0"));

        assert!(!strip_models(&mut root).expect("no-op"), "第二次无可删");
        assert_eq!(models(&root).expect("models array").len(), 1);
    }

    #[test]
    fn strip_reports_nothing_removed_when_the_list_is_not_ours() {
        let mut root = parse(r#"[ { "id": "glm-5.0" } ]"#);
        assert!(!strip_models(&mut root).expect("no-op"));
        assert_eq!(models(&root).expect("models array").len(), 1);
    }

    #[test]
    fn round_trips_a_real_file_on_disk() {
        // 走真实的 load/save（apply 与 clear 用的就是这一对），覆盖「目录不存在时
        // 自动创建」与「写出来的是 2 空格缩进的数组」这两点。
        let dir = std::env::temp_dir().join(format!("aistart-workbuddy-{}", std::process::id()));
        let path = dir.join("models.json");
        let _ = std::fs::remove_dir_all(&dir);

        let mut root = load(&path).expect("missing file counts as an empty array");
        assert!(models(&root).expect("array").is_empty());
        merge_models(&mut root, &context()).expect("merge succeeds");
        save(&path, &root).expect("save creates the directory");

        let written = std::fs::read_to_string(&path).expect("file written");
        assert!(written.contains("\n  {"), "应为 2 空格缩进:\n{written}");
        assert!(
            written.contains("\"url\": \"http://127.0.0.1:8931/v1/chat/completions\""),
            "{written}"
        );

        let mut reloaded = load(&path).expect("our own output must be readable");
        assert!(models(&reloaded).expect("array").iter().any(is_ours));

        assert!(strip_models(&mut reloaded).expect("strip succeeds"));
        assert!(
            reloaded.as_array().is_some_and(Vec::is_empty),
            "条目删完后数组应为空，clear 会据此删文件"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_shapes_it_cannot_merge_into() {
        let mut number = json!(42);
        assert!(models_slot(&mut number).is_err());

        let mut wrong_type = parse(r#"{ "models": "not-a-list" }"#);
        assert!(models_slot(&mut wrong_type).is_err());
    }

    #[test]
    fn reads_both_shapes() {
        assert!(is_ours(&json!({ "id": "aiStart" })));
        assert!(!is_ours(&json!({ "id": "custom-local:aiStart" })));
        assert!(!is_ours(&json!({ "name": "aiStart" })));
        assert!(points_at_gateway(
            &json!({ "url": "http://127.0.0.1:8931/v1/chat/completions" }),
            Some("http://127.0.0.1:8931/")
        ));
        assert!(!points_at_gateway(
            &json!({ "url": "https://api.example.com/v1/chat/completions" }),
            Some("http://127.0.0.1:8931")
        ));
        assert!(!points_at_gateway(
            &json!({ "url": "http://127.0.0.1:8931/v1" }),
            None
        ));
    }
}

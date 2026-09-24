//! Codex（OpenAI 的 Codex 桌面版 / Codex CLI）配置读写。
//!
//! Codex 与 CLI 共用 Codex home（`CODEX_HOME`，默认 `~/.codex`）。自定义供应商写在
//! `config.toml` 里：顶层 `model_provider` 指向一个 `[model_providers.<id>]` 表，表里给出
//! `base_url`、`wire_api` 与 provider 级凭据。这一形状与 CC Switch 的做法一致，也是 Codex
//! 真正认的形状——**缺了 `model_provider` 声明，Codex 会回落到内置 `openai` provider，
//! 顶层的 base_url 被整个忽略**，请求直连 api.openai.com。
//!
//! 刻意**不碰 `auth.json`**：它存的是官方 ChatGPT / Codex 登录缓存，桌面版靠它识别官方账号、
//! 才能启用远程控制与官方插件。凭据写进 provider 表的 `experimental_bearer_token`，
//! 与 CC Switch「切换第三方供应商时保留官方登录」的做法相同。
//!
//! 光写 provider 表还不够：**Codex 的模型列表（GUI 选择器与 `/model`）完全由
//! `model_catalog_json` 指向的目录决定**，不在目录里的 slug 会被直接忽略、顶层 `model`
//! 还会被桌面版回落成目录里的某个模型。所以应用时要把 aiStart 作为一条目录条目并进去
//! （CC Switch 同样是靠它自己的 `cc-switch-model-catalog.json` 让自定义模型可见的）。
//! 目录条目的字段很多（含大段提示词），凭空造会被 Codex 判为非法，因此这里**克隆目录里
//! 已有的条目**再改 slug / 显示名 / 上下文窗口。
//!
//! 这里只做「合并」：新增或更新 aiStart 自己的 provider、目录条目与顶层路由，文件里其余的
//! provider、模型条目、注释与未知字段原样保留。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use toml_edit::{value, DocumentMut, Item, Table, TableLike};

use crate::domain::app::{AppDescriptor, AppKind, ApplyMode, ApplyReport};
use crate::domain::catalog;
use crate::error::{AppError, AppResult};
use crate::platform::{
    gateway_alias_choice, AppConfigurator, ApplyContext, DetectResult, ModelChoice, GATEWAY_ALIAS,
};

/// aiStart 在 `[model_providers.*]` 下使用的 provider id。
/// 不能取 `openai` / `ollama` / `lmstudio` —— 那是 Codex 保留给内置 provider 的 id。
pub const PROVIDER_ID: &str = "aistart";
/// provider 表里的显示名。
const PROVIDER_NAME: &str = "aiStart";
/// Codex 的线协议。0.148 之后只接受 `responses`，其余值会让整份配置反序列化失败。
const WIRE_API: &str = "responses";

const MODEL_PROVIDER_KEY: &str = "model_provider";
const MODEL_KEY: &str = "model";
const PROVIDERS_KEY: &str = "model_providers";
/// 指向模型目录文件的路径（相对路径按 Codex home 解析）。
const CATALOG_KEY: &str = "model_catalog_json";

fn resolve_home(env: Option<String>) -> PathBuf {
    if let Some(dir) = env.filter(|dir| !dir.trim().is_empty()) {
        return PathBuf::from(dir.trim());
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".codex")
}

pub fn codex_home() -> PathBuf {
    resolve_home(std::env::var("CODEX_HOME").ok())
}

pub fn config_path() -> PathBuf {
    codex_home().join("config.toml")
}

/// 网关的根地址；Codex 会自行拼上 `/responses`。
fn gateway_base(ctx: &ApplyContext) -> String {
    format!("{}/v1", ctx.gateway_base_url.trim_end_matches('/'))
}

/// 读一份配置；文件不存在或为空时当作空文档。解析失败会报错而不是返回空，
/// 避免把看不懂的内容当成「空文件」覆盖掉。
fn load(path: &Path) -> AppResult<DocumentMut> {
    if !path.exists() {
        return Ok(DocumentMut::new());
    }
    let text = std::fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(DocumentMut::new());
    }
    text.parse::<DocumentMut>().map_err(|error| {
        AppError::Message(format!(
            "{} 不是合法 TOML（{error}），已放弃写入以免破坏现有配置",
            path.display()
        ))
    })
}

fn save(path: &Path, doc: &DocumentMut) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, doc.to_string())?;
    Ok(())
}

/// 取顶层 `model_providers` 下的 aiStart 表，不存在就建。已存在但不是表时报错而不是覆盖。
fn provider_slot(doc: &mut DocumentMut) -> AppResult<&mut dyn TableLike> {
    let root = doc.as_table_mut();
    if root.get(PROVIDERS_KEY).is_none() {
        // 隐式父表：只输出 `[model_providers.aistart]`，不会多出一行空的 `[model_providers]`。
        let mut parent = Table::new();
        parent.set_implicit(true);
        root.insert(PROVIDERS_KEY, Item::Table(parent));
    }
    let providers = root
        .get_mut(PROVIDERS_KEY)
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| {
            AppError::Message("model_providers 不是表，已放弃写入以免覆盖现有配置".into())
        })?;

    if providers.get(PROVIDER_ID).is_none() {
        providers.insert(PROVIDER_ID, Item::Table(Table::new()));
    }
    providers
        .get_mut(PROVIDER_ID)
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| {
            AppError::Message(format!(
                "model_providers.{PROVIDER_ID} 不是表，已放弃写入以免覆盖现有配置"
            ))
        })
}

/// 把 aiStart 的 provider 合并进 `[model_providers.aistart]`：只覆盖我们认识的字段，
/// 用户在该表里手加的键（如 `query_params`、`http_headers`）保留。
fn merge_provider(doc: &mut DocumentMut, ctx: &ApplyContext) -> AppResult<()> {
    let table = provider_slot(doc)?;
    table.insert("name", value(PROVIDER_NAME));
    table.insert("base_url", value(gateway_base(ctx)));
    table.insert("wire_api", value(WIRE_API));
    // 凭据挂在 provider 上，auth.json 里的官方登录缓存因此得以保留。
    table.insert("experimental_bearer_token", value(ctx.gateway_token.clone()));
    Ok(())
}

/// 顶层路由：把 Codex 指到 aiStart 这个 provider 与网关别名。
fn merge_routing(doc: &mut DocumentMut, ctx: &ApplyContext) {
    doc[MODEL_PROVIDER_KEY] = value(PROVIDER_ID);
    doc[MODEL_KEY] = value(ctx.gateway_model_id());
}

// ---------------------------------------------------------------------------
// 模型目录
//
// Codex 只认目录里有的 slug，所以「应用」必须把 aiStart 放进去。条目字段多且含大段
// 提示词，只能克隆已有条目；没有可克隆的条目时宁可不写目录，也不写一个 Codex 读不懂的
// 文件（那会让整个模型列表失效）。
// ---------------------------------------------------------------------------

fn resolve_catalog_path(home: &Path, raw: &str) -> PathBuf {
    let path = Path::new(raw);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        home.join(path)
    }
}

/// 当前配置指向的目录文件（相对路径按 Codex home 解析）。
fn catalog_pointer(home: &Path, doc: &DocumentMut) -> Option<PathBuf> {
    let raw = doc.as_table().get(CATALOG_KEY)?.as_str()?.trim();
    (!raw.is_empty()).then(|| resolve_catalog_path(home, raw))
}

/// 读一份目录；只有「带 `models` 数组的对象」才算数，其余一律当作不可用。
fn read_catalog(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    value.get("models")?.as_array()?;
    Some(value)
}

fn save_catalog(path: &Path, root: &Value) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = serde_json::to_string_pretty(root)?;
    text.push('\n');
    std::fs::write(path, text)?;
    Ok(())
}

/// 目录里任一可用的条目，用来当克隆模板。
fn catalog_template(root: &Value) -> Option<&Value> {
    root.get("models")?
        .as_array()?
        .iter()
        .find(|model| model.is_object())
}

fn is_ours(model: &Value) -> bool {
    model.get("slug").and_then(Value::as_str) == Some(GATEWAY_ALIAS)
}

/// 摘掉 aiStart 的条目，返回是否真的摘掉了。
fn drop_entry(root: &mut Value) -> bool {
    let Some(models) = root.get_mut("models").and_then(Value::as_array_mut) else {
        return false;
    };
    let before = models.len();
    models.retain(|model| !is_ours(model));
    models.len() != before
}

/// 克隆一条目录条目改成 aiStart。返回 None 表示没有模板可克隆。
fn entry_from_template(template: &Value, ctx: &ApplyContext) -> Option<Value> {
    let mut entry = template.as_object()?.clone();
    entry.insert("slug".into(), json!(GATEWAY_ALIAS));
    entry.insert("display_name".into(), json!(GATEWAY_ALIAS));
    entry.insert("description".into(), json!("aiStart 本地网关"));
    if ctx.model.supports_1m {
        entry.insert("context_window".into(), json!(1_000_000));
        entry.insert("max_context_window".into(), json!(1_000_000));
    }
    Some(Value::Object(entry))
}

/// 把 aiStart 条目并进 `models`：替换同名条目并放到最前，其余条目与其字段原样保留。
/// 返回是否真的写进去了（没有模板可克隆时为 false）。
fn merge_catalog(root: &mut Value, ctx: &ApplyContext) -> bool {
    let Some(entry) = catalog_template(root).and_then(|item| entry_from_template(item, ctx)) else {
        return false;
    };
    let Some(models) = root.get_mut("models").and_then(Value::as_array_mut) else {
        return false;
    };
    models.retain(|model| !is_ours(model));
    models.insert(0, entry);
    true
}

/// 把条目并进「当前正在用的」模型目录。就地合并那一个文件（本机上它通常是 CC Switch 写的
/// 那份），不另起一份、也不改指针，免得两个工具各写各的目录互相覆盖。
/// 返回 None 表示没写：要么没配置目录，要么目录里没有可克隆的条目。
fn apply_catalog(home: &Path, doc: &DocumentMut, ctx: &ApplyContext) -> AppResult<Option<PathBuf>> {
    let Some(path) = catalog_pointer(home, doc) else {
        return Ok(None);
    };
    let Some(mut root) = read_catalog(&path) else {
        return Ok(None);
    };
    if !merge_catalog(&mut root, ctx) {
        return Ok(None);
    }
    save_catalog(&path, &root)?;
    Ok(Some(path))
}

/// 从当前目录里摘掉我们的条目。
fn strip_catalog(home: &Path, doc: &DocumentMut) -> AppResult<()> {
    let Some(path) = catalog_pointer(home, doc) else {
        return Ok(());
    };
    let Some(mut root) = read_catalog(&path) else {
        return Ok(());
    };
    if drop_entry(&mut root) {
        save_catalog(&path, &root)?;
    }
    Ok(())
}

/// 移除 aiStart 的 provider；顶层路由若指向我们则一并清掉。
/// 返回是否真的删掉了 provider 表。
fn strip(doc: &mut DocumentMut) -> bool {
    let removed = doc
        .as_table_mut()
        .get_mut(PROVIDERS_KEY)
        .and_then(Item::as_table_like_mut)
        .and_then(|providers| providers.remove(PROVIDER_ID))
        .is_some();
    if !removed {
        return false;
    }

    let root = doc.as_table_mut();
    let routing_points_at_us = root
        .get(MODEL_PROVIDER_KEY)
        .and_then(Item::as_str)
        .is_some_and(|provider| provider == PROVIDER_ID);
    if routing_points_at_us {
        root.remove(MODEL_PROVIDER_KEY);
        let model_points_at_us = root
            .get(MODEL_KEY)
            .and_then(Item::as_str)
            .is_some_and(|model| model == GATEWAY_ALIAS);
        if model_points_at_us {
            root.remove(MODEL_KEY);
        }
    }

    // 我们建的容器空了就收干净，别留一个空表在别人的文件里。
    let providers_empty = root
        .get(PROVIDERS_KEY)
        .and_then(Item::as_table_like)
        .is_some_and(|providers| providers.is_empty());
    if providers_empty {
        root.remove(PROVIDERS_KEY);
    }
    true
}

/// 除了 provider 表还在，还必须确认顶层路由**当前**指向它 —— 用户可能在 GUI 里
/// 手动切回了别的 provider，那种情况下不该继续显示「使用中」。
fn configured_in(doc: &DocumentMut) -> bool {
    let root = doc.as_table();
    let routed = root
        .get(MODEL_PROVIDER_KEY)
        .and_then(Item::as_str)
        .is_some_and(|provider| provider == PROVIDER_ID);
    routed
        && root
            .get(PROVIDERS_KEY)
            .and_then(|providers| providers.get(PROVIDER_ID))
            .is_some()
}

pub fn is_configured() -> bool {
    let Ok(doc) = load(&config_path()) else {
        return false;
    };
    configured_in(&doc)
}

pub fn apply(ctx: &ApplyContext) -> AppResult<ApplyReport> {
    let path = config_path();
    let mut doc = load(&path)?;
    merge_provider(&mut doc, ctx)?;
    merge_routing(&mut doc, ctx);
    let catalog = apply_catalog(&codex_home(), &doc, ctx)?;
    save(&path, &doc)?;

    let mut steps = vec![
        format!(
            "合并写入 {}（其它 provider 与注释保持不变）",
            path.display()
        ),
        format!("model_provider = \"{PROVIDER_ID}\"，指向 [model_providers.{PROVIDER_ID}]"),
        format!("base_url = {}（wire_api = \"{WIRE_API}\"）", gateway_base(ctx)),
        format!(
            "网关 Key 写在该 provider 的 experimental_bearer_token（{} 专属）",
            ctx.gateway_token
        ),
        format!("model = {GATEWAY_ALIAS}"),
        format!(
            "上游模型: {} ({})",
            ctx.model.model,
            ctx.model.format.display_name()
        ),
    ];
    match &catalog {
        Some(catalog_path) => {
            steps.push(format!(
                "模型目录 {} 里加入 {GATEWAY_ALIAS} 条目（Codex 的模型列表由它决定，不写就看不到）",
                catalog_path.display()
            ));
        }
        None => {
            steps.push(
                "没找到可克隆的模型目录条目，未写模型目录：Codex 会继续用它自带的模型，请求仍按上面的 provider 走网关"
                    .into(),
            );
        }
    }
    steps.push("auth.json 未改动，官方登录缓存与 Codex 官方能力保留".into());
    steps.push("完全退出并重新打开 Codex 后生效".into());

    Ok(ApplyReport {
        kind: AppKind::Codex,
        model_id: ctx.model.id,
        model_name: ctx.model.name.clone(),
        apply_mode: ApplyMode::DirectConfig,
        target: format!("{} → model_providers.{PROVIDER_ID}", path.display()),
        restart_required: true,
        steps,
        note: Some(
            "只合并 aiStart 自己的 provider、模型目录条目与顶层 model_provider / model，不动文件里的其他内容；需要还原时用「移除模型配置」。auth.json 里是官方登录缓存，本应用不会写它。若本机同时用 CC Switch 接管 Codex，两边会写同一份 config.toml，谁后写谁生效——用本应用时应先退出 CC Switch 的接管。注意：Codex 桌面版的模型选择器还会按官方登录态做门控，未登录官方账号时自定义模型可能不出现在 GUI 里（官方标记为 not planned）；命令行 codex 的 /model 与请求路由不受影响。"
                .into(),
        ),
    })
}

pub fn clear() -> AppResult<()> {
    let path = config_path();
    if !path.exists() {
        return Ok(());
    }
    let mut doc = load(&path)?;
    // 目录条目可能还在（provider 被手工删掉过），所以先摘目录，再看 config.toml。
    strip_catalog(&codex_home(), &doc)?;
    if !strip(&mut doc) {
        save(&path, &doc)?;
        return Ok(());
    }
    // 表都清空后如果文件只剩空白，说明是我们建的：删掉，还原成「从未配置」。
    if doc.as_table().is_empty() {
        std::fs::remove_file(&path)?;
    } else {
        save(&path, &doc)?;
    }
    Ok(())
}

pub struct CodexConfigurator;

impl AppConfigurator for CodexConfigurator {
    fn descriptor(&self) -> AppDescriptor {
        catalog::builtin_app(AppKind::Codex)
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

    /// Codex 只认单个模型入口，给网关别名一个就够。
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

    fn parse(text: &str) -> DocumentMut {
        text.parse::<DocumentMut>().expect("test fixture is valid toml")
    }

    fn context() -> ApplyContext {
        ApplyContext {
            model: ModelConfig {
                id: 4,
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
            gateway_token: "codex".into(),
            model_choices: vec![gateway_alias_choice()],
        }
    }

    fn string_at<'a>(doc: &'a DocumentMut, path: &[&str]) -> Option<&'a str> {
        let mut item: &Item = doc.as_item();
        for key in path {
            item = item.get(key)?;
        }
        item.as_str()
    }

    #[test]
    fn writes_the_shape_codex_actually_reads() {
        let mut doc = DocumentMut::new();
        merge_provider(&mut doc, &context()).expect("merge succeeds");
        merge_routing(&mut doc, &context());

        // 缺了顶层 model_provider，Codex 会忽略 base_url 直连官方 —— 必须写。
        assert_eq!(string_at(&doc, &[MODEL_PROVIDER_KEY]), Some(PROVIDER_ID));
        assert_eq!(string_at(&doc, &[MODEL_KEY]), Some(GATEWAY_ALIAS));
        assert_eq!(
            string_at(&doc, &[PROVIDERS_KEY, PROVIDER_ID, "name"]),
            Some(PROVIDER_NAME)
        );
        assert_eq!(
            string_at(&doc, &[PROVIDERS_KEY, PROVIDER_ID, "base_url"]),
            Some("http://127.0.0.1:8931/v1")
        );
        assert_eq!(
            string_at(&doc, &[PROVIDERS_KEY, PROVIDER_ID, "wire_api"]),
            Some(WIRE_API)
        );
        assert_eq!(
            string_at(&doc, &[PROVIDERS_KEY, PROVIDER_ID, "experimental_bearer_token"]),
            Some("codex")
        );
    }

    #[test]
    fn merges_into_an_existing_config_without_touching_other_providers() {
        let mut doc = parse(
            "# 我的 Codex 配置\nmodel = \"gpt-5.5\"\nmodel_provider = \"my-relay\"\n\n[model_providers.my-relay]\nname = \"My Relay\"\nbase_url = \"https://relay.example.com\"\nwire_api = \"responses\"\nenv_key = \"MY_RELAY_KEY\"\n\n[tui]\nnotifications = true\n",
        );

        merge_provider(&mut doc, &context()).expect("merge succeeds");
        merge_routing(&mut doc, &context());
        let rendered = doc.to_string();
        let reparsed = parse(&rendered);

        assert_eq!(
            string_at(&reparsed, &[PROVIDERS_KEY, "my-relay", "base_url"]),
            Some("https://relay.example.com"),
            "其他 provider 必须原样保留"
        );
        assert_eq!(
            string_at(&reparsed, &[PROVIDERS_KEY, "my-relay", "env_key"]),
            Some("MY_RELAY_KEY")
        );
        assert_eq!(
            reparsed
                .as_table()
                .get("tui")
                .and_then(|tui| tui.get("notifications"))
                .and_then(Item::as_bool),
            Some(true),
            "无关的表与值必须原样保留"
        );
        assert_eq!(
            string_at(&reparsed, &[PROVIDERS_KEY, PROVIDER_ID, "wire_api"]),
            Some(WIRE_API)
        );
        // 注释与其它顶层键都要留住（toml_edit 是保留格式的编辑）。
        assert!(rendered.contains("# 我的 Codex 配置"), "{rendered}");
    }

    #[test]
    fn keeps_keys_the_user_added_to_our_own_table() {
        let mut doc = parse(
            "[model_providers.aistart]\nname = \"aiStart\"\nbase_url = \"http://127.0.0.1:8931/v1\"\nwire_api = \"responses\"\nhttp_headers = { \"X-Trace\" = \"1\" }\n",
        );

        merge_provider(&mut doc, &context()).expect("merge succeeds");

        assert_eq!(
            string_at(&doc, &[PROVIDERS_KEY, PROVIDER_ID, "experimental_bearer_token"]),
            Some("codex"),
            "凭据补上"
        );
        assert!(
            doc.as_table()
                .get(PROVIDERS_KEY)
                .and_then(|providers| providers.get(PROVIDER_ID))
                .and_then(|table| table.get("http_headers"))
                .is_some(),
            "我们表里用户手加的键要保留"
        );
    }

    #[test]
    fn rewrites_a_stale_token_instead_of_adding_a_second_provider() {
        let mut doc = parse(
            "[model_providers.aistart]\nname = \"old\"\nbase_url = \"http://127.0.0.1:9999/v1\"\nwire_api = \"responses\"\nexperimental_bearer_token = \"stale\"\n",
        );

        merge_provider(&mut doc, &context()).expect("merge succeeds");

        let providers = doc
            .as_table()
            .get(PROVIDERS_KEY)
            .and_then(Item::as_table_like)
            .expect("providers table");
        assert_eq!(providers.iter().count(), 1);
        assert_eq!(
            string_at(&doc, &[PROVIDERS_KEY, PROVIDER_ID, "experimental_bearer_token"]),
            Some("codex")
        );
        assert_eq!(
            string_at(&doc, &[PROVIDERS_KEY, PROVIDER_ID, "base_url"]),
            Some("http://127.0.0.1:8931/v1")
        );
    }

    #[test]
    fn strip_removes_only_our_provider_and_our_routing() {
        let mut doc = parse(
            "model = \"aiStart\"\nmodel_provider = \"aistart\"\n\n[model_providers.aistart]\nname = \"aiStart\"\nbase_url = \"http://127.0.0.1:8931/v1\"\nwire_api = \"responses\"\nexperimental_bearer_token = \"codex\"\n\n[model_providers.my-relay]\nname = \"My Relay\"\nbase_url = \"https://relay.example.com\"\n",
        );

        assert!(strip(&mut doc));

        let root = doc.as_table();
        assert!(root.get(MODEL_PROVIDER_KEY).is_none(), "顶层路由一并清掉");
        assert!(root.get(MODEL_KEY).is_none());
        assert_eq!(
            string_at(&doc, &[PROVIDERS_KEY, "my-relay", "base_url"]),
            Some("https://relay.example.com")
        );
        assert!(
            doc.as_table()
                .get(PROVIDERS_KEY)
                .and_then(|providers| providers.get(PROVIDER_ID))
                .is_none()
        );
    }

    #[test]
    fn strip_keeps_routing_that_points_somewhere_else() {
        let mut doc = parse(
            "model = \"gpt-5.5\"\nmodel_provider = \"my-relay\"\n\n[model_providers.aistart]\nname = \"aiStart\"\nbase_url = \"http://127.0.0.1:8931/v1\"\n\n[model_providers.my-relay]\nname = \"My Relay\"\nbase_url = \"https://relay.example.com\"\n",
        );

        assert!(strip(&mut doc));

        assert_eq!(string_at(&doc, &[MODEL_PROVIDER_KEY]), Some("my-relay"));
        assert_eq!(string_at(&doc, &[MODEL_KEY]), Some("gpt-5.5"));
    }

    fn template_catalog() -> Value {
        json!({
            "models": [
                {
                    "slug": "glm-5-2-260617",
                    "display_name": "glm",
                    "description": "glm",
                    "context_window": 200_000,
                    "max_context_window": 200_000,
                    "priority": 1000,
                    "model_messages": { "instructions_template": "You are Codex..." }
                }
            ]
        })
    }

    #[test]
    fn clones_an_existing_catalog_entry_and_patches_it() {
        let mut root = template_catalog();
        assert!(merge_catalog(&mut root, &context()));

        let models = root["models"].as_array().expect("models array");
        assert_eq!(models.len(), 2, "原有条目必须留着");
        assert_eq!(models[1]["slug"], "glm-5-2-260617");

        let ours = &models[0];
        assert_eq!(ours["slug"], GATEWAY_ALIAS);
        assert_eq!(ours["display_name"], GATEWAY_ALIAS);
        assert_eq!(ours["description"], "aiStart 本地网关");
        // 条目里那些 Codex 必需的字段靠克隆带过来，不能丢。
        assert_eq!(ours["priority"], json!(1000));
        assert_eq!(
            ours["model_messages"]["instructions_template"],
            "You are Codex..."
        );
    }

    #[test]
    fn supports_1m_bumps_the_catalog_context_window() {
        let mut root = template_catalog();
        merge_catalog(&mut root, &context());
        assert_eq!(
            root["models"][0]["context_window"],
            json!(200_000),
            "未勾选 1M 时保留模板值"
        );

        let mut big = template_catalog();
        let mut with_1m = context();
        with_1m.model.supports_1m = true;
        merge_catalog(&mut big, &with_1m);
        assert_eq!(big["models"][0]["context_window"], json!(1_000_000));
        assert_eq!(big["models"][0]["max_context_window"], json!(1_000_000));
    }

    #[test]
    fn re_merging_replaces_our_entry_instead_of_duplicating_it() {
        let mut root = template_catalog();
        merge_catalog(&mut root, &context());
        merge_catalog(&mut root, &context());

        let models = root["models"].as_array().expect("models array");
        assert_eq!(models.len(), 2);
        assert_eq!(models.iter().filter(|model| is_ours(model)).count(), 1);
    }

    #[test]
    fn without_a_clonable_entry_nothing_is_written() {
        // 空目录：不能凭空造条目，那会让 Codex 整份模型列表失效。
        let mut empty = json!({ "models": [] });
        assert!(!merge_catalog(&mut empty, &context()));
        assert!(empty["models"].as_array().expect("models array").is_empty());

        let mut missing_models = json!({ "other": 1 });
        assert!(!merge_catalog(&mut missing_models, &context()));
    }

    #[test]
    fn drop_entry_only_removes_ours() {
        let mut root = template_catalog();
        merge_catalog(&mut root, &context());

        assert!(drop_entry(&mut root));
        let models = root["models"].as_array().expect("models array");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["slug"], "glm-5-2-260617");
        assert!(!drop_entry(&mut root), "第二次无可删");
    }

    #[test]
    fn apply_catalog_merges_into_the_configured_catalog_file() {
        let dir = std::env::temp_dir().join(format!("aistart-codex-catalog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        let catalog_path = dir.join("cc-switch-model-catalog.json");
        std::fs::write(
            &catalog_path,
            serde_json::to_string(&template_catalog()).unwrap(),
        )
        .expect("seed catalog");

        let doc = parse("model_catalog_json = \"cc-switch-model-catalog.json\"\n");
        let written = apply_catalog(&dir, &doc, &context())
            .expect("catalog apply succeeds")
            .expect("a catalog was written");
        assert_eq!(written, catalog_path);
        // 指针不动：我们并进的就是它指向的那份。
        assert_eq!(
            string_at(&doc, &[CATALOG_KEY]),
            Some("cc-switch-model-catalog.json")
        );

        let reloaded = read_catalog(&catalog_path).expect("catalog readable");
        assert_eq!(
            reloaded["models"].as_array().expect("models array").len(),
            2
        );
        assert!(is_ours(&reloaded["models"][0]));

        // 移除时只摘自己的条目，别人的模型还在。
        strip_catalog(&dir, &doc).expect("strip succeeds");
        let stripped = read_catalog(&catalog_path).expect("catalog readable");
        let models = stripped["models"].as_array().expect("models array");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["slug"], "glm-5-2-260617");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_catalog_stays_out_when_there_is_no_usable_catalog() {
        let dir = std::env::temp_dir().join(format!("aistart-codex-nocat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        // 没配置目录：不写。
        let doc = parse("model = \"aiStart\"\n");
        assert!(apply_catalog(&dir, &doc, &context())
            .expect("no catalog configured is not an error")
            .is_none());
        assert!(
            !dir.join("cc-switch-model-catalog.json").exists(),
            "不该自己造目录文件"
        );

        // 指向的文件不是合法目录：同样不写，也不动那个文件。
        let broken = dir.join("broken.json");
        std::fs::write(&broken, "{ not json").expect("seed broken file");
        let doc = parse("model_catalog_json = \"broken.json\"\n");
        assert!(apply_catalog(&dir, &doc, &context())
            .expect("broken catalog is not an error")
            .is_none());
        assert_eq!(std::fs::read_to_string(&broken).unwrap(), "{ not json");

        strip_catalog(&dir, &doc).expect("strip is a no-op");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolves_catalog_paths_against_codex_home() {
        let home = PathBuf::from(r"C:\Users\someone\.codex");
        assert_eq!(
            resolve_catalog_path(&home, "cc-switch-model-catalog.json"),
            home.join("cc-switch-model-catalog.json")
        );
        assert_eq!(
            resolve_catalog_path(&home, r"D:\elsewhere\catalog.json"),
            PathBuf::from(r"D:\elsewhere\catalog.json")
        );
    }

    #[test]
    fn configured_only_while_the_live_route_points_at_us() {
        let mut applied = DocumentMut::new();
        merge_provider(&mut applied, &context()).expect("merge succeeds");
        merge_routing(&mut applied, &context());
        assert!(configured_in(&applied));

        // provider 表还在，但用户手动把路由切走了 → 不再算「使用中」。
        let mut switched_away = applied.clone();
        switched_away[MODEL_PROVIDER_KEY] = value("my-relay");
        assert!(!configured_in(&switched_away));

        // 路由指向一个不存在的 id → 也不算。
        let mut dangling = DocumentMut::new();
        dangling[MODEL_PROVIDER_KEY] = value(PROVIDER_ID);
        assert!(!configured_in(&dangling));
    }

    #[test]
    fn strip_is_a_no_op_when_nothing_is_ours() {
        let mut doc = parse(
            "[model_providers.my-relay]\nname = \"My Relay\"\nbase_url = \"https://relay.example.com\"\n",
        );
        assert!(!strip(&mut doc));
        assert_eq!(
            string_at(&doc, &[PROVIDERS_KEY, "my-relay", "name"]),
            Some("My Relay")
        );
    }

    #[test]
    fn round_trips_a_real_file_on_disk() {
        let dir = std::env::temp_dir().join(format!("aistart-codex-{}", std::process::id()));
        let path = dir.join("config.toml");
        let _ = std::fs::remove_dir_all(&dir);

        let mut doc = load(&path).expect("missing file counts as an empty document");
        merge_provider(&mut doc, &context()).expect("merge succeeds");
        merge_routing(&mut doc, &context());
        save(&path, &doc).expect("save creates the directory");

        let written = std::fs::read_to_string(&path).expect("file written");
        assert!(
            written.contains("[model_providers.aistart]"),
            "应写成表头而不是内联表：\n{written}"
        );

        let mut reloaded = load(&path).expect("our own output must be readable");
        assert!(strip(&mut reloaded));
        assert!(
            reloaded.as_table().is_empty(),
            "provider 与路由都清掉后文件应为空，clear 会据此删文件"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_shapes_it_cannot_merge_into() {
        let mut wrong_type = parse("model_providers = 42\n");
        assert!(merge_provider(&mut wrong_type, &context()).is_err());

        let mut wrong_provider = parse("[model_providers]\naistart = \"nope\"\n");
        assert!(merge_provider(&mut wrong_provider, &context()).is_err());
    }

    #[test]
    fn codex_home_prefers_the_env_var() {
        assert_eq!(
            resolve_home(Some(r"D:\codex-home".into())),
            PathBuf::from(r"D:\codex-home")
        );
        // 空串等同于没设置，回落到 ~/.codex。
        assert_eq!(
            resolve_home(Some("   ".into())),
            resolve_home(None)
        );
        assert!(resolve_home(None).ends_with(".codex"));
    }
}

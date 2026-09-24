use serde::{Deserialize, Serialize};

/// 系统提示词的注入方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PromptMode {
    /// 追加到现有 system 之后
    Append,
    /// 插入到现有 system 之前
    Prepend,
}

/// 一条过滤器的「规则动作」——策略模式，规则实现统一在同一个枚举里。
///
/// 序列化为内部标签 JSON（`{"kind":"system-prompt", ...}`），直接作为
/// `request_filters.rule_config` 列的内容；`kind` 取值与 `rule_kind` 列一致。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum FilterRule {
    /// 注入系统提示词。
    SystemPrompt { mode: PromptMode, text: String },
}

impl FilterRule {
    /// 与 `rule_kind` 列、前端 `kind` 一致的判别字符串。
    pub fn kind(&self) -> &'static str {
        match self {
            FilterRule::SystemPrompt { .. } => "system-prompt",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestFilter {
    pub id: i64,
    pub name: String,
    pub enabled: bool,
    /// 执行顺序（由小到大）。
    pub order: i64,
    pub rule: FilterRule,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

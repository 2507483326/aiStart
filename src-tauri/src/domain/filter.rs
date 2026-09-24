use serde::{Deserialize, Serialize};

/// 系统提示词的注入方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PromptMode {
    /// 追加到现有 system 之后
    Append,
    /// 插入到现有 system 之前
    Prepend,
    /// 整体替换现有 system
    Replace,
}

/// 文本查找替换的作用范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReplaceTarget {
    System,
    Messages,
    All,
}

/// 一条过滤器的「规则动作」——策略模式，三种实现统一在同一个枚举里。
///
/// 序列化为内部标签 JSON（`{"kind":"system-prompt", ...}`），直接作为
/// `request_filters.rule_config` 列的内容；`kind` 取值与 `rule_kind` 列一致。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum FilterRule {
    /// 注入系统提示词。
    SystemPrompt { mode: PromptMode, text: String },
    /// 覆盖请求参数（只覆盖填了的字段，未填的保持请求原值）。
    RequestParams {
        #[serde(default)]
        temperature: Option<f64>,
        #[serde(default, rename = "maxTokens")]
        max_tokens: Option<u32>,
        #[serde(default, rename = "topP")]
        top_p: Option<f64>,
        #[serde(default, rename = "stopSequences")]
        stop_sequences: Option<Vec<String>>,
    },
    /// 文本字面量查找替换（非正则）。
    TextReplace {
        find: String,
        #[serde(default)]
        replace: String,
        target: ReplaceTarget,
    },
}

impl FilterRule {
    /// 与 `rule_kind` 列、前端 `kind` 一致的判别字符串。
    pub fn kind(&self) -> &'static str {
        match self {
            FilterRule::SystemPrompt { .. } => "system-prompt",
            FilterRule::RequestParams { .. } => "request-params",
            FilterRule::TextReplace { .. } => "text-replace",
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

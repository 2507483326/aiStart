//! 「手动应用」客户端：没有可程序化写入的配置文件，或官方未公开其格式。
//!
//! 与 `dsh` / `windows` 里的实现不同，这里**不写任何第三方文件**：`apply` 只产出一份
//! 「对接说明」（接口地址 / API Key / 模型名称 / 该应用适用的协议），由用户照着在应用
//! GUI 里手动添加供应商。绑定关系仍记在 `settings` 里，网关据此把入站请求归到来源应用。
//!
//! 一旦某个应用实测出了真实的落盘配置（见 `catalog.rs` 里各描述符的注释），
//! 就为它单独实现一个 `AppConfigurator`，把 `apply_mode` 切成 `DirectConfig`。

use crate::domain::app::{AppDescriptor, AppKind, ApplyMode, ApplyReport};
use crate::domain::catalog;
use crate::error::AppResult;

use super::{gateway_alias_choice, AppConfigurator, ApplyContext, DetectResult, ModelChoice};

pub struct ManualConfigurator {
    kind: AppKind,
}

impl ManualConfigurator {
    pub fn new(kind: AppKind) -> Self {
        Self { kind }
    }
}

impl AppConfigurator for ManualConfigurator {
    fn descriptor(&self) -> AppDescriptor {
        catalog::builtin_app(self.kind)
    }

    fn detect(&self) -> AppResult<DetectResult> {
        #[cfg(windows)]
        return Ok(super::windows::detect_by_descriptor(&self.descriptor()));
        #[cfg(not(windows))]
        return Ok(DetectResult::missing());
    }

    /// 手动应用没有可验证的落盘配置，因此以「是否记录过绑定」为准。
    /// 这让卡片在应用后显示「使用中」，与其它客户端一致。
    fn is_configured(&self) -> AppResult<bool> {
        Ok(crate::settings::snapshot()
            .applied
            .contains_key(self.kind.as_str()))
    }

    /// ZCode 只认单个模型入口，给网关别名一个就够。
    fn exposed_models(&self) -> Vec<ModelChoice> {
        vec![gateway_alias_choice()]
    }

    fn apply(&self, ctx: &ApplyContext) -> AppResult<ApplyReport> {
        let descriptor = self.descriptor();
        let base = format!("{}/v1", ctx.gateway_base_url.trim_end_matches('/'));
        let model = ctx.gateway_model_id().to_string();

        // 各应用支持的协议不同，说明里要写清楚，否则用户会在 GUI 里选错格式。
        let protocol = match self.kind {
            AppKind::ZCode => "Chat Completions / Responses / Anthropic Messages（任选其一）",
            _ => "OpenAI 兼容",
        };

        Ok(ApplyReport {
            kind: self.kind,
            model_id: ctx.model.id,
            model_name: ctx.model.name.clone(),
            apply_mode: ApplyMode::Manual,
            target: format!(
                "{} → 手动填写（{}）",
                descriptor.name, descriptor.config_target
            ),
            restart_required: true,
            steps: vec![
                format!("在{}中添加一个自定义模型供应商", descriptor.config_target),
                format!("接口地址（Base URL）: {base}"),
                format!(
                    "API Key: {}（{} 专属）",
                    ctx.gateway_token, descriptor.name
                ),
                format!("模型名称: {model}"),
                format!("协议格式: {protocol}"),
                format!(
                    "上游模型: {} ({})",
                    ctx.model.model,
                    ctx.model.format.display_name()
                ),
                format!(
                    "保存后即可在 {} 内向本地网关发起请求，用量会归到「{}」来源",
                    descriptor.name,
                    self.kind.as_str()
                ),
            ],
            note: Some(
                "该客户端需在 GUI 中手动添加供应商，本应用不会改动它的配置文件。按上面的地址、Key、模型名填写即可。"
                    .into(),
            ),
        })
    }

    /// 没有写过任何第三方文件需要还原；绑定由 `clear_app_model` 在 settings 里清除。
    fn clear(&self) -> AppResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::{ModelConfig, ModelFormat};
    use crate::platform::GATEWAY_ALIAS;

    fn context() -> ApplyContext {
        ApplyContext {
            model: ModelConfig {
                id: 3,
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
            gateway_token: "zcode".into(),
            model_choices: vec![gateway_alias_choice()],
        }
    }

    #[test]
    fn apply_is_manual_and_spells_out_the_connection_details() {
        let report = ManualConfigurator::new(AppKind::ZCode)
            .apply(&context())
            .expect("manual apply never fails");

        assert_eq!(report.apply_mode, ApplyMode::Manual);
        assert_eq!(report.kind, AppKind::ZCode);

        let text = report.steps.join("\n");
        assert!(text.contains("http://127.0.0.1:8931/v1"), "{text}");
        assert!(text.contains("zcode"), "{text}");
        assert!(text.contains("Responses"), "{text}");
        // 模型名走网关别名，与其它客户端一致。
        assert!(text.contains(GATEWAY_ALIAS), "{text}");
    }

    #[test]
    fn each_manual_app_exposes_exactly_one_gateway_entry() {
        for kind in [AppKind::ZCode] {
            let models = ManualConfigurator::new(kind).exposed_models();
            assert_eq!(models.len(), 1);
            assert_eq!(models[0].id, GATEWAY_ALIAS);
        }
    }
}

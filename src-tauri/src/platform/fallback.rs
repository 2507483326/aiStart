use crate::domain::app::{AppDescriptor, AppKind, ApplyReport};
use crate::domain::catalog;
use crate::error::{AppError, AppResult};

use super::{AppConfigurator, ApplyContext, DetectResult};

pub struct UnsupportedConfigurator {
    kind: AppKind,
}

impl UnsupportedConfigurator {
    pub fn new(kind: AppKind) -> Self {
        Self { kind }
    }

    fn unsupported(&self) -> AppError {
        AppError::Unsupported(format!(
            "{} 的自动配置目前仅支持 Windows，当前平台请手动配置",
            catalog::builtin_app(self.kind).name
        ))
    }
}

impl AppConfigurator for UnsupportedConfigurator {
    fn descriptor(&self) -> AppDescriptor {
        catalog::builtin_app(self.kind)
    }

    fn detect(&self) -> AppResult<DetectResult> {
        Ok(DetectResult::missing())
    }

    fn is_configured(&self) -> AppResult<bool> {
        Ok(false)
    }

    fn apply(&self, _ctx: &ApplyContext) -> AppResult<ApplyReport> {
        Err(self.unsupported())
    }

    fn clear(&self) -> AppResult<()> {
        Err(self.unsupported())
    }
}

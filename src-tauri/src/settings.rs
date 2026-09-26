use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use rusqlite::{params, Connection};

use crate::db;
use crate::domain::app::AppKind;
use crate::domain::model::{ModelConfig, ModelFormat, ModelInput};
use crate::error::{AppError, AppResult};

/// 解析结果：点名命中的模型锁定单打，失败不参与切换；其余落到当前模型。
#[derive(Debug, Clone)]
pub enum ResolvedTarget {
    /// 请求模型名命中显示名：只调用该模型，失败不触发切换。
    Named(ModelConfig),
    /// 别名（aiStart/auto）/ 未指定 / 未命中：用当前模型，失败可触发事后切换。
    Active(ModelConfig),
}

impl ResolvedTarget {
    pub fn is_named(&self) -> bool {
        matches!(self, Self::Named(_))
    }

    pub fn into_config(self) -> ModelConfig {
        match self {
            Self::Named(config) | Self::Active(config) => config,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub models: Vec<ModelConfig>,
    pub active_model_id: Option<i64>,
    pub gateway_port: u16,
    pub auto_failover: bool,
    /// 开机自启（写入 HKCU Run 注册表项，见 platform::autostart）。
    pub launch_at_login: bool,
    /// 出站代理是否启用。关掉 = 直连，地址保留（重新打开即恢复，不用重填）。
    pub proxy_enabled: bool,
    /// 出站代理地址（如 `http://127.0.0.1:7890`）。
    /// 生效点是 providers::http_client()，所有出站流量共用。
    pub proxy_url: String,
    /// 请求报文保留天数（usage_payload 清理策略）：7 / 30 / 100，0 = 永久保留。
    /// 只删报文快照，usage_detail 明细与每日汇总不受影响。
    pub request_retention_days: i64,
    /// app_kind -> model_id
    pub applied: BTreeMap<String, i64>,
    /// app_kind -> 应用专属网关 Key（固定可读，= app_kind）
    pub app_tokens: BTreeMap<String, String>,
}

fn default_port() -> u16 {
    8931
}

/// 「请求保存时间」的合法天数：7 / 30 / 100 天，0 = 永久保留。后端校验与前端选项共用这套值。
pub const RETENTION_DAY_OPTIONS: [i64; 4] = [7, 30, 100, 0];

impl Default for Settings {
    fn default() -> Self {
        Self {
            models: Vec::new(),
            active_model_id: None,
            gateway_port: default_port(),
            auto_failover: false,
            // 默认开机启动：首次运行就写注册表，用户不想要再关。
            launch_at_login: true,
            // 代理默认关：地址都没有，开了也没用。
            proxy_enabled: false,
            proxy_url: String::new(),
            // 请求报文默认保留 7 天：磁盘增长最狠的就是报文快照，默认给个短窗口。
            request_retention_days: 7,
            applied: BTreeMap::new(),
            app_tokens: BTreeMap::new(),
        }
    }
}

impl Settings {
    pub fn active_model(&self) -> Option<&ModelConfig> {
        let id = self.active_model_id?;
        self.models.iter().find(|model| model.id == id)
    }

    pub fn applied_model(&self, kind: AppKind) -> Option<&ModelConfig> {
        let id = *self.applied.get(kind.as_str())?;
        self.models.iter().find(|model| model.id == id)
    }

    pub fn app_token(&self, kind: AppKind) -> Option<&str> {
        self.app_tokens.get(kind.as_str()).map(String::as_str)
    }

    /// 按请求携带的 token 反查来源应用；空 token 视为未匹配。
    pub fn app_for_token(&self, token: &str) -> Option<AppKind> {
        if token.is_empty() {
            return None;
        }
        self.app_tokens
            .iter()
            .find(|(_, value)| value.as_str() == token)
            .and_then(|(kind, _)| AppKind::parse(kind))
    }

    /// 解析本次请求的上游目标（一次请求只打一个上游，没有候选循环）：
    /// - 请求模型名命中某个模型的显示名（不区分大小写）→ 该模型（锁定，失败不参与切换）；
    /// - 别名 aiStart/auto（不区分大小写）、未指定、未命中 → 当前模型；
    /// - 没有可用模型 → None（调用方报错）。
    pub fn resolve_target(&self, requested: Option<&str>) -> Option<ResolvedTarget> {
        if let Some(needle) = requested
            .map(str::trim)
            .filter(|name| !name.is_empty() && !crate::gateway::is_auto_alias(name))
        {
            let needle = needle.to_lowercase();
            if let Some(model) = self
                .models
                .iter()
                .find(|model| model.name.to_lowercase() == needle)
            {
                return Some(ResolvedTarget::Named(model.clone()));
            }
            // 未命中 → 当前模型，与旧候选逻辑一致。
        }
        self.active_model().cloned().map(ResolvedTarget::Active)
    }

    fn next_model_id(&self) -> i64 {
        self.models.iter().map(|model| model.id).max().unwrap_or(0) + 1
    }

    pub fn upsert(&mut self, input: ModelInput) -> ModelConfig {
        let now = chrono::Local::now().to_rfc3339();
        let existing = input
            .id
            .and_then(|id| self.models.iter().position(|model| model.id == id));
        let id = existing
            .map(|index| self.models[index].id)
            .unwrap_or_else(|| self.next_model_id());
        let created_at = existing
            .map(|index| self.models[index].created_at.clone())
            .unwrap_or_else(|| now.clone());

        let config = ModelConfig {
            id,
            name: input.name,
            format: input.format,
            base_url: input.base_url,
            api_key: input.api_key,
            model: input.model,
            supports_1m: input.supports_1m,
            created_at,
            updated_at: now,
        };

        match existing {
            Some(index) => self.models[index] = config.clone(),
            None => self.models.push(config.clone()),
        }

        if self.active_model_id.is_none() {
            self.active_model_id = Some(id);
        }

        config
    }

    pub fn remove(&mut self, id: i64) -> bool {
        let before = self.models.len();
        self.models.retain(|model| model.id != id);
        if self.active_model_id == Some(id) {
            self.active_model_id = self.models.first().map(|model| model.id);
        }
        self.applied.retain(|_, value| *value != id);
        self.models.len() != before
    }
}

static STORE: OnceLock<RwLock<Settings>> = OnceLock::new();

fn store() -> &'static RwLock<Settings> {
    STORE.get_or_init(|| RwLock::new(Settings::default()))
}

pub fn init(dir: &Path) -> AppResult<()> {
    db::init(dir)?;

    let settings = load()?;

    *store().write().expect("settings lock poisoned") = settings;
    persist()
}

fn load() -> AppResult<Settings> {
    db::with_conn(|connection| {
        let mut settings = Settings::default();

        let mut statement = connection.prepare("SELECT key, value FROM app_settings")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (key, value) = row?;
            match key.as_str() {
                "active_model_id" => settings.active_model_id = value.trim().parse().ok(),
                "gateway_port" => {
                    if let Ok(port) = value.trim().parse() {
                        settings.gateway_port = port;
                    }
                }
                "auto_failover" => settings.auto_failover = value.trim() == "1",
                "launch_at_login" => settings.launch_at_login = value.trim() == "1",
                "proxy_enabled" => settings.proxy_enabled = value.trim() == "1",
                "proxy_url" => settings.proxy_url = value.trim().to_string(),
                "request_retention_days" => {
                    if let Ok(days) = value.trim().parse() {
                        settings.request_retention_days = days;
                    }
                }
                _ => {}
            }
        }

        settings.models = load_models(connection)?;
        settings.applied = load_bindings(connection)?;
        settings.app_tokens = load_app_tokens(connection)?;
        Ok(settings)
    })
}

fn load_models(connection: &Connection) -> AppResult<Vec<ModelConfig>> {
    let mut statement = connection.prepare(
        "SELECT model_id, name, format, base_url, api_key, model, supports_1m, created_time, update_time \
         FROM models ORDER BY model_id",
    )?;
    let rows = statement.query_map([], |row| {
        let format: String = row.get(2)?;
        Ok(ModelConfig {
            id: row.get(0)?,
            name: row.get(1)?,
            format: ModelFormat::parse(&format),
            base_url: row.get(3)?,
            api_key: row.get(4)?,
            model: row.get(5)?,
            supports_1m: row.get::<_, i64>(6)? != 0,
            created_at: db::iso_from_ms(row.get(7)?),
            updated_at: db::iso_from_ms(row.get(8)?),
        })
    })?;

    let mut models = Vec::new();
    for row in rows {
        models.push(row?);
    }
    Ok(models)
}

fn load_bindings(connection: &Connection) -> AppResult<BTreeMap<String, i64>> {
    let mut statement = connection.prepare("SELECT app_kind, model_id FROM app_model_bindings")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;

    let mut bindings = BTreeMap::new();
    for row in rows {
        let (app_kind, model_id) = row?;
        bindings.insert(app_kind, model_id);
    }
    Ok(bindings)
}

fn load_app_tokens(connection: &Connection) -> AppResult<BTreeMap<String, String>> {
    let mut statement = connection.prepare("SELECT app_kind, token FROM app_model_bindings")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut tokens = BTreeMap::new();
    for row in rows {
        let (app_kind, token) = row?;
        // 兼容补列后的旧行（token 为空）：回填为派生的专属 Key。
        let token = if token.trim().is_empty() {
            AppKind::parse(&app_kind)
                .map(|kind| kind.gateway_token().to_string())
                .unwrap_or(token)
        } else {
            token
        };
        tokens.insert(app_kind, token);
    }
    Ok(tokens)
}

fn setting_pairs(settings: &Settings) -> Vec<(&'static str, String)> {
    vec![
        (
            "active_model_id",
            settings
                .active_model_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
        ),
        ("gateway_port", settings.gateway_port.to_string()),
        (
            "auto_failover",
            if settings.auto_failover { "1" } else { "0" }.to_string(),
        ),
        (
            "launch_at_login",
            if settings.launch_at_login { "1" } else { "0" }.to_string(),
        ),
        (
            "proxy_enabled",
            if settings.proxy_enabled { "1" } else { "0" }.to_string(),
        ),
        ("proxy_url", settings.proxy_url.clone()),
        (
            "request_retention_days",
            settings.request_retention_days.to_string(),
        ),
    ]
}

pub fn persist() -> AppResult<()> {
    let snapshot = store().read().expect("settings lock poisoned").clone();
    db::with_tx(|transaction| {
        let now = db::now_ms();

        for (key, value) in setting_pairs(&snapshot) {
            transaction.execute(
                "INSERT INTO app_settings (key, value, created_time, update_time) VALUES (?1, ?2, ?3, ?3) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, update_time = excluded.update_time",
                params![key, value, now],
            )?;
        }

        transaction.execute("DELETE FROM models", [])?;
        for model in &snapshot.models {
            transaction.execute(
                "INSERT INTO models (model_id, name, format, base_url, api_key, model, supports_1m, created_time, update_time) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    model.id,
                    model.name,
                    model.format.as_str(),
                    model.base_url,
                    model.api_key,
                    model.model,
                    i64::from(model.supports_1m),
                    db::ms_from_iso(&model.created_at).unwrap_or(now),
                    db::ms_from_iso(&model.updated_at).unwrap_or(now),
                ],
            )?;
        }

        transaction.execute("DELETE FROM app_model_bindings", [])?;
        for (app_kind, model_id) in &snapshot.applied {
            let token = snapshot
                .app_tokens
                .get(app_kind)
                .cloned()
                .or_else(|| AppKind::parse(app_kind).map(|kind| kind.gateway_token().to_string()))
                .unwrap_or_default();
            transaction.execute(
                "INSERT INTO app_model_bindings (app_kind, model_id, token, created_time, update_time) VALUES (?1, ?2, ?3, ?4, ?4)",
                params![app_kind, model_id, token, now],
            )?;
        }

        Ok(())
    })
}

pub fn snapshot() -> Settings {
    store().read().expect("settings lock poisoned").clone()
}

/// 只取「代理开关 + 地址」。出站客户端每次请求前都要问一遍，
/// 不值得为此克隆整个 Settings（还带模型列表）。
pub fn proxy_settings() -> (bool, String) {
    let guard = store().read().expect("settings lock poisoned");
    (guard.proxy_enabled, guard.proxy_url.clone())
}

pub fn mutate<T>(f: impl FnOnce(&mut Settings) -> T) -> AppResult<T> {
    let value = {
        let mut guard = store().write().expect("settings lock poisoned");
        f(&mut guard)
    };
    persist()?;
    Ok(value)
}

/// DeepSeek Desktop 的配置文件路径固定为 `~/.dsh/settings.yaml`。
pub fn deepseek_config_path() -> String {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".dsh")
        .join("settings.yaml")
        .to_string_lossy()
        .to_string()
}

pub fn require_model(id: i64) -> AppResult<ModelConfig> {
    snapshot()
        .models
        .iter()
        .find(|model| model.id == id)
        .cloned()
        .ok_or_else(|| AppError::NotFound(format!("模型 {id} 不存在")))
}

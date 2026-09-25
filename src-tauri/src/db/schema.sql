-- =====================================================================
-- AI Start SQLite schema v8（db_schema_version = 8）
-- v1 首次落库：app_settings / models / app_model_bindings（配置与模型，取代 settings.json）、
-- usage_detail / usage_daily_total（token 消耗，取代 usage.jsonl）、events（审计事件）、
-- app_version_records（应用版本检查与更新记录）。
-- v2 新增：usage_payload（单次调用的请求/响应报文，供「请求明细抽屉」查看）。
-- v3 新增：request_filters（请求转发前按规则改写请求的过滤器）。
-- v4 新增：usage_detail.source_app（请求来源应用 / 原样 token）、app_model_bindings.token（应用专属网关 Key）。
-- v5 新增：usage_payload.upstream_request / upstream_request_truncated（提示词注入后实际发往上游的请求体）。
-- v6 新增：usage_detail.upstream_url / upstream_model（实际发往上游的接口地址与模型 ID，供「请求详情」展示）。
-- v7 变更：app_version_records 由「每次检查/动作追加一行」改为「每个应用一行」（app_kind 作主键，
--          刷新只更新这一行）。
-- v8 新增：usage_detail.proxied（这次请求是否经代理出站）。
--
-- 规范（对齐 eTeam：C:\eTeam\src\host\state\schema.sql）：
--   主键 = 每张表自己的编号列，统一 INTEGER 自增（仅 schema_meta / app_settings 以 key 为主键，
--          usage_daily_total 以 day 为主键，app_version_records 以 app_kind 为主键）
--   时间列一律以 *_time 结尾（Unix 毫秒）；每张表末尾固定 created_time / update_time
--   枚举 = TEXT（合法取值写在列注释里）；JSON = TEXT 存 JSON 字符串
--   表上不建外键、CHECK、UNIQUE、触发器——规则全部由写入代码保证
--   时间跨度前缀（day 等）单独成列，便于按天聚合与建索引
--
-- 注意：本文件由 src/db/mod.rs 通过 include_str!("schema.sql") 直接内嵌执行，
-- 是 schema 的单一来源（不存在与代码里的副本漂移问题）。
-- =====================================================================

-- ---------------------------------------------------------------------
-- 0. schema_meta —— 元数据（库版本号等键值对；key 即主键，唯一例外）
-- ---------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS schema_meta (
  key            TEXT PRIMARY KEY,             -- 元数据键，如 'db_schema_version' / 'db_created_at'
  value          TEXT NOT NULL,                -- 统一 TEXT 存放，数值由读取方解析
  created_time   INTEGER NOT NULL,             -- 创建时间
  update_time    INTEGER NOT NULL              -- 更新时间
);

-- ---------------------------------------------------------------------
-- 1. usage_detail —— Token 消耗明细（一行 = 一次网关请求/模型回复）
--    DB 即唯一存储（取代原 usage.jsonl）；写入 = 本表 INSERT + usage_daily_total 增量 upsert（单事务）
-- ---------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS usage_detail (
  usage_detail_id   INTEGER PRIMARY KEY AUTOINCREMENT,  -- 明细行号，自增
  day               TEXT NOT NULL,          -- 消耗日 'yyyy-MM-dd'（记录时按宿主本地时区折算）
  event_time        INTEGER NOT NULL,       -- 事件发生时刻（Unix 毫秒）
  model_name        TEXT NOT NULL DEFAULT '',  -- 命中/兜底的模型名（active model；错误时为当时生效名）
  served_by         TEXT NOT NULL DEFAULT '',  -- 实际服务该请求的上游模型名（自动切换后为接手方；错误请求为空）
  inbound_protocol  TEXT NOT NULL DEFAULT '',  -- 入站协议：anthropic-messages / openai-completions / openai-responses
  upstream_protocol TEXT NOT NULL DEFAULT '',  -- 上游协议（错误请求为空）
  input_tokens      INTEGER NOT NULL DEFAULT 0,   -- 不含缓存的输入
  output_tokens     INTEGER NOT NULL DEFAULT 0,   -- 输出
  cache_read_tokens  INTEGER,               -- 缓存读；未上报为 NULL（Anthropic cache_read_input_tokens / OpenAI cached_tokens）
  cache_write_tokens INTEGER,               -- 缓存写；未上报为 NULL（Anthropic cache_creation_input_tokens；OpenAI 无写入）
  reasoning_tokens   INTEGER,               -- 思考 token；不计入 total_tokens；未解析写 NULL（预留）
  total_tokens      INTEGER NOT NULL DEFAULT 0,   -- 真实消耗（对齐 cc-switch 口径）= input + output + cache_read + cache_write
  duration_ms       INTEGER NOT NULL DEFAULT 0,   -- 请求耗时（毫秒）
  ok                INTEGER NOT NULL DEFAULT 1,   -- 1=成功 / 0=失败
  failover          INTEGER NOT NULL DEFAULT 0,   -- 1=由自动切换接手 / 0=否
  error             TEXT,                   -- 失败原因；成功为 NULL
  source_app        TEXT NOT NULL DEFAULT '',  -- 来源应用：按请求 token 匹配到的 app_kind；未匹配则原样存该 token；''=历史数据/未记录
  upstream_url      TEXT NOT NULL DEFAULT '',  -- 实际发往上游的接口地址（完整 URL，含路径）；未发起上游请求为空
  upstream_model    TEXT NOT NULL DEFAULT '',  -- 实际发往上游的模型 ID（wire model，与显示名 served_by 不同）；未发起上游请求为空
  proxied           INTEGER NOT NULL DEFAULT 0,  -- 1=这次请求经代理出站 / 0=直连（含未发起上游请求的失败）
  created_time      INTEGER NOT NULL,       -- 入库时刻
  update_time       INTEGER NOT NULL        -- 明细行只插不改，= created_time
);

CREATE INDEX IF NOT EXISTS idx_usage_detail_day    ON usage_detail (day);
CREATE INDEX IF NOT EXISTS idx_usage_detail_time   ON usage_detail (event_time DESC);
CREATE INDEX IF NOT EXISTS idx_usage_detail_served ON usage_detail (served_by, day);
CREATE INDEX IF NOT EXISTS idx_usage_detail_source ON usage_detail (source_app);
CREATE INDEX IF NOT EXISTS idx_usage_detail_failed ON usage_detail (day, ok) WHERE ok = 0;

-- ---------------------------------------------------------------------
-- 2. usage_daily_total —— 每日消耗总和（一行 = 一天，全应用口径）
--    写入按事件增量 upsert（非强一致：精确口径可随时按 day 重查 usage_detail）
-- ---------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS usage_daily_total (
  day                TEXT PRIMARY KEY,      -- 消耗日 'yyyy-MM-dd'（行即主键）
  input_tokens       INTEGER NOT NULL DEFAULT 0,
  output_tokens      INTEGER NOT NULL DEFAULT 0,
  total_tokens       INTEGER NOT NULL DEFAULT 0,  -- 明细 total_tokens 之和（增量累计）
  calls              INTEGER NOT NULL DEFAULT 0,  -- 明细行数 = 调用次数
  failed_calls       INTEGER NOT NULL DEFAULT 0,  -- 其中失败次数（ok = 0）
  created_time       INTEGER NOT NULL,      -- 首次写入该日行
  update_time        INTEGER NOT NULL       -- 最近一次增量
);

-- ---------------------------------------------------------------------
-- 3. events —— 审计事件（只插入，不修改）
--    网关启停、模型增删启用、应用安装/更新/接入、版本检查等统一落这里
-- ---------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS events (
  event_id     INTEGER PRIMARY KEY AUTOINCREMENT,  -- 事件号，自增（全库递增，也按它排序）
  event_time   INTEGER NOT NULL,     -- 发生时刻（Unix 毫秒）
  actor_kind   TEXT NOT NULL,        -- 谁触发的：user / system / gateway
  actor_name   TEXT,                 -- 触发者名（用户 / '系统' / '网关'）；system 可空
  type         TEXT NOT NULL,        -- 事件类型，如 'gateway.started' / 'model.activated' / 'app.updated'（开放集合，不限定）
  target_kind  TEXT,                 -- 作用对象类别：app / model / gateway / settings；无对象为空
  target_id    TEXT,                 -- 作用对象标识（app_kind / model_id）；系统级事件为空
  payload      TEXT,                 -- 事件附数据（JSON）；无则 NULL
  created_time INTEGER NOT NULL,     -- 创建时间（= event_time）
  update_time  INTEGER NOT NULL      -- 更新时间（追加即写）
);

CREATE INDEX IF NOT EXISTS idx_events_time   ON events (event_time DESC);
CREATE INDEX IF NOT EXISTS idx_events_type   ON events (type, event_id);
CREATE INDEX IF NOT EXISTS idx_events_target ON events (target_kind, target_id, event_id) WHERE target_id IS NOT NULL;

-- ---------------------------------------------------------------------
-- 4. app_version_records —— 应用版本记录（一行 = 一个被管理应用的最新版本状态）
--    action=check 落检查结果，action=install/update 落安装更新动作；
--    两者都 upsert 该应用唯一的那一行（不追加历史），刷新只更新这一行；
--    同时作为「最近一次检查结果」的持久化来源，重启后徽标无需等待重新联网
-- ---------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS app_version_records (
  app_kind          TEXT PRIMARY KEY,   -- 应用：claude-desktop / deepseek-desktop（行即主键）
  action            TEXT NOT NULL,   -- 最近一次动作：check=版本检查 / install=首次安装 / update=更新
  installed_version TEXT,            -- 动作前的已安装版本；未安装为 NULL
  target_version    TEXT,            -- 检查到/要更新到的目标版本；检查失败为 NULL
  latest_version    TEXT,            -- 检查到的官方最新版本（check 专用快照，便于回看）
  update_available  INTEGER NOT NULL DEFAULT 0,  -- 1=有新版本 / 0=已是最新或无版本可比
  source_url        TEXT,            -- 目标版本来源地址（RELEASES / GitHub API）；检查失败为 NULL
  status            TEXT NOT NULL,   -- check: found=发现新版本 / up-to-date=已最新 / unreachable=上游不可达
                                     -- install|update: launched=已启动 / succeeded=成功 / failed=失败
  message           TEXT,            -- 说明 / 错误信息
  event_time        INTEGER NOT NULL,  -- 最近一次动作时刻（Unix 毫秒）
  created_time      INTEGER NOT NULL,  -- 首次建行时间
  update_time       INTEGER NOT NULL   -- 最近一次更新时间
);

-- ---------------------------------------------------------------------
-- 5. app_settings —— 应用设置（键值对；DB 即唯一存储，取代 settings.json 的标量字段）
--    合法键：active_model_id（生效模型 ID）/ gateway_port（本地网关端口）
--            / auto_failover（1/0）/ launch_at_login（开机启动，1/0）
--            / proxy_enabled（代理开关，1/0）/ proxy_url（出站代理地址，空串 = 直连）
-- ---------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS app_settings (
  key            TEXT PRIMARY KEY,             -- 设置键（行即主键，key 例外同 schema_meta）
  value          TEXT NOT NULL,                -- 统一 TEXT 存放，数值/布尔由读取方解析
  created_time   INTEGER NOT NULL,             -- 创建时间
  update_time    INTEGER NOT NULL              -- 更新时间
);

-- ---------------------------------------------------------------------
-- 6. models —— 上游模型（DB 即唯一存储，取代 settings.json 的 models 数组）
-- ---------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS models (
  model_id       INTEGER PRIMARY KEY AUTOINCREMENT,  -- 模型号，自增（全库唯一，对外即模型标识）
  name           TEXT NOT NULL,                -- 显示名
  format         TEXT NOT NULL,                -- 协议：openai-completions / anthropic-messages / openai-responses
  base_url       TEXT NOT NULL,                -- 上游基址
  api_key        TEXT NOT NULL DEFAULT '',     -- API Key（明文存放，与现有 settings.json 行为一致）
  model          TEXT NOT NULL,                -- 上游模型名
  supports_1m    INTEGER NOT NULL DEFAULT 0,   -- 是否支持 1M 上下文：1=支持 / 0=否
  created_time   INTEGER NOT NULL,             -- 创建时间
  update_time    INTEGER NOT NULL              -- 更新时间
);

CREATE INDEX IF NOT EXISTS idx_models_update ON models (update_time DESC);

-- ---------------------------------------------------------------------
-- 7. app_model_bindings —— 应用接入关系（一行 = 一个应用当前接入的模型；取代 settings.json 的 applied 映射）
--    一应用一行，唯一性 (app_kind) 由写入代码保证（本设计不用 UNIQUE 约束，同 eTeam 口径）
-- ---------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS app_model_bindings (
  app_model_binding_id INTEGER PRIMARY KEY AUTOINCREMENT,  -- 自增主键
  app_kind       TEXT NOT NULL,                -- 应用：claude-desktop / deepseek-desktop（一行一应用）
  model_id       INTEGER NOT NULL,             -- 接入的模型号（models.model_id，松引用，不建外键）
  token          TEXT NOT NULL DEFAULT '',     -- 该应用接入时使用的网关 Key（固定可读、不加前缀，= app_kind；网关据此匹配请求来源）
  created_time   INTEGER NOT NULL,             -- 创建时间
  update_time    INTEGER NOT NULL              -- 更新时间
);

CREATE INDEX IF NOT EXISTS idx_app_model_bindings_kind ON app_model_bindings (app_kind);

-- ---------------------------------------------------------------------
-- 8. usage_payload —— 请求/响应报文（一行 = 一次网关调用的报文快照）
--    与 usage_detail 一比一（usage_detail_id 松引用，不建外键）；
--    存「原生报文」：入站请求为客户端原始 body，上游请求为注入后实际发出的 body，
--    响应为上游原生形状（流式按事件拼装后再转回上游协议原生形状），
--    由前端按入站/上游协议解析展示；写入后不再做条数清理，全部保留
-- ---------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS usage_payload (
  usage_payload_id   INTEGER PRIMARY KEY AUTOINCREMENT,  -- 报文行号，自增
  usage_detail_id    INTEGER NOT NULL,   -- 关联明细行 usage_detail.usage_detail_id（松引用）
  inbound_request    TEXT,               -- 客户端发来的原始请求体（原生协议 JSON）；未捕获为 NULL
  inbound_headers    TEXT,               -- 客户端发来的 HTTP header（JSON 对象，原样保存不脱敏）；未捕获为 NULL
  upstream_request   TEXT,               -- 提示词注入后实际发往上游的请求体（上游协议原生形状）；未捕获为 NULL
  upstream_response  TEXT,               -- 上游原生响应：非流式=上游返回原文；流式=拼装后转回上游协议原生形状
  request_truncated  INTEGER NOT NULL DEFAULT 0,  -- 1=入站请求体因超上限被截断
  upstream_request_truncated INTEGER NOT NULL DEFAULT 0,  -- 1=上游请求体因超上限被截断
  response_truncated INTEGER NOT NULL DEFAULT 0,  -- 1=上游响应因超上限被截断
  is_stream          INTEGER NOT NULL DEFAULT 0,  -- 1=流式（响应为拼装结果）/ 0=非流式（上游原文）
  created_time       INTEGER NOT NULL,   -- 入库时刻
  update_time        INTEGER NOT NULL    -- 报文行只插不改，= created_time
);

CREATE INDEX IF NOT EXISTS idx_usage_payload_detail ON usage_payload (usage_detail_id);
CREATE INDEX IF NOT EXISTS idx_usage_payload_time   ON usage_payload (created_time DESC);

-- ---------------------------------------------------------------------
-- 9. request_filters —— 请求过滤器（网关把请求转发给上游前，按 sort_order 依次套用的改写规则）
--    每条规则一个动作（策略），当前只有一种 rule_kind：
--      system-prompt  注入系统提示词（mode=append/prepend）
--    规则体按 rule_kind 存放在 rule_config（JSON），rule_kind 仅作展示与索引的冗余列；
--    只有 enabled=1 的规则参与执行，顺序即 sort_order（本期等于创建顺序，不提供上移/下移）
-- ---------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS request_filters (
  request_filter_id INTEGER PRIMARY KEY AUTOINCREMENT,  -- 过滤器号，自增
  name              TEXT NOT NULL,             -- 显示名
  enabled           INTEGER NOT NULL DEFAULT 1,  -- 是否启用：1=启用 / 0=停用
  sort_order        INTEGER NOT NULL DEFAULT 0,  -- 执行顺序（由小到大；等于创建顺序）
  rule_kind         TEXT NOT NULL,             -- 规则类型：system-prompt
  rule_config       TEXT NOT NULL,             -- 规则体（JSON 字符串，含 kind 标签，按 rule_kind 解释）
  created_time      INTEGER NOT NULL,          -- 创建时间
  update_time       INTEGER NOT NULL           -- 更新时间
);

CREATE INDEX IF NOT EXISTS idx_request_filters_order ON request_filters (sort_order, request_filter_id);

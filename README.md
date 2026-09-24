# AI Start

管理开发者工具的桌面工具箱：**一键安装、一键更新、一键把任意模型接入桌面客户端**。

目前有左侧边栏的五个页面：

- **面板** — 本地网关的运行状态、调用统计、自动切换情况，以及应用/模型总览
- **应用** — Claude Desktop、DeepSeek Desktop、Codex、ZCode、WorkBuddy，负责安装 / 更新 / 模型接入
- **模型** — 统一管理三种上游协议的模型，并支持自动切换
- **过滤器** — 在请求转发前按规则改写请求（注入系统提示词）
- **统计** — Token 消耗贡献图与每一次请求的明细

技术栈：Tauri 2 + Vue 3 + vue-router + Tailwind CSS v4 + shadcn-vue + morphicons。

---

## 网关对外协议

网关在 `127.0.0.1` 上**同时暴露三种协议**，入站用哪种协议、上游用哪种协议互不影响：

| 端点 | 入站协议 |
| --- | --- |
| `POST /v1/messages` | Anthropic Messages |
| `POST /v1/chat/completions` | OpenAI Chat Completions |
| `POST /v1/responses` | OpenAI Responses |
| `GET /v1/models` | 返回网关对外暴露的全部 Claude 路由 |

- **API Key 固定为 `aiStart`**；对外暴露一组 `claude-*` 路由（见 `gateway/mod.rs` 的 `MODEL_ROLES`）。
  `x-api-key` 与 `Authorization: Bearer` 两种携带方式都接受。
- 模型名**不能**是不透明别名：Claude Desktop 会丢弃非 Anthropic 形态的名字，模型菜单会变空。
  上游真实模型由网关在转发时替换，用户看到的名字来自 `inferenceModels` 的 `labelOverride`。
- 错误响应会跟随入站协议：Anthropic 返回 `{"type":"error","error":{...}}`，
  OpenAI 返回 `{"error":{...}}`。

一次请求的完整链路是：

```
客户端(协议 A) → decode_request → 规范请求
              → 上游 Provider.encode_request → 上游(协议 B)
              → 上游 Provider.decode_response/decode_stream_event → 规范响应/事件
              → Provider(A).encode_response/encode_stream_event → 客户端(协议 A)
```

`anthropic-messages` 的这四个方法全部使用默认实现（恒等变换），所以它天然是"直通"路径；
另外两种协议各自实现双向翻译。流式情况下规范事件会经过 `encode_stream_event` 重新编码，
OpenAI Completions 输出 `chat.completion.chunk` + `data: [DONE]`，
Responses 输出 `response.created` / `response.output_text.delta` / `response.completed` 事件序列。

---

## 为什么需要本地网关

Claude Desktop 的第三方推理（3P）模式只认 **Anthropic Messages 协议**（`POST /v1/messages`）。
而绝大多数上游（DeepSeek、OpenAI、Ollama、各类 LiteLLM 代理）说的是 **OpenAI 协议**。

所以本项目内置一个只监听 `127.0.0.1` 的网关：

```
Claude Desktop ──(Anthropic Messages)──▶ 本地网关 ──┬─(Anthropic Messages)─▶ Anthropic / LiteLLM
                                                    ├─(Chat Completions)──▶ DeepSeek / Ollama / OpenAI
                                                    └─(Responses)─────────▶ OpenAI Responses
```

Claude Desktop 通过 3P gateway 配置指向本地端口，用户就能在 Claude Desktop 里选用任意后端模型。

---

## 三种协议如何统一：策略模式

核心设计是 **以 Anthropic Messages 作为规范模型（canonical）**，其余两种协议做双向翻译。

```
src-tauri/src/
├── domain/
│   └── canonical.rs            # CanonicalRequest：以 Anthropic Messages 为基准的请求模型
├── providers/
│   ├── mod.rs                  # ModelProvider trait + 静态注册表 + SSE 状态机
│   ├── anthropic_messages.rs   # 直通（is_passthrough = true）
│   ├── openai_completions.rs   # anthropic-messages ⇄ chat/completions
│   └── openai_responses.rs     # anthropic-messages ⇄ responses
└── gateway/
    ├── server.rs               # axum 路由：/v1/messages、/v1/models、/health
    └── sse.rs                  # 上游 SSE 拆帧
```

`ModelProvider` 是策略接口，四个方法覆盖一次推理的完整生命周期：

| 方法 | 职责 |
| --- | --- |
| `encode_request` | 规范请求 → 上游线格式 |
| `headers` | 认证与协议头（`x-api-key` / `Authorization: Bearer`） |
| `decode_response` | 上游响应 → 规范响应（非流式） |
| `decode_stream_event` | 上游 SSE 分片 → Anthropic SSE 事件序列 |

新增一种协议 = 新增一个 `impl ModelProvider`，网关与 UI 零改动。

**流式翻译**由 `StreamState` 承担，它实现了 Anthropic SSE 的状态机：

```
message_start → content_block_start → content_block_delta* → content_block_stop
              → message_delta → message_stop
```

各 Provider 只需调用 `state.delta(...)` / `state.open_tool(...)` / `state.finish(...)`，
块的开闭、索引递增、收尾事件全部自动处理，天然支持文本 / 思维链 / 工具调用三种块的相互切换。
思维链映射到 Anthropic 的 `thinking` 块，工具调用映射到 `tool_use` + `input_json_delta`。

---

## 模型管理与自动切换

模型表单只保留接入必需的字段：显示名称、上游协议、Base URL、API Key、上游模型 ID，
以及一个「支持 1M 上下文」勾选框。

- **获取模型列表 + 可搜索下拉**：按所选协议请求上游模型列表（`{base}/v1/models`），
  解析 OpenAI `{data:[{id}]}`、Anthropic `{data:[{id,display_name}]}`、
  Ollama `{models:[{name}]}` 与裸数组等结构，去重排序后直接填入输入框内的下拉，
  下拉自带搜索框（输入即过滤），也允许直接手填未在列表中的 ID。
- **支持 1M 上下文**：勾选后会以 `supports1m` + `prefer1m` 写入 Claude Desktop 的
  `inferenceModels`，Claude 的模型选择器会额外提供一个 1M 变体。
- **启用 / 使用中**：列表每行右侧的按钮用于切换当前接管的模型，启用后该行会变绿。
- **自动切换**：模型列表右上角的开关。开启后，网关向上游发起请求失败时
  （网络错误、5xx、401/403/404/408/429），会按列表顺序自动尝试下一个模型，
  成功后继续本次请求。面板会显示已触发次数与最近一次 `X → Y` 的切换记录。

请求本身的形状错误（400/422）不会触发切换 —— 那种错误换模型也一样会失败，
只会掩盖真正的问题。

## 请求过滤器

「过滤器」页面用于在网关把请求转发给上游**之前**，为请求注入系统提示词。每条过滤器一个动作，
只有启用的参与执行，按列表顺序（即创建顺序）依次叠加：

| 规则类型 | 作用 |
| --- | --- |
| 注入系统提示词 | 在 `system` 上追加或前置一段文本 |

实现要点：改写作用在**规范请求**（Anthropic 形态）上，因此三种入站协议统一生效；
注入点在 `gateway/server.rs::handle` 的 `decode_request` 之后、自动切换循环之外，
保证一次请求只套用一次。

## 用量统计

每次经由网关的调用都会追加一条记录到 `<配置目录>/usage.jsonl`（超过 20000 条自动裁剪到 10000 条），
记录包含时间、入站/上游协议、模型、输入/输出 Token、耗时、是否成功、是否发生了自动切换。

「统计」页面顶部是 GitHub 风格的贡献图（最近 53 周的每日 Token 消耗，5 档颜色），
下面是逐条请求的明细表。`usage_summary(days)` 返回按天与按模型的聚合，
`usage_records(limit)` 返回最近的明细。

## 平台抽象

```
src-tauri/src/platform/
├── mod.rs        # AppConfigurator trait + 平台工厂 + 环境变量展开
├── windows.rs    # Windows 实现
└── fallback.rs   # 非 Windows 占位实现
```

`AppConfigurator` 负责 `detect` / `is_configured` / `apply` / `clear`，新增一个客户端只需实现这个 trait。

### Claude Desktop 的接入方式

写入 Claude Desktop 的**用户级配置目录**（与应用内「Configure Third-Party Inference」同一位置，无需管理员权限）：

```
%LOCALAPPDATA%\Claude-3p\configLibrary\
├─ _meta.json                                 # appliedId 指向本应用的 profile
└─ 00000000-0000-4000-8000-000000008931.json  # 具体配置
     inferenceProvider            = gateway
     inferenceGatewayBaseUrl      = http://127.0.0.1:<port>
     inferenceGatewayApiKey       = aiStart
     inferenceGatewayAuthScheme   = bearer
     inferenceModels              = [{"name":"claude-sonnet-5","labelOverride":"aiStart · Sonnet"}, ...]
     modelDiscoveryEnabled        = false
     disableDeploymentModeChooser = true
```

应用时还会清除自己早期写在 `HKCU\SOFTWARE\Policies\Claude` 的托管策略 —— 托管级优先于用户级文件，
留着会把新写的配置整个盖掉。

注意：Claude Desktop 只在**启动时**读取配置，且**机器级**（`HKLM\SOFTWARE\Policies\Claude`）策略存在时
会完全忽略用户级配置与用户级文件。所以应用完成后需要完全退出并重新打开 Claude Desktop。

### DeepSeek Desktop 的说明

DeepSeek Desktop 没有公开的程序化配置格式，因此这里是**按最通用的 OpenAI 兼容结构写出参考配置**
（默认位置 `%APPDATA%\DeepSeek\config.json`，可在「设置」中改成你安装版本的真实路径）。
这一点在应用卡片与保存提示中都会明确说明。

### WorkBuddy 的说明

WorkBuddy 的本地自定义模型就落在**用户级**的 `%USERPROFILE%\.workbuddy\models.json`。
实测它的 `CustomModelsJSON` 特性已开启，内置 provider 会**监听**该文件（约 1s 去抖后自动同步），
因此这里走 `ApplyMode::DirectConfig`：`platform/workbuddy.rs` 把 aiStart 的条目**合并**进那个数组，
其它模型与整体形状保持不变。

- 条目形如
  `{ "id": "aiStart", "name": "aiStart", "vendor": "aiStart", "url": "http://127.0.0.1:8931/v1/chat/completions", "apiKey": "workbuddy", "supportsToolCall": true, ... }`。
  `url` 必须是**完整**地址且以 `/chat/completions` 结尾 —— WorkBuddy 的校验规则与 GUI 占位符都是这个形状，
  带该后缀时运行时不会再自动补全。
- **不写 `availableModels`**：WorkBuddy 一旦读到该字段就用它**替换**可用模型集合（不与内置模型合并），
  写了会把自带模型全部隐藏。
- 写完后**无需重启**，新建一个对话即可刷新模型选择器。企业管理员若禁用了「个人自定义模型」，该条目会被忽略。

### Codex 的接入方式

Codex 桌面版与 Codex CLI 共用 Codex home（`CODEX_HOME` 优先，默认 `%USERPROFILE%\.codex`），
自定义 provider 就写在这里的 `config.toml`。写入形状参考 CC Switch：

```toml
model_provider = "aistart"
model = "aiStart"

[model_providers.aistart]
name = "aiStart"
base_url = "http://127.0.0.1:<port>/v1"
wire_api = "responses"
experimental_bearer_token = "<该应用专属的网关 Key>"
```

- **顶层 `model_provider` 必须声明**：缺了它，Codex 会回落到内置 `openai` provider，
  顶层的 base_url 被整个忽略，请求直连 api.openai.com。
- **模型目录是必需的一环**：Codex 的模型列表（GUI 选择器与 `/model`）完全由
  `model_catalog_json` 指向的目录决定，**不在目录里的 slug 会被忽略，顶层 `model` 还会被
  桌面版回落成目录里的某个模型**（实测：只写 provider 时，应用后 46 秒 `model` 就从
  `aiStart` 被改回了别的）。所以应用时会把 `aiStart` 作为一条目录条目并进当前那份目录里。
  条目字段很多（含大段提示词），凭空造会被判非法，因此实现是**克隆目录里已有的条目**
  再改 slug / 显示名 / 上下文窗口（CC Switch 也是靠它自己的那份目录做到模型可见的）。
  勾选「支持 1M 上下文」时会把条目的 `context_window` / `max_context_window` 提到 1M。
- **不写 `auth.json`**：那里存的是官方 ChatGPT / Codex 登录缓存，桌面版靠它识别官方账号
  （远程控制、官方插件）。凭据放在 provider 表的 `experimental_bearer_token` 里，
  与 CC Switch「切换第三方供应商时保留官方登录」的做法一致。
- 只合并 aiStart 自己的 provider、目录条目与顶层 `model_provider` / `model`；文件里其余的
  provider、模型条目、注释与未知字段原样保留，「移除模型配置」也只删这一部分。
- **不要和 CC Switch 同时接管**：两者都写这同一份 `config.toml`（本机上 CC Switch 还会在
  `[model_providers.custom]` 里放 `PROXY_MANAGED` 占着 15721 端口），谁后写谁生效。
  用本应用接入前先关掉 CC Switch 的接管，免得互相覆盖。
- 已知边界：Codex 桌面版的模型选择器还会按官方登录态做门控，未登录官方账号时自定义模型
  可能不出现在 GUI 里（上游标记为 not planned，CC Switch 同样修不了）；命令行 `codex` 的
  `/model` 菜单与请求路由不受此限制。目录里若没有可克隆的条目（例如从未配过自定义模型），
  应用只写 provider 而不动目录，并在结果里说明。

### 「手动应用」的客户端

ZCode（智谱 GLM 官方 ADE）只能在 GUI 里手动添加「自定义 / OpenAI 兼容」供应商，官方未公开
配置文件格式，因此走 `ApplyMode::Manual`：`ManualConfigurator` **不写任何第三方文件**，
点「应用」只记录绑定并弹出「对接说明」（接口地址 / API Key / 模型名称 / 协议格式 + 调用示例，
逐项可复制），由用户照着填写。

- 安装/更新目前只配了 `Manual` 兜底（打开官方下载页）；`upgrade` 里的探测锚点（进程名、注册表
  `DisplayName`、安装位置）标为「待实测」，装上后按实际值回填，届时再接自动下载/静默安装。

---

## 一键安装 / 一键更新

`commands/apps.rs` 中的安装管线是完整的：

1. 若目录中为该应用配置了 `installerUrl`（安装包直链）→ 流式下载到缓存目录（带进度事件
   `install://progress`）→ 自动启动安装程序（`.msi` 走 `msiexec /i`）。
2. 若未配置直链 → 打开官方下载页，安装完成后由卸载表扫描自动识别。

安装状态通过扫描 Windows 卸载注册表项（`HKLM` / `HKCU` × 64 / 32 位视图）与常见安装路径得到。

---

## 目录结构

```
├── src/                          # 前端
│   ├── pages/                    # 页面：Dashboard / Apps / Models / Filters / Stats
│   ├── components/
│   │   ├── apps/                 # 应用卡片等
│   │   ├── models/               # 模型行 / 表单 / 网关面板
│   │   ├── filters/              # 过滤器卡片 / 表单
│   │   ├── stats/                # 贡献图
│   │   ├── layout/               # AppShell / SidebarNav / SettingsDialog
│   │   ├── common/               # 通用：MorphIconBox / EmptyState / ConfirmDialog ...
│   │   └── ui/                   # shadcn-vue 组件
│   ├── composables/              # useApps / useModels / useFilters / useGateway / useUsage / useSettings
│   ├── lib/                      # ipc（invoke 封装）/ types / format / notify / open
│   └── router/
└── src-tauri/src/                # 后端
    ├── domain/                   # 领域模型 + 规范请求 + 内置目录 + 过滤器规则
    ├── providers/                # 三种协议的双向翻译（decode/encode 请求、响应、事件流）
    ├── platform/                 # 客户端配置适配层
    ├── gateway/                  # 本地网关（axum + SSE + 用量记录 + 请求过滤器）
    ├── commands/                 # Tauri 命令
    ├── filters.rs                # 请求过滤器：持久化 + 规则套用
    ├── usage.rs                  # 用量记录与聚合
    └── settings.rs               # 配置持久化
```

---

## 开发

```bash
pnpm install
pnpm tauri dev

# 仅前端（dev server 端口为 16271）
pnpm dev

# 校验
pnpm build                                              # vue-tsc + vite build
cargo test --manifest-path src-tauri/Cargo.toml
cargo check --manifest-path src-tauri/Cargo.toml
```

前端 dev server 使用 `16271`（HMR `16272`），可在 `vite.config.ts` 中调整；
本地网关默认监听 `127.0.0.1:8931`，可在应用「设置」中修改。

配置与模型数据保存在 `%APPDATA%\com.aistart.toolbox\settings.json`（路径见应用「设置」对话框）。

## 已知边界

- 平台自动配置目前只实现了 Windows；`AppConfigurator` trait 已为 macOS / Linux 预留。
- Claude Desktop 的 3P 模式需要相应版本的官方客户端；机器级策略存在时会覆盖本应用的配置。
- DeepSeek Desktop 的配置属于参考写入，需按实际安装版本核对路径。

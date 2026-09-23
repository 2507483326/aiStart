# AI Start

管理开发者工具的桌面工具箱：**一键安装、一键更新、一键把任意模型接入桌面客户端**。

目前有左侧边栏的三个页面：

- **面板** — 本地网关的运行状态、调用统计、自动切换情况，以及应用/模型总览
- **应用** — Claude Desktop、DeepSeek Desktop，负责安装 / 更新 / 模型接入
- **模型** — 统一管理三种上游协议的模型，并支持自动切换

技术栈：Tauri 2 + Vue 3 + vue-router + Tailwind CSS v4 + shadcn-vue + morphicons。

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
以及一个「支持 1M 上下文」开关。

- **获取模型列表**：按所选协议请求上游的模型列表接口（`{base}/v1/models`），
  支持 OpenAI 的 `{data:[{id}]}`、Anthropic 的 `{data:[{id,display_name}]}`、
  Ollama 的 `{models:[{name}]}` 以及裸数组等常见返回结构，结果去重排序后可直接选中。
- **支持 1M 上下文**：勾选后会以 `supports1m` + `prefer1m` 写入 Claude Desktop 的
  `inferenceModels`，Claude 的模型选择器会额外提供一个 1M 变体。
- **自动切换**：模型列表右上角的开关。开启后，网关向上游发起请求失败时
  （网络错误、5xx、401/403/404/408/429），会按列表顺序自动尝试下一个模型，
  成功后继续本次请求。面板会显示已触发次数与最近一次 `X → Y` 的切换记录。

请求本身的形状错误（400/422）不会触发切换 —— 那种错误换模型也一样会失败，
只会掩盖真正的问题。

## 平台抽象

```
src-tauri/src/platform/
├── mod.rs        # AppConfigurator trait + 平台工厂 + 环境变量展开
├── windows.rs    # Windows 实现
└── fallback.rs   # 非 Windows 占位实现
```

`AppConfigurator` 负责 `detect` / `is_configured` / `apply` / `clear`，新增一个客户端只需实现这个 trait。

### Claude Desktop 的接入方式

写入**用户级**注册表策略（无需管理员权限）：

```
HKEY_CURRENT_USER\SOFTWARE\Policies\Claude
  inferenceProvider            = gateway
  inferenceGatewayBaseUrl      = http://127.0.0.1:<port>
  inferenceGatewayApiKey       = <本机生成的 token>
  inferenceGatewayAuthScheme   = bearer
  inferenceModels              = [{"name":"ai-start-...","labelOverride":"..."}]
  modelDiscoveryEnabled        = false
  disableDeploymentModeChooser = true
```

注意：Claude Desktop 只在**启动时**读取配置，且**机器级**（`HKLM\SOFTWARE\Policies\Claude`）策略存在时
会完全忽略用户级配置。所以应用完成后需要完全退出并重新打开 Claude Desktop。

### DeepSeek Desktop 的说明

DeepSeek Desktop 没有公开的程序化配置格式，因此这里是**按最通用的 OpenAI 兼容结构写出参考配置**
（默认位置 `%APPDATA%\DeepSeek\config.json`，可在「设置」中改成你安装版本的真实路径）。
这一点在应用卡片与保存提示中都会明确说明。

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
│   ├── pages/                    # 页面：AppsPage / ModelsPage
│   ├── components/
│   │   ├── apps/                 # 应用卡片等
│   │   ├── models/               # 模型卡片 / 表单 / 网关面板
│   │   ├── layout/               # AppShell / NavTabs / SettingsDialog
│   │   ├── common/               # 通用：MorphIconBox / EmptyState / ConfirmDialog ...
│   │   └── ui/                   # shadcn-vue 组件
│   ├── composables/              # useApps / useModels / useGateway / useSettings
│   ├── lib/                      # ipc（invoke 封装）/ types / format / notify / open
│   └── router/
└── src-tauri/src/                # 后端
    ├── domain/                   # 领域模型 + 规范请求 + 内置目录
    ├── providers/                # 三种协议的策略实现
    ├── platform/                 # 客户端配置适配层
    ├── gateway/                  # 本地网关（axum + SSE）
    ├── commands/                 # Tauri 命令
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

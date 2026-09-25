# 转发技术方案（现状架构）

> 2026-09-25。基于当前代码整理（`src-tauri/src/providers`、`src-tauri/src/gateway`、
> `src-tauri/src/filters.rs`）。协议字段口径详见 `protocol-comparison.md`，其审核结论见
> `protocol-comparison-review.md`。现状的问题清单与演进方案见 `forwarding-optimization.md`。

---

## 1. 总览

网关在本地起一个 axum 服务，对外暴露三个入站端点，对内按「当前模型」决定上游协议。
所有跨协议转发走**统一中间表示（规范层）**，方向永远是：

```
客户端报文 ──decode_request──▶ 规范层（CanonicalRequest）──encode_request──▶ 上游报文
上游响应   ──decode_response─▶ 规范层（Anthropic 形状） ──encode_response─▶ 客户端响应
上游 SSE   ──decode_stream───▶ 规范事件（Anthropic SSE） ──encode_stream───▶ 客户端 SSE
```

```
                    ┌────────────────────────────────────────────────┐
 /v1/messages ─────▶│ decode_request(inbound)                        │
 /v1/chat/complet ▶│   → CanonicalRequest { raw, body }             │
 /v1/responses ────▶│   → validate() → filters::apply()              │
                    │   → candidates_for(model) ── 逐个 dispatch ────┼──▶ 上游
                    │                                                │
                    │ 非流式: decode_response → encode_response      │
                    │ 流式:   decode_stream_event → (规范 SSE)        │
                    │         → encode_stream_event                  │
                    │         + ResponseAssembler（落库）             │
                    └────────────────────────────────────────────────┘
```

三个入站端点（`gateway/server.rs:211-233`）只是把 `ModelFormat` 打上标记后进同一个 `route()`，
没有按端点的独立逻辑——入站协议本身只有 3 处小分叉（错误体形状、错误事件形状、流形态判定）。

## 2. 规范层（canonical）

`domain/canonical.rs`。两份并存的状态：

- `raw: Value` —— 入站报文的**重建版**（不是客户端原文；OpenAI 系 decode 时已按规范重拼）；
- `body: RequestBody` —— 类型化的规范字段（Anthropic 内容块模型 + OpenAI 参数命名，见
  protocol-comparison.md §1「本项目的取法」）。

关键设计：

| 设计点 | 做法 |
| --- | --- |
| 规范形状 | 消息/内容块照抄 Anthropic Messages（`tool_use`/`tool_result`/`thinking`/`text`/`image`），标量参数用 OpenAI 命名（`reasoning_effort`/`response_format`/`parallel_tool_calls`…） |
| 藏匿字段 | 任何线上协议都没有的内部字段收进 `_canonical`（当前只有 `include_usage`、`max_tokens_field`），编码时整层删除（`wire.rs` 里 `CANONICAL_ONLY_KEY: Dropped`） |
| 一致性 | 改写必须走 `map_raw()`：改 raw 后重新反序列化 body，两者永不脱节（过滤器用它） |
| 校验 | `validate()` 拒掉无法保真转换的请求（当前只挡 `n > 1`） |

## 3. 协议能力表（wire.rs）——if/else 的第一层收敛

`providers/wire.rs` 把「每个协议能表达什么」集中为三张静态表 `ProtocolProfile`：

```rust
struct ProtocolProfile {
    reasoning_fields: &'static [&'static str],   // 思考字段按优先级
    rules: &'static [FieldRule],                 // 规范字段 → 落地方式
}
enum Slot { Same, Renamed(&'static str), Custom, Transformed, Dropped }
```

`apply_common_fields()` 按表落地标量字段：`Same`/`Renamed` 写入、`Transformed`/`Dropped` 删除、
`Custom` 留给 provider 拼结构。两种填充策略（`Fill`）：

- `Overwrite` —— 重建型报文（两个 OpenAI provider）直接覆盖；
- `IfAbsent` —— Anthropic 透传报文，只补缺失字段，不动客户端原样字段（保住 `tools` 上的
  `cache_control` 等扩展）。

**守护测试**：`protocol_profiles_cover_every_canonical_field` 用不带 `..Default::default()` 的
完整字面量构造 `RequestBody`——新增规范字段会先编译失败，再被「规范字段 = 已映射 ∪ 显式丢弃」
断言拦住。静默丢字段在**规范字段层面**结构上不可能。

## 4. 三个 Provider

`providers/mod.rs` 的 `ModelProvider` trait 是唯一的多态点（`provider_for(format)` 返回静态实例）：

| 方法 | Anthropic Messages | OpenAI Completions | OpenAI Responses |
| --- | --- | --- | --- |
| `is_passthrough` | ✅（raw 直通） | ❌（重建） | ❌（重建） |
| `decode_request` | raw + 抬升 2 个藏匿字段（`output_config.effort`→`reasoning_effort`、`disable_parallel_tool_use`→`parallel_tool_calls` 取反） | 重建：拆 system 消息、`tool` role→`tool_result` 块、content parts→块 | 重建：`input` items→消息、`function_call(_output)`→tool 块、`instructions`→system |
| `encode_request` | raw + `apply_common_fields(IfAbsent)` + 4 个变形字段落 Anthropic 写法 | 全新拼装：system→messages[0]、工具包 `function` 壳、tool_result→`tool` role 消息 + 标量表 | 全新拼装：消息→`input` items、system→`instructions`、format/effort 装进 `text.format`/`reasoning.effort` |
| `encode_request_passthrough`（同协议快路） | — 无需快路：`raw` 本来就是客户端原文，`encode_request` 即直通 | ✅ 客户端原文 + 换 `model` + 仅在 system 被过滤器改过时重写 system 消息 | ✅ 客户端原文 + 换 `model` + 仅在 system 被改过时重写 `instructions` |
| `decode_stream_event` | 原样记账转发（`message_start/delta/stop`、error、ping） | 分片→块状态机（`BlockNormalizer`），思考读 `reasoning_content`/`reasoning`/`reasoning_details[]` | 事件→块状态机，`output_index` 区分并行工具 |
| `encode_stream_event` | 恒等（规范即 Anthropic） | 块→`chat.completion.chunk` 分片（含 `[DONE]`、可选 usage 尾片） | 块→`response.*` 事件（text item 开闭、function_call item） |

**流式核心**：`StreamState`（上游解码侧，含 `BlockNormalizer`）把任意上游增量收成合法 Anthropic
块序列（不变式 I1–I5：单开块、载荷进匹配块、索引递增、并行工具缓冲串行化、工具必有块）；
`WireState`（出站编码侧）按客户端协议重建结构。两个状态机把「上游五花八门」与「客户端五花八门」
解耦在规范层两侧。

**落库**：`ResponseAssembler` 把规范事件拼回完整消息 → `encode_response` 转成上游协议原生形状
（与非流式响应同一形状）→ `usage::record_with_payload` 落库（256KB 截断）。

### 4.1 为什么只有 Anthropic 是直通（请求侧的成因与已落地的修法）

> **状态**：请求侧已改为同协议直通（见下「修法」）；**响应与流侧仍是重建**（分片 `id`/`created`
> 被重生、思考通道被改名等，见 D2 与 `forwarding-optimization.md` 方案 A2）。

`Completions → Completions` 这类同协议转发原本要重建报文，**不是协议要求，而是内部不变式的后果**：

1. **`CanonicalRequest` 的不变式是「raw = 规范形状」**。`apply_common_fields` 按 `wire.rs` 的槽位表
   往 raw 写字段名，过滤器写 `raw["system"]`——都假定 raw 用规范字段名。Anthropic 入站报文
   **本来就是规范形状**，`decode_request` 可以原样留下（只抬升两个藏匿字段）；两个 OpenAI 协议
   不是，只能重建一份规范形状的 raw 来维持不变式——客户端原始报文在入站第一步就丢了，
   `encode_request` 手上没有可透传的东西，只好从类型化 body 重拼。
2. **编解码器是规整器，不是恒等函数**。OpenAI 系的 `decode_stream_event` 有状态（喂
   `BlockNormalizer`、拆 `tool_calls`、抽 usage）——跨协议时它必须如此；Anthropic 的那个近似恒等。
   规范层是照着 Anthropic 定的，于是只有 Anthropic 白拿一份恒等转换。
3. **单一代码路径**。`route()` 只有一条 decode → filter → encode 流程，`is_passthrough` 全程只在
   一处被消费（`server.rs` 决定要不要合成 `message_start`）。

**修法（已落地）**：不动「raw = 规范形状」这条不变式（过滤器与协议表都依赖它），而是在入站处
**另存一份客户端原文**：

- `CanonicalRequest` 新增 `client_raw: Option<Value>`（客户端原文）与 `dirty: BTreeSet<String>`
  （被过滤器改写过的规范字段名）；网关在 `handle()` 里 `decode_request(raw.clone())?.retain_client_raw(raw)`。
- 过滤器套用后 `mark_dirty("system")`，直通路径据此决定要不要重写系统消息。
- 三个 provider 实现 `encode_request_passthrough()`：以 `client_raw` 为底，只覆盖上游模型名、
  `_canonical`、被改写的规范字段（Anthropic 走默认实现返回 `None`，它的 `encode_request` 本就是直通）。
- 网关 `encode_upstream_request()` 按「入站协议 == 上游协议」选快路，否则回退规范层重建。

顺带修掉的旧行为：同协议的 `tool_choice` 不再被丢（此前 decode 抬升列表里没有它，见 D1 ①）、
客户端没写 `max_tokens` 时才补默认值、扩展键与字段名原样带到上游。

**仍然存在的代价（响应与流侧，待方案 A2）**：

| 位置 | 往返一趟的变化 | 证据 |
| --- | --- | --- |
| ~~请求~~ | ~~OpenRouter 的 `provider`/`routes`/`transforms`、message 的 `name`、`logit_bias`/`seed`/penalties 全丢；多条 system/developer 消息被并成一条；图片只剩 data:URL；没写 `max_tokens` 被塞 8192；`stop` 无谓往返~~ **已修**：请求侧改为同协议直通，只覆盖 `model` / `_canonical` / 被过滤器改过的 system | `client_raw` + `is_dirty("system")` |
| 响应（非流） | `system_fingerprint`/`logprobs` 丢失、`created` 重生、`refusal` 变纯文本 | `encode_response` 重建 `choices[0]` |
| **流** | **分片 `id` 由上游 `chatcmpl-…` 变成 `msg_<uuid>`、`created` 每片重生** | 合成的 `message_start` + `StreamState::new` 生成 id |
| **流** | **思考通道被改名：上游发 `reasoning` → 客户端收到 `reasoning_content`** | 读侧按 `reasoning_fields` 逐个尝试、另有 `reasoning_details[]` 兜底；写侧硬编码 `reasoning_content` |
| 流 | `system_fingerprint`/`logprobs` 丢失、分片边界可能被合并、usage 尾片改为合成 | `BlockNormalizer` + `encode_stream_done` |

请求侧直通的前提是「过滤器改规范字段并**记录**被改字段集合」（`filters.rs` 里的 `mark_dirty` 已落地）：
直通以客户端原文为底、只覆盖 `dirty` 里的字段，过滤器改了却不标记，注入会在直通路径上**静默失效**
（反过来不再有泄漏风险——规范 raw 不参与直通，`raw["system"]` 这类规范化写入发不出去）。

## 5. 请求生命周期（gateway/server.rs）

```
route(inbound)
 ├─ extract_token → source_app_for（token 反查来源应用）
 ├─ decode_request → validate → filters::apply（循环外，重试只套一次）
 ├─ candidates_for(model)：别名/auto → 全量候选；命中显示名 → 只该模型
 ├─ for candidate in candidates:            // 自动切换循环
 │    dispatch: encode_upstream_request → POST → 非 2xx 时按状态码判 retryable
 │              （入站协议 == 上游协议 → 免转换快路；否则规范层重建）
 │    成功且 index>0 → record_failover + active_model_id 移交给接手方 + 事件
 │    失败且不可重试 → break
 ├─ 非流式：decode_response → 取 usage → record_usage → encode_response
 └─ 流式：SSE 管道（见下）
```

流式管道（`server.rs:570-698`）：

```
parse_sse_stream(upstream)
  → [DONE]? → decode_stream_done
  → decode_stream_event（上游→规范）
      → assembler.apply（落库侧累积）
      → encode_stream_event（规范→客户端）→ yield
  → 流末：wire_state 补齐 usage → encode_stream_done（usage 尾片 / [DONE] / response.completed）
  → verdict（Ok / EmptyReasoningOnly / Truncated / UpstreamError）→ record_usage
```

## 6. 模型选择与自动切换

- `settings.candidates_for(requested)`（`settings.rs:88-101`）：
  - 请求模型名 = 网关别名 `aiStart` / `auto`（`gateway::is_auto_alias`，大小写不敏感）或空/未命中
    → `candidate_models()`：当前模型优先 + `auto_failover` 开启时追加其余全部模型；
  - 请求模型名精确命中某个模型显示名 → 只调用该模型，自动切换对它无效。
- 切换成功后把 `active_model_id` 持久移交给接手方（后续请求直接以它为首选，不再先撞失败的主模型）。
- 重试判定：5xx 或 401/403/404/408/429 可重试；编码失败（本地 bug）不重试。
- 失败记录：最后一次尝试的 upstream_url / upstream_model 随 RouteFailure 落库。

## 7. 过滤器（filters.rs）

进程内缓存 + 全量落库的 `Vec<RequestFilter>`，规则是带标签的枚举（`FilterRule::SystemPrompt`
append/prepend）。`apply()` 在自动切换循环**之外**按序套用启用的规则，通过 `map_raw` 直接改
`raw["system"]` 再重建 body，并在 `CanonicalRequest` 上 `mark_dirty("system")`——同协议直通以客户端
原文为底、只覆盖被标记过的规范字段，标记这一步就是注入能在快路上生效的开关。当前只有系统提示词
注入一种规则；来源应用维度（`applied`、`app_tokens`）已存在于 settings，但过滤器尚未按应用过滤。

## 8. 入站协议的三处分叉点

除了 provider trait，整个网关只剩三处 `match inbound`：

1. `api_error`（server.rs:49）——Anthropic 形 `{type:error,error:{...}}` vs OpenAI 形 `{error:{...}}`；
2. `error_events`（server.rs:88）——流内错误事件同上两种形状；
3. `emit_initial`（server.rs:567）——上游非透传时先给客户端发一条合成的 `message_start`。

这是刻意收敛的结果：新增入站协议只需要 ① `ModelFormat` 变体 ② 一张协议表 ③ 一个 provider 实现
④ 这三处各加一个臂。

## 9. 现状已知问题（索引）

按代码与审核发现的问题，详细分析与修法在 `forwarding-optimization.md`：

- **D1** tool_choice 三个方向的转换缺陷：① Completions 入站静默丢 tool_choice（**同协议已随直通修掉**，
  跨协议仍在：抬升列表缺该字段）② `encode_tool_choice` 缺 `"function"` 对象臂 ③ Responses 的
  `tool_choice()` 丢 `any`/`required`；
- **D2** 同协议非 Anthropic 方向不是直通：**请求侧已修**（`client_raw` + 免转换快路，2026-09-25）；
  **响应与流侧仍重建**——分片 `id`/`created` 被重生、思考通道被改名、`system_fingerprint`/`logprobs`
  丢失（方案 A2 待做）。附带根因：`emit_initial` 用 `!upstream_provider.is_passthrough()` 判定
  「要不要合成 `message_start`」——这是「这个 provider 的 raw 是否透传」这个维度，而不是
  「入站协议 == 上游协议」；
- **D3** 过滤器必须 `mark_dirty`：直通只覆盖被标记的规范字段，过滤器改了却忘记标记，注入会在直通
  路径上静默失效（`filters.rs` 现已在 `SystemPrompt` 规则里标记 `system`；新增规则时别忘）；
- **D4** dispatch 无请求级超时（connect_timeout 之外挂起无限等）；
- **D5** `validate()` 漏挡 `n: 0`；
- **D6** 双发 `decode_stream_done`（server.rs 589 与 629 两个路径都调；现有 provider 靠
  `state.finished` 幂等兜底，属脆弱设计）；
- **D7** `reasoning_fields` 表在 Responses 的运行时读取上未真正用到（事件名匹配硬编码），
  协议表与 provider 行为有轻微脱节。

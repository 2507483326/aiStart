# 转发链路：整体流程、问题清单与方案

> 2026-09-25 重写（替代已删除的旧文档）。基于当前源码逐文件核对（`gateway/`、`providers/`、
> `filters.rs`、`settings.rs`、`usage.rs`、`commands/`），并按确认过的新流程重新组织：
> **一次请求只打一个上游，失败立即返回客户端；模型切换在请求结束后由后台连接测试驱动**。
> §2 是目标流程（含与现状代码的差距清单），§7 是核对出的问题，§8 是分批落地方案。
>
> 基线：`cargo test` 124 passed / 0 failed。同协议直通（请求、响应、流三侧）已全部落地。

---

## 1. 应用与网关的位置

aiStart 做两件事：

1. **接入客户端**：把五类客户端（Claude Desktop / DeepSeek Desktop / Codex / ZCode / WorkBuddy）
   的配置指向本地网关。`platform/` 下每个 `AppConfigurator` 负责探测安装、改写各自配置文件、
   记录绑定关系（`settings.applied`）与专属 token（`settings.app_tokens`）。
2. **转发**：网关（axum，默认端口 8931，仅绑定 127.0.0.1）收三种入站协议，转发到「当前模型」
   对应的上游；入站协议与上游协议一致时直通，不一致时经规范层转换。

入站端点（`gateway/server.rs::router`）：

| 端点 | 协议 |
| --- | --- |
| `POST /v1/messages` | Anthropic Messages |
| `POST /v1/chat/completions` | OpenAI Chat Completions |
| `POST /v1/responses` | OpenAI Responses |
| `GET /v1/models` | 固定四条 Claude 形状的路由（`MODEL_ROLES`，给 Claude Desktop 的模型选择器用） |
| `GET /health` | 状态面板数据 |

鉴权：`x-api-key` 或 `Authorization: Bearer`；token 与 `app_tokens` 匹配反查来源应用
（`source_app_for`），匹配不到就原样记录。token 固定可读（= app kind 字符串），见 §7-C3。

模块地图：

| 模块 | 职责 |
| --- | --- |
| `gateway/server.rs` | 请求生命周期、上游选择、直通判定、落库调用、事后切换触发 |
| `gateway/sse.rs` | SSE 解析（保留原文分帧）与编码 |
| `gateway/mod.rs` | 网关生命周期（启停/重启）、统计计数、`/v1/models` 的路由表 |
| `providers/wire.rs` | 标量协议表（规范字段 → 每协议落地方式） |
| `providers/mod.rs` | trait、`StreamState`/`WireState`/`ResponseAssembler`、共享 http 客户端（代理） |
| `providers/normalizer.rs` | `BlockNormalizer`（流式块状态机）与 `StreamVerdict`（判罚） |
| `providers/anthropic_messages.rs` / `openai_completions.rs` / `openai_responses.rs` | 三个协议的编解码 |
| `domain/canonical.rs` | `CanonicalRequest` / `RequestBody`（规范层） |
| `filters.rs` | 请求过滤器（当前只有系统提示词注入） |
| `settings.rs` | 模型列表、当前模型、代理、绑定，全部持久化到 SQLite |
| `usage.rs` | 明细落库（usage_detail + usage_payload + 每日汇总） |

## 2. 一次请求的生命周期（目标流程）

```
route(inbound)
 ├─ 1. 获取请求原文与 HTTP header（原样，不脱敏）
 ├─ 2. 解析原文，确定本次上游：
 │       请求模型名命中某个模型显示名 → 该模型（锁定，不参与切换）
 │       否则（别名 aiStart/auto、未指定、未命中）→ 当前模型
 │      没有可用模型 → 立即报错返回
 ├─ 3. 有需要注入的提示词 → 注入到上下文（规范层的 system 字段）
 ├─ 4. 判断入站协议与上游协议是否一致
 │
 ├─ 协议一致（直通）：
 │    ├─ 上游请求 = 注入后的客户端原文（只改模型名 / 被注入的 system / 规范内部字段）
 │    ├─ 加上网关构造的上游 header，发送请求
 │    ├─ 响应原样回给客户端，不解析不包装
 │
 ├─ 协议不一致（转换）：
 │    ├─ 上游请求 = 经规范层转换后的报文
 │    ├─ 加上网关构造的上游 header，发送请求
 │    ├─ 解析结果为规范响应，包装成入站协议的形状回给客户端
 │
 ├─ 请求完全结束后记录：原文 + header + 发往上游的报文 + 返回内容，一次写入数据库
 │   （流式 = 流彻底结束或客户端断开时；判罚/记账走旁路，见 §6）
 │
 └─ 事后切换：若开启了自动切换、本次用的是当前模型（非点名）、且按返回状态码判定失败
     → 后台异步对模型列表做连接测试（跳过刚失败的模型，同一时刻只跑一次探测）
     → 第一个通过的模型切换为当前模型（服务下一次请求）；全部不通过则原地不动
```

已确认的取舍（2026-09-25）：

| 决策点 | 取法 |
| --- | --- |
| 切换时机 | **响应之后后台异步做**：客户端立即拿到错误响应，探测与切换不拖慢本次请求 |
| 点名模型 | **保留锁定语义**：命中显示名就只打该模型，失败也不触发切换 |
| 入库时机 | **请求结束时一次写入**（原文 + header + 响应一条记录）；客户端中途断开由落库守卫兜底（§7-B3） |
| header 脱敏 | **原样存**：本地工具，库中本就有上游 Key 明文；排查问题最直接 |
| 触发切换的失败 | 5xx、401/403/404/408/429、网络错误/超时；4xx（换模型救不了）与流中途断流（已经开始 2xx 吐数）不触发，后者照旧记 `Truncated` |
| 连接测试 | 复用现有探测（最小 pong 请求，15s 超时），按模型列表顺序逐个试、跳过刚失败的；防抖：探测进行中时新触发合并且不重复跑 |
| 上游 header | 网关构造（鉴权、anthropic-version；Anthropic 上游透传客户端的 `anthropic-beta`）。客户端其余 header **不**转发——否则把网关 token 泄给上游 |
| 明细 `failover` 字段 | 语义改为「这次请求触发了切换探测」（写库时即可判定；切换结果由事件与统计呈现） |

与现状代码的差距（§8 批 1 要改的）：

| | 现状 | 目标 |
| --- | --- | --- |
| 上游选择 | `candidates_for` 展开候选列表，**循环**逐个试 | 只打一个上游：点名 → 该模型，否则当前模型 |
| 切换时机 | 请求中途：在别的候选上重发成功后接手本次请求，`active_model_id` 即刻移交 | 请求结束后：连接测试通过才切 `active_model_id`，服务的是下一次请求 |
| 切换依据 | 真实请求在另一候选上成功 | 探测请求通过 |
| 失败返回 | 客户端要等所有候选试完 | 第一个上游失败立即返回 |
| header 入库 | 只存请求体 | 请求体 + HTTP header 一并入库 |
| `decode_stream_done` 双发 | `[DONE]` 分支与流末各调一次（靠幂等兜底） | 收敛为只在流末调一次（批 3 顺带） |

注：注入在「解析」之后执行（流程图里画在 3），因为注入目标是规范层的 `system` 字段——
必须先解析才知道每个协议把 system 放哪（completions 的 `messages[0]` / Responses 的
`instructions` / Anthropic 的顶层 `system`）。

## 3. 规范层（domain/canonical.rs）

`CanonicalRequest` 持有四份状态：

| 字段 | 内容 | 谁依赖它 |
| --- | --- | --- |
| `raw` | 规范形状的请求 JSON（**不变式：= Anthropic Messages 形状**） | 协议表、过滤器（`map_raw`） |
| `body` | 类型化的 `RequestBody`（serde 解析自 raw，两者经 `map_raw` 永不脱节） | 各 provider 的 encode |
| `client_raw` | 客户端入站报文的**原文**（入站第一步另存；OpenAI 系 decode 会重建 raw，原文只在这里） | 同协议请求侧直通 |
| `dirty` | 被过滤器改写过的规范字段名集合 | 同协议直通决定覆盖哪些字段 |

`RequestBody` 的顶层字段（规范 = 三协议并集，标量参数用 OpenAI 命名）：
`model / max_tokens / system / messages / tools / tool_choice / temperature / top_p / stop_sequences /
stream / top_k / store / metadata / response_format / parallel_tool_calls / reasoning_effort /
service_tier / n`，外加规范内部层 `_canonical`（`include_usage`、`max_tokens_field`，出站前整层删除）。
`tool_choice` 与 `response_format` 是 `Option<Value>`（未类型化），这是 §7-A3 的根源。

`validate()`：`n > 1` 直接拒绝（网关只承载单候选）。

## 4. 协议能力表（providers/wire.rs）

标量字段「每协议怎么落」集中在三张静态表 `ProtocolProfile`：

- `Slot::Same` / `Renamed(新名)`：按表写入；
- `Slot::Transformed`：规范键名在该协议不存在，provider 把值变形后放到别处（如 `response_format`
  → Anthropic 的 `output_config.format`），规范名从报文删除；
- `Slot::Dropped`：协议没有对应语义，显式丢弃；
- `Slot::Custom`：结构字段（messages/tools/tool_choice…），留给 provider 手写拼装。

两种填充策略：`Overwrite`（重建型报文直接覆盖）、`IfAbsent`（Anthropic 透传报文只补缺失，
不动客户端原样字段，保住 `tools` 上的 `cache_control` 等扩展）。

守护测试 `protocol_profiles_cover_every_canonical_field` 用完整字面量构造 `RequestBody`——
新增规范字段不同步这张表会直接编译失败 + 断言失败，静默丢字段在规范字段层面结构上不可能。

## 5. 三个 Provider 与同协议直通

`ModelProvider` trait（`providers/mod.rs`）是唯一多态点，`provider_for(format)` 返回静态实例：

| 方法 | Anthropic Messages | Completions | Responses |
| --- | --- | --- | --- |
| `is_passthrough` | ✅ raw 即规范形状 | ❌ | ❌ |
| `decode_request` | 原样 + 抬升 2 个藏匿字段（`output_config.effort`、`disable_parallel_tool_use`） | 重建（拆 system、`tool` role → tool_result、content parts → 块） | 重建（`input` items → 消息、`instructions` → system） |
| `encode_request` | raw + `IfAbsent` 补写 + 4 个变形字段 | 全新拼装 + 标量表 | 全新拼装 + 标量表 |
| `encode_request_passthrough`（同协议快路） | 无需（默认 None） | ✅ 客户端原文为底 | ✅ 同左 |
| `decode_response` / `encode_response` | 恒等 | 规范 ↔ `chat.completion` | 规范 ↔ `response` 对象 |
| `decode_stream_event` | 事件转发 + usage/错误记账 | 分片 → 块状态机 | 事件 → 块状态机（`output_index` 区分并行工具） |
| `encode_stream_event` | 恒等 | 块 → `chat.completion.chunk` | 块 → `response.*` 事件 |

### 5.1 同协议直通（已落地）

一条谓词、三处消费（`gateway/server.rs::same_protocol`）：

```rust
pub(crate) fn same_protocol(inbound: ModelFormat, config: &ModelConfig) -> bool {
    config.format == inbound
}
```

1. **请求侧** `encode_upstream_request`：同协议走 `encode_request_passthrough`——以 `client_raw`
   为底，只改上游模型名、补默认输出上限（客户端没写时）、仅在 `is_dirty("system")` 时重写
   system 落点、删除 `_canonical`。客户端的扩展键（OpenRouter 的 `provider`/`models`/`route`、
   message 的 `name`、`logit_bias`、`stop` 原字段名……）原样带到上游。
2. **响应侧（非流式）** `encode_client_response`：同协议直接回上游原文，`decode_response`
   只用于记账。跨协议才把规范响应重建成客户端协议形状。
3. **流侧**：`SseFrame { event, data, raw }`（`gateway/sse.rs`）在解析层保留每一帧的原文，
   直通时 `yield frame.raw`——注释心跳、多行 `data:`、分帧格式全部保真；不合成
   `message_start`、不补 `[DONE]`/usage 尾片；解码失败/断流不再往客户端注入错误事件
   （上游原文已经发出去了，补上去等于伪造内容）。

语义是「**等价直连**」：客户端拿到的字节与直连上游一致。记账与判罚不受影响——
`StreamState`/`ResponseAssembler` 的推进与直通判定无关，两条路径跑同一份代码，明细照常落库。

**两条谓词，别合并**：`same_protocol` 决定「客户端出口是否直通」；`is_passthrough()` 决定
「跨协议时要不要替上游合成 `message_start`」（Anthropic 上游会自己发，两个 OpenAI 上游不会；
即 `emit_initial = !passthrough && !upstream_provider.is_passthrough()`）。原先的缺陷是缺了
前者，不是后者维度用错。

## 6. 流式管道（gateway/server.rs）

```
parse_sse_stream(upstream)                       // → SseFrame { event, data, raw }
  → first_frame_timeout(…, 300s)                 // 只限首帧；首帧之后不限时（B1）
  → 同协议：yield frame.raw（逐帧原文）
  → data == "[DONE]"? → 只记 upstream_ended（收尾统一到流末，B4）
  → decode_stream_event（上游 → 规范事件）
      → assembler.apply（落库侧累积，两条路径都跑）
      → encode_for_client（跨协议才编码出站；直通返回空）
  → 流末：decode_stream_done（只在这里调一次）→ wire_state 补 usage 终值
      → 跨协议才 encode_stream_done（usage 尾片 / [DONE] / response.completed）
  → verdict（Ok / EmptyReasoningOnly / Truncated / UpstreamError）→ record（统计 + 落库）
```

记账快照（`StreamAccounting`）全程收在 `Arc<Mutex<…>>` 里，生成器另持一个 `DisconnectGuard`：
客户端中途断开时 hyper 直接丢弃响应流、生成器尾部永远不会执行，守卫在 drop 时按快照补一条
`ok=false` / `error=「客户端断开」` 的记录；正常收尾先置位 `finished` 认领，守卫随即让位。
两种收尾共用同一份统计与落库代码，因此面板与明细的口径不会分叉。

- **出站编码侧**：`WireState` 按客户端协议重建结构（分片 id、工具索引换算、文本 item 开闭）。
- **解码侧**：`StreamState` 内嵌 `BlockNormalizer`，把任意上游增量收成合法规范块序列，
  不变式 I1–I5（单开块、载荷进匹配块、索引递增、并行工具缓冲串行化、工具必有块）。
  直通时它只喂记账，不参与出站。
- **判罚**（`StreamVerdict`，流结束后算，不干预流）：上游流内报错、只给思考没给正文、
  没发结束事件就断流，分别记 `UpstreamError` / `EmptyReasoningOnly` / `Truncated`。
  断流不触发事后切换（状态码已是 2xx，换模型救不了这条已经断掉的流）。
- **落库**：`ResponseAssembler` 把规范事件拼回完整消息 → `encode_response` 转成上游协议
  原生形状 → `usage_payload`（原文 + header + 发往上游的报文 + 上游响应，单条 256KB 截断）。
- usage 口径：OpenAI 系 `prompt_tokens` 含缓存命中，规范按 Anthropic 语义拆成
  「未命中输入 + 缓存读」；思考 token 单独记。回写给 OpenAI 客户端时再并回去。

## 7. 问题清单（重新核对过源码）

按影响分组。A 组是跨协议转发的保真缺口（只影响**跨协议**请求）；B 组是健壮性；
C 组是一致性/小项。

### A. 跨协议保真缺口

**A1｜Completions 入站跨协议丢 `tool_choice`**（高）
`openai_completions.rs::decode_request` 的抬升键列表（`temperature…n`）里没有 `tool_choice`
——`tools` 抬了，`tool_choice` 没抬。Completions 客户端强制/禁用工具调用的意图在切到
Anthropic / Responses 上游时静默丢失；Anthropic 出站的 `tool_choice_is_openai_shaped` 还会把
残留键删掉。同协议不受影响（直通以客户端原文为底）。

**A2｜Anthropic 入站跨协议丢结构化输出与 thinking 配置**（高）
`anthropic_messages.rs::decode_request` 只抬升 `output_config.effort` 与
`disable_parallel_tool_use`。客户端的 `output_config.format`（json_schema 结构化输出）与
`thinking`（预算式思考开关）不抬升 → 切到任一 OpenAI 上游时静默丢失。前者可以直接映射
（`response_format`），后者是预算值 ↔ 档位的有损映射，至少该留痕而不是无声丢。

**A3｜`tool_choice` / `response_format` 的规范形状未类型化**（高，A1/A2 的根源）
两个字段在 `RequestBody` 里是 `Option<Value>`，「规范形状」事实上取决于入站协议：
Anthropic 入站存 `{type:"any"/"tool",…}`，Completions 入站存 OpenAI 形（或根本不存，见 A1），
Responses 入站存摊平形。三个 encode 侧各自用 JSON 取值链猜测形状，漏一个臂就静默降级：
`{type:"any"}` → Responses 出站直接丢弃（`openai_responses.rs::tool_choice` 的 `_ => None`）；
`stop`、`frequency_penalty`/`presence_penalty`/`logit_bias`/`seed` 等未建模参数跨协议一律无声丢
（部分是协议本身没有对应语义，丢失合理；部分是规范层没建模，连「丢弃」都不是显式的）。

**A4｜图片跨协议仅支持 `data:` URL，http(s) 图静默丢弃**（中）
`data_url_to_source` 只认 `data:...;base64,...`（`openai_completions.rs`）。Completions /
Responses 入站的 `image_url` 是 http(s) 地址时，块被静默跳过——客户端的图在跨协议转发时消失，
连一个警告都没有。规范层的 image source 本就支持 `type:"url"`，decode 侧没接。

**A5｜`service_tier` 对 Anthropic 按同名透传**（中，需实测验证）
`wire.rs` 的 `ANTHROPIC_RULES` 把 `service_tier` 标为 `Same`。Anthropic Messages 请求没有这个
参数；Completions 客户端带了它再切到 Anthropic 上游，疑似被上游 400 拒掉。需实测确认后改 `Dropped`。

**A6｜`n: 0` 未被拒绝**（低）
`validate()` 只挡 `n > 1`；`n: 0` 语义非法，会原样转发给上游，报错形态不可控。

**A7｜跨协议流内上游错误事件的形状不统一**（低）
上游在流里报错时（completions 的 `{"error":…}` 分片、Anthropic 的 `error` 事件），解码侧
产出的规范错误事件经 `encode_stream_event` 的 `"error" => vec![canonical.clone()]` 原样转发
——那是 Anthropic 形状。Responses 客户端收到的是 `{type:"error",error:{…}}` 而非自己协议的
错误事件。而网关**自产**的错误（`error_events`）是按入站协议出形状的，两个来源不一致。

### B. 健壮性

**B1｜转发请求无超时，可无限挂起**（高）
共享 http 客户端只设了 `connect_timeout(20s)`（`providers/mod.rs::build_client`）；
`dispatch` 与非流式的 `upstream.json()` 都没有请求级超时。上游建连后不响应、或流式上游
中途停止吐数，请求会永久挂起：客户端等着、明细永远不会落库。探测与翻译路径都有硬超时
（15s / 120s），唯独主转发链路没有。

**B2｜SSE 解码按 chunk 做 UTF-8 lossy，跨 chunk 的多字节字符会损坏**（高）
`gateway/sse.rs::parse_sse_stream` 对每个网络 chunk 单独 `String::from_utf8_lossy`。一个
CJK 字符（3 字节）恰好跨在两个 chunk 边界上时，会变成两个 U+FFFD 替换符——流式输出中文时
概率不低，且直通后这些损坏字节会原样到达客户端。修法：按字节缓冲（`BytesMut`），凑齐完整行
再解码。

**B3｜客户端断开连接时，流式请求完全不落库**（中高）
`record_usage` 在流式生成器（`async_stream::stream!`）的尾部。客户端中途断开（Esc、超时、
崩溃）时 hyper 直接 drop 这个流，生成器状态机随之销毁——尾部的 `record_usage`、
`stats.record_tokens`、判罚全部不执行。长生成被取消是常态，用量统计因此系统性少记。
目标流程要求「请求完全结束后记录」，这条必须修：记账快照收进共享状态 + `Drop` 守卫，
drop 时也能落一条「客户端取消」的记录。

**B4｜`decode_stream_done` 双发，靠幂等兜底**（低，设计债）
`[DONE]` 分支与流末尾各调一次，正确性靠 `state.finished` 幂等保证。语义上它只该在流末
调一次（`[DONE]` 分支只负责记 `upstream_ended` 与 usage），现在的形状容易在后续改动里踩坑。

### C. 一致性 / 小项

**C1｜Completions 解码不捕获分片 `id`，明细里的流式响应 id 是合成的**（低）
`openai_completions.rs::decode_stream_event` 从不读分片的 `id` 字段（Responses 的
`response.created`、Anthropic 的 `message_start` 都捕获了）。于是 completions 流落库的
`upstream_response.id` 是网关生成的 `msg_<uuid>` 而非上游的 `chatcmpl-…`。只影响明细展示。

**C2｜`reasoning_fields` 协议表只有 Completions 真正在读**（低）
读侧消费点只有 `openai_completions.rs::reasoning_text`；Anthropic 臂（`["thinking"]`）无人读，
Responses 的解码按事件名硬编码，不查表。协议表与 provider 行为轻微脱节——要么让 decode 真的
走表，要么把表收窄成 completions 专属并注明。

**C3｜安全注意：固定 token + 全开放 CORS**（提示，非缺陷）
应用 token 是固定可读字符串（= app kind），`CorsLayer::permissive()`。任何本机网页/进程都能
用已知 token 调网关白嫖上游。单机工具场景可接受；若要收紧，优先校验 `Origin`/`Host` 或换
随机 token，不必动转发逻辑。**结论：决定不做**（2026-09-25 与用户确认），理由与两条备选路径
见 §8 批 4。

## 8. 方案（分批落地）

顺序按「流程收敛 → 保真 → 健壮性 → 杂项」。每批独立可回滚、独立可测。

### 批 1：流程收敛——单上游 + 事后切换 + header 入库（§2 差距表的全部）✅ 已完成（更早的提交落地）

**1a. 上游选择收敛**：`candidates_for` 改为 `resolve_target(requested) -> Option<ModelConfig>`
——点名命中显示名 → 该模型；否则 → 当前模型；无启用模型 → None（调用方报错）。
删除候选循环、`failover_used`、循环内的 `active_model_id` 移交与 `record_failover`。
第一个上游失败即构造 `RouteFailure` 返回客户端。

**1b. header 入库**：`UsagePayload` 加 `inbound_headers: Option<String>`（JSON 序列化的
header map），`usage_payload` 表加列（`schema.sql` + `db/mod.rs` 的 LEGACY_DDL 补列，
老库自动补）。`route()`/`handle()` 把 `HeaderMap` 序列化后传入；成功与失败两条落库路径都带上。
256KB 截断沿用 `cap_bytes`。

**1c. 事后切换**（`gateway/server.rs` 或独立 `gateway/failover.rs`）：

```text
触发条件（写库时判定，写入 failover 字段）:
    auto_failover 开 && 本次未点名模型 && 失败
    && (状态码 ∈ {5xx,401,403,404,408,429} || 网络错误/超时)
执行（tokio::spawn 后台）:
    防抖哨兵：探测已在跑 → 直接返回（本次触发合并进正在跑的那轮）
    for model in 模型列表（跳过刚失败的）:
        probe_completion(model)   // 复用 commands/models.rs 的探测，15s 超时
        通过 → 换 active_model_id 前复查当前值仍是失败的那个（避免覆盖用户手切）
              → settings::mutate + events::log("model.failover") + stats.record_failover
              → 释放哨兵，结束
    全部不通过 → events::log 记「切换失败：全部模型探测未通过」→ 释放哨兵
```

**1d. 测试**：`resolve_target` 的点名/别名/未启用行为；触发条件判定表；探测顺序与跳过逻辑；
「切换前复查」防覆盖；`failover` 字段语义。探测本身已有代码，重点在触发与守卫。

涉及文件：`gateway/server.rs`、`gateway/mod.rs`、`settings.rs`、`usage.rs`、`db/schema.sql`、
`db/mod.rs`、`tests.rs`。规模 ~1.5 天。回滚：整体 revert；`resolve_target` 与循环版行为
对点名/别名请求完全一致，风险集中在切换路径，出问题可临时把触发条件改为恒 false。

### 批 2：跨协议保真（修 A1–A4、A6）✅ 已完成

核心一步是**把形状分歧字段类型化**，其余缺口大多随之消失：

1. 新增 `CanonicalToolChoice`（`Auto | None | Required | Tool{name}`）与
   `CanonicalResponseFormat`（`Text | JsonObject | JsonSchema{name,description,schema,strict}`）枚举，
   `RequestBody` 的对应字段换成强类型；serde 反序列化天然拒绝非法形状（入站即 400，
   而不是转发到上游才炸）。规范 JSON 统一为 OpenAI Chat Completions 口径，
   各协议 decode 侧先归一再 parse（Anthropic 的 decode 由「parse 后 map_raw」改成
   「先 normalize 再 parse」，类型化字段才来得及校验形状）。
2. 每协议 `to_canonical / from_canonical` 纯函数（tool_choice、response_format、image source），
   `decode_request` 在抬升时调用——A1（completions 补抬 `tool_choice`）与 A2（Anthropic 补抬
   `output_config.format`）随之修掉。形状抬不了的臂显式 400，不再静默降级/丢弃。
3. 图片（A4）：decode 侧 `image_source_from_url` 把 http(s) `image_url` 存成
   `{type:"url",url}` 的 image source，`data:` 仍收 base64；encode 两侧本就支持 url 形态。
4. `validate()` 补 `n == 0` 拒绝；显式 `null` 的 tool_choice / response_format 视为未指定。

落地时的设计决定（与原计划的差异）：

* **`thinking` 有损映射口径**：预算 → 档位按 `≥32k → high`、`≥8k → medium`、其余 `low` 粗分；
  `output_config.effort` 显式档位优先于预算。映射只做「抬升」，不删 `thinking` /
  `output_config` 原键（同协议直通时客户端原文仍是底稿）；Anthropic 出站时报文里已有
  `thinking.budget_tokens` 就不再叠加 `output_config.effort`（两个思考开关同发会被上游拒）。
* Anthropic 的 `output_config.format` 只有 type+schema、没有 name，跨协议到 OpenAI 系时补
  缺省名 `"response"`。
* 测试：`canonical_fields_are_forwarded_per_protocol` 补 tool_choice / strict / 指定工具样例；
  新增 `anthropic_inbound_hidden_fields_survive_cross_protocol`（A2/A3，含同协议 thinking
  原样保留）、`http_image_urls_survive_cross_protocol`（A4）、
  `invalid_n_and_malformed_shapes_are_rejected_at_inbound`（A6 + 非法形状入站 400）。
  130 通过。

涉及文件：`domain/canonical.rs`、三个 provider、`tests.rs`。
回滚：纯增量改动，revert 即可。

### 批 3：健壮性（修 B1–B3，顺带 B4）✅ 已完成

1. **超时**：非流式在 `dispatch` 的请求 builder 上加 `.timeout(300s)`；流式对「建连 + 首帧」
   用 `tokio::time::timeout` 包裹首字节等待，流中不设总超时（长生成合法）。
2. **SSE 字节缓冲**：`parse_sse_stream` 改 `BytesMut` 累积，找到 `\n` 才 `from_utf8`
   解码整行；不完整尾字节留在缓冲等下一个 chunk。现有测试
   `sse_frames_keep_the_upstream_bytes_intact` 扩展一个「多字节字符跨 chunk」用例。
3. **断连落库**（目标流程「请求完全结束后记录」的前提）：把记账快照收进 `Arc<Mutex<…>>`
   （或 channel + spawn），生成器持一个 `Drop` 守卫，drop 时也能落一条记录，
   `error` 写「客户端断开」；正常结束时守卫让位给常规落库，不双记。
4. （顺带）`[DONE]` 分支收敛为只记 `upstream_ended` + usage，`decode_stream_done` 只在流末
   调用一次。

落地时的设计决定（与原计划的差异）：

* **两程限时，而不是一个总超时**：流式链路其实有两段「等」，各有各的堵法——
  第一段是 `builder.send()` 等响应头（上游受理但排队），第二段是响应体等首帧
  （回了 200 却不吐字节）。两段各套一次 `UPSTREAM_TIMEOUT`（300s），首帧之后的流**不限时**。
  第二段做成 `sse.rs::first_frame_timeout` 流适配器：超时以 `Err` 项进流，而不是直接中断，
  这样它和上游断流走完全同一条路径（记账 + 跨协议时报错给客户端），不新增分支。
* **超时算 `retryable`**：超时与网络错误同类（换一个模型可能就通了），因此照旧触发事后切换，
  写库时 `failover` 字段照常置位。
* **断连兜底用「生成器局部守卫」而非 channel + spawn**：`async_stream::stream!` 里的局部变量
  恰好在 hyper drop 掉响应体（客户端断开）时随之销毁，`Drop` 时机精确；且局部变量按声明逆序
  销毁——后声明的 `MutexGuard` 先释放，守卫才去加锁，不会自锁。
* **`finished` 是单向认领位**：`record()` 与守卫都先置位再写库，两者只会有一个真正落库。
  守卫加锁走 `unwrap_or_else(|poisoned| poisoned.into_inner())`——生成器持锁时 panic 也要能补记，
  快照本身仍是可读数据。**锁一律收在单个语句或单个块内**：`MutexGuard` 跨 `yield` 会让生成器
  不再是 `Send`。
* **兜底也要计统计**：断连记录同样走 `stats.record_tokens` / `record_error`，否则面板与明细两个口径
  会分叉。
* **B4 的可见影响**：`[DONE]` 不再就地收尾，而由流末统一产出终态事件。跨协议流的出站顺序不变
  （`decode_stream_done` 的规范事件仍在前、`encode_stream_done` 的协议尾片在后，只是两批都发生在
  循环退出之后）；直通流的 `[DONE]` 原文仍在解析循环里逐帧发出，不受影响。
* 测试：`first_frame_timeout_gives_up_when_the_upstream_never_speaks`、
  `first_frame_timeout_stops_policing_after_the_first_frame`、
  `sse_frames_decode_whole_lines_so_cjk_survives_chunk_boundaries`（B2），以及
  `gateway/server.rs` 内的 `disconnect_guard_records_what_the_stream_had_already_accounted_for` /
  `disconnect_guard_stays_silent_after_the_regular_hand_off`（守卫的 `StreamWriter` 做成可注入的
  `fn` 指针，单测注入捕获实现，不碰进程级 usage 库——`sqlite_persistence_round_trips` 断言的是
  记录条数）。135 通过。

涉及文件：`gateway/server.rs`、`gateway/sse.rs`、`tests.rs`。规模 ~1 天。
回滚：三项彼此独立，可单项 revert。

### 批 4：杂项与可选（A5 / A7 / C1 / C2 ✅ 已完成；C3 决定不做）

**A5 `service_tier`（Anthropic 侧改 `Dropped`）**

原计划是「实测 Anthropic 对未知参数的行为，报 400 就改 `Dropped`」。此处没做成实测（容器内无外网；
也不该拿用户的 Key 去试探上游），但**不需要实测就能判定 `Same` 是错的**：`service_tier` 的取值是
各家的词表——OpenAI 的 `flex`/`priority`/`scale`/`default` 与 Anthropic 的 `auto`/`standard_only`
只有 `auto` 重合。即便该参数在 Anthropic 侧存在，把入站的 `priority` 原样转发也会因非法枚举 400，
而 400 会废掉整个请求。丢弃则在两种假设下都安全：最坏是丢一个容量/优先级偏好，而 Anthropic
的默认档正是 `auto`——丢 `auto` 等于没丢。

代价（写在这里免得日后当 bug 查）：`Dropped` 在 Anthropic **本协议的编码路径上同样生效**，
所以 Anthropic 客户端自己带的 `service_tier` 也会被删掉——这条路与跨协议重建共用同一个
`encode_request`，不像两个 OpenAI 协议那样有独立的 `encode_request_passthrough`（它们以
客户端原文为底，从不查这张表，因此完全不受影响）。丢一个容量偏好换「绝不 400」，是这个改动
认下的取舍。

> 若日后实测确认 Anthropic 接受 `service_tier`，升级路径是把它从 `Dropped` 改成值映射
> （`auto` → `auto`，其余丢弃），而不是改回 `Same`；那样连 Anthropic 客户端自带的值也能保住。

**A7 错误事件形状**（`wire.rs` + 两个 OpenAI provider + `gateway/server.rs`）

形状定义收成一处：`wire::stream_error_event(format, kind, message)`（配 `stream_error_parts`
从规范错误事件里取 kind/message）。网关自产错误（`error_events`）与两个 OpenAI provider 的
`encode_stream_event` 的 `error` 臂都从它出——原先后者是把 Anthropic 形状的规范事件原样转发给
OpenAI 客户端，两侧形状不一致。Anthropic 臂保持逐字转发（默认实现即恒等：上游的 `error.type`
（如 `overloaded_error`）与附加字段因此保真，所以这一路等价于表里的形状）。测试
`stream_errors_have_one_shape_per_inbound_protocol` 对三个协议分别断言两个来源**逐字段相等**。

**C1 分片 id**：completions 解码捕获分片 `id`（空串不覆盖）填进 `StreamState.message_id`。
影响面只有落库的 `upstream_response.id` 与跨协议时发给客户端的 `message_start.message.id`——
同协议直通发的是上游原文，本来就不受影响。

**C2 `reasoning_fields`**：从 `ProtocolProfile` 移除，收窄成 `openai_completions.rs` 的
`REASONING_FIELDS` 常量（全链路只有它读）。Anthropic 的思考是内容块类型（`thinking`）、
Responses 的思考按事件名区分（`response.reasoning_summary_text.delta`），都不存在
「按 JSON 字段名找思考」这件事——表里那两条是死数据，还会让人以为它们在约束三个协议。
行为不变。

测试：`stream_errors_have_one_shape_per_inbound_protocol`（A7）、
`completions_stream_captures_the_upstream_chunk_id`（C1），
`anthropic_clients_lose_their_own_service_tier_until_the_parameter_is_verified`（A5 的代价，见上），
`canonical_fields_are_forwarded_per_protocol` 补两处断言（A5：Anthropic 丢弃 vs. 两个 OpenAI 协议保留）。

**顺手修的测试隔离问题**：`sqlite_persistence_round_trips` 与
`loopback_targets_bypass_the_proxy_and_requests_say_so` 都会 init/mutate 全局设置库，
并发跑时 `init` 的「读盘 → 覆盖内存 → 落盘」会把对方刚落盘的设置抹掉——实测表现为
`applied` 绑定凭空消失、`sqlite_persistence_round_trips` 偶发失败（首次遇到时本批还在改
`service_tier`，一度被误判为改动引起；单独跑必过、连跑偶发）。加了一把测试内的互斥锁串行化
这两者，连跑 5 次稳定。生产侧 `settings::init` 只在启动时调用一次（`lib.rs`），不存在这个交错，
所以这是纯测试隔离问题，未动生产代码。

**C3 决定不做**（2026-09-25 与用户确认）：`CorsLayer::permissive()` + 固定可读 token（= app kind）
在单机工具场景下可接受（§7-C3）——网关只监听 `127.0.0.1`，风险限于「本机的网页/进程能不能白嫖
上游」，而当前用法里客户端都是本机 app，收紧的收益不抵改动风险。两条备选路径留档，日后要做时
按此评估，不必重新推导：

* **`Origin` 白名单**：改动最小，但会影响网页/WebView 形态的客户端（本 app 自己的 WebView 若直接
  调网关端口也要进白名单），且对非浏览器来源没有任何作用。
* **随机 token**：真正能挡住网页 drive-by 的手段，但会让所有已绑定客户端失效、需要重新绑定。
  做成「设置项 + 默认关」可以不破坏现有绑定，代价是需要同时动 Rust 设置、DB 列与
  `SettingsDialog.vue` / `types.ts`。

138 通过。

### 不做的（明确排除）

- **不引入协议描述 DSL**：三个协议是有限集，流式块状态机无法声明式化，强行 DSL 同时失去
  类型检查与可读性。
- **不动 `BlockNormalizer`**：全链路正确性核心，不变式已被测试锁死。
- **不做候选列表内的协议亲和排序**：候选循环删除后，「候选排序」这个概念不复存在；
  同协议直通已经保证切换到同协议模型时自动走保真路径，无需额外偏好。
- **不重建直通开关**：同协议直通按 `same_protocol` 硬边界生效，无配置项；回滚手段就是让
  谓词返回 `false`（一行）。

## 9. 守护测试现状（138 passed）

关键守护点与对应测试：

| 守护点 | 测试 |
| --- | --- |
| 标量协议表完备性 | `protocol_profiles_cover_every_canonical_field` |
| 同协议请求保真 / 跨协议重建 | `same_protocol_passthrough_keeps_the_client_payload_intact`、`gateway_keeps_same_protocol_payloads_and_rebuilds_across_protocols`、`responses_passthrough_keeps_client_extensions` |
| 直通只在过滤器改写时覆盖 system | `passthrough_rewrites_system_only_after_a_filter_changed_it` |
| 直通判定恰好等价协议相等 | `same_protocol_holds_exactly_when_the_formats_match` |
| 响应逐字转发 | `same_protocol_response_is_forwarded_verbatim` |
| SSE 字节保真（心跳/多行 data/分帧） | `sse_frames_keep_the_upstream_bytes_intact` |
| SSE 跨 chunk 的多字节字符 | `sse_frames_decode_whole_lines_so_cjk_survives_chunk_boundaries` |
| 首帧限时不掐长生成 | `first_frame_timeout_gives_up_when_the_upstream_never_speaks`、`first_frame_timeout_stops_policing_after_the_first_frame` |
| 断连兜底落库且不双记 | `gateway::server::tests::disconnect_guard_records_what_the_stream_had_already_accounted_for`、`disconnect_guard_stays_silent_after_the_regular_hand_off` |
| 流内错误事件形状（两个来源一致） | `gateway::server::tests::stream_errors_have_one_shape_per_inbound_protocol` |
| 分片 id 进记账 | `completions_stream_captures_the_upstream_chunk_id` |
| `service_tier` 对 Anthropic 一律丢弃（含客户端自带值） | `anthropic_clients_lose_their_own_service_tier_until_the_parameter_is_verified` |
| 块状态机不变式 | `block_normalizer_keeps_payloads_in_their_own_block`、`block_normalizer_serializes_parallel_tool_arguments` |
| 流判罚 | `stream_verdict_flags_reasoning_only_and_truncated` |
| 过滤器 | `filter_injects_system_prompt`、`filter_skips_disabled_and_stacks_in_order` |

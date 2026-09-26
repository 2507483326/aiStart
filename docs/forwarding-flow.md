# 转发链路全流程（重新梳理版）

> **本文定位**：把「一次请求从进入网关到返回客户端、再到落库 / 事后切换」这条链路，从头到尾按
> **当前源码**讲清楚，然后给出问题清单与修复建议。目标是**清晰、可复现**：每一节都能照着代码走一遍，
> 每条问题都带位置与证据，不含糊。
>
> **与另两份文档的关系**（不重复、不替代）：
> - `docs/forwarding.md` 是**决策史**：记录了 A/B/C 三组问题与批 1–5 的落地过程、每次取舍的理由。
> - `docs/transport-review.md` 是**传输层专项**：超时 / 缓冲 / 重定向 / 落库的 H1–H3、M1–M5、L1–L7。
> - **本文**是把上面两份的结论按「一条请求的流水线」重新排一遍，并补上复核源码时新发现的问题
>   （第 8 节带 🆕 标记）。
>
> **基线**：`HEAD = 73f8b3b`（工作区含未提交改动：H2 / L1 / L3 已落地、单实例插件已加、文档已同步）。
> `cargo test` = **149 passed / 1 ignored**（实测，见 §9）。A5（落库解耦）已于本轮落地，见 §5 与 §8-A5。
> 依赖版本以 `Cargo.lock` 为准：reqwest 0.13.5、axum 0.8.9、tower-http 0.7.1、rusqlite 0.32。

---

## 1. 一句话概括与模块地图

**一句话**：aiStart 在本机 `127.0.0.1` 上开一个网关，收三种入站协议（Anthropic Messages /
OpenAI Chat Completions / OpenAI Responses），按请求里的模型名选定**唯一一个**上游，把报文
**同协议直通**或**经规范层转换**后转发；响应同样处理回客户端；请求彻底结束后落一条明细，
若本次是按当前模型走的失败请求，再在后台探测连接、把当前模型切到第一个通的模型（服务下次请求）。

```
客户端(协议 A) ─▶ 本地网关 127.0.0.1:<port> ─▶ 唯一上游(协议 B)
   ▲                                            │
   └──────────── 响应(协议 A) ◀─────────────────┘
```

模块地图（谁负责什么）：

| 模块 | 职责 | 关键文件 |
| --- | --- | --- |
| 入站路由 / 请求生命周期 | 鉴权、体积自检、解析、过滤、选上游、直通判定、落库、触发事后切换 | `gateway/server.rs` |
| 网关生命周期 | 启停/重启、端口绑定、统计计数器、`/health`、`/v1/models` | `gateway/mod.rs` |
| SSE 字节层 | 上游分帧解析（保留原文）、首帧限时、出站编码 | `gateway/sse.rs` |
| 事后切换 | 后台连接探测 → 切当前模型（纯函数 + 哨兵） | `gateway/failover.rs` |
| 规范层 | `CanonicalRequest` / `RequestBody`（= 三协议并集，Anthropic 形状为基） | `domain/canonical.rs` |
| 协议能力表 | 「规范字段 → 每协议怎么落」的静态表 + 错误事件形状 + stop_reason 映射 | `providers/wire.rs` |
| Provider trait + 流状态机 | 唯一多态点；`StreamState` / `BlockNormalizer` / `ResponseAssembler`；共享 http 客户端 | `providers/mod.rs`、`providers/normalizer.rs` |
| 三个协议实现 | 请求/响应/流的双向编解码 | `providers/{anthropic_messages,openai_completions,openai_responses}.rs` |
| 请求过滤器 | 转发前改写规范请求（当前只有注入 system 提示词） | `filters.rs` |
| 设置 | 模型列表、当前模型、代理、绑定；`resolve_target` 在这里 | `settings.rs` |
| 用量 | 明细 + 报文 + 每日汇总（SQLite） | `usage.rs`、`db/mod.rs` |

---

## 2. 一次请求的完整生命周期（端到端）

下面每一段都对应 `gateway/server.rs` 的具体代码，按执行顺序排列。

### 2.1 入站、计数、鉴权、体积自检

```
router()  (server.rs:44)
  ├─ 路由: POST /v1/messages | /v1/chat/completions | /v1/responses → 三者都进 route()
  ├─ layer(DefaultBodyLimit::max(64MB))   // axum 硬上限（先注册 = 内层）
  └─ layer(CorsLayer::permissive())       // 全开放 CORS（后注册 = 外层；已接受，见 §8-B）

route(inbound)  (server.rs:322)
  ├─ stats.requests += 1
  ├─ inbound_request = 入站报文前缀（最多 256KB+1 字节，落库用；见 server.rs:317）
  ├─ token = extract_token(headers)   // x-api-key 优先，否则 Authorization
  ├─ inbound_headers = 原样序列化所有 header（不脱敏）
  └─ handle(...)

handle()  (server.rs:777)
  ├─ ① Key 非空校验；否则 401 unauthorized（RouteFailure → 失败落库）
  ├─ ② check_inbound_body(body.len())  // >32MB → PayloadTooLarge → 413 request_too_large
  ├─ ③ raw = serde_json::from_slice(body)   // 非法 JSON → 400
  ├─ ④ request = inbound_provider.decode_request(raw)
  │        .retain_client_raw(raw)          // 客户端原文另存（同协议直通要用）
  ├─ ⑤ request.validate()                   // n==0 / n>1 → 400
  ├─ ⑥ request = filters::apply(...)        // 注入 system 提示词，跑一次
  └─ ⑦ settings.resolve_target(request.model) → 唯一上游；无启用模型 → 404
```

关键点：

- **鉴权**（`server.rs:80-91`）：只检查 Key 非空。token 拿去 `source_app_for` 反查来源应用
  （命中 `app_tokens` 返回 app kind，否则原样记录）。token 本身是**固定可读字符串**（= app kind），
  见 §8-B。
- **体积自检**（`server.rs:766-775`）：两层上限。32MB 是逻辑上限，超了按入站协议形状回
  `413 request_too_large` **并走统一失败落库**；64MB 是 axum 硬上限（`DefaultBodyLimit`），再往上
  由 axum 在 handler 之前用纯文本 413 挡掉（不落库）。硬上限是**内层**，它的 413 会经过外层 CORS，
  因此也带 CORS 头。
- **解析**（各 provider 的 `decode_request`）：Anthropic 是先 `normalize_anthropic_request`
  把藏匿字段（`tool_choice`、`output_config.format/effort`、`thinking`）抬到规范字段再 parse；
  两个 OpenAI 协议是**重建**规范请求（拆 system、`tool` role → `tool_result`、content parts → 块）。
- **`client_raw` 的意义**：OpenAI 系的 decode 会重建报文，客户端原文只留在这里；**同协议直通**
  时以它为底，客户端自己的字段名与扩展键（OpenRouter 的 `provider`/`route`、`logit_bias`、
  message 的 `name`……）才能原样带到上游。
- **过滤器**（`filters.rs`）：作用在**规范请求（Anthropic 形态）**上，所以三种入站协议统一生效。
  改过 `system` 会 `mark_dirty("system")`——同协议直通时只覆盖「脏字段」，否则注入会被客户端
  原文盖回去。

### 2.2 上游选择：一次只打一个

`settings.resolve_target()`（`settings.rs:104-120`）是**唯一的**上游选择逻辑：

| 请求里的 `model` | 结果 | 失败是否触发切换 |
| --- | --- | --- |
| 命中某模型的**显示名**（不区分大小写） | `Named(该模型)` | **否**（点名 = 锁定） |
| `aiStart` / `auto`（不区分大小写）、空、未命中 | `Active(当前模型)` | 是（若开自动切换） |
| 没有任何启用模型 | `None` | 调用方直接 404「网关没有启用中的模型」 |

> 没有候选列表、没有循环重试。**第一个上游失败就立刻返回客户端**（`server.rs:838-851`）。

### 2.3 出站请求构造

```
dispatch()  (server.rs:526)
  ├─ payload = encode_upstream_request(inbound, config, request)   // 见 §3
  ├─ builder = http_client().post(endpoint).json(payload)
  ├─ headers = provider.headers(config)   // 网关自己构造，客户端其余 header 不转发
  │     · Anthropic: x-api-key + anthropic-version(2023-06-01) + (非 api.anthropic.com 时) Bearer
  │     · 两个 OpenAI: Authorization: Bearer
  ├─ 仅当上游是 Anthropic 时：客户端带的 anthropic-beta 透传（其余客户端 header 一律不转发）
  ├─ 非流式：builder.timeout(300s)          // 覆盖建连→响应体读完
  │  流式：不设请求级超时，只对 builder.send()（等响应头）套 300s
  └─ 非 2xx → 读响应体（截断 400 字进文案），retryable = 5xx|401|403|404|408|429
```

- **超时口径**（`UPSTREAM_TIMEOUT = 300s`，`server.rs:30`）：非流式一个总超时；流式两程各 300s
  ——「等响应头」（`dispatch`）与「等首帧」（`first_frame_timeout`）。**首帧之后不再限时**（§4.2）。
- **header 策略**：上游 header 由网关构造。客户端其余 header **不转发**——否则会把网关 token
  泄给上游。这是刻意取舍。

### 2.4 选上游时的协议判定（两条谓词，别混）

| 谓词 | 定义 | 决定什么 |
| --- | --- | --- |
| `same_protocol(inbound, config)` | `config.format == inbound` | 客户端**出口**是否直通（请求侧 + 响应侧 + 流侧三处） |
| `provider.is_passthrough()` | Anthropic = true | 跨协议时，是否要替上游合成 `message_start`（Anthropic 上游会自己发，两个 OpenAI 上游不会） |

即 `emit_initial = !passthrough && !upstream_provider.is_passthrough()`（`server.rs:926`）。
两者不能合并：前者管「客户端拿的是不是上游原字节」，后者管「要不要补起始事件」。

---

## 3. 直通 vs 跨协议：请求侧怎么选报文

`encode_upstream_request()`（`server.rs:484-496`）：

```
if same_protocol(inbound, config):
    if let Some(payload) = provider.encode_request_passthrough(config, request):  // 免转换快路
        return payload
return provider.encode_request(config, request)   // 回退到规范层重建
```

- **同协议快路**（`encode_request_passthrough`）：只有两个 OpenAI provider 实现（返回 `Some`）；
  以 `client_raw` 为底，只改三处——上游模型名、被过滤器改过的字段（system / instructions）、
  删除 `_canonical` 内部层；客户端两个输出上限字段都没给时补 `DEFAULT_MAX_TOKENS`。
- **Anthropic provider** 不实现快路（返回 `None`），走 `encode_request`：`raw` 为底 +
  `Fill::IfAbsent` 补字段（保住 `tools` 上的 `cache_control` 等扩展）+ 按 Anthropic 写法补
  `metadata`/`tool_choice`/`output_config`——结果**等价于直通**。
- **跨协议**：`encode_request` 全新拼装，标量字段由 `wire::apply_common_fields` 按协议表落地
  （`Same`/`Renamed` 写入、`Transformed`/`Dropped` 删除、`Custom` 留给 provider 手写）。

**协议表完备性有守护测试**：`protocol_profiles_cover_every_canonical_field` 用完整字面量构造
`RequestBody`——往规范层加字段忘了同步表，会直接编译 + 断言失败，静默丢字段在规范字段层面
结构上不可能。

---

## 4. 上游响应处理

### 4.1 非流式（`server.rs:860-914`）

```
raw_response = upstream.json()                     // 无大小上限（见 §8-A6）
canonical    = upstream_provider.decode_response(config, raw)
从 canonical 抽 input/output/cache/reasoning tokens
stats.record_tokens(...)
record_usage(...)                                  // 一次写入明细 + 报文
wire = encode_client_response(inbound, config, raw, canonical)
     · 同协议 → 上游原文（decode 只用于记账）
     · 跨协议 → provider_for(inbound).encode_response(canonical)
return 200 + wire
```

### 4.2 流式管道（`server.rs:916-1095`）

```
wire_state = WireState { include_usage: request.include_usage, .. }
passthrough = same_protocol(inbound, config)
emit_initial = !passthrough && !upstream_provider.is_passthrough()
accounting = Arc<Mutex<StreamAccounting>>            // 记账快照，生成器边跑边推进
events = first_frame_timeout(parse_sse_stream(upstream.bytes_stream()), 300s)

生成器:
  _disconnect_guard = DisconnectGuard(accounting)     // 随生成器销毁，drop 时补记
  if emit_initial: 补 message_start（Anthropic 上游才需要）
  while frame = events.next():
      · 直通: 先 yield frame.raw（原文，注释心跳/多行 data 全保真）
      · data == "[DONE]": upstream_ended = true; continue（收尾统一到流末）
      · 解析失败（非 JSON）: continue
      · upstream_provider.decode_stream_event(...) → 规范事件
           ├─ Ok: assembler.apply（记账旁路）; 非直通时编码出站
           └─ Err: stream_error=..; 非直通时发 error 事件; 直通时**什么都不发**
  // 流末（B4：decode_stream_done 只在这里调一次）
  decode_stream_done(...) → 未收到结束信号的流在这里补终态
  wire_state 补 usage 终值；非直通时 encode_stream_done → usage 尾片/[DONE]/response.completed
  verdict = upstream_state.verdict(stream_error) → (ok, error)
  account.record(ok, error, stats)                    // 置 finished，守卫让位
```

**记账旁路**：`StreamState` / `ResponseAssembler` 的推进与是否直通无关——两条路径跑同一份代码，
明细照常落。**直通只影响客户端出口**，不影响记账与判罚。

**流判罚**（`StreamVerdict`，流结束后算，不干预流）：

| 判罚 | 触发 | 落库口径 |
| --- | --- | --- |
| `Ok` | 正常收尾 | 成功 |
| `EmptyReasoningOnly` | 只给了思考、无正文/工具（`text_chars==0 && tool_count==0 && thinking_chars>0`） | 失败 |
| `Truncated` | 上游没发结束信号（`finish_reason`/`message_stop`/`[DONE]` 都没有） | 失败 |
| `UpstreamError` | 解码/传输报错，或上游流内报错 | 失败 |

**断连兜底**（`DisconnectGuard`，`server.rs:727-761`）：客户端中途断开时 hyper 直接 drop 响应流，
生成器尾部永远跑不到。守卫在 drop 时按快照补一条 `ok=false` / `error="客户端断开"`。正常收尾先置位
`finished` 认领，守卫随即让位——**不双记**。守卫加锁用 `unwrap_or_else(|p| p.into_inner())`，
生成器持锁 panic 也能补记。

**块状态机**（`providers/normalizer.rs`）：把任意上游增量收成合法规范块序列，不变式 I1–I5
（单开块 / 载荷进匹配块 / 索引递增 / 并行工具缓冲串行化 / 工具必有块）。全链路正确性核心，测试锁死。

---

## 5. 落库与统计

**落库已解耦（A5 已修，2026-09-26）**：所有写库都交给一个**专用写线程**（`db::submit`），
网关只做一次**非阻塞投递**——`usage` / `settings` / `filters` / `events` / `updates` 全部走它。
写线程独占**写连接**（`synchronous=NORMAL`），读走另一条**只读连接**（`query_only=ON`），
WAL 下读不挡写。投递队列有界（4096），只有积压超过容量时才会按背压阻塞（选定「不丢数据」的代价）。
`db::flush()` 是屏障（测试 / 退出前用）；写失败没有调用方可以返回，记在 `db::write_failures()` /
`db::last_write_error()`，并在 `/health` 暴露。

两条路径都把「决定记什么」和「写库」分开：

- **非流式 / 失败**：`route()` 里 `usage::submit`（`usage.rs`）投递一条记录（明细 + 每日汇总 + 可选报文）——
  投递**在返回响应之前**，但不再等这次事务（含 commit/fsync）；客户端的响应延迟因此不再含 DB 时间。
- **流式**：`StreamAccounting::record`（生成器尾部）与 `DisconnectGuard::drop` 共用同一份投递。

**汇总查询只读每日表（rollup）**：`usage_daily_total`（一行 = 一天）在写入时就增量维护，
`summary()` / `totals()` 只读它（O(天数)），**不再扫 `usage_detail` 全表**——
统计页每 5 秒轮询也不会再与转发争用。

明细字段（`usage_detail`）：时间、模型名、`served_by`、来源应用、入站/上游协议、上游地址与模型、
是否走代理、input/output/cache_read/cache_write/reasoning tokens、耗时、`ok`、`failover`、错误文案。

报文（`usage_payload`）：入站报文 + 入站 header + 发往上游的报文 + 上游响应，各字段按
`PAYLOAD_MAX_BYTES = 256KB` 截断并置 `*_truncated`；流式响应是 `ResponseAssembler` 拼装后
再编码成**上游协议形状**的重建件（不是上游原文，展示用，已接受）。

**`failover` 字段语义**：不是「实际切换成功」，而是「本次请求触发了一次切换探测」（写库时即可判定）。
真正的切换结果由 `events` 表与统计呈现。逐请求事件目前前端没有页面展示（见 §8-A7）。

统计计数器（`GatewayStats`，`gateway/mod.rs:74-146`）：进程内原子计数，启动时用 `usage::totals()`
`hydrate()` 覆盖历史累计，跨重启保留。

---

## 6. 事后切换（`gateway/failover.rs`）

```
触发判定（route() 失败落库时，server.rs:367-371）:
    qualifies(auto_failover, named, retryable)
      = auto_failover && !named && retryable
      retryable = dispatch 判定：5xx | 401 | 403 | 404 | 408 | 429 | 网络错误 | 超时

执行（route() 落库后 tokio::spawn）:
    start_probe(stats, ctx)   // 进程级哨兵 PROBING：占用中则本次触发合并进正在跑的那轮
      run():
        for id in probe_order(models, failed_model_id):   // 模型列表原顺序，跳过刚失败的
            probe_completion(model)   // 15s 硬超时；失败/构造错误一律视同不通过
            通过 → may_switch(当前 active == 失败的那个) 复查（防覆盖用户手切）
                    → settings::mutate(active_model_id = 新) + events + stats.record_failover
                    → 结束
        全部不通过 → events::log("model.failover.exhausted")，原地不动
    哨兵由 ProbeGuard 在 drop 时释放（网关停止、任务被丢弃也释放）
```

要点：

- **探测与切换只服务下一次请求**，不接手本次——客户端已拿到错误响应。
- **触发条件是纯函数**（`qualifies` / `probe_order` / `may_switch`），可单测。
- **防抖**：同一时刻最多一轮探测，并发失败合并。
- **超时算 retryable**：换模型可能通，因此超时照旧触发探测，明细 `failover` 置位。

---

## 7. 关键不变式与已定取舍（速查）

**不变式**（改动前先看这里）：

- 规范请求 JSON 恒为 **Anthropic Messages 形状**（`raw`）；`body` 由 `raw` 反序列化，两者经
  `map_raw` 永不脱节。
- 网关只承载**单候选**：`n>1` 与 `n==0` 都拒绝。
- 客户端出口有三处直通点，全由 `same_protocol` 一个谓词控制；`is_passthrough()` 是另一维度。
- 流式收尾：`decode_stream_done` **只在流末调一次**（`[DONE]` 分支只记 `upstream_ended`）。
- 客户端断连也要落库，且与常规收尾**不双记**（`finished` 单向认领位）。
- `MutexGuard` 绝不跨 `yield`（否则生成器不再是 `Send`，且守卫 drop 时可能自锁）。

**已定取舍**（不是缺陷，别当 bug 查）：

| 取舍 | 取法 | 理由摘要 |
| --- | --- | --- |
| 切换时机 | 响应之后后台异步 | 客户端立即拿到错误，探测不拖慢本次 |
| 点名模型 | 锁定，失败不切换 | 尊重显式选择 |
| header 入库 | 原样存不脱敏 | 本地工具，库中本就有上游 Key 明文 |
| CORS / token | 全开放 + 固定可读 token | 只监听 127.0.0.1，单机场景可接受 |
| 流式超时 | 不设总超时 | 长生成合法 |
| 直通尾巴 | 不补起始/结束 | 上游原文就是客户端该收到的 |
| `EmptyReasoningOnly` | 记为失败 | 上游异常收尾，客户端只看到思考 |
| `service_tier` | Anthropic 侧一律丢弃 | 词表不同，转发必 400；丢默认档等于没丢 |
| 流式明细报文 | 重建件非原文 | 展示用途 |

---

## 8. 问题清单

分三组：**A = 当前代码里确实存在的缺陷**（该修）；**B = 已决定不动的取舍**（别重开）；
**C = 尚未落地的既有条目**（有方案，按序做）。🆕 = 本轮复核源码时新增/加强的判断。

### A. 真实缺陷（建议修）

**A1｜首帧之后没有任何超时或 keepalive（高）**
`build_client`（`providers/mod.rs:627-629`）只设了 `connect_timeout(20s)`，**没有** `read_timeout`、
没有 `tcp_keepalive`。流式链路只护住「等响应头」与「等首帧」两程；**首帧一到，限时全部撤防**
（`first_frame_timeout` 的语义就是「第一帧到了就不再限时」）。此后若连接半开（NAT 老化、断网、
上游假死不发 FIN），本地代码永远停在 `events.next().await`（`server.rs:985`），客户端一直等、
明细永远不落库（`DisconnectGuard` 只在客户端自己断开时才补记）。
**修法**：`build_client` 加 `.read_timeout(UPSTREAM_TIMEOUT)`（300s）。它「每次成功读取都会重置」，
正好是帧间隔看门狗语义，不会掐断正在正常输出的长流；与 `UPSTREAM_TIMEOUT` 同值，两处口径一致。
（`transport-review.md` H1，一行级改动，**本轮明确未做**。）

**A2｜出站跟随重定向，跨主机不剥离 `x-api-key`（高）**
共享客户端用 reqwest 默认重定向策略（`limited(10)`，跟随最多 10 跳）。跨主机跟随时 reqwest 只剥
5 个 header（`Authorization`、`Cookie`、`Cookie2`、`Proxy-Authorization`、`WWW-Authenticate`），
**`x-api-key`、`anthropic-version`、`anthropic-beta` 都不在其中**。而网关发往 Anthropic 上游的
鉴权正是 `x-api-key`。用户大多把模型指向第三方中转，上游只要回 `302 Location: https://evil/…`，
用户的 Key（很多中转和官方共用一把）就会被送到该主机；响应体也变成第三方内容；https→http 降级
照样跟随。
**修法**：`build_client` 加 `.redirect(Policy::none())`，上游 3xx 就按非 2xx 走现成错误路径。
（`transport-review.md` H3，一行级改动，**本轮明确未做**。）

**A3｜流末「先补终态、再判罚」：截断/出错的流，客户端收到的是「正常结束」（中高）**
收尾顺序是**先补尾巴、后判罚**：`server.rs:1058-1085` 先调 `decode_stream_done`（对没收到结束信号的
流会补 `state.finish("end_turn")` → `message_delta` + `message_stop`），并经 `encode_for_client` 发给
客户端；判罚 + 落库在 `server.rs:1089-1092`，**在尾巴之后**。于是同一条流里：**落库诚实地记
`Truncated`/`UpstreamError`，线上客户端却看到完整的正常结束**——Anthropic 入站是 `message_stop`，
Completions 入站是 `finish_reason:"stop"` + `usage` + `[DONE]`，Responses 入站是
`response.completed`。（`transport-review.md` M1。）

**🆕 A3+｜流内出错时，客户端会同时收到「错误事件」和「正常结束」**——这是 A3 的一个更刺眼的特例。
解码 `Err` 分支（`server.rs:1042-1052`）对非直通客户端**先 yield 一条 `error` 事件**；但
`error_events()` 是从 `wire::stream_error_event` 直接构造 `SseEvent`，**没有推进 `StreamState`**，
`state.finished` 仍是 `false`。于是紧接着的流末 `decode_stream_done` 判定「流没结束」，**又补一套
正常终态**发给同一个客户端。结果：**`event: error` 后面跟着 `message_stop`**（跨协议路径）。
**修法（A3 与 A3+ 一起）**：把判罚提到尾巴**之前**，让尾巴按判罚分支：
`Ok`/`EmptyReasoningOnly` 照常补终态；`Truncated`/`UpstreamError` 不补正常终态，改按入站协议发
终止性错误——Anthropic `event: error`、Completions 直接断流且**不发 `[DONE]`**、Responses 发
`response.failed`。注意别把 `usage` 弄丢（`message_delta` 是带累计 token 的那条）。

**A4｜直通 + 首帧前失败 = 一个空的 200（中）**
直通时网关刻意不补任何事件（理由：上游原文已发出，补上去等于伪造内容）。这个理由对**流中途**
的错误成立，但对**一个字节都没发出去**的情况不成立——而 `Err` 分支（`server.rs:1042-1052`）用的是
同一个判断。于是「首帧超时」或「建连后立刻断」在直通路径上表现为：客户端拿到
`200 text/event-stream`，响应体**立即结束、0 字节**，既没有错误事件也不知道是超时还是断网。
非直通路径则会发形状正确的 `error` 事件。
**修法**：生成器里记一个局部标志「已经发过字节没有」（`yield` 前置位即可），
`passthrough && !emitted` 时按入站协议发错误事件。
（`transport-review.md` M2。）

**A5｜同步 SQLite 跑在 tokio worker 上，且全进程单连接（中）✅ 已修（2026-09-26）**

> **落地结果**：不再用 `with_tx` / 单把 `Mutex<Connection>`。改为**专用写线程 + 有界 channel**
> （`db::submit` / `db::flush` / `write_failures` / `last_write_error`）：所有写库都是非阻塞投递，
> 只有积压超容量（4096）才按背压阻塞。读走单独的**只读连接**（`PRAGMA query_only=ON`，不用
> `SQLITE_OPEN_READ_ONLY`——WAL 下真正的只读连接会卡在 `-shm`）；写连接 `synchronous=NORMAL`。
> `summary()` / `totals()` 改为**读每日汇总表 `usage_daily_total`**（写入时增量维护，O(天数)），
> 不再扫 `usage_detail`。`DisconnectGuard::drop` 与其它写点一样只做投递——**没有用 `spawn_blocking`**：
> 因为运行时关闭期 `spawn_blocking` 会 panic，而 Drop 里入队只是内存操作（仅在队列满时阻塞）。
> 新增守护测试：`daily_rollup_tracks_cache_failovers_and_totals`、`v10_backfill_fills_daily_rollup_from_detail`、
> `database_write_failures_are_observable`；`sqlite_persistence_round_trips` 插入了 `db::flush()` 同步点。

原始问题（修复前）：`db::with_conn`/`with_tx` 是**同步** rusqlite，锁 `OnceLock<Mutex<Connection>>`
——全进程一把锁。调用点全在异步上下文：失败落库、流末 `StreamAccounting::record`（跑在 SSE 生成器
所在 worker）、以及 **`DisconnectGuard::drop`**。反过来 `summary()` 走 `read_since`：`SELECT … WHERE day >= ?1`
把窗口内**所有行**读进内存再在 Rust 里累加。**影响**：一次前端大查询握锁时，网关落库全部排队；
落库的 fsync 又占住一个 worker。「面板一刷新，转发就卡顿」的机制就在这里。
（`transport-review.md` M3。）

**A6｜三处无上限缓冲（中）**
① 非流式响应体 `upstream.json::<Value>()`（`server.rs:861`）；② 上游错误体 `response.text()`
（`server.rs:579`）；③ SSE 行/帧缓冲（`sse.rs:33-67` 的 `buffer` / `frame.raw`，上游不吐 `\n\n`
时会无限增长）。触发条件是「上游坏/被劫持」，在 A2（跟随重定向）打开时门槛不高——本机进程可能被
上游 OOM 掉。
**修法**：非流式先看 `Content-Length` 再累加 `chunk().len()` 到上限（如 32MB）；错误体上限可更小
（1MB）；`parse_sse_stream` 给单行、单帧各设上限（如 1MB / 8MB），超限 `yield Err` 走现成错误路径。
（`transport-review.md` M4。注：H2 已给**入站**加了上限，出站这三处没有。）

**A7｜报文永不清理；`events` 表无界面（中）**
一次请求最多写 4 个报文字段、各 256KB，最坏约 1MB；库是 WAL，**没有任何删除或 `VACUUM` 实现**，
而 `payload_detail` 的注释（`usage.rs:297`）已经写着「无报文记录（老数据或**已被保留策略清理**）」——
注释描述的是一个不存在的机制。长跑几个月后磁盘与 `usage_detail` 续读都会变慢。
另：`events::log` 只被 6 处调用（状态变更），后端 `list_events` 与前端 `eventApi.list` 都在，
**没有任何页面调用它**——审计记录只能翻库；`Cargo.toml` 没有 `log`/`tracing`，进程对 stdout 沉默。
**修法**：加启动 + 每日一次的清理（删早于 N 天的 `usage_payload`，保留 `usage_detail`），N 做成设置项；
给 `events` 表补一个列表页。**不建议**引入完整日志栈。（`transport-review.md` M5 + L6。）

**A8｜`extract_token` 大小写敏感，且把 `Basic` 当 token（低）**
`server.rs:80-91`：`trim_start_matches("Bearer ")` 区分大小写（RFC 7235 规定 scheme 不区分大小写，
`bearer`/`BEARER` 都合法）；`Authorization: Basic …` 会被原样当成 token 记进「来源应用」字段。
**修法**：切分后 `eq_ignore_ascii_case("bearer")`，非 Bearer scheme 返回 `None`。
（`transport-review.md` L2。）

**A9｜安装包下载无超时、失败留半截文件（低）**
`commands/apps.rs:302` 的 `get(url).send()` 无请求级超时（A1 的 `read_timeout` 顺带覆盖）；
`File::create(&destination)`（:311）直接写最终文件名，失败/中断留下半截文件，下次不清理也不复用。
**修法**：下到临时文件名，成功后 `rename`。（`transport-review.md` L4。）

**A10｜慢客户端反压的上限（低）**
客户端读得慢 → 生成器不被 poll → 不再 poll 上游 → 上游连接被一直攥着。这是**正确的**背压
（不缓冲进内存），问题在**上限**：客户端被冻住但不发 FIN/RST 时，这条链可以永久保持，入站侧同样
没有 keepalive 与写超时。**修法**：给监听 socket 开 `SO_KEEPALIVE`，或明确接受现状。
**不建议**加总时长上限。（`transport-review.md` L5。）

**A11｜网关停止时的宽限硬关被记成「客户端断开」（低）**
停止/重启时 5s 后硬关在途流（`gateway/mod.rs:17`、`:319-332`），`DisconnectGuard` 会把它们记成
「客户端断开」（`server.rs:754-759` 固定文案）——实际是网关自己关的，会误导「为什么我的长生成断了」
的排查。**修法**：给守卫一个可选的「关闭原因」，停止流程先置位再触发硬关。
（`transport-review.md` L7。）

### B. 已决定不动（别重开）

- **C3 网关鉴权 / CORS**：`CorsLayer::permissive()` + 固定可读 token（= app kind）在单机场景可接受
  （只监听 `127.0.0.1`）。2026-09-25 与用户确认「决定不做」，两条备选路径（Origin 白名单 / 随机
  token）留档在 `forwarding.md` §8 批 4。
- **入站 header 原样入库不脱敏**：既定决策，风险归到 A2 与 A7，而不是「该不该脱敏」。
- **单一上游 + 事后探测切换**、**流式不设总超时**、**同协议直通不补尾巴**、
  **`EmptyReasoningOnly` 记为失败**、**`Dropped` 对 Anthropic 自带的 `service_tier` 也生效**、
  **流式明细报文是重建件**：均为已定案取舍（见 §7）。

### C. 已定案但未落地（有方案，按序做）

| 条目 | 状态 | 出处 |
| --- | --- | --- |
| A1（`read_timeout`） | ❌ 未做 | transport-review H1 |
| A2（`Policy::none()`） | ❌ 未做 | transport-review H3 |
| A3 / A3+（判罚先于尾巴） | ❌ 未做（A3+ 为 🆕） | transport-review M1 |
| A4（直通首帧失败补错误事件） | ❌ 未做 | transport-review M2 |
| A5（落库 spawn_blocking / 只读连接 / SQL 聚合） | ❌ 未做 | transport-review M3 |
| A6（三处上限） | ❌ 未做 | transport-review M4 |
| A7（报文保留策略 + events 页面） | ❌ 未做 | transport-review M5 / L6 |
| A8–A11 | ❌ 未做 | transport-review L2 / L4 / L5 / L7 |

> 已在工作区落地（相对 HEAD）：**H2 入站体积**（32MB 逻辑 + 64MB 硬上限 + 413 协议形状 + 落库）、
> **L1 事件名回落**（`event:` 与 `data.type` 两个来源都试）、**L3 绕行名单收成一份**
> （`NO_PROXY_LIST`）。另加了单实例插件（`lib.rs`，非转发路径）。

### 附：我在复核中发现的其他小观察（低优先，供参考）

- **上游响应头从不被读取**（全仓库只有安装包下载读 `headers()`）——上游的 `x-request-id` 被丢掉，
  而它正是中转/官方支持要的号；明细行 id 是落库才生成的自增号，请求进行中无法引用、也从不发给上游。
  建议在入口生成短 id 并发给上游 + 写进明细（便宜的一半），完整日志栈建议不做（已并入 A7 的收尾）。
- **`first_frame_timeout` 把「心跳帧」也算首帧**：上游只要发 `event: ping` 就能让首帧计时解除。
  对长时间思考的上游是好事，但也意味着「只发心跳不吐正文」能一直保持连接——在 A1 修好前，这是
  唯一能让转发流不悬死的东西。
- **`stats.requests` 在鉴权之前自增**（`server.rs:328`）：缺 Key / 超限的请求也计入面板请求数。
  与 `errors` 口径一致，属预期，但看面板时要知道。

---

## 9. 建议的修复顺序

按「风险 × 成本」排序，每步独立可回滚、独立可测。**先做 1、2**（各一行，风险与收益最不对称）。

| 步 | 内容 | 为什么先做 |
| --- | --- | --- |
| 1 | **A2** `Policy::none()` | 一行，堵住 Key 泄漏给重定向目标 |
| 2 | **A1** `read_timeout(300s)` | 一行，堵住半开连接永久挂起 + 漏记 |
| 3 | **A3 + A3+ + A4** 判罚先于尾巴 + 直通首帧失败补错误事件 | 都在流末/错误分支，一起改，三协议各加守护测试 |
| 4 | **A5** ✅ 已修（写线程 + rollup）；**A7** 的报文保留一半已修（`cleanup_expired_payloads` 每日清理），`events` 页面未做 | A5 见 §8-A5；都在 `usage.rs` + `db/mod.rs` |
| 5 | **A6** 三处缓冲上限 | 收尾健壮性 |
| 6 | **A8–A11** | 低优先，择机 |

> 第 3 步动手前需先验证一点：**截断时 usage 怎么给客户端**（Anthropic 的 `error` 事件不带 usage）——
> 可以先发一条只带 usage 的 `message_delta` 再发错误，或接受截断时 usage 记已知部分并只发错误。
> 这取决于客户端 SDK 的实际解析行为，属于「改动前需先验证」的点。

---

## 10. 守护测试现状（149 passed, 1 ignored）

| 守护点 | 测试 |
| --- | --- |
| 标量协议表完备性 | `protocol_profiles_cover_every_canonical_field` |
| 同协议请求保真 / 跨协议重建 | `same_protocol_passthrough_keeps_the_client_payload_intact`、`gateway_keeps_same_protocol_payloads_and_rebuilds_across_protocols`、`responses_passthrough_keeps_client_extensions` |
| 直通只在过滤器改写时覆盖 system | `passthrough_rewrites_system_only_after_a_filter_changed_it` |
| 直通判定恰好等价协议相等 | `same_protocol_holds_exactly_when_the_formats_match` |
| 响应逐字转发 | `same_protocol_response_is_forwarded_verbatim` |
| SSE 字节保真 + 跨 chunk 多字节字符 | `sse_frames_keep_the_upstream_bytes_intact`、`sse_frames_decode_whole_lines_so_cjk_survives_chunk_boundaries` |
| 首帧限时不掐长生成 | `first_frame_timeout_gives_up_when_the_upstream_never_speaks`、`first_frame_timeout_stops_policing_after_the_first_frame` |
| 断连兜底落库且不双记 | `gateway::server::tests::disconnect_guard_records_what_the_stream_had_already_accounted_for`、`disconnect_guard_stays_silent_after_the_regular_hand_off` |
| 流内错误事件形状（两来源一致） | `gateway::server::tests::stream_errors_have_one_shape_per_inbound_protocol` |
| 分片 id 进记账 | `completions_stream_captures_the_upstream_chunk_id` |
| `service_tier` 对 Anthropic 一律丢弃 | `anthropic_clients_lose_their_own_service_tier_until_the_parameter_is_verified` |
| 块状态机不变式 | `block_normalizer_keeps_payloads_in_their_own_block`、`block_normalizer_serializes_parallel_tool_arguments` |
| 流判罚 | `stream_verdict_flags_reasoning_only_and_truncated` |
| 过滤器 | `filter_injects_system_prompt`、`filter_skips_disabled_and_stacks_in_order` |
| 入站体积上限（判定 + 413 形状） | `gateway::server::tests::inbound_body_check_rejects_only_above_the_cap`、`oversize_error_keeps_the_inbound_protocol_shape` |
| 超大入站请求被拒且落库（**默认忽略**） | `gateway::server::tests::h2_oversize_request_is_rejected_and_recorded` |
| SSE 帧名回落（两个来源） | `responses_frames_fall_back_to_the_body_type_when_the_envelope_is_unknown`、`anthropic_frames_fall_back_to_the_body_type_when_the_envelope_is_unknown` |
| 绕行名单语义与只覆盖回环 | `loopback_bypass_matches_reqwest_rules`、`no_proxy_list_only_covers_loopback` |
| 每日汇总增量维护（缓存/切换/total 三列） | `usage::tests::daily_rollup_tracks_cache_failovers_and_totals` |
| v10 回填（旧库补列后按明细回填每日表） | `db::tests::v10_backfill_fills_daily_rollup_from_detail` |
| 异步写失败可观测（计数 + 最近错误） | `database_write_failures_are_observable` |

唯一 `#[ignore]` 的那条要单独跑（它写进程级 usage 库，全量跑会顶掉 `sqlite_persistence_round_trips`
的条数断言）：

```bash
cargo test h2_oversize_request_is_rejected_and_recorded -- --ignored
```

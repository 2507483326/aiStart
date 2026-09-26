# 传输层复核：超时、缓冲、落库与重定向（2026-09-25）

**定位**：`docs/forwarding.md` 管「协议转换与保真」这条线（A / B / C 三组问题已收口）。本文是它的
**传输层姊妹篇**：只看网络与流式管道本身——超时与半开连接、缓冲上限、入站体积限制、重定向与凭据、
落库时序与磁盘增长。已经定案的事项不重复提（见 §5）。

**基线**：`HEAD = 73f8b3b`。涉及 `gateway/{server,sse,mod,failover}.rs`、`providers/{mod,wire}.rs`、
三个 provider、`usage.rs`、`db/mod.rs`、`commands/apps.rs`。依赖版本以 `Cargo.lock` 为准
（reqwest 0.13.5、axum 0.8.9、tower-http 0.7.1、rusqlite 0.32）。

**证据等级**（每条都标）：

- `读码确认`——对着本仓库源码（或 vendored 依赖源码）逐行读出，附 `file:line`；
- `已实测`——在本机跑过，附复现方法；
- `未实测`——推断，写明成立条件。

## 1. 结论摘要

| 编号 | 级别 | 问题 | 证据 | 成本 |
| --- | --- | --- | --- | --- |
| H1 | 高 | 首帧之后没有任何超时或 keepalive：上游半开（NAT 老化、断网、假死）时客户端永久挂起，明细也永远不落库 | 读码确认 | 一行 |
| H2 ✅ | 高 | 入站请求体 2MB 硬上限（axum 默认）在 handler 之前生效：`text/plain` 的 413、不落库。带 base64 图片的请求（A4 明确支持）正是这么被拒的 | 已实测 | 中 |
| H3 | 中高 | 出站跟随重定向（默认 ≤10 跳），跨主机只剥离 5 个 header，`x-api-key` 不在其中 → Anthropic 上游的 Key 会被送到重定向目标 | 读码确认（reqwest 源码） | 一行 |
| M1 | 中高 | 被截断的流给客户端补的是**正常结束**（`message_stop` / `finish_reason:"stop"` / `response.completed`）：库里写着 Truncated，客户端以为答完了 | 读码确认 | 中 |
| M2 | 中 | 同协议直通 + 上游在首帧前失败：客户端收到一个**空的 200**，没有错误事件也没有可读原因（非直通路径会给错误事件） | 读码确认 | 小 |
| M3 | 中 | 同步 SQLite 直接跑在 tokio worker 上（含 `DisconnectGuard::drop`），全局单连接互斥；`summary()` 把窗口内所有行拉进内存再在 Rust 里聚合 | 读码确认 | 中 |
| M4 | 中 | 三处无上限缓冲：非流式响应体 `upstream.json()`、上游错误体 `response.text()`、SSE 行/帧缓冲 | 读码确认 | 中 |
| M5 | 中 | 报文永不清理：单请求最坏 4 × 256KB；`payload_detail` 的注释已经写着「已被保留策略清理」，而清理并不存在 | 读码确认 | 中 |
| L1 ✅ | 低 | Anthropic / Responses 解码：`event:` 非空时完全压过 `data.type`，名字对不上就静默丢帧（Completions 不看 `event`，不受影响） | 读码确认 | 一行 |
| L2 | 低 | `extract_token` 的 `Bearer ` 前缀大小写敏感；`Authorization: Basic …` 会被当 token 记进来源应用 | 读码确认 | 一行 |
| L3 ✅ | 低 | 「走不走代理」的两套判定（`is_loopback_target` 与 `NoProxy` 名单）各写一份、靠人肉同步（语义今天是等价的，见 §4 勘误） | 读码确认 | 小 |
| L4 | 低 | 安装包下载无超时（H1 顺带解决），失败或中断留下半截文件，下次不清理 | 读码确认 | 小 |
| L5 | 低 | 慢客户端反压：卡住的客户端把上游连接一直攥着，入站侧没有 keepalive/写超时 | 读码确认 | 中 |
| L6 | 低 | 没有任何日志设施；`events` 表有数据但前端没有页面调用 `list_events`；上游响应头从不被读取，上游自己的请求号被丢掉 | 读码确认 | 小（不含运行日志设施） |
| L7 | 低 | 网关停止时 5s 宽限期硬关在途长流，明细记成「客户端断开」——实际是网关主动关的 | 读码确认 | 一行 |

**✅ = 2026-09-25 本轮已落地**（H2 / L1 / L3，逐条记录见 §8）。其余条目的方案仍按 §7 的批次留档，
其中 **H1 与 H3 本轮明确未做**——两条都还是一行级改动、方案原样有效，别把「没标 ✅」读成「已否决」。

## 2. 高

### H1｜首帧之后没有任何超时或 keepalive（B1 的剩余缺口）

**现象**：`UPSTREAM_TIMEOUT` 只盖住两程——非流式是请求级超时（`server.rs:530`），流式是「等响应头」
（`server.rs:533-537`）与「等首帧」（`sse.rs:89-110`，`server.rs:895`）。**首帧一到，限时就全部撤防**
（`first_frame_timeout` 的注释写得很清楚：「第一帧到了就不再限时」）。

此后如果连接进入半开状态——家用 NAT 把空闲映射老化掉、Wi-Fi 切换、上游进程被冻住不 FIN——
客户端会一直等，本地代码永远停在 `events.next().await`（`server.rs:940`）。`DisconnectGuard`
（`server.rs:697-732`）只在客户端自己断开时才补记；客户端不主动断，这条请求就既不返回也不落库。
`http_client` 只设了 `connect_timeout(20s)`（`providers/mod.rs:550-552`），没有 `read_timeout`，
也没有 `tcp_keepalive`：`SO_KEEPALIVE` 不设意味着不依赖 OS 兜底（Windows 默认就是关的）。

`docs/forwarding.md` §7-B1 描述的正是「流式上游中途停止吐数，请求会永久挂起」，批 3 只关掉了
「一个字节都没吐」的那一半；**吐过第一个字节之后的那一半仍然敞着**。

**证据**：`读码确认`。reqwest 0.13.5 的 `read_timeout` 文档（`~/.cargo/registry/.../reqwest-0.13.5/src/async_impl/client.rs:1455-1462`）：
「applies to each read operation, and resets after a successful read」——正好是「帧间隔看门狗」的语义，
不会把正在正常输出的长流掐断。

**建议**（两条路，推荐 A）：

A. 一行级：共享客户端加 `read_timeout(UPSTREAM_TIMEOUT)`（即 300s），放在 `providers/mod.rs::build_client`。

```rust
let mut builder = reqwest::Client::builder()
    .connect_timeout(Duration::from_secs(20))
    // 只在「连着但一个字节都没有」时触发：每次成功读取都会重置，
    // 持续输出的长流不会被它掐断；与 UPSTREAM_TIMEOUT 同值，两处口径一致。
    .read_timeout(Duration::from_secs(300));
```

选 300s 是因为它**正好等于** `UPSTREAM_TIMEOUT`：首帧那一程两处同时到期，不产生两套口径；首帧之后，
任何一次 300s 的完全沉默都判定为连接已死。对所有取用这个客户端的路径也自洽——探测/翻译/版本检查
都另有更短的请求级超时（`commands/models.rs:194`、`commands/translate.rs:65`、`updates.rs:69`），
非流式转发本来就有 300s 总超时；顺带补上安装包下载的漏洞（L4）。

B. 更严格：把 `first_frame_timeout` 改成真正的帧间隔看门狗——首帧给 300s，其后每帧之间给
90s（`sse.rs:89` 一个函数内部就能做，签名不变）。代价是多一份状态与两条测试，收益是把「半死」
判得更快。**注意**：不能在客户端层面配 90s 的 `read_timeout`，那会把首帧预算从 300s 砍到 90s。

**代价**：A 的 300s 沉默到底算不算「死」，取决于上游会不会发心跳。Anthropic 官方与多数中转会在
长思考期间发 `event: ping`；本地大模型跑满 300s 不吐任何字节的情况极少，且真发生时可调大这一值。

**验证状态**：`未实测`（修法来自依赖源码与文档，未构造半开连接验证）。

### H2｜入站 2MB 硬上限，超限时既不是协议形状也不落库

**现象**：`router()`（`server.rs:32-41`）没有设 `DefaultBodyLimit`，三个 handler 都直接收 `Bytes`
（`server.rs:273-295`）——axum 的默认上限 2MB 在 handler 之前生效。超限请求**永远不会进 handler**：
不落库、不计数（`stats.requests` 也不加）、响应体是纯文本。

**证据**：`已实测`。临时在 `router()` 上加了一条打点路由，用 `router().oneshot` 灌请求体：

```text
SIZE  1000000 -> 400 Bad Request  ctype=application/json  body={"type":"error",…"请求体不是合法 JSON"}
SIZE  3000000 -> 413 Payload Too Large  ctype=text/plain; charset=utf-8
                 body=Failed to buffer the request body: length limit exceeded
SIZE 12000000 -> 413 …（同上）
```

1MB 那个是故意灌坏数据走通的正常路径，说明**上限就在 2MB**；3MB / 12MB 都是 axum 的
`text/plain` 413。复现方法：临时加一条与 `/v1/messages` 同 extractor 的路由，用 `oneshot` 发不同
`Content-Length` 的 body，观察状态码与 `content-type`。

**影响**：`docs/forwarding.md` §8 批 2 的 A4 明确支持 `data:` URL 的 base64 图片——一张 1.5MB 的
图片 base64 之后约 2MB，再加提示词就一定越线。Anthropic / OpenAI 的官方 SDK 拿到
`text/plain` 的 413 只会抛出「无法解析响应体」这类错误，用户看到的不是「请求太大」；明细里连一条
失败记录都没有，事后完全查不到。

**修法（已落地，见 §8）**：两层上限，而不是把 `DefaultBodyLimit` 直接抬到 32MB——

1. `DefaultBodyLimit::max(64MB)`：**硬上限**，只挡明显离谱的请求。写在 `.layer(CorsLayer::permissive())`
   之前（后加的层在外层），让 axum 自己回的那个 413 也带 CORS 头；
2. handler 内自检 `body.len() > 32MB`：**逻辑上限**，超限按入站协议的形状回 413
   （复用 `api_error(inbound, StatusCode::PAYLOAD_TOO_LARGE, "request_too_large", …)`），并走现成的
   失败落库路径记一条明细。

提案里原本只写「抬到 32MB」，落地时把上限分成两层：只抬层的话，**超过** 32MB 的请求照样是 axum 的
纯文本 413、照样不落库——而「一张 2MB 图 + 长提示词」这类**稍微**超一点的情况恰恰最常见，正是要让它
拿到协议形状与一条可查明细的那些请求。64MB 之上的请求是滥用，不值得为它准备 JSON 形状和库记录。

**代价**：上限抬高 = 单请求内存峰值抬高（见 M4，两件事一起处理更划算）；32–64MB 之间的请求会被
完整读进内存再拒掉，这是「给它一个好错误」的必要代价。第 2 条要在 handler 里多一次 `body.len()`
判断，几乎零成本。

**验证状态**：`已实测`（axum 行为、响应形状）；端到端 `已实测`——`#[ignore]` 的
`h2_oversize_request_is_rejected_and_recorded` 用 40MB 请求真的跑通了一遍：413 + 一条
`ok=false` 明细 + 报文截断标记。

### H3｜跟随重定向：跨主机不剥离 `x-api-key`

**现象**：共享客户端用 reqwest 的默认重定向策略——`Policy::limited(10)`，即**跟随最多 10 跳**
（`reqwest-0.13.5/src/redirect.rs:160-165`）。跟随跨主机（host / port / scheme 任一不同）时，reqwest 只剥离
5 个 header（`redirect.rs:239-247`）：

```rust
headers.remove(AUTHORIZATION);
headers.remove(COOKIE);
headers.remove("cookie2");
headers.remove(PROXY_AUTHORIZATION);
headers.remove(WWW_AUTHENTICATE);
```

`x-api-key`、`anthropic-version`、`anthropic-beta` 都不在其中。而网关发往 Anthropic 协议上游的鉴权
正是 `x-api-key`（`providers::headers` 出的；`anthropic-beta` 还额外从客户端透传，`server.rs:516-523`）。

**影响**：本应用的用户大多把模型指向第三方中转（这正是「自定义上游」的用途）。上游只要回一个
`302 Location: https://evil.example/…`，用户的 Key 就会被原样发到那个主机——而很多人给中转用的是
**同一把官方 Key**。跟随还意味着响应体来自那个主机，明细里的「上游响应」会变成第三方内容；
scheme 降级（https → http）也照样跟随（跨 scheme 算跨主机，但如上，Auth 之外的 header 不剥）。
另外，重定向会把 POST 原样带到新地址（非流式请求会按新地址执行一次），用户配置的「一个上游」
在实践中变成了「一个上游 + 它指定的任意跳转链」。

**建议**：给 API 流量关掉自动重定向——`providers/mod.rs::build_client` 加一行 `.redirect(Policy::none())`。
关掉之后上游回 3xx 就按「非 2xx」走现成的错误路径（`server.rs:548-565`，含响应体保留与可重试判定），
用户能一眼看到「上游要我去别处」。安装包下载（`commands/apps.rs:302`）如果确实需要跟 302
（GitHub Release 常见），那里**单独**显式跟随（手工 `Location` 循环 + 上限 + 只允许 https 跳 https），
不要复用 API 的策略。

**代价**：极少数「中转用 302 做负载均衡」的配置会开始报错——但这恰恰是需要用户看见的事实。

**验证状态**：`读码确认`（对着 reqwest 0.13.5 源码）；`未实测`（没有真起一台会 302 的上游）。
**子结论未验证**：重定向是否把 POST 改写为 GET、是否丢弃请求体，本版 reqwest 的 `redirect.rs` 里
没有对应逻辑，本文不下断言——无论哪种，Key 泄漏的结论都成立。

## 3. 中高 / 中

### M1｜被截断的流，给客户端补的是「正常结束」

**现象**：流末的尾巴是「先补终态、再判罚」——`server.rs:1013-1040` 先调 `decode_stream_done`，
把产出的规范事件经 `encode_for_client` 发给客户端，再调 `encode_stream_done`；判罚与落库在
`server.rs:1044-1047`，**在尾巴之后**。

`decode_stream_done` 对没收到结束信号的流会补一条正常的终态：Anthropic 侧是
`state.finish("end_turn")`（`providers/anthropic_messages.rs:209-218`），产出
`message_delta` + `message_stop`（`providers/mod.rs:171-198`）。而判罚靠 `upstream_ended`
（`providers/mod.rs:211-228`）——它只有真正的结束信号才会置位。于是同一条流里：

- 落库：`StreamVerdict::Truncated` → 明细里是一条失败（诚实）；
- 线上：客户端收到完整的 `message_stop`（Anthropic 入站）/ `finish_reason:"stop"` +
  `usage` 分片 + `[DONE]`（Completions 入站，`openai_completions.rs:933-945`）/
  `response.completed`（Responses 入站，`openai_responses.rs:983-1020`）——**看起来答完了**。

对 Anthropic 入站尤其反直觉：`encode_stream_done` 在 Anthropic provider 里根本没重写
（默认实现 `providers/mod.rs:462` 返回空），但 `decode_stream_done` 补出的规范事件本身就会被
再编码成 `message_stop` 发出去——「补正常结尾」这件事在三个入站协议上都发生了。

**建议**：把判罚提到尾巴**之前**，让尾巴按判罚分支：

- `Ok` / `EmptyReasoningOnly`：照现在这样补终态；
- `Truncated` / `UpstreamError`：不补正常终态，改按入站协议发一条终止性错误——
  Anthropic `event: error`（`wire::stream_error_event` 已有）、Completions 直接断流且**不发 `[DONE]`**
  （客户端据此判定流不完整）、Responses 发 `response.failed`（`response.incomplete` 的兄弟事件）。
  文案里带上「上游未发结束信号」。

注意别把 `usage` 弄丢：`message_delta` 是携带累计 token 的那一条，截断路径要用错误事件之外的
一次 `usage` 承载（Anthropic 的 `error` 事件不带 usage；可以在错误事件之前先发一条只带 usage 的
`message_delta`，或接受截断时 usage 记为已知部分并只发错误——这一步要按客户端 SDK 的实际解析行为
定，属于**改动前需要先验证的点**）。

**代价**：截断路径的线上形状变了，需要三个协议各加一条守护测试；对「上游只是少发了一个
`[DONE]` 但内容完整」的中转，客户端会从「静默成功」变成「显式失败」——这是本条的**目的**，但对
用户是可见行为变化，值得在 release note 里写一句。

**验证状态**：`读码确认`。

### M2｜同协议直通 + 首帧前失败 = 空的 200

**现象**：直通时（`passthrough == true`）网关刻意不补任何事件，理由写在 `server.rs:980-983`：
「上游原文已经发出去了，补上去等于在客户端的流里伪造内容」。这个理由对**流中途**的错误成立，
但对**一个字节都还没发出去**的情况不成立——`Err` 分支（`server.rs:997-1006`）也走同一条判断：

```rust
Err(error) => {
    let message = error.to_string();
    snapshot().stream_error = Some(message.clone());
    if !passthrough {                       // ← 直通时连「首帧都没到」也保持沉默
        for event in error_events(inbound, &message) { … }
    }
    break;
}
```

于是「连上游 TCP 都没连上」或「首帧超时」（`sse.rs:98-104` 产出的那条 Err）在直通路径上表现为：
客户端拿到 `200 text/event-stream`，响应体随后**立即结束，0 字节**。它既没收到错误事件，也不知道
是超时还是断网。非直通路径则会发一条形状正确的 `event: error`——同一个网关，两条路径的排障体验不一样。

**建议**：在生成器里记一个「已经发过字节没有」的局部标志（`yield` 之前置位即可，不需要进
`Arc<Mutex>`——它只被生成器自己读写）；`passthrough && !emitted` 时按入站协议发错误事件。
直通时入站协议 == 上游协议，所以这个形状对客户端是原生形状，不存在「伪造别协议内容」的问题。

**代价**：极小；改动只在生成器内部，一条测试（构造一个建连失败或首帧超时的直通请求，断言
客户端收到的第一条事件是错误事件）。

**验证状态**：`读码确认`（代码路径清晰，但未构造直通失败场景跑过）。

### M3｜同步 SQLite 跑在 tokio worker 上，且是单连接

**现象**三件事叠在一起：

1. `db::with_conn` / `with_tx`（`db/mod.rs:157-168`）是**同步** rusqlite 调用，锁的是
   `OnceLock<Mutex<Connection>>`（`db/mod.rs:14`）——全进程一个连接、一把锁；
2. 调用点全在异步上下文里：`route()` 的失败分支（`server.rs:348`）、非流式成功落库、流末
   `StreamAccounting::record`（`server.rs:681-691`，跑在 SSE 生成器所在的 worker 上），以及
   **`DisconnectGuard::drop`**（`server.rs:715-732`）——drop 发生在哪个线程就在哪阻塞。工程里已经
   有 `spawn_blocking` 的用法（`gateway/mod.rs:449`），只是没用在落库上；
3. `summary()`（`usage.rs:396+`）走 `read_since`（`usage.rs:380-394`）：`SELECT … WHERE day >= ?1`
   把窗口内**所有行**读进 `Vec<UsageRecord>`，再在 Rust 里逐行累加 daily / by_model。跑一次「最近一年」
   就是一次全表读 + 一堆分配。

**影响**：一次前端大查询（或一次批量导入后的刷新）握着这把锁时，**网关的落库全部排队**；反过来，
落库的 fsync（WAL + `synchronous` 保持默认的 FULL，`db/mod.rs:22`）会占住一个 tokio worker。
worker 数量 = 核数，几条长事务就够把网关的吞吐压到零。这正是「面板一刷新，转发就卡顿」这类
症状的机制。

**建议**：

1. 落库全部走 `tokio::task::spawn_blocking`（`record_usage` / `record_with_payload` 包一层即可，
   注意 `DisconnectGuard::drop` 里**不能** await——那条路径改为 `spawn_blocking` 直接丢出去，
   丢掉 JoinHandle，用捕获到的快照做记录）；
2. 查询与写入分成两个连接（读连接 `OpenFlags::SQLITE_OPEN_READ_ONLY`），读不再挡写；
3. `summary()` 改成 SQL 聚合（`GROUP BY day` / `GROUP BY model_name` + `SUM`），只在 Rust 里拼结构；
4. WAL 下 `PRAGMA synchronous = NORMAL` 是通行做法（崩溃只丢最后几条记录，不损坏库），
   换回落库延迟。

**代价**：1 会引入「落库是异步的」这一事实，涉及记录顺序的测试要改成等一次 `spawn_blocking` 完成；
3 要重写查询但外部行为不变（现有测试可以照跑）。

**验证状态**：`读码确认`（锁与调用点）；`未实测`（没有实测「大查询把网关卡住」的量化影响）。

### M4｜三处无上限缓冲

| 位置 | 代码 | 风险 |
| --- | --- | --- |
| 非流式响应体 | `server.rs:816` `upstream.json::<Value>()` | 上游（或被重定向到的第三方，见 H3）返回超大 JSON → 整个 body 进内存再解析 |
| 上游错误体 | `server.rs:551` `response.text()` | 同上，非 2xx 时也没有上限（4xx/5xx 也能带 GB 级 body） |
| SSE 行/帧缓冲 | `sse.rs:33-67` `buffer` / `frame.raw` | 上游不吐 `\n`（或只吐 `data:` 不吐空行）→ `buffer` / `frame.raw` 无限增长 |

**影响**：本机小工具进程被上游 OOM 掉，网关线程随之死亡。触发条件是「上游坏/被劫持」，不是
「正常使用」，但在 H3 打开的情况下门槛并不高。

**建议**：

- 非流式响应：先看 `Content-Length`（有就拒绝超限值），再累加 `chunk().len()` 到上限
  （例如 32MB，与 H2 的上限同一量级）后报错；错误体上限可以小得多（1MB 足够）。
- `parse_sse_stream`：给**单行**和**单帧**各设上限（例如单行 1MB、单帧 8MB），超限 `yield Err(...)`
  走现有错误路径（记账 + 非直通报错）。这样也顺带保护了直通路径。

**代价**：直通路径一旦超限就不能再「原样转发」——只能报错中断，这与「直通不改流」的原则冲突，
需要在文档里写清「上限之内逐字转发」。

**验证状态**：`读码确认`。

### M5｜报文永不清理，且注释里已经假设了保留策略

**现象**：`PAYLOAD_MAX_BYTES = 256 * 1024`（`usage.rs:176`），一次请求最多写 4 个字段
（`inbound_request` / `inbound_headers` / `upstream_request` / `upstream_response`，
`usage.rs:255-279`，`server.rs:670-678`），最坏约 1MB。库是 WAL，没有任何删除、`VACUUM` 或
保留窗口的实现（`usage.rs` 里只有按模型/过滤条件重置那条线）。而 `payload_detail` 的注释
（`usage.rs:295`）已经写着：

> 按明细行号取报文详情；无报文记录（老数据或**已被保留策略清理**）返回 None。

注释描述的是一个不存在的机制。

**影响**：磁盘随用量单调增长，且增长的是最没用的那部分（报文全文）。长跑几个月后，磁盘和
`usage_detail` 的续读都会变慢。

**建议**：加一个启动 + 每日一次的清理任务：删掉早于 N 天的 `usage_payload` 行（**保留**
`usage_detail`，汇总与列表不受影响），随后按需 `VACUUM`（或设 `auto_vacuum`）。N 做成设置项
（默认比如 30 天）。这条同时让注释变成真的。

**代价**：小。注意 `usage_payload` 与 `usage_detail` 的关联删法（外键是 OFF，`db/mod.rs:22`，
所以要自己按 id 删），以及清理时机别和 H2/M3 的改动撞车。

**验证状态**：`读码确认`。

## 4. 低

- **L1｜Anthropic / Responses 解码丢帧**（`openai_responses.rs:405-416`、`anthropic_messages.rs` 的
  `decode_stream_event` 开头同名分支）：SSE 每帧的「事件名」有两处——`event:` 行与 `data` 里的
  `type` 字段。两个 provider 都是**信封优先**：`event` 非空就完全用它，只有 `event` 为空才回落
  `data.type`（Anthropic 侧再兜一个 `"message"`）。中转若发 `event: message` +
  `data.type: response.output_text.delta` 这类组合，帧会落进 `_ => {}`（`openai_responses.rs:520`）
  被静默丢掉。丢的可能是正文分片（客户端看到空白）、工具参数分片，或终止事件
  （`state.upstream_ended` 不置位 → 明细记成 Truncated，尽管答完了）。
  Completions provider 直接从 `data` 取字段、完全不看 `event`，不受影响。
  修法（**已落地**，见 §8）：把 `match` 改成返回 `Option<Vec<SseEvent>>`（Responses）/ `bool`
  （Anthropic）的形状——信封名匹配上就处理，没匹配上再拿 `data.type` 试一次，两次都不认识才丢。
  不认识的分支本来就无副作用（不改 `state`），所以这是纯加法，不可能影响今天已经正常的流。
  Anthropic 侧还有一处细节：认出来之后**事件名也要换成认出来的那个**——它的帧无论如何都要发给
  客户端，把不认识的信封名原样发过去，在 Anthropic SDK 眼里同样是丢帧。
  `读码确认`
- **L2｜`extract_token` 的大小写**（`server.rs:65-76`）：`trim_start_matches("Bearer ")` 区分大小写，
  而 RFC 7235 规定 scheme 不区分大小写（`bearer` / `BEARER` 都合法）；另外
  `Authorization: Basic …` 会被原样当成 token 记进「来源应用」字段。修法：切分后
  `eq_ignore_ascii_case("bearer")`，非 Bearer 的 scheme 返回 None。  
  `读码确认`
- **L3｜两套「回环」判定**（`providers/mod.rs:514-526` vs `560`；**已修**，见 §8）：`is_loopback_target`
  认 `localhost` / `*.localhost` / 解析得出的回环 IP；`NoProxy::from_string("localhost,127.0.0.0/8,::1")`
  是另一套实现。两处注释都写着「同源同义」，实际靠人肉保持同步。
  **勘误**：本条目初版写的是「`*.localhost` 只在其中一套里认」——对着 reqwest 内部真正做判定的那份
  实现（`hyper-util-0.1.20/src/client/proxy/matcher.rs` 的 `DomainMatcher::contains`，`515-541`）确认：
  域名条目本来就匹配它自己**与它的全部子域**，所以两边的**语义今天是等价的**，问题只在「各自实现
  一份、靠人肉对齐」。它影响的是 `request_proxies_through` 的**展示口径**与真实路由是否一致
  （明细里写「走代理」而实际直连，或反之）：今天是等价的，但只要有人在一边改了名单或规则，
  差异就从「没影响」变成「明细在说谎」。
  修法（已落地）：名单收成一个常量 `NO_PROXY_LIST`，路由侧交给 reqwest 的 `NoProxy`，展示侧用同一份
  名单 + 一份照 `hyper_util` 语义抄的匹配规则（那个 matcher 不对外公开，复用不了函数，只能对齐规则），
  另加守门测试挡住「往绕行名单里加非回环地址」。  
  `读码确认`（reqwest / hyper-util 源码）；`已实测`（规则表按 hyper-util 自带用例整理成测试）
- **L4｜安装包下载**（`commands/apps.rs:295-320`）：`get(url).send()` 没有请求级超时（H1 的
  `read_timeout` 顺带覆盖），且失败/取消时已经写了一半的文件留在安装目录，下次不清理也不复用。
  修法：下到临时文件名，成功后 `rename`。  
  `读码确认`
- **L5｜慢客户端反压**（`server.rs:940-1008`）：客户端读得慢 → 生成器不被 poll → 不再 poll 上游 →
  上游连接被一直攥着。这是**正确的**背压（不缓冲到内存里），问题是上限：客户端进程被冻住但不发
  FIN/RST 时，这条链可以永久保持，而入站侧同样没有 keepalive 与写超时。修法：给监听 socket 开
  `SO_KEEPALIVE`（`tokio::net::TcpSocket` + `socket2`），或明确接受现状并写进文档。不建议加总时长
  上限——会掐断合法的长生成。  
  `读码确认`（平台默认值 `未实测`）
- **L6｜没有请求 ID，也没有运行日志**（`server.rs:297-305`）：分三层看，价值差别很大。
  ① **完全没有日志设施**：`Cargo.toml` 里没有 `log`/`tracing`/`env_logger`，源码里除测试外没有一处
  `println!`/`eprintln!`——进程对 stdout 沉默，出事时没有实时线索。② **`events` 表写了但没有界面**：
  `events::log` 只被 6 处调用（`failover.rs:99/115/133`、`gateway/mod.rs:367/414`、
  `providers/mod.rs:565`），且都是**状态变更**（探测、切换、启停、代理无效）而非逐请求事件；
  后端有 `list_events`、前端有 `eventApi.list`（`src/lib/ipc.ts:74`），但**没有任何页面调用它**——
  这些审计记录目前只能翻库看。③ **每次请求的明细已经很全**（4 条报文 + 错误文案 + 耗时 + 是否
  代理 + 是否触发切换），事后复盘基本够用。
  真正缺的是两样**便宜**的东西：(a) 上游响应头**从不被读取**（全仓库只有安装包下载
  `install/sources.rs:296` 读 `headers()`），所以上游返回的 `x-request-id` / `request-id` 被丢掉——
  而这正是中转/官方支持要的那个号；(b) 自己这一侧的号：明细行的 `usage_detail_id` 是落库时才产生的
  自增号，请求进行中无法引用，也从不发给上游。
  建议拆两半：**便宜的一半**（入口生成短 id → 发给上游 + 写进明细；顺手接住上游的响应请求号；
  给现成的 `events` 表补一个列表页）留到 5e；**贵的一半**（引入日志库 + 落盘 + 轮转 + 实时视图）
  建议明确**不做**——单机工具里，把已有的明细与 events 接上界面比引入日志栈便宜得多。
  `读码确认`
- **L7｜重启宽限期的文案**（`gateway/mod.rs:17`、`322`）：停止/重启时 5s 后硬关在途流，
  `DisconnectGuard` 会把它们记成「客户端断开」（`server.rs:730`）——实际是网关自己关的。
  这会误导「为什么我的长生成断了」的排查。修法：给守卫一个可选的「关闭原因」，
  停止流程先置位再触发硬关。  
  `读码确认`

## 5. 已确认接受、本文不再重复提

- **C3（网关鉴权 / CORS 收紧）**：保持现状，2026-09-25 已与用户确认，两条备选路径的代价留档在
  `docs/forwarding.md` §8 批 4。`CorsLayer::permissive()` 与 `health` / `models` 免鉴权一并接受。
- **入站 header 原样入库、不脱敏**：既定决策（`server.rs:86-104` 的注释指向 §2），
  这意味着 `x-api-key` / `Authorization` 会连同报文一起落库——**与此相关的风险是 H3 与 M5**，
  而不是「该不该脱敏」。
- **单一上游 + 事后探测切换**、**流式不设总超时**、**同协议直通不补尾巴**、
  **`EmptyReasoningOnly` 记为失败**、**`Dropped` 对 Anthropic 自带的 `service_tier` 也生效**：
  均为已定案的取舍，见 `docs/forwarding.md` §5、§8。
- **流式明细里的 `upstream_response` 是重建件不是原文**
  （`server.rs:644-668` 把规范组装结果重新编码成上游协议形状）：展示用途，接受。

## 6. 证据与复现

- **H2**：临时在 `router()` 上加一条与 handler 同 extractor 的探针路由，用
  `router().oneshot(Request)` 发 1MB / 3MB / 12MB 三种 body（1MB 那条故意发坏 JSON 以确认上限
  确实是 2MB 而非别的数字），打印状态码、`content-type`、body。探针与临时路由已删除，
  `git diff --numstat` 对 `gateway/server.rs` 为空。
- **H3**：reqwest 源码取自本机 cargo registry 的 vendored 副本
  （`~/.cargo/registry/src/index.crates.io-*/reqwest-0.13.5/src/redirect.rs:160`、`:239`），
  与 `Cargo.lock` 锁定的版本一致。
- **其余条目**：`读码确认`，逐条附了 `file:line`。文中标 `未实测` 的都是「修法来自依赖文档/源码、
  未构造故障场景验证」的部分。

## 7. 建议的落地顺序（批 5 提案）

| 步 | 内容 | 理由 |
| --- | --- | --- |
| 5a | H3（`Policy::none()`）+ H1（`read_timeout`） | 各一行，风险与收益最不对称的两条；顺带覆盖 L4 的超时 |
| 5b | H2（抬高入站上限 + handler 内自检 + 落库） | 影响的是「图片请求根本发不出去」，用户可感知 |
| 5c | M1（判罚先于尾巴）+ M2（直通首帧失败补错误事件） | 两条都在改流末/错误分支，一起做，三条协议各加守护测试 |
| 5d | M5（报文保留策略）+ M3（落库 `spawn_blocking` / 只读连接 / SQL 聚合） | 都在 `usage.rs` + `db/mod.rs`，一次改完一起测 |
| 5e | M4（三处上限）+ L1–L7 | 收尾；L 组可择机 |

5a 可以立刻做，改动面最小、不需要动测试；5c 需要先定「截断时 usage 怎么给客户端」
（M1 里的待验证点）再动手。

**本轮的进度**（2026-09-25）：只做了 5b 的 H2 与 5e 里的 L1 / L3（见 §8）；5a（H1 / H3）与
5c / 5d、以及 M4 与 L 组的其余条目都没动，批次顺序照旧。

## 8. 本轮落地记录（2026-09-25）：H2 / L1 / L3

按用户指名落地三条（原话：「只修 L1 和 L3 和 H2」）。下面写清各自落成了什么样、测试在哪、
以及**有意没做**的部分。

### H2｜入站体积（`gateway/server.rs`、`error.rs`、`usage.rs`）

- **两层上限**：`MAX_INBOUND_BODY = 32MB`（逻辑上限，handler 内自检）+ `INBOUND_BODY_HARD_LIMIT = 64MB`
  （`DefaultBodyLimit::max`，axum 层）。32–64MB 之间的请求进得到 handler，拿到协议形状的 413 与一条
  失败明细；超过 64MB 仍由 axum 在 handler 之前用纯文本 413 挡掉（不落库、无 JSON 形状）。硬上限
  的那层写在 `CorsLayer` **之前**，让 axum 自己回的那个 413 也带 CORS 头。
- `AppError::PayloadTooLarge` → `413 request_too_large`（Anthropic 的正式错误类型；两个 OpenAI 协议把
  同一个词放进 `type` / `code`）。它走 `route()` 现成的失败落库路径——与「缺少网关 Key」「请求体不是
  合法 JSON」是同一条，所以「落一条明细、计入错误统计」是结构上带着的，不是另写一遍。
- **报文落库不再整份复制**：`inbound_request_text()` 只取 `PAYLOAD_MAX_BYTES + 1` 字节（多出来的那
  1 个字节是给 `cap_bytes` 判定 `request_truncated` 用的；`usage::PAYLOAD_MAX_BYTES` 因此升为
  `pub(crate)`）。改之前，一个 40MB 的请求会因为落库再复制出一份 40MB 的 `String`。
- 测试：`gateway::server::tests::inbound_body_check_rejects_only_above_the_cap`（边界 + 两个上限的
  大小关系）、`oversize_error_keeps_the_inbound_protocol_shape`（两个协议的错误信封）。
  端到端那条 `h2_oversize_request_is_rejected_and_recorded` **默认 `#[ignore]`**：它要往进程级 usage
  库写一条失败记录，全量跑会把 `sqlite_persistence_round_trips` 的「库里只有我这一条」断言顶掉
  （同一进程共用一个库——这正是 `StreamAccounting` 把写入做成可注入 `StreamWriter` 的原因）。
  单独跑并已跑过：`cargo test h2_oversize_request_is_rejected_and_recorded -- --ignored`
  → 40MB 请求得到 413 + 一条 `ok=false` 明细 + `request_truncated` 置位。

### L1｜事件名回落（`providers/{openai_responses,anthropic_messages}.rs`）

- Responses：`dispatch_event(name, data, state) -> Option<Vec<SseEvent>>`，`None` = 本协议不认识这个
  名字、且**一动 `state` 都没动**，所以调用方可以安全地拿 `data.type` 再试一次；两个候选都不认识
  才算真丢帧（与改动前一致）。
- Anthropic：`apply_event(name, data, state) -> bool`，同样的「不认识就不碰 `state`」约定；差别在于
  它的每一帧无论如何都要发给客户端，所以回落命中之后**事件名也换成认出来的那个**——把不认识的信封
  名原样发过去，在 Anthropic SDK 眼里同样是丢帧。两个都不认识时维持原行为（原样转发）。
- 测试：`responses_frames_fall_back_to_the_body_type_when_the_envelope_is_unknown`、
  `anthropic_frames_fall_back_to_the_body_type_when_the_envelope_is_unknown`（正例 + 「两个都不认识」的负例）。

### L3｜绕行判定收成一份（`providers/mod.rs`）

- `NO_PROXY_LIST` 一个常量：`build_client` 把它交给 reqwest 的 `NoProxy`（真正的路由），
  `is_loopback_target` 用同一份名单 + `no_proxy_matches`（照 `hyper_util` 的 matcher 语义对齐：
  IP / CIDR 按地址判，域名条目匹配它自己与全部子域，IPv6 的方括号剥掉）。
- 复用了多少、抄了多少，说清楚：reqwest 的 `NoProxy` 是不透明类型（只存字符串）、`hyper_util` 里那个
  真正做判定的 `NoProxy` 不是 `pub`，**函数没法复用**，所以匹配规则只能抄一份——抄的这一份由测试
  盯着（规则表直接照着 hyper-util 自带用例整理），名单也只有一份。
- 测试：`loopback_bypass_matches_reqwest_rules`、`no_proxy_list_only_covers_loopback`（守门：想把
  `10.0.0.0/8` 顺手加进绕行名单时会红，提醒那要连命名与展示口径一起重想）。

### 有意没做

- **H1（`read_timeout`）与 H3（`Policy::none()`）**：本轮明确未做，两条都还是一行级改动，方案与
  证据见 §2，随时可做。
- M1–M5 与 L2 / L4–L7：未动。
- H2 的**硬上限那一层没有测试**：要走 `router().oneshot` 灌一个 >64MB 的 body 才碰得到，而那条
  路径的收益只是「确认 axum 的兜底还在」——那是依赖行为，不是本仓库的契约。契约那两半（32MB 判定
  与 413 形状）都有测试。

# 转发优化方案

> 2026-09-25。针对「转发链路 if/else 多、同协议应免转换、模型切换应同协议优先、过滤器待扩展」
> 的方案文档。现状架构见 `forwarding-architecture.md`，协议对照见 `protocol-comparison.md`。
>
> 先说结论：**这套架构的 if/else 大头不在标量字段（那层已经被协议表收敛了），而在消息/工具/流事件
> 的 `Custom` 槽位与两个方向的「结构转换」。继续优化不是消灭分支，而是把分支从
> 「2×2 全组合」压到「每协议一份声明式描述」＋ 让同协议转发（请求、响应、流三处）走免转换快路。**
>
> **进度（2026-09-25）**：同协议快路的**请求侧（A1）已落地**，`cargo test` 120 passed；
> **响应/流侧（A2）未做**；§4 表里逐项标了状态。落地形态见 §1「落地形态」。

---

## 0. 现状量化：if/else 都在哪

| 文件 | 行数 | if/else | 分支的主要性质 |
| --- | --- | --- | --- |
| providers/openai_responses.rs | 1046 | ~81 | 结构映射：input items ↔ 块、事件名分发（必要复杂度） |
| providers/openai_completions.rs | 889 | ~70 | 结构映射：messages ↔ 块、分片 ↔ 块（必要复杂度） |
| providers/anthropic_messages.rs | 325 | ~34 | 透传补写 + 4 个变形字段（已最小） |
| providers/mod.rs | 476 | ~24 | StreamState/WireState/Assembler 状态机（必要复杂度） |
| gateway/server.rs | 701 | ~16 | 生命周期 + 落库 + 3 处协议分叉（已收敛） |
| providers/wire.rs | 240 | ~5 | 标量协议表（**已是目标形态**） |

关键判断：

1. **标量参数层已经解决了**。`wire.rs` 的 `Slot` 表 + `apply_common_fields` + 完备性测试，
   把「每个字段每协议怎么落」从代码分支变成了数据。这一层不要动，它是后续一切的地基。
2. **真正重复的分支在「结构字段」**：`messages`/`tools`/`tool_choice`/`response_format` 在
   `Slot::Custom` 下由每个 provider 手写拼装。三个 provider × 两个方向 = 每种结构 6 份手写逻辑，
   彼此用 JSON 取值器互相解析（`pointer("/function/name")` 这类），是 bug 温床（D1 的三个缺陷
   全部出在这里）。
3. **同协议转发被迫绕规范一圈**：OpenAI 入站 → 规范 → OpenAI 出站。**请求侧已改为真直通**
   （2026-09-25 落地，见 §1 的「落地形态」）；**响应与流侧仍重建**——分片 `id`/`created` 被重生、
   思考通道被改名。成因不是协议要求，而是「raw 必须是规范形状」这条内部不变式
   （只有 Anthropic 的报文天然满足它）——详见 `forwarding-architecture.md` §4.1。

---

## 1. 目标一：同协议免转换（方案 A「快路直通」）

> **落地状态（2026-09-25）**：**A1（请求侧）已实现并测试通过**（`cargo test` 120 passed）。
> **A2（响应与流侧）未做**——仍走重建路径，见下「A2」小节。

### 现状（A1 之前）

- Anthropic → Anthropic 已是真直通（raw 原样 + `Fill::IfAbsent` 补写）。
- Completions → Completions：`decode_request` 抬升 15 个字段重建 raw → `encode_request` 从
  `Map::new()` 重拼。丢扩展键（OpenRouter 的 `provider`/`models`/`route`，message 的 `name`，
  自定义 content part……），且 `stop` 往返变形。
- Responses → Responses：同样重建（`input` items ↔ 消息块 ↔ input items 两个方向白转）。

### 落地形态（A1，已实现）

与下面「方案」的设计意图一致，实现上把「raw 保留」换成了**另存一份客户端原文**——
`raw = 规范形状` 这条不变式必须保住（过滤器与协议表都依赖它），所以不改 `decode_request`
的输出，而是在网关入站处多留一份：

```rust
// gateway/server.rs::handle
let request = inbound_provider
    .decode_request(raw.clone())?
    .retain_client_raw(raw);                       // ← 客户端原文留在这里
```

```rust
// providers/mod.rs（trait 新增，默认返回 None = 无需快路）
fn encode_request_passthrough(&self, cfg, req) -> AppResult<Option<Value>> { Ok(None) }

// gateway/server.rs（唯一判定点，也是快路的可测入口）
pub(crate) fn encode_upstream_request(inbound, cfg, req) -> AppResult<Value> {
    if cfg.format == inbound {
        if let Some(payload) = provider_for(cfg.format).encode_request_passthrough(cfg, req)? {
            return Ok(payload);
        }
    }
    provider_for(cfg.format).encode_request(cfg, req)
}
```

- 两个 OpenAI provider 实现了快路：`client_raw` 为底 → 换 `model` → 客户端没写输出上限时才补
  `DEFAULT_MAX_TOKENS` → 仅当 `req.is_dirty("system")` 时重写 system 消息 → 删 `_canonical`。
- Anthropic 不实现（默认 `None`）：它的 `encode_request` 本身就是直通。
- 过滤器因此必须 `mark_dirty`（见 P0-a 的落地说明），否则注入在快路上静默失效。

### 方案（原设计，编码形态以「落地形态」为准）

把 Anthropic 透传模式推广到三个协议，统一为：

```
handle:
    raw = 客户端原文
    request = decode_request(raw.clone())        // raw 仍是规范形状，不变式不破
    request.client_raw = raw                     // 另存原文（实际实现）
encode_request_passthrough（同协议时）:
    payload = client_raw.clone()
    model 改写为上游模型 ID
    客户端没写输出上限时补 DEFAULT
    _canonical 整层删除
    仅当 is_dirty("system") 时按规范 body 重写 system 位置
encode_request（跨协议时）:
    走现有重建路径（不变）
```

选型理由（为什么不是「同协议完全绕过规范」）：

- **过滤器必须经规范**。提示词注入改 `system`，而 completions 的 system 在 messages 里、
  Responses 的在 `instructions` 里——只有规范层知道每个协议把它放哪。若 raw 直通不经规范，
  过滤器对 OpenAI 入站会失效（现在靠重建侥幸生效）。
- **模型名/别名改写也必须经规范**（`candidates_for` 按 body.model 选路）。
- 所以正确的快路是「**客户端原文 + 规范字段的显式覆盖**」：与 Anthropic 现状同构，只是
  「哪些字段被改过」不靠 `Fill::IfAbsent` 猜（无法区分「客户端给的」与「被改过的」），
  而是由过滤器显式登记在 `dirty` 集合里（实际实现的取法）。

### 前置改造（已落地）

**P0-a 过滤器改挂规范字段而非 raw 键名** ✅。`filters.rs` 之前直接改 `raw["system"]`——对
Completions 入站这会在 raw 顶层造出**伪 `system` 键**（OpenAI 请求没有这个顶层键）。今天无害
（重建路径无视 raw）。落地直通后，真正的失败模式是**反过来的**：快路以 `client_raw`（客户端原文）
为底，规范 raw 根本不参与直通，所以过滤器改了 raw 而不作声张，注入会**静默失效**——客户端原文里
没有那个改动，上游收到的还是老提示词。

改法（已实现）：`CanonicalRequest` 维护 `dirty: BTreeSet<String>`，过滤器套用后
`mark_dirty("system")`，快路据此决定要不要按规范 body 重写 system 位置
（`is_dirty("system")` → 三个 provider 各自的 system 落点：completions 的 `messages[0]`、
Responses 的 `instructions`、Anthropic 的顶层 `system`）。

**P0-b 补齐直通后的字段一致性** ✅。`protocol_profiles_cover_every_canonical_field` 继续守护
跨协议方向的完整性；新测试覆盖快路：同协议 encode 后除 `model`/`max_tokens`/被过滤字段外逐键等于
客户端原文（`tests.rs` 的 `same_protocol_passthrough_keeps_the_client_payload_intact`、
`gateway_keeps_same_protocol_payloads_and_rebuilds_across_protocols`、
`passthrough_rewrites_system_only_after_a_filter_changed_it`、
`responses_passthrough_keeps_client_extensions`、
`passthrough_falls_back_without_a_retained_client_payload`）。

### A1 落地点（已实现）

不是「每个 provider 拆出私有 `encode_passthrough`」，而是把它提成 trait 方法
`encode_request_passthrough`（`providers/mod.rs`，默认 `Ok(None)`）——判定点收在网关的
`encode_upstream_request(inbound, cfg, req)` 一处，三个协议共用同一条判定；
Anthropic 靠默认实现自然回退到它本就直通的 `encode_request`。

### A2：响应与流侧也直通（同协议时）—— **未落地**

只做请求侧收益有限——**今天同协议连响应和流也是重建的**，而且流侧的损失比请求侧更显眼：

| 位置 | 今天同协议往返造成的变化 |
| --- | --- |
| 响应（非流） | `system_fingerprint`/`logprobs` 丢失、`created` 重生、`refusal` 变纯文本 |
| **流** | **分片 `id` 由上游 `chatcmpl-…` 变成 `msg_<uuid>`、`created` 每片重生**（上游非透传时先合成 `message_start`） |
| **流** | **思考通道被改名**：上游发 `reasoning`（OpenRouter 系）→ 客户端收到 `reasoning_content`（读侧按 `reasoning_fields` 逐个尝试、另有 `reasoning_details[]` 兜底，写侧硬编码 `reasoning_content`） |
| 流 | `system_fingerprint`/`logprobs` 丢失、分片边界可能被 `BlockNormalizer` 合并、usage 尾片由网关合成而非透传 |

做法与 A1 同构，**但记账不能省**（usage、verdict、落库都靠解码侧）：

```
非流式（同协议）:
    decode_response  → 仅取 usage / 计算 verdict / 落库（不参与回包）
    yield 上游原始 body  // 需要时只改写 model 一行
流式（同协议）:
    parse_sse_stream(upstream)
      → 原始 (event,data) 直接 yield 给客户端（不再经 encode_stream_event）
      → 同时 tee 一份给 decode_stream_event 喂 StreamState/ResponseAssembler（记账与落库不变）
      → 流末仍需 wire_state 补齐 usage（客户端要 include_usage 时上游自己会给；
         网关只在缺 usage 时补，作为兜底）
```

要点：
- **`BlockNormalizer` 不参与同协议路径**。它存在是为了跨协议规整；同协议下发过来的分片本来就是合法
  的（客户端协议自己认自己），绕开它正好恢复「分片边界与 id 原样」。
- **`emit_initial` 的合成 `message_start` 在同协议下必须关掉**（现在它由
  `!upstream_provider.is_passthrough()` 决定，而不是由「入站协议 == 上游协议」决定）——
  这是 D2 在流侧的同一个根因：判定依据用错了维度。
- 跨协议路径完全不变（仍然 decode → 规整 → encode）。

`is_passthrough()` 的语义建议从「这个 provider 的 raw 是透传的」改为
「入站协议 == 上游协议」，让三个协议共享同一条快路判定；Anthropic 的现状是这个谓词的特例。

### 效果

| 方向 | 原状 | A1 后（现状） | A2 后（待做） |
| --- | --- | --- | --- |
| Anthropic → Anthropic | 真直通 | 不变 | 不变 |
| Completions → Completions | 请求/响应/流三处重建 | **请求真直通**（扩展键、`stop`、`logit_bias` 保真）；响应/流仍重建 | 三处真直通 |
| Responses → Responses | 同上 | **请求真直通**（`include`/`truncation`/`previous_response_id` 保真）；响应/流仍重建 | 同上 |

请求侧直通后新增保真的项：OpenRouter 的 `provider`/`models`/`routes`/`transforms`、message 的
`name`、`logit_bias`/`seed`/`frequency_penalty` 等采样参数、客户端自己的 `stop` 写法、
Responses 的 `previous_response_id`/`include`/`truncation`、同协议的 `tool_choice`；
`max_tokens` 默认值只在客户端确实没给输出上限时才补。

---

## 2. 目标二：模型自动切换时同协议优先

### 现状

`settings.candidates_for()` 的候选顺序 = 当前模型优先，其余按列表顺序，**与协议无关**。
入站协议与上游协议不同时必须全量转换；相同协议明明可以直通（目标一之后），却可能排在后面。

### 方案：给候选排序加一层协议亲和（不动切换语义）

```rust
// settings.rs
pub fn candidates_for(&self, requested: Option<&str>, inbound: ModelFormat) -> Vec<ModelConfig> {
    // 1. 别名/auto/未命中 → 候选 = candidate_models()
    // 2. 稳定分组：同协议候选排前（组内保持原顺序），异协议排后
    // 3. 指定显示名 → 仍然只该一个模型（协议不影响这个语义）
}
```

要点：

- **只在「别名/未命中」分支生效**。用户显式点名模型时不重排（既有测试
  `candidates_for_routes_aliases_to_the_usual_logic_and_named_models_to_themselves` 的语义保留）。
- **组内顺序稳定**：不引入「协议优先级」概念，同协议只是提前，先后仍由用户列表顺序决定，
  排序用稳定 partition 即可，行为可预测、可测试。
- 落库的 `failover` 记录已有 `inbound_protocol` / `upstream_protocol` 两列，
  事后可以从明细统计「因协议亲和而免于转换」的比例，验证收益。
- 文案与事件不变：`model.failover` 事件照旧，只是接手方更可能是同协议模型。

边际收益说明：转换本身是纯内存 JSON 变换（µs 级），同协议优先的真实收益**不是省 CPU**，而是：

1. **保真**：同协议直通后（目标一），排序让更多请求实际走上保真路径；
2. **行为一致性**：故障切换前后客户端看到的字段形状不变（从「转换后的近似」变成「原样」）。

### 测试

```rust
// 入站 completions，当前模型 anthropic，列表里还有另一个 completions 模型
// 期望：candidates = [当前? 同协议们..., 异协议们...]，当前模型仍第一
```

---

## 3. 目标三：结构字段的手写 if/else → 声明式（方案 B「结构协议表」）

这是对「if else 很多」的正面回答。标量已经表驱动了，把**结构映射也表驱动化**。

### 3.1 现状问题（为什么 Custom 槽位是最大分支源）

每个 provider 手写的结构逻辑分四类，每类 × 3 协议 × 2 方向 ≈ 18 份：

| 结构 | 逻辑 | 散落处 |
| --- | --- | --- |
| 消息 ↔ content blocks | role 映射、tool_result 拆分、system 位置 | completions decode/encode、responses decode/encode、anthropic 透传补写 |
| tools / tool_choice | 三种形状互转（Anthropic `{type:tool,name}` / CC `{type:function,function:{name}}` / Resp `{type:function,name}`） | 5 个函数，**已发现 3 个缺陷（D1）** |
| response_format | completions 形 ↔ `text.format` ↔ `output_config.format` | 4 个函数 |
| 流事件 | 事件名分发、item 开闭、索引换算 | 两个 OpenAI provider 的 encode/decode_stream |

这些函数互相之间用裸 JSON 取值（`get("function").and_then(...)` 链），没有类型约束，
「漏了一个形状」编译期无感、测试没覆盖就上线（D1 即实证）。

### 3.2 方案：强类型中间形 + 单一转换核

**第一步（必做，止血）：给三个「形状分歧字段」上强类型**

把 `tool_choice` / `response_format` / 工具定义从 `Option<Value>` 换成规范枚举：

```rust
// domain/canonical.rs
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalToolChoice {
    Auto,
    None,
    Required,
    Tool { name: String },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CanonicalResponseFormat {
    Text,
    JsonObject,
    JsonSchema { name: String, schema: Value, strict: Option<bool> },
}
```

- 每协议只写 **一对** `fn to_canonical(wire: &Value) -> Option<Canonical…>` /
  `fn from_canonical(c: &Canonical…) -> Option<Value>`；
- `Some/None` 显式表达「这个形状我能不能转」，转换不出的形状在 `decode_request` 阶段就有机会
  走 `validate()` 拒绝（可选：对无法转换的 tool_choice 直接 400，替代静默降级为 auto）；
- serde 的 `untagged/tag` 让非法形状在入站即报 400，而不是转发到上游才炸。

这一步直接修掉 D1 的三个缺陷，且把 5 个互转函数收敛成 3 组有类型边界的转换器，
`canonical_fields_are_forwarded_per_protocol` 补上 tool_choice 样例后三方向全被守护。

**第二步（可选，收益递减）：结构编解码器 trait 化**

流事件与消息块保持现状（它们本质是状态机，表驱动化收益低、可读性代价高——
`openai_responses` 的 item 开闭逻辑强行改成表只会更难读）。
只把「消息列表 ↔ 规范消息」这一块抽成独立模块：

```
providers/structure/
  messages.rs     // canonical Message[] ↔ 每协议 messages/input items（纯函数，双向）
  tools.rs        // CanonicalToolDef/ToolChoice ↔ 每协议形状
  format.rs       // CanonicalResponseFormat ↔ 每协议形状
```

每个文件 ≤200 行、纯函数、无状态，单测可以直接表驱动穷举。provider 的
`encode_request`/`decode_request` 退化为「调结构转换器 + 标量表 + 协议专属包装」三层，
预计两个 OpenAI provider 各减 150–250 行。

### 3.3 明确不做的

- **不引入 trait 泛化的「协议描述 DSL」**（类似把整个协议写成配置）。三个协议是有限集，
  状态机部分（流式块）无法声明式化，强行 DSL 会同时失去类型检查与可读性。
- **不动 `BlockNormalizer`**。它是全链路正确性核心，不变式已被测试锁死，现在的形式就是最优的。

---

## 4. 落地顺序与工作量

| 序 | 事项 | 规模 | 依赖 | 状态 |
| --- | --- | --- | --- | --- |
| 1 | **D1 修复**：`CanonicalToolChoice` 强类型 + 三协议转换器补全 + 测试补 tool_choice 样例 | ~1 天 | 无 | 未做（同协议方向已随 A1 顺带修好 ①） |
| 2 | **D3/P0-a**：过滤器改写挂规范字段集合（直通前置） | ~0.5 天 | 无 | **已落地** |
| 3 | **方案 A**：A1 请求侧（`client_raw` + `encode_request_passthrough` + `encode_upstream_request` 判定）、A2 响应/流侧（tee 记账 + 原样转发，`is_passthrough` 改判「入站协议 == 上游协议」）；新增同协议保真测试 | A1 ~2 天 + A2 ~1 天 | 依赖 2 | **A1 已落地**（2026-09-25，120 测试通过）；**A2 未做** |
| 4 | **目标二**：`candidates_for` 加 `inbound` 参数做稳定协议分组 | ~0.5 天 | 建议在 3 后（收益才显现） | 未做 |
| 5 | **D4**：dispatch 加请求级超时（建议 300s，流式只限建连+首字节） | ~0.5 天 | 无 | 未做 |
| 6 | **D5**：`validate()` 改 `n != 1` 拒绝；**D6**：合并双发 `decode_stream_done` | ~0.5 天 | 无 | 未做 |
| 7 | （可选）**方案 B 第二步**：structure/ 模块抽取 | ~2–3 天 | 依赖 1 | 未做 |
| 8 | 文档同步：protocol-comparison.md §3/§8 按审核记录修订 | ~0.5 天 | 随 1、3 | 未做 |

1–2 先行（修复正确性缺陷），3 是本次优化的主体，4 顺带完成。全部落地后：

- 同协议转发 = 零转换零丢失（三协议对齐 Anthropic 现状）；
- 跨协议转发 = 规范层全量保真转换，形状分歧字段全部类型化、测试穷举；
- 自动切换在同协议可用时优先同协议（显式指定模型的行为不变）；
- if/else 总量下降约 300–400 行，但**真正的收益是分支从 18 份手写互转收敛到 3 组类型化转换器 +
  1 张标量表 + 1 条直通快路**——新增第四个协议时，工作量 = 一张协议表 + 一个 provider 文件 +
  三组结构转换器，不再有散落的 JSON 取值链。

---

## 5. 风险与回滚

- **方案 A 改变同协议线上报文形状**（扩展键开始透传；请求侧已按此实现，**未加开关**——
  判定条件 `config.format == inbound` 是硬边界，跨协议仍走重建）。回滚 = 让
  `encode_upstream_request` 不调 `encode_request_passthrough`（一行），或在 provider 的快路里
  返回 `Ok(None)`。若要灰度，可在 settings 加 `passthrough_same_protocol: bool`（默认开）
  并把它并进那个 `if`。观察面：明细页的 `upstream_request` 报文。
- **A2 的 tee 记账不能漏**：旁路喂 `StreamState`/`ResponseAssembler` 的那一份必须与转发同步推进，
  否则 usage/verdict/落库会静默退化。落地时先加回归测试：同一段上游 SSE，直通路径与非直通路径
  落库的 usage 一致、verdict 一致。
- **协议亲和排序**改变故障切换顺序。保留在 `candidates_for` 单函数内，回滚 = 去掉 partition 一行。
- **`CanonicalToolChoice` 收紧形状**后，极端私有扩展（如带额外键的对象）会从「降级 auto」变成
  「入站 400」。落地时对未知形状保留 `#[serde(other)]` 兜底臂 + 明细记 `ok=0` 说明降级，
  与 EmptyReasoningOnly 的处理风格一致。

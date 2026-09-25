# protocol-comparison.md 审核记录

> 审核日期：2026-09-25。方法：把文档逐节与当前代码对照（`providers/wire.rs`、三个 provider、
> `gateway/server.rs`、`domain/canonical.rs`、`tests.rs`），字段级核对协议表三张 profile。
> 结论先行：**文档总体准确度高**——第 3 节字段表与 `wire.rs` 三张协议表逐槽位一致，停止原因表、
> usage 口径、流式事件表、第 9 节代码索引全部与代码吻合。发现 **4 处需要修正、3 处需要补充**，
> 其中 tool_choice 一条对应代码里的真实缺陷（不只是文档措辞问题）。

---

## 1. 需要修正的条目

### 1.1 §3 tool_choice 行「出入两向都转换」不成立（对应真实代码缺陷）⚠️

文档第 81 行写：`tool_choice`（规范用 completions 写法，出入两向都转换）。实际三条转换路径有缺陷：

| # | 路径 | 现状 | 证据 |
| --- | --- | --- | --- |
| ① | **Completions 入站 → 任何上游** | `openai_completions::decode_request` 的规范字段抬升列表（`providers/openai_completions.rs:569-584`）里**没有 `tool_choice`**，客户端的 tool_choice 被静默丢弃——这是最常用的入站协议 | `decode_request` 只抬升 temperature/top_p/stream/store/metadata/response_format/parallel_tool_calls/reasoning_effort/service_tier/n |
| ② | **Responses 入站 → Completions 上游** | `encode_tool_choice`（`providers/openai_completions.rs:99-114`）只有字符串臂和 `"tool"`（Anthropic 形）臂，**没有 `"function"` 对象臂**；规范里的 completions 形 `{type:function,function:{name}}` 落进 `_ => "auto"`，强制指定工具退化成 auto | `canonical_fields_are_forwarded_per_protocol` 测试（tests.rs:900）没覆盖 tool_choice，所以测试全绿 |
| ③ | **Anthropic 入站 → Responses 上游** | Anthropic 形 `{type:"any"}` 经 serde 原样进规范；`openai_responses::tool_choice`（`providers/openai_responses.rs:104-124`）只有 `function`/`tool` 两臂，`any`/`required` 落 `_ => None`，强制调用被静默丢弃 | 同上 |

测试没抓到的原因：`canonical_fields_are_forwarded_per_protocol` 的入站样例不含 tool_choice。

**修订建议**：§3 该行改为「规范用 completions 写法；**当前实现仅部分方向可靠**，缺陷清单见
`forwarding-optimization.md` D1」。修复方案见优化文档 D1（引入 `CanonicalToolChoice` 强类型枚举）。

### 1.2 §8 「Chat Completions → Chat Completions 零损失（同名直通）」表述过强

`openai_completions::encode_request` 不是直通——它从 `Map::new()` 重拼报文
（`providers/openai_completions.rs:218-258`），规范里有的字段才写；而丢得更早——
`decode_request` 只把抬升列表里的字段搬进规范（`providers/openai_completions.rs:569-584`），
列表之外的键在入站第一步就被丢弃。客户端报文里**规范之外的扩展键会静默丢失**，例如：

- OpenRouter 的 `provider` / `models` / `transforms` / `route` 等路由扩展；
- message 上的 `name` 字段；
- 自定义 content part 类型。

Responses → Responses 同理（`openai_responses::encode_request` 同样重建）。只有
Anthropic → Anthropic 是真直通（`raw` 原样 + `Fill::IfAbsent`）。

**修订建议**：§8 该格改为「**语义等价重建**：规范内字段无损（含 `max_completion_tokens` 保真），
协议扩展键静默丢失」。这也是优化文档方案 A（同协议直通）要解决的问题。

### 1.3 §9 测试名笔误

`block_normalizer_kesps_payloads_in_their_own_block` → 实际是
`block_normalizer_keeps_payloads_in_their_own_block`（tests.rs:716）。

### 1.4 §3 多模态输入行「base64 / URL」范围过宽

入站方向只接受 **data: URL**：`data_url_to_source`（`providers/openai_completions.rs:838-846`）
只解析 `data:` 前缀，`https://...` 图片 URL 在 completions / Responses 入站被静默跳过。
出站方向（两个 OpenAI provider 的 encode）才同时支持 base64 与 URL。

**修订建议**：该格注明「入站仅 data:URL；出站支持 base64 / URL」。

---

## 2. 需要补充的条目

### 2.1 「显式丢弃」的保证边界

§3 脚注说「显式丢弃的字段都写在协议表里，新加字段忘了映射测试会红」——这对 **`RequestBody` 的字段**
成立（`protocol_profiles_cover_every_canonical_field` 用完整字面量构造，漏字段编译不过）。
但客户端报文里**规范之外的键**走重建路径时是**隐式丢弃**，没有任何测试守护（见 1.2）。
两个口径建议在 §3 脚注里分开写清。

### 2.2 `validate()` 只挡 `n > 1`

`CanonicalRequest::validate()`（`domain/canonical.rs:267-274`）条件是 `n.is_some_and(|c| c > 1)`，
`n: 0` 会透传（上游大概率 400）。建议改成 `n != 1` 时拒绝。

### 2.3 dispatch 无总超时

`http_client()`（`providers/mod.rs:468-476`）只设了 `connect_timeout(20s)`，`dispatch`
没有请求级超时。上游建连成功后挂起（不回包也不断开）会让请求无限滞留，且流开始前
故障切换不会触发。`translate_text` 有 120s 超时，网关主路径反而没有。

### 2.4 过滤器改写的是 `raw`（做直通前必须先改这里）

`filters::apply_rule` → `apply_system_prompt`（`filters.rs:118-148`）直接改 `raw["system"]`。
对 Completions 入站这会在 raw 顶层造出一个**伪 `system` 键**（OpenAI 请求的 system 在 messages 里），
如今靠「重建路径无视 raw 顶层 system」侥幸无害；一旦按优化文档方案 A 做同协议直通，
这个伪键就会漏到上游。方案 A 的前置改造：过滤器改为改写 canonical 并记录变更字段集合。

---

## 3. 核对无误的条目（抽样）

| 文档条目 | 核对结果 |
| --- | --- |
| §3 三张字段表的全部 Slot（Same/Renamed/Custom/Transformed/Dropped） | 与 `wire.rs` ANTHROPIC_RULES / COMPLETIONS_RULES / RESPONSES_RULES 逐条一致 |
| §4 流式事件对照 + 「BlockNormalizer 收成交错块」 | 与 `normalizer.rs` 不变式 I1–I5 一致 |
| §5 思考通道三协议口径 + `reasoning_details[]` 兜底 + EmptyReasoningOnly | 与 `openai_completions.rs:15-39`、`mod.rs:211-228` 一致 |
| §6 停止原因表 | 与 `wire.rs:183-240` 四个映射函数一致 |
| §7 usage 口径（prompt_tokens 含缓存并回、Anthropic 口径拆分） | 与两个 OpenAI provider 的 decode/encode、`server.rs` 落库补齐逻辑一致 |
| §8 损失矩阵其余各格 | 与各 provider 的 encode/decode 实际行为一致（除 1.2 外） |
| §9 代码索引文件路径 | 全部存在，职责描述准确 |

---

## 4. 修订操作建议

以上 1.1–1.4 是对 `protocol-comparison.md` 的定点修订，2.1–2.4 是新增脚注/注记。
可以按审核记录直接改文档；其中 1.1 对应的代码缺陷建议随优化文档 D1 一并修复，
修复后把 §3 该行改回「出入两向都转换」。

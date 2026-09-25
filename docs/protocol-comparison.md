# 三种协议对照：Anthropic Messages / OpenAI Chat Completions / OpenAI Responses

> 这份文档回答两件事：**三个协议谁的表达能力最全**，以及**本网关的规范层怎么取长补短**。
> 官方文档链接见第 2 节，字段口径按 2026-09-24 抓取的官方文档整理。

---

## 1. 结论：没有哪个协议是全维度最全的

| 维度 | 最全的协议 | 依据 |
| --- | --- | --- |
| **内容块 / agent 语义** | **Anthropic Messages** | 唯一把「思考」做成一等公民内容块（`thinking` 带 `signature`、`redacted_thinking`、`signature_delta`），`tool_result` 里还能嵌文本/图片/文档块，`server_tool_use` 一族（web_search / code_execution / memory / text_editor）直接是协议的一部分；`stop_reason` 有 7 种（含 `pause_turn`、`refusal`、`model_context_window_exceeded`）；`usage` 明确区分缓存读/写；SSE 事件语义清晰（`content_block_start/delta/stop` 三件套）。 |
| **模型控制面（采样与请求参数）** | **OpenAI Chat Completions** | 参数最多：`n`、`presence_penalty`/`frequency_penalty`、`logit_bias`、`logprobs`/`top_logprobs`、`seed`、`response_format`（含 json_schema）、`modalities`/`audio`、`prediction`、`verbosity`、`web_search_options`、`service_tier`、`reasoning_effort`。 |
| **能力面 / 有状态** | **OpenAI Responses** | 唯一有状态（`store` + `previous_response_id` + `background`），唯一有结构化推理项（`reasoning` item + `encrypted_content`）、`include` / `truncation` / `max_tool_calls`，内置工具最多（web_search / file_search / code_interpreter / computer_use / image_generation / mcp / shell / apply_patch…），输出格式是强类型 item 列表。 |

**反过来看短板**：

- **Anthropic Messages** 控制面最少：没有 `n`、penalties、`logprobs`、`seed`、音频/图像**生成**、内置工具要按版本号挑；`temperature`/`top_p`/`top_k` 在新模型上已被废弃（非默认值会 400）。
- **Chat Completions** 的「思考」不是官方字段：官方只在 o 系列返回 `completion_tokens_details.reasoning_tokens`（**只有计数、没有正文**），正文走 `reasoning_content` / `reasoning` 是 DeepSeek 原生与 OpenRouter 系各家的私有扩展；也没有缓存断点控制（缓存是自动的），`finish_reason` 只有 5 个。
- **Responses** 最重，且它的状态类能力（`store`/`previous_response_id`/`background`）网关**无法保真代理**（会把同一个上游会话切碎）。

### 本项目的取法

规范层（`domain/canonical.rs`）= **Anthropic Messages 的内容块模型**（照抄它的 `content` 块、`message_start/delta/stop` 事件序列）
**＋ OpenAI 系的参数命名**（`store` / `metadata` / `response_format` / `reasoning_effort` / `service_tier` / `parallel_tool_calls` / `n` / `stream_options.include_usage`）。

理由：翻译的方向是「上游的任意形状 → 规范 → 客户端的任意形状」，而**块结构**是最难无损转换的部分（思考、工具、并行工具、块开闭顺序），Anthropic 的块模型恰好最完整、约束最明确；而**标量参数**只是改个名字，用 OpenAI 的名字更省事（两家 OpenAI 系协议还能同名直通）。所以：**拿 Anthropic 当骨架，拿 OpenAI 当参数命名表**。

**在我们网关里谁最无损**：`Anthropic → Anthropic`（恒等直通，零损失）> `OpenAI → OpenAI`（同名直通，只丢 `top_k`）> 其余组合（见第 8 节的损失矩阵）。

---

## 2. 官方文档

### Anthropic Messages

- **创建消息（POST /v1/messages）**：<https://docs.claude.com/en/api/messages/create>
  （另有同源的 `platform.claude.com` 镜像：<https://platform.claude.com/docs/en/api/messages/create>）
- **流式**：<https://docs.claude.com/en/api/messages-streaming>
- **停止原因**：<https://docs.claude.com/en/build-with-claude/handling-stop-reasons>
- **扩展思考（thinking）**：<https://docs.claude.com/en/build-with-claude/thinking>
- **提示缓存**：<https://docs.claude.com/en/docs/build-with-claude/prompt-caching>
- **错误码**：<https://docs.claude.com/en/api/errors>
- **API 总览（含版本头 `anthropic-version: 2023-06-01`）**：<https://docs.claude.com/en/api/overview>

### OpenAI Chat Completions

- **创建补全（POST /v1/chat/completions）**：<https://platform.openai.com/docs/api-reference/chat/create>
  （该地址现已重定向到 <https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create>）
- **流式分片说明**：<https://platform.openai.com/docs/api-reference/chat/streaming>
- **Chat 资源模型（`ChatCompletionChunk` / `delta` / `tool_calls` / logprobs）**：<https://developers.openai.com/api/reference/resources/chat>
- **`CompletionUsage`（含 `prompt_tokens_details` / `completion_tokens_details`）**：<https://developers.openai.com/api/reference/resources/completions>

### OpenAI Responses

- **创建响应（POST /v1/responses）**：<https://platform.openai.com/docs/api-reference/responses/create>
  （重定向到 <https://developers.openai.com/api/reference/resources/responses/methods/create>）
- **流式事件清单**：<https://platform.openai.com/docs/api-reference/responses-streaming>
  （重定向到 <https://developers.openai.com/api/reference/resources/responses>，事件表在 `.../responses/streaming-events`）
- **Responses 资源模型（output items / status / usage）**：<https://developers.openai.com/api/reference/resources/responses>

---

## 3. 请求字段对照

`—` = 该协议没有这个能力；「网关」列是规范层的落地方式（见 `providers/wire.rs` 的协议表）。

| 能力 | Anthropic Messages | OpenAI Chat Completions | OpenAI Responses | 网关（规范字段 → 各协议） |
| --- | --- | --- | --- | --- |
| 模型 | `model` | `model` | `model` | 三个都用 `model`；转发时替换成上游模型 ID |
| 会话 | `messages`（块数组，可比 string） | `messages`（扁平，`content` 字符串或 parts） | `input`（字符串或 item 数组） | `messages` / `system`；按入站协议归一 |
| 系统提示 | `system`（字符串或文本块数组） | `messages` 里 `role: system` / `developer` | `instructions` | `system` |
| 输出上限 | `max_tokens`（必填） | `max_tokens`（**已弃用**）/ `max_completion_tokens`（o 系列必须用这个） | `max_output_tokens` | `max_tokens` + `_canonical.max_tokens_field` 记住客户端原本用哪个字段，回写同名 |
| 采样 | `temperature` / `top_p` / `top_k`（后三者在新模型上已废弃） | `temperature` / `top_p` / `n` | `temperature` / `top_p`（无 `n`） | `temperature` / `top_p` / `top_k`（`top_k` 只给 Anthropic；`n>1` 直接 400） |
| 惩罚项 / 采样偏置 | — | `presence_penalty` / `frequency_penalty` / `logit_bias` | — | **不支持**（显式丢弃） |
| 可复现 / 对数概率 | — | `seed` / `logprobs` / `top_logprobs` | — （`include` 里有 `message.output_text.logprobs`） | **不支持**（显式丢弃） |
| 停止序列 | `stop_sequences` | `stop` | — （Responses 没有对应参数） | `stop_sequences` → completions 改名 `stop`；Responses 丢弃 |
| 流式 | `stream` | `stream` | `stream` | 同名 |
| 流式带 usage | 天然带（`message_delta.usage`） | 需 `stream_options.include_usage`，否则只有计数不给 usage | 天然带（`response.completed` 里） | `_canonical.include_usage`；转发 `stream_options` 并在流末尾补一个 `choices: []` 的 usage 分片 |
| 工具 | `tools`（可带 `cache_control` / `strict` / `input_examples` / 版本化服务端工具） | `tools[{type:function\|custom}]` | `tools`（function/custom + 8 种内置工具） | `tools` → 各自形状；服务端内置工具**不支持** |
| 工具选择 | `tool_choice: {type:auto\|any\|none\|tool}` | `auto` \| `none` \| `required` \| `{type:function,function:{name}}` | `auto` \| `none` \| `required` \| `{type:function,name}` | `tool_choice`（规范用 completions 写法，出入两向都转换） |
| 并行工具 | `tool_choice.disable_parallel_tool_use`（**取反**） | `parallel_tool_calls` | `parallel_tool_calls` | `parallel_tool_calls`；转 Anthropic 时取反写进 `tool_choice` |
| 结构化输出 | `output_config.format`（仅 json_schema） | `response_format`（text / json_object / json_schema） | `text.format`（同名三档，json_schema 摊平一层） | `response_format`（统一用 completions 形状）；→ Responses 转 `text.format`，→ Anthropic 只转 json_schema |
| 思考档位 | `thinking`（enabled/adaptive/disabled + `budget_tokens`）+ `output_config.effort` | `reasoning_effort`（none/minimal/low/medium/high/xhigh/max） | `reasoning: {effort, summary}` | `reasoning_effort`；Anthropic 的 `output_config.effort` 抬进来，出站按协议落到 `reasoning_effort` / `reasoning.effort` / `output_config.effort` |
| 输出详略 | — | `verbosity` | `text.verbosity` | **不支持**（显式丢弃） |
| 留存 / 检索 | 无（无状态） | `store` | `store` + `previous_response_id` + `background` | `store` 透传；Responses 的状态类字段**不支持** |
| 元数据 | `metadata.user_id`（只有这一个键） | `metadata`（≤16 对字符串） | `metadata`（同上） | `metadata`；转 Anthropic 时只投影 `user_id` |
| 服务档位 | `service_tier` | `service_tier` | `service_tier` | 同名（取值集合略有差异） |
| 缓存 | `cache_control`（显式断点，`ttl: 5m\|1h`）+ `cache_control` 顶层 | 自动缓存（无控制参数） | 自动缓存 + `prompt_cache_key` | 只对 Anthropic 方向有效（透传时保留）；其余丢弃 |
| 多模态输入 | `image` / `document`（PDF、纯文本）/ `file_id` | content parts（`image_url`、音频、`file`） | `input_image` / `input_file`（`file_id`/`file_url`/`file_data`） | 只归一图片（base64 / URL），文档与音频**不支持** |
| 多模态输出 | — | `modalities` + `audio` | `image_generation` 工具 | **不支持**（显式丢弃） |
| 会话态 / 截断策略 | `container`、`mcp_servers`(beta)、`inference_geo` | — | `truncation`、`include`、`max_tool_calls` | **不支持**（显式丢弃） |
| 其它 | `cache_control`、beta 头 `anthropic-beta` | `user`(弃用) / `safety_identifier`、`prompt_cache_key`、`prediction`、`web_search_options`、`service_tier` | `safety_identifier`、`prompt_cache_key` | 除 `service_tier` 外**不支持**（显式丢弃） |

> 网关侧「显式丢弃」的字段都写在 `providers/wire.rs` 的协议表里，并由
> `tests::protocol_profiles_cover_every_canonical_field` 保证「规范字段 = 已映射 ∪ 显式丢弃」，
> 新加字段忘了映射会让测试红。

---

## 4. 流式事件对照

| 阶段 | Anthropic Messages | OpenAI Chat Completions | OpenAI Responses |
| --- | --- | --- | --- |
| 开始 | `message_start`（一条完整 message 骨架 + usage） | 分片 `delta: {role: "assistant"}` | `response.created` → `response.in_progress` |
| 内容 | `content_block_start` → `content_block_delta`* → `content_block_stop`（每个块一对） | 每个分片 `choices[0].delta`（`content` / `reasoning_content` / `tool_calls`） | `response.output_item.added` → `response.content_part.added` → `response.output_text.delta`* → `response.output_text.done` → `response.output_item.done` |
| 思考 | `thinking_delta` + `signature_delta`（在块内） | 私有扩展字段（见第 5 节） | `response.reasoning_summary_text.delta` / `response.reasoning_text.delta` |
| 工具参数 | `input_json_delta`（同一个 `tool_use` 块内增量） | `tool_calls[i].function.arguments` 增量（i 可能交错） | `response.function_call_arguments.delta`（按 `output_index` 区分） |
| 结束 | `message_delta`（`stop_reason` + 累计 usage）→ `message_stop` | 带 `finish_reason` 的分片 → 可选 usage 分片 → `data: [DONE]` | `response.completed` / `response.incomplete`（带完整 output + usage） |
| 心跳 / 错误 | `ping` / `error` 事件 | 没有心跳；错误是 `{"error":{...}}` 分片（**没有 choices**） | `error` 事件 / `response.failed` |
| 事件名是否显式 | 有（`event:` 行） | 无（全靠 `object` 字段区分） | 有（每个事件都带 `type`） |

**关键差异**：只有 Anthropic 强制「块」要成对开闭且同一时刻只有一个块打开；completions 是「扁平增量随便交错」；Responses 用 item + index 组合表达结构。本网关用 `providers/normalizer.rs` 的 `BlockNormalizer` 把前两者的交错增量收成合法的 Anthropic 块序列（不变式写在那个文件头部）。

---

## 5. 思考（reasoning）通道：三个协议三种做法

| 协议 | 正文在哪 | 计数在哪 | 备注 |
| --- | --- | --- | --- |
| Anthropic Messages | `content[]` 里的 `thinking` 块（带 `signature` 校验；安全拦截时是 `redacted_thinking`，内容加密不可读） | `usage.output_tokens_details.thinking_tokens` | 回传时必须原样带 `signature`，否则 400（部分模型/`thinking.block_binding` 可宽恕） |
| OpenAI Chat Completions | **官方协议没有正文字段**，只有计数；正文是各家私有扩展 | `usage.completion_tokens_details.reasoning_tokens` | 实测：DeepSeek 原生用 `reasoning_content`，OpenRouter 系 / `api.commandcode.ai` 用 `reasoning`（还带结构化的 `reasoning_details[]`）。网关三个都读 |
| OpenAI Responses | `output[]` 里的 `reasoning` item（`summary[].text` 是摘要，`content[].text` 是完整思考，还有 `encrypted_content` 可回传） | `usage.output_tokens_details.reasoning_tokens` | 只有它能「回传加密思考」 |

⚠️ **实测已知坑**：`api.commandcode.ai` 上的 `deepseek/deepseek-v4.1-flash` 偶发（同请求 4 次里 2 次）把整轮正文写进 `reasoning`、`content` 留空，`finish_reason` 仍是 `stop`。客户端因此只看到「思考过程」。
网关现在的处理：照旧把这也转发出去（线上报文不变），但**明细记 `ok=0`** 并写明「上游只返回了思考内容，没有正文」（`StreamVerdict::EmptyReasoningOnly`）。

---

## 6. 停止原因对照

| 语义 | Anthropic Messages | Chat Completions | Responses（`status`） |
| --- | --- | --- | --- |
| 自然结束 | `end_turn` | `stop` | `completed` |
| 触到上限 | `max_tokens` | `length` | `incomplete` + `incomplete_details.reason = max_tokens` |
| 命中停止序列 | `stop_sequence` | `stop`（不区分） | — |
| 调用了工具 | `tool_use` | `tool_calls`（弃用的 `function_call`） | `completed`（内容里有 `function_call` item） |
| 被拒答 | `refusal`（+ `stop_details`） | `content_filter` | `completed`（内容里有 `refusal` part） |
| 长任务暂停 | `pause_turn` | — | — |
| 上下文被填满 | `model_context_window_exceeded` | `length`（不区分） | `incomplete` |

网关的规范取值就是 Anthropic 那一列，两个方向的映射表在 `providers/wire.rs`，测试 `tests::stop_reasons_round_trip_per_protocol`。

---

## 7. usage 口径对照

| 项 | Anthropic Messages | Chat Completions | Responses |
| --- | --- | --- | --- |
| 输入 | `input_tokens`（**不含**缓存命中） | `prompt_tokens`（**含**缓存命中） | `input_tokens`（含缓存命中） |
| 缓存读 | `cache_read_input_tokens` | `prompt_tokens_details.cached_tokens` | `input_tokens_details.cached_tokens` |
| 缓存写 | `cache_creation_input_tokens`（+ `cache_creation.ephemeral_5m/1h`） | — （自动缓存不单独计价） | — |
| 输出 | `output_tokens` | `completion_tokens` | `output_tokens` |
| 思考 | `output_tokens_details.thinking_tokens` | `completion_tokens_details.reasoning_tokens` | `output_tokens_details.reasoning_tokens` |
| 流式里出现的位置 | `message_start`（首包）+ `message_delta`（**累计值**，含 input/缓存） | 仅在最后一个 usage 分片（要 `stream_options.include_usage`） | `response.completed` / `response.incomplete` 的 `response.usage` |

规范层统一用 **Anthropic 口径**（`input` 不含缓存、缓存单列、思考单列），落库列见 `db/schema.sql` 的 `usage_detail`（`input_tokens` / `output_tokens` / `cache_read_tokens` / `cache_write_tokens` / `reasoning_tokens` / `total_tokens`）。
注意 `input` 语义两边相反，出站时要「把缓存并回 prompt_tokens」，这一步在 `openai_completions::encode_response` / `openai_responses::encode_response` 里，别漏。

---

## 8. 转换损失矩阵（入站 → 上游）

`loss` 一列是会被丢掉 / 变形的字段（协议表之外的东西）。

| 入站 ↓ \ 上游 → | Anthropic Messages | Chat Completions | Responses |
| --- | --- | --- | --- |
| **Anthropic Messages** | **零损失**（恒等直通；`tools` 上的 `cache_control` 等扩展字段也原样保留） | 丢：`top_k`、`thinking`（只把 `output_config.effort` 抬成 `reasoning_effort`）、`cache_control`、`container`、`mcp_servers`、`inference_geo`、服务端工具、文档/PDF 输入 | 丢：`top_k`、`stop_sequences`、`thinking`（同左）、`cache_control`、`container`、`mcp_servers` |
| **Chat Completions** | 丢：penalties、`logit_bias`、`logprobs`/`top_logprobs`、`seed`、`n`>1（直接 400）、`modalities`/`audio`、`prediction`、`verbosity`、`web_search_options`、`safety_identifier`、`prompt_cache_key` | **零损失**（同名直通，`max_completion_tokens` 也保真） | 丢：`stop`/`stop_sequences`、`n`、同上那一串 OpenAI 专有参数；`response_format` 与 `reasoning_effort` 会变形（`text.format` / `reasoning.effort`） |
| **Responses** | 丢：`truncation`、`previous_response_id`、`include`、`background`、`max_tool_calls`、`store`、`text.verbosity`、`reasoning.summary`、内置工具 | 丢：同上 + `text.verbosity`（completions 的 `verbosity` 网关未建模） | 保留大部分参数，但**状态类语义不保真**：`store` 会透传而 `previous_response_id` 被丢弃，上游会话连续性会断 |

`n > 1`（一次要多个候选）会被 `CanonicalRequest::validate()` 直接拒掉 400 —— 规范只承载单条 assistant 消息，静默只回第一个候选更糟。

---

## 9. 代码与测试索引

| 关注点 | 位置 |
| --- | --- |
| 规范请求/响应模型 | `src-tauri/src/domain/canonical.rs`（`RequestBody` / `CanonicalOnly` / `MaxTokensField` / `validate()`） |
| 协议能力表 + 停止原因表 | `src-tauri/src/providers/wire.rs`（`ProtocolProfile` / `Slot` / `apply_common_fields`） |
| 流式块状态机 + 流形态判定 | `src-tauri/src/providers/normalizer.rs`（`BlockNormalizer` / `StreamVerdict`） |
| 三个协议的编解码 | `src-tauri/src/providers/{anthropic_messages,openai_completions,openai_responses}.rs` |
| 网关装配（校验 / 自动收尾 / 落库） | `src-tauri/src/gateway/server.rs` |
| 落库字段 | `src-tauri/src/usage.rs` + `src-tauri/src/db/schema.sql` |
| 字段口径与不变式的守护测试 | `src-tauri/src/tests.rs`：`protocol_profiles_cover_every_canonical_field`、`canonical_fields_are_forwarded_per_protocol`、`stop_reasons_round_trip_per_protocol`、`block_normalizer_kesps_payloads_in_their_own_block`、`block_normalizer_serializes_parallel_tool_arguments`、`stream_verdict_flags_reasoning_only_and_truncated` |

---

## 10. 出处与维护

- 字段口径按 **2026-09-24** 抓取的官方文档整理（链接见第 2 节）。OpenAI 的文档站正在从 `platform.openai.com/docs` 迁到 `developers.openai.com/api/reference`，两个地址目前都能到，但页面结构会变。
- Anthropic 的文档站是 `docs.claude.com`，`platform.claude.com/docs/...` 是同源镜像（`.md` 后缀能拿到展开后的完整 schema，适合核对字段）。
- 三个协议的字段集合都可能随时扩充（文档原文都写了「values may expand」）。**改这里的表格时，记得同步 `providers/wire.rs` 的协议表**——测试会盯着两边一致。

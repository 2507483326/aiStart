//! 流式内容块状态机：把各上游五花八门的增量拼成合法的规范（Anthropic Messages）块序列。
//!
//! 上游的增量顺序不受我们控制（思考与正文交错、工具参数按序号交错…），而规范流对块结构有硬要求。
//! 这里用显式状态机把它收成一个不变式集合：
//!
//! * **I1** 任何时刻至多一个块打开；
//! * **I2** 载荷只进匹配的块（text→text、thinking→thinking、input_json→它所属的 tool 块）；
//!   种类不匹配时先收尾再开新块，绝不把载荷写进别的块；
//! * **I3** 块索引严格递增，收尾用它自己记下的索引；
//! * **I4** 并行工具调用的参数按上游序号缓冲，同一时刻只有一个 tool 块在收增量；
//!   其余序号在「工具阶段结束」（来正文/思考，或流结束）时按序号补发成独立的连续块，
//!   参数永远不会串到别的工具块上；
//! * **I5** 每个声明过的工具调用最终都会有且只有一个块（空参数也补一个空块）。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use super::SseEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Thinking,
    Tool,
}

impl Kind {
    fn start_payload(self, tool: Option<(&str, &str)>) -> Value {
        match self {
            Kind::Text => json!({ "type": "text", "text": "" }),
            Kind::Thinking => json!({ "type": "thinking", "thinking": "" }),
            Kind::Tool => {
                let (id, name) = tool.unwrap_or(("", ""));
                json!({ "type": "tool_use", "id": id, "name": name, "input": {} })
            }
        }
    }

    fn delta_payload(self, payload: &str) -> Value {
        match self {
            Kind::Text => json!({ "type": "text_delta", "text": payload }),
            Kind::Thinking => json!({ "type": "thinking_delta", "thinking": payload }),
            Kind::Tool => json!({ "type": "input_json_delta", "partial_json": payload }),
        }
    }
}

/// 当前打开的内容块。索引在打开时定下，收尾时直接用它的索引（I3）。
#[derive(Debug, Clone, Copy)]
struct OpenBlock {
    index: i64,
    kind: Kind,
    /// 工具块对应的上游 tool 序号。
    tool: Option<i64>,
}

#[derive(Debug, Default)]
pub struct BlockNormalizer {
    next_index: i64,
    open: Option<OpenBlock>,
    /// 上游 tool 序号 → (id, name)。
    tools: BTreeMap<i64, (String, String)>,
    /// 上游 tool 序号 → 已经为它建过的块（I5 用）。
    emitted: BTreeSet<i64>,
    /// 上游 tool 序号 → 还没下发的参数分片（I4）。
    buffered: BTreeMap<i64, String>,
    /// 当前唯一允许增量下发参数的工具序号。
    active_tool: Option<i64>,
    text_chars: usize,
    thinking_chars: usize,
}

impl BlockNormalizer {
    fn start(
        &mut self,
        kind: Kind,
        tool: Option<i64>,
        meta: Option<(&str, &str)>,
    ) -> SseEvent {
        let index = self.next_index;
        self.next_index += 1;
        self.open = Some(OpenBlock { index, kind, tool });
        if let Some(tool) = tool {
            // 上游没先声明就直接给参数时也要记账，否则同一个工具会再开一个块（I5）。
            self.emitted.insert(tool);
        }
        SseEvent::new(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": kind.start_payload(meta)
            }),
        )
    }

    fn stop_open(&mut self) -> Option<SseEvent> {
        self.open.take().map(|block| {
            SseEvent::new(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": block.index }),
            )
        })
    }

    fn delta(&mut self, kind: Kind, tool: Option<i64>, payload: &str) -> Vec<SseEvent> {
        let mut events = Vec::new();
        let mismatch = self.open.is_some_and(|block| {
            block.kind != kind || (kind == Kind::Tool && block.tool != tool)
        });
        if mismatch {
            // 工具阶段结束（缓冲的参数会在这里连续补发），再关掉种类不匹配的那个块。
            events.extend(self.end_tool_phase());
            if let Some(event) = self.stop_open() {
                events.push(event);
            }
        }
        if self.open.is_none() {
            let meta = tool.and_then(|index| {
                self.tools
                    .get(&index)
                    .map(|(id, name)| (id.clone(), name.clone()))
            });
            let meta = meta
                .as_ref()
                .map(|(id, name)| (id.as_str(), name.as_str()));
            events.push(self.start(kind, tool, meta));
        }
        if let Some(block) = self.open {
            events.push(SseEvent::new(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": block.index,
                    "delta": kind.delta_payload(payload)
                }),
            ));
        }
        events
    }

    pub fn text(&mut self, payload: &str) -> Vec<SseEvent> {
        if payload.is_empty() {
            return Vec::new();
        }
        self.text_chars += payload.len();
        self.delta(Kind::Text, None, payload)
    }

    pub fn thinking(&mut self, payload: &str) -> Vec<SseEvent> {
        if payload.is_empty() {
            return Vec::new();
        }
        self.thinking_chars += payload.len();
        self.delta(Kind::Thinking, None, payload)
    }

    /// 声明一个工具调用。块要等参数到来（或流结束）时才开，避免与 I4 的缓冲策略打架。
    pub fn tool_start(&mut self, upstream_index: i64, id: &str, name: &str) {
        self.tools
            .insert(upstream_index, (id.to_string(), name.to_string()));
    }

    pub fn tool_args(&mut self, upstream_index: i64, partial: &str) -> Vec<SseEvent> {
        match self.active_tool {
            Some(active) if active == upstream_index => {
                self.delta(Kind::Tool, Some(upstream_index), partial)
            }
            Some(_) => {
                self.buffered
                    .entry(upstream_index)
                    .or_default()
                    .push_str(partial);
                Vec::new()
            }
            None => {
                // 认领这个工具：先收尾当前的非工具块，再给它开一个自己的块做增量下发。
                let mut events = Vec::new();
                if self.open.is_some_and(|block| block.kind != Kind::Tool) {
                    if let Some(event) = self.stop_open() {
                        events.push(event);
                    }
                }
                self.active_tool = Some(upstream_index);
                events.extend(self.delta(Kind::Tool, Some(upstream_index), partial));
                events
            }
        }
    }

    /// 结束工具阶段：收尾当前工具块，把缓冲的参数按序号补发成独立的连续块，
    /// 再给没收到过参数的工具补一个空块（I4 / I5）。
    pub fn end_tool_phase(&mut self) -> Vec<SseEvent> {
        let pending: Vec<i64> = self
            .tools
            .keys()
            .copied()
            .filter(|index| !self.emitted.contains(index))
            .collect();
        if self.active_tool.is_none() && pending.is_empty() {
            return Vec::new();
        }

        let mut events = Vec::new();
        if let Some(event) = self.stop_open() {
            events.push(event);
        }
        self.active_tool = None;

        for index in pending {
            let Some((id, name)) = self.tools.get(&index).cloned() else {
                continue;
            };
            let args = self.buffered.remove(&index).unwrap_or_default();
            events.push(self.start(Kind::Tool, Some(index), Some((&id, &name))));
            if !args.is_empty() {
                if let Some(block) = self.open {
                    events.push(SseEvent::new(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": block.index,
                            "delta": { "type": "input_json_delta", "partial_json": args }
                        }),
                    ));
                }
            }
            if let Some(event) = self.stop_open() {
                events.push(event);
            }
        }
        events
    }

    /// 收尾：结束工具阶段并关闭当前块。
    pub fn close(&mut self) -> Vec<SseEvent> {
        let mut events = self.end_tool_phase();
        if let Some(event) = self.stop_open() {
            events.push(event);
        }
        events
    }

    pub fn text_chars(&self) -> usize {
        self.text_chars
    }

    pub fn thinking_chars(&self) -> usize {
        self.thinking_chars
    }

    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }
}

/// 一次流式调用最终的形态判定：决定这条明细算不算成功。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamVerdict {
    Ok,
    /// 上游只给了思考、没有正文也没有工具调用（finish_reason 却仍是 stop）。
    /// 实测 commandcode 的 deepseek-v4.1-flash 偶发这种收尾，客户端只会看到一段思考。
    EmptyReasoningOnly,
    /// 上游从头到尾没发过结束事件（`finish_reason` / `message_stop` / `[DONE]` 都没有）。
    Truncated,
    /// 解码或传输层报错。
    UpstreamError(String),
}

impl StreamVerdict {
    /// 落库口径：(是否成功, 失败原因)。
    pub fn outcome(&self) -> (bool, Option<String>) {
        match self {
            StreamVerdict::Ok => (true, None),
            StreamVerdict::EmptyReasoningOnly => (
                false,
                Some("上游只返回了思考内容，没有正文（上游异常收尾）".to_string()),
            ),
            StreamVerdict::Truncated => (
                false,
                Some("上游流未正常结束（连接中断或没有结束事件）".to_string()),
            ),
            StreamVerdict::UpstreamError(message) => (false, Some(message.clone())),
        }
    }
}

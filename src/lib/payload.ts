// 把网关落库的「原生报文」按协议解析成统一的视图模型，供详情页按 role 分块展示。
// 纯函数、无运行时依赖（仅类型），解析不了返回 null 由调用方回退原文。

export type PayloadPart =
  | { kind: "text"; text: string }
  | { kind: "thinking"; text: string }
  | { kind: "tool_use"; id: string; name: string; input: string }
  | { kind: "tool_result"; name: string | null; output: string; isError: boolean }
  | { kind: "image"; label: string };

export interface PayloadMessage {
  role: string;
  parts: PayloadPart[];
}

export interface ToolDefinition {
  name: string;
  description: string;
}

export interface RequestView {
  system: string | null;
  messages: PayloadMessage[];
  tools: ToolDefinition[];
}

export interface ResponseView {
  messages: PayloadMessage[];
  stopReason: string | null;
  usage: { input: number; output: number } | null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function asArray(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function asString(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function asNumber(value: unknown): number | null {
  return typeof value === "number" ? value : null;
}

function parseJson(raw: string | null | undefined): unknown {
  if (!raw) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

function pretty(value: unknown): string {
  if (typeof value === "string") return value;
  if (value === undefined || value === null) return "";
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

/** 工具参数可能是 JSON 字符串（OpenAI 系）或对象（Anthropic 系），统一美化成文本。 */
function prettyArguments(value: unknown): string {
  if (typeof value === "string") {
    try {
      return JSON.stringify(JSON.parse(value), null, 2);
    } catch {
      return value;
    }
  }
  return pretty(value);
}

/** 把 string | blocks | 单块 的 content 抽成纯文本（用于 system / tool_result）。 */
function textOfContent(value: unknown): string {
  if (typeof value === "string") return value;
  if (Array.isArray(value)) {
    return value
      .map((part) => {
        if (typeof part === "string") return part;
        if (isRecord(part)) return asString(part.text) ?? "";
        return "";
      })
      .filter(Boolean)
      .join("\n");
  }
  if (isRecord(value)) return asString(value.text) ?? "";
  return "";
}

/** Anthropic 形状的 content blocks → parts；顺带记录 call_id → 工具名，供 tool_result 标注。 */
function anthropicBlocks(value: unknown, names: Map<string, string>): PayloadPart[] {
  const parts: PayloadPart[] = [];
  for (const block of asArray(value)) {
    if (!isRecord(block)) continue;
    const type = asString(block.type);
    if (type === "text") {
      const text = asString(block.text) ?? "";
      if (text) parts.push({ kind: "text", text });
    } else if (type === "thinking") {
      const text = asString(block.thinking) ?? "";
      if (text) parts.push({ kind: "thinking", text });
    } else if (type === "tool_use") {
      const id = asString(block.id) ?? "";
      const name = asString(block.name) ?? "";
      if (id && name) names.set(id, name);
      parts.push({ kind: "tool_use", id, name, input: pretty(block.input ?? {}) });
    } else if (type === "tool_result") {
      const id = asString(block.tool_use_id) ?? "";
      parts.push({
        kind: "tool_result",
        name: names.get(id) ?? null,
        output: textOfContent(block.content),
        isError: block.is_error === true,
      });
    } else if (type === "image") {
      parts.push({ kind: "image", label: "图片" });
    }
  }
  return parts;
}

/** Anthropic / Responses 风格的扁平工具定义；也兼容 OpenAI completions 的嵌套写法。 */
function readTools(value: unknown): ToolDefinition[] {
  return asArray(value).flatMap((tool): ToolDefinition[] => {
    if (!isRecord(tool)) return [];
    const fn = isRecord(tool.function) ? tool.function : null;
    const name = asString(fn ? fn.name : tool.name);
    if (!name) return [];
    const description = asString(fn ? fn.description : tool.description) ?? "";
    return [{ name, description }];
  });
}

// ---------------------------------------------------------------------------
// 请求：按入站协议解析
// ---------------------------------------------------------------------------

function parseAnthropicRequest(data: Record<string, unknown>): RequestView | null {
  const messages = asArray(data.messages);
  const system = data.system === undefined ? "" : textOfContent(data.system);
  if (!messages.length && !system) return null;

  const names = new Map<string, string>();
  const out: PayloadMessage[] = [];
  for (const message of messages) {
    if (!isRecord(message)) continue;
    const role = asString(message.role) ?? "user";
    const content = message.content;
    const parts =
      typeof content === "string"
        ? content
          ? [{ kind: "text", text: content } as PayloadPart]
          : []
        : anthropicBlocks(content, names);
    if (parts.length) out.push({ role, parts });
  }
  return { system: system || null, messages: out, tools: readTools(data.tools) };
}

function parseCompletionsRequest(data: Record<string, unknown>): RequestView | null {
  const messages = asArray(data.messages);
  if (!messages.length) return null;

  const names = new Map<string, string>();
  const out: PayloadMessage[] = [];
  const systems: string[] = [];

  for (const message of messages) {
    if (!isRecord(message)) continue;
    const role = asString(message.role) ?? "user";

    if (role === "system" || role === "developer") {
      const text = textOfContent(message.content);
      if (text) systems.push(text);
      continue;
    }

    if (role === "tool" || role === "function") {
      const id = asString(message.tool_call_id) ?? "";
      out.push({
        role: "user",
        parts: [
          {
            kind: "tool_result",
            name: names.get(id) ?? null,
            output: textOfContent(message.content),
            isError: false,
          },
        ],
      });
      continue;
    }

    if (role === "assistant") {
      const parts: PayloadPart[] = [];
      const text = textOfContent(message.content);
      if (text) parts.push({ kind: "text", text });
      for (const call of asArray(message.tool_calls)) {
        if (!isRecord(call)) continue;
        const fn = isRecord(call.function) ? call.function : {};
        const id = asString(call.id) ?? "";
        const name = asString(fn.name) ?? "";
        if (id && name) names.set(id, name);
        parts.push({ kind: "tool_use", id, name, input: prettyArguments(fn.arguments) });
      }
      if (parts.length) out.push({ role, parts });
      continue;
    }

    const parts: PayloadPart[] = [];
    if (typeof message.content === "string") {
      if (message.content) parts.push({ kind: "text", text: message.content });
    } else {
      for (const part of asArray(message.content)) {
        if (!isRecord(part)) continue;
        const text = asString(part.text);
        if (text) {
          parts.push({ kind: "text", text });
        } else if (asString(part.type) === "image_url") {
          parts.push({ kind: "image", label: "图片" });
        }
      }
    }
    if (parts.length) out.push({ role, parts });
  }

  return { system: systems.length ? systems.join("\n") : null, messages: out, tools: readTools(data.tools) };
}

function parseResponsesRequest(data: Record<string, unknown>): RequestView | null {
  const input = data.input;
  if (typeof input !== "string" && !Array.isArray(input) && data.instructions === undefined) {
    return null;
  }

  const names = new Map<string, string>();
  const out: PayloadMessage[] = [];

  if (typeof input === "string") {
    if (input) out.push({ role: "user", parts: [{ kind: "text", text: input }] });
  } else {
    for (const item of asArray(input)) {
      if (!isRecord(item)) continue;
      const type = asString(item.type);

      if (type === "function_call") {
        const id = asString(item.call_id) ?? "";
        const name = asString(item.name) ?? "";
        if (id && name) names.set(id, name);
        out.push({
          role: "assistant",
          parts: [{ kind: "tool_use", id, name, input: prettyArguments(item.arguments) }],
        });
        continue;
      }

      if (type === "function_call_output") {
        const id = asString(item.call_id) ?? "";
        out.push({
          role: "user",
          parts: [
            {
              kind: "tool_result",
              name: names.get(id) ?? null,
              output: asString(item.output) ?? "",
              isError: false,
            },
          ],
        });
        continue;
      }

      const parts: PayloadPart[] = [];
      if (typeof item.content === "string") {
        if (item.content) parts.push({ kind: "text", text: item.content });
      } else {
        for (const part of asArray(item.content)) {
          if (!isRecord(part)) continue;
          const text = asString(part.text);
          if (text) {
            parts.push({ kind: "text", text });
          } else if (asString(part.type) === "input_image") {
            parts.push({ kind: "image", label: "图片" });
          }
        }
      }
      if (parts.length) out.push({ role: asString(item.role) ?? "user", parts });
    }
  }

  const system = asString(data.instructions);
  return { system: system || null, messages: out, tools: readTools(data.tools) };
}

export function parseRequest(raw: string | null, protocol: string): RequestView | null {
  const data = parseJson(raw);
  if (!isRecord(data)) return null;
  switch (protocol) {
    case "anthropic-messages":
      return parseAnthropicRequest(data);
    case "openai-completions":
      return parseCompletionsRequest(data);
    case "openai-responses":
      return parseResponsesRequest(data);
    default:
      return (
        parseAnthropicRequest(data) ??
        parseCompletionsRequest(data) ??
        parseResponsesRequest(data)
      );
  }
}

// ---------------------------------------------------------------------------
// 响应：按上游协议解析（流式拼装后的形状与非流式一致）
// ---------------------------------------------------------------------------

function parseAnthropicResponse(data: Record<string, unknown>): ResponseView | null {
  if (data.content === undefined) return null;
  const parts = anthropicBlocks(data.content, new Map());
  const usage = isRecord(data.usage)
    ? {
        input: asNumber(data.usage.input_tokens) ?? 0,
        output: asNumber(data.usage.output_tokens) ?? 0,
      }
    : null;
  return {
    messages: [{ role: asString(data.role) ?? "assistant", parts }],
    stopReason: asString(data.stop_reason),
    usage,
  };
}

function parseCompletionsResponse(data: Record<string, unknown>): ResponseView | null {
  const choice = asArray(data.choices)[0];
  if (!isRecord(choice) || !isRecord(choice.message)) return null;
  const message = choice.message;

  const parts: PayloadPart[] = [];
  const reasoning = asString(message.reasoning_content);
  if (reasoning) parts.push({ kind: "thinking", text: reasoning });
  const text = asString(message.content);
  if (text) parts.push({ kind: "text", text });
  for (const call of asArray(message.tool_calls)) {
    if (!isRecord(call)) continue;
    const fn = isRecord(call.function) ? call.function : {};
    parts.push({
      kind: "tool_use",
      id: asString(call.id) ?? "",
      name: asString(fn.name) ?? "",
      input: prettyArguments(fn.arguments),
    });
  }

  const usage = isRecord(data.usage)
    ? {
        input: asNumber(data.usage.prompt_tokens) ?? 0,
        output: asNumber(data.usage.completion_tokens) ?? 0,
      }
    : null;
  return {
    messages: [{ role: "assistant", parts }],
    stopReason: asString(choice.finish_reason),
    usage,
  };
}

function parseResponsesResponse(data: Record<string, unknown>): ResponseView | null {
  if (!Array.isArray(data.output)) return null;

  const parts: PayloadPart[] = [];
  for (const item of data.output) {
    if (!isRecord(item)) continue;
    const type = asString(item.type);
    if (type === "reasoning") {
      const text = asArray(item.summary)
        .map((summary) => (isRecord(summary) ? asString(summary.text) ?? "" : ""))
        .filter(Boolean)
        .join("\n");
      if (text) parts.push({ kind: "thinking", text });
    } else if (type === "message") {
      for (const part of asArray(item.content)) {
        if (!isRecord(part)) continue;
        const text = asString(part.text);
        if (text) parts.push({ kind: "text", text });
      }
    } else if (type === "function_call") {
      parts.push({
        kind: "tool_use",
        id: asString(item.call_id) ?? "",
        name: asString(item.name) ?? "",
        input: prettyArguments(item.arguments),
      });
    }
  }

  const usage = isRecord(data.usage)
    ? {
        input: asNumber(data.usage.input_tokens) ?? 0,
        output: asNumber(data.usage.output_tokens) ?? 0,
      }
    : null;
  return {
    messages: [{ role: "assistant", parts }],
    stopReason: asString(data.status),
    usage,
  };
}

export function parseResponse(raw: string | null, protocol: string): ResponseView | null {
  const data = parseJson(raw);
  if (!isRecord(data)) return null;
  switch (protocol) {
    case "anthropic-messages":
      return parseAnthropicResponse(data);
    case "openai-completions":
      return parseCompletionsResponse(data);
    case "openai-responses":
      return parseResponsesResponse(data);
    default:
      return (
        parseAnthropicResponse(data) ??
        parseCompletionsResponse(data) ??
        parseResponsesResponse(data)
      );
  }
}

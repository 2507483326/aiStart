import type { AppKind, ApplyMode, GatewayState, ModelFormat } from "@/lib/types";

export function formatNumber(value: number): string {
  return new Intl.NumberFormat("zh-CN").format(value);
}

export function formatCompact(value: number): string {
  if (value < 1000) return String(value);
  if (value < 1_000_000) return `${(value / 1000).toFixed(1)}K`;
  return `${(value / 1_000_000).toFixed(2)}M`;
}

export interface CacheTokens {
  inputTokens: number;
  cacheReadTokens?: number | null;
  cacheWriteTokens?: number | null;
}

/**
 * 真实消耗 token（口径对齐 cc-switch「真实消耗」）：输入 + 输出 + 缓存读 + 缓存写。
 * 输入 / 输出各自不含缓存，缓存读写单列，只有总量把它们合并。
 */
export function totalTokens(value: CacheTokens & { outputTokens: number }): number {
  return (
    value.inputTokens +
    value.outputTokens +
    (value.cacheReadTokens ?? 0) +
    (value.cacheWriteTokens ?? 0)
  );
}

/**
 * 缓存命中率（口径对齐 dsh）：缓存读 / 计费输入（未命中输入 + 缓存读 + 缓存写）。
 * 无计费输入时返回 null，界面应显「—」而不是 0%。
 */
export function cacheHitRate(value: CacheTokens): number | null {
  const cacheRead = value.cacheReadTokens ?? 0;
  const billed = value.inputTokens + cacheRead + (value.cacheWriteTokens ?? 0);
  return billed > 0 ? cacheRead / billed : null;
}

export function formatPercent(rate: number | null): string {
  return rate === null ? "—" : `${Math.round(rate * 100)}%`;
}

export function formatDateTime(value: string): string {
  if (!value) return "—";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function formatLatency(ms: number): string {
  if (ms < 1000) return `${ms} ms`;
  return `${(ms / 1000).toFixed(2)} s`;
}

export function prettyJson(text: string | null): string {
  if (!text) return "";
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return text;
  }
}

export function protocolLabel(value: string): string {
  switch (value) {
    case "anthropic-messages":
      return "Messages";
    case "openai-completions":
      return "Completions";
    case "openai-responses":
      return "Responses";
    default:
      return value || "—";
  }
}

export function maskSecret(value: string): string {
  if (!value) return "—";
  if (value.length <= 8) return "••••";
  return `${value.slice(0, 4)}••••${value.slice(-4)}`;
}

export const formatLabels: Record<ModelFormat, string> = {
  "anthropic-messages": "Anthropic Messages",
  "openai-completions": "OpenAI Chat Completions",
  "openai-responses": "OpenAI Responses",
};

export const formatShortLabels: Record<ModelFormat, string> = {
  "anthropic-messages": "Messages",
  "openai-completions": "Completions",
  "openai-responses": "Responses",
};

export const applyModeLabels: Record<ApplyMode, string> = {
  gateway: "网关接管",
  "direct-config": "写入配置",
  manual: "手动应用",
};

export const gatewayStateLabels: Record<GatewayState, string> = {
  stopped: "已停止",
  starting: "启动中",
  running: "运行中",
  stopping: "停止中",
};

export const sourceAppLabels: Record<AppKind, string> = {
  "claude-desktop": "Claude Desktop",
  "deepseek-desktop": "DeepSeek Desktop",
  codex: "Codex",
  zcode: "ZCode",
  workbuddy: "WorkBuddy",
};

export const sourceAppIcons: Record<AppKind, string> = {
  "claude-desktop": "/app-icons/claude-desktop.svg",
  "deepseek-desktop": "/app-icons/deepseek-desktop.png",
  codex: "/app-icons/codex.svg",
  zcode: "/app-icons/zcode.svg",
  workbuddy: "/app-icons/workbuddy.svg",
};

/** 请求来源：匹配到的应用显示其名称，未匹配的（自定义 Key）原样展示 token，未记录显示「未知」。 */
export function sourceAppLabel(value: string): string {
  if (!value) return "未知";
  return (sourceAppLabels as Record<string, string>)[value] ?? value;
}

/** 请求来源匹配到内置应用时返回其图标路径，未匹配（自定义 Key）返回 null。 */
export function sourceAppIcon(value: string): string | null {
  return (sourceAppIcons as Record<string, string>)[value] ?? null;
}

import type { ApplyMode, ModelFormat } from "@/lib/types";

export function formatNumber(value: number): string {
  return new Intl.NumberFormat("zh-CN").format(value);
}

export function formatCompact(value: number): string {
  if (value < 1000) return String(value);
  if (value < 1_000_000) return `${(value / 1000).toFixed(1)}K`;
  return `${(value / 1_000_000).toFixed(2)}M`;
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

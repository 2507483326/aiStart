export type ModelFormat =
  | "anthropic-messages"
  | "openai-completions"
  | "openai-responses";

export type AppKind =
  | "claude-desktop"
  | "deepseek-desktop"
  | "codex"
  | "zcode"
  | "workbuddy";

export type ApplyMode = "gateway" | "direct-config" | "manual";

export interface ModelConfig {
  id: number;
  name: string;
  format: ModelFormat;
  baseUrl: string;
  apiKey: string;
  model: string;
  supports1m: boolean;
  createdAt: string;
  updatedAt: string;
}

export interface ModelInput {
  id?: number;
  name: string;
  format: ModelFormat;
  baseUrl: string;
  apiKey: string;
  model: string;
  supports1m: boolean;
}

export interface FormatInfo {
  format: ModelFormat;
  displayName: string;
  defaultBaseUrl: string;
}

export type PromptMode = "append" | "prepend";

export type FilterRule = { kind: "system-prompt"; mode: PromptMode; text: string };

export interface RequestFilter {
  id: number;
  name: string;
  enabled: boolean;
  order: number;
  rule: FilterRule;
  createdAt: string;
  updatedAt: string;
}

export interface FilterInput {
  id?: number;
  name: string;
  enabled: boolean;
  rule: FilterRule;
}

export interface UsageRecord {
  id: number;
  timestamp: string;
  date: string;
  modelName: string;
  servedBy: string;
  sourceApp: string;
  upstreamUrl: string;
  upstreamModel: string;
  /// 这次请求是否经代理出站（未发起上游请求的失败算直连）。
  proxied: boolean;
  inboundProtocol: string;
  upstreamProtocol: string;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number | null;
  cacheWriteTokens: number | null;
  durationMs: number;
  ok: boolean;
  failover: boolean;
  error: string | null;
}

export interface UsagePayloadDetail {
  id: number;
  time: string;
  inboundRequest: string | null;
  upstreamRequest: string | null;
  upstreamResponse: string | null;
  requestTruncated: boolean;
  upstreamRequestTruncated: boolean;
  responseTruncated: boolean;
  stream: boolean;
}

export interface RequestDetail {
  record: UsageRecord;
  payload: UsagePayloadDetail | null;
}

export interface UsagePage {
  total: number;
  items: UsageRecord[];
}

export interface DailyUsage {
  date: string;
  requests: number;
  failed: number;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
  totalTokens: number;
}

export interface ModelUsage {
  modelName: string;
  requests: number;
  inputTokens: number;
  outputTokens: number;
}

export interface UsageSummary {
  totalRequests: number;
  failedRequests: number;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
  totalTokens: number;
  todayTokens: number;
  streakDays: number;
  daily: DailyUsage[];
  byModel: ModelUsage[];
}

export interface ToolApp {
  kind: AppKind;
  name: string;
  publisher: string;
  description: string;
  homepage: string;
  downloadPage: string;
  requiresGateway: boolean;
  applyMode: ApplyMode;
  configTarget: string;
  apiKey: string;
  installed: boolean;
  version: string | null;
  installLocation: string | null;
  latestVersion: string | null;
  updateAvailable: boolean;
  appliedModelId: number | null;
  appliedModelName: string | null;
}

export interface AppUpdate {
  kind: AppKind;
  latestVersion: string | null;
  updateAvailable: boolean;
}

export interface ApplyReport {
  kind: AppKind;
  modelId: number;
  modelName: string;
  applyMode: ApplyMode;
  target: string;
  restartRequired: boolean;
  steps: string[];
  note: string | null;
}

export interface InstallReport {
  kind: AppKind;
  action: string;
  target: string;
  launched: boolean;
  steps: string[];
}

export interface TestResult {
  ok: boolean;
  latencyMs: number;
  message: string;
  preview: string | null;
  inputTokens: number;
  outputTokens: number;
}

export type GatewayState = "stopped" | "starting" | "running" | "stopping";

export interface GatewayStatus {
  running: boolean;
  state: GatewayState;
  port: number;
  baseUrl: string;
  requests: number;
  errors: number;
  failovers: number;
  inputTokens: number;
  outputTokens: number;
  lastError: string | null;
  lastFailover: string | null;
  autoFailover: boolean;
  activeModelName: string | null;
  activeModelFormat: ModelFormat | null;
  activeModelId: number | null;
}

export interface SettingsView {
  gatewayPort: number;
  autoFailover: boolean;
  /// 开机自启（默认开启，落在 Windows 登录启动项上）。
  launchAtLogin: boolean;
  /// 出站代理是否启用（关掉即直连，proxyUrl 保留）。
  proxyEnabled: boolean;
  /// 出站代理地址，空串 = 直连。
  proxyUrl: string;
  activeModelId: number | null;
  applied: Record<string, number>;
}

export interface SettingsInput {
  gatewayPort?: number;
  autoFailover?: boolean;
  launchAtLogin?: boolean;
  proxyEnabled?: boolean;
  proxyUrl?: string;
}

export interface AppInfo {
  name: string;
  version: string;
  platform: string;
  arch: string;
  configDir: string;
}

export interface DownloadProgress {
  kind: AppKind;
  action: string;
  phase: "downloading" | "launched";
  received: number;
  total: number | null;
  percent: number | null;
}

export interface EventRecord {
  id: number;
  time: string;
  actorKind: string;
  actorName: string | null;
  type: string;
  targetKind: string | null;
  targetId: string | null;
  payload: string | null;
}

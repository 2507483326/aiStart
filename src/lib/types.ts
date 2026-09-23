export type ModelFormat =
  | "anthropic-messages"
  | "openai-completions"
  | "openai-responses";

export type AppKind = "claude-desktop" | "deepseek-desktop";

export type ApplyMode = "gateway" | "direct-config" | "manual";

export interface ModelConfig {
  id: string;
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
  id?: string;
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

export interface UsageRecord {
  timestamp: string;
  date: string;
  modelName: string;
  servedBy: string;
  inboundProtocol: string;
  upstreamProtocol: string;
  inputTokens: number;
  outputTokens: number;
  durationMs: number;
  ok: boolean;
  failover: boolean;
  error: string | null;
}

export interface DailyUsage {
  date: string;
  requests: number;
  failed: number;
  inputTokens: number;
  outputTokens: number;
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
  downloadPage: string;
  requiresGateway: boolean;
  applyMode: ApplyMode;
  configTarget: string;
  installed: boolean;
  version: string | null;
  installLocation: string | null;
  latestVersion: string | null;
  updateAvailable: boolean;
  appliedModelId: string | null;
  appliedModelName: string | null;
}

export interface ApplyReport {
  kind: AppKind;
  modelId: string;
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

export interface GatewayStatus {
  running: boolean;
  port: number;
  baseUrl: string;
  token: string;
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
}

export interface SettingsView {
  gatewayPort: number;
  gatewayToken: string;
  deepseekConfigPath: string;
  autoFailover: boolean;
  activeModelId: string | null;
  applied: Record<string, string>;
}

export interface SettingsInput {
  gatewayPort?: number;
  deepseekConfigPath?: string;
  autoFailover?: boolean;
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

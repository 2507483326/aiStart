import { invoke } from "@tauri-apps/api/core";

import type {
  AppInfo,
  AppKind,
  ApplyReport,
  FormatInfo,
  GatewayStatus,
  InstallReport,
  ModelConfig,
  ModelFormat,
  ModelInput,
  SettingsInput,
  SettingsView,
  TestResult,
  ToolApp,
  UsageRecord,
  UsageSummary,
} from "@/lib/types";

export const appApi = {
  list: () => invoke<ToolApp[]>("list_apps"),
  apply: (kind: AppKind, modelId?: string) =>
    invoke<ApplyReport>("apply_model", { kind, modelId: modelId ?? null }),
  clear: (kind: AppKind) => invoke<void>("clear_app_model", { kind }),
  install: (kind: AppKind) => invoke<InstallReport>("install_app", { kind }),
  update: (kind: AppKind) => invoke<InstallReport>("update_app", { kind }),
};

export const modelApi = {
  list: () => invoke<ModelConfig[]>("list_models"),
  formats: () => invoke<FormatInfo[]>("list_model_formats"),
  save: (input: ModelInput) => invoke<ModelConfig>("save_model", { input }),
  remove: (id: string) => invoke<ModelConfig[]>("delete_model", { id }),
  activate: (id: string) => invoke<GatewayStatus>("activate_model", { id }),
  test: (id: string) => invoke<TestResult>("test_model", { id }),
  fetchUpstream: (baseUrl: string, apiKey: string, format: ModelFormat) =>
    invoke<string[]>("fetch_upstream_models", { baseUrl, apiKey, format }),
};

export const gatewayApi = {
  status: () => invoke<GatewayStatus>("gateway_status"),
  restart: () => invoke<GatewayStatus>("restart_gateway"),
};

export const usageApi = {
  summary: (days: number) => invoke<UsageSummary>("usage_summary", { days }),
  records: (limit: number) => invoke<UsageRecord[]>("usage_records", { limit }),
};

export const systemApi = {
  settings: () => invoke<SettingsView>("get_settings"),
  updateSettings: (input: SettingsInput) =>
    invoke<SettingsView>("update_settings", { input }),
  info: () => invoke<AppInfo>("app_info"),
};

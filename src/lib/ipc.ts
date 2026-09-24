import { invoke } from "@tauri-apps/api/core";

import type {
  AppInfo,
  AppKind,
  ApplyReport,
  AppUpdate,
  EventRecord,
  FilterInput,
  FormatInfo,
  GatewayStatus,
  InstallReport,
  ModelConfig,
  ModelFormat,
  ModelInput,
  RequestDetail,
  RequestFilter,
  SettingsInput,
  SettingsView,
  TestResult,
  ToolApp,
  UsagePage,
  UsageRecord,
  UsageSummary,
} from "@/lib/types";

export const appApi = {
  list: () => invoke<ToolApp[]>("list_apps"),
  apply: (kind: AppKind, modelId?: number) =>
    invoke<ApplyReport>("apply_model", { kind, modelId: modelId ?? null }),
  clear: (kind: AppKind) => invoke<void>("clear_app_model", { kind }),
  install: (kind: AppKind) => invoke<InstallReport>("install_app", { kind }),
  update: (kind: AppKind) => invoke<InstallReport>("update_app", { kind }),
  installerUrl: (kind: AppKind) => invoke<string>("installer_url", { kind }),
  checkUpdates: () => invoke<AppUpdate[]>("check_app_updates"),
};

export const modelApi = {
  list: () => invoke<ModelConfig[]>("list_models"),
  formats: () => invoke<FormatInfo[]>("list_model_formats"),
  save: (input: ModelInput) => invoke<ModelConfig>("save_model", { input }),
  remove: (id: number) => invoke<ModelConfig[]>("delete_model", { id }),
  activate: (id: number) => invoke<GatewayStatus>("activate_model", { id }),
  test: (id: number) => invoke<TestResult>("test_model", { id }),
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
  page: (offset: number, limit: number) =>
    invoke<UsagePage>("usage_page", { offset, limit }),
  detail: (id: number) => invoke<RequestDetail | null>("usage_detail", { id }),
};

export const systemApi = {
  settings: () => invoke<SettingsView>("get_settings"),
  updateSettings: (input: SettingsInput) =>
    invoke<SettingsView>("update_settings", { input }),
  info: () => invoke<AppInfo>("app_info"),
  openDownloadDir: () => invoke<void>("open_download_dir"),
};

export const eventApi = {
  list: (limit?: number) => invoke<EventRecord[]>("list_events", { limit: limit ?? null }),
};

export const translateApi = {
  text: (text: string) => invoke<string>("translate_text", { text }),
};

export const filterApi = {
  list: () => invoke<RequestFilter[]>("list_filters"),
  save: (input: FilterInput) => invoke<RequestFilter>("save_filter", { input }),
  setEnabled: (id: number, enabled: boolean) =>
    invoke<RequestFilter>("set_filter_enabled", { id, enabled }),
  remove: (id: number) => invoke<RequestFilter[]>("delete_filter", { id }),
};

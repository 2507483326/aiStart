import { ref } from "vue";

import { systemApi } from "@/lib/ipc";
import { attempt } from "@/lib/notify";
import type { AppInfo, SettingsInput, SettingsView } from "@/lib/types";

const settings = ref<SettingsView | null>(null);
const info = ref<AppInfo | null>(null);

async function refresh(): Promise<void> {
  const [current, appInfo] = await Promise.all([
    systemApi.settings(),
    systemApi.info(),
  ]);
  settings.value = current;
  info.value = appInfo;
}

async function update(input: SettingsInput): Promise<boolean> {
  const next = await attempt(() => systemApi.updateSettings(input), {
    success: "设置已保存",
    error: "保存设置失败",
  });
  if (!next) return false;
  settings.value = next;
  return true;
}

export function useSettings() {
  return { settings, info, refresh, update };
}

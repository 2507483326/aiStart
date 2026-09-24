import { defineStore } from "pinia";

import { systemApi } from "@/lib/ipc";
import { attempt } from "@/lib/notify";
import type { AppInfo, SettingsInput, SettingsView } from "@/lib/types";

export const useSettingsStore = defineStore("settings", {
  state: () => ({
    settings: null as SettingsView | null,
    info: null as AppInfo | null,
  }),
  actions: {
    async refresh(): Promise<void> {
      const [current, appInfo] = await Promise.all([
        systemApi.settings(),
        systemApi.info(),
      ]);
      this.settings = current;
      this.info = appInfo;
    },
    async update(input: SettingsInput): Promise<boolean> {
      const next = await attempt(() => systemApi.updateSettings(input), {
        success: "设置已保存",
        error: "保存设置失败",
      });
      if (!next) return false;
      this.settings = next;
      return true;
    },
  },
});

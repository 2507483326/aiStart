import { defineStore } from "pinia";
import { listen } from "@tauri-apps/api/event";

import { appApi } from "@/lib/ipc";
import { attempt, notifyError, notifySuccess } from "@/lib/notify";
import type { AppKind, AppUpdate, DownloadProgress, ToolApp } from "@/lib/types";

export const useAppsStore = defineStore("apps", {
  state: () => ({
    apps: [] as ToolApp[],
    loading: false,
    busyKind: null as AppKind | null,
    download: null as DownloadProgress | null,
    lastSteps: [] as string[],
    listenerReady: false,
    knownUpdates: new Map<AppKind, AppUpdate>(),
  }),
  actions: {
    withKnownUpdates(list: ToolApp[]): ToolApp[] {
      return list.map((app) => {
        const update = this.knownUpdates.get(app.kind);
        return update
          ? {
              ...app,
              latestVersion: update.latestVersion,
              updateAvailable: update.updateAvailable,
            }
          : app;
      });
    },
    async ensureListener(): Promise<void> {
      if (this.listenerReady) return;
      await listen<DownloadProgress>("install://progress", (event) => {
        this.download = event.payload;
        if (event.payload.phase === "launched") {
          setTimeout(() => {
            this.download = null;
          }, 1200);
        }
      });
      this.listenerReady = true;
    },
    async checkUpdates(): Promise<void> {
      try {
        const updates = await appApi.checkUpdates();
        for (const update of updates) {
          this.knownUpdates.set(update.kind, update);
        }
        this.apps = this.withKnownUpdates(this.apps);
      } catch {
        // 离线或上游不可达时保留上一次结果，避免徽标被清空
      }
    },
    async refresh(): Promise<void> {
      this.loading = true;
      try {
        this.apps = this.withKnownUpdates(await appApi.list());
      } finally {
        this.loading = false;
      }
      void this.checkUpdates();
    },
    // 「刷新」按钮专用：重新拉列表并等联网检查完成，让按钮的 loading 覆盖整段过程，
    // 结束后给出明确反馈（否则后台检查静默，看起来像点了没反应）。
    async recheck(): Promise<void> {
      this.loading = true;
      try {
        this.apps = this.withKnownUpdates(await appApi.list());
        await this.checkUpdates();
      } catch (error) {
        notifyError(error, "检查更新失败");
        return;
      } finally {
        this.loading = false;
      }
      const count = this.apps.filter((app) => app.updateAvailable).length;
      notifySuccess(count > 0 ? `发现 ${count} 个可更新的应用` : "未发现可更新的应用");
    },
    async install(kind: AppKind): Promise<void> {
      this.busyKind = kind;
      try {
        const report = await attempt(() => appApi.install(kind), {
          error: "启动安装失败",
        });
        if (!report) return;
        this.lastSteps = report.steps;
        notifySuccess(
          `${kind === "claude-desktop" ? "Claude Desktop" : "DeepSeek Desktop"} 安装流程已启动`,
          report.steps[report.steps.length - 1],
        );
        await this.refresh();
      } finally {
        this.busyKind = null;
      }
    },
    async update(kind: AppKind): Promise<void> {
      this.busyKind = kind;
      try {
        const report = await attempt(() => appApi.update(kind), {
          error: "更新失败",
        });
        if (!report) return;
        this.lastSteps = report.steps;
        notifySuccess("更新流程已启动", report.steps[report.steps.length - 1]);
        await this.refresh();
      } finally {
        this.busyKind = null;
      }
    },
    async apply(kind: AppKind, modelId?: number): Promise<boolean> {
      this.busyKind = kind;
      try {
        const report = await attempt(() => appApi.apply(kind, modelId), {
          error: "一键应用模型失败",
        });
        if (!report) return false;
        this.lastSteps = report.steps;
        notifySuccess(
          `已把「${report.modelName}」接入 ${report.target.split(" → ")[0]}`,
          report.note ?? undefined,
        );
        await this.refresh();
        return true;
      } finally {
        this.busyKind = null;
      }
    },
    async clear(kind: AppKind): Promise<boolean> {
      this.busyKind = kind;
      try {
        const applied = await attempt(() => appApi.clear(kind), {
          success: "已移除该应用的模型配置",
          error: "移除配置失败",
        });
        if (applied === undefined) return false;
        await this.refresh();
        return true;
      } finally {
        this.busyKind = null;
      }
    },
  },
});

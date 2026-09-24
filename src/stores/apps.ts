import { defineStore } from "pinia";
import { listen } from "@tauri-apps/api/event";

import { appApi } from "@/lib/ipc";
import { attempt, notifyError, notifySuccess } from "@/lib/notify";
import type { AppKind, AppUpdate, DownloadProgress, ToolApp } from "@/lib/types";

// 同一应用上有多个互斥的异步操作，需区分是哪一个在跑：
// 只按 kind 记录会让所有按钮都变成 loading（点「升级」却是「应用」在转圈）。
export type AppAction = "install" | "update" | "apply" | "clear";

// 直链解析要联网（版本探测 + 元数据），菜单打开时静默预取、点击时命中缓存即可瞬时复制；
// 同一应用的并发解析去重，避免「预取还没回来就点了复制」时打两次请求。
const downloadUrlRequests = new Map<AppKind, Promise<string>>();

export const useAppsStore = defineStore("apps", {
  state: () => ({
    apps: [] as ToolApp[],
    loading: false,
    pending: null as { kind: AppKind; action: AppAction } | null,
    download: null as DownloadProgress | null,
    lastSteps: [] as string[],
    listenerReady: false,
    knownUpdates: new Map<AppKind, AppUpdate>(),
    downloadUrls: {} as Record<string, string>,
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
    // 取该应用的安装包直链：命中缓存直接返回，否则联网解析（见 `installer_url`）。
    // `silent` 供菜单打开时的预取——失败不弹错，等用户真点「复制下载地址」再报。
    async downloadUrl(kind: AppKind, silent = false): Promise<string | undefined> {
      const cached = this.downloadUrls[kind];
      if (cached) return cached;

      let request = downloadUrlRequests.get(kind);
      if (!request) {
        request = appApi.installerUrl(kind);
        downloadUrlRequests.set(kind, request);
      }
      try {
        const url = await request;
        this.downloadUrls[kind] = url;
        return url;
      } catch (error) {
        if (!silent) notifyError(error, "解析下载地址失败");
        return undefined;
      } finally {
        downloadUrlRequests.delete(kind);
      }
    },
    async install(kind: AppKind): Promise<void> {
      // 安装会换版本，直链随之改变，缓存作废。
      delete this.downloadUrls[kind];
      this.pending = { kind, action: "install" };
      try {
        const report = await attempt(() => appApi.install(kind), {
          error: "启动安装失败",
        });
        if (!report) return;
        this.lastSteps = report.steps;
        const name = this.apps.find((app) => app.kind === kind)?.name ?? kind;
        notifySuccess(
          `${name} 安装流程已启动`,
          report.steps[report.steps.length - 1],
        );
        await this.refresh();
      } finally {
        this.pending = null;
      }
    },
    async update(kind: AppKind): Promise<void> {
      // 升级会换版本，直链随之改变，缓存作废。
      delete this.downloadUrls[kind];
      this.pending = { kind, action: "update" };
      try {
        const report = await attempt(() => appApi.update(kind), {
          error: "更新失败",
        });
        if (!report) return;
        this.lastSteps = report.steps;
        notifySuccess("更新流程已启动", report.steps[report.steps.length - 1]);
        await this.refresh();
      } finally {
        this.pending = null;
      }
    },
    async apply(kind: AppKind, modelId?: number): Promise<boolean> {
      this.pending = { kind, action: "apply" };
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
        this.pending = null;
      }
    },
    async clear(kind: AppKind): Promise<boolean> {
      this.pending = { kind, action: "clear" };
      try {
        const applied = await attempt(() => appApi.clear(kind), {
          success: "已移除该应用的模型配置",
          error: "移除配置失败",
        });
        if (applied === undefined) return false;
        await this.refresh();
        return true;
      } finally {
        this.pending = null;
      }
    },
  },
});

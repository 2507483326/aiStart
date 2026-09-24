import { defineStore } from "pinia";
import { listen } from "@tauri-apps/api/event";

import { gatewayApi } from "@/lib/ipc";
import { attempt } from "@/lib/notify";
import type { GatewayState, GatewayStatus } from "@/lib/types";

export const useGatewayStore = defineStore("gateway", {
  state: () => ({
    status: null as GatewayStatus | null,
    /** 重启请求在途。区别于状态机里的过渡态，用来锁住按钮。 */
    busy: false,
    listenerReady: false,
  }),
  getters: {
    running: (state): boolean => state.status?.running ?? false,
    phase: (state): GatewayState => state.status?.state ?? "stopped",
    transitioning: (state): boolean =>
      state.status?.state === "starting" || state.status?.state === "stopping",
  },
  actions: {
    /** 后端每次状态变化都会推 `gateway://state`，比轮询更及时，也才能看到过渡态。 */
    async ensureListener(): Promise<void> {
      if (this.listenerReady) return;
      await listen<GatewayStatus>("gateway://state", (event) => {
        this.status = event.payload;
      });
      this.listenerReady = true;
    },
    async refresh(): Promise<void> {
      try {
        this.status = await gatewayApi.status();
      } catch {
        // 单次轮询失败保留上一次状态，避免徽标闪回「已停止」
      }
    },
    async restart(): Promise<boolean> {
      this.busy = true;
      try {
        const result = await attempt(() => gatewayApi.restart(), {
          success: "本地网关已重启",
          error: "重启网关失败",
        });
        if (!result) return false;
        this.status = result;
        return true;
      } finally {
        this.busy = false;
      }
    },
  },
});

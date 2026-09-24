import { defineStore } from "pinia";

import { gatewayApi } from "@/lib/ipc";
import { attempt } from "@/lib/notify";
import type { GatewayStatus } from "@/lib/types";

export const useGatewayStore = defineStore("gateway", {
  state: () => ({
    status: null as GatewayStatus | null,
    loading: false,
  }),
  getters: {
    running: (state): boolean => state.status?.running ?? false,
  },
  actions: {
    async refresh(): Promise<void> {
      this.loading = true;
      try {
        this.status = await gatewayApi.status();
      } finally {
        this.loading = false;
      }
    },
    async restart(): Promise<boolean> {
      const result = await attempt(() => gatewayApi.restart(), {
        success: "本地网关已重启",
        error: "重启网关失败",
      });
      if (!result) return false;
      this.status = result;
      return true;
    },
  },
});

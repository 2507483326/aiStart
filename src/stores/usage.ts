import { defineStore } from "pinia";

import { usageApi } from "@/lib/ipc";
import type { DailyUsage, UsageRecord, UsageTotals } from "@/lib/types";

export const useUsageStore = defineStore("usage", {
  state: () => ({
    /// 每日汇总行（直读，读时不做 SUM）。
    daily: [] as DailyUsage[],
    /// 今日一行；当天无记录时后端返回零值行。
    today: null as DailyUsage | null,
    /// 全量累计（day = '' 的那一行）。
    total: null as UsageTotals | null,
    records: [] as UsageRecord[],
    loading: false,
  }),
  actions: {
    async refresh(days = 365, limit = 300): Promise<void> {
      this.loading = true;
      try {
        const [nextDaily, nextToday, nextTotal, nextRecords] = await Promise.all([
          usageApi.daily(days),
          usageApi.today(),
          usageApi.total(),
          usageApi.records(limit),
        ]);
        this.daily = nextDaily;
        this.today = nextToday;
        this.total = nextTotal;
        this.records = nextRecords;
      } finally {
        this.loading = false;
      }
    },
  },
});

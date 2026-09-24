import { defineStore } from "pinia";

import { usageApi } from "@/lib/ipc";
import type { UsageRecord, UsageSummary } from "@/lib/types";

export const useUsageStore = defineStore("usage", {
  state: () => ({
    summary: null as UsageSummary | null,
    records: [] as UsageRecord[],
    loading: false,
  }),
  actions: {
    async refresh(days = 365, limit = 300): Promise<void> {
      this.loading = true;
      try {
        const [nextSummary, nextRecords] = await Promise.all([
          usageApi.summary(days),
          usageApi.records(limit),
        ]);
        this.summary = nextSummary;
        this.records = nextRecords;
      } finally {
        this.loading = false;
      }
    },
  },
});

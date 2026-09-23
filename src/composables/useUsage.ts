import { ref } from "vue";

import { usageApi } from "@/lib/ipc";
import type { UsageRecord, UsageSummary } from "@/lib/types";

const summary = ref<UsageSummary | null>(null);
const records = ref<UsageRecord[]>([]);
const loading = ref(false);

async function refresh(days = 365, limit = 300): Promise<void> {
  loading.value = true;
  try {
    const [nextSummary, nextRecords] = await Promise.all([
      usageApi.summary(days),
      usageApi.records(limit),
    ]);
    summary.value = nextSummary;
    records.value = nextRecords;
  } finally {
    loading.value = false;
  }
}

export function useUsage() {
  return { summary, records, loading, refresh };
}

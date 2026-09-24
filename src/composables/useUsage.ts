import { storeToRefs } from "pinia";

import { useUsageStore } from "@/stores/usage";

export function useUsage() {
  const store = useUsageStore();
  return {
    ...storeToRefs(store),
    refresh: store.refresh,
  };
}

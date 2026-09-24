import { storeToRefs } from "pinia";

import { useSettingsStore } from "@/stores/settings";

export function useSettings() {
  const store = useSettingsStore();
  return {
    ...storeToRefs(store),
    refresh: store.refresh,
    update: store.update,
  };
}

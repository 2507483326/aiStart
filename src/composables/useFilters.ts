import { storeToRefs } from "pinia";

import { useFiltersStore } from "@/stores/filters";

export function useFilters() {
  const store = useFiltersStore();
  return {
    ...storeToRefs(store),
    refresh: store.refresh,
    save: store.save,
    setEnabled: store.setEnabled,
    remove: store.remove,
  };
}

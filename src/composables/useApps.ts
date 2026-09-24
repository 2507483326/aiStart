import { storeToRefs } from "pinia";

import { useAppsStore } from "@/stores/apps";

export function useApps() {
  const store = useAppsStore();
  return {
    ...storeToRefs(store),
    refresh: store.refresh,
    ensureListener: store.ensureListener,
    install: store.install,
    update: store.update,
    apply: store.apply,
    clear: store.clear,
  };
}

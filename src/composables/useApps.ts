import { storeToRefs } from "pinia";

import { useAppsStore } from "@/stores/apps";

export function useApps() {
  const store = useAppsStore();
  return {
    ...storeToRefs(store),
    refresh: store.refresh,
    recheck: store.recheck,
    ensureListener: store.ensureListener,
    install: store.install,
    update: store.update,
    apply: store.apply,
    clear: store.clear,
    downloadUrl: store.downloadUrl,
  };
}

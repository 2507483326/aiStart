import { storeToRefs } from "pinia";

import { useModelsStore } from "@/stores/models";

export function useModels() {
  const store = useModelsStore();
  return {
    ...storeToRefs(store),
    refresh: store.refresh,
    loadMeta: store.loadMeta,
    save: store.save,
    remove: store.remove,
    activate: store.activate,
    test: store.test,
    fetchUpstream: store.fetchUpstream,
    fetchUpstreamQuiet: store.fetchUpstreamQuiet,
    prefetchUpstream: store.prefetchUpstream,
  };
}

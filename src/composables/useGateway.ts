import { storeToRefs } from "pinia";

import { useGatewayStore } from "@/stores/gateway";

export function useGateway() {
  const store = useGatewayStore();
  return {
    ...storeToRefs(store),
    refresh: store.refresh,
    restart: store.restart,
    ensureListener: store.ensureListener,
  };
}

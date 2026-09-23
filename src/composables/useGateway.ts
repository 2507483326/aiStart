import { computed, ref } from "vue";

import { gatewayApi } from "@/lib/ipc";
import { attempt } from "@/lib/notify";
import type { GatewayStatus } from "@/lib/types";

const status = ref<GatewayStatus | null>(null);
const loading = ref(false);

async function refresh(): Promise<void> {
  loading.value = true;
  try {
    status.value = await gatewayApi.status();
  } finally {
    loading.value = false;
  }
}

async function start(): Promise<boolean> {
  const result = await attempt(() => gatewayApi.start(), {
    success: "本地网关已启动",
    error: "启动网关失败",
  });
  if (!result) return false;
  status.value = result;
  return true;
}

async function stop(): Promise<boolean> {
  const result = await attempt(() => gatewayApi.stop(), {
    success: "本地网关已停止",
    error: "停止网关失败",
  });
  if (!result) return false;
  status.value = result;
  return true;
}

async function restart(): Promise<boolean> {
  const result = await attempt(() => gatewayApi.restart(), {
    success: "本地网关已重启",
    error: "重启网关失败",
  });
  if (!result) return false;
  status.value = result;
  return true;
}

export function useGateway() {
  return {
    status,
    loading,
    running: computed(() => status.value?.running ?? false),
    refresh,
    start,
    stop,
    restart,
  };
}

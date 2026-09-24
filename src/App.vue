<script setup lang="ts">
import { onMounted, watch } from "vue";
import { useIntervalFn } from "@vueuse/core";

import AppShell from "@/components/layout/AppShell.vue";
import { Toaster } from "@/components/ui/sonner";
import { useGateway } from "@/composables/useGateway";
import { useModels } from "@/composables/useModels";
import { useSettings } from "@/composables/useSettings";

const gateway = useGateway();
const { refresh: refreshSettings } = useSettings();
const { refresh: refreshModels } = useModels();

onMounted(async () => {
  await gateway.ensureListener();
  await Promise.all([refreshSettings(), gateway.refresh()]);
});

// 自动切换成功后网关会把接手方写成当前模型，模型列表的「使用中」要跟着走。
// 网关状态里 activeModelId 一变就刷新模型，事件推送与 5 秒轮询都会触发这里。
watch(
  () => gateway.status.value?.activeModelId,
  (id, previous) => {
    if (id != null && id !== previous) refreshModels();
  },
);

// 事件推送是主路径，轮询只作兜底（漏事件或后端被外部改状态时补上）。
useIntervalFn(() => gateway.refresh(), 5000);
</script>

<template>
  <AppShell />
  <Toaster position="bottom-right" />
</template>

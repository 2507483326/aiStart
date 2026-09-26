<script setup lang="ts">
import { onMounted } from "vue";
import { RefreshCw, TriangleAlert } from "lucide";

import AppCard from "@/components/apps/AppCard.vue";
import EmptyState from "@/components/common/EmptyState.vue";
import MorphIconBox from "@/components/common/MorphIconBox.vue";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { useApps } from "@/composables/useApps";
import { useModels } from "@/composables/useModels";

const { apps, loading, refresh, recheck, ensureListener } = useApps();
const { models, activeModelId, refresh: refreshModels } = useModels();

onMounted(async () => {
  await ensureListener();
  await Promise.all([refresh(), refreshModels()]);
});
</script>

<template>
  <div class="mx-auto max-w-6xl space-y-5">
    <div class="flex items-center justify-between">
      <div>
        <h2 class="text-sm font-semibold">开发者工具</h2>
        <p class="text-xs text-muted-foreground">
          把任意模型的推理能力接入这些客户端。
        </p>
      </div>
      <Button variant="outline" size="sm" class="gap-2" :disabled="loading" @click="recheck">
        <MorphIconBox :icon="RefreshCw" :size="15" :class="loading ? 'animate-spin' : ''" />
        刷新
      </Button>
    </div>

    <Alert v-if="!models.length">
      <MorphIconBox :icon="TriangleAlert" :size="14" />
      <AlertTitle>还没有可用模型</AlertTitle>
      <AlertDescription>
        请先到「模型」标签页添加一个上游模型并启用，然后回到这里执行「一键应用模型」。
      </AlertDescription>
    </Alert>

    <div v-if="loading && !apps.length" class="grid gap-4 lg:grid-cols-2">
      <Skeleton v-for="index in 2" :key="index" class="h-64 w-full" />
    </div>

    <EmptyState
      v-else-if="!apps.length"
      icon="M4 4h6v6H4z M14 4h6v6h-6z M4 14h6v6H4z M14 14h6v6h-6z"
      title="未检测到可管理的应用"
    />

    <div v-else class="grid gap-4 lg:grid-cols-2">
      <AppCard
        v-for="app in apps"
        :key="app.kind"
        :app="app"
        :active-model-id="activeModelId"
      />
    </div>
  </div>
</template>

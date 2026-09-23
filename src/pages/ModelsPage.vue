<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { ArrowLeftRight, Plus, RefreshCw } from "lucide";

import EmptyState from "@/components/common/EmptyState.vue";
import MorphIconBox from "@/components/common/MorphIconBox.vue";
import ModelCard from "@/components/models/ModelCard.vue";
import ModelFormDialog from "@/components/models/ModelFormDialog.vue";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { useGateway } from "@/composables/useGateway";
import { useModels } from "@/composables/useModels";
import { useSettings } from "@/composables/useSettings";
import type { ModelConfig } from "@/lib/types";

const { models, activeModelId, loading, refresh, loadMeta } = useModels();
const gateway = useGateway();
const { settings, update } = useSettings();

const dialogOpen = ref(false);
const editing = ref<ModelConfig | null>(null);

const autoFailover = computed(() => settings.value?.autoFailover ?? false);

async function toggleFailover(value: boolean) {
  if (await update({ autoFailover: value })) {
    await gateway.refresh();
  }
}

function openCreate() {
  editing.value = null;
  dialogOpen.value = true;
}

function openEdit(model: ModelConfig) {
  editing.value = model;
  dialogOpen.value = true;
}

onMounted(async () => {
  await Promise.all([refresh(), loadMeta(), gateway.refresh()]);
});
</script>

<template>
  <div class="mx-auto max-w-6xl space-y-5">
    <div class="flex flex-wrap items-center justify-between gap-3">
      <div>
        <h2 class="text-sm font-semibold">模型列表</h2>
        <p class="text-xs text-muted-foreground">
          三种协议共用一套调用方式，网关负责请求与流式响应的双向翻译。
        </p>
      </div>

      <div class="flex flex-wrap items-center gap-2">
        <label
          class="flex cursor-pointer items-center gap-2 rounded-md border px-3 py-1.5"
          title="开启后，网关请求上游失败时会按列表顺序自动切换到下一个可用模型"
        >
          <MorphIconBox
            :icon="ArrowLeftRight"
            :size="15"
            :class="autoFailover ? 'text-emerald-500' : 'text-muted-foreground'"
          />
          <span class="text-xs font-medium">自动切换</span>
          <Switch
            :model-value="autoFailover"
            :disabled="!models.length"
            @update:model-value="toggleFailover"
          />
        </label>

        <Button variant="outline" size="sm" class="gap-2" :disabled="loading" @click="refresh">
          <MorphIconBox
            :icon="RefreshCw"
            :size="15"
            :class="loading ? 'animate-spin' : ''"
          />
          刷新
        </Button>

        <Button size="sm" class="gap-2" @click="openCreate">
          <MorphIconBox :icon="Plus" :size="15" />
          添加模型
        </Button>
      </div>
    </div>

    <div v-if="loading && !models.length" class="space-y-2">
      <Skeleton v-for="index in 3" :key="index" class="h-16 w-full" />
    </div>

    <EmptyState
      v-else-if="!models.length"
      icon="M12 3v18 M3 12h18"
      title="还没有模型"
      description="添加一个上游模型后即可启用本地网关，并把推理能力接入桌面客户端。"
    >
      <Button size="sm" class="gap-2" @click="openCreate">
        <MorphIconBox :icon="Plus" :size="15" />
        添加模型
      </Button>
    </EmptyState>

    <div v-else class="space-y-2">
      <ModelCard
        v-for="model in models"
        :key="model.id"
        :model="model"
        :active="model.id === activeModelId"
        @edit="openEdit"
      />
    </div>

    <ModelFormDialog v-model:open="dialogOpen" :model="editing" @saved="gateway.refresh" />
  </div>
</template>

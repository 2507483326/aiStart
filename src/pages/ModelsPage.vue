<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { ArrowLeftRight, BrainCircuit, Plus, RefreshCw } from "lucide";

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

const { models, activeModelId, loading, refresh, loadMeta, prefetchUpstream } = useModels();
const gateway = useGateway();
const { settings, update } = useSettings();

const dialogOpen = ref(false);
const editing = ref<ModelConfig | null>(null);

const PAGE_SIZE = 10;
const page = ref(1);

const pageCount = computed(() => Math.max(1, Math.ceil(models.value.length / PAGE_SIZE)));
const pagedModels = computed(() => {
  const start = (page.value - 1) * PAGE_SIZE;
  return models.value.slice(start, start + PAGE_SIZE);
});
const rangeLabel = computed(() => {
  if (!models.value.length) return "共 0 条";
  const from = (page.value - 1) * PAGE_SIZE + 1;
  const to = Math.min(page.value * PAGE_SIZE, models.value.length);
  return `第 ${from}–${to} 条，共 ${models.value.length} 条`;
});

// 删除导致当前页越界时，退回最后一页
watch(pageCount, (count) => {
  if (page.value > count) page.value = count;
});

function changePage(next: number): void {
  page.value = Math.min(Math.max(1, next), pageCount.value);
}

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

async function reload() {
  try {
    await refresh();
  } finally {
    prefetchUpstream();
  }
}

async function handleSaved() {
  try {
    await gateway.refresh();
  } finally {
    prefetchUpstream();
  }
}

onMounted(async () => {
  await Promise.allSettled([refresh(), loadMeta(), gateway.refresh()]);
  prefetchUpstream();
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
          class="flex cursor-pointer items-center gap-2 rounded-md border bg-background px-3 py-1.5 shadow-xs transition-colors hover:bg-accent/50"
          title="开启后，网关请求上游失败时会按列表顺序自动切换到下一个可用模型，并把它设为当前模型"
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

        <Button variant="outline" size="sm" class="gap-2" :disabled="loading" @click="reload">
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
      :icon="BrainCircuit"
      title="还没有模型"
      description="添加一个上游模型后即可启用本地网关，并把推理能力接入桌面客户端。"
    >
      <Button size="sm" class="gap-2" @click="openCreate">
        <MorphIconBox :icon="Plus" :size="15" />
        添加模型
      </Button>
    </EmptyState>

    <div v-else class="space-y-3">
      <div class="space-y-2">
        <ModelCard
          v-for="model in pagedModels"
          :key="model.id"
          :model="model"
          :active="model.id === activeModelId"
          @edit="openEdit"
        />
      </div>

      <div
        v-if="pageCount > 1"
        class="flex items-center justify-between gap-3 border-t pt-3 text-xs text-muted-foreground"
      >
        <div class="flex items-center gap-2">
          <Button
            variant="outline"
            size="xs"
            :disabled="page <= 1 || loading"
            @click="changePage(page - 1)"
          >
            上一页
          </Button>
          <Button
            variant="outline"
            size="xs"
            :disabled="page >= pageCount || loading"
            @click="changePage(page + 1)"
          >
            下一页
          </Button>
          <span class="tabular-nums">第 {{ page }} / {{ pageCount }} 页</span>
        </div>
        <span>{{ rangeLabel }}</span>
      </div>
    </div>

    <ModelFormDialog v-model:open="dialogOpen" :model="editing" @saved="handleSaved" />
  </div>
</template>

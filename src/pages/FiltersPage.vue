<script setup lang="ts">
import { onMounted, ref } from "vue";
import { Plus } from "lucide";

import EmptyState from "@/components/common/EmptyState.vue";
import MorphIconBox from "@/components/common/MorphIconBox.vue";
import FilterCard from "@/components/filters/FilterCard.vue";
import FilterFormDialog from "@/components/filters/FilterFormDialog.vue";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { useFilters } from "@/composables/useFilters";
import type { RequestFilter } from "@/lib/types";

const { filters, loading, refresh } = useFilters();

const dialogOpen = ref(false);
const editing = ref<RequestFilter | null>(null);

function openCreate() {
  editing.value = null;
  dialogOpen.value = true;
}

function openEdit(filter: RequestFilter) {
  editing.value = filter;
  dialogOpen.value = true;
}

onMounted(() => {
  refresh();
});
</script>

<template>
  <div class="mx-auto max-w-6xl space-y-5">
    <div class="flex flex-wrap items-center justify-between gap-3">
      <div>
        <h2 class="text-sm font-semibold">提示词注入列表</h2>
        <p class="text-xs text-muted-foreground">
          启用的注入规则会在请求转发给上游之前，按列表顺序依次注入系统提示词。
        </p>
      </div>

      <Button size="sm" class="gap-2" @click="openCreate">
        <MorphIconBox :icon="Plus" :size="15" />
        添加提示词注入
      </Button>
    </div>

    <div v-if="loading && !filters.length" class="space-y-2">
      <Skeleton v-for="index in 3" :key="index" class="h-16 w-full" />
    </div>

    <EmptyState
      v-else-if="!filters.length"
      icon="M3 5h18l-7 8v6l-4-2v-4z"
      title="还没有提示词注入"
      description="添加一条注入规则，就能在请求转发给上游前为请求注入系统提示词。"
    >
      <Button size="sm" class="gap-2" @click="openCreate">
        <MorphIconBox :icon="Plus" :size="15" />
        添加提示词注入
      </Button>
    </EmptyState>

    <div v-else class="space-y-2">
      <FilterCard
        v-for="filter in filters"
        :key="filter.id"
        :filter="filter"
        @edit="openEdit"
      />
    </div>

    <FilterFormDialog v-model:open="dialogOpen" :filter="editing" />
  </div>
</template>

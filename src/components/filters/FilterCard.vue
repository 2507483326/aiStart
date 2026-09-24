<script setup lang="ts">
import { computed } from "vue";
import { Pencil, Trash2 } from "lucide";

import ConfirmDialog from "@/components/common/ConfirmDialog.vue";
import MorphIconBox from "@/components/common/MorphIconBox.vue";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { useFilters } from "@/composables/useFilters";
import type { PromptMode, RequestFilter } from "@/lib/types";

const props = defineProps<{ filter: RequestFilter }>();
const emit = defineEmits<{ edit: [filter: RequestFilter] }>();

const { setEnabled, remove } = useFilters();

const KIND_LABEL = "系统提示词";

const MODE_LABELS: Record<PromptMode, string> = {
  append: "追加到末尾",
  prepend: "插入到开头",
};

const summary = computed(() => {
  const rule = props.filter.rule;
  return `${MODE_LABELS[rule.mode]}：${truncate(rule.text)}`;
});

function truncate(text: string): string {
  return text.length > 40 ? `${text.slice(0, 40)}…` : text;
}
</script>

<template>
  <div
    class="flex items-center gap-4 rounded-lg border bg-card px-4 py-3 transition-[border-color,background-color] duration-200"
    :class="
      filter.enabled ? 'border-emerald-500/40' : 'hover:border-foreground/20 hover:bg-accent/30'
    "
  >
    <div class="min-w-0 flex-1">
      <div class="flex flex-wrap items-center gap-2">
        <p class="truncate text-sm font-medium">{{ filter.name }}</p>
        <Badge variant="outline">{{ KIND_LABEL }}</Badge>
      </div>
      <p class="mt-0.5 truncate font-mono text-xs text-muted-foreground">{{ summary }}</p>
    </div>

    <div class="flex shrink-0 items-center gap-3">
      <label class="flex cursor-pointer items-center gap-1.5">
        <span
          class="text-xs font-medium"
          :class="
            filter.enabled ? 'text-emerald-600 dark:text-emerald-400' : 'text-muted-foreground'
          "
        >
          {{ filter.enabled ? "启用" : "停用" }}
        </span>
        <Switch
          :model-value="filter.enabled"
          @update:model-value="(value) => setEnabled(filter.id, value)"
        />
      </label>

      <Button variant="ghost" size="icon-xs" @click="emit('edit', filter)">
        <MorphIconBox :icon="Pencil" :size="14" />
      </Button>
      <ConfirmDialog
        title="删除提示词注入"
        :description="`确定删除「${filter.name}」吗？该操作不可撤销。`"
        confirm-text="删除"
        destructive
        @confirm="remove(filter.id)"
      >
        <template #trigger>
          <Button variant="ghost" size="icon-xs">
            <MorphIconBox :icon="Trash2" :size="14" class="text-destructive" />
          </Button>
        </template>
      </ConfirmDialog>
    </div>
  </div>
</template>

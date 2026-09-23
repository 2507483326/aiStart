<script setup lang="ts">
import { computed, ref } from "vue";
import { CircleCheck, LoaderCircle, Pencil, Play, Trash2, Wifi } from "lucide";

import ConfirmDialog from "@/components/common/ConfirmDialog.vue";
import FormatBadge from "@/components/common/FormatBadge.vue";
import MorphIconBox from "@/components/common/MorphIconBox.vue";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { useModels } from "@/composables/useModels";
import { formatLatency } from "@/lib/format";
import { cn } from "@/lib/utils";
import type { ModelConfig, TestResult } from "@/lib/types";

const props = defineProps<{
  model: ModelConfig;
  active: boolean;
}>();

const emit = defineEmits<{ edit: [model: ModelConfig] }>();

const { activate, remove, test, testingId } = useModels();

const testing = computed(() => testingId.value === props.model.id);
const result = ref<TestResult | null>(null);

async function runTest() {
  result.value = null;
  const outcome = await test(props.model.id);
  if (outcome) result.value = outcome;
}
</script>

<template>
  <div
    class="flex items-center gap-4 rounded-lg border bg-card px-4 py-3 transition-colors"
    :class="active ? 'border-emerald-500/40' : ''"
  >
    <div class="min-w-0 flex-1">
      <div class="flex flex-wrap items-center gap-2">
        <p class="truncate text-sm font-medium">{{ model.name }}</p>
        <Badge v-if="model.supports1m" variant="outline">1M</Badge>
        <FormatBadge :format="model.format" />
        <Badge
          v-if="active"
          variant="outline"
          class="border-transparent bg-emerald-500/15 text-emerald-600 dark:text-emerald-400"
        >
          <MorphIconBox :icon="CircleCheck" :size="12" />
          使用中
        </Badge>
      </div>
      <p class="mt-0.5 truncate font-mono text-xs text-muted-foreground">
        {{ model.model }} · {{ model.baseUrl }}
      </p>
      <p
        v-if="result"
        class="mt-0.5 truncate text-xs"
        :class="result.ok ? 'text-emerald-600 dark:text-emerald-400' : 'text-destructive'"
      >
        {{ result.ok ? "连通正常" : "连通失败" }} · {{ formatLatency(result.latencyMs) }} ·
        {{ result.preview ?? result.message }}
      </p>
    </div>

    <div class="flex shrink-0 items-center gap-1.5">
      <Button size="sm" variant="outline" class="gap-2" :disabled="testing" @click="runTest">
        <MorphIconBox
          :icon="testing ? LoaderCircle : Wifi"
          :size="15"
          :class="testing ? 'animate-spin' : ''"
        />
        测试
      </Button>

      <Button
        size="sm"
        class="gap-2"
        :variant="active ? 'outline' : 'default'"
        :class="
          cn(
            active &&
              'border-emerald-500/40 bg-emerald-500/10 text-emerald-600 hover:bg-emerald-500/10 dark:text-emerald-400',
          )
        "
        :disabled="active"
        @click="activate(model.id)"
      >
        <MorphIconBox :icon="active ? CircleCheck : Play" :size="15" />
        {{ active ? "使用中" : "启用" }}
      </Button>

      <Button variant="ghost" size="icon-sm" @click="emit('edit', model)">
        <MorphIconBox :icon="Pencil" :size="15" />
      </Button>
      <ConfirmDialog
        title="删除模型"
        :description="`确定删除「${model.name}」吗？该操作不可撤销。`"
        confirm-text="删除"
        destructive
        @confirm="remove(model.id)"
      >
        <template #trigger>
          <Button variant="ghost" size="icon-sm">
            <MorphIconBox :icon="Trash2" :size="15" class="text-destructive" />
          </Button>
        </template>
      </ConfirmDialog>
    </div>
  </div>
</template>

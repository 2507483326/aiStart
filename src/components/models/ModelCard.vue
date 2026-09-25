<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from "vue";
import { CircleCheck, Copy, LoaderCircle, Pencil, Play, Trash2, Wifi } from "lucide";

import ConfirmDialog from "@/components/common/ConfirmDialog.vue";
import FormatBadge from "@/components/common/FormatBadge.vue";
import MorphIconBox from "@/components/common/MorphIconBox.vue";
import UpstreamModelSelect from "@/components/models/UpstreamModelSelect.vue";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { useModels } from "@/composables/useModels";
import { formatLatency } from "@/lib/format";
import { cn } from "@/lib/utils";
import type { ModelConfig, TestResult } from "@/lib/types";

const props = defineProps<{
  model: ModelConfig;
  active: boolean;
}>();

const emit = defineEmits<{ edit: [model: ModelConfig] }>();

const { activate, duplicate, remove, test, save, prefetchUpstream, upstreamCache } = useModels();

const testing = ref(false);
const result = ref<TestResult | null>(null);
const modelId = ref(props.model.model);
const upstreamOptions = computed(() => upstreamCache.value[props.model.id] ?? []);

watch(
  () => props.model.model,
  (value) => {
    modelId.value = value;
  },
);

let clearTimer: ReturnType<typeof setTimeout> | undefined;

async function runTest() {
  result.value = null;
  if (clearTimer) clearTimeout(clearTimer);
  testing.value = true;
  try {
    const outcome = await test(props.model.id);
    if (!outcome) return;
    result.value = outcome;
    if (outcome.ok) {
      clearTimer = setTimeout(() => {
        result.value = null;
      }, 5000);
    }
  } finally {
    testing.value = false;
  }
}

onUnmounted(() => {
  if (clearTimer) clearTimeout(clearTimer);
});

async function setModelId(value: string) {
  modelId.value = value;
  await commitModelId();
}

async function commitModelId() {
  const next = modelId.value.trim();
  if (!next || next === props.model.model) {
    modelId.value = props.model.model;
    return;
  }
  const saved = await save({
    id: props.model.id,
    name: props.model.name,
    format: props.model.format,
    baseUrl: props.model.baseUrl,
    apiKey: props.model.apiKey,
    model: next,
    supports1m: props.model.supports1m,
  });
  if (!saved) modelId.value = props.model.model;
}

function blurOnEnter(event: KeyboardEvent) {
  (event.target as HTMLInputElement).blur();
}

async function duplicateModel() {
  if (await duplicate(props.model)) prefetchUpstream();
}
</script>

<template>
  <div
    class="flex items-center gap-4 rounded-lg border bg-card px-4 py-3 transition-[border-color,box-shadow,background-color] duration-200 hover:shadow-sm"
    :class="active ? 'border-emerald-500/40' : 'hover:border-foreground/20 hover:bg-accent/30'"
  >
    <div class="min-w-0 flex-1">
      <div class="flex flex-wrap items-center gap-2">
        <p class="truncate text-sm font-semibold">{{ model.name }}</p>
        <FormatBadge :format="model.format" />
      </div>
      <p class="mt-0.5 truncate font-mono text-xs text-muted-foreground">
        {{ model.baseUrl }}
      </p>
      <p
        v-if="result"
        class="mt-0.5 truncate text-xs"
        :class="result.ok ? 'text-emerald-600 dark:text-emerald-400' : 'text-destructive'"
      >
        {{ result.ok ? "连通正常" : "连通失败" }} · {{ formatLatency(result.latencyMs) }} ·
        <span :title="result.proxied ? '这次测试经代理出站' : '这次测试直连（代理未启用，或上游是本机地址）'">
          {{ result.proxied ? "经代理" : "直连" }}
        </span>
        · {{ result.preview ?? result.message }}
      </p>
    </div>

    <div class="flex shrink-0 items-center gap-1">
      <UpstreamModelSelect
        v-if="upstreamOptions.length"
        :model-value="modelId"
        :options="upstreamOptions"
        size="sm"
        trigger-class="mr-1"
        placeholder="上游模型 ID"
        @update:model-value="setModelId"
      />
      <Input
        v-else
        v-model="modelId"
        class="mr-1 h-7 w-56 px-2 font-mono text-xs"
        spellcheck="false"
        aria-label="上游模型 ID"
        title="上游模型 ID，失焦或回车后自动保存"
        @blur="commitModelId"
        @keydown.enter="blurOnEnter"
      />
      <Button
        size="xs"
        class="gap-1"
        :variant="active ? 'outline' : 'default'"
        :class="
          cn(
            active &&
              'border-transparent bg-emerald-500 text-white hover:bg-emerald-500 disabled:opacity-100 dark:bg-emerald-500 dark:text-white',
          )
        "
        :disabled="active"
        @click="activate(model.id)"
      >
        <MorphIconBox :icon="active ? CircleCheck : Play" :size="14" />
        <span class="w-9 text-center">{{ active ? "使用中" : "启用" }}</span>
      </Button>

      <Button size="xs" variant="outline" class="gap-1" :disabled="testing" @click="runTest">
        <MorphIconBox
          :icon="testing ? LoaderCircle : Wifi"
          :size="14"
          :class="testing ? 'animate-spin' : ''"
        />
        测试
      </Button>

      <Button variant="ghost" size="icon-xs" title="复制模型" @click="duplicateModel">
        <MorphIconBox :icon="Copy" :size="14" />
      </Button>
      <Button variant="ghost" size="icon-xs" @click="emit('edit', model)">
        <MorphIconBox :icon="Pencil" :size="14" />
      </Button>
      <ConfirmDialog
        title="删除模型"
        :description="`确定删除「${model.name}」吗？该操作不可撤销。`"
        confirm-text="删除"
        destructive
        @confirm="remove(model.id)"
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

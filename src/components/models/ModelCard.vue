<script setup lang="ts">
import { computed, ref } from "vue";
import { CircleCheck, LoaderCircle, Pencil, Play, Trash2, Wifi } from "lucide";

import ConfirmDialog from "@/components/common/ConfirmDialog.vue";
import FormatBadge from "@/components/common/FormatBadge.vue";
import MorphIconBox from "@/components/common/MorphIconBox.vue";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { useModels } from "@/composables/useModels";
import { formatLatency, maskSecret } from "@/lib/format";
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
  <Card class="gap-3">
    <CardHeader>
      <div class="flex items-start justify-between gap-3">
        <div class="min-w-0 space-y-1">
          <CardTitle class="truncate text-base">{{ model.name }}</CardTitle>
          <CardDescription class="truncate font-mono text-xs">
            {{ model.model }}
          </CardDescription>
        </div>
        <div class="flex shrink-0 items-center gap-1.5">
          <Badge v-if="model.supports1m" variant="outline">1M</Badge>
          <FormatBadge :format="model.format" />
          <Badge v-if="active" variant="secondary">
            <MorphIconBox :icon="CircleCheck" :size="12" />
            启用中
          </Badge>
        </div>
      </div>
    </CardHeader>

    <CardContent class="space-y-2 text-xs">
      <div class="grid grid-cols-2 gap-x-6 gap-y-2">
        <div class="col-span-2">
          <p class="text-muted-foreground">Base URL</p>
          <p class="break-all font-mono">{{ model.baseUrl }}</p>
        </div>
        <div>
          <p class="text-muted-foreground">API Key</p>
          <p class="font-mono">{{ maskSecret(model.apiKey) }}</p>
        </div>
        <div>
          <p class="text-muted-foreground">上下文窗口</p>
          <p>{{ model.supports1m ? "1M tokens" : "上游默认" }}</p>
        </div>
      </div>

      <div
        v-if="result"
        class="flex items-start gap-2 rounded-md border px-3 py-2"
        :class="
          result.ok
            ? 'border-emerald-500/30 bg-emerald-500/5 text-emerald-700 dark:text-emerald-400'
            : 'border-destructive/30 bg-destructive/5 text-destructive'
        "
      >
        <MorphIconBox :icon="Wifi" :size="14" class="mt-0.5 shrink-0" />
        <div class="min-w-0">
          <p class="font-medium">
            {{ result.ok ? "连通正常" : "连通失败" }} ·
            {{ formatLatency(result.latencyMs) }}
          </p>
          <p class="mt-0.5 break-words opacity-80">
            {{ result.preview ?? result.message }}
          </p>
        </div>
      </div>
    </CardContent>

    <CardFooter class="flex-wrap gap-2">
      <Button
        size="sm"
        variant="outline"
        class="gap-2"
        :disabled="testing"
        @click="runTest"
      >
        <MorphIconBox
          :icon="testing ? LoaderCircle : Wifi"
          :size="15"
          :class="testing ? 'animate-spin' : ''"
        />
        测试连通
      </Button>

      <Button size="sm" class="gap-2" :disabled="active" @click="activate(model.id)">
        <MorphIconBox :icon="active ? CircleCheck : Play" :size="15" />
        {{ active ? "已启用" : "设为网关模型" }}
      </Button>

      <div class="ml-auto flex items-center gap-1">
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
    </CardFooter>
  </Card>
</template>

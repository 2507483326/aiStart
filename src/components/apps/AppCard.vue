<script setup lang="ts">
import { computed, ref, watch } from "vue";
import {
  CircleCheck,
  Download,
  ExternalLink,
  LoaderCircle,
  Plug,
  RefreshCw,
  Route,
  Trash2,
} from "lucide";

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
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useApps } from "@/composables/useApps";
import { applyModeLabels } from "@/lib/format";
import { openUrl } from "@/lib/open";
import type { ModelConfig, ToolApp } from "@/lib/types";

const props = defineProps<{
  app: ToolApp;
  models: ModelConfig[];
  activeModelId: string | null;
}>();

const { busyKind, download, apply, clear, install, update } = useApps();

const selectedModelId = ref<string>("");

watch(
  () => [props.app.appliedModelId, props.activeModelId, props.models.length] as const,
  () => {
    if (selectedModelId.value) return;
    selectedModelId.value =
      props.app.appliedModelId ?? props.activeModelId ?? props.models[0]?.id ?? "";
  },
  { immediate: true },
);

const busy = computed(() => busyKind.value === props.app.kind);
const installIcon = computed(() => (props.app.installed ? RefreshCw : Download));
const applyIcon = computed(() =>
  props.app.appliedModelId ? CircleCheck : Plug,
);
const progress = computed(() => {
  const current = download.value;
  if (!current || current.kind !== props.app.kind || current.phase !== "downloading") {
    return null;
  }
  return current;
});

async function doApply() {
  await apply(props.app.kind, selectedModelId.value || undefined);
}
</script>

<template>
  <Card class="gap-4">
    <CardHeader>
      <div class="flex items-start justify-between gap-4">
        <div class="space-y-1">
          <CardTitle class="text-base">{{ app.name }}</CardTitle>
          <CardDescription class="text-xs">
            {{ app.publisher }} · {{ app.description }}
          </CardDescription>
        </div>
        <div class="flex shrink-0 flex-col items-end gap-1.5">
          <Badge :variant="app.installed ? 'default' : 'outline'">
            {{ app.installed ? "已安装" : "未安装" }}
          </Badge>
          <span v-if="app.version" class="font-mono text-[11px] text-muted-foreground">
            v{{ app.version }}
          </span>
        </div>
      </div>
    </CardHeader>

    <CardContent class="space-y-3 text-xs">
      <div class="grid grid-cols-2 gap-x-6 gap-y-2">
        <div>
          <p class="text-muted-foreground">接入方式</p>
          <p class="font-medium">{{ applyModeLabels[app.applyMode] }}</p>
        </div>
        <div>
          <p class="text-muted-foreground">当前模型</p>
          <p class="font-medium">
            <template v-if="app.appliedModelName">
              <span class="text-emerald-600 dark:text-emerald-400">
                {{ app.appliedModelName }}
              </span>
            </template>
            <template v-else>
              <span class="text-muted-foreground">未接入</span>
            </template>
          </p>
        </div>
        <div class="col-span-2">
          <p class="text-muted-foreground">配置位置</p>
          <p class="break-all font-mono">{{ app.configTarget }}</p>
        </div>
        <div v-if="app.installLocation" class="col-span-2">
          <p class="text-muted-foreground">安装路径</p>
          <p class="break-all font-mono">{{ app.installLocation }}</p>
        </div>
      </div>

      <div
        v-if="progress"
        class="space-y-1.5 rounded-md border bg-muted/40 px-3 py-2"
      >
        <div class="flex items-center justify-between text-[11px]">
          <span>正在下载安装包…</span>
          <span class="tabular-nums">{{ progress.percent?.toFixed(1) ?? "—" }}%</span>
        </div>
        <div class="h-1 overflow-hidden rounded-full bg-border">
          <div
            class="h-full bg-primary transition-all"
            :style="{ width: `${progress.percent ?? 0}%` }"
          />
        </div>
      </div>
    </CardContent>

    <CardFooter class="flex-wrap gap-2">
      <Select v-model="selectedModelId" :disabled="!models.length">
        <SelectTrigger size="sm" class="min-w-52 flex-1">
          <SelectValue>
            {{
              models.find((item) => item.id === selectedModelId)?.name ??
              "选择要接入的模型"
            }}
          </SelectValue>
        </SelectTrigger>
        <SelectContent>
          <SelectItem v-for="model in models" :key="model.id" :value="model.id">
            {{ model.name }}
          </SelectItem>
        </SelectContent>
      </Select>

      <Button
        size="sm"
        class="gap-2"
        :disabled="busy || !selectedModelId"
        @click="doApply"
      >
        <MorphIconBox
          :icon="busy ? LoaderCircle : applyIcon"
          :size="15"
          :class="busy ? 'animate-spin' : ''"
        />
        一键应用模型
      </Button>

      <Button
        variant="outline"
        size="sm"
        class="gap-2"
        :disabled="busy"
        @click="app.installed ? update(app.kind) : install(app.kind)"
      >
        <MorphIconBox :icon="installIcon" :size="15" />
        {{ app.installed ? "一键更新" : "一键安装" }}
      </Button>

      <DropdownMenu>
        <DropdownMenuTrigger as-child>
          <Button variant="ghost" size="icon-sm">
            <MorphIconBox :icon="Route" :size="15" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuItem
            :disabled="!app.appliedModelId"
            @select="clear(app.kind)"
          >
            <MorphIconBox :icon="Trash2" :size="14" />
            移除模型配置
          </DropdownMenuItem>
          <DropdownMenuItem @select="openUrl(app.downloadPage)">
            <MorphIconBox :icon="ExternalLink" :size="14" />
            打开官方下载页
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </CardFooter>
  </Card>
</template>

<script setup lang="ts">
import { computed, ref } from "vue";
import {
  CircleArrowUp,
  CircleCheck,
  Copy,
  Download,
  ExternalLink,
  LoaderCircle,
  Plug,
  RefreshCw,
  Route,
  Trash2,
} from "lucide";

import MorphIconBox from "@/components/common/MorphIconBox.vue";
import ManualGuideDialog from "@/components/apps/ManualGuideDialog.vue";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
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
import { useApps } from "@/composables/useApps";
import { notifySuccess } from "@/lib/notify";
import { openUrl } from "@/lib/open";
import type { AppKind, ToolApp } from "@/lib/types";

const props = defineProps<{
  app: ToolApp;
  activeModelId: number | null;
}>();

const { busyKind, download, apply, clear, install, update } = useApps();

const APP_ICONS: Record<AppKind, string> = {
  "claude-desktop": "/app-icons/claude-desktop.svg",
  "deepseek-desktop": "/app-icons/deepseek-desktop.png",
  codex: "/app-icons/codex.svg",
  zcode: "/app-icons/zcode.svg",
  workbuddy: "/app-icons/workbuddy.svg",
};

const busy = computed(() => busyKind.value === props.app.kind);
const guideOpen = ref(false);
const installIcon = computed(() => (props.app.installed ? RefreshCw : Download));
const applyIcon = computed(() =>
  props.app.appliedModelId ? CircleCheck : Plug,
);
const icon = computed(() => APP_ICONS[props.app.kind]);
const status = computed(() => {
  if (props.app.updateAvailable) {
    return { label: "新版本", variant: "default" as const, upgrade: true };
  }
  return props.app.installed
    ? { label: "已安装", variant: "default" as const, upgrade: false }
    : { label: "未安装", variant: "outline" as const, upgrade: false };
});
const versionText = computed(() => {
  if (!props.app.installed) return "未安装";
  const current = props.app.version ?? "未知";
  return props.app.updateAvailable && props.app.latestVersion
    ? `${current} → ${props.app.latestVersion}`
    : current;
});
const progress = computed(() => {
  const current = download.value;
  if (!current || current.kind !== props.app.kind || current.phase !== "downloading") {
    return null;
  }
  return current;
});

async function doApply() {
  // 手动应用的应用没有可写入的配置：应用成功后弹出「对接说明」，
  // 让用户照着在客户端 GUI 里填写。
  const applied = await apply(props.app.kind);
  if (applied && props.app.applyMode === "manual") {
    guideOpen.value = true;
  }
}

async function copyApiKey() {
  await navigator.clipboard.writeText(props.app.apiKey);
  notifySuccess("API Key 已复制");
}
</script>

<template>
  <Card
    class="gap-4 transition-[border-color,box-shadow,background-color] duration-200 hover:border-foreground/20 hover:bg-accent/[0.03] hover:shadow-md"
  >
    <CardHeader>
      <div class="flex items-start justify-between gap-4">
        <div class="flex min-w-0 items-center gap-3">
          <img :src="icon" :alt="app.name" class="size-10 shrink-0 object-contain" />
          <div class="min-w-0 space-y-0.5">
            <CardTitle>{{ app.name }}</CardTitle>
            <p class="truncate text-xs text-muted-foreground">
              {{ app.publisher }}
            </p>
          </div>
        </div>
        <Badge
          class="shrink-0"
          :variant="status.variant"
          :class="status.upgrade ? 'border-transparent bg-emerald-500 text-white' : ''"
        >
          <MorphIconBox v-if="status.upgrade" :icon="CircleArrowUp" :size="12" />
          {{ status.label }}
        </Badge>
      </div>
    </CardHeader>

    <CardContent class="space-y-1 font-mono text-xs">
      <p class="truncate">
        <span class="text-muted-foreground">版本号: </span>{{ versionText }}
      </p>
      <p class="truncate">
        <span class="text-muted-foreground">官网地址: </span>
        <button
          type="button"
          class="text-blue-600 underline-offset-4 hover:underline dark:text-blue-400"
          @click="openUrl(app.homepage)"
        >
          {{ app.homepage }}
        </button>
      </p>
      <p class="flex items-center gap-1.5 truncate">
        <span class="text-muted-foreground">API Key: </span>
        <span class="min-w-0 truncate">{{ app.apiKey }}</span>
        <button
          type="button"
          class="shrink-0 text-muted-foreground transition-colors hover:text-foreground"
          title="复制 API Key"
          @click="copyApiKey"
        >
          <MorphIconBox :icon="Copy" :size="13" />
        </button>
      </p>
    </CardContent>

    <CardContent v-if="progress">
      <div class="space-y-1.5 rounded-md border bg-muted/40 px-3 py-2 text-xs">
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

    <CardFooter class="mt-auto gap-3">
      <Button
        size="xs"
        class="gap-1"
        :disabled="busy || !activeModelId"
        @click="doApply"
      >
        <MorphIconBox
          :icon="busy ? LoaderCircle : applyIcon"
          :size="15"
          :class="busy ? 'animate-spin' : ''"
        />
        应用
      </Button>

      <Button
        variant="outline"
        size="xs"
        class="gap-1"
        :disabled="busy"
        @click="app.installed ? update(app.kind) : install(app.kind)"
      >
        <MorphIconBox :icon="installIcon" :size="15" />
        {{ app.installed ? "升级" : "安装" }}
      </Button>

      <DropdownMenu>
        <DropdownMenuTrigger as-child>
          <Button variant="outline" size="icon-xs" class="ml-auto">
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

  <ManualGuideDialog v-model:open="guideOpen" :app="app" />
</template>

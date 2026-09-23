<script setup lang="ts">
import { computed } from "vue";
import { Activity, ArrowLeftRight, Copy, LoaderCircle, Power, RefreshCw, Route } from "lucide";

import MorphIconBox from "@/components/common/MorphIconBox.vue";
import StatTile from "@/components/common/StatTile.vue";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { useGateway } from "@/composables/useGateway";
import { formatCompact, formatNumber } from "@/lib/format";
import { notifySuccess } from "@/lib/notify";

const { status, running, loading, start, stop, restart } = useGateway();

const statusIcon = computed(() => (running.value ? Activity : Power));

const sample = computed(
  () =>
    `curl ${status.value?.baseUrl ?? "http://127.0.0.1:8931"}/v1/messages \\
  -H "content-type: application/json" \\
  -H "x-api-key: ${status.value?.token ?? "<token>"}" \\
  -d '{"model":"any","max_tokens":64,"messages":[{"role":"user","content":"ping"}]}'`,
);

async function copySample() {
  await navigator.clipboard.writeText(sample.value);
  notifySuccess("调用示例已复制");
}
</script>

<template>
  <Card>
    <CardHeader>
      <div class="flex items-start justify-between gap-3">
        <div class="space-y-1">
          <CardTitle class="text-base">本地网关</CardTitle>
          <CardDescription class="text-xs">
            对外暴露 Anthropic Messages 协议，内部按上游协议转发并流式翻译。
          </CardDescription>
        </div>
        <Badge :variant="running ? 'default' : 'outline'" class="gap-1.5">
          <MorphIconBox :icon="statusIcon" :size="12" />
          {{ running ? "运行中" : "已停止" }}
        </Badge>
      </div>
    </CardHeader>

    <CardContent class="space-y-4">
      <div class="grid grid-cols-4 gap-3">
        <StatTile label="监听地址" :value="status?.baseUrl ?? '—'" />
        <StatTile label="累计请求" :value="formatNumber(status?.requests ?? 0)" />
        <StatTile
          label="错误次数"
          :value="formatNumber(status?.errors ?? 0)"
          :tone="(status?.errors ?? 0) > 0 ? 'danger' : 'default'"
        />
        <StatTile
          label="输出 Token"
          :value="formatCompact(status?.outputTokens ?? 0)"
          :hint="`输入 ${formatCompact(status?.inputTokens ?? 0)}`"
        />
      </div>

      <div class="flex flex-wrap items-center gap-2 text-xs">
        <span class="text-muted-foreground">接管模型</span>
        <Badge v-if="status?.activeModelName" variant="secondary">
          {{ status.activeModelName }}
        </Badge>
        <span v-else class="text-muted-foreground">尚未设置</span>
        <span v-if="status?.activeModelFormat" class="font-mono text-muted-foreground">
          {{ status.activeModelFormat }}
        </span>
      </div>

      <div class="flex flex-wrap items-center gap-2 text-xs">
        <span class="flex items-center gap-1.5 text-muted-foreground">
          <MorphIconBox :icon="ArrowLeftRight" :size="13" />
          自动切换
        </span>
        <Badge :variant="status?.autoFailover ? 'default' : 'outline'">
          {{ status?.autoFailover ? "已开启" : "已关闭" }}
        </Badge>
        <template v-if="status?.autoFailover">
          <span class="text-muted-foreground">
            已触发 {{ status.failovers }} 次
          </span>
          <span v-if="status.lastFailover" class="font-mono text-muted-foreground">
            {{ status.lastFailover }}
          </span>
        </template>
        <span v-else class="text-muted-foreground">
          上游失败时按模型列表顺序自动尝试下一个
        </span>
      </div>

      <Alert v-if="status?.lastError" variant="destructive">
        <MorphIconBox :icon="Route" :size="14" />
        <AlertTitle>最近一次网关错误</AlertTitle>
        <AlertDescription class="break-words font-mono text-xs">
          {{ status.lastError }}
        </AlertDescription>
      </Alert>

      <div class="space-y-1.5">
        <div class="flex items-center justify-between">
          <p class="text-xs text-muted-foreground">调用示例</p>
          <Button variant="ghost" size="sm" class="gap-1.5" @click="copySample">
            <MorphIconBox :icon="Copy" :size="13" />
            复制
          </Button>
        </div>
        <pre
          class="overflow-x-auto rounded-md border bg-muted/40 px-3 py-2 font-mono text-[11px] leading-relaxed"
          >{{ sample }}</pre
        >
      </div>
    </CardContent>

    <div class="flex flex-wrap gap-2 px-6 pb-6">
      <Button
        v-if="!running"
        size="sm"
        class="gap-2"
        :disabled="loading"
        @click="start"
      >
        <MorphIconBox :icon="loading ? LoaderCircle : Power" :size="15" :class="loading ? 'animate-spin' : ''" />
        启动网关
      </Button>
      <Button v-else variant="outline" size="sm" class="gap-2" :disabled="loading" @click="stop">
        <MorphIconBox :icon="Power" :size="15" />
        停止网关
      </Button>
      <Button variant="ghost" size="sm" class="gap-2" :disabled="loading" @click="restart">
        <MorphIconBox :icon="RefreshCw" :size="15" />
        重启
      </Button>
    </div>
  </Card>
</template>

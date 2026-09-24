<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { Activity, ArrowLeftRight, Copy, LoaderCircle, RefreshCw, Route } from "lucide";

import MorphIconBox from "@/components/common/MorphIconBox.vue";
import StatTile from "@/components/common/StatTile.vue";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { useGateway } from "@/composables/useGateway";
import { cacheHitRate, formatCompact, formatNumber, formatPercent } from "@/lib/format";
import { usageApi } from "@/lib/ipc";
import { notifySuccess } from "@/lib/notify";
import type { UsageSummary } from "@/lib/types";

const { status, running, loading, restart } = useGateway();

const baseUrl = computed(() => status.value?.baseUrl ?? "http://127.0.0.1:8931");

const endpoints = [
  { path: "/v1/messages", label: "Messages" },
  { path: "/v1/chat/completions", label: "Completions" },
  { path: "/v1/responses", label: "Responses" },
];
const selectedEndpoint = ref(endpoints[0]);
const endpointUrl = computed(() => `${baseUrl.value}${selectedEndpoint.value.path}`);

// 面板只展示当日用量，口径与「统计」页一致。
const today = ref<UsageSummary | null>(null);

const cacheRead = computed(() => today.value?.cacheReadTokens ?? 0);
const cacheHit = computed(() =>
  formatPercent(
    cacheHitRate({
      inputTokens: today.value?.inputTokens ?? 0,
      cacheReadTokens: today.value?.cacheReadTokens,
      cacheWriteTokens: today.value?.cacheWriteTokens,
    }),
  ),
);

async function loadToday() {
  today.value = await usageApi.summary(1);
}

async function handleRestart() {
  if (await restart()) await loadToday();
}

const sample = computed(
  () => `curl ${baseUrl.value}/v1/messages \\
  -H "content-type: application/json" \\
  -H "x-api-key: aiStartClaude" \\
  -d '{"model":"aiStart","max_tokens":64,"messages":[{"role":"user","content":"ping"}]}'`,
);

async function copyText(text: string, message: string) {
  await navigator.clipboard.writeText(text);
  notifySuccess(message);
}

onMounted(loadToday);
</script>

<template>
  <Card>
    <CardHeader>
      <div class="flex items-center justify-between gap-3">
        <div class="flex items-center gap-2">
          <CardTitle>本地网关</CardTitle>
          <Button
            variant="outline"
            size="sm"
            class="gap-1.5"
            :disabled="loading"
            @click="handleRestart"
          >
            <MorphIconBox
              :icon="loading ? LoaderCircle : RefreshCw"
              :size="14"
              :class="loading ? 'animate-spin' : ''"
            />
            重启网关
          </Button>
        </div>
        <Badge :variant="running ? 'default' : 'outline'" class="gap-1.5">
          <MorphIconBox :icon="Activity" :size="12" />
          {{ running ? "运行中" : "已停止" }}
        </Badge>
      </div>
    </CardHeader>

    <CardContent class="space-y-4">
      <div class="grid grid-cols-2 gap-3 md:grid-cols-3 lg:grid-cols-5">
        <StatTile label="监听地址" :value="baseUrl" value-class="text-sm" />
        <StatTile label="今日请求" :value="formatNumber(today?.totalRequests ?? 0)" />
        <StatTile
          label="今日错误"
          :value="formatNumber(today?.failedRequests ?? 0)"
          :tone="(today?.failedRequests ?? 0) > 0 ? 'danger' : 'default'"
        />
        <StatTile
          label="缓存命中"
          :value="cacheHit"
          :hint="`缓存读 ${formatCompact(cacheRead)}`"
          :tone="cacheRead > 0 ? 'success' : 'default'"
        />
        <StatTile
          label="输出 Token"
          :value="formatCompact(today?.outputTokens ?? 0)"
          :hint="`输入 ${formatCompact(today?.inputTokens ?? 0)}`"
        />
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
          <span class="text-muted-foreground">已触发 {{ status.failovers }} 次</span>
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

      <div class="space-y-2">
        <p class="text-sm text-muted-foreground">对接说明</p>
        <div class="divide-y rounded-md border bg-muted/40 text-sm">
          <div class="space-y-2 px-3 py-2.5 transition-colors duration-150 hover:bg-accent/30">
            <div class="flex flex-wrap items-center gap-2">
              <span class="text-muted-foreground">接口地址</span>
              <div class="flex gap-0.5 rounded-md border bg-background p-0.5">
                <button
                  v-for="endpoint in endpoints"
                  :key="endpoint.path"
                  type="button"
                  class="rounded px-2 py-0.5 text-xs transition-colors"
                  :class="
                    endpoint.path === selectedEndpoint.path
                      ? 'bg-primary text-primary-foreground'
                      : 'text-muted-foreground hover:bg-accent'
                  "
                  @click="selectedEndpoint = endpoint"
                >
                  {{ endpoint.label }}
                </button>
              </div>
            </div>
            <div class="flex items-center gap-2">
              <span class="min-w-0 flex-1 break-all font-mono text-foreground">
                {{ endpointUrl }}
              </span>
              <Button
                variant="ghost"
                size="icon-xs"
                class="shrink-0 text-muted-foreground"
                aria-label="复制接口地址"
                @click="copyText(endpointUrl, '接口地址已复制')"
              >
                <MorphIconBox :icon="Copy" :size="14" />
              </Button>
            </div>
          </div>

          <div class="flex items-center gap-2 px-3 py-2.5 transition-colors duration-150 hover:bg-accent/30">
            <span class="shrink-0 text-muted-foreground">API Key</span>
            <span class="min-w-0 flex-1 break-all font-mono text-foreground">
              aiStart<span class="text-muted-foreground">[应用名称]</span>
            </span>
            <Button
              variant="ghost"
              size="icon-xs"
              class="shrink-0 text-muted-foreground"
              aria-label="复制 API Key"
              @click="copyText('aiStart', 'API Key 已复制')"
            >
              <MorphIconBox :icon="Copy" :size="14" />
            </Button>
          </div>

          <div class="flex items-center gap-2 px-3 py-2.5 transition-colors duration-150 hover:bg-accent/30">
            <span class="shrink-0 text-muted-foreground">模型名称</span>
            <span class="min-w-0 flex-1 font-mono text-foreground">aiStart</span>
            <Button
              variant="ghost"
              size="icon-xs"
              class="shrink-0 text-muted-foreground"
              aria-label="复制模型名称"
              @click="copyText('aiStart', '模型名称已复制')"
            >
              <MorphIconBox :icon="Copy" :size="14" />
            </Button>
          </div>
        </div>
      </div>

      <div class="space-y-2">
        <div class="flex items-center justify-between">
          <p class="text-sm text-muted-foreground">调用示例</p>
          <Button
            variant="ghost"
            size="sm"
            class="gap-1.5"
            @click="copyText(sample, '调用示例已复制')"
          >
            <MorphIconBox :icon="Copy" :size="14" />
            复制
          </Button>
        </div>
        <pre
          class="overflow-x-auto rounded-md border bg-muted/40 px-3 py-2 font-mono text-sm leading-relaxed"
          >{{ sample }}</pre
        >
      </div>
    </CardContent>
  </Card>
</template>

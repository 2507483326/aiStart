<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { ArrowLeft, ArrowRight, FileQuestion } from "lucide";

import EmptyState from "@/components/common/EmptyState.vue";
import MorphIconBox from "@/components/common/MorphIconBox.vue";
import CollapsibleCard from "@/components/stats/CollapsibleCard.vue";
import MessageList from "@/components/stats/MessageList.vue";
import PayloadCard from "@/components/stats/PayloadCard.vue";
import RawPayload from "@/components/stats/RawPayload.vue";
import ToolDefinitions from "@/components/stats/ToolDefinitions.vue";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { TooltipProvider } from "@/components/ui/tooltip";
import {
  cacheHitRate,
  formatDateTime,
  formatLatency,
  formatNumber,
  formatPercent,
  protocolLabel,
  sourceAppIcon,
  sourceAppLabel,
} from "@/lib/format";
import { usageApi } from "@/lib/ipc";
import { parseRequest, parseResponse } from "@/lib/payload";
import type { RequestDetail } from "@/lib/types";

const route = useRoute();
const router = useRouter();

const detail = ref<RequestDetail | null>(null);
const loading = ref(true);
const error = ref<string | null>(null);

const record = computed(() => detail.value?.record ?? null);
const payload = computed(() => detail.value?.payload ?? null);
const sourceIcon = computed(() => (record.value ? sourceAppIcon(record.value.sourceApp) : null));

const requestView = computed(() =>
  parseRequest(payload.value?.inboundRequest ?? null, record.value?.inboundProtocol ?? ""),
);
const responseView = computed(() =>
  parseResponse(payload.value?.upstreamResponse ?? null, record.value?.upstreamProtocol ?? ""),
);

const systemLabelClass =
  "border-transparent bg-amber-500/15 text-amber-600 dark:text-amber-400";

/** null 表示上游未回报该字段，界面显「—」而不是 0。 */
function tokenText(value: number | null): string {
  return value === null ? "—" : formatNumber(value);
}

const tokenStats = computed(() => {
  const value = record.value;
  if (!value) return [];
  const cacheRead = value.cacheReadTokens ?? 0;
  return [
    { label: "输入", value: formatNumber(value.inputTokens), tone: "" },
    { label: "输出", value: formatNumber(value.outputTokens), tone: "" },
    { label: "缓存读", value: tokenText(value.cacheReadTokens), tone: "" },
    { label: "缓存写", value: tokenText(value.cacheWriteTokens), tone: "" },
    {
      label: "缓存命中",
      value: formatPercent(cacheHitRate(value)),
      tone: cacheRead > 0 ? "text-emerald-600 dark:text-emerald-400" : "",
    },
    { label: "合计", value: formatNumber(value.inputTokens + value.outputTokens), tone: "" },
  ];
});

async function load(): Promise<void> {
  loading.value = true;
  error.value = null;
  detail.value = null;
  try {
    detail.value = await usageApi.detail(Number(route.params.id));
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause);
  } finally {
    loading.value = false;
  }
}

watch(() => route.params.id, load, { immediate: true });

function goBack(): void {
  if (window.history.state?.back) router.back();
  else router.push({ name: "stats" });
}
</script>

<template>
  <TooltipProvider>
    <div class="w-full space-y-4">
      <Button variant="ghost" size="sm" class="gap-1.5" @click="goBack">
        <MorphIconBox :icon="ArrowLeft" :size="15" />
        返回
      </Button>

      <div v-if="loading" class="py-16 text-center text-sm text-muted-foreground">
        正在加载…
      </div>

      <div v-else-if="error" class="py-16 text-center text-sm text-muted-foreground">
        读取失败：{{ error }}
      </div>

      <EmptyState
        v-else-if="!record"
        :icon="FileQuestion"
        title="没有找到这条请求"
        description="它可能已被清理，或链接有误。"
      />

      <template v-else>
        <Card class="gap-4">
          <CardHeader>
            <div class="flex items-start justify-between gap-3">
              <div class="space-y-1">
                <CardTitle class="text-base">
                  {{ record.servedBy || record.modelName || "请求详情" }}
                </CardTitle>
                <CardDescription class="text-xs">
                  {{ formatDateTime(record.timestamp) }}
                </CardDescription>
              </div>
              <div class="flex items-center gap-2">
                <Badge
                  v-if="record.ok"
                  variant="outline"
                  class="border-transparent bg-emerald-500/15 text-emerald-600 dark:text-emerald-400"
                >
                  成功
                </Badge>
                <Badge v-else variant="destructive">失败</Badge>
                <Badge v-if="record.failover" variant="outline">自动切换</Badge>
              </div>
            </div>
          </CardHeader>

          <CardContent class="space-y-4">
            <div class="flex flex-wrap items-center gap-x-6 gap-y-2 text-xs">
              <span class="inline-flex items-center gap-1.5">
                <span class="text-muted-foreground">来源</span>
                <img v-if="sourceIcon" :src="sourceIcon" alt="" class="size-4 shrink-0 object-contain" />
                <span class="font-medium">{{ sourceAppLabel(record.sourceApp) }}</span>
              </span>
              <span class="inline-flex items-center gap-1.5">
                <span class="text-muted-foreground">协议</span>
                <span class="inline-flex items-center gap-1 font-medium">
                  {{ protocolLabel(record.inboundProtocol) }}
                  <MorphIconBox :icon="ArrowRight" :size="12" class="text-muted-foreground" />
                  {{ protocolLabel(record.upstreamProtocol) }}
                </span>
              </span>
              <span class="inline-flex items-center gap-1.5">
                <span class="text-muted-foreground">耗时</span>
                <span class="font-mono font-medium tabular-nums">
                  {{ formatLatency(record.durationMs) }}
                </span>
              </span>
            </div>

            <dl
              class="grid grid-cols-3 gap-px overflow-hidden rounded-lg border bg-border sm:grid-cols-6"
            >
              <div v-for="stat in tokenStats" :key="stat.label" class="bg-card px-3 py-2">
                <dt class="text-[11px] text-muted-foreground">{{ stat.label }}</dt>
                <dd class="mt-0.5 font-mono text-sm font-medium tabular-nums" :class="stat.tone">
                  {{ stat.value }}
                </dd>
              </div>
            </dl>

            <div
              v-if="record.error"
              class="rounded-md border border-destructive/40 bg-destructive/10 p-3 text-xs text-destructive"
            >
              {{ record.error }}
            </div>
          </CardContent>
        </Card>

        <div v-if="!payload" class="py-6 text-center text-sm text-muted-foreground">
          该请求未记录报文（可能是升级前的历史记录，或报文已被清理）。
        </div>

        <template v-else>
          <CollapsibleCard
            v-if="requestView?.tools.length"
            title="工具定义"
            description="本次请求向模型声明的工具，点击查看说明。"
          >
            <ToolDefinitions :tools="requestView.tools" />
          </CollapsibleCard>

          <CollapsibleCard title="入站请求" description="客户端发来的原始请求">
            <template #action>
              <Badge v-if="payload.requestTruncated" variant="outline">已截断</Badge>
            </template>

            <template v-if="requestView">
              <PayloadCard
                v-if="requestView.system"
                label="system"
                :label-class="systemLabelClass"
                :text="requestView.system"
              >
                <pre
                  class="text-xs leading-relaxed break-words whitespace-pre-wrap"
                  >{{ requestView.system }}</pre
                >
              </PayloadCard>
              <MessageList v-if="requestView.messages.length" :messages="requestView.messages" />
            </template>
            <RawPayload
              v-else
              :raw="payload.inboundRequest"
              label="原始请求（无法解析）"
              :truncated="payload.requestTruncated"
            />
          </CollapsibleCard>

          <CollapsibleCard
            title="上游响应"
            :description="`${payload.stream ? '流式（已按事件拼装）' : '非流式'} · 上游原生返回`"
          >
            <template #action>
              <Badge v-if="payload.responseTruncated" variant="outline">已截断</Badge>
            </template>

            <template v-if="responseView">
              <MessageList :messages="responseView.messages" />
              <div class="flex flex-wrap gap-x-5 gap-y-1 text-xs text-muted-foreground">
                <span v-if="responseView.stopReason">停止原因 {{ responseView.stopReason }}</span>
                <span v-if="responseView.usage">
                  Token 输入 {{ formatNumber(responseView.usage.input) }} · 输出
                  {{ formatNumber(responseView.usage.output) }}
                </span>
              </div>
            </template>
            <RawPayload
              v-else
              :raw="payload.upstreamResponse"
              label="原始响应（无法解析）"
              :truncated="payload.responseTruncated"
            />
          </CollapsibleCard>

          <Card>
            <CardHeader>
              <CardTitle class="text-base">原始报文</CardTitle>
              <CardDescription class="text-xs">解析前的原文，便于排查。</CardDescription>
            </CardHeader>
            <CardContent class="space-y-2">
              <RawPayload
                :raw="payload.inboundRequest"
                label="入站请求原文"
                :truncated="payload.requestTruncated"
              />
              <RawPayload
                :raw="payload.upstreamResponse"
                label="上游响应原文"
                :truncated="payload.responseTruncated"
              />
            </CardContent>
          </Card>
        </template>
      </template>
    </div>
  </TooltipProvider>
</template>

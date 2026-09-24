<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { ArrowLeft, FileQuestion } from "lucide";

import EmptyState from "@/components/common/EmptyState.vue";
import MorphIconBox from "@/components/common/MorphIconBox.vue";
import MessageList from "@/components/stats/MessageList.vue";
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
import { formatDateTime, formatLatency, formatNumber, protocolLabel } from "@/lib/format";
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

const requestView = computed(() =>
  parseRequest(payload.value?.inboundRequest ?? null, record.value?.inboundProtocol ?? ""),
);
const responseView = computed(() =>
  parseResponse(payload.value?.upstreamResponse ?? null, record.value?.upstreamProtocol ?? ""),
);

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
        <Card>
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
          <CardContent class="flex flex-wrap gap-x-5 gap-y-1.5 text-xs text-muted-foreground">
            <span>
              入站 {{ protocolLabel(record.inboundProtocol) }} → 上游
              {{ protocolLabel(record.upstreamProtocol) }}
            </span>
            <span>输入 {{ formatNumber(record.inputTokens) }}</span>
            <span>输出 {{ formatNumber(record.outputTokens) }}</span>
            <span>合计 {{ formatNumber(record.inputTokens + record.outputTokens) }}</span>
            <span>耗时 {{ formatLatency(record.durationMs) }}</span>
          </CardContent>
          <CardContent v-if="record.error" class="pt-0">
            <div
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
          <Card v-if="requestView?.tools.length">
            <CardHeader>
              <CardTitle class="text-base">工具定义</CardTitle>
              <CardDescription class="text-xs">
                本次请求向模型声明的工具，点击查看说明。
              </CardDescription>
            </CardHeader>
            <CardContent>
              <ToolDefinitions :tools="requestView.tools" />
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <div class="flex items-center justify-between gap-3">
                <div class="space-y-1">
                  <CardTitle class="text-base">入站请求</CardTitle>
                  <CardDescription class="text-xs">客户端发来的原始请求</CardDescription>
                </div>
                <Badge v-if="payload.requestTruncated" variant="outline">已截断</Badge>
              </div>
            </CardHeader>
            <CardContent class="space-y-3">
              <template v-if="requestView">
                <div v-if="requestView.system" class="rounded-lg border bg-card">
                  <div class="border-b px-3 py-1.5">
                    <Badge
                      variant="outline"
                      class="border-transparent bg-amber-500/15 text-[10px] text-amber-600 dark:text-amber-400"
                    >
                      system
                    </Badge>
                  </div>
                  <pre
                    class="px-3 py-2.5 text-xs leading-relaxed break-words whitespace-pre-wrap"
                    >{{ requestView.system }}</pre
                  >
                </div>
                <MessageList v-if="requestView.messages.length" :messages="requestView.messages" />
              </template>
              <RawPayload
                v-else
                :raw="payload.inboundRequest"
                label="原始请求（无法解析）"
                :truncated="payload.requestTruncated"
              />
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <div class="flex items-center justify-between gap-3">
                <div class="space-y-1">
                  <CardTitle class="text-base">上游响应</CardTitle>
                  <CardDescription class="text-xs">
                    {{ payload.stream ? "流式（已按事件拼装）" : "非流式" }} · 上游原生返回
                  </CardDescription>
                </div>
                <Badge v-if="payload.responseTruncated" variant="outline">已截断</Badge>
              </div>
            </CardHeader>
            <CardContent class="space-y-3">
              <template v-if="responseView">
                <MessageList :messages="responseView.messages" />
                <div class="flex flex-wrap gap-x-5 gap-y-1 text-xs text-muted-foreground">
                  <span v-if="responseView.stopReason">
                    停止原因 {{ responseView.stopReason }}
                  </span>
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
            </CardContent>
          </Card>

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

<script setup lang="ts">
import { onMounted } from "vue";
import { RefreshCw } from "lucide";

import MorphIconBox from "@/components/common/MorphIconBox.vue";
import StatTile from "@/components/common/StatTile.vue";
import ContributionHeatmap from "@/components/stats/ContributionHeatmap.vue";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { useUsage } from "@/composables/useUsage";
import { formatCompact, formatDateTime, formatLatency, formatNumber } from "@/lib/format";

const { summary, records, loading, refresh } = useUsage();

onMounted(() => refresh(365));

function protocolLabel(value: string): string {
  switch (value) {
    case "anthropic-messages":
      return "Messages";
    case "openai-completions":
      return "Completions";
    case "openai-responses":
      return "Responses";
    default:
      return value || "—";
  }
}
</script>

<template>
  <div class="mx-auto max-w-6xl space-y-5">
    <div class="grid grid-cols-6 gap-3">
      <StatTile label="总请求" :value="formatNumber(summary?.totalRequests ?? 0)" />
      <StatTile
        label="失败请求"
        :value="formatNumber(summary?.failedRequests ?? 0)"
        :tone="(summary?.failedRequests ?? 0) > 0 ? 'danger' : 'default'"
      />
      <StatTile label="输入 Token" :value="formatCompact(summary?.inputTokens ?? 0)" />
      <StatTile label="输出 Token" :value="formatCompact(summary?.outputTokens ?? 0)" />
      <StatTile label="今日 Token" :value="formatCompact(summary?.todayTokens ?? 0)" />
      <StatTile
        label="连续活跃"
        :value="`${summary?.streakDays ?? 0} 天`"
        :tone="(summary?.streakDays ?? 0) > 0 ? 'success' : 'default'"
      />
    </div>

    <Card>
      <CardHeader>
        <div class="flex items-start justify-between gap-3">
          <div class="space-y-1">
            <CardTitle class="text-base">Token 贡献</CardTitle>
            <CardDescription class="text-xs">
              最近 53 周每天的 Token 消耗量，颜色越深消耗越多。
            </CardDescription>
          </div>
          <Button variant="outline" size="sm" class="gap-2" :disabled="loading" @click="refresh(365)">
            <MorphIconBox
              :icon="RefreshCw"
              :size="15"
              :class="loading ? 'animate-spin' : ''"
            />
            刷新
          </Button>
        </div>
      </CardHeader>
      <CardContent>
        <ContributionHeatmap :daily="summary?.daily ?? []" />
      </CardContent>
    </Card>

    <Card>
      <CardHeader>
        <div class="space-y-1">
          <CardTitle class="text-base">请求明细</CardTitle>
          <CardDescription class="text-xs">
            每次经由网关的调用及其 Token 消耗，最新 300 条。
          </CardDescription>
        </div>
      </CardHeader>
      <CardContent>
        <div v-if="!records.length" class="py-10 text-center text-sm text-muted-foreground">
          还没有请求记录。让 Claude Desktop 或任意客户端调用一次网关即可看到数据。
        </div>

        <div v-else class="overflow-x-auto">
          <table class="w-full text-xs">
            <thead>
              <tr class="border-b text-left text-muted-foreground">
                <th class="py-2 pr-3 font-medium">时间</th>
                <th class="py-2 pr-3 font-medium">模型</th>
                <th class="py-2 pr-3 font-medium">入站</th>
                <th class="py-2 pr-3 font-medium">上游</th>
                <th class="py-2 pr-3 text-right font-medium">输入</th>
                <th class="py-2 pr-3 text-right font-medium">输出</th>
                <th class="py-2 pr-3 text-right font-medium">合计</th>
                <th class="py-2 pr-3 text-right font-medium">耗时</th>
                <th class="py-2 font-medium">状态</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="record in records" :key="record.timestamp" class="border-b last:border-0">
                <td class="py-2 pr-3 whitespace-nowrap text-muted-foreground">
                  {{ formatDateTime(record.timestamp) }}
                </td>
                <td class="py-2 pr-3">
                  <span class="font-medium">{{ record.servedBy || record.modelName || "—" }}</span>
                  <Badge v-if="record.failover" variant="outline" class="ml-1.5 text-[10px]">
                    自动切换
                  </Badge>
                </td>
                <td class="py-2 pr-3 whitespace-nowrap text-muted-foreground">
                  {{ protocolLabel(record.inboundProtocol) }}
                </td>
                <td class="py-2 pr-3 whitespace-nowrap text-muted-foreground">
                  {{ protocolLabel(record.upstreamProtocol) }}
                </td>
                <td class="py-2 pr-3 text-right font-mono tabular-nums">
                  {{ formatNumber(record.inputTokens) }}
                </td>
                <td class="py-2 pr-3 text-right font-mono tabular-nums">
                  {{ formatNumber(record.outputTokens) }}
                </td>
                <td class="py-2 pr-3 text-right font-mono font-medium tabular-nums">
                  {{ formatNumber(record.inputTokens + record.outputTokens) }}
                </td>
                <td class="py-2 pr-3 text-right font-mono tabular-nums text-muted-foreground">
                  {{ formatLatency(record.durationMs) }}
                </td>
                <td class="py-2">
                  <Badge
                    v-if="record.ok"
                    variant="outline"
                    class="border-transparent bg-emerald-500/15 text-emerald-600 dark:text-emerald-400"
                  >
                    成功
                  </Badge>
                  <Badge v-else variant="destructive" :title="record.error ?? ''">失败</Badge>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </CardContent>
    </Card>
  </div>
</template>

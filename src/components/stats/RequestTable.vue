<script setup lang="ts">
import { useRouter } from "vue-router";
import { ChevronRight } from "lucide";

import MorphIconBox from "@/components/common/MorphIconBox.vue";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  cacheHitRate,
  formatDateTime,
  formatLatency,
  formatNumber,
  formatPercent,
  protocolLabel,
} from "@/lib/format";
import type { UsageRecord } from "@/lib/types";

withDefaults(
  defineProps<{
    records: UsageRecord[];
    empty?: string;
  }>(),
  {
    empty: "还没有请求记录。让 Claude Desktop 或任意客户端调用一次网关即可看到数据。",
  },
);

const router = useRouter();

function openDetail(record: UsageRecord): void {
  router.push({ name: "request-detail", params: { id: String(record.id) } });
}

function cacheHit(record: UsageRecord): string {
  return formatPercent(cacheHitRate(record));
}
</script>

<template>
  <div v-if="!records.length" class="py-10 text-center text-sm text-muted-foreground">
    {{ empty }}
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
          <th class="py-2 pr-3 text-right font-medium">缓存</th>
          <th class="py-2 pr-3 text-right font-medium">耗时</th>
          <th class="py-2 pr-3 font-medium">状态</th>
          <th class="w-8 py-2"></th>
        </tr>
      </thead>
      <tbody>
        <tr
          v-for="record in records"
          :key="record.id"
          class="cursor-pointer border-b transition-colors last:border-0 hover:bg-accent/40"
          @click="openDetail(record)"
        >
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
            {{ cacheHit(record) }}
          </td>
          <td class="py-2 pr-3 text-right font-mono tabular-nums text-muted-foreground">
            {{ formatLatency(record.durationMs) }}
          </td>
          <td class="py-2 pr-3">
            <Badge
              v-if="record.ok"
              variant="outline"
              class="border-transparent bg-emerald-500/15 text-emerald-600 dark:text-emerald-400"
            >
              成功
            </Badge>
            <Badge v-else variant="destructive" :title="record.error ?? ''">失败</Badge>
          </td>
          <td class="py-1.5 pl-1">
            <Button
              variant="ghost"
              size="icon-xs"
              class="text-muted-foreground"
              aria-label="查看请求详情"
              @click.stop="openDetail(record)"
            >
              <MorphIconBox :icon="ChevronRight" :size="15" />
            </Button>
          </td>
        </tr>
      </tbody>
    </table>
  </div>
</template>

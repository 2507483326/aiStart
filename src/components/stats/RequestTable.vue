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
  sourceAppIcon,
  sourceAppLabel,
  totalTokens,
} from "@/lib/format";
import type { UsageRecord } from "@/lib/types";
import { cn } from "@/lib/utils";

withDefaults(
  defineProps<{
    records: UsageRecord[];
    empty?: string;
    scroll?: boolean;
  }>(),
  {
    empty: "还没有请求记录。让 任意客户端调用一次网关即可看到数据。",
    scroll: false,
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

  <div
    v-else
    :class="cn('overflow-x-auto', scroll && 'max-h-[30rem] overflow-y-auto')"
  >
    <table class="w-full text-xs">
      <thead :class="scroll ? 'sticky top-0 z-10 bg-card' : undefined">
        <tr class="border-b text-left text-muted-foreground">
          <th class="py-2 pr-3 font-medium">时间</th>
          <th class="py-2 pr-3 font-medium">来源</th>
          <th class="py-2 pr-3 font-medium">模型</th>
          <th class="py-2 pr-3 font-medium">入站</th>
          <th class="py-2 pr-3 font-medium">上游</th>
          <th class="py-2 pr-3 text-right font-medium">输入</th>
          <th class="py-2 pr-3 text-right font-medium">输出</th>
          <th class="py-2 pr-3 text-right font-medium" title="输入 + 输出 + 缓存读 + 缓存写">
            合计
          </th>
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
          class="cursor-pointer border-b transition-colors duration-150 last:border-0 hover:bg-accent/50"
          @click="openDetail(record)"
        >
          <td class="py-2 pr-3 whitespace-nowrap text-muted-foreground">
            {{ formatDateTime(record.timestamp) }}
          </td>
          <td class="py-2 pr-3 whitespace-nowrap text-muted-foreground">
            <span class="inline-flex items-center gap-1.5">
              <img
                v-if="sourceAppIcon(record.sourceApp)"
                :src="sourceAppIcon(record.sourceApp)!"
                alt=""
                class="size-4 shrink-0 object-contain"
              />
              {{ sourceAppLabel(record.sourceApp) }}
            </span>
          </td>
          <td class="py-2 pr-3">
            <span class="font-medium">{{ record.servedBy || record.modelName || "—" }}</span>
            <Badge v-if="record.proxied" variant="outline" class="ml-1.5" title="经代理出站">
              代理
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
            {{ formatNumber(totalTokens(record)) }}
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

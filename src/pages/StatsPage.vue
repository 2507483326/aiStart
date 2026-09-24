<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { useRouter } from "vue-router";
import { useIntervalFn } from "@vueuse/core";
import { ChevronRight } from "lucide";

import MorphIconBox from "@/components/common/MorphIconBox.vue";
import StatTile from "@/components/common/StatTile.vue";
import ContributionHeatmap from "@/components/stats/ContributionHeatmap.vue";
import RequestTable from "@/components/stats/RequestTable.vue";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardAction,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useUsage } from "@/composables/useUsage";
import { cacheHitRate, formatCompact, formatNumber, formatPercent } from "@/lib/format";

const router = useRouter();
const { summary, records, refresh } = useUsage();

// 统计页只预览最新若干条，完整列表在「请求明细」页
const PREVIEW_LIMIT = 50;

const YEAR_SPAN = 4;
const currentYear = new Date().getFullYear();
const years = Array.from({ length: YEAR_SPAN }, (_, index) => currentYear - index);
const year = ref(currentYear);

const oldestDay = new Date(currentYear - (YEAR_SPAN - 1), 0, 1).getTime();
const rangeDays = Math.max(1, Math.floor((Date.now() - oldestDay) / 86_400_000) + 1);

function selectYear(value: unknown): void {
  year.value = Number(value);
}

// 缓存命中率 = 缓存读 / 计费输入（未命中输入 + 缓存读 + 缓存写），与 dsh 同口径。
const cacheHit = computed(() =>
  formatPercent(
    cacheHitRate({
      inputTokens: summary.value?.inputTokens ?? 0,
      cacheReadTokens: summary.value?.cacheReadTokens ?? 0,
      cacheWriteTokens: summary.value?.cacheWriteTokens ?? 0,
    }),
  ),
);

onMounted(() => refresh(rangeDays, PREVIEW_LIMIT));

// 有新请求时自动刷新（组件卸载自动停止）
useIntervalFn(() => refresh(rangeDays, PREVIEW_LIMIT), 5000);

function openAll(): void {
  router.push({ name: "requests" });
}
</script>

<template>
  <div class="mx-auto max-w-6xl space-y-5">
    <div class="grid grid-cols-4 gap-3 xl:grid-cols-7">
      <StatTile label="总请求" :value="formatNumber(summary?.totalRequests ?? 0)" />
      <StatTile
        label="失败请求"
        :value="formatNumber(summary?.failedRequests ?? 0)"
        :tone="(summary?.failedRequests ?? 0) > 0 ? 'danger' : 'default'"
      />
      <StatTile label="输入 Token" :value="formatCompact(summary?.inputTokens ?? 0)" />
      <StatTile label="输出 Token" :value="formatCompact(summary?.outputTokens ?? 0)" />
      <StatTile
        label="缓存命中"
        :value="cacheHit"
        :tone="(summary?.cacheReadTokens ?? 0) > 0 ? 'success' : 'default'"
      />
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
            <CardTitle>Token 贡献</CardTitle>
          </div>
          <Select :model-value="year" @update:model-value="selectYear">
            <SelectTrigger size="sm" class="w-[110px]">
              <SelectValue>{{ year }} 年</SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectItem v-for="value in years" :key="value" :value="value">
                {{ value }} 年
              </SelectItem>
            </SelectContent>
          </Select>
        </div>
      </CardHeader>
      <CardContent>
        <ContributionHeatmap :daily="summary?.daily ?? []" :year="year" />
      </CardContent>
    </Card>

    <Card>
      <CardHeader>
        <div class="space-y-1">
          <CardTitle>请求明细</CardTitle>
        </div>
        <CardAction>
          <Button
            variant="ghost"
            size="icon-xs"
            class="text-muted-foreground"
            aria-label="查看全部请求明细"
            @click="openAll"
          >
            <MorphIconBox :icon="ChevronRight" :size="15" />
          </Button>
        </CardAction>
      </CardHeader>
      <CardContent>
        <RequestTable :records="records" scroll />
      </CardContent>
    </Card>
  </div>
</template>

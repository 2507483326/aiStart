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
const { daily, today, total, records, refresh } = useUsage();

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
      inputTokens: total.value?.inputTokens ?? 0,
      cacheReadTokens: total.value?.cacheReadTokens ?? 0,
      cacheWriteTokens: total.value?.cacheWriteTokens ?? 0,
    }),
  ),
);

const todayCacheHit = computed(() =>
  formatPercent(
    cacheHitRate({
      inputTokens: today.value?.inputTokens ?? 0,
      cacheReadTokens: today.value?.cacheReadTokens ?? 0,
      cacheWriteTokens: today.value?.cacheWriteTokens ?? 0,
    }),
  ),
);

function iso(date: Date): string {
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

// 连续活跃天数：从今天往回数「有请求」的连续日。数据来自每日行，无需后端再算。
const streakDays = computed(() => {
  const active = new Set(
    daily.value.filter((entry) => entry.requests > 0).map((entry) => entry.date),
  );
  let streak = 0;
  const cursor = new Date();
  cursor.setHours(0, 0, 0, 0);
  while (active.has(iso(cursor))) {
    streak += 1;
    cursor.setDate(cursor.getDate() - 1);
  }
  return streak;
});

onMounted(() => refresh(rangeDays, PREVIEW_LIMIT));

// 有新请求时自动刷新（组件卸载自动停止）；页面不可见时不查，省掉后台无谓的汇总查询。
useIntervalFn(() => {
  if (document.visibilityState === "visible") refresh(rangeDays, PREVIEW_LIMIT);
}, 5000);

function openAll(): void {
  router.push({ name: "requests" });
}
</script>

<template>
  <div class="w-full space-y-5">
    <div class="grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-7">
      <StatTile
        label="总请求"
        :value="formatNumber(total?.requests ?? 0)"
        :hint="`今日请求 ${formatNumber(today?.requests ?? 0)}`"
      />
      <StatTile
        label="总 Token"
        :value="formatCompact(total?.outputTokens ?? 0)"
        :hint="`输入 ${formatCompact(total?.inputTokens ?? 0)}`"
      />
      <StatTile
        label="今日 Token"
        :value="formatCompact(today?.outputTokens ?? 0)"
        :hint="`输入 ${formatCompact(today?.inputTokens ?? 0)}`"
      />
      <StatTile
        label="总缓存命中"
        :value="cacheHit"
        :hint="`缓存读 ${formatCompact(total?.cacheReadTokens ?? 0)}`"
        :tone="(total?.cacheReadTokens ?? 0) > 0 ? 'success' : 'default'"
      />
      <StatTile
        label="今日缓存命中"
        :value="todayCacheHit"
        :hint="`缓存读 ${formatCompact(today?.cacheReadTokens ?? 0)}`"
        :tone="(today?.cacheReadTokens ?? 0) > 0 ? 'success' : 'default'"
      />
      <StatTile
        label="总失败请求"
        :value="formatNumber(total?.failed ?? 0)"
        :hint="`今日失败 ${formatNumber(today?.failed ?? 0)}`"
        :tone="(total?.failed ?? 0) > 0 ? 'danger' : 'default'"
      />
      <StatTile
        label="活跃天数"
        :value="`${streakDays} 天`"
        :tone="streakDays > 0 ? 'success' : 'default'"
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
        <ContributionHeatmap :daily="daily" :year="year" />
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

<script setup lang="ts">
import { computed } from "vue";

import type { DailyUsage } from "@/lib/types";
import { formatCompact, formatNumber } from "@/lib/format";

const props = defineProps<{ daily: DailyUsage[]; year: number }>();

const WEEKDAYS = ["日", "一", "二", "三", "四", "五", "六"];
const COLUMN_WIDTH = 15;

interface Cell {
  date: string;
  tokens: number;
  requests: number;
  label: string;
  future: boolean;
}

const usageByDate = computed(() => {
  const map = new Map<string, DailyUsage>();
  for (const entry of props.daily) map.set(entry.date, entry);
  return map;
});

function iso(date: Date): string {
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

const today = new Date();
today.setHours(0, 0, 0, 0);

const firstDay = computed(() => new Date(props.year, 0, 1));
const leadingBlanks = computed(() => firstDay.value.getDay());

const cells = computed<(Cell | null)[]>(() => {
  const list: (Cell | null)[] = [];
  for (let index = 0; index < leadingBlanks.value; index += 1) list.push(null);

  const last = new Date(props.year, 11, 31);
  const cursor = new Date(firstDay.value);
  while (cursor <= last) {
    const key = iso(cursor);
    const entry = usageByDate.value.get(key);
    const future = cursor > today;
    const tokens = entry?.totalTokens ?? 0;
    const requests = entry?.requests ?? 0;
    list.push({
      date: key,
      tokens,
      requests,
      future,
      label: future
        ? `${key} · 尚未到来`
        : `${key} · ${formatCompact(tokens)} tokens · ${requests} 次请求`,
    });
    cursor.setDate(cursor.getDate() + 1);
  }

  while (list.length % 7 !== 0) list.push(null);
  return list;
});

const columns = computed(() => cells.value.length / 7);

const monthLabels = computed(() => {
  const start = firstDay.value.getTime();
  const monthColumns = Array.from({ length: 12 }, (_, month) => {
    const offset = Math.round((new Date(props.year, month, 1).getTime() - start) / 86_400_000);
    return Math.floor((leadingBlanks.value + offset) / 7);
  });

  return monthColumns.map((column, month) => ({
    key: String(month),
    text: `${month + 1}月`,
    width: Math.max(1, (monthColumns[month + 1] ?? columns.value) - column) * COLUMN_WIDTH,
  }));
});

// 固定档位标尺（沿用 eTeam「AI 代码工程师日强度」口径）：10万/100万/300万三道台阶，
// 严格大于判定；0 恒空档。不按当年数据相对分档，避免档位随数据漂移。
function level(tokens: number): number {
  if (tokens <= 0) return 0;
  if (tokens > 3_000_000) return 4;
  if (tokens > 1_000_000) return 3;
  if (tokens > 100_000) return 2;
  return 1;
}

const levelClass = [
  "bg-muted",
  "bg-emerald-500/25",
  "bg-emerald-500/45",
  "bg-emerald-500/70",
  "bg-emerald-500",
];

const hoverClass = [
  "hover:bg-muted-foreground/40",
  "hover:bg-emerald-500/50",
  "hover:bg-emerald-500/70",
  "hover:bg-emerald-500/90",
  "hover:bg-emerald-400",
];

function cellClass(cell: Cell): string[] {
  if (cell.future) return ["bg-muted/40"];
  const value = level(cell.tokens);
  return [levelClass[value], hoverClass[value]];
}

const inYear = computed(() => props.daily.filter((entry) => entry.date.startsWith(`${props.year}-`)));

const yearTokens = computed(() =>
  inYear.value.reduce((sum, entry) => sum + (entry.totalTokens ?? 0), 0),
);

const yearRequests = computed(() =>
  inYear.value.reduce((sum, entry) => sum + (entry.requests ?? 0), 0),
);

const todayTokens = computed(() => usageByDate.value.get(iso(today))?.totalTokens ?? 0);
</script>

<template>
  <div class="space-y-3">
    <div class="flex gap-2 overflow-x-auto pb-1">
      <div class="grid grid-rows-7 gap-[3px] text-[11px] leading-3 text-muted-foreground">
        <span
          v-for="(day, index) in WEEKDAYS"
          :key="day"
          class="h-3"
          :class="index % 2 === 0 ? 'invisible' : ''"
        >
          {{ day }}
        </span>
      </div>

      <div class="min-w-0 space-y-1">
        <div class="flex text-[11px] leading-3 text-muted-foreground">
          <span
            v-for="label in monthLabels"
            :key="label.key"
            class="shrink-0"
            :style="{ width: `${label.width}px` }"
          >
            {{ label.text }}
          </span>
        </div>

        <div class="grid grid-flow-col grid-rows-7 gap-[3px]">
          <template v-for="(cell, index) in cells" :key="index">
            <div
              v-if="cell"
              :title="cell.label"
              :class="cellClass(cell)"
              class="size-3 rounded-[2px] border border-border/60 transition-colors duration-100"
            />
            <div v-else class="size-3" />
          </template>
        </div>
      </div>
    </div>

    <div class="flex items-center justify-between gap-3 text-[11px] text-muted-foreground">
      <div class="flex items-center gap-3">
        <span>
          {{ year }} 年
          <span
            class="font-medium text-foreground tabular-nums"
            :title="`${formatNumber(yearTokens)} tokens`"
          >{{ formatCompact(yearTokens) }}</span>
          <span class="ml-0.5">tokens</span>
        </span>
        <span>
          请求
          <span class="font-medium text-foreground tabular-nums">{{ formatNumber(yearRequests) }}</span>
          <span class="ml-0.5">次</span>
        </span>
        <span>
          今日
          <span
            class="font-medium text-foreground tabular-nums"
            :title="`${formatNumber(todayTokens)} tokens`"
          >{{ formatCompact(todayTokens) }}</span>
          <span class="ml-0.5">tokens</span>
        </span>
      </div>

      <div class="flex items-center gap-1.5">
        <span>少</span>
        <span
          v-for="(cls, index) in levelClass"
          :key="index"
          :class="cls"
          class="size-3 rounded-[2px] border border-border/60"
        />
        <span>多</span>
      </div>
    </div>
  </div>
</template>

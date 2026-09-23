<script setup lang="ts">
import { computed } from "vue";

import type { DailyUsage } from "@/lib/types";
import { formatCompact } from "@/lib/format";

const props = defineProps<{ daily: DailyUsage[]; days?: number }>();

const WEEKS = 53;
const WEEKDAYS = ["日", "一", "二", "三", "四", "五", "六"];

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

const cells = computed(() => {
  const today = new Date();
  today.setHours(0, 0, 0, 0);

  const start = new Date(today);
  start.setDate(start.getDate() - (WEEKS * 7 - 1));
  start.setDate(start.getDate() - start.getDay());

  const list: { date: string; tokens: number; requests: number; label: string }[] = [];
  const cursor = new Date(start);
  while (cursor <= today) {
    const key = iso(cursor);
    const entry = usageByDate.value.get(key);
    const tokens = entry?.totalTokens ?? 0;
    list.push({
      date: key,
      tokens,
      requests: entry?.requests ?? 0,
      label: `${key} · ${formatCompact(tokens)} tokens · ${entry?.requests ?? 0} 次请求`,
    });
    cursor.setDate(cursor.getDate() + 1);
  }
  return list;
});

const columns = computed(() => Math.ceil(cells.value.length / 7));

const monthLabels = computed(() => {
  const raw: { index: number; text: string }[] = [];
  let lastMonth = -1;
  for (let column = 0; column < columns.value; column += 1) {
    const cell = cells.value[column * 7];
    if (!cell) continue;
    const month = Number(cell.date.slice(5, 7));
    if (month !== lastMonth) {
      raw.push({ index: column, text: `${month}月` });
      lastMonth = month;
    }
  }
  return raw.map((label, position) => ({
    key: `${label.index}-${label.text}`,
    text: label.text,
    width: Math.max(1, (raw[position + 1]?.index ?? columns.value) - label.index) * 15,
  }));
});

function level(tokens: number): number {
  if (tokens <= 0) return 0;
  if (tokens >= 100_000) return 4;
  if (tokens >= 20_000) return 3;
  if (tokens >= 5_000) return 2;
  return 1;
}

const levelClass = [
  "bg-muted",
  "bg-emerald-500/25",
  "bg-emerald-500/45",
  "bg-emerald-500/70",
  "bg-emerald-500",
];
</script>

<template>
  <div class="space-y-3">
    <div class="flex gap-2 overflow-x-auto pb-1">
      <div class="grid grid-rows-7 gap-[3px] text-[10px] leading-3 text-muted-foreground">
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
        <div class="flex text-[10px] leading-3 text-muted-foreground">
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
          <div
            v-for="cell in cells"
            :key="cell.date"
            :title="cell.label"
            :class="levelClass[level(cell.tokens)]"
            class="size-3 rounded-[2px]"
          />
        </div>
      </div>
    </div>

    <div class="flex items-center justify-end gap-1.5 text-[10px] text-muted-foreground">
      <span>少</span>
      <span
        v-for="(cls, index) in levelClass"
        :key="index"
        :class="cls"
        class="size-3 rounded-[2px]"
      />
      <span>多</span>
    </div>
  </div>
</template>

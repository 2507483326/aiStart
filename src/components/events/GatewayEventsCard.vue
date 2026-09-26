<script setup lang="ts">
import { onMounted, ref, watch } from "vue";
import { Activity } from "lucide";

import EmptyState from "@/components/common/EmptyState.vue";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { useGateway } from "@/composables/useGateway";
import { formatDateTime } from "@/lib/format";
import { eventApi } from "@/lib/ipc";
import type { EventRecord } from "@/lib/types";

// 概览只留最近几条：网关的启停与自动切换；逐请求的成败在「请求」页看。
const EVENT_LIMIT = 5;

const eventLabels: Record<string, string> = {
  "gateway.started": "网关已启动",
  "gateway.stopped": "网关已停止",
  "gateway.error": "网关错误",
  "model.failover": "自动切换模型",
  "model.failover.skipped": "已跳过自动切换",
  "model.failover.exhausted": "自动切换已穷尽",
};

interface GatewayEventView {
  id: number;
  label: string;
  time: string;
  detail: string | null;
}

const events = ref<GatewayEventView[]>([]);
const loading = ref(false);

const gateway = useGateway();

/** 网关事件 = 网关进程写下的：actor 标成「网关」，或事件类型以 gateway. 开头。 */
function isGatewayEvent(event: EventRecord): boolean {
  return (
    event.actorKind === "gateway" ||
    event.actorName === "网关" ||
    event.type.startsWith("gateway.")
  );
}

/** 事件的附加数据是网关写入的 JSON，按类型取一句关键信息；取不到就不显示。 */
function eventDetail(event: EventRecord): string | null {
  if (!event.payload) return null;
  try {
    const payload = JSON.parse(event.payload) as Record<string, unknown>;
    switch (event.type) {
      case "model.failover":
        return `${payload.from ?? "?"} → ${payload.to ?? "?"}`;
      case "model.failover.skipped":
        return typeof payload.reason === "string" ? payload.reason : null;
      case "model.failover.exhausted":
        return typeof payload.failed === "string" ? `${payload.failed} 之后无可用模型` : null;
      case "gateway.error":
        return typeof payload.message === "string" ? payload.message : null;
      default:
        return null;
    }
  } catch {
    return null;
  }
}

function toView(event: EventRecord): GatewayEventView {
  return {
    id: event.id,
    label: eventLabels[event.type] ?? event.type,
    time: formatDateTime(event.time),
    detail: eventDetail(event),
  };
}

async function load() {
  loading.value = true;
  try {
    const all = await eventApi.list(200);
    events.value = all.filter(isGatewayEvent).slice(0, EVENT_LIMIT).map(toView);
  } finally {
    loading.value = false;
  }
}

onMounted(load);

// 网关状态一变（启停、自动切换接手方、错误计数）就重读事件，卡片跟着刷新。
watch(
  [
    () => gateway.phase.value,
    () => gateway.status.value?.activeModelId,
    () => gateway.status.value?.errors,
  ],
  () => void load(),
);
</script>

<template>
  <Card class="gap-4">
    <CardHeader>
      <div class="space-y-1">
        <CardTitle>网关事件</CardTitle>
        <CardDescription class="text-xs">
          本地网关的启停、自动切换与错误记录
        </CardDescription>
      </div>
    </CardHeader>

    <CardContent>
      <ol v-if="events.length" class="h-60 divide-y overflow-y-auto">
        <li
          v-for="event in events"
          :key="event.id"
          class="flex h-12 items-center justify-between gap-3 px-2 transition-colors duration-150 hover:bg-accent/50"
        >
          <div class="min-w-0 leading-tight">
            <p class="truncate text-xs font-medium">{{ event.label }}</p>
            <p
              v-if="event.detail"
              class="mt-0.5 truncate text-[11px] text-muted-foreground"
            >
              {{ event.detail }}
            </p>
          </div>
          <time
            class="shrink-0 font-mono text-[11px] tabular-nums text-muted-foreground"
          >
            {{ event.time }}
          </time>
        </li>
      </ol>

      <EmptyState
        v-else-if="!loading"
        :icon="Activity"
        title="还没有网关事件"
        description="网关启动、停止或自动切换模型时，会在这里留下记录。"
      />
      <p v-else class="py-8 text-center text-xs text-muted-foreground">
        正在读取网关事件…
      </p>
    </CardContent>
  </Card>
</template>

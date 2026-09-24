<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { useRouter } from "vue-router";
import { ArrowLeft, RefreshCw } from "lucide";

import MorphIconBox from "@/components/common/MorphIconBox.vue";
import RequestTable from "@/components/stats/RequestTable.vue";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { usageApi } from "@/lib/ipc";
import type { UsageRecord } from "@/lib/types";

const PAGE_SIZE = 50;

const router = useRouter();

const items = ref<UsageRecord[]>([]);
const total = ref(0);
const page = ref(1);
const loading = ref(true);
const error = ref<string | null>(null);

const pageCount = computed(() => Math.max(1, Math.ceil(total.value / PAGE_SIZE)));
const rangeLabel = computed(() => {
  if (!total.value) return "共 0 条";
  const from = (page.value - 1) * PAGE_SIZE + 1;
  const to = Math.min(page.value * PAGE_SIZE, total.value);
  return `第 ${from}–${to} 条，共 ${total.value} 条`;
});

async function load(): Promise<void> {
  loading.value = true;
  error.value = null;
  try {
    const result = await usageApi.page((page.value - 1) * PAGE_SIZE, PAGE_SIZE);
    items.value = result.items;
    total.value = result.total;
    // 记录被清理导致当前页越界时，退回最后一页
    if (page.value > pageCount.value) {
      page.value = pageCount.value;
      await load();
    }
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause);
  } finally {
    loading.value = false;
  }
}

function changePage(next: number): void {
  page.value = Math.min(Math.max(1, next), pageCount.value);
  load();
}

onMounted(load);

function goBack(): void {
  if (window.history.state?.back) router.back();
  else router.push({ name: "stats" });
}
</script>

<template>
  <div class="mx-auto max-w-6xl space-y-4">
    <div class="flex items-center justify-between gap-3">
      <Button variant="ghost" size="sm" class="gap-1.5" @click="goBack">
        <MorphIconBox :icon="ArrowLeft" :size="15" />
        返回
      </Button>
      <Button variant="outline" size="sm" class="gap-1.5" :disabled="loading" @click="load">
        <MorphIconBox :icon="RefreshCw" :size="15" :class="loading ? 'animate-spin' : ''" />
        刷新
      </Button>
    </div>

    <Card>
      <CardHeader>
        <div class="space-y-1">
          <CardTitle>请求明细</CardTitle>
          <CardDescription class="text-xs">全部网关调用记录，最新在前。</CardDescription>
        </div>
      </CardHeader>
      <CardContent class="space-y-3">
        <div v-if="error" class="py-10 text-center text-sm text-muted-foreground">
          读取失败：{{ error }}
        </div>
        <RequestTable v-else :records="items" />

        <div
          v-if="!error"
          class="flex items-center justify-between gap-3 border-t pt-3 text-xs text-muted-foreground"
        >
          <span>{{ rangeLabel }}</span>
          <div class="flex items-center gap-2">
            <span class="tabular-nums">第 {{ page }} / {{ pageCount }} 页</span>
            <Button
              variant="outline"
              size="xs"
              :disabled="page <= 1 || loading"
              @click="changePage(page - 1)"
            >
              上一页
            </Button>
            <Button
              variant="outline"
              size="xs"
              :disabled="page >= pageCount || loading"
              @click="changePage(page + 1)"
            >
              下一页
            </Button>
          </div>
        </div>
      </CardContent>
    </Card>
  </div>
</template>

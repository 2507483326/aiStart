<script setup lang="ts">
import { ref } from "vue";
import { Wrench } from "lucide";

import MorphIconBox from "@/components/common/MorphIconBox.vue";
import { Badge } from "@/components/ui/badge";
import { HoverCard, HoverCardContent, HoverCardTrigger } from "@/components/ui/hover-card";
import type { ToolDefinition } from "@/lib/payload";

defineProps<{ tools: ToolDefinition[] }>();

// 悬停卡默认由 hover 触发，这里改成点击：open 受控后忽略组件内部的 update:open，
// 关闭改由卡片自身的点击外部 / Esc 事件驱动。
const openIndex = ref(-1);
const wasOpen = ref(false);

function rememberOpen(index: number): void {
  wasOpen.value = openIndex.value === index;
}

function toggleFromClick(index: number): void {
  openIndex.value = wasOpen.value ? -1 : index;
}

function toggle(index: number): void {
  openIndex.value = openIndex.value === index ? -1 : index;
}

function close(): void {
  openIndex.value = -1;
}
</script>

<template>
  <div class="flex flex-wrap gap-1.5">
    <HoverCard
      v-for="(tool, index) in tools"
      :key="tool.name"
      :open="openIndex === index"
    >
      <HoverCardTrigger as-child>
        <Badge
          variant="outline"
          class="cursor-pointer gap-1 transition-colors hover:bg-accent"
          role="button"
          tabindex="0"
          @pointerdown="rememberOpen(index)"
          @click="toggleFromClick(index)"
          @keydown.enter="toggle(index)"
        >
          <MorphIconBox :icon="Wrench" :size="11" />
          <span class="font-mono">{{ tool.name }}</span>
        </Badge>
      </HoverCardTrigger>

      <HoverCardContent
        align="start"
        class="w-80 space-y-1.5 text-xs"
        @pointer-down-outside="close"
        @escape-key-down="close"
      >
        <div class="flex items-center gap-1.5 font-mono font-medium">
          <MorphIconBox :icon="Wrench" :size="12" />
          {{ tool.name }}
        </div>
        <p class="leading-relaxed break-words whitespace-pre-wrap text-muted-foreground">
          {{ tool.description || "该工具没有提供说明。" }}
        </p>
      </HoverCardContent>
    </HoverCard>
  </div>
</template>

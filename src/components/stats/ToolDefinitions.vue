<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { Languages, Wrench } from "lucide";

import MorphIconBox from "@/components/common/MorphIconBox.vue";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { HoverCard, HoverCardContent, HoverCardTrigger } from "@/components/ui/hover-card";
import { useTranslate } from "@/composables/useTranslate";
import type { ToolDefinition } from "@/lib/payload";

defineProps<{ tools: ToolDefinition[] }>();

// 悬停卡默认由 hover 触发，这里改成点击：open 受控后忽略组件内部的 update:open，
// 关闭改由卡片自身的点击外部 / Esc 事件驱动。
const openIndex = ref(-1);
const wasOpen = ref(false);

// 弹层同一时刻只开一个，翻译状态随之重置，避免串到下一个工具。
const { translated, translating, showing, error, toggle, reset } = useTranslate();

watch(openIndex, reset);

const translateLabel = computed(() => {
  if (translating.value) return "翻译中…";
  if (!translated.value) return "翻译";
  return showing.value ? "原文" : "译文";
});

function rememberOpen(index: number): void {
  wasOpen.value = openIndex.value === index;
}

function toggleFromClick(index: number): void {
  openIndex.value = wasOpen.value ? -1 : index;
}

function toggleOpen(index: number): void {
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
          @keydown.enter="toggleOpen(index)"
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
          <Button
            v-if="tool.description.trim()"
            variant="ghost"
            size="xs"
            class="ml-auto gap-1 px-1.5 font-sans text-[11px] text-muted-foreground"
            :disabled="translating"
            @click="toggle(tool.description)"
          >
            <MorphIconBox :icon="Languages" :size="12" />
            {{ translateLabel }}
          </Button>
        </div>

        <p
          v-if="showing && translated"
          class="leading-relaxed break-words whitespace-pre-wrap text-muted-foreground"
        >
          {{ translated }}
        </p>
        <p v-else class="leading-relaxed break-words whitespace-pre-wrap text-muted-foreground">
          {{ tool.description || "该工具没有提供说明。" }}
        </p>

        <p v-if="error" class="text-[11px] text-destructive">{{ error }}</p>
      </HoverCardContent>
    </HoverCard>
  </div>
</template>

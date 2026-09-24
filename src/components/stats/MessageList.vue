<script setup lang="ts">
import { CornerDownRight, Wrench } from "lucide";

import MorphIconBox from "@/components/common/MorphIconBox.vue";
import PayloadCard from "@/components/stats/PayloadCard.vue";
import { Badge } from "@/components/ui/badge";
import type { PayloadMessage } from "@/lib/payload";

defineProps<{ messages: PayloadMessage[] }>();

const roleClass: Record<string, string> = {
  user: "border-transparent bg-sky-500/15 text-sky-600 dark:text-sky-400",
  assistant: "border-transparent bg-emerald-500/15 text-emerald-600 dark:text-emerald-400",
  system: "border-transparent bg-amber-500/15 text-amber-600 dark:text-amber-400",
};

function roleStyle(role: string): string {
  return roleClass[role] ?? "border-transparent bg-muted text-muted-foreground";
}

/** 折叠/翻译只作用于可读文本，跳过工具参数与工具返回等结构化内容。 */
function messageText(message: PayloadMessage): string {
  return message.parts
    .filter((part) => part.kind === "text" || part.kind === "thinking")
    .map((part) => part.text)
    .join("\n\n");
}
</script>

<template>
  <div class="space-y-3">
    <PayloadCard
      v-for="(message, index) in messages"
      :key="index"
      :label="message.role"
      :label-class="roleStyle(message.role)"
      :text="messageText(message)"
    >
      <template v-for="(part, partIndex) in message.parts" :key="partIndex">
        <pre
          v-if="part.kind === 'text'"
          class="font-sans text-xs leading-relaxed break-words whitespace-pre-wrap"
          >{{ part.text }}</pre
        >

        <details
          v-else-if="part.kind === 'thinking'"
          class="rounded-md border border-dashed bg-muted/30 px-2.5 py-1.5"
        >
          <summary class="cursor-pointer text-[11px] text-muted-foreground">思考过程</summary>
          <pre
            class="mt-1.5 font-sans text-[11px] leading-relaxed text-muted-foreground whitespace-pre-wrap break-words"
            >{{ part.text }}</pre
          >
        </details>

        <div
          v-else-if="part.kind === 'tool_use'"
          class="rounded-md border border-l-2 border-l-primary/50 bg-muted/30 p-2.5"
        >
          <div class="flex items-center gap-2">
            <MorphIconBox :icon="Wrench" :size="13" class="shrink-0 text-muted-foreground" />
            <span class="text-xs font-medium">{{ part.name || "工具调用" }}</span>
            <span v-if="part.id" class="truncate font-mono text-[10px] text-muted-foreground">
              {{ part.id }}
            </span>
          </div>
          <pre
            class="mt-2 rounded bg-background/70 p-2 font-mono text-[11px] leading-relaxed whitespace-pre-wrap break-all"
            >{{ part.input }}</pre
          >
        </div>

        <div
          v-else-if="part.kind === 'tool_result'"
          class="rounded-md border border-l-2 p-2.5"
          :class="
            part.isError
              ? 'border-destructive/40 border-l-destructive bg-destructive/10'
              : 'border-l-muted-foreground/40 bg-muted/30'
          "
        >
          <div class="flex items-center gap-2">
            <MorphIconBox
              :icon="CornerDownRight"
              :size="13"
              class="shrink-0 text-muted-foreground"
            />
            <span class="text-xs font-medium">
              {{ part.name ? `工具返回 · ${part.name}` : "工具返回" }}
            </span>
            <span v-if="part.isError" class="text-[10px] text-destructive">错误</span>
          </div>
          <pre
            v-if="part.output"
            class="mt-2 font-mono text-[11px] leading-relaxed whitespace-pre-wrap break-all"
            >{{ part.output }}</pre
          >
        </div>

        <Badge v-else-if="part.kind === 'image'" variant="outline">{{ part.label }}</Badge>
      </template>

      <p v-if="!message.parts.length" class="text-xs text-muted-foreground">（无内容）</p>
    </PayloadCard>
  </div>
</template>

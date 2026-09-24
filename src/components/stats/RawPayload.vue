<script setup lang="ts">
import { prettyJson } from "@/lib/format";

defineProps<{
  raw: string | null;
  label?: string;
  truncated?: boolean;
}>();
</script>

<template>
  <details class="rounded-md border bg-muted/20">
    <summary class="cursor-pointer px-3 py-1.5 text-[11px] text-muted-foreground">
      {{ label ?? "原始报文" }}<span v-if="truncated">（已截断）</span>
    </summary>
    <pre
      class="max-h-[55vh] overflow-auto border-t p-3 font-mono text-[11px] leading-relaxed break-all whitespace-pre-wrap"
      >{{ prettyJson(raw) }}</pre
    >
  </details>
</template>

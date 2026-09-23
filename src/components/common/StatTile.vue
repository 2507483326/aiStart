<script setup lang="ts">
defineProps<{
  label: string;
  value: string;
  hint?: string;
  tone?: "default" | "success" | "warning" | "danger";
}>();

const toneClass: Record<string, string> = {
  default: "text-foreground",
  success: "text-emerald-600 dark:text-emerald-400",
  warning: "text-amber-600 dark:text-amber-400",
  danger: "text-destructive",
};
</script>

<template>
  <div class="rounded-lg border bg-card px-4 py-3">
    <p class="text-xs text-muted-foreground">{{ label }}</p>
    <p class="mt-1 text-lg font-semibold tabular-nums" :class="toneClass[tone ?? 'default']">
      {{ value }}
    </p>
    <p v-if="hint" class="mt-0.5 text-xs text-muted-foreground">{{ hint }}</p>
  </div>
</template>

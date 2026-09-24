<script setup lang="ts">
defineProps<{
  label: string;
  value: string;
  hint?: string;
  tone?: "default" | "success" | "warning" | "danger";
  valueClass?: string;
}>();

const toneClass: Record<string, string> = {
  default: "text-foreground",
  success: "text-emerald-600 dark:text-emerald-400",
  warning: "text-amber-600 dark:text-amber-400",
  danger: "text-destructive",
};
</script>

<template>
  <div
    class="flex flex-col rounded-lg border bg-card px-4 py-3 transition-[border-color,background-color] duration-200 hover:border-foreground/20 hover:bg-accent/[0.04]"
  >
    <p class="text-xs text-muted-foreground">{{ label }}</p>
    <div class="flex min-w-0 flex-1 flex-col justify-center">
      <p
        class="mt-1 text-base font-semibold tabular-nums"
        :class="[toneClass[tone ?? 'default'], valueClass]"
      >
        {{ value }}
      </p>
      <p v-if="hint" class="mt-0.5 text-xs text-muted-foreground">{{ hint }}</p>
    </div>
  </div>
</template>

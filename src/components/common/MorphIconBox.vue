<script setup lang="ts">
import { MorphIcon } from "morphicons/vue";
import type { IconInput, SpringPreset } from "morphicons/vue";

withDefaults(
  defineProps<{
    icon: IconInput;
    size?: number | string;
    strokeWidth?: number | string;
    spring?: SpringPreset;
  }>(),
  {
    size: 16,
    strokeWidth: 2,
    spring: "snappy",
  },
);
</script>

<template>
  <MorphIcon
    :icon="icon"
    :size="size"
    :stroke-width="strokeWidth"
    :spring="spring"
  />
</template>

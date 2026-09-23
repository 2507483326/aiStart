<script setup lang="ts">
import type { IconInput } from "morphicons/vue";

import MorphIconBox from "@/components/common/MorphIconBox.vue";

defineProps<{
  icon: IconInput;
  title: string;
  description?: string;
}>();
</script>

<template>
  <div
    class="flex flex-col items-center justify-center gap-3 rounded-lg border border-dashed px-6 py-14 text-center"
  >
    <div
      class="flex size-11 items-center justify-center rounded-full bg-muted text-muted-foreground"
    >
      <MorphIconBox :icon="icon" :size="20" />
    </div>
    <div class="space-y-1">
      <p class="text-sm font-medium">{{ title }}</p>
      <p v-if="description" class="max-w-md text-xs text-muted-foreground">
        {{ description }}
      </p>
    </div>
    <slot />
  </div>
</template>

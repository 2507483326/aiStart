<script setup lang="ts">
import { computed } from "vue";

import { Badge } from "@/components/ui/badge";
import { formatLabels, formatShortLabels } from "@/lib/format";
import type { ModelFormat } from "@/lib/types";

const props = defineProps<{ format: ModelFormat; full?: boolean }>();

const variant = computed(() => {
  switch (props.format) {
    case "anthropic-messages":
      return "default" as const;
    case "openai-completions":
      return "secondary" as const;
    default:
      return "outline" as const;
  }
});

const label = computed(() =>
  props.full ? formatLabels[props.format] : formatShortLabels[props.format],
);
</script>

<template>
  <Badge :variant="variant" :title="formatLabels[format]">{{ label }}</Badge>
</template>

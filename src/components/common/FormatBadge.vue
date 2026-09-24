<script setup lang="ts">
import { computed } from "vue";

import { Badge } from "@/components/ui/badge";
import { formatLabels, formatShortLabels } from "@/lib/format";
import type { ModelFormat } from "@/lib/types";

const props = defineProps<{ format: ModelFormat; full?: boolean }>();

const label = computed(() =>
  props.full ? formatLabels[props.format] : formatShortLabels[props.format],
);
</script>

<template>
  <Badge
    variant="outline"
    class="px-1.5 py-0 text-[10px] font-normal leading-4 text-muted-foreground"
    :title="formatLabels[format]"
  >
    {{ label }}
  </Badge>
</template>

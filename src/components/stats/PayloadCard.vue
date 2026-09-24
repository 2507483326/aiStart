<script setup lang="ts">
import { computed, ref } from "vue";
import { ChevronDown, ChevronRight, Languages } from "lucide";

import MorphIconBox from "@/components/common/MorphIconBox.vue";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { useTranslate } from "@/composables/useTranslate";

const props = withDefaults(
  defineProps<{
    label?: string;
    labelClass?: string;
    text?: string;
    defaultCollapsed?: boolean;
  }>(),
  {
    label: "",
    labelClass: "",
    text: "",
    defaultCollapsed: false,
  },
);

const collapsed = ref(props.defaultCollapsed);
const { translated, translating, showing, error, toggle } = useTranslate();

const translatable = computed(() => props.text.trim().length > 0);
const translateLabel = computed(() => {
  if (translating.value) return "翻译中…";
  if (!translated.value) return "翻译";
  return showing.value ? "原文" : "译文";
});
</script>

<template>
  <div class="rounded-lg border bg-card">
    <div class="flex items-center gap-2 border-b px-3 py-1.5">
      <Badge v-if="label" variant="outline" :class="labelClass">
        {{ label }}
      </Badge>
      <slot name="meta" />

      <div class="ml-auto flex items-center gap-0.5">
        <Button
          v-if="translatable"
          variant="ghost"
          size="xs"
          class="gap-1 px-1.5 text-[11px] text-muted-foreground"
          :disabled="translating"
          @click="toggle(text)"
        >
          <MorphIconBox :icon="Languages" :size="12" />
          {{ translateLabel }}
        </Button>
        <Button
          variant="ghost"
          size="icon-xs"
          class="text-muted-foreground"
          :aria-label="collapsed ? '展开' : '折叠'"
          :aria-expanded="!collapsed"
          @click="collapsed = !collapsed"
        >
          <MorphIconBox :icon="collapsed ? ChevronRight : ChevronDown" :size="14" />
        </Button>
      </div>
    </div>

    <div v-show="!collapsed" class="space-y-2 px-3 py-2.5">
      <template v-if="showing && translated">
        <pre
          class="font-sans text-xs leading-relaxed break-words whitespace-pre-wrap"
          >{{ translated }}</pre
        >
      </template>
      <slot v-else />

      <p v-if="error" class="text-[11px] text-destructive">{{ error }}</p>
    </div>
  </div>
</template>

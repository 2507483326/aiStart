<script setup lang="ts">
import { ref } from "vue";
import { ChevronDown, ChevronRight } from "lucide";

import MorphIconBox from "@/components/common/MorphIconBox.vue";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";

const props = withDefaults(
  defineProps<{
    title: string;
    description?: string;
    contentClass?: string;
    defaultCollapsed?: boolean;
  }>(),
  {
    description: "",
    contentClass: "space-y-3",
    defaultCollapsed: false,
  },
);

const collapsed = ref(props.defaultCollapsed);
</script>

<template>
  <Card>
    <CardHeader>
      <div class="flex items-center justify-between gap-3">
        <div class="space-y-1">
          <CardTitle>{{ title }}</CardTitle>
          <CardDescription v-if="description" class="text-xs">{{ description }}</CardDescription>
        </div>
        <div class="flex items-center gap-2">
          <slot name="action" />
          <Button
            variant="ghost"
            size="icon-xs"
            class="text-muted-foreground"
            :aria-label="collapsed ? '展开' : '折叠'"
            :aria-expanded="!collapsed"
            @click="collapsed = !collapsed"
          >
            <MorphIconBox :icon="collapsed ? ChevronRight : ChevronDown" :size="15" />
          </Button>
        </div>
      </div>
    </CardHeader>

    <CardContent v-show="!collapsed" :class="contentClass">
      <slot />
    </CardContent>
  </Card>
</template>

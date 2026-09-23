<script setup lang="ts">
import { computed } from "vue";
import { useRoute } from "vue-router";
import { BrainCircuit, Boxes, LayoutDashboard } from "@lucide/vue";

import { cn } from "@/lib/utils";

const route = useRoute();

const items = [
  { name: "dashboard", label: "面板", to: "/dashboard", icon: LayoutDashboard },
  { name: "apps", label: "应用", to: "/apps", icon: Boxes },
  { name: "models", label: "模型", to: "/models", icon: BrainCircuit },
];

const activeName = computed(() => String(route.name ?? ""));
</script>

<template>
  <nav class="space-y-1 px-3">
    <RouterLink
      v-for="item in items"
      :key="item.name"
      :to="item.to"
      :class="
        cn(
          'flex items-center gap-2.5 rounded-md px-3 py-2 text-sm font-medium transition-colors',
          activeName === item.name
            ? 'bg-sidebar-accent text-sidebar-accent-foreground'
            : 'text-muted-foreground hover:bg-sidebar-accent/50 hover:text-foreground',
        )
      "
    >
      <component :is="item.icon" class="size-4 shrink-0" />
      <span>{{ item.label }}</span>
    </RouterLink>
  </nav>
</template>

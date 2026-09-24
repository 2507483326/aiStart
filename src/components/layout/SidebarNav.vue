<script setup lang="ts">
import { useRoute } from "vue-router";
import { BarChart3, Boxes, BrainCircuit, Funnel, LayoutDashboard } from "@lucide/vue";

import { cn } from "@/lib/utils";

const route = useRoute();

const items = [
  { name: "dashboard", label: "面板", to: "/dashboard", icon: LayoutDashboard },
  { name: "apps", label: "应用", to: "/apps", icon: Boxes },
  { name: "models", label: "模型", to: "/models", icon: BrainCircuit },
  { name: "filters", label: "提示词注入", to: "/filters", icon: Funnel },
  { name: "stats", label: "统计", to: "/stats", icon: BarChart3 },
];

// 按路径前缀匹配，让 /stats/requests/:id 这类子页面仍高亮所属区块
function isActive(to: string): boolean {
  return route.path === to || route.path.startsWith(`${to}/`);
}
</script>

<template>
  <nav class="space-y-1 px-3">
    <RouterLink
      v-for="item in items"
      :key="item.name"
      :to="item.to"
      :class="
        cn(
          'relative flex cursor-pointer items-center gap-2.5 rounded-md px-3 py-2 text-sm font-medium transition-[color,background-color] duration-150',
          isActive(item.to)
            ? 'bg-sidebar-accent text-sidebar-accent-foreground'
            : 'text-muted-foreground hover:bg-sidebar-accent/50 hover:text-foreground',
        )
      "
    >
      <span
        v-if="isActive(item.to)"
        class="absolute top-1/2 left-0 h-4 w-0.5 -translate-y-1/2 rounded-full bg-primary"
      />
      <component :is="item.icon" class="size-4 shrink-0 transition-colors" />
      <span>{{ item.label }}</span>
    </RouterLink>
  </nav>
</template>

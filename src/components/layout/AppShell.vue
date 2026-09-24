<script setup lang="ts">
import { computed } from "vue";
import { useRoute } from "vue-router";
import {
  Activity,
  BarChart3,
  Boxes,
  BrainCircuit,
  Funnel,
  LayoutDashboard,
  Settings2,
  Sparkles,
} from "lucide";

import MorphIconBox from "@/components/common/MorphIconBox.vue";
import AppBackdrop from "@/components/layout/AppBackdrop.vue";
import SettingsDialog from "@/components/layout/SettingsDialog.vue";
import SidebarNav from "@/components/layout/SidebarNav.vue";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { useGateway } from "@/composables/useGateway";
import { useSettings } from "@/composables/useSettings";

const route = useRoute();
const gateway = useGateway();
const { info } = useSettings();

const title = computed(() => (route.meta.title as string) ?? "AI Start");
const subtitle = computed(() => (route.meta.subtitle as string) ?? "");

const sectionIcons = {
  dashboard: LayoutDashboard,
  apps: Boxes,
  models: BrainCircuit,
  filters: Funnel,
  stats: BarChart3,
  requests: BarChart3,
  "request-detail": BarChart3,
};

const activeIcon = computed(
  () => sectionIcons[route.name as keyof typeof sectionIcons] ?? Sparkles,
);
</script>

<template>
  <div class="flex h-screen overflow-hidden bg-background">
    <aside
      class="flex w-60 shrink-0 flex-col border-r bg-sidebar bg-gradient-to-b from-sidebar to-sidebar/90"
    >
      <div class="flex items-center gap-2.5 px-5 py-4">
        <div
          class="flex size-8 items-center justify-center rounded-lg bg-primary text-primary-foreground"
        >
          <MorphIconBox :icon="Sparkles" :size="17" />
        </div>
        <div class="leading-tight">
          <p class="text-sm font-semibold">{{ info?.name ?? "AI Start" }}</p>
          <p class="text-[11px] text-muted-foreground">
            v{{ info?.version ?? "0.1.0" }} · {{ info?.platform ?? "-" }}
          </p>
        </div>
      </div>

      <SidebarNav />

      <div class="mt-auto space-y-2 border-t p-3">
        <div class="rounded-lg border bg-card px-3 py-2.5 transition-colors duration-200 hover:border-foreground/20">
          <div class="flex items-center justify-between">
            <span class="flex items-center gap-1.5 text-xs font-medium">
              <MorphIconBox
                :icon="Activity"
                :size="13"
                :class="gateway.running.value ? 'text-emerald-500' : 'text-muted-foreground'"
              />
              本地网关
            </span>
            <Badge :variant="gateway.running.value ? 'default' : 'outline'">
              {{ gateway.running.value ? "运行中" : "已停止" }}
            </Badge>
          </div>
          <p class="mt-1.5 truncate font-mono text-[11px] text-muted-foreground">
            {{ gateway.status.value?.baseUrl ?? "—" }}
          </p>
        </div>

        <SettingsDialog>
          <Button variant="ghost" size="sm" class="w-full justify-start gap-2">
            <MorphIconBox :icon="Settings2" :size="15" />
            设置
          </Button>
        </SettingsDialog>
      </div>
    </aside>

    <div class="relative flex min-w-0 flex-1 flex-col">
      <AppBackdrop />
      <header
        class="relative flex items-center gap-3 border-b bg-gradient-to-b from-background/80 to-background/60 px-6 py-3.5 backdrop-blur-sm shadow-[0_1px_2px_-1px_rgb(0_0_0/0.06)]"
      >
        <MorphIconBox :icon="activeIcon" :size="19" class="text-muted-foreground" />
        <div class="leading-tight">
          <p class="text-base font-semibold">{{ title }}</p>
          <p class="text-xs text-muted-foreground">{{ subtitle }}</p>
        </div>
      </header>

      <main class="relative flex-1 overflow-y-auto px-6 py-5">
        <RouterView v-slot="{ Component }">
          <Transition name="page" mode="out-in">
            <component :is="Component" />
          </Transition>
        </RouterView>
      </main>
    </div>
  </div>
</template>

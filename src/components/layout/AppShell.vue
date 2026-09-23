<script setup lang="ts">
import { computed } from "vue";
import { useRoute } from "vue-router";
import {
  Activity,
  BrainCircuit,
  Boxes,
  LayoutDashboard,
  Power,
  Settings2,
  Sparkles,
} from "lucide";

import MorphIconBox from "@/components/common/MorphIconBox.vue";
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
};

const activeIcon = computed(
  () => sectionIcons[route.name as keyof typeof sectionIcons] ?? Sparkles,
);

const gatewayIcon = computed(() => (gateway.running.value ? Activity : Power));

async function toggleGateway() {
  if (gateway.running.value) {
    await gateway.stop();
  } else {
    await gateway.start();
  }
}
</script>

<template>
  <div class="flex h-screen overflow-hidden bg-background">
    <aside class="flex w-60 shrink-0 flex-col border-r bg-sidebar">
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
        <div class="rounded-lg border bg-card px-3 py-2.5">
          <div class="flex items-center justify-between">
            <span class="flex items-center gap-1.5 text-xs font-medium">
              <MorphIconBox
                :icon="gatewayIcon"
                :size="13"
                :class="gateway.running.value ? 'text-emerald-500' : 'text-muted-foreground'"
              />
              本地网关
            </span>
            <Badge :variant="gateway.running.value ? 'default' : 'outline'" class="text-[10px]">
              {{ gateway.running.value ? "运行中" : "已停止" }}
            </Badge>
          </div>
          <p class="mt-1.5 truncate font-mono text-[11px] text-muted-foreground">
            {{ gateway.status.value?.baseUrl ?? "—" }}
          </p>
          <Button
            variant="outline"
            size="sm"
            class="mt-2 w-full gap-2"
            :disabled="gateway.loading.value"
            @click="toggleGateway"
          >
            <MorphIconBox :icon="gatewayIcon" :size="14" />
            {{ gateway.running.value ? "停止" : "启动" }}
          </Button>
        </div>

        <SettingsDialog>
          <Button variant="ghost" size="sm" class="w-full justify-start gap-2">
            <MorphIconBox :icon="Settings2" :size="15" />
            设置
          </Button>
        </SettingsDialog>
      </div>
    </aside>

    <div class="flex min-w-0 flex-1 flex-col">
      <header class="flex items-center gap-3 border-b px-6 py-3.5">
        <MorphIconBox :icon="activeIcon" :size="19" class="text-muted-foreground" />
        <div class="leading-tight">
          <p class="text-sm font-semibold">{{ title }}</p>
          <p class="text-xs text-muted-foreground">{{ subtitle }}</p>
        </div>
      </header>

      <main class="flex-1 overflow-y-auto px-6 py-5">
        <RouterView />
      </main>
    </div>
  </div>
</template>

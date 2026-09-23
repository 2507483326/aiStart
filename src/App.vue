<script setup lang="ts">
import { onMounted } from "vue";
import { useIntervalFn } from "@vueuse/core";

import AppShell from "@/components/layout/AppShell.vue";
import { Toaster } from "@/components/ui/sonner";
import { useGateway } from "@/composables/useGateway";
import { useSettings } from "@/composables/useSettings";

const gateway = useGateway();
const { refresh: refreshSettings } = useSettings();

onMounted(async () => {
  await Promise.all([refreshSettings(), gateway.refresh()]);
});

useIntervalFn(() => gateway.refresh(), 5000);
</script>

<template>
  <AppShell />
  <Toaster position="bottom-right" />
</template>

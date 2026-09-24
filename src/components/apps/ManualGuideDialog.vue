<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { Copy } from "lucide";

import MorphIconBox from "@/components/common/MorphIconBox.vue";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { useGateway } from "@/composables/useGateway";
import { sourceAppIcon } from "@/lib/format";
import { notifySuccess } from "@/lib/notify";
import type { ToolApp } from "@/lib/types";

const props = defineProps<{
  app: ToolApp;
  open: boolean;
}>();

const emit = defineEmits<{ "update:open": [value: boolean] }>();

const { status } = useGateway();

// 手动接入的应用没有可写入的配置，这里给出「对接说明」，由用户在 GUI 里照着填。
const GATEWAY_ALIAS = "aiStart";

const baseUrl = computed(() => status.value?.baseUrl ?? "http://127.0.0.1:8931");

const endpoints = [
  { path: "/v1/messages", label: "Messages" },
  { path: "/v1/chat/completions", label: "Completions" },
  { path: "/v1/responses", label: "Responses" },
];
const selectedEndpoint = ref(endpoints[0]);

// 打开时把协议切到该应用默认推荐的那一个。
watch(
  () => props.open,
  (open) => {
    if (!open) return;
    const preferred =
      props.app.kind === "codex"
        ? "/v1/responses"
        : "/v1/chat/completions";
    selectedEndpoint.value =
      endpoints.find((endpoint) => endpoint.path === preferred) ?? endpoints[0];
  },
);

const endpointUrl = computed(() => `${baseUrl.value}${selectedEndpoint.value.path}`);

const protocolHint = computed(() => {
  switch (props.app.kind) {
    case "codex":
      return "Responses（wire_api = \"responses\"）";
    case "workbuddy":
      return "OpenAI 兼容（Chat Completions）";
    case "zcode":
      return "Messages / Completions / Responses 任选";
    default:
      return "OpenAI 兼容";
  }
});

const sample = computed(
  () => `curl ${endpointUrl.value} \\
  -H "content-type: application/json" \\
  -H "x-api-key: ${props.app.apiKey}" \\
  -d '{"model":"${GATEWAY_ALIAS}","max_tokens":64,"messages":[{"role":"user","content":"ping"}]}'`,
);

const icon = computed(() => sourceAppIcon(props.app.kind));

async function copyText(text: string, message: string) {
  await navigator.clipboard.writeText(text);
  notifySuccess(message);
}
</script>

<template>
  <Dialog :open="open" @update:open="emit('update:open', $event)">
    <DialogContent class="sm:max-w-lg">
      <DialogHeader>
        <DialogTitle class="flex items-center gap-2">
          <img v-if="icon" :src="icon" :alt="app.name" class="size-5 object-contain" />
          对接说明 · {{ app.name }}
        </DialogTitle>
        <DialogDescription>
          在{{ app.configTarget }}里添加一个自定义供应商，按下面的信息填写即可。本应用不会改动它的配置文件。
        </DialogDescription>
      </DialogHeader>

      <div class="space-y-4 py-2">
        <div class="divide-y rounded-md border bg-muted/40 text-sm">
          <div class="space-y-2 px-3 py-2.5 transition-colors duration-150 hover:bg-accent/30">
            <div class="flex flex-wrap items-center gap-2">
              <span class="text-muted-foreground">接口地址</span>
              <div class="flex gap-0.5 rounded-md border bg-background p-0.5">
                <button
                  v-for="endpoint in endpoints"
                  :key="endpoint.path"
                  type="button"
                  class="rounded px-2 py-0.5 text-xs transition-colors"
                  :class="
                    endpoint.path === selectedEndpoint.path
                      ? 'bg-primary text-primary-foreground'
                      : 'text-muted-foreground hover:bg-accent'
                  "
                  @click="selectedEndpoint = endpoint"
                >
                  {{ endpoint.label }}
                </button>
              </div>
            </div>
            <div class="flex items-center gap-2">
              <span class="min-w-0 flex-1 break-all font-mono text-foreground">
                {{ endpointUrl }}
              </span>
              <Button
                variant="ghost"
                size="icon-xs"
                class="shrink-0 text-muted-foreground"
                aria-label="复制接口地址"
                @click="copyText(endpointUrl, '接口地址已复制')"
              >
                <MorphIconBox :icon="Copy" :size="14" />
              </Button>
            </div>
          </div>

          <div class="flex items-center gap-2 px-3 py-2.5 transition-colors duration-150 hover:bg-accent/30">
            <span class="shrink-0 text-muted-foreground">API Key</span>
            <span class="min-w-0 flex-1 break-all font-mono text-foreground">
              {{ app.apiKey }}
            </span>
            <Button
              variant="ghost"
              size="icon-xs"
              class="shrink-0 text-muted-foreground"
              aria-label="复制 API Key"
              @click="copyText(app.apiKey, 'API Key 已复制')"
            >
              <MorphIconBox :icon="Copy" :size="14" />
            </Button>
          </div>

          <div class="flex items-center gap-2 px-3 py-2.5 transition-colors duration-150 hover:bg-accent/30">
            <span class="shrink-0 text-muted-foreground">模型名称</span>
            <span class="min-w-0 flex-1 font-mono text-foreground">{{ GATEWAY_ALIAS }}</span>
            <Button
              variant="ghost"
              size="icon-xs"
              class="shrink-0 text-muted-foreground"
              aria-label="复制模型名称"
              @click="copyText(GATEWAY_ALIAS, '模型名称已复制')"
            >
              <MorphIconBox :icon="Copy" :size="14" />
            </Button>
          </div>

          <div class="flex items-center gap-2 px-3 py-2.5 transition-colors duration-150 hover:bg-accent/30">
            <span class="shrink-0 text-muted-foreground">协议格式</span>
            <span class="min-w-0 flex-1 text-foreground">{{ protocolHint }}</span>
          </div>
        </div>

        <div class="space-y-2">
          <div class="flex items-center justify-between">
            <p class="text-sm text-muted-foreground">调用示例</p>
            <Button
              variant="ghost"
              size="sm"
              class="gap-1.5"
              @click="copyText(sample, '调用示例已复制')"
            >
              <MorphIconBox :icon="Copy" :size="14" />
              复制
            </Button>
          </div>
          <pre
            class="overflow-x-auto rounded-md border bg-muted/40 px-3 py-2 font-mono text-sm leading-relaxed"
            >{{ sample }}</pre
          >
        </div>
      </div>

      <DialogFooter>
        <Button @click="emit('update:open', false)">完成</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>

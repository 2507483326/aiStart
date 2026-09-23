<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { LoaderCircle, RefreshCw } from "lucide";

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
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Separator } from "@/components/ui/separator";
import { Switch } from "@/components/ui/switch";
import { useModels } from "@/composables/useModels";
import { notifyError, notifySuccess } from "@/lib/notify";
import type { ModelConfig, ModelFormat, ModelInput } from "@/lib/types";

const open = defineModel<boolean>("open", { required: true });

const props = defineProps<{ model: ModelConfig | null }>();
const emit = defineEmits<{ saved: [] }>();

const { formats, presets, save, fetchUpstream } = useModels();

const name = ref("");
const format = ref<ModelFormat>("openai-completions");
const baseUrl = ref("");
const apiKey = ref("");
const model = ref("");
const supports1m = ref(false);
const presetLabel = ref("");
const upstreamModels = ref<string[]>([]);
const saving = ref(false);
const fetching = ref(false);

const isEdit = computed(() => Boolean(props.model));
const activeFormat = computed(() =>
  formats.value.find((item) => item.format === format.value),
);

function reset() {
  const source = props.model;
  name.value = source?.name ?? "";
  format.value = source?.format ?? "openai-completions";
  baseUrl.value = source?.baseUrl ?? "https://api.openai.com/v1";
  apiKey.value = source?.apiKey ?? "";
  model.value = source?.model ?? "";
  supports1m.value = source?.supports1m ?? false;
  presetLabel.value = "";
  upstreamModels.value = [];
}

watch(open, (value) => {
  if (value) reset();
});

function changeFormat(value: unknown) {
  const next = value as ModelFormat;
  if (next === format.value) return;
  const info = formats.value.find((item) => item.format === next);
  if (info) baseUrl.value = info.defaultBaseUrl;
  upstreamModels.value = [];
  format.value = next;
}

function applyPreset(presetName: string) {
  const preset = presets.value.find((item) => item.name === presetName);
  if (!preset) return;
  presetLabel.value = preset.name;
  upstreamModels.value = [];
  name.value = preset.name;
  format.value = preset.format;
  baseUrl.value = preset.baseUrl;
  model.value = preset.model;
}

async function loadUpstreamModels() {
  if (!baseUrl.value.trim()) {
    notifyError("请先填写 Base URL");
    return;
  }
  fetching.value = true;
  try {
    const list = await fetchUpstream(baseUrl.value.trim(), apiKey.value, format.value);
    if (!list) return;
    upstreamModels.value = list;
    if (!model.value && list.length) model.value = list[0];
    notifySuccess(`获取到 ${list.length} 个模型`);
  } finally {
    fetching.value = false;
  }
}

async function submit() {
  const input: ModelInput = {
    id: props.model?.id,
    name: name.value.trim(),
    format: format.value,
    baseUrl: baseUrl.value.trim(),
    apiKey: apiKey.value,
    model: model.value.trim(),
    supports1m: supports1m.value,
  };

  saving.value = true;
  try {
    const ok = await save(input);
    if (ok) {
      open.value = false;
      emit("saved");
    }
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <Dialog v-model:open="open">
    <DialogContent class="max-h-[88vh] overflow-y-auto sm:max-w-xl">
      <DialogHeader>
        <DialogTitle>{{ isEdit ? "编辑模型" : "添加模型" }}</DialogTitle>
        <DialogDescription>
          选择上游协议，网关会把它统一翻译成 Anthropic Messages 协议供桌面客户端使用。
        </DialogDescription>
      </DialogHeader>

      <div class="space-y-4 py-2">
        <div v-if="!isEdit && presets.length" class="space-y-2">
          <Label>快速填充</Label>
          <Select
            :model-value="presetLabel"
            @update:model-value="(value) => applyPreset(String(value ?? ''))"
          >
            <SelectTrigger class="w-full">
              <SelectValue>
                {{ presetLabel || "从常用预设开始…" }}
              </SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectItem v-for="preset in presets" :key="preset.name" :value="preset.name">
                {{ preset.name }} — {{ preset.note }}
              </SelectItem>
            </SelectContent>
          </Select>
        </div>

        <Separator />

        <div class="grid grid-cols-2 gap-4">
          <div class="space-y-2">
            <Label for="model-name">显示名称</Label>
            <Input id="model-name" v-model="name" placeholder="DeepSeek Chat" />
          </div>
          <div class="space-y-2">
            <Label for="model-format">上游协议</Label>
            <Select :model-value="format" @update:model-value="changeFormat">
              <SelectTrigger id="model-format" class="w-full">
                <SelectValue>
                  {{ activeFormat?.displayName ?? "选择上游协议" }}
                </SelectValue>
              </SelectTrigger>
              <SelectContent>
                <SelectItem v-for="item in formats" :key="item.format" :value="item.format">
                  {{ item.displayName }}
                </SelectItem>
              </SelectContent>
            </Select>
          </div>
        </div>

        <p
          v-if="activeFormat"
          class="rounded-md border bg-muted/40 px-3 py-2 text-xs text-muted-foreground"
        >
          {{ activeFormat.description }}
        </p>

        <div class="space-y-2">
          <Label for="model-base-url">Base URL</Label>
          <Input
            id="model-base-url"
            v-model="baseUrl"
            class="font-mono text-xs"
            :placeholder="activeFormat?.defaultBaseUrl ?? 'https://api.example.com/v1'"
          />
        </div>

        <div class="space-y-2">
          <Label for="model-api-key">API Key</Label>
          <Input
            id="model-api-key"
            v-model="apiKey"
            type="password"
            class="font-mono text-xs"
            placeholder="sk-…"
          />
        </div>

        <div class="space-y-2">
          <div class="flex items-center justify-between">
            <Label for="model-upstream">上游模型 ID</Label>
            <Button
              variant="ghost"
              size="sm"
              class="gap-1.5"
              :disabled="fetching"
              @click="loadUpstreamModels"
            >
              <MorphIconBox
                :icon="fetching ? LoaderCircle : RefreshCw"
                :size="13"
                :class="fetching ? 'animate-spin' : ''"
              />
              获取模型列表
            </Button>
          </div>
          <Input
            id="model-upstream"
            v-model="model"
            class="font-mono text-xs"
            placeholder="deepseek-chat"
          />
          <Select
            v-if="upstreamModels.length"
            :model-value="model"
            @update:model-value="(value) => (model = String(value ?? ''))"
          >
            <SelectTrigger class="w-full">
              <SelectValue>
                {{ model || `从获取到的 ${upstreamModels.length} 个模型中选择…` }}
              </SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectItem v-for="id in upstreamModels" :key="id" :value="id">
                {{ id }}
              </SelectItem>
            </SelectContent>
          </Select>
        </div>

        <div class="flex items-center justify-between gap-4 rounded-md border px-3 py-2.5">
          <div class="space-y-0.5">
            <Label for="model-1m" class="cursor-pointer">支持 1M 上下文</Label>
            <p class="text-xs text-muted-foreground">
              标记后会写入 Claude Desktop 的模型列表，选择器会额外提供 1M 变体。
            </p>
          </div>
          <Switch id="model-1m" v-model="supports1m" />
        </div>
      </div>

      <DialogFooter>
        <Button variant="outline" @click="open = false">取消</Button>
        <Button :disabled="saving" @click="submit">
          {{ isEdit ? "保存修改" : "添加模型" }}
        </Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { ChevronDown, Search } from "@lucide/vue";
import { LoaderCircle, RefreshCw } from "lucide";
import { ComboboxInput } from "reka-ui";

import MorphIconBox from "@/components/common/MorphIconBox.vue";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Combobox,
  ComboboxAnchor,
  ComboboxEmpty,
  ComboboxItem,
  ComboboxItemIndicator,
  ComboboxList,
  ComboboxTrigger,
  ComboboxViewport,
} from "@/components/ui/combobox";
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
import { useModels } from "@/composables/useModels";
import { notifyError, notifySuccess } from "@/lib/notify";
import type { ModelConfig, ModelFormat, ModelInput } from "@/lib/types";

const open = defineModel<boolean>("open", { required: true });

const props = defineProps<{ model: ModelConfig | null }>();
const emit = defineEmits<{ saved: [] }>();

const { formats, save, fetchUpstream } = useModels();

const name = ref("");
const format = ref<ModelFormat>("openai-completions");
const baseUrl = ref("");
const apiKey = ref("");
const model = ref("");
const supports1m = ref(false);
const upstreamModels = ref<string[]>([]);
const searchTerm = ref("");
const comboOpen = ref(false);
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
  searchTerm.value = source?.model ?? "";
  upstreamModels.value = [];
  comboOpen.value = false;
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

function onSearch(value: unknown) {
  searchTerm.value = String(value ?? "");
}

function commitTypedModel() {
  const typed = searchTerm.value.trim();
  if (!typed) return;
  if (!upstreamModels.value.includes(typed)) {
    model.value = typed;
  }
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
    if (!model.value && list.length) {
      model.value = list[0];
      searchTerm.value = list[0];
    }
    notifySuccess(`获取到 ${list.length} 个模型`);
    comboOpen.value = true;
  } finally {
    fetching.value = false;
  }
}

async function submit() {
  commitTypedModel();
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

        <Separator />

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

          <Combobox
            v-model="model"
            :open="comboOpen"
            :reset-search-term-on-blur="false"
            @update:open="comboOpen = $event"
          >
            <ComboboxAnchor as-child>
              <div class="relative">
                <Search
                  class="pointer-events-none absolute top-1/2 left-3 size-3.5 -translate-y-1/2 opacity-50"
                />
                <ComboboxInput
                  id="model-upstream"
                  :display-value="(value: unknown) => String(value ?? '')"
                  placeholder="deepseek-chat，或点右侧箭头从列表选择"
                  class="h-9 w-full rounded-md border bg-transparent pr-9 pl-8 font-mono text-xs shadow-xs outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50"
                  @update:model-value="onSearch"
                  @focus="comboOpen = true"
                  @blur="commitTypedModel"
                />
                <ComboboxTrigger
                  class="absolute top-1/2 right-2 -translate-y-1/2 rounded-sm p-1 text-muted-foreground hover:text-foreground"
                  aria-label="展开模型列表"
                >
                  <ChevronDown class="size-4 opacity-60" />
                </ComboboxTrigger>
              </div>
            </ComboboxAnchor>

            <ComboboxList class="w-(--reka-combobox-trigger-width)">
              <ComboboxEmpty>没有匹配的模型，先点「获取模型列表」</ComboboxEmpty>
              <ComboboxViewport class="max-h-72 overflow-y-auto p-1">
                <ComboboxItem v-for="id in upstreamModels" :key="id" :value="id">
                  {{ id }}
                  <ComboboxItemIndicator />
                </ComboboxItem>
              </ComboboxViewport>
            </ComboboxList>
          </Combobox>

          <label class="flex cursor-pointer items-center gap-2 pt-1 text-xs text-muted-foreground">
            <Checkbox v-model="supports1m" />
            该模型支持 1M 上下文
          </label>
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

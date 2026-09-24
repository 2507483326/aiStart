<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from "vue";
import { Eye, EyeOff, LoaderCircle, RefreshCw, Wifi } from "lucide";

import MorphIconBox from "@/components/common/MorphIconBox.vue";
import UpstreamModelSelect from "@/components/models/UpstreamModelSelect.vue";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
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
import { formatLatency } from "@/lib/format";
import { notifyError, notifySuccess } from "@/lib/notify";
import type { ModelConfig, ModelFormat, ModelInput, TestResult } from "@/lib/types";

const open = defineModel<boolean>("open", { required: true });

const props = defineProps<{ model: ModelConfig | null }>();
const emit = defineEmits<{ saved: [] }>();

const { formats, save, fetchUpstream, fetchUpstreamQuiet, testConfig, upstreamCache } = useModels();

const name = ref("");
const format = ref<ModelFormat>("openai-completions");
const baseUrl = ref("");
const apiKey = ref("");
const showApiKey = ref(false);
const model = ref("");
const supports1m = ref(false);
const upstreamModels = ref<string[]>([]);
const saving = ref(false);
const fetching = ref(false);
const testing = ref(false);
const testResult = ref<TestResult | null>(null);

let clearTimer: ReturnType<typeof setTimeout> | undefined;

onUnmounted(() => {
  if (clearTimer) clearTimeout(clearTimer);
});

// 表单值一变，上一次的探测结果就不再可信
watch([baseUrl, apiKey, model, format], () => {
  testResult.value = null;
  if (clearTimer) clearTimeout(clearTimer);
});

const isEdit = computed(() => Boolean(props.model));
const activeFormat = computed(() =>
  formats.value.find((item) => item.format === format.value),
);
// 添加时 Base URL 只给 placeholder 提示，留空就用该协议的默认地址
const resolvedBaseUrl = computed(
  () => baseUrl.value.trim() || activeFormat.value?.defaultBaseUrl || "",
);

function reset() {
  const source = props.model;
  name.value = source?.name ?? "";
  format.value = source?.format ?? "openai-completions";
  baseUrl.value = source?.baseUrl ?? "";
  apiKey.value = source?.apiKey ?? "";
  showApiKey.value = false;
  model.value = source?.model ?? "";
  supports1m.value = source?.supports1m ?? false;
  // 编辑时直接用进入模型页预取到的上游列表，拉到过就展示成选择框
  upstreamModels.value = source ? (upstreamCache.value[source.id] ?? []) : [];
}

watch(open, (value) => {
  if (value) {
    reset();
    loadUpstreamQuiet();
  }
});

// 编辑时若没有预取到上游列表，打开弹窗后静默补拉一次，拉到就切成选择框
async function loadUpstreamQuiet() {
  const source = props.model;
  if (!source || upstreamModels.value.length || fetching.value) return;
  fetching.value = true;
  try {
    const list = await fetchUpstreamQuiet(source.baseUrl, source.apiKey, source.format);
    const unchanged =
      baseUrl.value === source.baseUrl &&
      apiKey.value === source.apiKey &&
      format.value === source.format;
    if (unchanged && list.length) upstreamModels.value = list;
  } finally {
    fetching.value = false;
  }
}

function changeFormat(value: unknown) {
  const next = value as ModelFormat;
  if (next === format.value) return;
  // 编辑时切换协议顺手填入默认地址；添加时保留 placeholder，不写入实际值
  if (isEdit.value) {
    const info = formats.value.find((item) => item.format === next);
    if (info) baseUrl.value = info.defaultBaseUrl;
  }
  upstreamModels.value = [];
  format.value = next;
}

async function loadUpstreamModels() {
  const target = resolvedBaseUrl.value;
  if (!target) {
    notifyError("请先填写 Base URL");
    return;
  }
  fetching.value = true;
  try {
    const list = await fetchUpstream(target, apiKey.value, format.value);
    if (!list) return;
    upstreamModels.value = list;
    if (!model.value && list.length) {
      model.value = list[0];
    }
    notifySuccess(`获取到 ${list.length} 个模型`);
  } finally {
    fetching.value = false;
  }
}

// 用表单里（可能尚未保存的）当前值发一次最小补全请求，验证配置是否可用
async function runTest() {
  testResult.value = null;
  if (clearTimer) clearTimeout(clearTimer);
  const target = resolvedBaseUrl.value;
  if (!target) {
    notifyError("请先填写 Base URL");
    return;
  }
  if (!model.value.trim()) {
    notifyError("请先填写上游模型 ID");
    return;
  }
  testing.value = true;
  try {
    const outcome = await testConfig(
      target,
      apiKey.value,
      model.value.trim(),
      format.value,
    );
    if (!outcome) return;
    testResult.value = outcome;
    // 成功提示 5s 后自动消失，失败保持可见
    if (outcome.ok) {
      clearTimer = setTimeout(() => {
        testResult.value = null;
      }, 5000);
    }
  } finally {
    testing.value = false;
  }
}

async function submit() {
  const input: ModelInput = {
    id: props.model?.id,
    name: name.value.trim(),
    format: format.value,
    baseUrl: resolvedBaseUrl.value,
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
          <div class="relative">
            <Input
              id="model-api-key"
              v-model="apiKey"
              :type="showApiKey ? 'text' : 'password'"
              class="pr-9 font-mono text-xs"
              placeholder="sk-…"
            />
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              class="absolute top-1/2 right-0.5 -translate-y-1/2 text-muted-foreground hover:text-foreground"
              :aria-label="showApiKey ? '隐藏 API Key' : '显示 API Key'"
              @click="showApiKey = !showApiKey"
            >
              <MorphIconBox :icon="showApiKey ? EyeOff : Eye" :size="14" />
            </Button>
          </div>
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

          <!-- 未获取到模型列表前只给一个普通输入框，拉到列表后才变成可搜索的下拉框 -->
          <Input
            v-if="!upstreamModels.length"
            id="model-upstream"
            v-model="model"
            class="font-mono text-xs"
            placeholder="deepseek-chat"
          />

          <UpstreamModelSelect
            v-else
            id="model-upstream"
            v-model="model"
            :options="upstreamModels"
          />

          <label class="flex cursor-pointer items-center gap-2 pt-1 text-xs text-muted-foreground">
            <Checkbox v-model="supports1m" />
            该模型支持 1M 上下文
          </label>
        </div>
      </div>

      <p
        v-if="testResult"
        class="px-1 text-xs"
        :class="testResult.ok ? 'text-emerald-600 dark:text-emerald-400' : 'text-destructive'"
      >
        {{ testResult.ok ? "连通正常" : "连通失败" }} · {{ formatLatency(testResult.latencyMs) }} ·
        {{ testResult.preview ?? testResult.message }}
      </p>

      <DialogFooter>
        <Button variant="outline" class="mr-auto gap-1" :disabled="testing" @click="runTest">
          <MorphIconBox
            :icon="testing ? LoaderCircle : Wifi"
            :size="14"
            :class="testing ? 'animate-spin' : ''"
          />
          测试连接
        </Button>
        <Button variant="outline" @click="open = false">取消</Button>
        <Button :disabled="saving" @click="submit">
          {{ isEdit ? "保存修改" : "添加模型" }}
        </Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>

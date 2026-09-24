<script setup lang="ts">
import { computed, ref, watch } from "vue";

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
import { Textarea } from "@/components/ui/textarea";
import { useFilters } from "@/composables/useFilters";
import { notifyError } from "@/lib/notify";
import type {
  FilterInput,
  FilterRule,
  PromptMode,
  ReplaceTarget,
  RequestFilter,
} from "@/lib/types";

type FilterRuleKind = FilterRule["kind"];

const open = defineModel<boolean>("open", { required: true });
const props = defineProps<{ filter: RequestFilter | null }>();
const emit = defineEmits<{ saved: [] }>();

const { save } = useFilters();

const RULE_KINDS: { value: FilterRuleKind; label: string }[] = [
  { value: "system-prompt", label: "注入系统提示词" },
  { value: "request-params", label: "覆盖请求参数" },
  { value: "text-replace", label: "文本查找替换" },
];

const PROMPT_MODES: { value: PromptMode; label: string }[] = [
  { value: "append", label: "追加到末尾" },
  { value: "prepend", label: "插入到开头" },
  { value: "replace", label: "整体替换" },
];

const REPLACE_TARGETS: { value: ReplaceTarget; label: string }[] = [
  { value: "all", label: "system + messages" },
  { value: "system", label: "仅 system" },
  { value: "messages", label: "仅 messages" },
];

const name = ref("");
const enabled = ref(true);
const kind = ref<FilterRuleKind>("system-prompt");
const saving = ref(false);

const promptMode = ref<PromptMode>("append");
const promptText = ref("");

const temperature = ref("");
const maxTokens = ref("");
const topP = ref("");
const stopSequences = ref("");

const find = ref("");
const replace = ref("");
const target = ref<ReplaceTarget>("all");

const isEdit = computed(() => Boolean(props.filter));
const kindLabel = computed(
  () => RULE_KINDS.find((item) => item.value === kind.value)?.label ?? "",
);
const modeLabel = computed(
  () => PROMPT_MODES.find((item) => item.value === promptMode.value)?.label ?? "",
);
const targetLabel = computed(
  () => REPLACE_TARGETS.find((item) => item.value === target.value)?.label ?? "",
);

function reset() {
  const source = props.filter;
  name.value = source?.name ?? "";
  enabled.value = source?.enabled ?? true;
  kind.value = source?.rule.kind ?? "system-prompt";

  promptMode.value = "append";
  promptText.value = "";
  temperature.value = "";
  maxTokens.value = "";
  topP.value = "";
  stopSequences.value = "";
  find.value = "";
  replace.value = "";
  target.value = "all";

  const rule = source?.rule;
  if (!rule) return;
  switch (rule.kind) {
    case "system-prompt":
      promptMode.value = rule.mode;
      promptText.value = rule.text;
      break;
    case "request-params":
      temperature.value = rule.temperature?.toString() ?? "";
      maxTokens.value = rule.maxTokens?.toString() ?? "";
      topP.value = rule.topP?.toString() ?? "";
      stopSequences.value = rule.stopSequences?.join(", ") ?? "";
      break;
    case "text-replace":
      find.value = rule.find;
      replace.value = rule.replace;
      target.value = rule.target;
      break;
  }
}

watch(open, (value) => {
  if (value) reset();
});

function toNumber(value: string): number | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  const parsed = Number(trimmed);
  return Number.isFinite(parsed) ? parsed : null;
}

function buildRule(): FilterRule | null {
  if (kind.value === "system-prompt") {
    if (!promptText.value.trim()) {
      notifyError("系统提示词不能为空");
      return null;
    }
    return { kind: "system-prompt", mode: promptMode.value, text: promptText.value };
  }
  if (kind.value === "request-params") {
    const stops = stopSequences.value
      .split(",")
      .map((item) => item.trim())
      .filter(Boolean);
    const max = toNumber(maxTokens.value);
    return {
      kind: "request-params",
      temperature: toNumber(temperature.value),
      maxTokens: max === null ? null : Math.max(0, Math.trunc(max)),
      topP: toNumber(topP.value),
      stopSequences: stops.length ? stops : null,
    };
  }
  if (!find.value) {
    notifyError("要查找的文本不能为空");
    return null;
  }
  return { kind: "text-replace", find: find.value, replace: replace.value, target: target.value };
}

async function submit() {
  if (!name.value.trim()) {
    notifyError("过滤器名称不能为空");
    return;
  }
  const rule = buildRule();
  if (!rule) return;

  const input: FilterInput = {
    id: props.filter?.id,
    name: name.value.trim(),
    enabled: enabled.value,
    rule,
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
        <DialogTitle>{{ isEdit ? "编辑过滤器" : "添加过滤器" }}</DialogTitle>
        <DialogDescription>
          过滤器会在网关把请求转发给上游之前，按列表顺序依次生效。
        </DialogDescription>
      </DialogHeader>

      <div class="space-y-4 py-2">
        <div class="space-y-2">
          <Label for="filter-name">名称</Label>
          <Input id="filter-name" v-model="name" placeholder="例如：统一追加系统提示词" />
        </div>

        <div class="space-y-2">
          <Label for="filter-rule-kind">规则类型</Label>
          <Select
            :model-value="kind"
            @update:model-value="(value) => (kind = value as FilterRuleKind)"
          >
            <SelectTrigger id="filter-rule-kind" class="w-full">
              <SelectValue>{{ kindLabel }}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectItem v-for="item in RULE_KINDS" :key="item.value" :value="item.value">
                {{ item.label }}
              </SelectItem>
            </SelectContent>
          </Select>
        </div>

        <Separator />

        <template v-if="kind === 'system-prompt'">
          <div class="space-y-2">
            <Label for="filter-prompt-mode">注入方式</Label>
            <Select
              :model-value="promptMode"
              @update:model-value="(value) => (promptMode = value as PromptMode)"
            >
              <SelectTrigger id="filter-prompt-mode" class="w-full">
                <SelectValue>{{ modeLabel }}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                <SelectItem v-for="item in PROMPT_MODES" :key="item.value" :value="item.value">
                  {{ item.label }}
                </SelectItem>
              </SelectContent>
            </Select>
          </div>
          <div class="space-y-2">
            <Label for="filter-prompt-text">系统提示词</Label>
            <Textarea
              id="filter-prompt-text"
              v-model="promptText"
              placeholder="始终使用中文回答，并保持简洁。"
            />
          </div>
        </template>

        <template v-else-if="kind === 'request-params'">
          <div class="grid grid-cols-2 gap-4">
            <div class="space-y-2">
              <Label for="filter-temperature">temperature</Label>
              <Input
                id="filter-temperature"
                v-model="temperature"
                type="number"
                step="0.1"
                placeholder="留空则不覆盖"
              />
            </div>
            <div class="space-y-2">
              <Label for="filter-max-tokens">max_tokens</Label>
              <Input
                id="filter-max-tokens"
                v-model="maxTokens"
                type="number"
                placeholder="留空则不覆盖"
              />
            </div>
            <div class="space-y-2">
              <Label for="filter-top-p">top_p</Label>
              <Input
                id="filter-top-p"
                v-model="topP"
                type="number"
                step="0.1"
                placeholder="留空则不覆盖"
              />
            </div>
            <div class="space-y-2">
              <Label for="filter-stop">stop_sequences</Label>
              <Input id="filter-stop" v-model="stopSequences" placeholder="逗号分隔，留空则不设置" />
            </div>
          </div>
          <p class="text-xs text-muted-foreground">只覆盖填写的参数，未填写的保持请求原值。</p>
        </template>

        <template v-else>
          <div class="space-y-2">
            <Label for="filter-find">查找内容</Label>
            <Input
              id="filter-find"
              v-model="find"
              class="font-mono text-xs"
              placeholder="要被替换的字面量文本"
            />
          </div>
          <div class="space-y-2">
            <Label for="filter-replace">替换为</Label>
            <Input
              id="filter-replace"
              v-model="replace"
              class="font-mono text-xs"
              placeholder="留空表示删除匹配内容"
            />
          </div>
          <div class="space-y-2">
            <Label for="filter-target">作用范围</Label>
            <Select
              :model-value="target"
              @update:model-value="(value) => (target = value as ReplaceTarget)"
            >
              <SelectTrigger id="filter-target" class="w-full">
                <SelectValue>{{ targetLabel }}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                <SelectItem v-for="item in REPLACE_TARGETS" :key="item.value" :value="item.value">
                  {{ item.label }}
                </SelectItem>
              </SelectContent>
            </Select>
          </div>
        </template>

        <Separator />

        <label class="flex cursor-pointer items-center justify-between">
          <span class="text-sm font-medium">启用</span>
          <Switch v-model="enabled" />
        </label>
      </div>

      <DialogFooter>
        <Button variant="outline" @click="open = false">取消</Button>
        <Button :disabled="saving" @click="submit">
          {{ isEdit ? "保存修改" : "添加过滤器" }}
        </Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>

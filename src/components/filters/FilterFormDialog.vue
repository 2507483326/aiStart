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
import type { FilterInput, FilterRule, PromptMode, RequestFilter } from "@/lib/types";

const open = defineModel<boolean>("open", { required: true });
const props = defineProps<{ filter: RequestFilter | null }>();
const emit = defineEmits<{ saved: [] }>();

const { save } = useFilters();

const PROMPT_MODES: { value: PromptMode; label: string }[] = [
  { value: "append", label: "追加到末尾" },
  { value: "prepend", label: "插入到开头" },
];

const name = ref("");
const enabled = ref(true);
const promptMode = ref<PromptMode>("append");
const promptText = ref("");
const saving = ref(false);

const isEdit = computed(() => Boolean(props.filter));
const modeLabel = computed(
  () => PROMPT_MODES.find((item) => item.value === promptMode.value)?.label ?? "",
);

function reset() {
  const rule = props.filter?.rule;
  name.value = props.filter?.name ?? "";
  enabled.value = props.filter?.enabled ?? true;
  promptMode.value = rule?.mode ?? "append";
  promptText.value = rule?.text ?? "";
}

watch(open, (value) => {
  if (value) reset();
});

function buildRule(): FilterRule | null {
  if (!promptText.value.trim()) {
    notifyError("系统提示词不能为空");
    return null;
  }
  return { kind: "system-prompt", mode: promptMode.value, text: promptText.value };
}

async function submit() {
  if (!name.value.trim()) {
    notifyError("名称不能为空");
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
        <DialogTitle>{{ isEdit ? "编辑提示词注入" : "添加提示词注入" }}</DialogTitle>
        <DialogDescription>
          注入规则会在网关把请求转发给上游之前，按列表顺序依次生效。
        </DialogDescription>
      </DialogHeader>

      <div class="space-y-4 py-2">
        <div class="space-y-2">
          <Label for="filter-name">名称</Label>
          <Input id="filter-name" v-model="name" placeholder="例如：统一追加系统提示词" />
        </div>

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

        <Separator />

        <label class="flex cursor-pointer items-center justify-between">
          <span class="text-sm font-medium">启用</span>
          <Switch v-model="enabled" />
        </label>
      </div>

      <DialogFooter>
        <Button variant="outline" @click="open = false">取消</Button>
        <Button :disabled="saving" @click="submit">
          {{ isEdit ? "保存修改" : "添加提示词注入" }}
        </Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>

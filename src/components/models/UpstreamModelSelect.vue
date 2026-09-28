<script setup lang="ts">
import { computed, ref } from "vue";
import { Check, ChevronDown } from "@lucide/vue";
import { ComboboxAnchor, ComboboxInput, ComboboxTrigger } from "reka-ui";

import {
  Combobox,
  ComboboxGroup,
  ComboboxItem,
  ComboboxItemIndicator,
  ComboboxList,
  ComboboxViewport,
} from "@/components/ui/combobox";

const props = withDefaults(
  defineProps<{
    modelValue: string;
    options: string[];
    id?: string;
    size?: "default" | "sm";
    placeholder?: string;
    triggerClass?: string;
  }>(),
  { id: undefined, size: "default", placeholder: "选择模型", triggerClass: "" },
);

const emit = defineEmits<{ "update:modelValue": [value: string] }>();

const open = ref(false);
const searchTerm = ref("");

const displayValue = (value: unknown) => String(value ?? "");

function onOpen(value: boolean) {
  open.value = value;
  if (!value) searchTerm.value = "";
}

function onSearch(value: unknown) {
  searchTerm.value = String(value ?? "");
}

function update(value: unknown) {
  emit("update:modelValue", String(value ?? ""));
}

function prefixOf(id: string): string {
  const separator = id.search(/[/\-_.]/);
  return separator > 0 ? id.slice(0, separator) : id;
}

const groups = computed(() => {
  const buckets = new Map<string, string[]>();
  for (const id of props.options) {
    const prefix = prefixOf(id);
    const bucket = buckets.get(prefix);
    if (bucket) bucket.push(id);
    else buckets.set(prefix, [id]);
  }
  const ordered = [...buckets.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([prefix, items]) => ({ prefix, items: [...items].sort((a, b) => a.localeCompare(b)) }));

  // 已选模型所属的分类提到最顶部，方便下次快速看到
  const selected = props.modelValue
    ? ordered.findIndex((group) => group.prefix === prefixOf(props.modelValue))
    : -1;
  if (selected > 0) ordered.unshift(...ordered.splice(selected, 1));
  return ordered;
});

// 输入的不是列表里已有的项时，提供一条「使用输入的 ID」的选项，实现手输自定义模型。
const customCandidate = computed(() => {
  const term = searchTerm.value.trim();
  if (!term || props.options.includes(term)) return "";
  return term;
});
</script>

<template>
  <Combobox
    :model-value="modelValue"
    :open="open"
    @update:model-value="update"
    @update:open="onOpen"
  >
    <ComboboxAnchor
      :class="[
        'relative flex items-center',
        size === 'sm' ? 'w-56' : 'w-full',
        triggerClass,
      ]"
    >
      <ComboboxInput
        :id="id"
        :display-value="displayValue"
        :placeholder="placeholder"
        spellcheck="false"
        :class="[
          'border-input placeholder:text-muted-foreground w-full min-w-0 rounded-md border bg-transparent py-1 font-mono shadow-xs outline-none transition-[color,box-shadow] focus-visible:border-ring focus-visible:ring-ring/50 focus-visible:ring-3',
          size === 'sm' ? 'h-7 pr-7 pl-2 text-xs' : 'h-8 pr-8 pl-3 text-xs',
        ]"
        @update:model-value="onSearch"
      />
      <ComboboxTrigger
        class="text-muted-foreground hover:text-foreground focus-visible:ring-ring/50 absolute right-1 flex items-center justify-center rounded-sm outline-none focus-visible:ring-3"
        :class="size === 'sm' ? 'size-5' : 'size-6'"
        aria-label="展开模型列表"
      >
        <ChevronDown
          class="opacity-60 transition-transform duration-200"
          :class="[size === 'sm' ? 'size-3.5' : 'size-4', open ? 'rotate-180' : '']"
        />
      </ComboboxTrigger>
    </ComboboxAnchor>

    <ComboboxList align="start" class="w-(--reka-combobox-trigger-width) min-w-64">
      <ComboboxViewport class="max-h-72 overflow-y-auto p-1">
        <ComboboxGroup v-for="group in groups" :key="group.prefix">
          <div class="flex items-center gap-2 px-2 pt-2 pb-1">
            <span class="border-border w-4 shrink-0 border-t border-dashed" />
            <span class="text-muted-foreground shrink-0 text-xs font-medium">
              {{ group.prefix }}
            </span>
            <span class="border-border flex-1 border-t border-dashed" />
          </div>
          <ComboboxItem v-for="id in group.items" :key="id" :value="id" :text-value="id">
            <span class="truncate font-mono text-xs">{{ id }}</span>
            <ComboboxItemIndicator>
              <Check class="size-4" />
            </ComboboxItemIndicator>
          </ComboboxItem>
        </ComboboxGroup>

        <ComboboxItem
          v-if="customCandidate"
          :key="customCandidate"
          :value="customCandidate"
          :text-value="customCandidate"
        >
          <span class="truncate font-mono text-xs">使用「{{ customCandidate }}」</span>
        </ComboboxItem>
      </ComboboxViewport>
    </ComboboxList>
  </Combobox>
</template>

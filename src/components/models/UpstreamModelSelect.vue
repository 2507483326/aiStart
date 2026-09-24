<script setup lang="ts">
import { computed, ref } from "vue";
import { Check, ChevronDown } from "@lucide/vue";

import { Button } from "@/components/ui/button";
import {
  Combobox,
  ComboboxAnchor,
  ComboboxGroup,
  ComboboxInput,
  ComboboxItem,
  ComboboxItemIndicator,
  ComboboxList,
  ComboboxTrigger,
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

function onOpen(value: boolean) {
  open.value = value;
  searchTerm.value = "";
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

const customCandidate = computed(() => {
  const term = searchTerm.value.trim();
  if (!term || props.options.includes(term)) return "";
  const matched = props.options.some((item) =>
    item.toLowerCase().includes(term.toLowerCase()),
  );
  return matched ? "" : term;
});
</script>

<template>
  <Combobox
    :model-value="modelValue"
    :open="open"
    @update:model-value="update"
    @update:open="onOpen"
  >
    <ComboboxAnchor as-child class="w-full">
      <ComboboxTrigger as-child>
        <Button
          :id="id"
          variant="outline"
          :size="size === 'sm' ? 'xs' : 'default'"
          class="justify-between font-mono font-normal"
          :class="[
            size === 'sm' ? 'w-56 text-xs' : 'w-full text-xs',
            triggerClass,
            modelValue ? '' : 'text-muted-foreground',
          ]"
        >
          <span class="truncate">{{ modelValue || placeholder }}</span>
          <ChevronDown
            class="shrink-0 opacity-60"
            :class="size === 'sm' ? 'size-3.5' : 'size-4'"
          />
        </Button>
      </ComboboxTrigger>
    </ComboboxAnchor>

    <ComboboxList align="start" class="w-(--reka-combobox-trigger-width) min-w-64">
      <ComboboxInput
        :display-value="() => ''"
        placeholder="搜索模型…"
        class="h-8 py-0 text-xs"
        @update:model-value="onSearch"
      />
      <ComboboxViewport class="max-h-72 overflow-y-auto p-1">
        <ComboboxGroup v-for="group in groups" :key="group.prefix">
          <div class="flex items-center gap-2 px-2 pt-2 pb-1">
            <span class="w-4 shrink-0 border-t border-dashed border-border" />
            <span class="shrink-0 text-xs font-medium text-muted-foreground">
              {{ group.prefix }}
            </span>
            <span class="flex-1 border-t border-dashed border-border" />
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
          :value="customCandidate"
          :text-value="customCandidate"
        >
          <span class="truncate font-mono text-xs">使用「{{ customCandidate }}」</span>
        </ComboboxItem>
      </ComboboxViewport>
    </ComboboxList>
  </Combobox>
</template>

<script setup lang="ts">
import { ref } from "vue";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";

withDefaults(
  defineProps<{
    title: string;
    description?: string;
    confirmText?: string;
    destructive?: boolean;
  }>(),
  { confirmText: "确认", destructive: false },
);

const emit = defineEmits<{ confirm: [] }>();
const open = ref(false);

function confirm() {
  open.value = false;
  emit("confirm");
}
</script>

<template>
  <Dialog v-model:open="open">
    <DialogTrigger as-child>
      <slot name="trigger" />
    </DialogTrigger>
    <DialogContent class="sm:max-w-sm">
      <DialogHeader>
        <DialogTitle>{{ title }}</DialogTitle>
        <DialogDescription v-if="description">{{ description }}</DialogDescription>
      </DialogHeader>
      <DialogFooter>
        <Button variant="outline" @click="open = false">取消</Button>
        <Button :variant="destructive ? 'destructive' : 'default'" @click="confirm">
          {{ confirmText }}
        </Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>

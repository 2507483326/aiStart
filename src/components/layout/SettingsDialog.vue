<script setup lang="ts">
import { ref, watch } from "vue";
import { Copy, FolderCog, FolderDown, FolderOpen } from "lucide";

import MorphIconBox from "@/components/common/MorphIconBox.vue";
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
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { useSettings } from "@/composables/useSettings";
import { openDownloadDir } from "@/lib/open";
import { notifySuccess } from "@/lib/notify";

const { settings, info, update } = useSettings();

const open = ref(false);
const port = ref<string>("");
const saving = ref(false);

watch(open, (value) => {
  if (!value || !settings.value) return;
  port.value = String(settings.value.gatewayPort);
});

async function save() {
  const parsed = Number.parseInt(port.value, 10);
  if (!Number.isInteger(parsed) || parsed < 1024 || parsed > 65535) {
    port.value = String(settings.value?.gatewayPort ?? 8931);
    return;
  }
  saving.value = true;
  try {
    await update({ gatewayPort: parsed });
    open.value = false;
  } finally {
    saving.value = false;
  }
}

async function copyDownloadDir() {
  const dir = info.value?.downloadDir;
  if (!dir) return;
  await navigator.clipboard.writeText(dir);
  notifySuccess("下载地址已复制");
}
</script>

<template>
  <Dialog v-model:open="open">
    <DialogTrigger as-child>
      <slot />
    </DialogTrigger>
    <DialogContent class="sm:max-w-lg">
      <DialogHeader>
        <DialogTitle>设置</DialogTitle>
        <DialogDescription>
          本地网关端口。修改端口会自动重启网关。
        </DialogDescription>
      </DialogHeader>

      <div class="space-y-5 py-2">
        <div class="space-y-2">
          <Label for="gateway-port">本地网关端口</Label>
          <Input id="gateway-port" v-model="port" inputmode="numeric" placeholder="8931" />
          <p class="text-xs text-muted-foreground">
            网关只监听 127.0.0.1，对外暴露 Anthropic Messages 接口。
          </p>
        </div>

        <div class="rounded-md border bg-muted/40 px-3 py-2 text-xs text-muted-foreground">
          <p class="flex items-center gap-1.5">
            <MorphIconBox :icon="FolderCog" :size="13" />
            应用数据目录
          </p>
          <p class="mt-1 break-all font-mono">{{ info?.configDir }}</p>
        </div>

        <div class="rounded-md border bg-muted/40 px-3 py-2 text-xs text-muted-foreground">
          <p class="flex items-center gap-1.5">
            <MorphIconBox :icon="FolderDown" :size="13" />
            下载文件夹
          </p>
          <p class="mt-1 break-all font-mono">{{ info?.downloadDir }}</p>
          <div class="mt-2 flex gap-2">
            <Button variant="outline" class="gap-1.5" @click="openDownloadDir">
              <MorphIconBox :icon="FolderOpen" :size="14" />
              打开下载文件夹
            </Button>
            <Button variant="outline" class="gap-1.5" @click="copyDownloadDir">
              <MorphIconBox :icon="Copy" :size="14" />
              复制下载地址
            </Button>
          </div>
        </div>
      </div>

      <DialogFooter>
        <Button variant="outline" @click="open = false">取消</Button>
        <Button :disabled="saving" @click="save">保存</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>

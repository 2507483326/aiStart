<script setup lang="ts">
import { ref, watch, computed } from "vue";
import { Clock, FolderCog, FolderOpen, Globe, Power } from "lucide";

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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { useSettings } from "@/composables/useSettings";
import { openDataDir } from "@/lib/open";

const { settings, info, update } = useSettings();

// 请求报文保留时间：0 = 永久（不清理）。值与后端 settings::RETENTION_DAY_OPTIONS 对齐。
const RETENTION_OPTIONS = [
  { value: "7", label: "7 天" },
  { value: "30", label: "30 天" },
  { value: "100", label: "100 天" },
  { value: "0", label: "永久" },
];

const open = ref(false);
const port = ref<string>("");
const proxyEnabled = ref(false);
const proxyUrl = ref<string>("");
const launchAtLogin = ref(true);
const retention = ref<string>("7");
const saving = ref(false);

const retentionLabel = computed(
  () => RETENTION_OPTIONS.find((item) => item.value === retention.value)?.label ?? "",
);

// 打开时用当前设置填表单，只点「保存」才写回去：取消就丢弃这一轮的改动。
watch(open, (value) => {
  if (!value || !settings.value) return;
  port.value = String(settings.value.gatewayPort);
  proxyEnabled.value = settings.value.proxyEnabled;
  proxyUrl.value = settings.value.proxyUrl;
  launchAtLogin.value = settings.value.launchAtLogin;
  retention.value = String(settings.value.requestRetentionDays);
});

async function save() {
  const parsed = Number.parseInt(port.value, 10);
  if (!Number.isInteger(parsed) || parsed < 1024 || parsed > 65535) {
    port.value = String(settings.value?.gatewayPort ?? 8931);
    return;
  }
  saving.value = true;
  try {
    // 代理地址在后端校验：写错了会带着原因报错，此时整批设置都不落库（端口也不会白改）。
    // 「开着却没地址」同样由后端拦下，前端不复刻这套规则，省得两处口径打架。
    const saved = await update({
      gatewayPort: parsed,
      proxyEnabled: proxyEnabled.value,
      proxyUrl: proxyUrl.value.trim(),
      launchAtLogin: launchAtLogin.value,
      requestRetentionDays: Number.parseInt(retention.value, 10),
    });
    // 存失败就别关窗：报错多半是代理地址没填对，用户得能就地改。
    if (saved) open.value = false;
  } finally {
    saving.value = false;
  }
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
          网关端口、开机启动与出站代理。修改端口会自动重启网关。
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

        <div class="space-y-2">
          <div class="flex items-center justify-between">
            <Label for="proxy-url" class="flex items-center gap-1.5">
              <MorphIconBox :icon="Globe" :size="13" />
              网络代理
            </Label>
            <Switch v-model="proxyEnabled" />
          </div>
          <Input
            id="proxy-url"
            v-model="proxyUrl"
            :disabled="!proxyEnabled"
            placeholder="http://127.0.0.1:7890"
          />
        </div>

        <div class="space-y-2">
          <Label for="request-retention" class="flex items-center gap-1.5">
            <MorphIconBox :icon="Clock" :size="13" />
            请求保存时间
          </Label>
          <Select v-model="retention">
            <SelectTrigger id="request-retention" class="w-full">
              <SelectValue>{{ retentionLabel }}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectItem v-for="item in RETENTION_OPTIONS" :key="item.value" :value="item.value">
                {{ item.label }}
              </SelectItem>
            </SelectContent>
          </Select>
          <p class="text-xs text-muted-foreground">
            超过该时间的请求报文会被清理；消耗明细与统计仍然保留。
          </p>
        </div>

        <label class="flex items-center justify-between rounded-md border px-3 py-2.5">
          <span class="space-y-0.5">
            <span class="flex items-center gap-1.5 text-sm font-medium">
              <MorphIconBox :icon="Power" :size="13" />
              开机启动
            </span>
            <span class="block text-xs font-normal text-muted-foreground">
              登录 Windows 后自动启动本应用。
            </span>
          </span>
          <Switch v-model="launchAtLogin" />
        </label>

        <div class="rounded-md border bg-muted/40 px-3 py-2 text-xs text-muted-foreground">
          <p class="flex items-center gap-1.5">
            <MorphIconBox :icon="FolderCog" :size="13" />
            应用数据目录
          </p>
          <p class="mt-1 break-all font-mono">{{ info?.configDir }}</p>
          <div class="mt-2">
            <Button variant="outline" class="gap-1.5" @click="openDataDir">
              <MorphIconBox :icon="FolderOpen" :size="14" />
              打开文件夹
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

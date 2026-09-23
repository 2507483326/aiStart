import { ref } from "vue";
import { listen } from "@tauri-apps/api/event";

import { appApi } from "@/lib/ipc";
import { attempt, notifySuccess } from "@/lib/notify";
import type { AppKind, DownloadProgress, ToolApp } from "@/lib/types";

const apps = ref<ToolApp[]>([]);
const loading = ref(false);
const busyKind = ref<AppKind | null>(null);
const download = ref<DownloadProgress | null>(null);
const lastSteps = ref<string[]>([]);
const listenerReady = ref(false);

async function ensureListener(): Promise<void> {
  if (listenerReady.value) return;
  await listen<DownloadProgress>("install://progress", (event) => {
    download.value = event.payload;
    if (event.payload.phase === "launched") {
      setTimeout(() => {
        download.value = null;
      }, 1200);
    }
  });
  listenerReady.value = true;
}

async function refresh(): Promise<void> {
  loading.value = true;
  try {
    apps.value = await appApi.list();
  } finally {
    loading.value = false;
  }
}

async function install(kind: AppKind): Promise<void> {
  busyKind.value = kind;
  try {
    const report = await attempt(() => appApi.install(kind), {
      error: "启动安装失败",
    });
    if (!report) return;
    lastSteps.value = report.steps;
    notifySuccess(
      `${kind === "claude-desktop" ? "Claude Desktop" : "DeepSeek Desktop"} 安装流程已启动`,
      report.steps[report.steps.length - 1],
    );
    await refresh();
  } finally {
    busyKind.value = null;
  }
}

async function update(kind: AppKind): Promise<void> {
  busyKind.value = kind;
  try {
    const report = await attempt(() => appApi.update(kind), {
      error: "更新失败",
    });
    if (!report) return;
    lastSteps.value = report.steps;
    notifySuccess("更新流程已启动", report.steps[report.steps.length - 1]);
    await refresh();
  } finally {
    busyKind.value = null;
  }
}

async function apply(kind: AppKind, modelId?: string): Promise<boolean> {
  busyKind.value = kind;
  try {
    const report = await attempt(() => appApi.apply(kind, modelId), {
      error: "一键应用模型失败",
    });
    if (!report) return false;
    lastSteps.value = report.steps;
    notifySuccess(`已把「${report.modelName}」接入 ${report.target.split(" → ")[0]}`, report.note ?? undefined);
    await refresh();
    return true;
  } finally {
    busyKind.value = null;
  }
}

async function clear(kind: AppKind): Promise<boolean> {
  busyKind.value = kind;
  try {
    const applied = await attempt(() => appApi.clear(kind), {
      success: "已移除该应用的模型配置",
      error: "移除配置失败",
    });
    if (applied === undefined) return false;
    await refresh();
    return true;
  } finally {
    busyKind.value = null;
  }
}

export function useApps() {
  return {
    apps,
    loading,
    busyKind,
    download,
    lastSteps,
    refresh,
    ensureListener,
    install,
    update,
    apply,
    clear,
  };
}

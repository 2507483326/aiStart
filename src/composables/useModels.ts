import { computed, ref } from "vue";

import { modelApi, systemApi } from "@/lib/ipc";
import { attempt } from "@/lib/notify";
import type {
  FormatInfo,
  ModelConfig,
  ModelFormat,
  ModelInput,
  ModelPreset,
  TestResult,
} from "@/lib/types";

const models = ref<ModelConfig[]>([]);
const formats = ref<FormatInfo[]>([]);
const presets = ref<ModelPreset[]>([]);
const activeModelId = ref<string | null>(null);
const loading = ref(false);
const testingId = ref<string | null>(null);
const lastTest = ref<TestResult | null>(null);

const activeModel = computed(
  () => models.value.find((model) => model.id === activeModelId.value) ?? null,
);

async function refresh(): Promise<void> {
  loading.value = true;
  try {
    const [list, active] = await Promise.all([
      modelApi.list(),
      systemApi.settings(),
    ]);
    models.value = list;
    activeModelId.value = active.activeModelId;
  } finally {
    loading.value = false;
  }
}

async function loadMeta(): Promise<void> {
  const [formatList, presetList] = await Promise.all([
    modelApi.formats(),
    modelApi.presets(),
  ]);
  formats.value = formatList;
  presets.value = presetList;
}

async function save(input: ModelInput): Promise<boolean> {
  const saved = await attempt(() => modelApi.save(input), {
    success: "模型已保存",
    error: "保存模型失败",
  });
  if (!saved) return false;
  await refresh();
  return true;
}

async function remove(id: string): Promise<boolean> {
  const next = await attempt(() => modelApi.remove(id), {
    success: "模型已删除",
    error: "删除模型失败",
  });
  if (!next) return false;
  models.value = next;
  await refresh();
  return true;
}

async function activate(id: string): Promise<boolean> {
  const result = await attempt(() => modelApi.activate(id), {
    error: "启用模型失败",
  });
  if (!result) return false;
  activeModelId.value = id;
  return true;
}

async function test(id: string): Promise<TestResult | undefined> {
  testingId.value = id;
  try {
    const result = await attempt(() => modelApi.test(id), {
      error: "连通性测试失败",
    });
    if (!result) return undefined;
    lastTest.value = result;
    return result;
  } finally {
    testingId.value = null;
  }
}

async function fetchUpstream(
  baseUrl: string,
  apiKey: string,
  format: ModelFormat,
): Promise<string[] | undefined> {
  return attempt(() => modelApi.fetchUpstream(baseUrl, apiKey, format), {
    error: "获取模型列表失败",
  });
}

export function useModels() {
  return {
    models,
    formats,
    presets,
    activeModel,
    activeModelId,
    loading,
    testingId,
    lastTest,
    refresh,
    loadMeta,
    save,
    remove,
    activate,
    test,
    fetchUpstream,
  };
}

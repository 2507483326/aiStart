import { defineStore } from "pinia";

import { modelApi, systemApi } from "@/lib/ipc";
import { attempt } from "@/lib/notify";
import type {
  FormatInfo,
  ModelConfig,
  ModelFormat,
  ModelInput,
  TestResult,
} from "@/lib/types";

export const useModelsStore = defineStore("models", {
  state: () => ({
    models: [] as ModelConfig[],
    formats: [] as FormatInfo[],
    activeModelId: null as number | null,
    loading: false,
    upstreamCache: {} as Record<number, string[]>,
  }),
  getters: {
    activeModel(state): ModelConfig | null {
      return state.models.find((model) => model.id === state.activeModelId) ?? null;
    },
  },
  actions: {
    async refresh(): Promise<void> {
      this.loading = true;
      try {
        const [list, active] = await Promise.all([
          modelApi.list(),
          systemApi.settings(),
        ]);
        this.models = list;
        this.activeModelId = active.activeModelId;
      } finally {
        this.loading = false;
      }
    },
    async loadMeta(): Promise<void> {
      this.formats = await modelApi.formats();
    },
    async save(input: ModelInput): Promise<boolean> {
      const saved = await attempt(() => modelApi.save(input), {
        success: "模型已保存",
        error: "保存模型失败",
      });
      if (!saved) return false;
      await this.refresh();
      return true;
    },
    async duplicate(model: ModelConfig): Promise<boolean> {
      const saved = await attempt(
        () =>
          modelApi.save({
            name: `${model.name} 副本`,
            format: model.format,
            baseUrl: model.baseUrl,
            apiKey: model.apiKey,
            model: model.model,
            supports1m: model.supports1m,
          }),
        { success: "模型已复制", error: "复制模型失败" },
      );
      if (!saved) return false;
      await this.refresh();
      return true;
    },
    async remove(id: number): Promise<boolean> {
      const next = await attempt(() => modelApi.remove(id), {
        success: "模型已删除",
        error: "删除模型失败",
      });
      if (!next) return false;
      this.models = next;
      await this.refresh();
      return true;
    },
    async activate(id: number): Promise<boolean> {
      const result = await attempt(() => modelApi.activate(id), {
        error: "启用模型失败",
      });
      if (!result) return false;
      this.activeModelId = id;
      return true;
    },
    async test(id: number): Promise<TestResult | undefined> {
      return attempt(() => modelApi.test(id), {
        error: "连通性测试失败",
      });
    },
    async testConfig(
      baseUrl: string,
      apiKey: string,
      model: string,
      format: ModelFormat,
    ): Promise<TestResult | undefined> {
      return attempt(() => modelApi.testConfig(baseUrl, apiKey, model, format), {
        error: "连通性测试失败",
      });
    },
    async fetchUpstream(
      baseUrl: string,
      apiKey: string,
      format: ModelFormat,
    ): Promise<string[] | undefined> {
      return attempt(() => modelApi.fetchUpstream(baseUrl, apiKey, format), {
        error: "获取模型列表失败",
      });
    },
    async fetchUpstreamQuiet(
      baseUrl: string,
      apiKey: string,
      format: ModelFormat,
    ): Promise<string[]> {
      try {
        return await modelApi.fetchUpstream(baseUrl, apiKey, format);
      } catch {
        return [];
      }
    },
    async prefetchUpstream(): Promise<void> {
      const results = await Promise.all(
        this.models.map(async (item) => {
          try {
            const list = await modelApi.fetchUpstream(item.baseUrl, item.apiKey, item.format);
            return [item.id, list] as const;
          } catch {
            return [item.id, [] as string[]] as const;
          }
        }),
      );
      const next: Record<number, string[]> = {};
      for (const [id, list] of results) {
        if (list.length) next[id] = list;
      }
      this.upstreamCache = next;
    },
  },
});

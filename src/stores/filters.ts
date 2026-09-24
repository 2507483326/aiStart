import { defineStore } from "pinia";

import { filterApi } from "@/lib/ipc";
import { attempt } from "@/lib/notify";
import type { FilterInput, RequestFilter } from "@/lib/types";

export const useFiltersStore = defineStore("filters", {
  state: () => ({
    filters: [] as RequestFilter[],
    loading: false,
  }),
  actions: {
    async refresh(): Promise<void> {
      this.loading = true;
      try {
        this.filters = await filterApi.list();
      } finally {
        this.loading = false;
      }
    },
    async save(input: FilterInput): Promise<boolean> {
      const saved = await attempt(() => filterApi.save(input), {
        success: "提示词注入已保存",
        error: "保存提示词注入失败",
      });
      if (!saved) return false;
      await this.refresh();
      return true;
    },
    async setEnabled(id: number, enabled: boolean): Promise<boolean> {
      const updated = await attempt(() => filterApi.setEnabled(id, enabled), {
        error: "切换提示词注入状态失败",
      });
      if (!updated) return false;
      const index = this.filters.findIndex((filter) => filter.id === id);
      if (index >= 0) this.filters[index] = updated;
      return true;
    },
    async remove(id: number): Promise<boolean> {
      const next = await attempt(() => filterApi.remove(id), {
        success: "提示词注入已删除",
        error: "删除提示词注入失败",
      });
      if (!next) return false;
      this.filters = next;
      return true;
    },
  },
});

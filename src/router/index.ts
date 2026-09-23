import { createRouter, createWebHashHistory } from "vue-router";

import AppsPage from "@/pages/AppsPage.vue";
import DashboardPage from "@/pages/DashboardPage.vue";
import ModelsPage from "@/pages/ModelsPage.vue";
import StatsPage from "@/pages/StatsPage.vue";

export const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: "/", redirect: "/dashboard" },
    {
      path: "/dashboard",
      name: "dashboard",
      component: DashboardPage,
      meta: { title: "面板", subtitle: "本地网关运行状态、调用统计与自动切换" },
    },
    {
      path: "/apps",
      name: "apps",
      component: AppsPage,
      meta: { title: "应用", subtitle: "一键安装、更新并把模型接入桌面客户端" },
    },
    {
      path: "/models",
      name: "models",
      component: ModelsPage,
      meta: { title: "模型", subtitle: "统一管理三种协议的上游模型" },
    },
    {
      path: "/stats",
      name: "stats",
      component: StatsPage,
      meta: { title: "统计", subtitle: "Token 消耗贡献图与每一次请求的明细" },
    },
  ],
});

import { createRouter, createWebHashHistory } from "vue-router";

import AppsPage from "@/pages/AppsPage.vue";
import DashboardPage from "@/pages/DashboardPage.vue";
import FiltersPage from "@/pages/FiltersPage.vue";
import ModelsPage from "@/pages/ModelsPage.vue";
import RequestDetailPage from "@/pages/RequestDetailPage.vue";
import RequestsPage from "@/pages/RequestsPage.vue";
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
      meta: { title: "应用", subtitle: "一键把模型接入桌面客户端" },
    },
    {
      path: "/models",
      name: "models",
      component: ModelsPage,
      meta: { title: "模型", subtitle: "统一管理上游模型" },
    },
    {
      path: "/filters",
      name: "filters",
      component: FiltersPage,
      meta: { title: "提示词注入", subtitle: "请求转发给上游前注入系统提示词" },
    },
    {
      path: "/stats",
      name: "stats",
      component: StatsPage,
      meta: { title: "统计", subtitle: "Token 消耗贡献图与每一次请求的明细" },
    },
    {
      path: "/stats/requests",
      name: "requests",
      component: RequestsPage,
      meta: { title: "请求明细", subtitle: "全部网关调用记录" },
    },
    {
      path: "/stats/requests/:id",
      name: "request-detail",
      component: RequestDetailPage,
      meta: { title: "请求详情", subtitle: "一次网关调用的完整报文" },
    },
  ],
});

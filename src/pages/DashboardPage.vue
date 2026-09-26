<script setup lang="ts">
import { computed, onMounted } from "vue";
import { useRouter } from "vue-router";
import { ArrowRight, BrainCircuit } from "lucide";

import EmptyState from "@/components/common/EmptyState.vue";
import MorphIconBox from "@/components/common/MorphIconBox.vue";
import GatewayEventsCard from "@/components/events/GatewayEventsCard.vue";
import GatewayPanel from "@/components/models/GatewayPanel.vue";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Separator } from "@/components/ui/separator";
import { useGateway } from "@/composables/useGateway";
import { useModels } from "@/composables/useModels";

const router = useRouter();
const gateway = useGateway();
const { models, activeModel, refresh: refreshModels } = useModels();

// 面板只做概览：模型多了也不铺开，最多列 5 个，其余到「模型」页看。
const PREVIEW_LIMIT = 5;
const previewModels = computed(() => models.value.slice(0, PREVIEW_LIMIT));

onMounted(async () => {
  await Promise.all([gateway.refresh(), refreshModels()]);
});
</script>

<template>
  <div class="mx-auto max-w-6xl space-y-5">
    <GatewayPanel />

    <div class="grid gap-4 lg:grid-cols-2">
      <GatewayEventsCard />

      <Card class="gap-4">
        <CardHeader>
          <div class="flex items-center justify-between">
            <div class="space-y-1">
              <CardTitle>模型概览</CardTitle>
              <CardDescription class="text-xs">
                共 {{ models.length }} 个上游模型，网关当前接管
                {{ activeModel?.name ?? "未设置" }}
              </CardDescription>
            </div>
            <Button variant="ghost" size="sm" class="gap-1.5" @click="router.push('/models')">
              管理
              <MorphIconBox :icon="ArrowRight" :size="14" />
            </Button>
          </div>
        </CardHeader>
        <CardContent>
          <div v-if="models.length" class="space-y-2.5">
            <template v-for="(model, index) in previewModels" :key="model.id">
              <Separator v-if="index > 0" />
              <div class="-mx-2 flex items-center justify-between gap-3 rounded-md px-2 py-1.5 transition-colors duration-150 hover:bg-accent/50">
                <div class="flex min-w-0 items-center gap-2.5">
                  <MorphIconBox
                    :icon="BrainCircuit"
                    :size="16"
                    :class="
                      model.id === activeModel?.id
                        ? 'text-emerald-500'
                        : 'text-muted-foreground'
                    "
                  />
                  <div class="min-w-0 leading-tight">
                    <p class="truncate text-sm font-medium">{{ model.name }}</p>
                    <p class="truncate font-mono text-xs text-muted-foreground">
                      {{ model.model }}
                    </p>
                  </div>
                </div>
                <div class="flex shrink-0 items-center gap-1.5">
                  <Badge v-if="model.supports1m" variant="outline">1M</Badge>
                  <Badge v-if="model.id === activeModel?.id" variant="secondary">
                    启用中
                  </Badge>
                </div>
              </div>
            </template>
          </div>
          <EmptyState
            v-else
            :icon="BrainCircuit"
            title="还没有模型"
            description="添加模型后即可启用网关并把推理能力接入桌面客户端。"
          />
        </CardContent>
      </Card>
    </div>
  </div>
</template>

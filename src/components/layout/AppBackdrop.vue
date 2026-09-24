<script setup lang="ts">
import { ref } from "vue";

import { useBackdrop } from "@/composables/useBackdrop";

const canvasRef = ref<HTMLCanvasElement | null>(null);
useBackdrop(canvasRef);
</script>

<!--
  统一背景板：铺满父级（内容列）的纯装饰画布。父级须 `relative`，本元素以
  absolute 打底，DOM 序在页头/主体之前 → 绘在最底层。zero 交互（
  pointer-events-none）+ aria-hidden：不吃点击、不进无障碍树。
-->
<template>
  <canvas
    ref="canvasRef"
    aria-hidden="true"
    class="pointer-events-none absolute inset-0 h-full w-full"
  />
</template>

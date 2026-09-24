/**
 * 背景板的 canvas 接线（移植自 eTeam 的 eteamsBackdrop，改成 Vue 组合式）：
 * 只做「取 2d 上下文 / 尺寸测量 / DPR / rAF / 可见性 / 降运动 / 主题采样 /
 * 鼠标热场」的薄接线，全部视觉算法在 `@/lib/backdrop/engine` 的纯函数里。
 *
 * 与 eTeam 的关键差异：aiStart 的壳是固定高（根 h-screen、内容列 flex-1、
 * 只有 <main> 内部滚动），背景直接绝对定位铺满内容列即可——**不需要 eTeam
 * 那套 0 高 sticky 条 + 滚动宿主可视高取 min 的「钉住」逻辑**，背景天然不随
 * 内容滚动漂移。
 */
import { onBeforeUnmount, onMounted, type Ref } from "vue";

import {
  BACKDROP_SEED,
  createHeightField,
  decayHeat,
  planHeatOps,
  planStaticLayer,
  sampleBackdropPalette,
  splatHeat,
  type BackdropPalette,
  type HeatField,
} from "@/lib/backdrop/engine";

/** 网格尺寸：内容列宽 <600 → 12，否则 14。 */
function cellFor(w: number): number {
  return w > 0 && w < 600 ? 12 : 14;
}
/** 动层目标帧率（30fps 足够「缓慢退热」观感，省电）。 */
const FRAME_MIN_MS = 1000 / 30;
/** DPR 封顶（超出无收益且更耗）。 */
const DPR_CAP = 2;
/** 主题防抖：调色板变化后 150ms 尾随合并再重绘静层。 */
const THEME_DEBOUNCE_MS = 150;
/** 看门狗周期 / 静止判定阈值：可见态 lastRender 超 2500ms 即自愈。 */
const WATCHDOG_INTERVAL_MS = 1000;
const WATCHDOG_STALL_MS = 2500;
/** 鼠标微光节流：距上次落点 ≥90ms 且位移 ≥24px 才 splat 一粒。 */
const HEAT_MIN_INTERVAL_MS = 90;
const HEAT_MIN_DIST_PX = 24;

/** performance.now 口径的时刻。 */
function nowMs(): number {
  return typeof performance !== "undefined" ? performance.now() : 0;
}

/**
 * 把背景板挂到给定 canvas 上，作用域元素（内容列）提供尺寸与事件面。
 * 组件卸载时自动清理全部监听/观察器/计时器。
 */
export function useBackdrop(canvasRef: Ref<HTMLCanvasElement | null>): void {
  let dispose: (() => void) | null = null;

  onMounted(() => {
    const canvas = canvasRef.value;
    // 作用域元素 = canvas 的父节点（内容列）：提供尺寸，也是 pointermove 事件面
    // （canvas 自身 pointer-events-none 不吃交互，事件天然落到内容列上）。
    const scopeEl = canvas?.parentElement ?? null;
    if (canvas === null || scopeEl === null) return;
    const ctx = canvas.getContext("2d");
    if (ctx === null) return;

    let disposed = false;
    let raf = 0;
    let w = 0;
    let h = 0;
    let dpr = 1;
    let cell = cellFor(0);
    let palette: BackdropPalette = sampleBackdropPalette(() => null);
    let offscreen: HTMLCanvasElement | null = null;
    let heat: HeatField = new Map();
    let lastT = 0;
    let lastRender = 0;
    let hidden = typeof document !== "undefined" && document.hidden;

    const reducedMq =
      typeof matchMedia === "function" ? matchMedia("(prefers-reduced-motion: reduce)") : null;
    let reduced = reducedMq?.matches ?? false;

    /** 从作用域根采样宿主变量，缺省落引擎兜底（slate-600 / sky-500）。 */
    const readVar = (name: string): string | null => {
      const v = getComputedStyle(scopeEl).getPropertyValue(name);
      return v.trim() === "" ? null : v;
    };

    /** 重绘静层到离屏 canvas（仅 resize / 换主题时调用）。 */
    const renderStatic = (): void => {
      if (w <= 0 || h <= 0) return;
      const heights = createHeightField(Math.ceil(w / cell), Math.ceil(h / cell), BACKDROP_SEED);
      const staticOps = planStaticLayer(w, h, cell, palette, heights);
      if (offscreen === null) offscreen = document.createElement("canvas");
      offscreen.width = Math.max(1, Math.round(w * dpr));
      offscreen.height = Math.max(1, Math.round(h * dpr));
      const octx = offscreen.getContext("2d");
      if (octx === null) return;
      octx.setTransform(dpr, 0, 0, dpr, 0, 0);
      octx.clearRect(0, 0, w, h);
      for (const op of staticOps) {
        octx.fillStyle = op.color;
        octx.fillRect(op.x, op.y, op.w, op.h);
      }
    };

    /** 画一帧：离屏静层 + 热场微光叠加（30fps 限流）。 */
    const drawFrame = (force: boolean): void => {
      const now = nowMs();
      if (!force && now - lastRender < FRAME_MIN_MS) return;
      lastRender = now;
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      if (offscreen !== null) ctx.drawImage(offscreen, 0, 0);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      for (const op of planHeatOps(heat, cell, w, h, palette)) {
        ctx.fillStyle = op.color;
        ctx.fillRect(op.x, op.y, op.w, op.h);
      }
    };

    /** 全量重规划：调色板 → 静层 → 清空热场 → 推一帧。 */
    const replan = (): void => {
      palette = sampleBackdropPalette(readVar);
      renderStatic();
      heat = new Map();
      drawFrame(true);
    };

    const loop = (t: number): void => {
      if (disposed) return;
      if (hidden) return; // 页面隐藏：不推进也不重绘、不累积 rAF
      raf = requestAnimationFrame(loop);
      const dt = lastT === 0 ? 0 : Math.min((t - lastT) / 1000, 0.1);
      lastT = t;
      heat = decayHeat(heat, dt);
      // 热场熄灭 → 停帧省电（按需循环）
      if (heat.size === 0) {
        cancelAnimationFrame(raf);
        raf = 0;
        drawFrame(false);
        return;
      }
      drawFrame(false);
    };

    /** 依据 reduced/hidden 状态启停主循环。 */
    const syncLoop = (): void => {
      cancelAnimationFrame(raf);
      lastT = 0;
      if (disposed || hidden) return;
      if (reduced) {
        drawFrame(true); // 降运动：单帧静图
        return;
      }
      raf = requestAnimationFrame(loop);
    };

    // 尺寸：位图按内容列实测规划，尺寸/DPR/cell 未变则短路（避免无谓重排）。
    const measure = (): void => {
      const nextW = scopeEl.clientWidth;
      const nextH = scopeEl.clientHeight;
      if (nextW <= 0 || nextH <= 0) return;
      const nextDpr = Math.min(window.devicePixelRatio || 1, DPR_CAP);
      const nextCell = cellFor(nextW);
      if (
        Math.abs(nextW - w) < 0.5 &&
        Math.abs(nextH - h) < 0.5 &&
        nextDpr === dpr &&
        nextCell === cell
      ) {
        return;
      }
      w = nextW;
      h = nextH;
      dpr = nextDpr;
      cell = nextCell;
      canvas.width = Math.max(1, Math.round(w * dpr));
      canvas.height = Math.max(1, Math.round(h * dpr));
      replan();
    };
    const ro = new ResizeObserver(() => measure());
    measure(); // 同步首测（挂载帧内即定尺寸，RO 首回调幂等短路）
    ro.observe(scopeEl);

    // 主题：palette 相等即短路；真变化 150ms 尾随防抖合并后重绘静层。
    let themeTimer: number | null = null;
    const mo = new MutationObserver(() => {
      const next = sampleBackdropPalette(readVar);
      if (next.label === palette.label && next.brand === palette.brand) return;
      palette = next;
      if (themeTimer !== null) clearTimeout(themeTimer);
      themeTimer = window.setTimeout(() => {
        themeTimer = null;
        if (disposed) return;
        renderStatic();
        drawFrame(true);
      }, THEME_DEBOUNCE_MS);
    });
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["class", "style"] });

    // 降运动：媒体查询即时响应（用户改系统设置不重载也生效）。
    const onReduced = (): void => {
      reduced = reducedMq?.matches ?? false;
      syncLoop();
    };
    reducedMq?.addEventListener("change", onReduced);

    const onVisibility = (): void => {
      hidden = document.hidden;
      syncLoop();
    };
    document.addEventListener("visibilitychange", onVisibility);

    // 鼠标格子微光：监听挂作用域列（canvas 自身 pointer-events-none 不吃交互）。
    // 落点即按需启动 rAF 循环；reduced / 页面隐藏时不生成（静板）。
    let lastHeatAt = 0;
    let lastHeatX = -1e9;
    let lastHeatY = -1e9;
    const onPointerMove = (event: PointerEvent): void => {
      if (disposed || hidden || reduced) return;
      if (event.pointerType === "touch") return;
      const now = nowMs();
      const rect = canvas.getBoundingClientRect();
      const x = event.clientX - rect.left;
      const y = event.clientY - rect.top;
      const dist = Math.hypot(x - lastHeatX, y - lastHeatY);
      if (now - lastHeatAt < HEAT_MIN_INTERVAL_MS || dist < HEAT_MIN_DIST_PX) return;
      lastHeatAt = now;
      lastHeatX = x;
      lastHeatY = y;
      heat = splatHeat(heat, x, y, cell, w, h);
      if (raf === 0 && !disposed) {
        lastT = 0;
        raf = requestAnimationFrame(loop);
      }
    };
    scopeEl.addEventListener("pointermove", onPointerMove);

    // 看门狗：可见且非空闲却长时间未推进，先辨上下文丢失（丢则全量 replan），
    // 否则强制推帧 / 重启循环。空闲（无热场、静层已画）是设计态，不算 stall。
    const watchdog = window.setInterval(() => {
      if (disposed || hidden || reduced) return;
      if (heat.size === 0 && offscreen !== null) return;
      if (nowMs() - lastRender <= WATCHDOG_STALL_MS) return;
      if (ctx.isContextLost()) {
        replan();
        return;
      }
      lastT = 0;
      if (raf === 0) raf = requestAnimationFrame(loop);
      else drawFrame(true);
    }, WATCHDOG_INTERVAL_MS);

    syncLoop();

    dispose = () => {
      disposed = true;
      cancelAnimationFrame(raf);
      clearInterval(watchdog);
      if (themeTimer !== null) clearTimeout(themeTimer);
      ro.disconnect();
      mo.disconnect();
      reducedMq?.removeEventListener("change", onReduced);
      document.removeEventListener("visibilitychange", onVisibility);
      scopeEl.removeEventListener("pointermove", onPointerMove);
    };
  });

  onBeforeUnmount(() => {
    dispose?.();
    dispose = null;
  });
}

/**
 * 背景板引擎：内容区「右上角网格 + 高地图 → 左下角渐隐」以及「鼠标滑动时的
 * 格子微光」的全部**纯计算**（移植自 eTeam 的 backdropEngine，按 aiStart 取舍
 * 裁剪——不搬像素坦克，只保留 网格 + 高地图 + 鼠标微光）。
 *
 * 为什么独立成纯模块：canvas / rAF 都不可单测——本模块零 DOM 依赖、零副作用、
 * 全部确定性（显式 seed），把高度场、渐隐、调色板采样、静层绘制指令、鼠标热场
 * 做成可单测的纯函数；`AppBackdrop.vue` 只做 canvas/DPR/rAF/可见性/降运动的薄接线。
 */

/** 矩形填充指令（坐标逻辑 px；color 已带 alpha）。 */
export interface BackdropRect {
  x: number;
  y: number;
  w: number;
  h: number;
  color: string;
}

/* —— 渐隐（让「有东西」严格压在内容区右上角）—— */

/** 渐隐起点：距右上角归一化距离 d ≤ 0.10 全实。 */
export const FADE_NEAR = 0.1;
/** 渐隐终点：d ≥ 0.40 完全透明（可见区 ≈ FAR² = 16%，把纹理压回右上角）。 */
export const FADE_FAR = 0.4;

/** clamp 到 [0,1]。 */
export function clamp01(v: number): number {
  return v < 0 ? 0 : v > 1 ? 1 : v;
}

/** 平滑阶梯（Hermite）：edge0→edge1 之间 0→1 平滑过渡，版面无硬边。 */
export function smoothstep(edge0: number, edge1: number, v: number): number {
  const t = clamp01((v - edge0) / (edge1 - edge0));
  return t * t * (3 - 2 * t);
}

/**
 * 右上→左下渐隐系数：`d = ((W−x) + y) / (W+H)`——d=0 在右上角、1 在左下角；
 * alpha = 1 − smoothstep(FADE_NEAR, FADE_FAR, d)。网格/高地图/微光统一乘该
 * 系数，保证整张背景板只有右上角「有东西」。
 */
export function fadeAlpha(x: number, y: number, w: number, h: number): number {
  const d = (w - x + y) / (w + h);
  return 1 - smoothstep(FADE_NEAR, FADE_FAR, d);
}

/* —— 确定性随机 —— */

/**
 * mulberry32 PRNG：32bit 种子 → [0,1) 确定性序列。引擎全部随机性都出自显式
 * seed 的本函数，重放即复现。
 */
export function mulberry32(seed: number): () => number {
  let s = seed | 0;
  return () => {
    s = (s + 0x6d2b79f5) | 0;
    let t = Math.imul(s ^ (s >>> 15), 1 | s);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/* —— 高度场（高地图）—— */

/**
 * 确定性值噪声高度场：格点格值（mulberry32）+ 双线性插值 + 两个倍频程
 * （基频 + 半幅两倍频），输出 [0,1] 的平滑高度，格点对齐绘制网格。
 *
 * 返回长度 cols*rows 的行主序数组；(cols,rows) 是**格点数**（含边界），
 * 绘制时每格 (cx,cy) 取其左上格点值。
 */
export function createHeightField(cols: number, rows: number, seed: number): Float32Array {
  const out = new Float32Array(cols * rows);
  const rand = mulberry32(seed);
  // 基频晶格（stride 4）+ 细节晶格（stride 2，半幅）——直接在输出分辨率上
  // 以「每 stride 一个随机格点、其余双线性」实现，省一次中间数组。
  const base = new Float32Array(cols * rows);
  const detail = new Float32Array(cols * rows);
  for (let cy = 0; cy < rows; cy++) {
    for (let cx = 0; cx < cols; cx++) {
      base[cy * cols + cx] = cy % 4 === 0 && cx % 4 === 0 ? rand() : 0;
      detail[cy * cols + cx] = cy % 2 === 0 && cx % 2 === 0 ? rand() : 0;
    }
  }
  const sample = (lattice: Float32Array, stride: number, cx: number, cy: number): number => {
    const x0 = Math.floor(cx / stride) * stride;
    const y0 = Math.floor(cy / stride) * stride;
    const x1 = Math.min(x0 + stride, cols - 1);
    const y1 = Math.min(y0 + stride, rows - 1);
    const tx = stride === 0 ? 0 : (cx - x0) / stride;
    const ty = stride === 0 ? 0 : (cy - y0) / stride;
    const v00 = lattice[y0 * cols + x0] ?? 0;
    const v01 = lattice[y0 * cols + x1] ?? 0;
    const v10 = lattice[y1 * cols + x0] ?? 0;
    const v11 = lattice[y1 * cols + x1] ?? 0;
    const top = v00 * (1 - tx) + v01 * tx;
    const bottom = v10 * (1 - tx) + v11 * tx;
    return top * (1 - ty) + bottom * ty;
  };
  for (let cy = 0; cy < rows; cy++) {
    for (let cx = 0; cx < cols; cx++) {
      const b = sample(base, 4, cx, cy);
      const d = sample(detail, 2, cx, cy);
      out[cy * cols + cx] = clamp01(b * 0.7 + d * 0.3);
    }
  }
  return out;
}

/* —— 调色板 —— */

/**
 * 背景板绘制基底色（**实色，不带 alpha**——alpha 一律在使用位按元素语义单次
 * 烘焙为最终有效值）。
 */
export interface BackdropPalette {
  /** 中性基底（实色）：网格线 / 高地图低两档（亮暗自适应）。 */
  label: string;
  /** 品牌点缀基底（实色）：高地图峰顶 / 鼠标微光（亮暗自适应）。 */
  brand: string;
  /** 画布级合成透明度（恒 1.0——有效 alpha 已在指令级单次烘焙）。 */
  compositeAlpha: number;
}

/** 画布级合成透明度（1.0：有效 alpha 已在指令级烘焙，不再全局折半）。 */
export const COMPOSITE_ALPHA_CAP = 1;

/* 水印化定标（= 指令烘焙出的最终屏上值；均远低于正文对比度）：
 * 网格 0.05、高地图 3 档量化带 0.03 / 0.06 / 峰顶 accent 0.10×peak
 * （量化是行/列合并的前提——相邻格同带率大增）。 */
export const GRID_LINE_ALPHA = 0.05;
/** 高地图低档量化带（height < 0.5）。 */
export const TERRAIN_LOW_ALPHA = 0.03;
/** 高地图中档量化带（0.5 ≤ height < 0.8）。 */
export const TERRAIN_MID_ALPHA = 0.06;
/** 高地图峰顶带系数（height ≥ 0.8：α = 0.10 × peak 内插，peak∈[0,1]）。 */
export const TERRAIN_PEAK_ALPHA = 0.1;
/** 高地图中档阈值（height ≥ 此值进中档带）。 */
export const TERRAIN_MID_THRESHOLD = 0.5;

/**
 * 给完整色值叠 alpha：`#rrggbb` → `rgba(r,g,b,a)`；`rgba(r,g,b,a)` → alpha
 * 相乘（取更透明者）；其余原样返回（不猜格式）。**因此 token 必须是 hex 或
 * rgb/rgba**，写 oklch 会走「原样返回」分支、丢掉透明度。
 */
export function withAlpha(color: string, alpha: number): string {
  const a = clamp01(alpha);
  const hex = /^#([0-9a-fA-F]{6})$/.exec(color);
  if (hex !== null) {
    const n = Number.parseInt(hex[1] ?? '000000', 16);
    const r = (n >> 16) & 0xff;
    const g = (n >> 8) & 0xff;
    const b = n & 0xff;
    return `rgba(${r},${g},${b},${round3(a)})`;
  }
  const rgba = /^rgba?\(([^)]+)\)$/i.exec(color);
  if (rgba !== null) {
    const parts = (rgba[1] ?? '').split(',').map((p) => Number.parseFloat(p.trim()));
    if (parts.length >= 3 && parts.every((p) => Number.isFinite(p))) {
      const r = parts[0] ?? 0;
      const g = parts[1] ?? 0;
      const b = parts[2] ?? 0;
      const base = parts.length >= 4 ? (parts[3] ?? 1) : 1;
      return `rgba(${r},${g},${b},${round3(a * base)})`;
    }
  }
  return color;
}

/** 三位小数舍入（rgba 字符串稳定输出）。 */
function round3(v: number): number {
  return Math.round(v * 1000) / 1000;
}

/** 从宿主变量名读色的采样键（亮暗两块同名定义，值随主题切换）。 */
export interface PaletteVarNames {
  /** 主文字色（网格/地形基底，亮暗自适应）。 */
  readonly label: string;
  /** 品牌点缀色（峰顶 / 微光，亮暗自适应）。 */
  readonly brand: string;
}

export const DEFAULT_PALETTE_VARS: PaletteVarNames = {
  label: '--backdrop-label',
  brand: '--backdrop-accent',
};

/**
 * 采样调色板：`read(name)` 由组件提供（getComputedStyle 包一层），返回
 * null/undefined/空串即用字面兜底（slate-600 / sky-500）。本函数纯：同一组
 * 输入色产出同一调色板。基底一律实色（alpha 在使用位单次烘焙）。
 */
export function sampleBackdropPalette(read: (name: string) => string | null): BackdropPalette {
  const label = read(DEFAULT_PALETTE_VARS.label)?.trim() || '#475569';
  const brand = read(DEFAULT_PALETTE_VARS.brand)?.trim() || '#0ea5e9';
  return { label, brand, compositeAlpha: COMPOSITE_ALPHA_CAP };
}

/* —— 静层绘制指令（网格 + 高地图，合并版）—— */

/** 高地图峰顶阈值：height ≥ 0.8 进 accent 峰顶带（0.10 × peak 内插）。 */
const PEAK_THRESHOLD = 0.8;

/** 指令合并参数：相邻段 α 差 ≤ 此值合并为一条 rect；有效 α < MIN_OP_ALPHA 跳过。 */
const MERGE_ALPHA_TOL = 0.015;
const MIN_OP_ALPHA = 0.01;

/** 高地图格子着色：返回 [基底色, 原始 α 系数]（×fade 后即最终有效值）。 */
function terrainShade(height: number, palette: BackdropPalette): [string, number] {
  if (height >= PEAK_THRESHOLD) {
    const peak = (height - PEAK_THRESHOLD) / (1 - PEAK_THRESHOLD);
    return [palette.brand, TERRAIN_PEAK_ALPHA * peak];
  }
  if (height >= TERRAIN_MID_THRESHOLD) return [palette.label, TERRAIN_MID_ALPHA];
  return [palette.label, TERRAIN_LOW_ALPHA];
}

/** 合并中的段：基底色 + α 累加（均值 = Σα/格数，墨量 Σα·面积 守恒）。 */
interface RunAcc {
  base: string;
  x: number;
  y: number;
  w: number;
  h: number;
  sum: number;
  count: number;
  last: number;
}

/**
 * 规划静层（网格线 + 高地图着色），按「地形先、网格后」排序。纯函数。
 *
 * 指令合并（性能护栏：逐格画在 1920×1080/c24 是 ~1 万条 → 预算 ≤400）：
 * - 地形：同**行**内相邻格（同基底、α 差 ≤0.015）合并为一条 rect，再做一次
 *   同列纵向合并（量化带使相邻格同 α 率大增）；
 * - 网格：同**列/行**的连续段（α 差 ≤0.015）合并为一条 rect；
 * - 有效 α < 0.01 的指令直接跳过（渐隐尽头的格子零指令）。
 * 坐标仍逻辑 px，颜色已烘焙 alpha。
 */
export function planStaticLayer(
  w: number,
  h: number,
  cell: number,
  palette: BackdropPalette,
  heights: Float32Array,
): BackdropRect[] {
  const ops: BackdropRect[] = [];
  const cols = Math.max(1, Math.ceil(w / cell));
  const rows = Math.max(1, Math.ceil(h / cell));
  const flush = (run: RunAcc | null): void => {
    if (run === null) return;
    const a = run.sum / run.count;
    if (a < MIN_OP_ALPHA) return;
    ops.push({ x: run.x, y: run.y, w: run.w, h: run.h, color: withAlpha(run.base, a) });
  };

  // —— 地形：按行合并，再按列纵向合并 ——
  const terrain: RunAcc[] = [];
  const flushT = (run: RunAcc | null): void => {
    if (run === null) return;
    const a = run.sum / run.count;
    if (a < MIN_OP_ALPHA) return;
    terrain.push(run);
  };
  for (let cy = 0; cy < rows; cy++) {
    const y = cy * cell;
    const ch = Math.min(cell, h - y);
    let run: RunAcc | null = null;
    for (let cx = 0; cx < cols; cx++) {
      const height = heights[cy * cols + cx] ?? 0;
      const x = cx * cell;
      const cw = Math.min(cell, w - x);
      const fade = fadeAlpha(x + cw / 2, y + ch / 2, w, h);
      const [base, coef] = terrainShade(height, palette);
      const a = coef * fade;
      if (a < MIN_OP_ALPHA) {
        flushT(run);
        run = null;
        continue;
      }
      if (run !== null && run.base === base && Math.abs(a - run.last) <= MERGE_ALPHA_TOL) {
        run.w += cw;
        run.sum += a;
        run.count += 1;
        run.last = a;
      } else {
        flushT(run);
        run = { base, x, y, w: cw, h: ch, sum: a, count: 1, last: a };
      }
    }
    flushT(run);
  }
  // 纵向合并：同列同宽同基底、纵向相邻、α 差 ≤ 容差 → 面积加权均值。
  const byCol = new Map<string, RunAcc[]>();
  for (const r of terrain) {
    const key = `${r.x}|${r.w}|${r.base}`;
    const list = byCol.get(key);
    if (list === undefined) byCol.set(key, [r]);
    else list.push(r);
  }
  for (const list of byCol.values()) {
    list.sort((a, b) => a.y - b.y);
    let run: RunAcc | null = null;
    for (const seg of list) {
      const a = seg.sum / seg.count;
      if (run !== null && run.y + run.h === seg.y && Math.abs(a - run.last) <= MERGE_ALPHA_TOL) {
        run.h += seg.h;
        run.sum += seg.sum;
        run.count += seg.count;
        run.last = a;
      } else {
        flush(run);
        run = { ...seg };
      }
    }
    flush(run);
  }

  // —— 网格线：竖线按列合并、横线按行合并（每条格线通常 1 条指令）——
  for (let cx = 0; cx <= cols; cx++) {
    const x = Math.min(cx * cell, w - 1);
    let run: RunAcc | null = null;
    for (let cy = 0; cy < rows; cy++) {
      const y = cy * cell;
      const segH = Math.min(cell, h - y);
      const a = GRID_LINE_ALPHA * fadeAlpha(x, y + cell / 2, w, h);
      if (a < MIN_OP_ALPHA) {
        flush(run);
        run = null;
        continue;
      }
      if (run !== null && run.y + run.h === y && Math.abs(a - run.last) <= MERGE_ALPHA_TOL) {
        run.h += segH;
        run.sum += a;
        run.count += 1;
        run.last = a;
      } else {
        flush(run);
        run = { base: palette.label, x, y, w: 1, h: segH, sum: a, count: 1, last: a };
      }
    }
    flush(run);
  }
  for (let cy = 0; cy <= rows; cy++) {
    const y = Math.min(cy * cell, h - 1);
    let run: RunAcc | null = null;
    for (let cx = 0; cx < cols; cx++) {
      const x = cx * cell;
      const segW = Math.min(cell, w - x);
      const a = GRID_LINE_ALPHA * fadeAlpha(x + cell / 2, y, w, h);
      if (a < MIN_OP_ALPHA) {
        flush(run);
        run = null;
        continue;
      }
      if (run !== null && run.x + run.w === x && Math.abs(a - run.last) <= MERGE_ALPHA_TOL) {
        run.w += segW;
        run.sum += a;
        run.count += 1;
        run.last = a;
      } else {
        flush(run);
        run = { base: palette.label, x, y, w: segW, h: 1, sum: a, count: 1, last: a };
      }
    }
    flush(run);
  }
  return ops;
}

/* —— 鼠标格子微光 —— */

/**
 * 指针扫过时不创建任何新图形——让**格子本身**微亮，随后慢慢退回原色。三件套
 * （全部纯函数）：
 * - `splatHeat`：落点写入热场（指针所在格 + 邻格随距离衰减）；
 * - `decayHeat`：每帧全场指数退热（e 指数，与帧率无关）；
 * - `planHeatOps`：热场 → 叠加填充指令（只输出有热的格子）。
 *
 * 热场是 `Map<cellKey, heat>`，只持有被扫过的格子（其余部分零状态），每帧从
 * Map 清零项删除——长页面扫一圈也不会积累状态。
 */

/** 单格微光峰值 α（heat=1 时叠加的 alpha；远低于坦克级，水印之上）。 */
export const HEAT_PEAK_ALPHA = 0.14;
/** 邻格衰减系数：距落点 d 格的热量 = falloff^d（d=0 → 1，d=1 → 1/2，…）。 */
export const HEAT_NEIGHBOR_FALLOFF = 0.5;
/** 单帧退热时间常数（秒）：heat *= exp(-dt / TAU)——0.35s 后剩 ~e^-1。 */
export const HEAT_DECAY_TAU = 0.35;
/** 热量低于此值视为熄灭（Map 删除 + 指令跳过）。 */
export const HEAT_OFF_THRESHOLD = 0.012;

/** 热场：cellKey「cx,cy」→ heat ∈ (0,1]。组件持有并跨帧传递，引擎只做纯变换。 */
export type HeatField = Map<string, number>;

/** 格坐标 → 热场键。 */
export function heatKey(cx: number, cy: number): string {
  return `${cx},${cy}`;
}

/**
 * 把一次指针落点写入热场（**纯**——返回新 Map，不改输入）。
 * 指针所在格 (cx,cy) 热量 = 1，正交邻格按 falloff^d 递减；已有热量取 max
 * （连续扫过只增不跳变）。出界格子不写入。
 */
export function splatHeat(
  field: HeatField,
  x: number,
  y: number,
  cell: number,
  w: number,
  h: number,
): HeatField {
  const cols = Math.max(1, Math.ceil(w / cell));
  const rows = Math.max(1, Math.ceil(h / cell));
  const cx = Math.floor(x / cell);
  const cy = Math.floor(y / cell);
  if (cx < 0 || cy < 0 || cx >= cols || cy >= rows) return field;
  const next = new Map(field);
  const spots: readonly (readonly [number, number, number])[] = [
    [cx, cy, 1],
    [cx - 1, cy, HEAT_NEIGHBOR_FALLOFF],
    [cx + 1, cy, HEAT_NEIGHBOR_FALLOFF],
    [cx, cy - 1, HEAT_NEIGHBOR_FALLOFF],
    [cx, cy + 1, HEAT_NEIGHBOR_FALLOFF],
  ];
  for (const [sx, sy, heat] of spots) {
    if (sx < 0 || sy < 0 || sx >= cols || sy >= rows) continue;
    const key = heatKey(sx, sy);
    next.set(key, Math.max(next.get(key) ?? 0, heat));
  }
  return next;
}

/**
 * 全场退热一帧（**纯**）：heat = heat × e^(−dt/TAU)，低于熄灭阈值的格子从
 * Map 删除。dt ≤ 0 时原样返回。
 */
export function decayHeat(field: HeatField, dt: number): HeatField {
  if (dt <= 0) return field;
  const factor = Math.exp(-dt / HEAT_DECAY_TAU);
  const next = new Map<string, number>();
  for (const [key, heat] of field) {
    const decayed = heat * factor;
    if (decayed >= HEAT_OFF_THRESHOLD) next.set(key, decayed);
  }
  return next;
}

/**
 * 热场 → 叠加填充指令（**纯**）：每个有热的格子一条 rect，色 = palette.brand
 * （与峰顶 accent 同源），α = HEAT_PEAK_ALPHA × heat × fadeAlpha(格心)——
 * 与静层同一右上渐隐源，左下扫过自然无痕迹。
 */
export function planHeatOps(
  field: HeatField,
  cell: number,
  w: number,
  h: number,
  palette: BackdropPalette,
): BackdropRect[] {
  if (field.size === 0) return [];
  const ops: BackdropRect[] = [];
  for (const [key, heat] of field) {
    if (heat < HEAT_OFF_THRESHOLD) continue;
    const comma = key.indexOf(',');
    const cx = Number.parseInt(key.slice(0, comma), 10);
    const cy = Number.parseInt(key.slice(comma + 1), 10);
    if (!Number.isFinite(cx) || !Number.isFinite(cy)) continue;
    const x = cx * cell;
    const y = cy * cell;
    const cw = Math.min(cell, w - x);
    const ch = Math.min(cell, h - y);
    if (cw <= 0 || ch <= 0) continue;
    const alpha = HEAT_PEAK_ALPHA * heat * fadeAlpha(x + cw / 2, y + ch / 2, w, h);
    if (alpha < MIN_OP_ALPHA) continue;
    ops.push({ x, y, w: cw, h: ch, color: withAlpha(palette.brand, alpha) });
  }
  return ops;
}

/** 背景板确定性种子：同尺寸重放即复现（同宽同种子逐像素一致）。 */
export const BACKDROP_SEED = 0x0ea5e9;

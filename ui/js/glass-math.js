// ============================================================
// 液态玻璃（Liquid Glass）光学参数 —— 纯函数，无 DOM / 无 Tauri 依赖
//
// 为什么需要这层数学：
//   面板背后是「真实桌面」的截图，桌面可能是纯白、纯黑或彩色壁纸。
//   如果只按设计稿盖 rgba(255,255,255,.1)，白壁纸 + 白字会直接失去可读性；
//   如果一律用不透明底，又违背液态玻璃的物理光学。
//   玻璃体由四层合成（自下而上）：
//     1) 桌面截图的模糊 + 增饱和（CSS 负责）
//     2) 对比度色阶（本文件按背景亮度分位解出 alpha）
//     3) rgba(255,255,255,.10 / .04) 白偏光（固定，只做“玻璃抛光”）
//     4) 墨色（深色字 / 浅色字，由合成亮度选对比度更高者；CSS 用 [data-ink] 切换 token）
//   暗色主题仍按「正文 ≥ 4.7:1」解最小色阶（验收 4.5），纯白壁纸下到 ~.86，仍是半透明。
//   亮色主题不再靠白底保深色字：白色阶最多 18%，合成偏暗时整块表面切深色 token + 浅色字。
//   中间亮度壁纸上黑/白墨色的最大对比度约 4.0:1（验收线 3.9），这是「非常透明」的物理折中。
//
// 合成按 CSS 实际行为在 sRGB（gamma 编码）空间逐通道进行，再转线性算对比度，
// 比“直接对亮度做 alpha 混合”的近似更准。solveMinAlpha 依赖一个性质：
//   色阶 alpha 越大 → 合成越靠近色阶色 → 正文对比度单调上升（亮/暗都成立）。
// ============================================================

/** 白偏光层：暗色 .10、亮色 .04（亮色只留一点点抛光，不再做大面积白底） */
export const TINT_ALPHA = { dark: 0.1, light: 0.04 };

/** 正文验收目标下限：设计下限 4.5，留 0.2 余量给卡片内高光/顶部 specular */
export const TARGET_CONTRAST = 4.7;

/**
 * 亮色玻璃的白色色阶上限。
 * 亮色不再像暗色那样按 4.7:1 一路加白；最多只铺 18% 白，让壁纸透出来。
 * 代价是中间亮度壁纸上黑/白墨色的最大对比度约 4.1:1（验收线放宽到 4.0），
 * 由 glass.js 按合成亮度自动选择「深色字 / 浅色字」，不再用白底硬保深色字。
 */
export const MAX_LIGHT_GRADE = 0.14;

/**
 * 暗色材质的「玻璃密度」：桌面上越亮，深色玻璃越实（越接近 macOS 深色材质的密度），
 * 深色/中调壁纸则保持几乎通透。这只是设计密度，不是对比度下限：
 * 最终 alpha 取 max(对比度解, 密度值)，所以 4.5:1 仍由对比度解兜住。
 * 例：p95=0 → .10（真玻璃）；p95=.5 → ~.52（彩色壁纸仍能透出颜色）；
 * p95=1 → .86（纯白壁纸下接近 macOS 深色控制中心）。
 */
export const DARK_DENSITY = { base: 0.1, gain: 0.78, gamma: 0.9, max: 0.86 };

/**
 * 蓝玻璃上的白字目标。
 * #007aff 本身对纯白只有 4.04:1（仓库 §6.1 已接受的强调色例外约 3.77）；
 * 亮色主题在纯白壁纸上不管叠多厚都到不了 3.77，3.5 是「0.6 半透明蓝」的物理上限内
 * 仍可读的折中值。暗色主题不受影响（蓝盖在深色玻璃上，通常 4.7:1）。
 */
export const ACCENT_TARGET = 3.5;

/** 正文色（与 shared.css 的 --text 一致） */
const TEXT_RGB = {
  dark: [242, 243, 245], // #f2f3f5
  light: [29, 29, 31], // #1d1d1f
};

/** 色阶色（近黑 / 近白，略偏冷，避免死板的中性灰） */
const GRADE_RGB = {
  dark: [9, 11, 17],
  light: [250, 251, 255],
};

/** macOS 系统蓝（需求指定 0, 122, 255） */
const ACCENT_RGB = [0, 122, 255];

/** 色阶 alpha 的允许区间（下限就是需求的“0.1 白”量级） */
const ALPHA_MIN = 0.1;
const ALPHA_MAX = 0.95;

/** 蓝玻璃 alpha 的允许区间（0.6 为需求基准） */
const ACCENT_ALPHA_MIN = 0.6;
const ACCENT_ALPHA_MAX = 0.9;

/** 阴影兜底色相（背景是灰阶时用这个，避免 HSL 色相抖动） */
const FALLBACK_HUE = 220;

// ---------- 基础色彩工具 ----------

export function clamp(v, lo, hi) {
  return Math.min(hi, Math.max(lo, v));
}

function clamp01(v) {
  return clamp(v, 0, 1);
}

/** sRGB 通道（0..1）→ 线性 */
export function srgbToLinear(c) {
  return c <= 0.04045 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
}

/** 线性 → sRGB 通道（0..1） */
export function linearToSrgb(c) {
  return c <= 0.0031308 ? c * 12.92 : 1.055 * Math.pow(c, 1 / 2.4) - 0.055;
}

/** 相对亮度（rgb 通道 0..255） */
export function relativeLuminance(rgb) {
  const r = srgbToLinear(clamp01(rgb[0] / 255));
  const g = srgbToLinear(clamp01(rgb[1] / 255));
  const b = srgbToLinear(clamp01(rgb[2] / 255));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/** 两个相对亮度之间的 WCAG 对比度（自动按亮/暗排序） */
export function contrastRatio(l1, l2) {
  const a = Math.max(l1, l2);
  const b = Math.min(l1, l2);
  return (a + 0.05) / (b + 0.05);
}

/** sRGB 逐通道 alpha 合成：over 盖在 under 上，返回 0..255 的 rgb 数组 */
export function compositeSrgb(over, under, alpha) {
  const a = clamp01(alpha);
  return over.map((c, i) => a * c + (1 - a) * under[i]);
}

/** 相对亮度为 lum 的中性灰在 sRGB 空间的值（0..255） */
function grayFromLuminance(lum) {
  const v = linearToSrgb(clamp01(lum)) * 255;
  return [v, v, v];
}

/** rgb → hsl（h 0..360, s/l 0..1） */
export function rgbToHsl(rgb) {
  const r = clamp01(rgb[0] / 255);
  const g = clamp01(rgb[1] / 255);
  const b = clamp01(rgb[2] / 255);
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  const d = max - min;
  if (d === 0) return { h: 0, s: 0, l };
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h;
  if (max === r) h = ((g - b) / d + (g < b ? 6 : 0)) / 6;
  else if (max === g) h = ((b - r) / d + 2) / 6;
  else h = ((r - g) / d + 4) / 6;
  return { h: h * 360, s, l };
}

/** hsl（h 0..360, s/l 0..1）→ rgb 0..255 */
export function hslToRgb(h, s, l) {
  const hh = ((h % 360) + 360) % 360 / 360;
  if (s <= 0) {
    const v = Math.round(clamp01(l) * 255);
    return [v, v, v];
  }
  const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
  const p = 2 * l - q;
  const channel = (t) => {
    let tt = t;
    if (tt < 0) tt += 1;
    if (tt > 1) tt -= 1;
    if (tt < 1 / 6) return p + (q - p) * 6 * tt;
    if (tt < 1 / 2) return q;
    if (tt < 2 / 3) return p + (q - p) * (2 / 3 - tt) * 6;
    return p;
  };
  return [
    Math.round(clamp01(channel(hh + 1 / 3)) * 255),
    Math.round(clamp01(channel(hh)) * 255),
    Math.round(clamp01(channel(hh - 1 / 3)) * 255),
  ];
}

// ---------- 色阶求解 ----------

/**
 * 给定主题与背景亮度，计算该 alpha 下的最终合成亮度与正文对比度。
 * 背景按「亮度为 backdropLum 的中性灰」处理（p95/p05 是分位亮度，已偏保守）。
 */
function gradeAt(theme, backdropLum, alpha) {
  const base = grayFromLuminance(backdropLum);
  const grade = GRADE_RGB[theme] || GRADE_RGB.dark;
  const afterGrade = compositeSrgb(grade, base, alpha);
  const afterTint = compositeSrgb([255, 255, 255], afterGrade, TINT_ALPHA[theme]);
  const lum = relativeLuminance(afterTint);
  return {
    rgb: afterTint,
    lum,
    contrast: contrastRatio(lum, relativeLuminance(TEXT_RGB[theme])),
  };
}

/**
 * 解出满足 target 对比度的最小色阶 alpha。
 * 单调性：alpha 越大越靠近色阶色，正文对比度只升不降。
 */
export function solveGradeAlpha(theme, backdropLum, target = TARGET_CONTRAST) {
  const t = theme === "light" ? "light" : "dark";
  const b = clamp01(Number.isFinite(backdropLum) ? backdropLum : 0.5);
  const goal = Number.isFinite(target) ? target : TARGET_CONTRAST;
  if (gradeAt(t, b, ALPHA_MAX).contrast < goal) {
    // 极端背景：已到上限仍不达标，返回上限并在调用方记录（不再继续加白/加黑）
    return ALPHA_MAX;
  }
  let lo = ALPHA_MIN;
  let hi = ALPHA_MAX;
  for (let i = 0; i < 26; i += 1) {
    const mid = (lo + hi) / 2;
    if (gradeAt(t, b, mid).contrast >= goal) hi = mid;
    else lo = mid;
  }
  return hi;
}

/**
 * 蓝玻璃 alpha：白字对「蓝色 × alpha 盖在面板合成色上」要 ≥ 3:1。
 * 暗色主题下 0.6 已足够（面板本身够暗）；亮色主题若面板偏亮会自动升到 ≤0.9。
 */
function solveAccentAlpha(theme, panelLum) {
  const under = grayFromLuminance(panelLum);
  const at = (alpha) => {
    const rgb = compositeSrgb(ACCENT_RGB, under, alpha);
    return contrastRatio(relativeLuminance([255, 255, 255]), relativeLuminance(rgb));
  };
  if (at(ACCENT_ALPHA_MIN) >= ACCENT_TARGET) return ACCENT_ALPHA_MIN;
  let lo = ACCENT_ALPHA_MIN;
  let hi = ACCENT_ALPHA_MAX;
  if (at(hi) < ACCENT_TARGET) return hi;
  for (let i = 0; i < 18; i += 1) {
    const mid = (lo + hi) / 2;
    if (at(mid) >= ACCENT_TARGET) hi = mid;
    else lo = mid;
  }
  return hi;
}

// ---------- 阴影 ----------

/**
 * 由背景平均色推导“同色系深色”投影。
 * 不做纯黑：饱和度夹在 18%–40%，亮度固定为暗 11% / 亮 24%；
 * 背景是灰阶时用主题兜底色相，避免色相噪声。
 */
export function shadowColor(theme, mean) {
  const rgb =
    Array.isArray(mean) && mean.length >= 3
      ? [Number(mean[0]) || 0, Number(mean[1]) || 0, Number(mean[2]) || 0]
      : [128, 128, 128];
  const { h, s } = rgbToHsl(rgb);
  const hue = s < 0.08 ? FALLBACK_HUE : h;
  const sat = clamp(s * 0.7, 0.18, 0.4);
  const light = theme === "light" ? 0.24 : 0.11;
  return hslToRgb(hue, sat, light);
}

/** 多层柔和投影（右侧 8px、底部 44px 窗口留白内，用负 spread 收敛） */
function shadowStack(rgb) {
  const [r, g, b] = rgb;
  return (
    `0 26px 56px -18px rgba(${r}, ${g}, ${b}, 0.55), ` +
    `0 10px 24px -12px rgba(${r}, ${g}, ${b}, 0.42), ` +
    `0 2px 6px -1px rgba(${r}, ${g}, ${b}, 0.28)`
  );
}

// ---------- 对外主入口 ----------

/**
 * 计算一块面板的液态玻璃参数。
 * @param {"light"|"dark"} theme
 * @param {{p05:number,p95:number,mean:number[]}} stats 后端取样结果
 */
export function computeGlassStyle(theme, stats) {
  const t = theme === "light" ? "light" : "dark";
  const p05 = clamp01(Number(stats && stats.p05));
  const p95 = clamp01(Number(stats && stats.p95));
  // 暗色主题看“最亮 5%”（白字不能被亮斑吃掉）；亮色主题看“最暗 5%”（深字不能被暗斑吃掉）
  const backdropLum = t === "dark" ? p95 : p05;
  const contrastAlpha = solveGradeAlpha(t, backdropLum, TARGET_CONTRAST);
  // 暗色材质额外叠一层「背景越亮、玻璃越实」的设计密度；对比度仍由 contrastAlpha 兜底。
  // 亮色反过来：最多只铺 MAX_LIGHT_GRADE 的白色，不做大面积白底；可读性交给下面的墨色选择。
  const density =
    t === "dark"
      ? clamp(
          DARK_DENSITY.base + DARK_DENSITY.gain * Math.pow(backdropLum, DARK_DENSITY.gamma),
          ALPHA_MIN,
          DARK_DENSITY.max
        )
      : 0;
  const gradeAlpha =
    t === "dark" ? Math.max(contrastAlpha, density) : Math.min(contrastAlpha, MAX_LIGHT_GRADE);
  const graded = gradeAt(t, backdropLum, gradeAlpha);
  // 墨色自适应：合成后的玻璃偏亮 → 深色字；偏暗 → 浅色字。取对比度更高的那一个。
  const lightInkContrast = contrastRatio(graded.lum, relativeLuminance(TEXT_RGB.dark));
  const darkInkContrast = contrastRatio(graded.lum, relativeLuminance(TEXT_RGB.light));
  const ink = t === "dark" || lightInkContrast >= darkInkContrast ? "light" : "dark";
  const inkRgb = ink === "light" ? TEXT_RGB.dark : TEXT_RGB.light;
  const inkContrast = Math.max(lightInkContrast, darkInkContrast);
  const accentAlpha = solveAccentAlpha(t, graded.lum);
  // 深色玻璃配深色投影，浅色玻璃配浅色投影；都不使用纯黑
  const shadowRgb = shadowColor(ink === "light" ? "dark" : "light", stats && stats.mean);

  return {
    theme: t,
    grade: `rgba(${GRADE_RGB[t][0]}, ${GRADE_RGB[t][1]}, ${GRADE_RGB[t][2]}, ${gradeAlpha.toFixed(3)})`,
    tint: `rgba(255, 255, 255, ${TINT_ALPHA[t]})`,
    gradeAlpha,
    accentAlpha,
    shadow: shadowStack(shadowRgb),
    shadowRgb,
    backdropLum,
    compositeLum: graded.lum,
    compositeRgb: graded.rgb.map((v) => Math.round(v)),
    ink,
    inkRgb,
    inkContrast,
    // 兼容旧调用：正文对比度改为「当前墨色」的对比度
    bodyContrast: inkContrast,
    // 蓝玻璃上的白字对比度（用于单测与调试）
    accentContrast: contrastRatio(
      relativeLuminance([255, 255, 255]),
      relativeLuminance(compositeSrgb(ACCENT_RGB, grayFromLuminance(graded.lum), accentAlpha))
    ),
  };
}

/** 没有取样（截屏失败/未跑 JS）时的安全默认值，与 materials.css 的兜底 token 对齐 */
export function defaultGlassStyle(theme) {
  const t = theme === "light" ? "light" : "dark";
  const gradeAlpha = t === "light" ? 0.24 : 0.72;
  const ink = t === "light" ? "dark" : "light";
  const inkRgb = ink === "light" ? TEXT_RGB.dark : TEXT_RGB.light;
  const shadowRgb = ink === "light" ? [18, 22, 34] : [46, 52, 70];
  return {
    theme: t,
    grade: `rgba(${GRADE_RGB[t][0]}, ${GRADE_RGB[t][1]}, ${GRADE_RGB[t][2]}, ${gradeAlpha})`,
    tint: `rgba(255, 255, 255, ${TINT_ALPHA[t]})`,
    gradeAlpha,
    accentAlpha: ACCENT_ALPHA_MIN,
    shadow: shadowStack(shadowRgb),
    shadowRgb,
    backdropLum: 0.5,
    compositeLum: 0,
    compositeRgb: [0, 0, 0],
    ink,
    inkRgb,
    inkContrast: 0,
    bodyContrast: 0,
    accentContrast: 0,
  };
}

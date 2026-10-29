// ============================================================
// 液态玻璃控制层：桌面取样 → CSS 变量 → 主题/材质变化重算
//
// 背景：面板是顶栏透明窗口里的 .panel，WebView2 的 backdrop-filter 采不到
// 窗口外的桌面（见 docs/design-system.md §1）。因此面板打开前由 Rust
// 截取「面板将要占据的屏幕矩形」，这里把结果写成 --lg-backdrop，
// 再由 materials.css 做 blur(30px) saturate(150%) + feDisplacementMap。
//
// 职责边界：
//   - 本文件只负责「什么时候截、截到的图/统计量怎么落到哪个面板」，不含光学公式
//     （公式在 glass-math.js，纯函数可单测）。
//   - 面板的显示/隐藏、窗口高度、竞态 token 由 panels.js 负责；这里只处理玻璃本身。
//   - 截屏失败 / 超时 / 非 Windows：回落到 CSS 默认色阶（仍然可读的模拟玻璃）。
// ============================================================
import { invoke, listen } from "./api.js";
import { resolvedTheme, subscribeTheme } from "./theme.js";
import { computeGlassStyle, defaultGlassStyle } from "./glass-math.js";

/** 截屏 IPC 的超时：正常 10–40ms；超时先按兜底玻璃打开面板，不阻塞交互 */
const CAPTURE_TIMEOUT_MS = 220;

/** 入场动画结束后再启用位移滤镜，避免 filter + transform 同帧栅格化掉帧 */
const REFRACT_DELAY_MS = 320;

/** 太小的矩形（面板还没布局）不截屏 */
const MIN_CAPTURE_SIZE = 80;

/**
 * 每块面板的取样缓存：
 * el -> { stats: {p05,p95,mean}, style, at }
 * 只在同一块面板生命周期内复用（面板重新打开时 panels.js 会 resetGlass）。
 */
const cache = new Map();

/** 每次 prepare 的序号：后发的请求覆盖先发的，过期结果直接丢弃 */
let seq = 0;

/** el -> 当前有效的请求序号（同样是过期的判定依据） */
const jobs = new WeakMap();

// ---------- 小工具 ----------

function isLiquid() {
  return document.documentElement.dataset.material === "liquid";
}

function nextFrame() {
  return new Promise((resolve) => {
    if (typeof requestAnimationFrame === "function") requestAnimationFrame(() => resolve());
    else setTimeout(resolve, 16);
  });
}

/** Promise 与超时赛跑；失败/超时都返回 null（不抛） */
function withTimeout(promise, ms) {
  return new Promise((resolve) => {
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      resolve(null);
    }, ms);
    Promise.resolve(promise)
      .then((value) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        resolve(value);
      })
      .catch(() => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        resolve(null);
      });
  });
}

/** 边缘折射装饰层（液态玻璃的“液态”来源；必须排在镜面边之上、内容之下） */
function ensureEdge(el) {
  if (el.querySelector(":scope > .lg-edge")) return;
  const edge = document.createElement("span");
  edge.className = "lg-edge";
  edge.setAttribute("aria-hidden", "true");
  el.appendChild(edge);
}

function removeEdge(el) {
  el.querySelectorAll(":scope > .lg-edge").forEach((n) => n.remove());
}

/** 把光学参数落到面板上（颜色/阴影/墨色都是内联变量，CSS 只消费） */
function applyStyle(el, style) {
  el.style.setProperty("--lg-grade", style.grade);
  el.style.setProperty("--lg-tint", style.tint);
  el.style.setProperty("--lg-accent-alpha", String(style.accentAlpha));
  el.style.setProperty("--lg-shadow", style.shadow);
  // 亮色玻璃不再靠白底保深色字：合成偏暗时整块衬底切到深色 token + 浅色墨
  el.dataset.ink = style.ink || (resolvedTheme() === "light" ? "dark" : "light");
}

function applyLive(el, dataUrl, style) {
  el.style.setProperty("--lg-backdrop", `url("${dataUrl}")`);
  applyStyle(el, style);
  el.dataset.glass = "live";
}

function applyFallback(el) {
  el.style.removeProperty("--lg-backdrop");
  applyStyle(el, defaultGlassStyle(resolvedTheme()));
  el.dataset.glass = "fallback";
  el.classList.remove("lg-refract");
}

function scheduleRefract(el, token, delay = REFRACT_DELAY_MS) {
  setTimeout(() => {
    if (jobs.get(el) !== token) return;
    if (!isLiquid()) return;
    if (el.classList.contains("hidden")) return;
    el.classList.add("lg-refract");
  }, delay);
}

/** 截屏 + 校验 + 统计；过期的结果返回 null */
async function capture(el, token) {
  const rect = el.getBoundingClientRect();
  if (
    !Number.isFinite(rect.width) ||
    !Number.isFinite(rect.height) ||
    rect.width < MIN_CAPTURE_SIZE ||
    rect.height < MIN_CAPTURE_SIZE
  ) {
    return null;
  }
  const res = await withTimeout(
    invoke("glass_backdrop", {
      x: rect.left,
      y: rect.top,
      width: rect.width,
      height: rect.height,
    }),
    CAPTURE_TIMEOUT_MS
  );
  if (jobs.get(el) !== token) return null; // 面板已关闭 / 又发起了新的请求
  if (!res || typeof res.dataUrl !== "string" || !res.dataUrl.startsWith("data:image/")) {
    return null;
  }
  const num = (v) => (Number.isFinite(Number(v)) ? Number(v) : 0.5);
  return {
    dataUrl: res.dataUrl,
    stats: {
      p05: num(res.p05),
      p95: num(res.p95),
      mean: Array.isArray(res.mean) ? res.mean.slice(0, 3) : [128, 128, 128],
    },
    rect: { width: rect.width, height: rect.height },
  };
}

// ---------- 对外接口 ----------

/**
 * 面板打开前调用：截屏并写入玻璃变量。
 * 调用方必须保证面板已完成布局（.hidden 已移除；可用 .lg-wait 保持不可见）。
 * @returns {Promise<boolean>} 是否拿到了真实桌面
 */
export async function prepareGlass(el) {
  if (!el) return false;
  if (!isLiquid()) return false;
  ensureEdge(el);
  el.classList.remove("lg-refract");
  const token = ++seq;
  jobs.set(el, token);

  const shot = await capture(el, token);
  if (jobs.get(el) !== token) return false;

  if (!shot) {
    applyFallback(el);
    return false;
  }
  const style = computeGlassStyle(resolvedTheme(), shot.stats);
  applyLive(el, shot.dataUrl, style);
  cache.set(el, { stats: shot.stats, style, at: Date.now() });
  scheduleRefract(el, token);
  return true;
}

/**
 * 面板以非液态玻璃材质打开时调用：清掉上一次的取样，避免稍后切到 liquid 时
 * 复用一张已经过期的桌面（切材质时若没有缓存会重新截一次）。
 */
export function resetGlass(el) {
  if (!el) return;
  cache.delete(el);
  el.classList.remove("lg-refract");
  removeEdge(el);
}

/**
 * 主题切换时重算色阶 / 蓝 alpha / 投影：不重新截屏、不闪烁。
 * 材质切到 liquid 且当前面板没有本次打开的有效取样时，才隐藏面板补截一次。
 */
function syncOpenPanels() {
  if (!document.querySelectorAll) return;
  const panels = document.querySelectorAll(".panel:not(.hidden):not(.panel-out)");
  for (const el of panels) {
    if (isLiquid()) {
      const rec = cache.get(el);
      if (rec) {
        const style = computeGlassStyle(resolvedTheme(), rec.stats);
        applyStyle(el, style);
        rec.style = style;
        if (el.dataset.glass !== "live") el.dataset.glass = "live";
        // 已经开着的面板不需要再等入场动画
        if (!el.classList.contains("lg-refract")) {
          const token = jobs.get(el) || ++seq;
          jobs.set(el, token);
          scheduleRefract(el, token, 0);
        }
      } else {
        recaptureOpen(el).catch(() => {});
      }
    } else {
      el.classList.remove("lg-refract");
      if (el.dataset.glass && el.dataset.glass !== "off") el.dataset.glass = "off";
    }
  }
}

/**
 * 面板已经展开时，切到 liquid 需要补一次截屏：
 * 先 visibility:hidden（窗口在该区域是透明的，屏幕上就是桌面），截完再恢复。
 * 期间会短暂空白 ~30ms，只发生在用户主动切换材质的瞬间。
 */
async function recaptureOpen(el) {
  if (!isLiquid() || el.classList.contains("hidden")) return false;
  el.classList.add("lg-wait");
  try {
    await nextFrame();
    await nextFrame();
    return await prepareGlass(el);
  } finally {
    el.classList.remove("lg-wait");
  }
}

// ============================================================
// 全部 surface：顶栏 / Dock / 管理面板 / 概览 / 应用中心
//
// 这些窗口的取样由 Rust 在 show 之前完成（window_backdrop + surface_backdrops 缓存），
// 前端只做两件事：
//   1) 启动时用 surface_backdrop 命令兜底读取（顶栏在页面加载前就已 show，事件会丢）；
//   2) 监听 surface-backdrop 事件（Dock 重显、管理面板/概览/应用中心每次打开都会发）。
// 每个窗口只命中一个 surface；面板（.panel）仍走上面的 prepareGlass。
// ============================================================

/** 当前窗口的玻璃表面：{ el, kind }；一个页面至多一个 */
let surface = null;

/** surface 元素 -> { stats, style }（主题切换时只重算，不重新截屏） */
const surfaceCache = new Map();

/** 不同表面的模糊半径：32px 顶栏不能和 604px 面板用同一个 30px */
function blurFor(kind) {
  if (kind === "topbar") return "18px";
  if (kind === "dock") return "24px";
  return "30px";
}

function detectSurface() {
  if (typeof document === "undefined") return null;
  const topbar = document.querySelector(".topbar");
  if (topbar) return { el: topbar, kind: "topbar" };
  const dock = document.querySelector(".dock");
  if (dock) return { el: dock, kind: "dock" };
  const manager = document.querySelector(".mp-panel");
  if (manager) return { el: manager, kind: "manager" };
  const backdrop = document.getElementById("backdrop");
  if (backdrop) {
    return {
      el: backdrop,
      kind: document.getElementById("ac-panel") ? "launcher" : "overview",
    };
  }
  return null;
}

/** 注入折射滤镜（顶栏页已在 topbar.html 内联，检测到就不重复注入） */
function ensureFilters() {
  if (document.getElementById("lg-refract") && document.getElementById("lg-refract-edge")) {
    return;
  }
  const wrap = document.createElement("div");
  wrap.innerHTML = `
    <svg class="lg-filters" width="0" height="0" aria-hidden="true" focusable="false">
      <defs>
        <filter id="lg-refract" x="-15%" y="-15%" width="130%" height="130%" color-interpolation-filters="sRGB">
          <feTurbulence type="fractalNoise" baseFrequency="0.006 0.012" numOctaves="2" seed="11" result="lgNoise" />
          <feGaussianBlur in="lgNoise" stdDeviation="2" result="lgSoftNoise" />
          <feDisplacementMap in="SourceGraphic" in2="lgSoftNoise" scale="12" xChannelSelector="R" yChannelSelector="G" />
        </filter>
        <filter id="lg-refract-edge" x="-10%" y="-10%" width="120%" height="120%" color-interpolation-filters="sRGB">
          <feTurbulence type="fractalNoise" baseFrequency="0.02 0.035" numOctaves="2" seed="4" result="lgEdgeNoise" />
          <feGaussianBlur in="lgEdgeNoise" stdDeviation="0.7" result="lgEdgeSoft" />
          <feDisplacementMap in="SourceGraphic" in2="lgEdgeSoft" scale="3" xChannelSelector="R" yChannelSelector="G" />
        </filter>
      </defs>
    </svg>`;
  const svg = wrap.firstElementChild;
  if (svg) document.body.appendChild(svg);
}

function statNumber(v) {
  const n = Number(v);
  return Number.isFinite(n) ? Math.min(1, Math.max(0, n)) : 0.5;
}

/** 把 Rust 的 window_backdrop 结果落到 surface 上 */
function applySurface(entry, res) {
  if (!entry || !res || typeof res.dataUrl !== "string") return false;
  const stats = {
    p05: statNumber(res.p05),
    p95: statNumber(res.p95),
    mean: Array.isArray(res.mean) ? res.mean.slice(0, 3) : [128, 128, 128],
  };
  const style = computeGlassStyle(resolvedTheme(), stats);
  const el = entry.el;
  el.style.setProperty("--lg-backdrop", `url("${res.dataUrl}")`);
  applyStyle(el, style);
  el.dataset.glass = "live";
  surfaceCache.set(el, { stats, style });
  if (entry.kind === "dock" || entry.kind === "manager") {
    ensureEdge(el);
    setTimeout(() => {
      if (!isLiquid()) return;
      el.classList.add("lg-refract");
    }, REFRACT_DELAY_MS);
  }
  return true;
}

function applySurfaceFallback(entry) {
  if (!entry || surfaceCache.has(entry.el)) return;
  entry.el.dataset.glass = "fallback";
  applyStyle(entry.el, defaultGlassStyle(resolvedTheme()));
}

/** 启动时 / 每次打开时拉一次缓存；失败保持 CSS 兜底色阶 */
async function refreshSurface(entry) {
  if (!entry) return false;
  let res = null;
  try {
    res = await invoke("surface_backdrop");
  } catch (e) {
    res = null;
  }
  if (res) return applySurface(entry, res);
  applySurfaceFallback(entry);
  return false;
}

/**
 * 切成 liquid 时，如果这个 surface 还没有底片，就地把表面 visibility:hidden
 * 一拍（窗口是透明的，截到的是桌面），补一张再揭示。
 * 只发生在用户主动切材质、且启动时是 solid/glow 的情况下；约 30ms 空白。
 */
async function recaptureSurface(entry) {
  if (!isLiquid() || !entry) return false;
  const el = entry.el;
  el.classList.add("lg-wait");
  try {
    await nextFrame();
    await nextFrame();
    const rect = el.getBoundingClientRect();
    if (rect.width < MIN_CAPTURE_SIZE || rect.height < 1) return false;
    const res = await withTimeout(
      invoke("glass_backdrop", {
        x: rect.left,
        y: rect.top,
        width: rect.width,
        height: rect.height,
      }),
      CAPTURE_TIMEOUT_MS
    );
    if (res) return applySurface(entry, res);
    return false;
  } finally {
    el.classList.remove("lg-wait");
  }
}

/** 主题/材质切换：用已缓存的统计量重算色阶，不重新截屏 */
function regradeSurface() {
  if (!surface) return;
  const rec = surfaceCache.get(surface.el);
  if (!rec) {
    // 还没有取样：内联的是上一主题的兜底色阶，必须跟着主题换掉
    applyStyle(surface.el, defaultGlassStyle(resolvedTheme()));
    return;
  }
  const style = computeGlassStyle(resolvedTheme(), rec.stats);
  applyStyle(surface.el, style);
  rec.style = style;
}

function initSurfaceGlass() {
  if (surface) return;
  const entry = detectSurface();
  if (!entry) return;
  surface = entry;
  const { el, kind } = entry;
  ensureFilters();
  el.classList.add("lg-surface", `lg-${kind}`);
  if (kind === "overview" || kind === "launcher") el.classList.add("lg-overlay");
  el.style.setProperty("--lg-blur", blurFor(kind));
  el.dataset.glass = "fallback";
  applyStyle(el, defaultGlassStyle(resolvedTheme()));
  if (kind === "dock" || kind === "manager") ensureEdge(el);
  refreshSurface(entry)
    .catch(() => {})
    .finally(() => {
      // 顶栏 / Dock 的底片来自 Rust 的**壁纸取样**（wallpaper_backdrop），与窗口可见性、
      // AppBar 预留时机无关，所以页面就绪后再兜底拉一次即可。
      // 不要在这里用 glass_backdrop 截屏补拍：启动瞬间条带下面还是前台窗口的标题栏，
      // 补拍到的会是那条纯色，反而把玻璃覆盖成「一整条灰白」。
      if ((kind === "topbar" || kind === "dock") && isLiquid() && !surfaceCache.has(el)) {
        refreshSurface(entry).catch(() => {});
      }
    });

  // Dock 首次 show 后若还没有底片，再拉一次（同样走壁纸取样，绝不截屏）。
  if (kind === "dock") {
    const retryAfterShow = () => {
      if (!surfaceCache.has(el)) refreshSurface(entry).catch(() => {});
    };
    window.addEventListener("easydash:dock-shown", retryAfterShow);
    setTimeout(retryAfterShow, 900);
  }

  // 兜底：Dock 首帧的 surface-backdrop 事件可能早于本页监听器就绪
  setTimeout(() => {
    if (!surfaceCache.has(el)) refreshSurface(entry).catch(() => {});
  }, 700);

  try {
    listen("surface-backdrop", (e) => {
      // 顶栏/Dock 是常驻 chrome：一旦已有底片，后续重显事件不再覆盖。
      // 管理面板/概览/应用中心的打开补截走下面的 openEvent（每次打开都重新取）。
      if (e && e.payload && !surfaceCache.has(entry.el)) applySurface(entry, e.payload);
    }).catch(() => {});
  } catch (e) {
    /* Tauri 未注入时忽略 */
  }

  // 管理面板 / 概览 / 应用中心：每次打开都会在 show 前重新取样。
  // 这里不用窗口级 surface_backdrop（window_backdrop 仍在真实桌面验证中），
  // 直接用已验证可用的 glass_backdrop 把元素隐藏一拍就地补截，保证不出 .3 white fallback。
  const openEvent = {
    manager: "manager-open",
    overview: "overview-data",
    launcher: "app-center-open",
  }[kind];
  if (openEvent) {
    try {
      listen(openEvent, () => {
        // 打开动画：等窗口级底片补截完成、.lg-wait 揭示后再让页面入场，
        // 否则入场动画会在「表面被隐藏截屏」的阶段被吃掉一半
        // （surface-open.js 监听 easydash:surface-ready）。
        setTimeout(async () => {
          await recaptureSurface(entry).catch(() => {});
          window.dispatchEvent(
            new CustomEvent("easydash:surface-ready", { detail: { kind } })
          );
        }, 30);
      }).catch(() => {});
    } catch (e) {
      /* 同上 */
    }
  }
}

// 主题 / 材质变化（theme.js 的订阅者会收到完整快照）
let surfaceWasLiquid = isLiquid();
subscribeTheme(() => {
  syncOpenPanels();
  const liquid = isLiquid();
  // 启动时是 solid/glow，用户切到 liquid：没有底片就补截一次。
  // 概览/应用中心不在这里补：它们打开时若材质还不是 liquid 会跳过取样，
  // 下一次打开会重新截；避免在它们之上开着控制中心时把控制中心截进全屏底图。
  if (
    liquid &&
    !surfaceWasLiquid &&
    surface &&
    !surfaceCache.has(surface.el) &&
    (surface.kind === "topbar" || surface.kind === "dock" || surface.kind === "manager")
  ) {
    // 顶栏/Dock 走壁纸取样（refreshSurface → surface_backdrop），管理面板才需要截屏补拍
    if (surface.kind === "manager") recaptureSurface(surface).catch(() => {});
    else refreshSurface(surface).catch(() => {});
  }
  surfaceWasLiquid = liquid;
  regradeSurface();
});

initSurfaceGlass();

// 页面隐藏/卸载时不需要额外清理；面板 / surface DOM 常驻。

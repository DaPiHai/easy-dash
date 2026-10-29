// 顶栏面板管理：互斥开关、窗口高度联动、统一的关闭触发。
// 面板高度常量集中在这里，其他模块只按名字引用，避免魔法数字散落各处。
//
// 依赖方向（避免循环 import）：
//   topbar.js → panels.js        （注入回调、调用 open/toggle/close）
//   notifications.js / control-center.js 不 import 本模块，
//   只把 onOpen/onClose 交给 topbar.js 注册，天然无环。
import { invoke } from "./api.js";
import { prepareGlass, resetGlass } from "./glass.js";

/** 顶栏常态高度（逻辑像素），与后端 commands.rs 的 BAR_H 一致 */
export const BAR_HEIGHT = 32;

/** 各面板展开时的窗口高度（逻辑像素） */
export const PANEL_HEIGHTS = {
  notifications: 620, // 面板 536 + top 40 + 底部 44 阴影空间
  control: 688, // 面板 604（含多屏卡片 + 界面材质分段）+ top 40 + 底部 44 阴影空间
};

/** 面板名 → 容器 id（与 topbar.html 对应） */
export const PANEL_IDS = {
  notifications: "panel-notifications",
  control: "panel-control",
};

const panels = new Map(); // name -> { el, onOpen, onClose }
let openName = null;
let barEl = null;
let inited = false;

/** 注册一个面板：容器 + 打开/关闭回调 */
export function registerPanel(name, { element, onOpen, onClose } = {}) {
  const el = element || document.getElementById(PANEL_IDS[name]);
  if (!el) {
    console.warn("[顶栏] 面板容器不存在:", name);
    return;
  }
  panels.set(name, {
    el,
    onOpen: typeof onOpen === "function" ? onOpen : () => {},
    onClose: typeof onClose === "function" ? onClose : () => {},
  });
  if (openName !== name) el.classList.add("hidden");
}

/** 绑定全局关闭触发（空白处 mousedown / Esc / 窗口失焦） */
export function initPanels() {
  if (inited) return;
  inited = true;
  barEl = document.getElementById("topbar");

  document.addEventListener("mousedown", (e) => {
    if (!openName) return;
    const p = panels.get(openName);
    if (p && p.el.contains(e.target)) return; // 面板内部的点击不算空白
    if (barEl && barEl.contains(e.target)) return; // 顶栏按钮自己处理开关
    closePanels();
  });

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && openName) closePanels();
  });

  // 点击到别的应用窗口（本窗口失焦）时收起面板
  window.addEventListener("blur", () => {
    if (openName) closePanels();
  });
}

export function isPanelOpen(name) {
  return openName === name;
}

export function getOpenPanel() {
  return openName;
}

/**
 * 打开面板（互斥）：先收另一个 → 拉高窗口 → 抢焦点。
 * 返回是否成功打开。
 *
 * 液态玻璃下多一步「桌面取样」：窗口还是 32px 高时先截面板将占据的屏幕区域，
 * 拿到图之后才拉高窗口并揭示面板（截屏里因此不会包含面板自身）。
 * 面板先挂 .lg-wait（visibility:hidden + 禁止动画）保证截屏期间不可见、
 * 且内联 max-height 让 32px 视口下面板也能量出最终高度。
 */
export function openPanel(name) {
  const target = panels.get(name);
  if (!target) return false;
  if (openName === name) return true;
  // 上一块面板若正在播退出动画，直接取消，避免延迟隐藏把新面板盖掉
  clearCloseTimer();
  // 先收另一个，但不缩窗口：紧接着会 resize 到新高度，避免 32px 中间态闪动
  hideCurrent(false);
  // 正在播退出动画的面板已不在 openName 里（hideCurrent 早退）：
  // 这里统一把其它面板立即隐藏，避免残留 .panel-out / 半透明状态
  for (const [key, p] of panels) {
    if (key === name || !p || !p.el) continue;
    p.el.classList.remove("panel-out");
    p.el.classList.add("hidden");
  }
  openName = name;
  const el = target.el;
  const height = PANEL_HEIGHTS[name] || BAR_HEIGHT;
  const token = ++openToken;
  const liquid = document.documentElement.dataset.material === "liquid";

  // 每次打开都丢弃上一轮的取样：桌面/位置可能已变
  resetGlass(el);
  // 先挂不可见 + 目标高度，再移除 .hidden：保证同一帧内不会先播入场动画/先按 32px 视口布局
  if (liquid) {
    el.classList.add("lg-wait");
    // 与 CSS 的 max-height: calc(100vh - 84px) 等价，但用目标窗口高度而不是当前 32px
    el.style.maxHeight = Math.max(0, height - 84) + "px";
  } else {
    el.style.removeProperty("max-height");
    el.classList.remove("lg-wait");
  }
  el.classList.remove("hidden", "panel-out");
  document.body.classList.add("panel-open");
  safeCall(target.onOpen);
  // 焦点先给（Esc 依赖窗口焦点），截屏不会改变窗口尺寸
  requestFocus();

  if (liquid) {
    prepareGlass(el)
      .then(async () => {
        if (token !== openToken || openName !== name) return;
        // 先拉高窗口，再等 WebView 视口真正长高，最后才移除内联高度并揭示
        await requestResize(height);
        await waitForViewportGrowth();
        if (token !== openToken || openName !== name) return;
        el.style.removeProperty("max-height");
        el.classList.remove("lg-wait");
      })
      .catch(() => {
        // 玻璃失败也必须把面板放出来，不能卡在 visibility:hidden
        if (token !== openToken || openName !== name) return;
        el.style.removeProperty("max-height");
        el.classList.remove("lg-wait");
        requestResize(height);
      });
  } else {
    requestResize(height);
  }
  return true;
}

/** 再点同一入口：已打开则关闭，否则打开 */
export function togglePanel(name) {
  if (openName === name) {
    closePanels();
    return true;
  }
  return openPanel(name);
}

/** 收起当前面板（窗口缩回常态高度） */
export function closePanels() {
  hideCurrent(true);
}

/** 退出动画时长（与 motion.css 的 .panel.panel-out 一致） */
const PANEL_OUT_MS = 150;
/** 退出动画期间的延迟隐藏计时器 */
let closeTimer = null;

/**
 * 打开竞态 token：每次打开/收起 +1。
 * 液态玻璃的截屏是异步的，若期间用户按 Esc / 点别处关掉面板，
 * 迟到的回调不能再把窗口拉高或揭示面板。
 */
let openToken = 0;

/**
 * 等 WebView 视口真正长高再移除内联 max-height。
 * 顶栏窗口默认 32px；`resize_topbar` 已返回但 WebView 视口可能还差一帧。
 * 若过早移除内联高度，面板会被 `max-height: calc(100vh - 84px)` 压扁一帧。
 * 兜底 220ms：即使视口没跟上，也照常揭示，不能卡在隐藏态。
 */
function waitForViewportGrowth(timeout = 220) {
  return new Promise((resolve) => {
    const t0 = performance.now();
    const tick = () => {
      if (window.innerHeight > BAR_HEIGHT + 1 || performance.now() - t0 >= timeout) {
        resolve();
        return;
      }
      if (typeof requestAnimationFrame === "function") requestAnimationFrame(tick);
      else setTimeout(tick, 16);
    };
    if (typeof requestAnimationFrame === "function") requestAnimationFrame(tick);
    else setTimeout(tick, 16);
  });
}

function clearCloseTimer() {
  if (closeTimer) {
    clearTimeout(closeTimer);
    closeTimer = null;
  }
}

function hideCurrent(resize) {
  if (!openName) return;
  // 让仍在等待截屏的回调失效
  openToken += 1;
  const name = openName;
  openName = null;
  document.body.classList.remove("panel-open");
  const p = panels.get(name);
  clearCloseTimer();
  if (!p) {
    if (resize) requestResize(BAR_HEIGHT);
    return;
  }
  safeCall(p.onClose);
  if (resize) {
    // iOS 抽屉：先播 150ms 退出动画（下沉 + 淡出），再隐藏并把窗口缩回 32px
    p.el.classList.add("panel-out");
    closeTimer = setTimeout(() => {
      closeTimer = null;
      p.el.classList.remove("panel-out");
      p.el.classList.add("hidden");
      requestResize(BAR_HEIGHT);
    }, PANEL_OUT_MS);
  } else {
    // 面板互切：立即隐藏，不播退出动画（避免中间态闪动）
    p.el.classList.remove("panel-out");
    p.el.classList.add("hidden");
  }
}

// 面板回调里抛错不能影响开关逻辑
function safeCall(fn) {
  try {
    const r = fn();
    if (r && typeof r.catch === "function") r.catch((e) => console.error("[顶栏] 面板回调异常", e));
  } catch (e) {
    console.error("[顶栏] 面板回调异常", e);
  }
}

function requestResize(height) {
  try {
    return invoke("resize_topbar", { height }).catch(() => {});
  } catch (e) {
    /* Tauri 尚未注入时忽略 */
    return Promise.resolve();
  }
}

function requestFocus() {
  try {
    invoke("focus_topbar").catch(() => {});
  } catch (e) {
    /* Tauri 尚未注入时忽略 */
  }
}

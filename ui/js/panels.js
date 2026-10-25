// 顶栏面板管理：互斥开关、窗口高度联动、统一的关闭触发。
// 面板高度常量集中在这里，其他模块只按名字引用，避免魔法数字散落各处。
//
// 依赖方向（避免循环 import）：
//   topbar.js → panels.js        （注入回调、调用 open/toggle/close）
//   notifications.js / control-center.js 不 import 本模块，
//   只把 onOpen/onClose 交给 topbar.js 注册，天然无环。
import { invoke } from "./api.js";

/** 顶栏常态高度（逻辑像素），与后端 commands.rs 的 BAR_H 一致 */
export const BAR_HEIGHT = 32;

/** 各面板展开时的窗口高度（逻辑像素） */
export const PANEL_HEIGHTS = {
  notifications: 470,
  control: 410,
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
 */
export function openPanel(name) {
  const target = panels.get(name);
  if (!target) return false;
  if (openName === name) return true;
  // 先收另一个，但不缩窗口：紧接着会 resize 到新高度，避免 32px 中间态闪动
  hideCurrent(false);
  openName = name;
  target.el.classList.remove("hidden");
  document.body.classList.add("panel-open");
  safeCall(target.onOpen);
  // 窗口高度 + 焦点（Esc 需要窗口有焦点）
  requestResize(PANEL_HEIGHTS[name] || BAR_HEIGHT);
  requestFocus();
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

function hideCurrent(resize) {
  if (!openName) return;
  const name = openName;
  openName = null;
  const p = panels.get(name);
  if (p) {
    p.el.classList.add("hidden");
    safeCall(p.onClose);
  }
  document.body.classList.remove("panel-open");
  if (resize) requestResize(BAR_HEIGHT);
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
    invoke("resize_topbar", { height }).catch(() => {});
  } catch (e) {
    /* Tauri 尚未注入时忽略 */
  }
}

function requestFocus() {
  try {
    invoke("focus_topbar").catch(() => {});
  } catch (e) {
    /* Tauri 尚未注入时忽略 */
  }
}

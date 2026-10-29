// ============================================================
// 全屏表面（活动概览 / 应用中心）打开动画的共享控制
//
// 关键：窗口的 show/hide 由 Rust 直接调用，show 的第一帧会先呈现
// 「上一次打开的界面」。所以关闭时必须把页面退回关闭态 .surf-closed
// （遮罩 opacity 0，瞬时、无过渡）——Rust 在 hide() 后发 surface-closed，
// 这里收到就重置；页面加载时也先置为关闭态。这样下次 show 的第一帧
// 就是透明的，不会闪出旧界面（浅色 + 液态玻璃下最容易看成白闪）。
//
// 打开时序：open 事件（overview-data / app-center-open）
//   → beginSurfaceOpen()：确保处于 .surf-closed（等玻璃底片）
//   → glass.js 在玻璃底片补截完成、.lg-wait 揭示后派发 easydash:surface-ready
//   → 移除 .surf-closed、加 .surf-open（CSS 播遮罩淡入 + 内容上浮 + 卡片错峰）
//   → 600ms 后移除 .surf-open（回到静态样式，不残留 animation / fill）
// 兜底：glass.js 未加载 / 事件丢失时 420ms 后自行入场，避免停在透明态。
//
// 为什么遮罩用 transition 而不是 animation：liquid 的 .lg-wait 会给
// #backdrop 加 `animation: none !important`（截屏期间），animation 会被
// 掐断并重播，transition 不受影响（见 motion.css §11）。
// ============================================================
import { listen } from "./api.js";

/** 玻璃就绪事件丢失时的兜底入场延迟：补截 30ms 延迟 + 220ms 超时 + 余量 */
const OPEN_FALLBACK_MS = 420;

/** .surf-open 的保留时长：最长错峰 + 动画时长 + 余量（清理后回到静态样式） */
const OPEN_HOLD_MS = 600;

/** surface-closed 监听注册失败（Tauri 尚未注入）时的重试间隔与次数 */
const BIND_RETRY_MS = 200;
const BIND_RETRY_MAX = 30;

let backdropEl = null;
let fallbackTimer = 0;
let holdTimer = 0;
/** 是否有一次打开在等待入场（关闭后迟到的 surface-ready 不能把窗口重新「打开」） */
let openPending = false;

function backdrop() {
  if (!backdropEl) backdropEl = document.getElementById("backdrop");
  return backdropEl;
}

function reducedMotion() {
  try {
    return !!window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  } catch (e) {
    return false;
  }
}

/** 从关闭态开始入场；重复调用幂等（重复的 surface-ready 不会二次播放） */
function play() {
  if (!openPending) return;
  const el = backdrop();
  if (!el || !el.classList.contains("surf-closed")) return;
  openPending = false;
  if (fallbackTimer) {
    clearTimeout(fallbackTimer);
    fallbackTimer = 0;
  }
  el.classList.remove("surf-closed");
  void el.offsetWidth; // 强制重排：连续打开也能从关闭态重新开始
  el.classList.add("surf-open");
  if (holdTimer) clearTimeout(holdTimer);
  holdTimer = setTimeout(() => {
    holdTimer = 0;
    el.classList.remove("surf-open");
  }, OPEN_HOLD_MS);
}

/** 回到关闭态（窗口已隐藏时调用；瞬时生效，专供下一次 show 的首帧） */
function resetClosed() {
  openPending = false;
  const el = backdrop();
  if (!el) return;
  if (holdTimer) {
    clearTimeout(holdTimer);
    holdTimer = 0;
  }
  if (fallbackTimer) {
    clearTimeout(fallbackTimer);
    fallbackTimer = 0;
  }
  el.classList.remove("surf-open");
  el.classList.add("surf-closed");
}

/** 打开事件里调用：先落到关闭态，等玻璃就绪再入场 */
export function beginSurfaceOpen() {
  const el = backdrop();
  if (!el) return;
  if (reducedMotion()) {
    // 减少动效：直接显示。加载/关闭时预置了 .surf-closed，这里必须清掉
    resetClosed();
    el.classList.remove("surf-closed");
    return;
  }
  resetClosed();
  openPending = true;
  void el.offsetWidth; // 强制重排：连续打开也能从关闭态重新开始
  fallbackTimer = setTimeout(() => {
    fallbackTimer = 0;
    play();
  }, OPEN_FALLBACK_MS);
}

// 玻璃底片补截完成（glass.js 在 .lg-wait 揭示后派发）
window.addEventListener("easydash:surface-ready", play);

// 窗口隐藏 → 回到关闭态（Rust 在 hide() 之后派发）
function bindClosedEvent(triesLeft) {
  let p = null;
  try {
    p = listen("surface-closed", resetClosed);
  } catch (e) {
    p = null;
  }
  if (!p || typeof p.then !== "function") {
    // Tauri 尚未注入：稍后重试（窗口创建后一直隐藏，首次 show 前必然已注入）
    if (triesLeft > 0) setTimeout(() => bindClosedEvent(triesLeft - 1), BIND_RETRY_MS);
    return;
  }
  Promise.resolve(p).catch(() => {
    if (triesLeft > 0) setTimeout(() => bindClosedEvent(triesLeft - 1), BIND_RETRY_MS);
  });
}
bindClosedEvent(BIND_RETRY_MAX);

// 页面加载时窗口是隐藏的：先落到关闭态，避免首次 show 时闪出空白/旧内容。
// （不要在这里按 document.hasFocus() 补播：WebView2 对隐藏窗口的焦点/可见性
//   判断不可靠，误播会重新把窗口留在「打开态」，下次 show 又会闪。）
resetClosed();

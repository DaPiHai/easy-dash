// ============================================================
// 鼠标流光：光标跟随柔光 + 沿边框流动的流光 + 点按光晕
//
// 参照系统：
// - iOS（触感高光 / Liquid Glass）：按下时从落点扩散的柔光（.glow-burst）；
// - 国产安卓（HyperOS / OriginOS / HarmonyOS 的「流光边框」）：液态玻璃下
//   1px 边框光沿边缘缓慢绕行（--gaf 动画），指针方位只决定起始角（--ga）；
// - 两者共用的「光跟手走」聚光：--gx / --gy 跟随指针，带 60–110ms 甩尾。
//
// 用法：页面入口 `import "./cursor-glow.js";` 一次即可（ES Module 单例）。
// 样式见 ui/css/cursor-glow.css。
//
// 实现要点：
// 1) 事件：document 上单个 passive pointermove（rAF 合并）+ pointerdown（点按光晕）；
//    跟随光只响应鼠标（pointerType === "mouse"），点按光晕允许触屏。
// 2) 命中：e.target.closest(HOSTS)，嵌套时取最近的 host（磁贴优先于面板）。
// 3) 装饰层：给 host 懒注入 .glow-spot / .glow-streak（点按光晕按需注入 .glow-burst）；
//    host 被 innerHTML 重建时按需重注入。
// 4) rect 缓存：只在 host 切换、resize、捕获阶段 scroll、pointerdown 与 500ms 心跳时刷新，
//    避免每次 pointermove 都强制布局。
// 5) 开关：读 get_config().pointerGlow（缺省开），并监听 config-changed 同步。
// 6) reduce-motion：写 <html data-glow-motion="off">，CSS 关掉延迟跟随、边框流光与点按光晕。
// ============================================================
import { invokeRetry, listen } from "./api.js";

/** 可承载流光的面（就近命中）。外壳自身的伪元素已被材质层占用，故一律用注入节点。 */
const HOST_SELECTOR = [
  ".topbar",
  ".dock",
  ".dock-item",
  ".panel",
  ".mp-panel",
  ".ctl-tile",
  ".ctl-card",
  ".ctl-action",
  ".ntf-card",
  ".ntf-action",
  ".ac-searchbar",
  ".ac-app",
  ".ac-pager-btn",
  ".mp-card",
  ".mp-row",
  ".mp-btn",
  ".med",
  ".card",
].join(",");

const SPOT_CLASS = "glow-spot";
const STREAK_CLASS = "glow-streak";
const BURST_CLASS = "glow-burst";
/** 点按光晕的最小可点面积（px²）：小按钮用按压反馈就够，铺满式光晕反而像闪白 */
const BURST_MIN_AREA = 2500;
/** rect 缓存心跳（ms）：host 内部滚动/异步布局后兜底校正 */
const RECT_REFRESH_MS = 500;

let enabled = true;
let started = false;
let active = null; // { host, spot }
let pending = null; // { x, y }
let raf = 0;
let rect = null; // { left, top, width, height, cx, cy }
let rectAt = 0;
let rectTimer = 0;
let settleTimers = [];
let lastX = -1;
let lastY = -1;

// ---------- rect 缓存 ----------

function refreshRect() {
  if (!active) {
    rect = null;
    return;
  }
  const r = active.host.getBoundingClientRect();
  rect = {
    left: r.left,
    top: r.top,
    width: r.width,
    height: r.height,
    cx: r.left + r.width / 2,
    cy: r.top + r.height / 2,
  };
  rectAt = performance.now();
}

/** host 切换 / 滚动 / 缩放时刷新，并挂一个心跳避免长驻过期 */
function scheduleRectRefresh() {
  refreshRect();
  if (rectTimer) clearTimeout(rectTimer);
  rectTimer = setTimeout(() => {
    rectTimer = 0;
    refreshRect();
  }, RECT_REFRESH_MS);
}

function clearSettleTimers() {
  for (const t of settleTimers) clearTimeout(t);
  settleTimers = [];
}

/** 面板/抽屉入场有 240ms 动画：激活后再补两次校正，避免光点落位在动画中途 */
function scheduleSettle() {
  clearSettleTimers();
  const relayout = () => {
    refreshRect();
    if (active && lastX >= 0) writeSpot(active, lastX, lastY);
  };
  settleTimers.push(setTimeout(relayout, 130));
  settleTimers.push(setTimeout(relayout, 320));
}

// ---------- 光层 ----------

/**
 * 注入（或复用）两层装饰：
 * - .glow-spot   跟随指针的柔光（opacity 由 --glow-strength 控制）
 * - .glow-streak 液态玻璃的边框流光（必须是兄弟节点：父级 opacity 会把子元素压暗）
 */
function ensureLayers(host) {
  let spot = host.querySelector(":scope > ." + SPOT_CLASS);
  if (!spot) {
    spot = document.createElement("span");
    spot.className = SPOT_CLASS;
    spot.setAttribute("aria-hidden", "true");
    host.appendChild(spot);
  }
  let streak = host.querySelector(":scope > ." + STREAK_CLASS);
  if (!streak) {
    streak = document.createElement("span");
    streak.className = STREAK_CLASS;
    streak.setAttribute("aria-hidden", "true");
    host.appendChild(streak);
  }
  return { spot, streak };
}

function writeSpot(layers, clientX, clientY) {
  if (!rect) return;
  const { spot, streak } = layers;
  spot.style.setProperty("--gx", (clientX - rect.left).toFixed(1) + "px");
  spot.style.setProperty("--gy", (clientY - rect.top).toFixed(1) + "px");
  const deg = (Math.atan2(clientY - rect.cy, clientX - rect.cx) * 180) / Math.PI + 90;
  const angle = deg.toFixed(1) + "deg";
  spot.style.setProperty("--ga", angle);
  streak.style.setProperty("--ga", angle);
}

function activate(host) {
  const layers = ensureLayers(host);
  const { spot } = layers;
  host.classList.add("glow-host");
  spot.classList.add("glow-instant");
  // 必须先切 active 再量 rect：否则量到的是上一个 host 的几何，光点会偏
  active = { host, spot, streak: layers.streak };
  scheduleRectRefresh();
  if (lastX >= 0) writeSpot(layers, lastX, lastY);
  void spot.offsetWidth; // 强制一帧落位，再恢复跟随过渡
  spot.classList.remove("glow-instant");
  host.classList.add("glow-on");
  scheduleSettle();
}

function deactivate() {
  if (active) active.host.classList.remove("glow-on");
  active = null;
  rect = null;
  pending = null;
  if (raf) {
    cancelAnimationFrame(raf);
    raf = 0;
  }
  if (rectTimer) {
    clearTimeout(rectTimer);
    rectTimer = 0;
  }
  clearSettleTimers();
}

function update(clientX, clientY) {
  if (!active || !active.spot) return;
  if (!rect || performance.now() - rectAt > RECT_REFRESH_MS * 2) refreshRect();
  if (!rect) return;
  writeSpot(active, clientX, clientY);
}

function onFrame() {
  raf = 0;
  const p = pending;
  pending = null;
  if (p) update(p.x, p.y);
}

// ---------- 事件 ----------

function onPointerMove(e) {
  if (!enabled) return;
  if (e.pointerType && e.pointerType !== "mouse") return;
  lastX = e.clientX;
  lastY = e.clientY;

  // host 被重渲染（Dock / 应用中心 / 通知列表）后旧节点已脱离文档
  if (active && !active.host.isConnected) deactivate();

  const host = e.target && typeof e.target.closest === "function" ? e.target.closest(HOST_SELECTOR) : null;
  if (host !== (active ? active.host : null)) {
    if (!host) {
      deactivate();
      return;
    }
    if (active) active.host.classList.remove("glow-on");
    activate(host);
  }
  pending = { x: e.clientX, y: e.clientY };
  if (!raf) raf = requestAnimationFrame(onFrame);
}

// ---------- 点按光晕（iOS 触感高光 / 安卓 ripple 的柔光版） ----------

/**
 * 在按下点展开一圈柔光（一次性 540ms）。
 * 只给面积足够大的 host 播（卡片 / 磁贴 / 外壳），小按钮靠 motion.css 的按压缩放反馈。
 */
function triggerBurst(host, clientX, clientY) {
  if (!host || !host.isConnected) return;
  const r = host.getBoundingClientRect();
  if (!r.width || !r.height) return;
  if (r.width * r.height < BURST_MIN_AREA) return;
  let burst = host.querySelector(":scope > ." + BURST_CLASS);
  if (!burst) {
    burst = document.createElement("span");
    burst.className = BURST_CLASS;
    burst.setAttribute("aria-hidden", "true");
    host.appendChild(burst);
  }
  burst.style.setProperty("--bx", (clientX - r.left).toFixed(1) + "px");
  burst.style.setProperty("--by", (clientY - r.top).toFixed(1) + "px");
  burst.classList.remove("play");
  void burst.offsetWidth; // 强制重排，连点也能重播
  burst.classList.add("play");
  if (burst._burstTimer) clearTimeout(burst._burstTimer);
  burst._burstTimer = setTimeout(() => {
    burst._burstTimer = null;
    burst.classList.remove("play");
  }, 600);
}

function bindEvents() {
  if (started) return;
  started = true;

  document.addEventListener("pointermove", onPointerMove, { passive: true });
  document.addEventListener("pointerleave", deactivate, { passive: true });
  document.addEventListener(
    "pointerdown",
    (e) => {
      if (!enabled) return;
      if (e.button !== 0) return; // 只有主键/触摸产生点按光晕，右键菜单不闪
      if (active) refreshRect();
      const host =
        e.target && typeof e.target.closest === "function" ? e.target.closest(HOST_SELECTOR) : null;
      if (host) triggerBurst(host, e.clientX, e.clientY);
    },
    { passive: true, capture: true }
  );
  document.addEventListener(
    "scroll",
    () => {
      if (active) refreshRect();
    },
    { passive: true, capture: true }
  );
  window.addEventListener("resize", () => {
    if (active) scheduleRectRefresh();
  });
  window.addEventListener("blur", deactivate);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState !== "visible") deactivate();
  });
}

// ---------- 开关（config.pointerGlow，默认开） ----------

function setEnabled(next) {
  const value = next !== false;
  enabled = value;
  if (!value) deactivate();
}

async function loadPref() {
  try {
    const cfg = await invokeRetry("get_config", undefined, {
      attempts: 3,
      perTryMs: 2500,
      gapMs: 400,
    });
    if (cfg && typeof cfg === "object") setEnabled(cfg.pointerGlow !== false);
  } catch (e) {
    // 读不到配置时保持默认开启，不影响主体功能
  }
}

function listenPref() {
  try {
    const p = listen("config-changed", (e) => {
      if (!e || !e.payload || typeof e.payload !== "object") return;
      if (!("pointerGlow" in e.payload)) return;
      setEnabled(e.payload.pointerGlow !== false);
    });
    if (p && typeof p.catch === "function") p.catch(() => {});
  } catch (e) {
    /* Tauri 尚未注入时忽略：loadPref 会兜底 */
  }
}

// ---------- 减少动效 ----------

function applyMotionPref() {
  const root = document.documentElement;
  let reduce = false;
  try {
    reduce = !!(window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches);
  } catch (e) {
    reduce = false;
  }
  if (reduce) root.setAttribute("data-glow-motion", "off");
  else root.removeAttribute("data-glow-motion");
}

function bindMotionPref() {
  applyMotionPref();
  try {
    const mq = window.matchMedia("(prefers-reduced-motion: reduce)");
    if (mq && typeof mq.addEventListener === "function") mq.addEventListener("change", applyMotionPref);
  } catch (e) {
    /* 老内核忽略 */
  }
}

// ---------- 启动 ----------

bindMotionPref();
bindEvents();
listenPref();
loadPref();

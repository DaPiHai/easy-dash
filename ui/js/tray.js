// 顶栏任务托盘镜像：渲染系统托盘图标、hover 提示、左/右键转发到真实托盘。
// 数据来自 get_tray_items()；后端刷新完成后会发 tray-changed（payload 为数组）。
import { invoke, invokeRetry, listen, escapeHtml } from "./api.js";

/** 托盘刷新间隔（毫秒） */
const REFRESH_MS = 10000;

const LETTER_COLORS = ["#3584e4", "#e66100", "#2ec27e", "#9141ac", "#e01b24", "#1c71d8", "#8f4e2b"];

let trayEl = null;
let tooltipEl = null;
let items = [];
let started = false;
let timer = null;
let domBound = false;
let listenBound = false;
let fetchSeq = 0;

/** 启动托盘镜像（幂等）：首刷 + 每 10s 刷新 */
export function startTray() {
  if (started) return;
  started = true;
  trayEl = document.getElementById("tray");
  tooltipEl = document.getElementById("tooltip");
  if (!trayEl) return;

  bindDomOnce();
  bindListenOnce();

  // 启动时请求一次后端枚举，随后读取缓存
  requestBackendRefresh();
  load(true);

  timer = setInterval(() => {
    if (!started) return;
    requestBackendRefresh();
    load(false);
  }, REFRESH_MS);
}

/** 停止托盘镜像（配置关闭时调用）：清空界面与定时器 */
export function stopTray() {
  started = false;
  if (timer) {
    clearInterval(timer);
    timer = null;
  }
  items = [];
  fetchSeq++;
  hideTooltip();
  if (trayEl) trayEl.innerHTML = "";
}

// ---------- 数据加载 ----------

async function load(first) {
  if (!trayEl || !started) return;
  const seq = ++fetchSeq;
  try {
    const list = first
      ? await invokeRetry("get_tray_items", undefined, { attempts: 3, perTryMs: 3000 })
      : await invoke("get_tray_items");
    if (!started || seq !== fetchSeq) return; // 过期结果丢弃
    applyItems(list);
  } catch (e) {
    // 托盘镜像不可用（Win10 / 任务栏隐藏 / 非 Win11 XAML 任务栏）时静默
  }
}

function applyItems(list) {
  const next = Array.isArray(list) ? list : [];
  if (sameItems(next, items)) return; // 内容没变就不重绘，避免 hover 闪烁
  items = next;
  render();
}

function sameItems(a, b) {
  if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    const x = a[i] || {};
    const y = b[i] || {};
    if (x.name !== y.name || x.kind !== y.kind || x.icon !== y.icon) return false;
  }
  return true;
}

function render() {
  if (!trayEl) return;
  trayEl.innerHTML = items.map(itemHtml).join("");
  markDarkIcons();
}

/**
 * 深色图标在深色顶栏上几乎不可见（Docker 黑鲸鱼、抖音黑底、QQ 深色企鹅等）。
 * 用 canvas 采样图标的 alpha 加权平均亮度，偏暗的加 .tray-dark 类，
 * CSS 会给它垫一层浅色圆角底。data URL 同源，canvas 不会被污染。
 */
function markDarkIcons() {
  if (!trayEl) return;
  const imgs = trayEl.querySelectorAll("img");
  for (const img of imgs) {
    if (img.dataset.darkChecked) continue;
    img.dataset.darkChecked = "1";
    const analyze = () => {
      try {
        const canvas = document.createElement("canvas");
        canvas.width = 16;
        canvas.height = 16;
        const ctx = canvas.getContext("2d", { willReadFrequently: true });
        if (!ctx) return;
        ctx.clearRect(0, 0, 16, 16);
        ctx.drawImage(img, 0, 0, 16, 16);
        const data = ctx.getImageData(0, 0, 16, 16).data;
        let sum = 0;
        let weight = 0;
        for (let i = 0; i < data.length; i += 4) {
          const a = data[i + 3];
          if (a < 40) continue; // 透明padding不计入
          const lum = 0.2126 * data[i] + 0.7152 * data[i + 1] + 0.0722 * data[i + 2];
          sum += lum * a;
          weight += a;
        }
        if (weight > 0 && sum / weight < 85) {
          img.classList.add("tray-dark");
        }
      } catch (e) {
        /* canvas 不可用时保持原样 */
      }
    };
    if (img.complete && img.naturalWidth > 0) analyze();
    else img.addEventListener("load", analyze, { once: true });
  }
}

function itemHtml(item, index) {
  const it = item || {};
  const name = String(it.name || "托盘项");
  let inner;
  if (it.kind === "overflow") {
    // 「显示隐藏的图标」按钮
    inner = '<span class="tray-overflow">»</span>';
  } else if (it.icon) {
    inner = '<img src="' + escapeHtml(it.icon) + '" alt="">';
  } else {
    // 无图标时用首字母占位
    const letter = escapeHtml((name.slice(0, 1) || "?").toUpperCase());
    inner =
      '<span class="tray-letter" style="background:' +
      LETTER_COLORS[index % LETTER_COLORS.length] +
      '">' +
      letter +
      "</span>";
  }
  // title 提供系统级提示（窗口常态仅 32px 高，自绘 tooltip 放不下时用它）
  return (
    '<button class="tray-item" type="button" data-index="' +
    index +
    '" title="' +
    escapeHtml(name) +
    '" aria-label="' +
    escapeHtml(name) +
    '">' +
    inner +
    "</button>"
  );
}

function requestBackendRefresh() {
  try {
    invoke("refresh_tray").catch(() => {});
  } catch (e) {
    /* Tauri 尚未注入时忽略 */
  }
}

// ---------- 事件绑定（委托，图标重绘不需要重新绑定） ----------

function bindDomOnce() {
  if (domBound) return;
  domBound = true;

  trayEl.addEventListener("click", (e) => {
    const el = e.target.closest ? e.target.closest(".tray-item") : null;
    if (!el) return;
    clickTray(el, "left");
  });

  trayEl.addEventListener("contextmenu", (e) => {
    const el = e.target.closest ? e.target.closest(".tray-item") : null;
    if (!el) return;
    e.preventDefault();
    clickTray(el, "right");
  });

  trayEl.addEventListener("mouseover", (e) => {
    const el = e.target.closest ? e.target.closest(".tray-item") : null;
    if (el) showTooltip(el);
  });
  trayEl.addEventListener("mouseleave", hideTooltip);
}

function bindListenOnce() {
  if (listenBound) return;
  listenBound = true;
  try {
    const p = listen("tray-changed", (e) => {
      if (!started) return;
      if (e && Array.isArray(e.payload)) applyItems(e.payload);
      else load(false);
    });
    if (p && typeof p.catch === "function") p.catch(() => {});
  } catch (e) {
    /* Tauri 尚未注入时忽略 */
  }
}

function clickTray(el, button) {
  const index = Number(el.dataset.index);
  if (!Number.isInteger(index) || index < 0) return;
  hideTooltip();
  // 弹窗锚点 = 顶栏图标中心（顶栏窗口固定在屏幕 (0,0)，client 坐标即屏幕坐标）
  let anchorX = null;
  try {
    const r = el.getBoundingClientRect();
    anchorX = Math.round(r.left + r.width / 2);
  } catch (e) {
    anchorX = null;
  }
  try {
    invoke("tray_click", { index, button, anchorX }).catch(() => {});
  } catch (e) {
    /* 忽略 */
  }
}

// ---------- hover 提示（复用 dock 的 tooltip 样式） ----------

function showTooltip(el) {
  if (!tooltipEl) return;
  // 顶栏窗口常态只有 32px 高，自绘提示会被窗口裁掉；此时依赖 title 的原生提示
  if (window.innerHeight < 80) {
    hideTooltip();
    return;
  }
  const item = items[Number(el.dataset.index)];
  const text = (item && item.name) || el.getAttribute("title") || "";
  if (!text) return;
  const r = el.getBoundingClientRect();
  tooltipEl.textContent = text;
  tooltipEl.classList.remove("hidden");
  const w = tooltipEl.offsetWidth;
  // 水平方向夹在窗口内，垂直方向放在图标下沿
  const left = Math.max(w / 2 + 4, Math.min(window.innerWidth - w / 2 - 4, r.left + r.width / 2));
  tooltipEl.style.left = Math.round(left) + "px";
  tooltipEl.style.top = Math.round(r.bottom + 6) + "px";
}

function hideTooltip() {
  if (tooltipEl) tooltipEl.classList.add("hidden");
}

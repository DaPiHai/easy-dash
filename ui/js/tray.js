// 顶栏任务托盘镜像：渲染系统托盘图标、hover 提示、左/右键转发到真实托盘，
// 以及图标拖拽排序（Dock 同款「位置预览」：源项半透明留在新位置、ghost 跟手、实时让位）。
// 数据来自 get_tray_items()；后端刷新完成后会发 tray-changed（payload 为数组）。
import { invoke, invokeRetry, listen, escapeHtml } from "./api.js";
import { subscribeTheme, resolvedTheme } from "./theme.js";

/** 兜底刷新间隔（毫秒）：正常情况下由 tray-changed 事件驱动，不再轮询 */
const FALLBACK_MS = 30000;
/** 拖拽判定阈值（像素） */
const DRAG_THRESHOLD = 5;
/** 拖拽结束后抑制 click 的时长（毫秒），避免松手被当成一次托盘点击 */
const CLICK_SUPPRESS_MS = 300;
/** 本地顺序锁定时长：刚提交 set_tray_order 后，避免旧顺序的快照立刻把界面顶回去
 *  （后端要保存配置 + 重新枚举托盘，留足 3s 更稳） */
const ORDER_LOCK_MS = 3000;

const LETTER_COLORS = ["#3584e4", "#e66100", "#2ec27e", "#9141ac", "#e01b24", "#1c71d8", "#8f4e2b"];

let trayEl = null;
let tooltipEl = null;
let items = [];
let started = false;
let timer = null;
let domBound = false;
let listenBound = false;
let themeBound = false;
let fetchSeq = 0;

// ---------- 拖拽状态 ----------
let dragState = null; // { key, el, pointerId, startX, startY, moved, ghost, grabX, grabY }
let pendingItems = null; // 拖拽期间到达的最新列表（拖拽结束后再应用）
let orderLock = null; // { keys: string[], until: number }
let suppressClickUntil = 0;

/** 稳定排序 key：优先用后端 orderKey，旧数据回退 name/index */
function keyOf(item, index) {
  const it = item || {};
  if (it.orderKey) return String(it.orderKey);
  return String(it.name || "item") + "#" + index;
}

/** 启动托盘镜像（幂等）：事件驱动；30s 兜底读缓存，防止极端情况下漏事件 */
export function startTray() {
  if (started) return;
  started = true;
  trayEl = document.getElementById("tray");
  tooltipEl = document.getElementById("tooltip");
  if (!trayEl) return;

  bindDomOnce();
  bindListenOnce();
  bindThemeOnce();

  // 启动时读取缓存；之后由 tray-changed 事件驱动（代理启动时已广播 TaskbarCreated）
  load(true);

  timer = setInterval(() => {
    if (!started) return;
    load(false);
  }, FALLBACK_MS);
}

/** 停止托盘镜像（配置关闭时调用）：清空界面与定时器 */
export function stopTray() {
  started = false;
  if (timer) {
    clearInterval(timer);
    timer = null;
  }
  cleanupDrag();
  pendingItems = null;
  orderLock = null;
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
  // 拖拽期间不重绘：只记住最新列表，松手后统一应用，避免 10s 轮询把拖拽中的 DOM 换掉
  if (dragState) {
    pendingItems = next;
    return;
  }
  const ordered = applyOrderLock(next);
  if (sameItems(ordered, items)) return; // 内容没变就不重绘，避免 hover 闪烁
  items = ordered;
  render();
}

/** 刚提交排序后的一小段时间内，用本地顺序重排后端快照，避免视觉回跳 */
function applyOrderLock(list) {
  if (!orderLock) return list;
  if (Date.now() > orderLock.until) {
    orderLock = null;
    return list;
  }
  const arr = list.slice();
  const iconSlots = [];
  const icons = [];
  arr.forEach((it, i) => {
    if (it && it.kind === "icon") {
      iconSlots.push(i);
      icons.push(it);
    }
  });
  if (!icons.length) return arr;

  const byKey = new Map();
  icons.forEach((it, i) => byKey.set(keyOf(it, i), it));
  const ordered = [];
  for (const k of orderLock.keys) {
    const it = byKey.get(k);
    if (it) {
      ordered.push(it);
      byKey.delete(k);
    }
  }
  for (const it of byKey.values()) ordered.push(it);
  if (ordered.length !== icons.length) return arr; // 数据不一致，放弃锁定

  let k = 0;
  for (const slot of iconSlots) arr[slot] = ordered[k++];
  return arr;
}

function sameItems(a, b) {
  if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    const x = a[i] || {};
    const y = b[i] || {};
    if (x.name !== y.name || x.kind !== y.kind || x.icon !== y.icon) return false;
    if (x.orderKey !== undefined && y.orderKey !== undefined && x.orderKey !== y.orderKey) return false;
  }
  return true;
}

function render() {
  if (!trayEl) return;
  trayEl.innerHTML = items.map(itemHtml).join("");
  markTrayIcons();
}

/** 深色主题下“近黑”的亮度阈值（0–255，alpha 加权平均） */
const TRAY_DARK_LUM = 85;

/**
 * 托盘图标随主题做可见性修正：
 * - 深色主题：近黑图标在深色顶栏上几乎不可见（Docker 黑鲸鱼、抖音黑底、QQ 深色企鹅等），
 *   加 .tray-dark → CSS 垫一层浅色圆角底；
 * - 浅色主题：近白图标在浅色顶栏上看不见，加 .tray-invert → CSS filter brightness(.35)。
 * dataset.trayTheme 记录上次分析时的主题，主题没变就跳过重采样（render 频繁调用也不心疼）。
 * data URL 同源，canvas 不会被污染。
 */
function markTrayIcons() {
  if (!trayEl) return;
  const theme = resolvedTheme() === "light" ? "light" : "dark";
  const imgs = trayEl.querySelectorAll("img");
  for (const img of imgs) {
    if (img.dataset.trayTheme === theme) continue;
    img.dataset.trayTheme = theme;
    analyzeIcon(img, theme);
  }
}

/**
 * 主题切换后强制重算全部图标：清掉两类修正类和 dataset 缓存再重新分析，
 * 避免「上次分析时的 theme 缓存命中 → 不再重算」或 img.complete 时序导致的类残留。
 */
function resetTrayIcons() {
  if (!trayEl) return;
  for (const img of trayEl.querySelectorAll("img")) {
    img.classList.remove("tray-dark", "tray-invert");
    delete img.dataset.trayTheme;
  }
  markTrayIcons();
}

function analyzeIcon(img, theme) {
  const apply = () => {
    // 浅色主题不再按亮度给整张图加 filter：很多应用图标是白底/浅底方形，
    // brightness 会把整块底压成半透明黑，反而挡住图标。浅色下只用 CSS
    // 的 drop-shadow 给白色透明图标补轮廓。
    if (theme !== "dark") {
      img.classList.remove("tray-dark", "tray-invert");
      return;
    }
    const lum = averageIconLuminance(img);
    if (lum === null) return;
    img.classList.toggle("tray-dark", lum < TRAY_DARK_LUM);
    img.classList.remove("tray-invert");
  };
  if (img.complete && img.naturalWidth > 0) apply();
  else img.addEventListener("load", apply, { once: true });
}

/** 采样 16×16 图标，返回 alpha 加权平均亮度；canvas 不可用时返回 null */
function averageIconLuminance(img) {
  try {
    const canvas = document.createElement("canvas");
    canvas.width = 16;
    canvas.height = 16;
    const ctx = canvas.getContext("2d", { willReadFrequently: true });
    if (!ctx) return null;
    ctx.clearRect(0, 0, 16, 16);
    ctx.drawImage(img, 0, 0, 16, 16);
    const data = ctx.getImageData(0, 0, 16, 16).data;
    let sum = 0;
    let weight = 0;
    for (let i = 0; i < data.length; i += 4) {
      const a = data[i + 3];
      if (a < 40) continue; // 透明 padding 不计入
      const lum = 0.2126 * data[i] + 0.7152 * data[i + 1] + 0.0722 * data[i + 2];
      sum += lum * a;
      weight += a;
    }
    return weight > 0 ? sum / weight : null;
  } catch (e) {
    return null; // canvas 不可用时保持原样
  }
}

function itemHtml(item, index) {
  const it = item || {};
  const name = String(it.name || "托盘项");
  const kind = it.kind === "overflow" ? "overflow" : "icon";
  let inner;
  if (kind === "overflow") {
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
    '" data-kind="' +
    kind +
    '" data-key="' +
    escapeHtml(keyOf(it, index)) +
    '" title="' +
    escapeHtml(name) +
    '" aria-label="' +
    escapeHtml(name) +
    '">' +
    inner +
    "</button>"
  );
}

// ---------- 事件绑定（委托，图标重绘不需要重新绑定） ----------

function bindDomOnce() {
  if (domBound) return;
  domBound = true;

  trayEl.addEventListener("click", (e) => {
    // 刚拖拽过：松手产生的 click 不能当成托盘点击（只抑制一次）
    if (suppressClickUntil) {
      const now = Date.now();
      if (now < suppressClickUntil) {
        suppressClickUntil = 0;
        e.preventDefault();
        e.stopPropagation();
        return;
      }
      suppressClickUntil = 0;
    }
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

  // ---- 拖拽排序（pointer 事件；只对 kind=icon 生效） ----
  trayEl.addEventListener("pointerdown", onPointerDown);
  window.addEventListener("pointermove", onPointerMove);
  window.addEventListener("pointerup", onPointerUp);
  window.addEventListener("pointercancel", onPointerCancel);
  window.addEventListener("blur", onPointerCancel);
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

function bindThemeOnce() {
  if (themeBound) return;
  themeBound = true;
  subscribeTheme(() => {
    // 主题切换后强制重算：先清缓存/旧类，再按新主题重新采样（防止 tray-dark/tray-invert 残留）
    if (started) resetTrayIcons();
  });
}

function clickTray(el, button) {
  const index = Number(el.dataset.index);
  if (!Number.isInteger(index) || index < 0) {
    showTrayClickError("托盘项索引无效，请稍后重试");
    return;
  }
  hideTooltip();
  // 弹窗锚点 = 顶栏图标中心（CSS 客户区坐标）；后端用 ClientToScreen + 该窗口 DPI
  // 换算为屏幕物理坐标，副屏/混合 DPI 下不再假设窗口在 (0,0)。
  let anchorX = null;
  let anchorY = null;
  try {
    const r = el.getBoundingClientRect();
    anchorX = Math.round(r.left + r.width / 2);
    anchorY = Math.round(r.top + r.height / 2);
  } catch (e) {
    anchorX = null;
    anchorY = null;
  }
  // 带上稳定 key：托盘项增减会让后端缓存索引漂移，后端按 key 兜底
  const key = el.dataset.key || null;
  try {
    invoke("tray_click", { index, button, anchorX, anchorY, key }).catch((e) =>
      showTrayClickError(e)
    );
  } catch (e) {
    showTrayClickError(e);
  }
}

/** 托盘点击失败时在图标附近短暂提示，避免"点了完全没反应"且无从排查 */
function showTrayClickError(e) {
  const msg =
    (typeof e === "string" ? e : (e && e.message) || "") || "托盘点击失败";
  if (!tooltipEl) return;
  tooltipEl.textContent = msg;
  tooltipEl.classList.remove("hidden");
  clearTimeout(showTrayClickError._timer);
  showTrayClickError._timer = setTimeout(() => hideTooltip(), 2400);
}

// ---------- 拖拽排序（Dock 同款位置预览） ----------

/** 当前 DOM 里的图标元素（document 顺序，不含 overflow） */
function iconElements() {
  if (!trayEl) return [];
  return Array.from(trayEl.querySelectorAll('.tray-item[data-kind="icon"]'));
}

function onPointerDown(e) {
  if (e.button !== 0) return; // 只处理左键；右键留给 contextmenu
  if (dragState) return;
  const el = e.target.closest ? e.target.closest(".tray-item") : null;
  if (!el) return;
  if (el.dataset.kind !== "icon") return; // overflow 固定不拖
  const key = el.dataset.key || "";
  if (!key) return;
  // 只有一个图标时没有排序意义
  if (iconElements().length < 2) return;

  const r = el.getBoundingClientRect();
  dragState = {
    key,
    el,
    pointerId: e.pointerId,
    startX: e.clientX,
    startY: e.clientY,
    moved: false,
    ghost: null,
    grabX: e.clientX - r.left,
    grabY: e.clientY - r.top,
  };
  // 捕获指针：拖出元素/窗口边缘也能继续收到 move/up
  try {
    if (el.setPointerCapture) el.setPointerCapture(e.pointerId);
  } catch (err) {
    /* 忽略 */
  }
}

function onPointerMove(e) {
  const d = dragState;
  if (!d) return;
  if (!d.moved) {
    if (Math.hypot(e.clientX - d.startX, e.clientY - d.startY) < DRAG_THRESHOLD) return;
    beginDrag();
  }
  updateGhost(e.clientX, e.clientY);
  updateLiveOrder(e.clientX);
}

/** 进入拖拽：克隆一个跟手的 ghost，源项半透明留在原位（之后实时让位） */
function beginDrag() {
  const d = dragState;
  d.moved = true;
  hideTooltip();

  const r = d.el.getBoundingClientRect();
  const ghost = d.el.cloneNode(true);
  ghost.className = d.el.className + " drag-ghost";
  ghost.style.cssText =
    "position:fixed;pointer-events:none;z-index:90;margin:0;" +
    "left:" + r.left + "px;top:" + r.top + "px;" +
    "width:" + r.width + "px;height:" + r.height + "px;";
  document.body.appendChild(ghost);
  d.ghost = ghost;

  d.el.classList.add("drag-src");
  document.body.classList.add("tray-dragging");
}

function updateGhost(x, y) {
  const d = dragState;
  if (!d || !d.ghost) return;
  d.ghost.style.left = x - d.grabX + "px";
  d.ghost.style.top = y - d.grabY + "px";
}

/**
 * 实时让位：按指针 x 在所有图标中点之间找插入位置，直接 insertBefore。
 * overflow 永远不被移动（它始终是最前/固定项），因此不会被插到前面。
 */
function updateLiveOrder(x) {
  const d = dragState;
  if (!d || !d.moved) return;
  const icons = iconElements();
  if (icons.length < 2) return;

  // 找第一个中心 x 大于指针的图标作为插入参考
  let ref = null;
  for (const el of icons) {
    if (el === d.el) continue;
    const r = el.getBoundingClientRect();
    if (x < r.left + r.width / 2) {
      ref = el;
      break;
    }
  }

  if (ref) {
    if (d.el.nextElementSibling !== ref) trayEl.insertBefore(d.el, ref);
    return;
  }

  // 指针在所有图标右侧 → 放到最后一个图标之后（overflow 若在其后则插到它前面）
  const others = icons.filter((el) => el !== d.el);
  const last = others[others.length - 1];
  if (!last) return;
  const after = last.nextElementSibling;
  if (after === d.el) return;
  if (after) trayEl.insertBefore(d.el, after);
  else trayEl.appendChild(d.el);
}

function onPointerUp(e) {
  const d = dragState;
  if (!d) return;

  if (!d.moved) {
    // 只是点击，没有拖动：清理状态，什么都不做（click 会正常转发）
    cleanupDrag();
    applyPending(); // 期间积攒的列表照常应用
    return;
  }

  // 按 DOM 顺序收集 icon 的 orderKey（overflow 不在其中）
  const domOrder = iconElements().map((el) => el.dataset.key || "").filter(Boolean);

  cleanupDrag();
  // 只要真的拖动过，松手产生的 click 都要抑制一次，避免被当成托盘点击
  suppressClickUntil = Date.now() + CLICK_SUPPRESS_MS;

  const changed = reorderItemsByKeys(domOrder);
  if (changed) {
    render();
    commitOrder();
  }
  applyPending();
}

function onPointerCancel() {
  if (!dragState) return;
  const d = dragState;
  cleanupDrag();
  if (d.moved) render(); // 拖拽只动了 DOM，render 即恢复本地 items 的原顺序
  applyPending();
}

/** 清理拖拽的视觉/状态（不提交顺序） */
function cleanupDrag() {
  const d = dragState;
  dragState = null;
  if (d) {
    if (d.ghost && d.ghost.parentNode) d.ghost.parentNode.removeChild(d.ghost);
    if (d.el) {
      d.el.classList.remove("drag-src");
      try {
        if (d.el.releasePointerCapture && d.pointerId !== undefined) {
          d.el.releasePointerCapture(d.pointerId);
        }
      } catch (err) {
        /* 指针已释放时忽略 */
      }
    }
  }
  document.body.classList.remove("tray-dragging");
}

/** 拖拽期间积攒的最新列表，拖拽结束后应用 */
function applyPending() {
  if (!pendingItems) return;
  const next = pendingItems;
  pendingItems = null;
  applyItems(next);
}

/**
 * 把本地 items 的 icon 顺序改成 keys（DOM 里的新顺序）。
 * overflow 项保持它原来的槽位，不参与排序。
 */
function reorderItemsByKeys(keys) {
  if (!Array.isArray(keys) || !keys.length) return false;

  const iconSlots = [];
  const icons = [];
  items.forEach((it, i) => {
    if (it && it.kind === "icon") {
      iconSlots.push(i);
      icons.push({ item: it, key: keyOf(it, i) });
    }
  });
  if (icons.length !== keys.length) return false;

  const byKey = new Map(icons.map((x) => [x.key, x.item]));
  const ordered = [];
  for (const k of keys) {
    const it = byKey.get(k);
    if (!it) return false;
    ordered.push(it);
    byKey.delete(k);
  }

  const next = items.slice();
  let n = 0;
  for (const slot of iconSlots) next[slot] = ordered[n++];
  items = next;
  return true;
}

/** 把当前 icon 顺序提交给后端，并短暂锁定本地顺序 */
function commitOrder() {
  const order = [];
  items.forEach((it, i) => {
    if (it && it.kind === "icon") order.push(keyOf(it, i));
  });
  orderLock = { keys: order, until: Date.now() + ORDER_LOCK_MS };
  try {
    invoke("set_tray_order", { order }).catch(() => {});
  } catch (e) {
    /* Tauri 尚未注入时忽略 */
  }
}

// ---------- hover 提示（复用 dock 的 tooltip 样式） ----------

function showTooltip(el) {
  if (!tooltipEl) return;
  if (dragState && dragState.moved) return; // 拖拽中不弹提示
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

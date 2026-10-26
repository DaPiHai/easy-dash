// 应用中心（全屏应用网格）：搜索 / 键盘导航 / 启动 / 拖拽排序 / 导入应用。
//
// 关闭时机（重要）：只在 Esc、点击 #backdrop 空白、启动成功（后端自动隐藏）时关闭。
// **不监听 window.blur** —— 应用中心打开时顶栏/Dock 在其上层（后端会把它们重新置顶），
// 点 Dock 九点格会先让本窗口失焦；若 blur 抢先 close，随后 Dock 的 toggle_app_center
// 又会重新打开，表现为「关不掉」。其它页面切换（活动概览/顶栏活动）由后端统一处理。
import { invoke, invokeRetry, listen, escapeHtml } from "./api.js";

// 与 Dock 一致的字母占位配色
const LETTER_COLORS = ["#3584e4", "#e66100", "#2ec27e", "#9141ac", "#e01b24", "#1c71d8", "#8f4e2b"];

/** 拖拽后抑制一次 click 的时长（毫秒），避免拖完松手把卡片当成一次启动 */
const DRAG_CLICK_SUPPRESS_MS = 300;
/** 拖拽判定阈值（像素）：超过才算拖动，否则按点击处理 */
const APP_DRAG_THRESHOLD = 6;
/** 自动滚动：边缘感应区 & 步长 */
const AUTO_SCROLL_ZONE = 60;
const AUTO_SCROLL_STEP = 18;

let apps = []; // 全量应用（后端顺序，拖拽排序改的就是它）
let filtered = []; // 当前搜索结果
let selectedIndex = -1; // 键盘选中项（filtered 下标）
let query = ""; // 当前搜索词（小写、已 trim）
let loading = false; // 首次/重试加载中
let loadFailed = false; // 最近一次加载失败
let refreshing = false; // 正在请求后端刷新
let launching = false; // 启动中（防重复触发）
let refreshTimer = null; // 刷新兜底恢复
let errorTimer = null; // 启动失败提示
let listening = false; // 事件监听是否已注册

// ---------- 拖拽排序状态 ----------
let dragState = null; // { el, id, pointerId, startX, startY, lastX, lastY, lastMoveAt, moved, ghost, grabX, grabY }
let dragClickGuard = 0; // 拖拽结束后抑制 click 的截止时间
let autoScrollTimer = null; // 边缘自动滚动定时器
let autoScrollDir = 0; // -1 上 / 1 下 / 0 停
let pendingAppsReload = false; // 拖拽期间收到 apps-changed，结束后补一次刷新

// ---------- 导入 / 右键菜单状态 ----------
let importing = false; // 导入流程进行中
let contextApp = null; // 右键菜单对应的应用
let menuOpen = false; // 右键菜单是否展开
let toastTimer = null; // 提示自动消失

const backdropEl = document.getElementById("backdrop");
const scrollEl = document.getElementById("ac-scroll");
const gridEl = document.getElementById("ac-grid");
const statusEl = document.getElementById("ac-status");
const searchEl = document.getElementById("ac-search");
const countEl = document.getElementById("ac-count");
const refreshBtn = document.getElementById("ac-refresh");
const refreshLabel = document.getElementById("ac-refresh-label");
const errorEl = document.getElementById("ac-error");
const importBtn = document.getElementById("ac-import");
const importLabel = document.getElementById("ac-import-label");
const toastEl = document.getElementById("ac-toast");
const menuEl = document.getElementById("ac-menu");
const dragHintEl = document.getElementById("ac-drag-hint");

// ---------- 小工具 ----------

function errText(e) {
  if (typeof e === "string") return e;
  if (e && typeof e.message === "string") return e.message;
  try {
    return String(e);
  } catch (_) {
    return "未知错误";
  }
}

function focusSearch() {
  try {
    searchEl.focus({ preventScroll: true });
  } catch (e) {
    try {
      searchEl.focus();
    } catch (e2) {
      /* 忽略 */
    }
  }
}

/** 页面加载时窗口若已可见/已有焦点，主动聚焦（防止 app-center-open 早于脚本而丢失自动聚焦） */
function focusIfVisible() {
  try {
    if (document.visibilityState === "visible" || document.hasFocus()) {
      focusSearch();
    }
  } catch (e) {
    /* 忽略 */
  }
}

function closeCenter() {
  try {
    invoke("close_app_center").catch(() => {});
  } catch (e) {
    /* Tauri 尚未注入时忽略 */
  }
}

// ---------- 渲染 ----------

/** 按当前搜索词重算 filtered，并保留（可选）选中项的 id */
function rebuildFiltered(preserveId) {
  const q = searchEl.value.trim().toLowerCase();
  query = q;
  filtered = q ? apps.filter((a) => String(a.name || "").toLowerCase().includes(q)) : apps.slice();

  let idx = 0;
  if (preserveId) {
    const i = filtered.findIndex((a) => a.id === preserveId);
    if (i >= 0) idx = i;
  }
  selectedIndex = filtered.length ? idx : -1;
  render();
}

function render() {
  renderStatus();
  gridEl.innerHTML = filtered.map(cardHtml).join("");

  if (query) {
    countEl.textContent = filtered.length + " / " + apps.length + " 个应用";
  } else {
    countEl.textContent = apps.length ? apps.length + " 个应用" : "";
  }

  // 搜索时禁止拖拽排序，顺手把提示文案切换掉
  if (dragHintEl) dragHintEl.textContent = query ? " · 搜索时不可拖拽排序" : " · 拖拽排序";

  updateSelectionDom(false);
}

/** 加载态 / 失败态（带重试）/ 搜索空态 */
function renderStatus() {
  if (!statusEl) return;
  let html = "";
  if (loading) {
    html = '<div class="ac-status-text">正在加载应用…</div>';
  } else if (loadFailed && !apps.length) {
    html =
      '<div class="ac-status-text">无法读取应用列表</div>' +
      '<button class="ac-retry" id="ac-retry" type="button">重试</button>';
  } else if (!filtered.length) {
    html = '<div class="ac-status-text">' + (query ? "没有找到应用" : "没有可用的应用") + "</div>";
  }
  statusEl.innerHTML = html;
  statusEl.classList.toggle("hidden", !html);
}

function cardHtml(app, index) {
  const name = String(app.name || "未命名应用");
  const id = String(app.id || "");
  let icon;
  if (app.icon) {
    icon = '<img src="' + escapeHtml(app.icon) + '" alt="">';
  } else {
    const letter = escapeHtml((name.slice(0, 1) || "?").toUpperCase());
    icon =
      '<span class="ac-letter" style="background:' +
      LETTER_COLORS[index % LETTER_COLORS.length] +
      '">' +
      letter +
      "</span>";
  }
  const badge = app.kind === "import" ? '<span class="ac-import-badge">导入</span>' : "";
  const cls =
    "ac-app" +
    (index === selectedIndex ? " selected" : "") +
    (app.kind === "import" ? " imported" : "");
  return (
    '<button class="' +
    cls +
    '" type="button" data-i="' +
    index +
    '" data-id="' +
    escapeHtml(id) +
    '" title="' +
    escapeHtml(name) +
    '">' +
    badge +
    '<span class="ac-icon">' +
    icon +
    '</span><span class="ac-name">' +
    escapeHtml(name) +
    "</span></button>"
  );
}

function updateSelectionDom(scroll) {
  const cards = gridEl.children;
  for (let i = 0; i < cards.length; i++) {
    cards[i].classList.toggle("selected", i === selectedIndex);
  }
  if (!scroll) return;
  const el = cards[selectedIndex];
  if (el && typeof el.scrollIntoView === "function") {
    el.scrollIntoView({ block: "nearest", inline: "nearest" });
  }
}

function selectAt(index, scroll) {
  if (!filtered.length) {
    selectedIndex = -1;
    updateSelectionDom(false);
    return;
  }
  selectedIndex = Math.max(0, Math.min(filtered.length - 1, index));
  updateSelectionDom(scroll);
}

// ---------- 提示 ----------

/** 底部提示：ok=绿色（成功），err=红色（失败），3s 后消失 */
function showToast(msg, kind) {
  if (!toastEl) return;
  toastEl.textContent = msg;
  toastEl.classList.remove("hidden", "ok", "err");
  toastEl.classList.add(kind === "ok" ? "ok" : "err");
  if (toastTimer) clearTimeout(toastTimer);
  toastTimer = setTimeout(() => {
    toastTimer = null;
    if (toastEl) toastEl.classList.add("hidden");
  }, 3000);
}

// ---------- 数据 ----------

/**
 * 拉取应用列表。
 * @param {{silent?: boolean, first?: boolean}} opts silent=不显示加载态；first=用 invokeRetry（启动阶段 IPC 可能还没就绪）
 */
async function loadApps(opts) {
  const silent = !!(opts && opts.silent);
  const first = !!(opts && opts.first);
  if (!silent) {
    loading = true;
    renderStatus();
  }
  const prev = filtered[selectedIndex];
  const prevId = prev ? prev.id : null;
  try {
    // 启动阶段用 invokeRetry 兜住「IPC 尚未就绪」的静默丢包；
    // 次数刻意压低一些，让真正的失败态能较快出现（不必等 5×700ms）。
    const list = first
      ? await invokeRetry("get_apps", undefined, { attempts: 3, perTryMs: 4000, gapMs: 600 })
      : await invoke("get_apps");
    apps = (Array.isArray(list) ? list : []).filter((a) => a && typeof a === "object" && a.id);
    loadFailed = false;
    loading = false;
    rebuildFiltered(prevId);
  } catch (e) {
    loading = false;
    loadFailed = true;
    if (!apps.length) {
      filtered = [];
      selectedIndex = -1;
      gridEl.innerHTML = "";
      countEl.textContent = "";
    }
    renderStatus();
  }
}

function finishRefresh() {
  refreshing = false;
  if (refreshTimer) {
    clearTimeout(refreshTimer);
    refreshTimer = null;
  }
  if (refreshBtn) refreshBtn.disabled = false;
  if (refreshBtn) refreshBtn.classList.remove("busy");
  if (refreshLabel) refreshLabel.textContent = "刷新";
}

// ---------- 启动 / 错误提示 ----------

async function launch(app) {
  if (!app || launching) return;
  launching = true;
  try {
    // 成功时后端会自动隐藏应用中心，这里不需要（也不应该）自己关闭
    await invoke("launch_app_entry", { id: app.id });
  } catch (e) {
    // 失败：保持打开并提示，允许重试
    launching = false;
    showLaunchError(errText(e));
  }
}

function showLaunchError(msg) {
  if (!errorEl) return;
  errorEl.textContent = msg;
  errorEl.classList.remove("hidden");
  if (errorTimer) clearTimeout(errorTimer);
  errorTimer = setTimeout(() => {
    errorTimer = null;
    hideLaunchError();
  }, 3000);
}

function hideLaunchError() {
  if (errorTimer) {
    clearTimeout(errorTimer);
    errorTimer = null;
  }
  if (errorEl) {
    errorEl.textContent = "";
    errorEl.classList.add("hidden");
  }
}

// ---------- 键盘 ----------

/** 当前网格列数（响应式，由计算样式里的 grid-template-columns 得到） */
function gridColumns() {
  try {
    const tpl = getComputedStyle(gridEl).gridTemplateColumns;
    if (!tpl || tpl === "none") return 1;
    const n = tpl.trim().split(/\s+/).filter(Boolean).length;
    return Math.max(1, n);
  } catch (e) {
    return 1;
  }
}

function moveSelection(key) {
  const n = filtered.length;
  if (!n) return;
  const cols = gridColumns();
  let i = selectedIndex < 0 ? 0 : selectedIndex;
  if (key === "ArrowRight") i = Math.min(n - 1, i + 1);
  else if (key === "ArrowLeft") i = Math.max(0, i - 1);
  else if (key === "ArrowDown") i = Math.min(n - 1, i + cols);
  else if (key === "ArrowUp") i = Math.max(0, i - cols);
  selectAt(i, true);
}

function onKeyDown(e) {
  if (e.isComposing || e.defaultPrevented) return; // 中文输入法组词中不抢键
  if (e.altKey || e.ctrlKey || e.metaKey) return; // 热键交给系统

  const key = e.key;
  if (key === "Escape") {
    e.preventDefault();
    // 拖拽中按 Esc：取消拖拽并恢复原顺序
    if (dragState) {
      cancelAppDrag();
      return;
    }
    // 右键菜单开着时先关菜单，不关整个应用中心
    if (menuOpen) {
      hideAppMenu();
      return;
    }
    closeCenter();
    return;
  }
  if (key === "Enter") {
    e.preventDefault();
    const app = filtered[selectedIndex] || filtered[0];
    if (app) launch(app);
    return;
  }
  if (key === "ArrowUp" || key === "ArrowDown" || key === "ArrowLeft" || key === "ArrowRight") {
    if (!filtered.length) return;
    e.preventDefault();
    moveSelection(key);
  }
}

// ---------- 刷新 / 导入 ----------

function onRefreshClick() {
  // 点完按钮把焦点还给搜索框，方便继续输入
  focusSearch();
  if (refreshing) return;
  refreshing = true;
  if (refreshBtn) refreshBtn.disabled = true;
  if (refreshBtn) refreshBtn.classList.add("busy");
  if (refreshLabel) refreshLabel.textContent = "刷新中…";
  try {
    invoke("refresh_apps").catch(() => {});
  } catch (e) {
    /* 忽略 */
  }
  // 正常情况下由 apps-changed 恢复；这里加 10s 兜底，避免事件丢失后一直「刷新中…」
  if (refreshTimer) clearTimeout(refreshTimer);
  refreshTimer = setTimeout(finishRefresh, 10000);
}

function restoreImportButton() {
  importing = false;
  if (importBtn) importBtn.disabled = false;
  if (importBtn) importBtn.classList.remove("busy");
  if (importLabel) importLabel.textContent = "导入";
}

async function onImportClick() {
  focusSearch();
  if (importing) return;
  importing = true;
  if (importBtn) importBtn.disabled = true;
  if (importBtn) importBtn.classList.add("busy");
  if (importLabel) importLabel.textContent = "导入中…";
  try {
    const path = await invoke("pick_import_file");
    if (!path) {
      // 用户取消：什么都不做
      return;
    }
    const entry = await invoke("import_app", { path });
    const name = entry && entry.name ? String(entry.name) : "应用";
    showToast("已导入 " + name, "ok");
    // 列表刷新交给后端 apps-changed；这里静默拉一次兜底（不改后端数据）
    loadApps({ silent: true });
  } catch (e) {
    showToast(errText(e), "err");
  } finally {
    restoreImportButton();
  }
}

// ---------- 导入应用的右键菜单 ----------

function showAppMenu(app, x, y) {
  if (!menuEl || !app) return;
  contextApp = app;
  menuEl.innerHTML = '<button class="ac-menu-item" type="button" data-act="remove">移除导入应用</button>';
  menuEl.classList.remove("hidden");
  menuOpen = true;
  // 先显示再测量，然后夹到视口内
  const w = menuEl.offsetWidth || 150;
  const h = menuEl.offsetHeight || 36;
  menuEl.style.left = Math.max(8, Math.min(window.innerWidth - w - 8, x)) + "px";
  menuEl.style.top = Math.max(8, Math.min(window.innerHeight - h - 8, y)) + "px";
}

function hideAppMenu() {
  if (!menuEl) return;
  menuEl.classList.add("hidden");
  menuEl.innerHTML = "";
  contextApp = null;
  menuOpen = false;
}

async function onMenuClick(e) {
  const btn = e.target.closest ? e.target.closest("[data-act]") : null;
  if (!btn) return;
  const app = contextApp;
  hideAppMenu();
  if (!app || btn.dataset.act !== "remove") return;
  const name = app.name || "该应用";
  if (!window.confirm("从应用中心移除「" + name + "」？")) return;
  try {
    await invoke("remove_imported_app", { id: app.id });
    showToast("已移除 " + name, "ok");
    loadApps({ silent: true }); // 同样以后端 apps-changed 为准，这里兜底
  } catch (err) {
    showToast(errText(err), "err");
  }
}

function onGridContextMenu(e) {
  const card = e.target.closest ? e.target.closest(".ac-app") : null;
  // 所有卡片都阻止 WebView 默认菜单；只有「导入」卡片才弹自绘菜单
  e.preventDefault();
  if (!card) {
    hideAppMenu();
    return;
  }
  const i = Number(card.dataset.i);
  const app = filtered[i];
  if (!app || app.kind !== "import") {
    hideAppMenu();
    return;
  }
  showAppMenu(app, e.clientX, e.clientY);
}

// ---------- 拖拽排序（pointer 事件 + Dock 同款位置预览；仅在没有搜索词时可用） ----------

function startAutoScroll() {
  if (autoScrollTimer) return;
  autoScrollTimer = setInterval(tickAutoScroll, 40);
}

function stopAutoScroll() {
  autoScrollDir = 0;
  if (autoScrollTimer) {
    clearInterval(autoScrollTimer);
    autoScrollTimer = null;
  }
}

function tickAutoScroll() {
  const d = dragState;
  if (!d || !d.moved) {
    stopAutoScroll();
    return;
  }
  // 方向由最近一次 pointermove 计算（指针停在边缘不动时也要持续滚动）
  if (!autoScrollDir) return;
  const before = scrollEl.scrollTop;
  scrollEl.scrollTop += autoScrollDir * AUTO_SCROLL_STEP;
  // 滚动后卡片位置变了，用最后的指针位置重新计算落点
  if (scrollEl.scrollTop !== before) updateAppLiveOrder(d.lastX, d.lastY);
}

function maybeAutoScroll(y) {
  const r = scrollEl.getBoundingClientRect();
  if (y < r.top + AUTO_SCROLL_ZONE) autoScrollDir = -1;
  else if (y > r.bottom - AUTO_SCROLL_ZONE) autoScrollDir = 1;
  else autoScrollDir = 0;
}

/** 按下卡片：记录起点；移动超过阈值才真正开始拖拽 */
function onGridPointerDown(e) {
  if (e.button !== 0) return; // 只处理左键
  if (dragState) return;
  if (query) return; // 搜索时禁用拖拽排序
  const card = e.target.closest ? e.target.closest(".ac-app") : null;
  if (!card || !gridEl.contains(card)) return;
  const id = card.dataset.id || "";
  if (!id) return;
  if (filtered.length < 2) return; // 只有一张卡片时没有排序意义

  const r = card.getBoundingClientRect();
  dragState = {
    el: card,
    id,
    pointerId: e.pointerId,
    startX: e.clientX,
    startY: e.clientY,
    lastX: e.clientX,
    lastY: e.clientY,
    moved: false,
    ghost: null,
    grabX: e.clientX - r.left,
    grabY: e.clientY - r.top,
  };
  // 捕获指针：拖到网格外/窗口边缘也能继续收到 move/up
  try {
    if (card.setPointerCapture) card.setPointerCapture(e.pointerId);
  } catch (err) {
    /* 忽略 */
  }
}

function onWindowPointerMove(e) {
  const d = dragState;
  if (!d) return;
  d.lastX = e.clientX;
  d.lastY = e.clientY;
  if (!d.moved) {
    if (Math.hypot(e.clientX - d.startX, e.clientY - d.startY) < APP_DRAG_THRESHOLD) return;
    beginAppDrag();
  }
  updateAppGhost(e.clientX, e.clientY);
  maybeAutoScroll(e.clientY);
  updateAppLiveOrder(e.clientX, e.clientY);
}

/** 进入拖拽：克隆一张跟手的 ghost，源卡片半透明留在实时让位后的位置 */
function beginAppDrag() {
  const d = dragState;
  d.moved = true;
  hideAppMenu();

  const r = d.el.getBoundingClientRect();
  const ghost = d.el.cloneNode(true);
  ghost.className = d.el.className + " drag-ghost";
  // 去掉与「选中/源项」相关的类：ghost 是浮起来的一张干净卡片
  ghost.classList.remove("selected", "drag-src");
  ghost.removeAttribute("id");
  ghost.style.cssText =
    "position:fixed;pointer-events:none;z-index:90;margin:0;" +
    "left:" + r.left + "px;top:" + r.top + "px;" +
    "width:" + r.width + "px;height:" + r.height + "px;";
  document.body.appendChild(ghost);
  d.ghost = ghost;

  d.el.classList.add("drag-src");
  document.body.classList.add("ac-dragging");
  gridEl.classList.add("ac-dragging");
  startAutoScroll();
}

function updateAppGhost(x, y) {
  const d = dragState;
  if (!d || !d.ghost) return;
  d.ghost.style.left = x - d.grabX + "px";
  d.ghost.style.top = y - d.grabY + "px";
}

/**
 * 实时让位：按指针所在卡片决定插到它前面还是后面，直接 gridEl.insertBefore。
 * 用矩形命中（而不是 elementFromPoint），避免源卡片自身挡住命中。
 */
function updateAppLiveOrder(x, y) {
  const d = dragState;
  if (!d || !d.moved) return;

  const gridRect = gridEl.getBoundingClientRect();
  // 指针在网格下方 → 移到末尾；在上方 → 移到开头
  if (y > gridRect.bottom) {
    if (gridEl.lastElementChild !== d.el) gridEl.appendChild(d.el);
    return;
  }
  if (y < gridRect.top) {
    if (gridEl.firstElementChild !== d.el) gridEl.insertBefore(d.el, gridEl.firstElementChild);
    return;
  }

  // 指针落在哪张卡片上（排除源自身）
  let target = null;
  for (const el of gridEl.children) {
    if (el === d.el) continue;
    const r = el.getBoundingClientRect();
    if (x >= r.left && x <= r.right && y >= r.top && y <= r.bottom) {
      target = el;
      break;
    }
  }
  if (!target) return; // 落在空白/间隙：保持当前位置

  const rect = target.getBoundingClientRect();
  const midY = rect.top + rect.height / 2;
  const midX = rect.left + rect.width / 2;
  // 上下半区决定 before/after；同一水平带内再按左右中点判定
  const after = Math.abs(y - midY) > rect.height * 0.25 ? y > midY : x >= midX;

  if (after) {
    if (target.nextElementSibling !== d.el) gridEl.insertBefore(d.el, target.nextElementSibling);
  } else {
    if (target.previousElementSibling !== d.el) gridEl.insertBefore(d.el, target);
  }
}

function onWindowPointerUp(e) {
  const d = dragState;
  if (!d) return;

  if (!d.moved) {
    // 只是点击：清理状态，click 会正常启动应用
    cleanupAppDrag();
    return;
  }

  // 松手：DOM 顺序就是最终顺序
  const domIds = Array.from(gridEl.querySelectorAll(".ac-app")).map((el) => el.dataset.id || "");
  const prevSelected = filtered[selectedIndex];
  const keepId = prevSelected ? prevSelected.id : null;

  const changed = applyDomOrderToApps(domIds);
  cleanupAppDrag();
  // 拖拽后抑制一次 click，避免松手被当成一次启动
  dragClickGuard = Date.now() + DRAG_CLICK_SUPPRESS_MS;

  if (changed) {
    renderPreservingScroll(keepId);
    commitAppOrder();
  }
  // 拖拽期间若有 apps-changed 被搁置，这里补一次刷新
  if (pendingAppsReload) {
    pendingAppsReload = false;
    loadApps({ silent: true });
  }
}

function onWindowPointerCancel() {
  if (!dragState) return;
  cancelAppDrag();
}

/** 取消拖拽：apps 未被改动，render 即恢复原顺序 */
function cancelAppDrag() {
  const d = dragState;
  if (!d) return;
  const moved = d.moved;
  cleanupAppDrag();
  if (moved) render();
  if (pendingAppsReload) {
    pendingAppsReload = false;
    loadApps({ silent: true });
  }
}

/** 移除 ghost / 半透明源项 / 拖拽类，释放指针捕获（不提交顺序） */
function cleanupAppDrag() {
  const d = dragState;
  dragState = null;
  stopAutoScroll();
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
  document.body.classList.remove("ac-dragging");
  gridEl.classList.remove("ac-dragging");
}

/**
 * 用 DOM 顺序回写 apps（无搜索词时 filtered 与 apps 同序）。
 * 返回是否成功；数据不一致时不动，避免把列表写坏。
 */
function applyDomOrderToApps(domIds) {
  if (query) return false;
  if (!apps.length || domIds.length !== apps.length) return false;
  const byId = new Map(apps.map((a) => [String(a.id), a]));
  const seen = new Set();
  const next = [];
  for (const id of domIds) {
    const key = String(id);
    const app = byId.get(key);
    if (!app || seen.has(key)) return false;
    seen.add(key);
    next.push(app);
  }
  apps = next;
  return true;
}

/** 重建列表但保持滚动位置与选中项 */
function renderPreservingScroll(keepId) {
  const top = scrollEl.scrollTop;
  rebuildFiltered(keepId);
  scrollEl.scrollTop = top;
}

/** 提交新的应用顺序（后端会存盘并重排缓存） */
function commitAppOrder() {
  const order = apps.map((a) => String(a.id));
  try {
    invoke("set_app_order", { order }).catch(() => {});
  } catch (e) {
    /* Tauri 尚未注入时忽略 */
  }
}

/** 拖拽结束后抑制一次 click（防止拖完松手直接启动应用） */
function shouldSuppressClick() {
  if (!dragClickGuard) return false;
  const now = Date.now();
  if (now < dragClickGuard) {
    dragClickGuard = 0;
    return true;
  }
  dragClickGuard = 0;
  return false;
}

// ---------- 事件 ----------

/** 每次打开应用中心：重置搜索/选中、聚焦输入框、滚动到顶 */
function onOpen() {
  launching = false;
  cancelAppDrag(); // 上次若残留拖拽状态，清掉并恢复顺序
  pendingAppsReload = false;
  hideAppMenu();
  hideLaunchError();
  searchEl.value = "";
  query = "";
  scrollEl.scrollTop = 0;
  rebuildFiltered(null);
  focusSearch();
  // 静默拉一次缓存：后端缓存为空/过期会自动后台刷新并随后发 apps-changed
  loadApps({ silent: true });
}

/** 后端枚举完成：恢复刷新按钮并保持当前搜索词重绘 */
function onAppsChanged() {
  finishRefresh();
  // 拖拽中不要重绘，否则 ghost 会丢；结束后补一次
  if (dragState) {
    pendingAppsReload = true;
    return;
  }
  loadApps({ silent: true });
}

function bindEvents() {
  searchEl.addEventListener("input", () => rebuildFiltered(null));
  if (refreshBtn) refreshBtn.addEventListener("click", onRefreshClick);
  if (importBtn) importBtn.addEventListener("click", onImportClick);

  // 卡片：点击启动。
  // 注意：hover 只做 CSS 高亮，不改变键盘选中项 —— 否则鼠标恰好停在某张卡片上时，
  // Enter 会启动「鼠标下那张」而不是键盘选中的那张（GNOME 也是两套状态互不干扰）。
  gridEl.addEventListener("click", (e) => {
    if (shouldSuppressClick()) return; // 刚拖拽完，忽略这一次 click
    const card = e.target.closest ? e.target.closest(".ac-app") : null;
    if (!card) return;
    const i = Number(card.dataset.i);
    if (Number.isInteger(i) && filtered[i]) launch(filtered[i]);
  });

  // 导入应用的右键菜单
  gridEl.addEventListener("contextmenu", onGridContextMenu);
  if (menuEl) menuEl.addEventListener("click", onMenuClick);

  // 拖拽排序（pointer 事件 + Dock 同款位置预览；HTML5 DnD 在物理鼠标下很难触发）
  gridEl.addEventListener("pointerdown", onGridPointerDown);
  window.addEventListener("pointermove", onWindowPointerMove);
  window.addEventListener("pointerup", onWindowPointerUp);
  window.addEventListener("pointercancel", onWindowPointerCancel);
  // 窗口失焦时取消拖拽（应用中心本身不因失焦关闭）
  window.addEventListener("blur", () => {
    if (dragState) cancelAppDrag();
  });

  // 失败态里的「重试」
  statusEl.addEventListener("click", (e) => {
    if (e.target.closest && e.target.closest("#ac-retry")) loadApps({ first: true });
  });

  // 点击背景空白关闭（只有 target 就是 #backdrop 时才算）
  backdropEl.addEventListener("mousedown", (e) => {
    if (e.target === backdropEl) closeCenter();
  });

  // 右键菜单：点空白处关闭
  document.addEventListener("mousedown", (e) => {
    if (!menuOpen || !menuEl) return;
    if (menuEl.contains(e.target)) return;
    hideAppMenu();
  });
  // 右键菜单：滚动列表时关闭
  scrollEl.addEventListener("scroll", () => {
    if (menuOpen) hideAppMenu();
  });

  document.addEventListener("keydown", onKeyDown);
}

function ensureListeners() {
  if (listening) return;
  try {
    const p1 = listen("app-center-open", onOpen);
    const p2 = listen("apps-changed", onAppsChanged);
    if (p1 && typeof p1.catch === "function") {
      p1.catch((e) => console.warn("[应用中心] app-center-open 监听失败", e));
    }
    if (p2 && typeof p2.catch === "function") {
      p2.catch((e) => console.warn("[应用中心] apps-changed 监听失败", e));
    }
    listening = true;
  } catch (e) {
    // Tauri 尚未注入：waitTauri 之后再注册
  }
}

async function waitTauri(timeoutMs) {
  const limit = timeoutMs || 15000;
  const t0 = Date.now();
  while (!(window.__TAURI__ && window.__TAURI__.core)) {
    if (Date.now() - t0 > limit) throw new Error("Tauri API 注入超时");
    await new Promise((r) => setTimeout(r, 100));
  }
}

async function main() {
  bindEvents();
  ensureListeners();
  focusIfVisible(); // 窗口若已可见（页面加载慢于首次打开事件），先聚焦搜索框

  await waitTauri();
  ensureListeners();

  await loadApps({ first: true }); // 首次加载显示「正在加载应用…」
  focusIfVisible(); // 兜底再聚焦一次
}

main().catch((e) => {
  console.error("[应用中心] 启动失败", e);
});

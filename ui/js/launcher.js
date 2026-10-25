// 应用中心（全屏应用网格）：搜索 / 键盘导航 / 启动。
//
// 关闭时机（重要）：只在 Esc、点击 #backdrop 空白、启动成功（后端自动隐藏）时关闭。
// **不监听 window.blur** —— 应用中心打开时顶栏/Dock 在其上层（后端会把它们重新置顶），
// 点 Dock 九点格会先让本窗口失焦；若 blur 抢先 close，随后 Dock 的 toggle_app_center
// 又会重新打开，表现为「关不掉」。其它页面切换（活动概览/顶栏活动）由后端统一处理。
import { invoke, invokeRetry, listen, escapeHtml } from "./api.js";

// 与 Dock 一致的字母占位配色
const LETTER_COLORS = ["#3584e4", "#e66100", "#2ec27e", "#9141ac", "#e01b24", "#1c71d8", "#8f4e2b"];

let apps = []; // 全量应用（后端顺序）
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

const backdropEl = document.getElementById("backdrop");
const scrollEl = document.getElementById("ac-scroll");
const gridEl = document.getElementById("ac-grid");
const statusEl = document.getElementById("ac-status");
const searchEl = document.getElementById("ac-search");
const countEl = document.getElementById("ac-count");
const refreshBtn = document.getElementById("ac-refresh");
const refreshLabel = document.getElementById("ac-refresh-label");
const errorEl = document.getElementById("ac-error");

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
  const cls = "ac-app" + (index === selectedIndex ? " selected" : "");
  return (
    '<button class="' +
    cls +
    '" type="button" data-i="' +
    index +
    '" title="' +
    escapeHtml(name) +
    '"><span class="ac-icon">' +
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

// ---------- 刷新 ----------

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

// ---------- 事件 ----------

/** 每次打开应用中心：重置搜索/选中、聚焦输入框、滚动到顶 */
function onOpen() {
  launching = false;
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
  loadApps({ silent: true });
}

function bindEvents() {
  searchEl.addEventListener("input", () => rebuildFiltered(null));
  if (refreshBtn) refreshBtn.addEventListener("click", onRefreshClick);

  // 卡片：点击启动。
  // 注意：hover 只做 CSS 高亮，不改变键盘选中项 —— 否则鼠标恰好停在某张卡片上时，
  // Enter 会启动「鼠标下那张」而不是键盘选中的那张（GNOME 也是两套状态互不干扰）。
  gridEl.addEventListener("click", (e) => {
    const card = e.target.closest ? e.target.closest(".ac-app") : null;
    if (!card) return;
    const i = Number(card.dataset.i);
    if (Number.isInteger(i) && filtered[i]) launch(filtered[i]);
  });

  // 失败态里的「重试」
  statusEl.addEventListener("click", (e) => {
    if (e.target.closest && e.target.closest("#ac-retry")) loadApps({ first: true });
  });

  // 点击背景空白关闭（只有 target 就是 #backdrop 时才算）
  backdropEl.addEventListener("mousedown", (e) => {
    if (e.target === backdropEl) closeCenter();
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

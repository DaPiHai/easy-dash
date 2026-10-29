// 应用中心（全屏应用网格 + 分页）：搜索 / 键盘导航 / 启动 / 拖拽排序 / 导入应用。
//
// 关闭时机（重要）：只在 Esc、点击 #backdrop 空白、启动成功（后端自动隐藏）时关闭。
// **不监听 window.blur** —— 应用中心打开时顶栏/Dock 在其上层（后端会把它们重新置顶），
// 点 Dock 九点格会先让本窗口失焦；若 blur 抢先 close，随后 Dock 的 toggle_app_center
// 又会重新打开，表现为「关不掉」。其它页面切换（活动概览/顶栏活动）由后端统一处理。
//
// 分页模型（#ac-scroll 不再纵向滚动）：
//   每页容量 pageSize = cols × rows，cols 由 grid-template-columns 计算样式得到，
//   rows 由 #ac-scroll 视口高度、卡片固定高度 104px、行间距 8px 推算，只渲染当前页。
//   翻页入口：滚轮 / 分页器 / PageUp/PageDown/Home/End / 方向键越过页边界 / 拖拽到上下边缘悬停。
//
// 外观：import theme.js 让本窗口也能响应 theme-changed / config-changed。
// （此前只靠 head 内联脚本读一次缓存，运行中切浅色/深色或换材质时应用中心不会跟着变。）
import "./theme.js";
import "./cursor-glow.js";
import "./glass.js";
import { beginSurfaceOpen } from "./surface-open.js";
import { invoke, invokeRetry, listen, escapeHtml } from "./api.js";

// 与 Dock 一致的字母占位配色
const LETTER_COLORS = ["#3584e4", "#e66100", "#2ec27e", "#9141ac", "#e01b24", "#1c71d8", "#8f4e2b"];

/** 拖拽后抑制一次 click 的时长（毫秒），避免拖完松手把卡片当成一次启动 */
const DRAG_CLICK_SUPPRESS_MS = 300;
/** 拖拽判定阈值（像素）：超过才算拖动，否则按点击处理 */
const APP_DRAG_THRESHOLD = 6;
/** 卡片高度 / 行间距：与 launcher.css 的 .ac-grid 保持一致（计算样式取不到时兜底） */
const PAGE_ROW_HEIGHT = 104;
const PAGE_ROW_GAP = 8;
/** 拖拽到视口上下边缘的感应区高度与悬停自动翻页时长 */
const DRAG_EDGE_ZONE = 64;
const DRAG_EDGE_HOLD_MS = 550;
/** 滚轮翻页：累计阈值、冷却时间、空闲清零时间 */
const WHEEL_THRESHOLD = 28;
const WHEEL_COOLDOWN_MS = 380;
const WHEEL_IDLE_RESET_MS = 260;
/** 翻页动画时长（与 launcher.css 的 @keyframes 一致，仅用于清理 class） */
const PAGE_ANIM_MS = 200;

// ---------- 列表状态 ----------
let apps = []; // 全量应用（后端顺序，拖拽排序改的就是它）
let filtered = []; // 当前搜索结果
let selectedIndex = -1; // 键盘选中项（filtered 全局下标）
let query = ""; // 当前搜索词（小写、已 trim）
let loading = false; // 首次/重试加载中
let loadFailed = false; // 最近一次加载失败
let refreshing = false; // 正在请求后端刷新
let launching = false; // 启动中（防重复触发）
let refreshTimer = null; // 刷新兜底恢复
let errorTimer = null; // 启动失败提示
let listening = false; // 事件监听是否已注册

// ---------- 分页状态 ----------
let page = 0; // 当前页（0 起）
let pageSize = 12; // 每页卡片数（每次 render 按布局重算）
let pageCount = 1; // 总页数
let cols = 4; // 当前列数（方向键上下移动步长）
let pageAnimTimer = null; // 翻页动画 class 清理
let relayoutRaf = 0; // resize 合并到一帧
let wheelAccum = 0; // 滚轮累计位移
let wheelLastFlipAt = 0; // 上次翻页时间（冷却）
let wheelResetTimer = null; // 滚轮空闲清零

// ---------- 拖拽排序状态 ----------
// { id, pointerId, captureEl, el, startX, startY, lastX, lastY, moved, ghost,
//   grabX, grabY, tempIndex, edgeDir }
let dragState = null;
let dragClickGuard = 0; // 拖拽结束后抑制 click 的截止时间
let edgeTimer = null; // 边缘悬停自动翻页定时器
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
const pagerEl = document.getElementById("ac-pager");
const pagerPrevEl = document.getElementById("ac-pager-prev");
const pagerNextEl = document.getElementById("ac-pager-next");
const pagerDotsEl = document.getElementById("ac-pager-dots");
const pagerCountEl = document.getElementById("ac-pager-count");

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

function clamp(n, lo, hi) {
  return Math.max(lo, Math.min(hi, n));
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

// ---------- 布局 / 分页 ----------

/** 读取网格列数、行数、每页容量（行高 104px、行间距 8px，来自计算样式） */
function measureGrid() {
  let columns = 1;
  let gapY = PAGE_ROW_GAP;
  let rowH = PAGE_ROW_HEIGHT;
  try {
    const cs = getComputedStyle(gridEl);
    const tpl = cs.gridTemplateColumns;
    if (tpl && tpl !== "none") {
      const n = tpl.trim().split(/\s+/).filter(Boolean).length;
      if (n > 0) columns = n;
    }
    const gap = parseFloat(cs.rowGap);
    if (Number.isFinite(gap)) gapY = gap;
    const auto = parseFloat(cs.gridAutoRows);
    if (Number.isFinite(auto) && auto > 0) rowH = auto;
  } catch (e) {
    /* 计算样式取不到时用默认值 */
  }
  const viewportH = Math.max(0, scrollEl.clientHeight);
  const rows = Math.max(1, Math.floor((viewportH + gapY) / (rowH + gapY)));
  return { columns, rows, pageSize: Math.max(1, columns * rows) };
}

/**
 * 重算分页容量并夹取页码。
 * 先按“分页器隐藏”的最大高度试算：确实需要多页时才显示分页器，再按压缩后的高度重算，
 * 避免分页器自身占位导致「本来一页能放下却显示两页」。
 */
function computePageModel() {
  if (pagerEl) pagerEl.classList.add("hidden");
  let metrics = measureGrid();
  let count = Math.max(1, Math.ceil(filtered.length / metrics.pageSize));
  if (count > 1 && pagerEl) {
    pagerEl.classList.remove("hidden");
    metrics = measureGrid();
    count = Math.max(1, Math.ceil(filtered.length / metrics.pageSize));
    if (count <= 1) pagerEl.classList.add("hidden");
  }
  cols = metrics.columns;
  pageSize = metrics.pageSize;
  pageCount = count;
  page = clamp(page, 0, pageCount - 1);
}

function animatePage(dir) {
  if (!dir) return;
  const cls = dir === "next" ? "page-next" : "page-prev";
  gridEl.classList.remove("page-next", "page-prev");
  // 强制重排，保证同一方向的连续翻页也能重启动画
  void gridEl.offsetWidth;
  gridEl.classList.add(cls);
  if (pageAnimTimer) clearTimeout(pageAnimTimer);
  pageAnimTimer = setTimeout(() => {
    pageAnimTimer = null;
    gridEl.classList.remove("page-next", "page-prev");
  }, PAGE_ANIM_MS + 40);
}

/** 翻到指定页（滚轮 / 分页器 / PageUp/PageDown 用）；返回是否真的翻页 */
function goPage(target) {
  if (!filtered.length || pageCount <= 1) return false;
  const t = clamp(target, 0, pageCount - 1);
  if (t === page) return false;
  const dir = t > page ? "next" : "prev";
  page = t;
  // 让选中项落到新页，保证 Enter / 方向键始终有落点
  const start = page * pageSize;
  const end = Math.min(filtered.length, start + pageSize);
  if (selectedIndex < start || selectedIndex >= end) selectedIndex = start;
  render();
  animatePage(dir);
  return true;
}

/** 跳到指定全局下标所在页（Home / End 用） */
function jumpTo(index) {
  if (!filtered.length) return;
  const targetIndex = clamp(index, 0, filtered.length - 1);
  const targetPage = clamp(Math.floor(targetIndex / pageSize), 0, pageCount - 1);
  const dir = targetPage === page ? null : targetPage > page ? "next" : "prev";
  selectedIndex = targetIndex;
  page = targetPage;
  render();
  if (dir) animatePage(dir);
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

/**
 * 当前页要渲染的卡片（全局下标）。
 * 拖拽中会把被拖卡片按「目标全局下标 tempIndex」插回预览顺序，再取本页窗口，
 * 保证翻页后 ghost 与半透明源项连续，且松手落点与预览完全一致。
 *
 * 预览顺序 preview = filtered 去掉被拖卡片后再把被拖卡片插到 tempIndex：
 *   preview[i] = (i < t ? rest[i] : i > t ? rest[i-1] : 被拖卡片)
 * 松手时 t = 页起点 + DOM 下标，正好是插入 apps 的目标位置。
 */
function buildPageItems() {
  const start = page * pageSize;
  const end = Math.min(filtered.length, start + pageSize);

  const plain = () => {
    const out = [];
    for (let i = start; i < end; i++) out.push({ app: filtered[i], index: i });
    return out;
  };

  if (!dragState || !dragState.moved) return plain();

  const dragId = String(dragState.id);
  const indexById = new Map();
  for (let i = 0; i < filtered.length; i++) indexById.set(String(filtered[i].id), i);
  const draggedIdx = indexById.has(dragId) ? indexById.get(dragId) : -1;
  if (draggedIdx < 0) return plain();

  const draggedApp = filtered[draggedIdx];
  // rest：去掉被拖卡片后的顺序（搜索态禁用拖拽，此时 filtered 与 apps 同序）
  const rest = filtered.filter((a) => String(a.id) !== dragId);

  let t = dragState.tempIndex;
  if (!Number.isInteger(t)) t = draggedIdx;
  t = clamp(t, 0, filtered.length - 1);
  // 防御：确保被拖卡片一定落在当前页窗口内（否则半透明源项会消失）
  if (end > start) t = clamp(t, start, end - 1);
  dragState.tempIndex = t;

  const out = [];
  for (let i = start; i < end; i++) {
    let app;
    if (i === t) app = draggedApp;
    else if (i < t) app = rest[i];
    else app = rest[i - 1];
    if (!app) app = draggedApp; // 防御：数据异常时不丢卡片
    out.push({ app, index: indexById.get(String(app.id)) });
  }
  return out;
}

function render() {
  computePageModel();

  if (!filtered.length) {
    selectedIndex = -1;
  } else if (selectedIndex > filtered.length - 1) {
    selectedIndex = filtered.length - 1;
  }

  renderStatus();

  const draggingId = dragState && dragState.moved ? String(dragState.id) : "";
  const items = buildPageItems();
  gridEl.innerHTML = items
    .map((it) => cardHtml(it.app, it.index, draggingId !== "" && String(it.app.id) === draggingId))
    .join("");

  // 打开动画的错峰序号（motion.css §11 用；上限 12 档，避免长列表拖尾）
  const cards = gridEl.children;
  for (let i = 0; i < cards.length; i++) {
    cards[i].style.setProperty("--enter-i", String(Math.min(i, 12)));
  }

  updateCount();
  updateHint();
  updateSelectionDom();
  renderPager();

  if (dragState && dragState.moved) bindDragSource();
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

function cardHtml(app, index, isDragSource) {
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
    (app.kind === "import" ? " imported" : "") +
    (isDragSource ? " drag-src" : "");
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

function updateCount() {
  if (!countEl) return;
  if (query) {
    countEl.textContent = filtered.length + " / " + apps.length + " 个应用";
  } else {
    countEl.textContent = apps.length ? apps.length + " 个应用" : "";
  }
}

function updateHint() {
  if (!dragHintEl) return;
  dragHintEl.textContent = query ? " · 搜索时不可拖拽排序" : " · 拖拽排序";
}

function updateSelectionDom() {
  const cards = gridEl.children;
  for (let i = 0; i < cards.length; i++) {
    const idx = Number(cards[i].dataset.i);
    cards[i].classList.toggle("selected", Number.isInteger(idx) && idx === selectedIndex);
  }
}

function renderPager() {
  if (!pagerEl) return;
  const show = pageCount > 1 && filtered.length > 0;
  pagerEl.classList.toggle("hidden", !show);
  if (!show) {
    if (pagerDotsEl) pagerDotsEl.innerHTML = "";
    return;
  }
  if (pagerPrevEl) pagerPrevEl.disabled = page <= 0;
  if (pagerNextEl) pagerNextEl.disabled = page >= pageCount - 1;
  if (pagerCountEl) pagerCountEl.textContent = page + 1 + " / " + pageCount;
  renderDots();
}

function renderDots() {
  if (!pagerDotsEl) return;
  const total = pageCount;
  const pages = [];
  if (total <= 11) {
    for (let i = 0; i < total; i++) pages.push(i);
  } else {
    // 页数很多时只展示：首尾 + 当前页附近，其余用省略号
    const set = new Set([0, 1, total - 2, total - 1]);
    for (let i = page - 2; i <= page + 2; i++) {
      if (i >= 0 && i < total) set.add(i);
    }
    const sorted = Array.from(set).sort((a, b) => a - b);
    let prev = -1;
    for (const p of sorted) {
      if (prev >= 0 && p - prev > 1) pages.push(-1);
      pages.push(p);
      prev = p;
    }
  }
  const html = [];
  for (const p of pages) {
    if (p < 0) {
      html.push('<span class="ac-dot-gap" aria-hidden="true">…</span>');
      continue;
    }
    const active = p === page;
    html.push(
      '<button class="ac-dot' +
        (active ? " active" : "") +
        '" type="button" data-page="' +
        p +
        '"' +
        (active ? ' aria-current="page"' : "") +
        ' aria-label="第 ' +
        (p + 1) +
        ' 页" title="第 ' +
        (p + 1) +
        ' 页"></button>'
    );
  }
  pagerDotsEl.innerHTML = html.join("");
}

// ---------- 键盘 / 滚轮 ----------

/** 方向键：全局下标移动；跨页边界时自动翻页 */
function moveSelection(key) {
  const n = filtered.length;
  if (!n) return;
  const i = selectedIndex < 0 ? 0 : selectedIndex;
  let next = i;
  if (key === "ArrowRight") next = Math.min(n - 1, i + 1);
  else if (key === "ArrowLeft") next = Math.max(0, i - 1);
  else if (key === "ArrowDown") next = Math.min(n - 1, i + cols);
  else if (key === "ArrowUp") next = Math.max(0, i - cols);

  selectedIndex = next;
  const nextPage = clamp(Math.floor(next / pageSize), 0, pageCount - 1);
  if (nextPage !== page) {
    const dir = nextPage > page ? "next" : "prev";
    page = nextPage;
    render();
    animatePage(dir);
  } else {
    updateSelectionDom();
  }
}

/** 滚轮翻页：累积 deltaY（横向优先 deltaX），超过阈值翻一页，带冷却 */
function onWheel(e) {
  // 固定页面不滚动：始终阻止原生滚动/缩放链
  e.preventDefault();
  if (dragState || menuOpen) return; // 拖拽 / 右键菜单打开时忽略
  if (filtered.length === 0 || pageCount <= 1) return;

  const dx = e.deltaX || 0;
  const dy = e.deltaY || 0;
  const delta = Math.abs(dx) > Math.abs(dy) ? dx : dy;
  if (!delta) return;

  const now = Date.now();
  if (now - wheelLastFlipAt < WHEEL_COOLDOWN_MS) return;

  wheelAccum += delta;
  if (wheelResetTimer) clearTimeout(wheelResetTimer);
  wheelResetTimer = setTimeout(() => {
    wheelResetTimer = null;
    wheelAccum = 0;
  }, WHEEL_IDLE_RESET_MS);

  if (wheelAccum >= WHEEL_THRESHOLD) {
    wheelAccum = 0;
    if (goPage(page + 1)) wheelLastFlipAt = now;
  } else if (wheelAccum <= -WHEEL_THRESHOLD) {
    wheelAccum = 0;
    if (goPage(page - 1)) wheelLastFlipAt = now;
  }
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
  if (key === "PageDown" || key === "PageUp") {
    if (!filtered.length) return;
    e.preventDefault();
    goPage(page + (key === "PageDown" ? 1 : -1));
    return;
  }
  if (key === "Home" || key === "End") {
    if (!filtered.length) return;
    e.preventDefault();
    jumpTo(key === "Home" ? 0 : filtered.length - 1);
    return;
  }
  if (key === "ArrowUp" || key === "ArrowDown" || key === "ArrowLeft" || key === "ArrowRight") {
    if (!filtered.length) return;
    e.preventDefault();
    moveSelection(key);
  }
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
      page = 0;
      pageCount = 1;
      gridEl.innerHTML = "";
      if (countEl) countEl.textContent = "";
      renderPager();
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

function clearEdgeTimer() {
  if (edgeTimer) {
    clearTimeout(edgeTimer);
    edgeTimer = null;
  }
}

/** 拖拽到视口上下边缘悬停 550ms 自动翻页；按住不放可以连续翻 */
function armEdgeTimer() {
  const d = dragState;
  if (!d || !d.moved || !d.edgeDir) return;
  clearEdgeTimer();
  edgeTimer = setTimeout(() => {
    edgeTimer = null;
    const cur = dragState;
    if (!cur || cur !== d || !cur.moved || !cur.edgeDir) return;
    if (!flipPageForDrag(cur.edgeDir)) return; // 首/末页不循环，到边界停住
    armEdgeTimer();
  }, DRAG_EDGE_HOLD_MS);
}

function maybeEdgeFlip(y) {
  const d = dragState;
  if (!d || !d.moved) return;
  const r = scrollEl.getBoundingClientRect();
  let dir = 0;
  if (y <= r.top + DRAG_EDGE_ZONE) dir = -1;
  else if (y >= r.bottom - DRAG_EDGE_ZONE) dir = 1;
  if (dir === d.edgeDir) return;
  d.edgeDir = dir;
  if (dir) armEdgeTimer();
  else clearEdgeTimer();
}

/** 拖拽中翻页：把被拖卡片按临时下标渲染进新页（向下翻→新页首，向上翻→上一页尾） */
function flipPageForDrag(dir) {
  const d = dragState;
  if (!d || !d.moved) return false;
  const target = page + dir;
  if (target < 0 || target > pageCount - 1) return false;
  page = target;
  const start = page * pageSize;
  d.tempIndex = dir > 0 ? start : start + pageSize - 1;
  render();
  animatePage(dir > 0 ? "next" : "prev");
  return true;
}

/** 拖拽中重绘后重新绑定半透明源项（ghost 在 body 上，不受重绘影响） */
function bindDragSource() {
  const d = dragState;
  if (!d) return;
  let el = null;
  const kids = gridEl.children;
  for (let i = 0; i < kids.length; i++) {
    if (kids[i].dataset.id === d.id) {
      el = kids[i];
      break;
    }
  }
  d.el = el;
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
    id,
    captureEl: card,
    el: card,
    pointerId: e.pointerId,
    startX: e.clientX,
    startY: e.clientY,
    lastX: e.clientX,
    lastY: e.clientY,
    moved: false,
    ghost: null,
    grabX: e.clientX - r.left,
    grabY: e.clientY - r.top,
    tempIndex: 0,
    edgeDir: 0,
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
  if (dragState !== d) return;
  updateAppGhost(e.clientX, e.clientY);
  maybeEdgeFlip(e.clientY);
  updateAppLiveOrder(e.clientX, e.clientY);
}

/** 进入拖拽：克隆一张跟手的 ghost，源卡片半透明留在实时让位后的位置 */
function beginAppDrag() {
  const d = dragState;
  if (!d) return;
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

  // 初始目标下标 = 卡片当前全局下标（原位）；翻页时由 flipPageForDrag 改写
  const draggedIdx = filtered.findIndex((a) => String(a.id) === d.id);
  d.tempIndex = draggedIdx >= 0 ? draggedIdx : 0;
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
 * 指针在网格上下方时交给边缘翻页处理，不在这里抢位置。
 */
function updateAppLiveOrder(x, y) {
  const d = dragState;
  if (!d || !d.moved || !d.el) return;

  const gridRect = gridEl.getBoundingClientRect();
  if (y < gridRect.top || y > gridRect.bottom) return;

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

/** 用「全局下标 = 页起点 + DOM 下标」把被拖卡片插回 apps */
function applyDragResult(d, domIndex) {
  if (query) return false;
  const dragId = String(d.id);
  const draggedApp = apps.find((a) => String(a.id) === dragId);
  if (!draggedApp) return false;
  const rest = apps.filter((a) => String(a.id) !== dragId);
  const target = clamp(page * pageSize + Math.max(0, domIndex), 0, rest.length);
  rest.splice(target, 0, draggedApp);
  if (rest.length !== apps.length) return false;
  apps = rest;
  return true;
}

function onWindowPointerUp() {
  const d = dragState;
  if (!d) return;

  if (!d.moved) {
    // 只是点击：清理状态，click 会正常启动应用
    cleanupAppDrag();
    return;
  }

  // 松手：DOM 顺序（页内）就是最终顺序
  let domIndex = -1;
  const kids = gridEl.children;
  for (let i = 0; i < kids.length; i++) {
    if (kids[i].dataset.id === d.id) {
      domIndex = i;
      break;
    }
  }
  const prevSelected = filtered[selectedIndex];
  const keepId = prevSelected ? prevSelected.id : null;

  const before = apps.map((a) => String(a.id));
  const changed = domIndex >= 0 ? applyDragResult(d, domIndex) : false;
  cleanupAppDrag();
  // 拖拽后抑制一次 click，避免松手被当成一次启动
  dragClickGuard = Date.now() + DRAG_CLICK_SUPPRESS_MS;

  if (changed) {
    const after = apps.map((a) => String(a.id));
    const reordered = after.some((id, i) => id !== before[i]);
    rebuildFiltered(keepId);
    if (reordered) commitAppOrder();
  } else {
    render();
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
  clearEdgeTimer();
  if (d) {
    d.edgeDir = 0;
    if (d.ghost && d.ghost.parentNode) d.ghost.parentNode.removeChild(d.ghost);
    for (const el of [d.el, d.captureEl]) {
      if (el && el.classList) el.classList.remove("drag-src");
    }
    const src = d.captureEl || d.el;
    if (src) {
      try {
        if (src.releasePointerCapture && d.pointerId !== undefined) {
          src.releasePointerCapture(d.pointerId);
        }
      } catch (err) {
        /* 指针已释放时忽略 */
      }
    }
  }
  document.body.classList.remove("ac-dragging");
  gridEl.classList.remove("ac-dragging");
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

/** 每次打开应用中心：重置搜索/选中/页码、聚焦输入框 */
function onOpen() {
  // 先落到关闭态（遮罩 opacity 0），玻璃就绪后由 surface-open.js 播入场
  beginSurfaceOpen();
  launching = false;
  cancelAppDrag(); // 上次若残留拖拽状态，清掉并恢复顺序
  pendingAppsReload = false;
  hideAppMenu();
  hideLaunchError();
  searchEl.value = "";
  query = "";
  page = 0;
  wheelAccum = 0;
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

/** 窗口尺寸/布局变化：列数或每页容量变了才重绘（拖拽中跳过） */
function scheduleRelayout() {
  if (relayoutRaf) return;
  relayoutRaf = requestAnimationFrame(() => {
    relayoutRaf = 0;
    if (dragState) return;
    const metrics = measureGrid();
    if (metrics.columns === cols && metrics.pageSize === pageSize) return;
    render();
  });
}

function onPagerClick(e) {
  let handled = true;
  const dot = e.target.closest ? e.target.closest("[data-page]") : null;
  if (dot) {
    const p = Number(dot.dataset.page);
    if (Number.isInteger(p)) goPage(p);
  } else if (e.target.closest && e.target.closest("#ac-pager-prev")) {
    goPage(page - 1);
  } else if (e.target.closest && e.target.closest("#ac-pager-next")) {
    goPage(page + 1);
  } else {
    handled = false;
  }
  if (handled) focusSearch();
}

function bindEvents() {
  searchEl.addEventListener("input", () => {
    page = 0;
    rebuildFiltered(null);
  });
  if (refreshBtn) refreshBtn.addEventListener("click", onRefreshClick);
  if (importBtn) importBtn.addEventListener("click", onImportClick);

  // 卡片：点击启动（data-i 始终是 filtered 的全局下标，分页后仍正确）。
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

  // 滚轮换页（必须 passive:false 才能阻止原生滚动/手势）
  scrollEl.addEventListener("wheel", onWheel, { passive: false });

  // 分页器：上一页 / 圆点 / 下一页
  if (pagerEl) pagerEl.addEventListener("click", onPagerClick);

  document.addEventListener("keydown", onKeyDown);

  // 窗口尺寸变化 / 布局变化后重算分页
  window.addEventListener("resize", scheduleRelayout);
  if (typeof ResizeObserver === "function") {
    try {
      const ro = new ResizeObserver(scheduleRelayout);
      ro.observe(scrollEl);
    } catch (e) {
      /* 旧内核不支持时忽略，靠 resize 兜底 */
    }
  }
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

/** 主题模块（theme-ui 提供）：加载失败也不影响应用中心，回退到 shared.css 的媒体查询 */
async function initTheme() {
  try {
    await import("./theme.js");
  } catch (e) {
    /* 主题模块缺失/加载失败时忽略 */
  }
}

async function main() {
  initTheme();
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

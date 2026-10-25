// 通知中心面板（双栏）：左栏大时钟 + 日期 + 应用分组（堆叠卡片）通知列表；
// 右栏中文月历 + 媒体卡；底部通栏「清除全部 / 在系统通知中心打开」。
// 只在配置 notificationCenter 为 true 时由 topbar.js 调 setEnabled(true) 启动拉取。
import { invoke, invokeRetry, listen, escapeHtml } from "./api.js";
import { renderCalendar, todayInfo, shiftView } from "./calendar.js";
import { createMediaCard } from "./media.js";

/** localStorage：上次打开面板时看到的最大通知时间戳（毫秒） */
const LAST_SEEN_KEY = "easydash.lastSeen";
/** sessionStorage：本地「清除全部」隐藏的通知 id 集合（刷新后重置） */
const HIDDEN_KEY = "easydash.hiddenNotifications";

const LETTER_COLORS = ["#3584e4", "#e66100", "#2ec27e", "#9141ac", "#e01b24", "#1c71d8", "#8f4e2b"];
const WEEKDAY_CN = ["日", "一", "二", "三", "四", "五", "六"];

// 面板骨架（内容由本模块填充；不依赖 topbar.html 里的静态结构）
const PANEL_HTML = `
  <div class="np-cols">
    <div class="np-left">
      <div class="np-head">
        <div class="np-clock" id="np-clock">--:--</div>
        <div class="np-date" id="np-date"></div>
      </div>
      <div class="np-list" id="np-list"><div class="np-empty">没有通知</div></div>
    </div>
    <div class="np-right">
      <div class="cal" id="np-calendar"></div>
      <div class="np-media" id="np-media"></div>
    </div>
  </div>
  <div class="np-actions">
    <button class="panel-btn" id="np-clear" type="button">清除全部</button>
    <button class="panel-btn" id="np-open-sys" type="button">在系统通知中心打开</button>
  </div>
`;

/**
 * 创建通知中心面板控制器。
 * @param {{onUnreadChange?: (hasUnread: boolean) => void}} opts
 * @returns {{onOpen: Function, onClose: Function, setEnabled: Function, isEnabled: Function}}
 */
export function createNotificationsPanel({ onUnreadChange } = {}) {
  const el = document.getElementById("panel-notifications");

  let items = []; // 后端返回的原始列表（按时间倒序）
  let visible = []; // 过滤本地隐藏后的可见列表
  let groups = []; // 按 appKey 分组后的列表（组内倒序，组间按 maxTs 倒序）
  const expanded = new Set(); // 已展开的 appKey
  let lastSeen = readLastSeen();
  let enabled = false;
  let panelOpen = false;
  let loadFailed = false;
  let view = todayInfo(); // 月历当前显示的年月
  let slowTimer = null;
  let fastTimer = null;
  let clockTimer = null;
  let listening = false;
  let media = null;
  const notifyUnread = typeof onUnreadChange === "function" ? onUnreadChange : () => {};

  // DOM 引用：必须等 innerHTML 落地后再取，否则全是 null
  let clockEl = null;
  let dateEl = null;
  let calEl = null;
  let listEl = null;

  if (el) {
    el.innerHTML = PANEL_HTML;
    clockEl = document.getElementById("np-clock");
    dateEl = document.getElementById("np-date");
    calEl = document.getElementById("np-calendar");
    listEl = document.getElementById("np-list");
    // 右栏媒体卡（自己管 start/stop）
    media = createMediaCard(document.getElementById("np-media"));
    bindDom();
  }

  // ---------- DOM 绑定 ----------

  function bindDom() {
    const clear = document.getElementById("np-clear");
    const sys = document.getElementById("np-open-sys");

    if (calEl) {
      calEl.addEventListener("click", (e) => {
        const btn = e.target.closest ? e.target.closest("[data-cal-dir]") : null;
        if (!btn) return;
        view = shiftView(view, Number(btn.dataset.calDir) || 0);
        renderCalendar(calEl, view);
      });
    }
    if (clear) {
      clear.addEventListener("click", onClearAll);
    }
    if (sys) {
      sys.addEventListener("click", () => {
        try {
          invoke("open_system_notification_center").catch(() => {});
        } catch (e) {
          /* 忽略 */
        }
      });
    }
    if (listEl) {
      // 应用分组：点卡片展开 / 收起该应用的全部通知
      listEl.addEventListener("click", (e) => {
        const card = e.target.closest ? e.target.closest(".ng-card") : null;
        if (!card) return;
        const wrap = card.closest(".ng");
        if (!wrap) return;
        const key = wrap.dataset.key || "";
        if (expanded.has(key)) expanded.delete(key);
        else expanded.add(key);
        renderList();
      });
    }
  }

  // ---------- 时钟 / 日期 ----------

  function pad2(n) {
    return n < 10 ? "0" + n : String(n);
  }

  function updateClock() {
    const now = new Date();
    if (clockEl) clockEl.textContent = pad2(now.getHours()) + ":" + pad2(now.getMinutes());
    if (dateEl) {
      dateEl.textContent =
        now.getFullYear() +
        "年" +
        (now.getMonth() + 1) +
        "月" +
        now.getDate() +
        "日 星期" +
        WEEKDAY_CN[now.getDay()];
    }
  }

  function startClock() {
    stopClock();
    updateClock();
    clockTimer = setInterval(updateClock, 1000);
  }

  function stopClock() {
    if (clockTimer) {
      clearInterval(clockTimer);
      clockTimer = null;
    }
  }

  // ---------- 数据 ----------

  function requestBackendRefresh() {
    try {
      invoke("refresh_notifications").catch(() => {});
    } catch (e) {
      /* Tauri 尚未注入时忽略 */
    }
  }

  async function refreshTick(first) {
    if (!enabled) return;
    requestBackendRefresh();
    try {
      const list = first
        ? await invokeRetry("get_notifications", undefined, { attempts: 3, perTryMs: 2500 })
        : await invoke("get_notifications");
      applyItems(list);
    } catch (e) {
      loadFailed = true;
      renderList();
    }
  }

  function applyItems(list) {
    if (!enabled) return; // 关掉入口后到达的迟到结果直接丢弃
    const arr = Array.isArray(list) ? list.filter((n) => n && typeof n === "object") : [];
    // 后端一般已按时间倒序，这里再兜底排一次，保证相对时间顺序稳定
    arr.sort((a, b) => (Number(b.tsMs) || 0) - (Number(a.tsMs) || 0));
    items = arr;
    loadFailed = false;
    // 清掉已不存在的应用的展开状态，避免 Set 无限增长
    const alive = new Set(arr.map((n) => String(n.appKey || n.appName || "system")));
    for (const k of Array.from(expanded)) {
      if (!alive.has(k)) expanded.delete(k);
    }
    renderList();
    const ts = latestTs(items);
    if (panelOpen && ts > lastSeen) {
      lastSeen = ts;
      writeLastSeen(ts);
    }
    // 未读判定：存在比「上次打开时看到的最大时间戳」更新的通知
    notifyUnread(!panelOpen && items.length > 0 && ts > lastSeen);
  }

  function latestTs(list) {
    let max = 0;
    for (const n of list) {
      const t = Number(n && n.tsMs) || 0;
      if (t > max) max = t;
    }
    return max;
  }

  /** 打开面板即视为已读 */
  function markSeen() {
    const ts = latestTs(items);
    if (ts > lastSeen) {
      lastSeen = ts;
      writeLastSeen(ts);
    }
    notifyUnread(false);
  }

  // ---------- 列表渲染（按 appKey 分组的堆叠卡片） ----------

  /** 按 appKey 分组；组内按 tsMs 倒序，组间按组内最大 tsMs 倒序 */
  function buildGroups(list) {
    const map = new Map();
    for (const n of list) {
      const key = String(n.appKey || n.appName || "system");
      let g = map.get(key);
      if (!g) {
        g = { key, appName: String(n.appName || "系统"), icon: null, maxTs: 0, items: [] };
        map.set(key, g);
      }
      g.items.push(n);
      const ts = Number(n.tsMs) || 0;
      if (ts > g.maxTs) g.maxTs = ts;
    }
    const out = Array.from(map.values());
    for (const g of out) {
      g.items.sort((a, b) => (Number(b.tsMs) || 0) - (Number(a.tsMs) || 0));
      const withIcon = g.items.find((it) => it.appIcon);
      g.icon = withIcon ? String(withIcon.appIcon) : null;
    }
    out.sort((a, b) => b.maxTs - a.maxTs);
    return out;
  }

  function renderList() {
    if (!listEl) return;
    const hidden = readHidden();
    visible = items.filter((n) => !hidden.has(String(n.id)));
    groups = buildGroups(visible);

    if (!groups.length) {
      // 读取失败与真正的空态给不同文案
      listEl.innerHTML =
        loadFailed && !items.length
          ? '<div class="np-empty">无法读取系统通知历史</div>'
          : '<div class="np-empty">没有通知</div>';
      listEl.scrollTop = 0;
      return;
    }
    // 展开/收起会整表重绘，记住滚动位置，避免用户点开一个组就被弹回顶部
    const keepTop = listEl.scrollTop;
    listEl.innerHTML = groups.map((g, i) => groupHtml(g, i)).join("");
    if (keepTop > 0) listEl.scrollTop = keepTop;
  }

  function iconHtml(group, index) {
    if (group.icon) return '<img src="' + escapeHtml(group.icon) + '" alt="">';
    const letter = escapeHtml((group.appName.slice(0, 1) || "?").toUpperCase());
    return (
      '<span class="ng-letter" style="background:' +
      LETTER_COLORS[index % LETTER_COLORS.length] +
      '">' +
      letter +
      "</span>"
    );
  }

  function groupHtml(group, index) {
    const isOpen = expanded.has(group.key);
    const newest = group.items[0] || {};
    // 背后最多两层堆叠（条数 =2 一层，≥3 两层）
    const layers = group.items.length >= 3 ? 2 : group.items.length === 2 ? 1 : 0;
    let stack = "";
    if (layers >= 1) stack += '<span class="ng-layer ng-layer-1"></span>';
    if (layers >= 2) stack += '<span class="ng-layer ng-layer-2"></span>';

    const meta =
      '<span class="ng-meta">' +
      '<span class="ng-app">' + escapeHtml(group.appName) + "</span>" +
      (group.items.length > 1 ? '<span class="ng-count">' + group.items.length + "</span>" : "") +
      '<span class="ng-time">' + escapeHtml(relTime(newest.tsMs)) + "</span>" +
      "</span>";

    const titleText = newest.title || newest.body || "（无内容）";
    const title = '<span class="ng-title">' + escapeHtml(titleText) + "</span>";
    const body =
      newest.title && newest.body ? '<span class="ng-body">' + escapeHtml(newest.body) + "</span>" : "";

    return (
      '<div class="ng' +
      (isOpen ? " expanded" : "") +
      (layers ? " stacked stacked-" + layers : "") +
      '" data-key="' +
      escapeHtml(group.key) +
      '">' +
      stack +
      '<button class="ng-card" type="button" aria-expanded="' + (isOpen ? "true" : "false") + '">' +
      '<span class="ng-icon">' + iconHtml(group, index) + "</span>" +
      '<span class="ng-main">' + meta + title + body + "</span>" +
      "</button>" +
      (isOpen ? '<div class="ng-items">' + group.items.map(itemHtml).join("") + "</div>" : "") +
      "</div>"
    );
  }

  function itemHtml(n) {
    const title = n.title ? '<div class="ng-item-title">' + escapeHtml(n.title) + "</div>" : "";
    const body = n.body ? '<div class="ng-item-body">' + escapeHtml(n.body) + "</div>" : "";
    return (
      '<div class="ng-item">' +
      title +
      body +
      '<div class="ng-item-time">' + escapeHtml(relTime(n.tsMs)) + "</div>" +
      "</div>"
    );
  }

  /** 相对时间：刚刚 / N分钟前 / 今天 HH:mm / 昨天 HH:mm / MM-DD */
  function relTime(tsMs) {
    const ts = Number(tsMs) || 0;
    if (!ts) return "";
    const now = Date.now();
    const d = new Date(ts);
    const diff = now - ts;
    if (diff < 60 * 1000) return "刚刚";
    if (diff < 60 * 60 * 1000) return Math.floor(diff / (60 * 1000)) + "分钟前";
    const hm = pad2(d.getHours()) + ":" + pad2(d.getMinutes());
    if (isSameDay(d, new Date(now))) return "今天 " + hm;
    if (isSameDay(d, new Date(now - 24 * 60 * 60 * 1000))) return "昨天 " + hm;
    return pad2(d.getMonth() + 1) + "-" + pad2(d.getDate());
  }

  function isSameDay(a, b) {
    return (
      a.getFullYear() === b.getFullYear() &&
      a.getMonth() === b.getMonth() &&
      a.getDate() === b.getDate()
    );
  }

  /** 「清除全部」：仅本地隐藏（sessionStorage 记 id），下次刷新列表时过滤 */
  function onClearAll() {
    if (!visible.length) return;
    const hidden = readHidden();
    for (const n of visible) hidden.add(String(n.id));
    writeHidden(hidden);
    renderList();
  }

  // ---------- 本地存储（读写失败不影响功能） ----------

  function readLastSeen() {
    try {
      return Number(window.localStorage.getItem(LAST_SEEN_KEY)) || 0;
    } catch (e) {
      return 0;
    }
  }

  function writeLastSeen(ts) {
    try {
      window.localStorage.setItem(LAST_SEEN_KEY, String(ts));
    } catch (e) {
      /* 忽略 */
    }
  }

  function readHidden() {
    try {
      const raw = window.sessionStorage.getItem(HIDDEN_KEY);
      const arr = raw ? JSON.parse(raw) : [];
      return new Set(Array.isArray(arr) ? arr.map(String) : []);
    } catch (e) {
      return new Set();
    }
  }

  function writeHidden(set) {
    try {
      window.sessionStorage.setItem(HIDDEN_KEY, JSON.stringify(Array.from(set)));
    } catch (e) {
      /* 忽略 */
    }
  }

  // ---------- 定时器 ----------

  function syncTimers() {
    if (slowTimer) {
      clearInterval(slowTimer);
      slowTimer = null;
    }
    if (fastTimer) {
      clearInterval(fastTimer);
      fastTimer = null;
    }
    if (!enabled) return;
    if (panelOpen) {
      fastTimer = setInterval(() => refreshTick(false), 5000); // 面板打开时每 5s
    } else {
      slowTimer = setInterval(() => refreshTick(false), 60000); // 常态每 60s
    }
  }

  function ensureListen() {
    if (listening) return;
    listening = true;
    try {
      const p = listen("notifications-changed", (e) => {
        if (!enabled) return;
        if (e && Array.isArray(e.payload)) applyItems(e.payload);
        else refreshTick(false);
      });
      if (p && typeof p.catch === "function") p.catch(() => {});
    } catch (e) {
      /* Tauri 尚未注入时忽略 */
    }
  }

  // ---------- 对外接口 ----------

  return {
    onOpen() {
      if (!el) return;
      panelOpen = true;
      view = todayInfo(); // 每次打开回到当前月份
      if (calEl) renderCalendar(calEl, view);
      startClock();
      syncTimers();
      ensureListen();
      if (media) media.start();
      refreshTick(false);
      markSeen();
    },
    onClose() {
      panelOpen = false;
      stopClock();
      syncTimers();
      if (media) media.stop();
    },
    setEnabled(v) {
      const next = !!v;
      if (next === enabled) {
        if (enabled) syncTimers();
        return;
      }
      enabled = next;
      if (!enabled) {
        panelOpen = false;
        stopClock();
        items = [];
        visible = [];
        groups = [];
        expanded.clear();
        loadFailed = false;
        if (listEl) listEl.innerHTML = "";
        if (media) media.stop();
        syncTimers();
        notifyUnread(false);
      } else {
        ensureListen();
        syncTimers();
        refreshTick(true);
      }
    },
    isEnabled() {
      return enabled;
    },
  };
}

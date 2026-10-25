// 通知中心面板：大时钟 + 中文月历 + 最近通知列表。
// 只在配置 notificationCenter 为 true 时由 topbar.js 调 setEnabled(true) 启动拉取。
import { invoke, invokeRetry, listen, escapeHtml } from "./api.js";
import { renderCalendar, todayInfo, shiftView } from "./calendar.js";

/** localStorage：上次打开面板时看到的最大通知时间戳（毫秒） */
const LAST_SEEN_KEY = "easydash.lastSeen";
/** sessionStorage：本地「清除全部」隐藏的通知 id 集合（刷新后重置） */
const HIDDEN_KEY = "easydash.hiddenNotifications";

const LETTER_COLORS = ["#3584e4", "#e66100", "#2ec27e", "#9141ac", "#e01b24", "#1c71d8", "#8f4e2b"];
const WEEKDAY_CN = ["日", "一", "二", "三", "四", "五", "六"];

// 面板骨架（内容由本模块填充；不依赖 topbar.html 里的静态结构）
const PANEL_HTML = `
  <div class="np-head">
    <div class="np-clock" id="np-clock">--:--</div>
    <div class="np-date" id="np-date"></div>
  </div>
  <div class="cal" id="np-calendar"></div>
  <div class="np-list" id="np-list"><div class="np-empty">没有通知</div></div>
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
  let lastSeen = readLastSeen();
  let enabled = false;
  let panelOpen = false;
  let loadFailed = false;
  let view = todayInfo(); // 月历当前显示的年月
  let slowTimer = null;
  let fastTimer = null;
  let clockTimer = null;
  let listening = false;
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

  // ---------- 列表渲染 ----------

  function renderList() {
    if (!listEl) {
      // innerHTML 被重新渲染过（不会发生）时兜底
      return;
    }
    const hidden = readHidden();
    visible = items.filter((n) => !hidden.has(String(n.id)));

    if (!visible.length) {
      // 读取失败与真正的空态给不同文案
      listEl.innerHTML =
        loadFailed && !items.length
          ? '<div class="np-empty">无法读取系统通知历史</div>'
          : '<div class="np-empty">没有通知</div>';
      return;
    }
    listEl.innerHTML = visible.map(itemHtml).join("");
  }

  function itemHtml(n, index) {
    const name = String(n.appName || "系统");
    const icon = n.appIcon
      ? '<img src="' + escapeHtml(n.appIcon) + '" alt="">'
      : '<span class="np-letter" style="background:' +
        LETTER_COLORS[index % LETTER_COLORS.length] +
        '">' +
        escapeHtml((name.slice(0, 1) || "?").toUpperCase()) +
        "</span>";
    const title = n.title ? '<div class="np-title">' + escapeHtml(n.title) + "</div>" : "";
    const body = n.body ? '<div class="np-body">' + escapeHtml(n.body) + "</div>" : "";
    return (
      '<div class="np-item">' +
      '<div class="np-icon">' + icon + "</div>" +
      '<div class="np-main">' +
      '<div class="np-app">' + escapeHtml(name) + "</div>" +
      title +
      body +
      "</div>" +
      '<div class="np-time">' + escapeHtml(relTime(n.tsMs)) + "</div>" +
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
    if (diff < 0) return "刚刚";
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
      refreshTick(false);
      markSeen();
    },
    onClose() {
      panelOpen = false;
      stopClock();
      syncTimers();
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
        loadFailed = false;
        if (listEl) listEl.innerHTML = "";
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

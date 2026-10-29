// 通知中心面板（双栏）：左栏大时钟 + 日期 + 应用分组（堆叠卡片）通知列表；
// 右栏中文月历 + 媒体卡；底部通栏「清除全部 / 在系统通知中心打开」。
// 只在配置 notificationCenter 为 true 时由 topbar.js 调 setEnabled(true) 启动拉取。
// 样式前缀 ntf- / cal- / med-（见 ui/css/panels.css）。
import { invoke, invokeRetry, listen, escapeHtml } from "./api.js";
import { renderCalendar, todayInfo, shiftView } from "./calendar.js";
import { createMediaCard } from "./media.js";
import { icon } from "./icons.js";

/** localStorage：上次打开面板时看到的最大通知时间戳（毫秒） */
const LAST_SEEN_KEY = "easydash.lastSeen";
/** sessionStorage：本地「清除全部」隐藏的通知 id 集合（刷新后重置） */
const HIDDEN_KEY = "easydash.hiddenNotifications";

const LETTER_COLORS = ["#3584e4", "#e66100", "#2ec27e", "#9141ac", "#e01b24", "#1c71d8", "#8f4e2b"];
const WEEKDAY_CN = ["日", "一", "二", "三", "四", "五", "六"];

/** 空态 / 失败态（图标 + 文案） */
function emptyHtml(text) {
  return (
    '<div class="ntf-empty">' +
    '<span class="ntf-empty-ico">' +
    icon("bellOff", { size: 48 }) +
    "</span>" +
    '<span class="ntf-empty-text">' +
    escapeHtml(text) +
    "</span>" +
    "</div>"
  );
}

/** Tauri 错误对象/字符串 → 可展示文本 */
function errText(e) {
  if (typeof e === "string") return e;
  if (e && typeof e.message === "string") return e.message;
  try {
    return String(e);
  } catch (_) {
    return "未知错误";
  }
}

// 面板骨架（内容由本模块填充；不依赖 topbar.html 里的静态结构）
const PANEL_HTML = `
  <div class="ntf-cols">
    <div class="ntf-left">
      <div class="ntf-head">
        <div class="ntf-clock" id="ntf-clock">--:--</div>
        <div class="ntf-date" id="ntf-date"></div>
      </div>
      <div class="ntf-list" id="ntf-list">${emptyHtml("没有通知")}</div>
    </div>
    <div class="ntf-right">
      <div class="ntf-cal" id="ntf-calendar"></div>
      <div class="ntf-media" id="ntf-media"></div>
    </div>
  </div>
  <div class="ntf-error hidden" id="ntf-error"></div>
  <div class="ntf-actions">
    <button class="ntf-action" id="ntf-clear" type="button">
      <span class="ntf-action-ico">${icon("trash", { size: 14 })}</span>清除全部
    </button>
    <button class="ntf-action" id="ntf-open-sys" type="button">
      <span class="ntf-action-ico">${icon("externalLink", { size: 14 })}</span>在系统通知中心打开
    </button>
  </div>
`;

/**
 * 创建通知中心面板控制器。
 * @param {{onUnreadChange?: (hasUnread: boolean) => void,
 *          onOpenApp?: () => void}} opts
 *   onOpenApp：成功打开来源应用后回调（顶栏用它收起面板）
 */
export function createNotificationsPanel({ onUnreadChange, onOpenApp } = {}) {
  const el = document.getElementById("panel-notifications");

  let items = []; // 后端返回的原始列表（按时间倒序）
  let visible = []; // 过滤本地隐藏后的可见列表
  let groups = []; // 按 appKey 分组后的列表（组内倒序，组间按 maxTs 倒序）
  const expanded = new Set(); // 已展开的 appKey
  let lastSeen = readLastSeen();
  let unreadBaseline = lastSeen; // 本次打开时的已读水位：组内最新时间超过它 → 显示未读点
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
  const notifyOpenApp = typeof onOpenApp === "function" ? onOpenApp : () => {};

  // DOM 引用：必须等 innerHTML 落地后再取，否则全是 null
  let clockEl = null;
  let dateEl = null;
  let calEl = null;
  let listEl = null;
  let errEl = null;
  let errorTimer = null;
  let enterTimer = null; // 入场错峰动画的收尾计时器
  let openingKey = ""; // 防连点：正在打开的应用分组

  if (el) {
    el.innerHTML = PANEL_HTML;
    clockEl = document.getElementById("ntf-clock");
    dateEl = document.getElementById("ntf-date");
    calEl = document.getElementById("ntf-calendar");
    listEl = document.getElementById("ntf-list");
    errEl = document.getElementById("ntf-error");
    // 右栏媒体卡（自己管 start/stop）
    media = createMediaCard(document.getElementById("ntf-media"));
    bindDom();
  }

  // ---------- DOM 绑定 ----------

  function bindDom() {
    const clear = document.getElementById("ntf-clear");
    const sys = document.getElementById("ntf-open-sys");

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
      // 应用分组：
      // - 点右上角「打开应用」/ 应用图标 / 展开后的单条通知 → 进入来源应用；
      // - 点卡片其余区域 → 展开 / 收起该应用的全部通知。
      listEl.addEventListener("click", (e) => {
        const groupEl = e.target.closest ? e.target.closest(".ntf-group") : null;
        if (!groupEl) return;
        const key = groupEl.dataset.key || "";
        const wantsOpen =
          e.target.closest(".ntf-open-app") ||
          e.target.closest(".ntf-icon") ||
          e.target.closest(".ntf-item");
        if (wantsOpen && groupEl.classList.contains("has-launch")) {
          openApp(key);
          return;
        }
        const card = e.target.closest(".ntf-card");
        if (!card) return;
        if (expanded.has(key)) expanded.delete(key);
        else expanded.add(key);
        renderList();
      });
      // 键盘：展开后的单条通知可用 Enter / Space 打开应用
      listEl.addEventListener("keydown", (e) => {
        if (e.key !== "Enter" && e.key !== " ") return;
        const item = e.target.closest ? e.target.closest(".ntf-item-clickable") : null;
        if (!item) return;
        e.preventDefault();
        const groupEl = item.closest(".ntf-group");
        if (groupEl) openApp(groupEl.dataset.key || "");
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

  /** 打开面板即视为已读（未读点水位取打开前的值，供本次会话标记「新」） */
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
        g = { key, appName: String(n.appName || "系统"), icon: null, launch: null, maxTs: 0, items: [] };
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
      // 启动目标：同一应用的通知处理器相同，取任意一条有 launch 的即可
      const withLaunch = g.items.find((it) => it.launch);
      g.launch = withLaunch ? String(withLaunch.launch) : null;
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
        loadFailed && !items.length ? emptyHtml("无法读取系统通知历史") : emptyHtml("没有通知");
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
      '<span class="ntf-letter" style="background:' +
      LETTER_COLORS[index % LETTER_COLORS.length] +
      '">' +
      letter +
      "</span>"
    );
  }

  function groupHtml(group, index) {
    const isOpen = expanded.has(group.key);
    const newest = group.items[0] || {};
    const isUnread = group.maxTs > unreadBaseline;
    const launchable = !!group.launch;
    const appLabel = escapeHtml(group.appName);
    // 背后最多两层堆叠（条数 =2 一层，≥3 两层）
    const layers = group.items.length >= 3 ? 2 : group.items.length === 2 ? 1 : 0;
    let stack = "";
    if (layers >= 1) stack += '<span class="ntf-layer ntf-layer-1"></span>';
    if (layers >= 2) stack += '<span class="ntf-layer ntf-layer-2"></span>';

    const meta =
      '<span class="ntf-meta">' +
      '<span class="ntf-app">' + escapeHtml(group.appName) + "</span>" +
      (group.items.length > 1
        ? '<span class="ntf-count">' + group.items.length + " 条</span>"
        : "") +
      (isUnread ? '<span class="ntf-dot" title="新通知"></span>' : "") +
      '<span class="ntf-time">' + escapeHtml(relTime(newest.tsMs)) + "</span>" +
      "</span>";

    const titleText = newest.title || newest.body || "（无内容）";
    const title = '<span class="ntf-title">' + escapeHtml(titleText) + "</span>";
    const body =
      newest.title && newest.body ? '<span class="ntf-body">' + escapeHtml(newest.body) + "</span>" : "";

    return (
      '<div class="ntf-group' +
      (isOpen ? " expanded" : "") +
      (layers ? " stacked stacked-" + layers : "") +
      (launchable ? " has-launch" : "") +
      '" data-key="' +
      escapeHtml(group.key) +
      '">' +
      stack +
      '<button class="ntf-card" type="button" aria-expanded="' +
      (isOpen ? "true" : "false") +
      '" aria-label="' +
      escapeHtml(group.appName + (group.items.length > 1 ? "，共 " + group.items.length + " 条通知" : "")) +
      '">' +
      '<span class="ntf-icon' +
      (launchable ? " ntf-icon-open" : "") +
      '"' +
      (launchable ? ' title="打开 ' + appLabel + '"' : "") +
      ">" +
      iconHtml(group, index) +
      "</span>" +
      '<span class="ntf-main">' + meta + title + body + "</span>" +
      "</button>" +
      // 「进入应用」入口：放在卡片右上角（兄弟节点，避免按钮嵌套）
      (launchable
        ? '<button class="ntf-open-app" type="button" title="打开 ' +
          appLabel +
          '" aria-label="打开 ' +
          appLabel +
          '">' +
          icon("externalLink", { size: 12 }) +
          "</button>"
        : "") +
      (isOpen ? '<div class="ntf-items">' + group.items.map((n) => itemHtml(n, group)).join("") + "</div>" : "") +
      "</div>"
    );
  }

  function itemHtml(n, group) {
    const title = n.title ? '<div class="ntf-item-title">' + escapeHtml(n.title) + "</div>" : "";
    const body = n.body ? '<div class="ntf-item-body">' + escapeHtml(n.body) + "</div>" : "";
    const clickable = !!(group && group.launch);
    return (
      '<div class="ntf-item' +
      (clickable ? " ntf-item-clickable" : "") +
      '"' +
      (clickable
        ? ' role="button" tabindex="0" title="打开 ' + escapeHtml(group.appName) + '"'
        : "") +
      ">" +
      title +
      body +
      '<div class="ntf-item-time">' + escapeHtml(relTime(n.tsMs)) + "</div>" +
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

  // ---------- 进入来源应用 ----------

  /** 打开分组对应的来源应用；后端只接受分组键，启动目标由通知缓存解析。 */
  function openApp(key) {
    if (!key || openingKey) return;
    const group = groups.find((g) => g.key === key);
    if (!group || !group.launch) return;
    openingKey = key;
    invoke("open_notification_app", { appKey: key })
      .then(() => {
        hideError();
        // 成功后面板由 topbar 收起（启动的应用同时会拿到前台）
        notifyOpenApp();
      })
      .catch((e) => {
        showError("打开应用失败：" + errText(e));
      })
      .finally(() => {
        openingKey = "";
      });
  }

  function showError(msg) {
    if (!errEl) return;
    errEl.textContent = msg;
    errEl.classList.remove("hidden");
    if (errorTimer) clearTimeout(errorTimer);
    errorTimer = setTimeout(() => {
      errorTimer = null;
      hideError();
    }, 3000);
  }

  function hideError() {
    if (errorTimer) {
      clearTimeout(errorTimer);
      errorTimer = null;
    }
    if (errEl) {
      errEl.classList.add("hidden");
      errEl.textContent = "";
    }
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

  /**
   * 通知分组错峰入场：每次打开面板重播一次（动画定义在 motion.css 的 .ntf-list.entering）。
   * 只加一个 900ms 的临时类，之后到达的新通知不再重播，避免列表抖动。
   */
  function playEnterStagger() {
    if (!listEl) return;
    listEl.classList.remove("entering");
    void listEl.offsetWidth; // 强制重排，连续打开也能重播
    listEl.classList.add("entering");
    if (enterTimer) clearTimeout(enterTimer);
    enterTimer = setTimeout(() => {
      enterTimer = null;
      if (listEl) listEl.classList.remove("entering");
    }, 900);
  }

  return {
    onOpen() {
      if (!el) return;
      panelOpen = true;
      hideError();
      // 记录打开前的已读水位：此后渲染出的、比它新的分组显示未读点
      unreadBaseline = lastSeen;
      view = todayInfo(); // 每次打开回到当前月份
      if (calEl) renderCalendar(calEl, view);
      startClock();
      syncTimers();
      ensureListen();
      if (media) media.start();
      playEnterStagger();
      refreshTick(false);
      markSeen();
    },
    onClose() {
      panelOpen = false;
      openingKey = "";
      hideError();
      stopClock();
      syncTimers();
      if (enterTimer) {
        clearTimeout(enterTimer);
        enterTimer = null;
      }
      if (listEl) listEl.classList.remove("entering");
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
        unreadBaseline = 0;
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

// 管理面板（manager 窗口）：通用 / 应用 / 托盘 / 关于 四个 tab。
//
// 约定：
// - 窗口 label=manager；显示时后端发 manager-open，关闭走 close_manager。
// - update_preferences({ patch }) 立即提交单字段开关，返回 warnings 字符串数组。
// - 托盘排序只提交 kind==="icon" 的 orderKey（overflow 不传），overflow 固定第一。
// - 固定应用用 set_pinned 提交「完整列表」实现增删与上下移。
// - 所有列表操作失败都回滚到「后端为准」的状态并弹红色 toast。
import { invoke, invokeRetry, listen, escapeHtml } from "./api.js";

/** 与 Dock / 应用中心一致的字母占位配色 */
const LETTER_COLORS = ["#3584e4", "#e66100", "#2ec27e", "#9141ac", "#e01b24", "#1c71d8", "#8f4e2b"];
/** icon-ready 防抖（毫秒）：UWP 图标是异步补的，短时间会连发多次 */
const ICON_DEBOUNCE_MS = 300;
/** toast 显示时长 */
const TOAST_MS = 3000;
/** 开关字段（camelCase，与 Config 一致） */
const PREF_FIELDS = ["fullscreenHide", "reserveTopbarSpace", "trayIcons", "notificationCenter", "controlCenter"];

// ---------- 状态 ----------
let config = null; // 最近一次 get_config
let info = null; // get_manager_info
let pinnedIcons = []; // (string|null)[]，与 config.pinned 顺序对应
let importedApps = []; // 应用中心导入应用（kind==="import"）
let trayItems = []; // 原始托盘项
let trayDisplay = []; // 展示顺序：overflow 在前 + icon
let patching = false; // 开关提交中
let opRunning = false; // 任意按钮操作中（防重复）
let listening = false;
let iconTimer = null;
let toastTimer = null;
let configEventTimer = null;
let lastConfigRefreshAt = 0; // 本端刚刷新过配置的时间（抑制紧随其后的 config-changed 重复拉取）
let lastAppsLoadAt = 0; // 本端刚刷新过应用列表的时间（同上，抑制 apps-changed 重复拉取）

// ---------- DOM ----------
const panelEl = document.getElementById("mp-panel");
const tabsEl = document.getElementById("mp-tabs");
const contentEl = document.getElementById("mp-content");
const toastEl = document.getElementById("mp-toast");
const pinnedListEl = document.getElementById("mp-pinned-list");
const importListEl = document.getElementById("mp-import-list");
const trayListEl = document.getElementById("mp-tray-list");
const hotkeyOverviewEl = document.getElementById("mp-hotkey-overview");
const hotkeyAppCenterEl = document.getElementById("mp-hotkey-appcenter");
const hotkeyWarnEl = document.getElementById("mp-hotkey-warn");
const saveHotkeysBtn = document.getElementById("mp-save-hotkeys");
const addPinnedBtn = document.getElementById("mp-add-pinned");
const importAppBtn = document.getElementById("mp-import-app");
const closeBtn = document.getElementById("mp-close");
const versionEl = document.getElementById("mp-version");
const identifierEl = document.getElementById("mp-identifier");
const configPathEl = document.getElementById("mp-config-path");
const switchInputs = panelEl ? Array.from(panelEl.querySelectorAll("input[data-pref]")) : [];

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

/** 底部 toast：ok=绿色，err=红色，3s 自动消失 */
function showToast(msg, kind) {
  if (!toastEl) return;
  toastEl.textContent = String(msg || "");
  toastEl.classList.remove("hidden", "ok", "err");
  toastEl.classList.add(kind === "ok" ? "ok" : "err");
  if (toastTimer) clearTimeout(toastTimer);
  toastTimer = setTimeout(() => {
    toastTimer = null;
    if (toastEl) toastEl.classList.add("hidden");
  }, TOAST_MS);
}

/** 按钮操作期间锁住整个面板，防重复点击 */
async function withBusy(btn, fn) {
  if (opRunning) return;
  opRunning = true;
  if (panelEl) panelEl.classList.add("mp-busy");
  if (btn) {
    btn.disabled = true;
    btn.classList.add("busy");
  }
  try {
    await fn();
  } finally {
    opRunning = false;
    if (panelEl) panelEl.classList.remove("mp-busy");
    if (btn) {
      btn.disabled = false;
      btn.classList.remove("busy");
    }
  }
}

/** 应用图标：有图标用 img，否则首字母色块 */
function iconHtml(icon, name, index) {
  if (icon) {
    return '<img class="mp-icon" src="' + escapeHtml(String(icon)) + '" alt="">';
  }
  const letter = escapeHtml((String(name || "?").slice(0, 1) || "?").toUpperCase());
  return (
    '<span class="mp-letter" style="background:' + LETTER_COLORS[index % LETTER_COLORS.length] + '">' + letter + "</span>"
  );
}

function escapeAttr(s) {
  return escapeHtml(String(s == null ? "" : s));
}

// ---------- tab 切换 ----------

function setTab(name) {
  if (!name) return;
  if (tabsEl) {
    for (const btn of tabsEl.querySelectorAll(".mp-tab")) {
      btn.classList.toggle("active", btn.dataset.tab === name);
    }
  }
  const sections = panelEl ? panelEl.querySelectorAll(".mp-section") : [];
  for (const sec of sections) {
    sec.classList.toggle("hidden", sec.id !== "tab-" + name);
  }
  if (contentEl) contentEl.scrollTop = 0;
}

// ---------- 渲染：通用 ----------

function renderSwitches() {
  for (const input of switchInputs) {
    const key = input.dataset.pref;
    if (PREF_FIELDS.indexOf(key) < 0) continue;
    input.checked = !!(config && config[key]);
    input.disabled = !config || patching;
  }
}

function renderHotkeys() {
  if (!config) return;
  // 正在输入时不覆盖用户输入
  if (hotkeyOverviewEl && document.activeElement !== hotkeyOverviewEl) {
    hotkeyOverviewEl.value = String(config.hotkey || "");
  }
  if (hotkeyAppCenterEl && document.activeElement !== hotkeyAppCenterEl) {
    hotkeyAppCenterEl.value = String(config.appCenterHotkey || "");
  }
}

function renderHotkeyWarnings(list) {
  if (!hotkeyWarnEl) return;
  const warnings = Array.isArray(list) ? list.filter(Boolean) : [];
  if (!warnings.length) {
    hotkeyWarnEl.textContent = "";
    hotkeyWarnEl.classList.add("hidden");
    return;
  }
  hotkeyWarnEl.textContent = warnings.join("\n");
  hotkeyWarnEl.classList.remove("hidden");
}

// ---------- 渲染：应用（固定 / 导入） ----------

function renderPinned() {
  if (!pinnedListEl) return;
  const pinned = config && Array.isArray(config.pinned) ? config.pinned : [];
  if (!pinned.length) {
    pinnedListEl.innerHTML = '<div class="mp-empty">还没有固定应用，点右上角「添加固定应用」</div>';
    return;
  }
  const rows = pinned.map((p, i) => {
    const item = p || {};
    const name = String(item.name || item.path || "应用");
    const path = String(item.path || "");
    return (
      '<div class="mp-item" data-index="' + i + '">' +
      '<div class="mp-item-icon">' + iconHtml(pinnedIcons[i], name, i) + "</div>" +
      '<div class="mp-item-main">' +
      '<div class="mp-item-name">' + escapeHtml(name) + "</div>" +
      '<div class="mp-item-path" title="' + escapeAttr(path) + '">' + escapeHtml(path) + "</div>" +
      "</div>" +
      '<div class="mp-item-actions">' +
      '<button class="mp-icon-btn" type="button" data-act="up" title="上移"' + (i === 0 ? " disabled" : "") + ">↑</button>" +
      '<button class="mp-icon-btn" type="button" data-act="down" title="下移"' +
      (i === pinned.length - 1 ? " disabled" : "") +
      ">↓</button>" +
      '<button class="mp-icon-btn danger" type="button" data-act="remove" title="从 Dock 移除">✕</button>' +
      "</div>" +
      "</div>"
    );
  });
  pinnedListEl.innerHTML = rows.join("");
}

function renderImported() {
  if (!importListEl) return;
  if (!importedApps.length) {
    importListEl.innerHTML = '<div class="mp-empty">还没有导入应用，点右上角「导入应用」</div>';
    return;
  }
  const rows = importedApps.map((app, i) => {
    const name = String(app.name || app.launch || "应用");
    const path = String(app.launch || "");
    const args = String(app.args || "");
    return (
      '<div class="mp-item" data-index="' + i + '">' +
      '<div class="mp-item-icon">' + iconHtml(app.icon, name, i) + "</div>" +
      '<div class="mp-item-main">' +
      '<div class="mp-item-name">' + escapeHtml(name) + "</div>" +
      '<div class="mp-item-path" title="' + escapeAttr(path + (args ? " " + args : "")) + '">' +
      escapeHtml(path) +
      (args ? " " + escapeHtml(args) : "") +
      "</div>" +
      "</div>" +
      '<div class="mp-item-actions">' +
      '<button class="mp-icon-btn danger" type="button" data-act="remove" title="移除导入应用">✕</button>' +
      "</div>" +
      "</div>"
    );
  });
  importListEl.innerHTML = rows.join("");
}

// ---------- 渲染：托盘 ----------

function buildTrayDisplay() {
  const overflow = [];
  const icons = [];
  for (const item of trayItems) {
    if (!item) continue;
    if (item.kind === "overflow") overflow.push(item);
    else icons.push(item);
  }
  trayDisplay = overflow.concat(icons);
}

function renderTray() {
  if (!trayListEl) return;
  buildTrayDisplay();
  if (!trayDisplay.length) {
    trayListEl.innerHTML = '<div class="mp-empty">托盘镜像不可用（需要 Windows 11 的 XAML 任务栏，且「顶栏托盘镜像」已开启）</div>';
    return;
  }
  const icons = trayDisplay.filter((t) => t.kind === "icon");
  let iconIndex = 0;
  const rows = trayDisplay.map((item, i) => {
    const isOverflow = item.kind === "overflow";
    const name = String(item.name || (isOverflow ? "显示隐藏的图标" : "托盘图标"));
    const upDisabled = isOverflow || iconIndex <= 0;
    const downDisabled = isOverflow || iconIndex >= icons.length - 1;
    if (!isOverflow) iconIndex++;
    return (
      '<div class="mp-item' + (isOverflow ? " mp-item-static" : "") + '" data-index="' + i + '">' +
      '<div class="mp-item-icon">' + iconHtml(item.icon, name, i) + "</div>" +
      '<div class="mp-item-main">' +
      '<div class="mp-item-name">' +
      escapeHtml(name) +
      (isOverflow ? ' <span class="mp-badge">固定</span>' : "") +
      "</div>" +
      '<div class="mp-item-path">' +
      (isOverflow ? "始终排在最前，不参与排序" : escapeHtml(String(item.orderKey || ""))) +
      "</div>" +
      "</div>" +
      '<div class="mp-item-actions">' +
      '<button class="mp-icon-btn" type="button" data-act="up" title="上移"' + (upDisabled ? " disabled" : "") + ">↑</button>" +
      '<button class="mp-icon-btn" type="button" data-act="down" title="下移"' + (downDisabled ? " disabled" : "") + ">↓</button>" +
      "</div>" +
      "</div>"
    );
  });
  trayListEl.innerHTML = rows.join("");
}

// ---------- 渲染：关于 ----------

function renderAbout() {
  if (versionEl) versionEl.textContent = info && info.version ? String(info.version) : "--";
  if (identifierEl) identifierEl.textContent = info && info.identifier ? String(info.identifier) : "--";
  const path = info && info.configPath ? String(info.configPath) : "";
  if (configPathEl) {
    configPathEl.textContent = path || "--";
    configPathEl.title = path;
  }
}

// ---------- 读取数据 ----------

async function loadInfo() {
  try {
    info = await invoke("get_manager_info");
  } catch (e) {
    info = null;
  }
  renderAbout();
}

async function loadConfig(first) {
  try {
    config = first
      ? await invokeRetry("get_config", undefined, { attempts: 3, perTryMs: 3000 })
      : await invoke("get_config");
  } catch (e) {
    config = null;
    showToast("读取配置失败：" + errText(e), "err");
  }
  renderSwitches();
  renderHotkeys();
  renderPinned();
  renderImported();
  return !!config;
}

/** 静默重读配置（不弹错误 toast，用于操作成功/收到 config-changed 后对齐真实状态） */
async function refreshConfigSilent() {
  lastConfigRefreshAt = Date.now();
  try {
    const next = await invoke("get_config");
    if (next && typeof next === "object") config = next;
  } catch (e) {
    /* 读失败时保留本地状态 */
  }
  renderSwitches();
  renderHotkeys();
  renderPinned();
}

async function loadPinnedIcons() {
  const pinned = config && Array.isArray(config.pinned) ? config.pinned : [];
  if (!pinned.length) {
    pinnedIcons = [];
    renderPinned();
    return;
  }
  try {
    const icons = await invoke("get_pinned_icons", { paths: pinned.map((p) => String((p && p.path) || "")) });
    pinnedIcons = Array.isArray(icons) ? icons : [];
  } catch (e) {
    pinnedIcons = [];
  }
  renderPinned();
}

async function loadImportedApps() {
  lastAppsLoadAt = Date.now();
  try {
    const list = await invoke("get_apps");
    importedApps = (Array.isArray(list) ? list : []).filter((a) => a && a.kind === "import");
  } catch (e) {
    importedApps = [];
  }
  renderImported();
}

async function loadTrayItems() {
  try {
    const list = await invoke("get_tray_items");
    trayItems = Array.isArray(list) ? list : [];
  } catch (e) {
    trayItems = [];
  }
  renderTray();
}

/** 打开面板 / 首屏加载：全量拉一次 */
async function loadAll() {
  renderHotkeyWarnings([]);
  await loadInfo();
  const ok = await loadConfig(true);
  if (!ok) {
    // 配置读不到时至少把列表置成空态，避免一直显示「加载中…」
    renderPinned();
    renderImported();
    renderTray();
    return;
  }
  await Promise.all([loadPinnedIcons(), loadImportedApps(), loadTrayItems()]);
}

// ---------- 操作：通用 ----------

async function submitPreference(field, value) {
  if (!config || patching) return;
  const prev = config[field];
  // 先乐观更新，失败再回滚
  config[field] = value;
  patching = true;
  renderSwitches();
  try {
    const warnings = await invoke("update_preferences", { patch: { [field]: value } });
    if (Array.isArray(warnings) && warnings.length) {
      showToast(warnings[0], "err");
    }
    await refreshConfigSilent();
  } catch (e) {
    config[field] = prev;
    showToast("保存失败：" + errText(e), "err");
  } finally {
    patching = false;
    renderSwitches();
  }
}

async function saveHotkeys(btn) {
  const hotkey = hotkeyOverviewEl ? hotkeyOverviewEl.value.trim() : "";
  const appCenterHotkey = hotkeyAppCenterEl ? hotkeyAppCenterEl.value.trim() : "";
  if (!hotkey || !appCenterHotkey) {
    showToast("热键不能为空（例如 alt+f1 / alt+space）", "err");
    return;
  }
  await withBusy(btn, async () => {
    try {
      const warnings = await invoke("update_preferences", { patch: { hotkey, appCenterHotkey } });
      const list = Array.isArray(warnings) ? warnings.filter(Boolean) : [];
      renderHotkeyWarnings(list);
      if (list.length) showToast("热键未完全生效：" + list[0], "err");
      else showToast("热键已保存", "ok");
      await refreshConfigSilent();
    } catch (e) {
      showToast("保存热键失败：" + errText(e), "err");
    }
  });
}

async function reloadConfig(btn) {
  await withBusy(btn, async () => {
    try {
      await invoke("reload_config");
      await loadAll();
      showToast("配置已重载", "ok");
    } catch (e) {
      showToast("重载失败：" + errText(e), "err");
    }
  });
}

async function openConfigFolder(btn) {
  await withBusy(btn, async () => {
    try {
      await invoke("open_config_folder");
      showToast("已打开配置目录", "ok");
    } catch (e) {
      showToast("打开失败：" + errText(e), "err");
    }
  });
}

// ---------- 操作：应用 / Dock ----------

async function addPinnedApp(btn) {
  await withBusy(btn, async () => {
    let path = null;
    try {
      path = await invoke("pick_import_file");
    } catch (e) {
      showToast("选择文件失败：" + errText(e), "err");
      return;
    }
    if (!path) return; // 取消选择
    try {
      const entry = await invoke("add_pinned_app", { path });
      await refreshConfigSilent();
      await loadPinnedIcons();
      showToast("已添加 " + ((entry && entry.name) || "应用"), "ok");
    } catch (e) {
      showToast("添加失败：" + errText(e), "err");
    }
  });
}

async function movePinned(index, delta, btn) {
  if (!config || !Array.isArray(config.pinned)) return;
  const list = config.pinned.slice();
  const to = index + delta;
  if (to < 0 || to >= list.length) return;
  const tmp = list[index];
  list[index] = list[to];
  list[to] = tmp;
  await withBusy(btn, async () => {
    try {
      await invoke("set_pinned", { pinned: list });
      config.pinned = list;
      renderPinned();
      await loadPinnedIcons();
    } catch (e) {
      showToast("调整顺序失败：" + errText(e), "err");
    }
  });
}

async function removePinned(index, btn) {
  if (!config || !Array.isArray(config.pinned)) return;
  const list = config.pinned.slice();
  const removed = list[index] || {};
  list.splice(index, 1);
  await withBusy(btn, async () => {
    try {
      await invoke("set_pinned", { pinned: list });
      config.pinned = list;
      renderPinned();
      await loadPinnedIcons();
      showToast("已从 Dock 移除 " + (removed.name || removed.path || "应用"), "ok");
    } catch (e) {
      showToast("删除失败：" + errText(e), "err");
    }
  });
}

async function resetDockOrder(btn) {
  await withBusy(btn, async () => {
    try {
      await invoke("set_dock_order", { keys: [] });
      showToast("已重置 Dock 未固定排序", "ok");
    } catch (e) {
      showToast("重置失败：" + errText(e), "err");
    }
  });
}

// ---------- 操作：应用中心导入应用 ----------

async function importAppEntry(btn) {
  await withBusy(btn, async () => {
    let path = null;
    try {
      path = await invoke("pick_import_file");
    } catch (e) {
      showToast("选择文件失败：" + errText(e), "err");
      return;
    }
    if (!path) return;
    try {
      const entry = await invoke("import_app", { path });
      await loadImportedApps();
      showToast("已导入 " + ((entry && entry.name) || "应用"), "ok");
    } catch (e) {
      showToast("导入失败：" + errText(e), "err");
    }
  });
}

async function removeImportedApp(index, btn) {
  const app = importedApps[index];
  if (!app || !app.id) return;
  await withBusy(btn, async () => {
    try {
      await invoke("remove_imported_app", { id: app.id });
      await loadImportedApps();
      showToast("已移除 " + (app.name || "应用"), "ok");
    } catch (e) {
      showToast("移除失败：" + errText(e), "err");
    }
  });
}

async function resetAppOrder(btn) {
  await withBusy(btn, async () => {
    try {
      await invoke("set_app_order", { order: [] });
      showToast("已重置应用中心排序", "ok");
    } catch (e) {
      showToast("重置失败：" + errText(e), "err");
    }
  });
}

// ---------- 操作：托盘 ----------

/** 只在 icon 之间上下移动；overflow 永远固定在第一位，不参与提交 */
async function moveTrayIcon(displayIndex, delta, btn) {
  const item = trayDisplay[displayIndex];
  if (!item || item.kind !== "icon") return;
  const icons = trayDisplay.filter((t) => t && t.kind === "icon");
  const from = icons.indexOf(item);
  const to = from + delta;
  if (from < 0 || to < 0 || to >= icons.length) return;

  const list = icons.slice();
  const tmp = list[from];
  list[from] = list[to];
  list[to] = tmp;
  const order = list.map((t) => String(t.orderKey || "")).filter(Boolean);
  if (!order.length) return;

  await withBusy(btn, async () => {
    try {
      await invoke("set_tray_order", { order });
      // 本地按新顺序重建：overflow 在前 + 新 icon 顺序
      const overflow = trayDisplay.filter((t) => t && t.kind === "overflow");
      trayItems = overflow.concat(list);
      renderTray();
    } catch (e) {
      showToast("调整托盘顺序失败：" + errText(e), "err");
    }
  });
}

async function resetTrayOrder(btn) {
  await withBusy(btn, async () => {
    try {
      await invoke("set_tray_order", { order: [] });
      await loadTrayItems();
      showToast("已重置托盘顺序", "ok");
    } catch (e) {
      showToast("重置失败：" + errText(e), "err");
    }
  });
}

// ---------- 关闭 ----------

async function closeManager() {
  try {
    await invoke("close_manager");
  } catch (e) {
    showToast("关闭失败：" + errText(e), "err");
  }
}

// ---------- 事件绑定 ----------

const ACTIONS = {
  "reload-config": (btn) => reloadConfig(btn),
  "open-config": (btn) => openConfigFolder(btn),
  "reset-dock-order": (btn) => resetDockOrder(btn),
  "reset-app-order": (btn) => resetAppOrder(btn),
  "reset-tray-order": (btn) => resetTrayOrder(btn),
};

function bindEvents() {
  // tab 切换
  if (tabsEl) {
    tabsEl.addEventListener("click", (e) => {
      const btn = e.target.closest ? e.target.closest(".mp-tab") : null;
      if (btn && btn.dataset.tab) setTab(btn.dataset.tab);
    });
  }

  if (closeBtn) closeBtn.addEventListener("click", closeManager);
  if (saveHotkeysBtn) saveHotkeysBtn.addEventListener("click", () => saveHotkeys(saveHotkeysBtn));
  if (addPinnedBtn) addPinnedBtn.addEventListener("click", () => addPinnedApp(addPinnedBtn));
  if (importAppBtn) importAppBtn.addEventListener("click", () => importAppEntry(importAppBtn));

  // 开关：切换即提交单个字段
  if (panelEl) {
    panelEl.addEventListener("change", (e) => {
      const input = e.target;
      if (!input || input.type !== "checkbox" || !input.dataset.pref) return;
      const field = input.dataset.pref;
      if (PREF_FIELDS.indexOf(field) < 0) return;
      if (input.disabled) return;
      submitPreference(field, !!input.checked);
    });

    // data-action 按钮（重载 / 打开目录 / 各种重置）
    panelEl.addEventListener("click", (e) => {
      const btn = e.target.closest ? e.target.closest("[data-action]") : null;
      if (!btn) return;
      const fn = ACTIONS[btn.dataset.action];
      if (fn) fn(btn);
    });
  }

  // 固定应用列表：上移 / 下移 / 删除
  if (pinnedListEl) {
    pinnedListEl.addEventListener("click", (e) => {
      const btn = e.target.closest ? e.target.closest("[data-act]") : null;
      if (!btn || btn.disabled) return;
      const row = btn.closest(".mp-item");
      const index = Number(row && row.dataset.index);
      if (!Number.isInteger(index)) return;
      const act = btn.dataset.act;
      if (act === "up") movePinned(index, -1, btn);
      else if (act === "down") movePinned(index, 1, btn);
      else if (act === "remove") removePinned(index, btn);
    });
  }

  // 导入应用列表：移除
  if (importListEl) {
    importListEl.addEventListener("click", (e) => {
      const btn = e.target.closest ? e.target.closest("[data-act]") : null;
      if (!btn || btn.disabled) return;
      const row = btn.closest(".mp-item");
      const index = Number(row && row.dataset.index);
      if (!Number.isInteger(index)) return;
      if (btn.dataset.act === "remove") removeImportedApp(index, btn);
    });
  }

  // 托盘列表：上移 / 下移
  if (trayListEl) {
    trayListEl.addEventListener("click", (e) => {
      const btn = e.target.closest ? e.target.closest("[data-act]") : null;
      if (!btn || btn.disabled) return;
      const row = btn.closest(".mp-item");
      const index = Number(row && row.dataset.index);
      if (!Number.isInteger(index)) return;
      const act = btn.dataset.act;
      if (act === "up") moveTrayIcon(index, -1, btn);
      else if (act === "down") moveTrayIcon(index, 1, btn);
    });
  }

  // Esc 关闭窗口
  document.addEventListener("keydown", (e) => {
    if (e.isComposing) return;
    if (e.key === "Escape") {
      e.preventDefault();
      closeManager();
    }
  });

  // 窗口重新可见时（有的环境下 manager-open 可能早于监听注册）兜底刷新
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") {
      // 数据为空说明首屏没加载成功，补一次
      if (!config) loadAll();
    }
  });
}

/** 后端事件：打开面板 / 配置变更 / 托盘变更 / 应用变更 / 图标就绪 */
function ensureListeners() {
  if (listening) return;
  try {
    const handlers = [
      ["manager-open", () => {
        setTab("general");
        renderHotkeyWarnings([]);
        loadAll();
      }],
      ["config-changed", () => {
        // 本端刚刷新过就不再重复拉（避免一次操作触发两次 get_config）
        if (configEventTimer) clearTimeout(configEventTimer);
        configEventTimer = setTimeout(() => {
          configEventTimer = null;
          if (Date.now() - lastConfigRefreshAt < 400) return;
          refreshConfigSilent();
        }, 120);
      }],
      ["tray-changed", () => {
        loadTrayItems();
      }],
      ["apps-changed", () => {
        // 本端刚刷新过就不再重复拉（例如导入成功后自己已经 refresh 过）
        if (Date.now() - lastAppsLoadAt < 400) return;
        loadImportedApps();
      }],
      ["icon-ready", () => {
        if (iconTimer) clearTimeout(iconTimer);
        iconTimer = setTimeout(() => {
          iconTimer = null;
          loadPinnedIcons();
        }, ICON_DEBOUNCE_MS);
      }],
    ];
    for (const [name, fn] of handlers) {
      const p = listen(name, fn);
      if (p && typeof p.catch === "function") p.catch(() => {});
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

// ---------- 启动 ----------

async function main() {
  bindEvents();
  ensureListeners();
  setTab("general");

  await waitTauri();
  ensureListeners();
  // 管理面板窗口启动时是隐藏的：不预拉数据，首次显示由 manager-open 触发；
  // 若页面加载时窗口已可见（首次打开早于脚本就绪），补一次全量加载。
  if (document.visibilityState === "visible") await loadAll();
}

main().catch((e) => {
  console.error("[管理面板] 启动失败", e);
});

// 顶栏入口模块：日期时钟、左侧「活动」、中间时钟（通知中心入口）、
// 右侧托盘镜像 + 控制中心入口，负责把各功能模块装配到面板管理器上。
//
// 依赖方向：本模块是唯一入口（topbar.html 只引它），其余都是被它 import 的叶子模块，
// 不存在循环依赖：panels.js 只依赖 api.js。
import { invoke, invokeRetry, listen } from "./api.js";
import { initPanels, registerPanel, togglePanel, closePanels, isPanelOpen, BAR_HEIGHT } from "./panels.js";
import { createNotificationsPanel } from "./notifications.js";
import { createControlCenterPanel, ICONS } from "./control-center.js";
import { startTray, stopTray } from "./tray.js";

const WEEKDAY_CN = ["日", "一", "二", "三", "四", "五", "六"];

/** get_config 缺失字段时的兜底（与 config.rs 默认值一致） */
const DEFAULT_CONFIG = { trayIcons: true, notificationCenter: true, controlCenter: true };

let config = Object.assign({}, DEFAULT_CONFIG);
let ready = false; // 配置就绪后才允许开面板（否则窗口还是 32px 高）
let notificationsPanel = null;
let controlPanel = null;

const clockBtn = document.getElementById("clock-btn");
const ccBtn = document.getElementById("cc-btn");
const ccIconsEl = document.getElementById("cc-icons");
const dateEl = document.getElementById("date");
const clockEl = document.getElementById("clock");
const unreadDot = document.getElementById("unread-dot");
const trayEl = document.getElementById("tray");

// ---------- 顶部时钟 ----------

function pad2(n) {
  return n < 10 ? "0" + n : String(n);
}

function tickClock() {
  const now = new Date();
  if (dateEl) {
    dateEl.textContent =
      now.getMonth() + 1 + "月" + now.getDate() + "日 周" + WEEKDAY_CN[now.getDay()];
  }
  if (clockEl) clockEl.textContent = pad2(now.getHours()) + ":" + pad2(now.getMinutes());
}

// ---------- 未读红点 ----------

function setUnread(hasUnread) {
  if (unreadDot) unreadDot.classList.toggle("hidden", !hasUnread);
}

// ---------- 右侧聚合图标（网络 / 音量 / 电池） ----------

function renderStatusIcons(s) {
  if (!ccIconsEl || !s) return;
  const wifiOn = !!(s.network || (s.wifi && (s.wifi.enabled || s.wifi.connected)));
  const muted = s.muted === true;
  const vol = typeof s.volume === "number" ? s.volume : 0;

  let html = wifiOn ? ICONS.wifi : ICONS.wifiOff;
  html += muted || vol === 0 ? ICONS.volumeMuted : ICONS.volume;

  if (s.battery !== null && s.battery !== undefined) {
    const pct = Math.max(6, Math.min(100, s.battery));
    const cls = s.charging ? " charging" : s.battery <= 15 ? " low" : "";
    html +=
      '<span class="cc-bat-icon"><span class="cc-bat-fill' +
      cls +
      '" style="width:' +
      pct +
      '%"></span></span>';
    html += '<span class="cc-bat-pct">' + s.battery + "%</span>";
  }
  ccIconsEl.innerHTML = html;
}

// ---------- 按钮绑定 ----------

function bindButtons() {
  const activities = document.getElementById("activities");
  if (activities) {
    activities.addEventListener("click", () => {
      try {
        invoke("toggle_overview").catch(() => {});
      } catch (e) {
        /* Tauri 尚未注入时忽略 */
      }
    });
  }

  if (clockBtn) {
    clockBtn.addEventListener("click", () => {
      if (!ready || config.notificationCenter === false) return;
      togglePanel("notifications");
    });
  }

  if (ccBtn) {
    ccBtn.addEventListener("click", () => {
      if (!ready || config.controlCenter === false) return;
      togglePanel("control");
    });
  }

  const gearBtn = document.getElementById("gear-btn");
  if (gearBtn) {
    gearBtn.addEventListener("click", () => {
      if (!ready) return;
      try {
        invoke("toggle_manager").catch(() => {});
      } catch (e) {
        /* Tauri 尚未注入时忽略 */
      }
    });
  }
}

// ---------- 面板模块装配 ----------

function createPanels() {
  notificationsPanel = createNotificationsPanel({ onUnreadChange: setUnread });
  registerPanel("notifications", {
    onOpen: () => notificationsPanel.onOpen(),
    onClose: () => notificationsPanel.onClose(),
  });

  controlPanel = createControlCenterPanel({ onState: renderStatusIcons });
  registerPanel("control", {
    onOpen: () => controlPanel.onOpen(),
    onClose: () => controlPanel.onClose(),
  });
}

// ---------- 配置应用 ----------

function applyConfig(cfg) {
  config = Object.assign({}, DEFAULT_CONFIG, cfg || {});
  const notiOn = config.notificationCenter !== false;
  const ccOn = config.controlCenter !== false;
  const trayOn = config.trayIcons !== false;

  // 入口显隐：false 时不渲染入口、不发起对应刷新
  if (clockBtn) clockBtn.disabled = !notiOn;
  if (!notiOn) setUnread(false);
  if (ccBtn) ccBtn.classList.toggle("hidden", !ccOn);
  if (trayEl) trayEl.classList.toggle("hidden", !trayOn);

  if (notificationsPanel) notificationsPanel.setEnabled(notiOn);
  if (controlPanel) controlPanel.setEnabled(ccOn);

  if (trayOn) startTray();
  else stopTray();

  // 入口被关掉时顺手收起对应面板
  if (!notiOn && isPanelOpen("notifications")) closePanels();
  if (!ccOn && isPanelOpen("control")) closePanels();
}

// ---------- 全局事件 ----------

function safeListen(name, handler) {
  try {
    const p = listen(name, handler);
    if (p && typeof p.catch === "function") p.catch(() => {});
  } catch (e) {
    /* Tauri 尚未注入时忽略 */
  }
}

function listenEvents() {
  // 概览打开时后端通知顶栏收起面板
  safeListen("close-panels", () => closePanels());

  // 全屏隐藏：先收面板（窗口缩回 32px）再隐藏
  safeListen("fullscreen-changed", (e) => {
    if (e && e.payload === true) {
      closePanels();
      document.body.classList.add("hidden");
    } else {
      document.body.classList.remove("hidden");
    }
  });

  // 配置热更新
  safeListen("config-changed", (e) => {
    if (e && e.payload) applyConfig(e.payload);
  });
}

// ---------- 启动 ----------

async function waitTauri(timeoutMs = 15000) {
  const t0 = Date.now();
  while (!(window.__TAURI__ && window.__TAURI__.core)) {
    if (Date.now() - t0 > timeoutMs) throw new Error("Tauri API 注入超时");
    await new Promise((r) => setTimeout(r, 100));
  }
}

async function main() {
  // 时钟和骨架先起来：即使 IPC 注入慢，顶栏也能正常显示时间
  tickClock();
  setInterval(tickClock, 1000);

  initPanels();
  bindButtons();
  createPanels();

  try {
    await waitTauri();
    config = await invokeRetry("get_config", undefined, { attempts: 5, perTryMs: 2500 });
  } catch (e) {
    console.warn("[顶栏] 读取配置失败，使用默认配置", e);
    config = Object.assign({}, DEFAULT_CONFIG);
  }

  // 自愈：页面被重载（开发刷新 / 看门狗）时窗口可能还停留在面板高度，
  // 这里把窗口复位到常态 32px，否则整块透明区域会一直挡在上方。
  if (Math.abs(window.innerHeight - BAR_HEIGHT) > 1) {
    try {
      invoke("resize_topbar", { height: BAR_HEIGHT }).catch(() => {});
    } catch (e) {
      /* 忽略 */
    }
  }

  applyConfig(config);
  listenEvents();
  ready = true;
}

main().catch((e) => {
  // 细节已由 topbar.html 里的独立错误横幅显示，这里保证不静默
  console.error("[顶栏] 启动失败", e);
});

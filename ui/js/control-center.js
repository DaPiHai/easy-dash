// 控制中心面板：3 张快捷磁贴（Wi-Fi / 蓝牙 / 飞行模式）+ 外观（明暗主题 / 界面材质）
// + 音量与亮度滑条 + 电源模式三档 + 电池行与电源动作。
// 磁贴交互：Wi-Fi / 蓝牙的图标泡 = 开关，文字区 = 进入对应连接子视图（connections.js）；
// 飞行模式整块 = 开关。操作全部「乐观更新 + 面板打开时每 2s 轮询 get_quick_state 纠偏」。
// 外观走 theme.js（setThemeMode / setMaterialStyle），状态由 subscribeTheme 回推选中态。
import { invoke, invokeRetry, listen } from "./api.js";
import { createConnections } from "./connections.js";
import { icon } from "./icons.js";
import {
  getThemeState,
  subscribeTheme,
  setThemeMode,
  setMaterialStyle,
  MATERIAL_STYLES,
} from "./theme.js";
import { createWheelStepper } from "./wheel.js";
import { attachSegIndicators } from "./seg-thumb.js";

// topbar.js 仍从这里取聚合图标：保持旧导出路径不变
export { ICONS } from "./icons.js";

/** 明暗主题两档（顺序即渲染顺序） */
const THEME_MODES = ["light", "dark"];

/** 界面材质三档的展示名（顺序取 theme.js 的 MATERIAL_STYLES，与后端白名单一致） */
const MATERIAL_LABELS = {
  solid: "标准",
  liquid: "液态玻璃",
  glow: "柔光玻璃",
};

/** 滚轮每一档调节的百分点（音量 / 亮度共用） */
const WHEEL_STEP = 5;

const PANEL_HTML = `
  <div class="ctl-error hidden" id="ctl-error"></div>
  <div class="ctl-main" id="ctl-main">
    <div class="ctl-tiles">
      <!-- Wi-Fi：图标泡 = 开关，其余区域 = 进入 Wi-Fi 列表 -->
      <div class="ctl-tile" id="ctl-wifi">
        <button class="ctl-tile-toggle" id="ctl-wifi-toggle" type="button" aria-pressed="false"
                aria-label="开启 Wi-Fi" title="开启 Wi-Fi">
          <span class="ctl-tile-ico" id="ctl-wifi-icon"></span>
        </button>
        <button class="ctl-tile-open" id="ctl-wifi-open" type="button"
                aria-label="打开 Wi-Fi 网络列表" title="Wi-Fi 网络列表">
          <span class="ctl-tile-text">
            <span class="ctl-tile-name">Wi-Fi</span>
            <span class="ctl-tile-sub" id="ctl-wifi-sub">--</span>
          </span>
          <span class="ctl-tile-chevron">${icon("chevronRight", { size: 14 })}</span>
        </button>
      </div>
      <!-- 蓝牙：图标泡 = 开关，其余区域 = 进入蓝牙设备列表 -->
      <div class="ctl-tile" id="ctl-bt">
        <button class="ctl-tile-toggle" id="ctl-bt-toggle" type="button" aria-pressed="false"
                aria-label="开启蓝牙" title="开启蓝牙">
          <span class="ctl-tile-ico" id="ctl-bt-icon"></span>
        </button>
        <button class="ctl-tile-open" id="ctl-bt-open" type="button"
                aria-label="打开蓝牙设备列表" title="蓝牙设备列表">
          <span class="ctl-tile-text">
            <span class="ctl-tile-name">蓝牙</span>
            <span class="ctl-tile-sub" id="ctl-bt-sub">--</span>
          </span>
          <span class="ctl-tile-chevron">${icon("chevronRight", { size: 14 })}</span>
        </button>
      </div>
      <!-- 飞行模式：整块单按钮 -->
      <button class="ctl-tile ctl-tile-solo" id="ctl-airplane" type="button" aria-pressed="false"
              aria-label="开启飞行模式" title="开启飞行模式">
        <span class="ctl-tile-ico" id="ctl-airplane-icon"></span>
        <span class="ctl-tile-text">
          <span class="ctl-tile-name">飞行模式</span>
          <span class="ctl-tile-sub" id="ctl-airplane-sub">--</span>
        </span>
      </button>
    </div>

    <section class="ctl-card">
      <div class="ctl-card-head">
        <span class="ctl-card-title">外观</span>
        <span class="ctl-card-sub" id="ctl-theme-sub">--</span>
      </div>
      <div class="ctl-seg ctl-theme" id="ctl-theme" role="group" aria-label="明暗主题">
        <button type="button" data-mode="light">
          <span class="ctl-seg-ico">${icon("sun", { size: 14 })}</span>浅色
        </button>
        <button type="button" data-mode="dark">
          <span class="ctl-seg-ico">${icon("moon", { size: 14 })}</span>深色
        </button>
      </div>
      <div class="ctl-seg ctl-material" id="ctl-material" role="group" aria-label="界面材质">
        <button type="button" data-material="solid">
          <span class="ctl-seg-ico">${icon("layers", { size: 14 })}</span>标准
        </button>
        <button type="button" data-material="liquid">
          <span class="ctl-seg-ico">${icon("droplet", { size: 14 })}</span>液态玻璃
        </button>
        <button type="button" data-material="glow">
          <span class="ctl-seg-ico">${icon("sparkles", { size: 14 })}</span>柔光玻璃
        </button>
      </div>
    </section>

    <section class="ctl-card">
      <div class="ctl-card-head">
        <span class="ctl-card-title">多屏</span>
        <span class="ctl-card-sub" id="ctl-multi-sub">--</span>
      </div>
      <div class="ctl-multi-row">
        <div class="ctl-multi-item">
          <span class="ctl-multi-label">副屏顶栏</span>
          <button class="ctl-switch" id="ctl-topbar-all" type="button" role="switch"
                  aria-checked="true" aria-label="在副屏显示顶栏"></button>
        </div>
        <div class="ctl-multi-item">
          <span class="ctl-multi-label">副屏 Dock</span>
          <button class="ctl-switch" id="ctl-dock-all" type="button" role="switch"
                  aria-checked="false" aria-label="在副屏显示 Dock"></button>
        </div>
      </div>
    </section>

    <section class="ctl-card">
      <div class="ctl-card-head">
        <span class="ctl-card-title">音量与亮度</span>
      </div>
      <div class="ctl-slider-row" id="ctl-volume-row" title="滚轮调节音量">
        <span class="ctl-sl-ico" id="ctl-vol-icon"></span>
        <input class="ctl-slider" id="ctl-volume" type="range" min="0" max="100" step="1" value="50"
               aria-label="音量" />
        <span class="ctl-pct" id="ctl-volume-pct">--</span>
        <button class="ctl-mute" id="ctl-mute" type="button" title="静音"></button>
      </div>
      <div class="ctl-slider-row" id="ctl-brightness-row" title="滚轮调节亮度">
        <span class="ctl-sl-ico" id="ctl-sun-icon"></span>
        <input class="ctl-slider" id="ctl-brightness" type="range" min="0" max="100" step="1" value="50"
               aria-label="亮度" />
        <span class="ctl-pct" id="ctl-brightness-pct">--</span>
      </div>
    </section>

    <section class="ctl-card">
      <div class="ctl-card-head">
        <span class="ctl-card-title">电源</span>
        <span class="ctl-card-sub" id="ctl-power-sub">--</span>
      </div>
      <div class="ctl-seg ctl-power" id="ctl-power" role="group" aria-label="电源模式">
        <button type="button" data-mode="efficiency">节能</button>
        <button type="button" data-mode="balanced">平衡</button>
        <button type="button" data-mode="performance">高性能</button>
      </div>
    </section>
  </div>

  <div class="ctl-sub hidden" id="ctl-sub"></div>

  <div class="ctl-footer">
    <span class="ctl-bat"><span class="ctl-bat-fill" id="ctl-bat-fill"></span></span>
    <span class="ctl-bat-text" id="ctl-bat-text">电量未知</span>
    <span class="ctl-actions">
      <button class="ctl-action" id="ctl-settings" type="button">
        <span class="ctl-action-ico">${icon("settings", { size: 13 })}</span>设置
      </button>
      <button class="ctl-action" id="ctl-lock" type="button">
        <span class="ctl-action-ico">${icon("lock", { size: 13 })}</span>锁屏
      </button>
      <button class="ctl-action" id="ctl-power-btn" type="button">
        <span class="ctl-action-ico">${icon("power", { size: 13 })}</span>电源
      </button>
    </span>
  </div>

  <div class="ctl-power-menu hidden" id="ctl-power-menu">
    <button type="button" data-action="sleep">睡眠</button>
    <button type="button" data-action="shutdown">关机</button>
    <button type="button" data-action="restart">重启</button>
    <button type="button" data-action="signout">注销</button>
    <button type="button" data-action="quit-app">退出 easy-dash</button>
  </div>
`;

function clampInt(v, min, max) {
  const n = Math.round(Number(v));
  if (!Number.isFinite(n)) return min;
  return Math.max(min, Math.min(max, n));
}

function errText(e) {
  if (typeof e === "string") return e;
  if (e && typeof e.message === "string") return e.message;
  try {
    return String(e);
  } catch (_) {
    return "未知错误";
  }
}

const POWER_MODE_LABEL = {
  efficiency: "节能",
  balanced: "平衡",
  performance: "高性能",
};

/**
 * 创建控制中心面板控制器。
 * @param {{onState?: (state: object) => void}} opts onState：每次拿到状态都回调（顶栏聚合图标同步用）
 */
export function createControlCenterPanel({ onState } = {}) {
  const el = document.getElementById("panel-control");

  let state = null;
  let enabled = false;
  let panelOpen = false;
  let loadedOnce = false;
  let pollTimer = null;
  let btTimer = null;
  let errorTimer = null;
  let correctTimer = null;
  let volumeTimer = null;
  let brightnessTimer = null;
  let lastVolumeTouch = 0;
  let lastBrightnessTouch = 0;
  let volumeHeld = false; // 滑条按住期间不让轮询结果覆盖
  let brightnessHeld = false;
  // 滚轮相对调节：80ms 合批，避免连续滚动产生几十次 IPC
  let volumeNudge = null; // { delta, value, waiters[] }
  let volumeNudgeTimer = null;
  let brightnessNudge = null;
  let brightnessNudgeTimer = null;
  let confirmState = null; // 关机/重启的 3 秒二次确认
  let subKind = null; // null | "wifi" | "bluetooth"
  let btCount = 0; // 已连接蓝牙设备数（连接入口摘要用）
  let connections = null;
  const emitState = typeof onState === "function" ? onState : () => {};

  // DOM 引用（innerHTML 之后才可用）
  let errEl = null;
  let mainEl = null;
  let subEl = null;
  let wifiTile = null;
  let wifiIcon = null;
  let wifiSub = null;
  let btTile = null;
  let btIcon = null;
  let btSub = null;
  let airplaneTile = null;
  let airplaneIcon = null;
  let airplaneSub = null;
  let themeSeg = null;
  let themeSub = null;
  let materialSeg = null;
  let wifiToggle = null;
  let wifiOpen = null;
  let btToggle = null;
  let btOpen = null;
  let volIcon = null;
  let sunIcon = null;
  let volumeRow = null;
  let volumeSlider = null;
  let volumePct = null;
  let muteBtn = null;
  let brightnessRow = null;
  let brightnessSlider = null;
  let brightnessPct = null;
  let powerSeg = null;
  let powerSub = null;
  let batFill = null;
  let batText = null;
  let powerBtn = null;
  let powerMenu = null;
  let multiTopbarAll = null;
  let multiDockAll = null;
  let multiSub = null;
  let multiConfig = { topbarAll: true, dockAll: false };
  let multiMonitorCount = 1;

  if (el) {
    el.innerHTML = PANEL_HTML;
    collectRefs();
    setupConnections();
    bindDom();
    setStaticIcons();
    initTheme();
    bindConfigEvents();
    // 分段控件滑动指示器（明暗 / 界面材质 / 电源三组）
    attachSegIndicators(el);
  }

  // ---------- 初始化 ----------

  function collectRefs() {
    errEl = document.getElementById("ctl-error");
    mainEl = document.getElementById("ctl-main");
    subEl = document.getElementById("ctl-sub");
    wifiTile = document.getElementById("ctl-wifi");
    wifiIcon = document.getElementById("ctl-wifi-icon");
    wifiSub = document.getElementById("ctl-wifi-sub");
    btTile = document.getElementById("ctl-bt");
    btIcon = document.getElementById("ctl-bt-icon");
    btSub = document.getElementById("ctl-bt-sub");
    airplaneTile = document.getElementById("ctl-airplane");
    airplaneIcon = document.getElementById("ctl-airplane-icon");
    airplaneSub = document.getElementById("ctl-airplane-sub");
    themeSeg = document.getElementById("ctl-theme");
    themeSub = document.getElementById("ctl-theme-sub");
    materialSeg = document.getElementById("ctl-material");
    wifiToggle = document.getElementById("ctl-wifi-toggle");
    wifiOpen = document.getElementById("ctl-wifi-open");
    btToggle = document.getElementById("ctl-bt-toggle");
    btOpen = document.getElementById("ctl-bt-open");
    volIcon = document.getElementById("ctl-vol-icon");
    sunIcon = document.getElementById("ctl-sun-icon");
    volumeRow = document.getElementById("ctl-volume-row");
    volumeSlider = document.getElementById("ctl-volume");
    volumePct = document.getElementById("ctl-volume-pct");
    muteBtn = document.getElementById("ctl-mute");
    brightnessRow = document.getElementById("ctl-brightness-row");
    brightnessSlider = document.getElementById("ctl-brightness");
    brightnessPct = document.getElementById("ctl-brightness-pct");
    powerSeg = document.getElementById("ctl-power");
    powerSub = document.getElementById("ctl-power-sub");
    batFill = document.getElementById("ctl-bat-fill");
    batText = document.getElementById("ctl-bat-text");
    powerBtn = document.getElementById("ctl-power-btn");
    powerMenu = document.getElementById("ctl-power-menu");
    multiTopbarAll = document.getElementById("ctl-topbar-all");
    multiDockAll = document.getElementById("ctl-dock-all");
    multiSub = document.getElementById("ctl-multi-sub");
  }

  /** 连接子视图：渲染、错误、刷新职责都交给 connections.js */
  function setupConnections() {
    if (!subEl) return;
    connections = createConnections({
      host: subEl,
      onError: (msg) => showError(msg),
      onChanged: () => {
        // 连接状态变化：刷新聚合状态与设备计数
        refresh({ silent: true });
        loadBtCount();
      },
      onBack: () => backToMain(),
    });
  }

  function setStaticIcons() {
    if (wifiIcon) wifiIcon.innerHTML = icon("wifi", { size: 18 });
    if (btIcon) btIcon.innerHTML = icon("bluetooth", { size: 18 });
    if (airplaneIcon) airplaneIcon.innerHTML = icon("airplane", { size: 18 });
    if (volIcon) volIcon.innerHTML = icon("volume", { size: 16 });
    if (sunIcon) sunIcon.innerHTML = icon("sun", { size: 16 });
    if (muteBtn) muteBtn.innerHTML = icon("volume", { size: 16 });
  }

  // ---------- 外观（theme.js 为唯一数据源：明暗主题 + 界面材质） ----------

  /** 材质值 → 展示名（非法值按标准处理，与 theme.js 的归一规则一致） */
  function materialLabel(value) {
    return MATERIAL_LABELS[value] || MATERIAL_LABELS.solid;
  }

  /** 外观状态 → 两组分段选中态 + 副标题（显示当前明暗 + 材质） */
  function renderTheme(theme) {
    if (!theme) return;
    const mode = THEME_MODES.indexOf(theme.mode) >= 0 ? theme.mode : "dark";
    const resolvedLight = theme.resolved === "light";
    const material = MATERIAL_STYLES.indexOf(theme.material) >= 0 ? theme.material : "solid";
    if (themeSeg) {
      const buttons = themeSeg.querySelectorAll("button[data-mode]");
      for (const b of buttons) {
        const on = b.dataset.mode === mode;
        b.classList.toggle("active", on);
        b.setAttribute("aria-pressed", on ? "true" : "false");
      }
    }
    if (materialSeg) {
      const buttons = materialSeg.querySelectorAll("button[data-material]");
      for (const b of buttons) {
        const on = b.dataset.material === material;
        b.classList.toggle("active", on);
        b.setAttribute("aria-pressed", on ? "true" : "false");
      }
    }
    if (themeSub) {
      themeSub.textContent =
        "当前" + (resolvedLight ? "浅色" : "深色") + " · " + materialLabel(material);
    }
  }

  function initTheme() {
    // subscribeTheme 注册时不回调，先用 getThemeState 取初值
    renderTheme(getThemeState());
    subscribeTheme(renderTheme);
  }

  // ---------- 多屏（每屏顶栏 / 每屏 Dock） ----------

  function bindConfigEvents() {
    // 其它入口（管理面板/另一块屏的控制中心）改了配置时同步开关
    try {
      const p = listen("config-changed", () => {
        if (panelOpen) refreshMulti();
      });
      if (p && typeof p.catch === "function") p.catch(() => {});
    } catch (e) {
      /* Tauri 尚未注入时忽略 */
    }
  }

  function setSwitch(btn, on, enabled) {
    if (!btn) return;
    btn.classList.toggle("on", !!on);
    btn.setAttribute("aria-checked", on ? "true" : "false");
    btn.disabled = !enabled;
  }

  function renderMulti() {
    const enabled = multiMonitorCount >= 2;
    setSwitch(multiTopbarAll, multiConfig.topbarAll, enabled);
    setSwitch(multiDockAll, multiConfig.dockAll, enabled);
    if (multiSub) {
      multiSub.textContent = enabled
        ? `${multiMonitorCount} 块显示器 · 主屏常开`
        : "仅 1 块显示器";
    }
  }

  async function refreshMulti() {
    try {
      const [cfg, layout] = await Promise.all([
        invoke("get_config"),
        invoke("get_monitor_layout"),
      ]);
      multiConfig = {
        topbarAll: cfg && cfg.topbarAllMonitors !== false,
        dockAll: !!(cfg && cfg.dockAllMonitors),
      };
      multiMonitorCount = (layout && layout.count) || 1;
      renderMulti();
    } catch (e) {
      // 单测/预览环境没有这些命令：保持默认值，不打断面板
      renderMulti();
    }
  }

  async function onMultiToggle(which) {
    if (multiMonitorCount < 2) return;
    const before = { ...multiConfig };
    if (which === "topbar") multiConfig.topbarAll = !multiConfig.topbarAll;
    else multiConfig.dockAll = !multiConfig.dockAll;
    renderMulti();
    try {
      const patch =
        which === "topbar"
          ? { topbarAllMonitors: multiConfig.topbarAll }
          : { dockAllMonitors: multiConfig.dockAll };
      const warnings = await invoke("update_preferences", { patch });
      if (Array.isArray(warnings) && warnings.length) {
        showNotice(warnings.join("；"));
      }
    } catch (e) {
      multiConfig = before;
      renderMulti();
      showError(errText(e));
    }
  }

  async function onThemeModeClick(mode) {
    if (!mode) return;
    if (getThemeState().mode === mode) return;
    try {
      const warnings = await setThemeMode(mode);
      if (Array.isArray(warnings) && warnings.length) {
        // setThemeMode 的 warnings 不是失败，用中性内联提示（不用「操作失败」前缀）
        showNotice(warnings.join("；"));
      }
    } catch (e) {
      showError(errText(e));
    }
  }

  async function onMaterialClick(material) {
    if (!material) return;
    if (getThemeState().material === material) return;
    try {
      const warnings = await setMaterialStyle(material);
      if (Array.isArray(warnings) && warnings.length) {
        // warnings 不是失败（例如 Tauri 未就绪）：用中性内联提示
        showNotice(warnings.join("；"));
      }
    } catch (e) {
      showError(errText(e));
    }
  }

  function bindDom() {
    // Wi-Fi / 蓝牙：图标泡切换开关，文字区进入连接子视图；飞行模式整块切换
    if (wifiToggle) wifiToggle.addEventListener("click", onWifiClick);
    if (btToggle) btToggle.addEventListener("click", onBluetoothClick);
    if (wifiOpen) wifiOpen.addEventListener("click", onWifiOpen);
    if (btOpen) btOpen.addEventListener("click", onBtOpen);
    if (airplaneTile) airplaneTile.addEventListener("click", onAirplaneClick);

    if (multiTopbarAll) {
      multiTopbarAll.addEventListener("click", () => onMultiToggle("topbar"));
    }
    if (multiDockAll) {
      multiDockAll.addEventListener("click", () => onMultiToggle("dock"));
    }

    if (themeSeg) {
      themeSeg.addEventListener("click", (e) => {
        const btn = e.target.closest ? e.target.closest("[data-mode]") : null;
        if (btn) onThemeModeClick(btn.dataset.mode);
      });
    }
    if (materialSeg) {
      materialSeg.addEventListener("click", (e) => {
        const btn = e.target.closest ? e.target.closest("[data-material]") : null;
        if (btn) onMaterialClick(btn.dataset.material);
      });
    }

    if (volumeSlider) {
      volumeSlider.addEventListener("input", onVolumeInput);
      volumeSlider.addEventListener("change", onVolumeCommit);
      volumeSlider.addEventListener("pointerdown", () => {
        volumeHeld = true;
        lastVolumeTouch = Date.now();
      });
    }
    if (brightnessSlider) {
      brightnessSlider.addEventListener("input", onBrightnessInput);
      brightnessSlider.addEventListener("change", onBrightnessCommit);
      brightnessSlider.addEventListener("pointerdown", () => {
        brightnessHeld = true;
        lastBrightnessTouch = Date.now();
      });
    }
    // 滚轮调节：整行都可滚（滑条轨道只有 6px 高，命中区域太小）；向上 = 增大
    const volumeStepper = createWheelStepper();
    if (volumeRow) {
      volumeRow.addEventListener(
        "wheel",
        (e) => {
          if (!enabled || !state || state.volume === null || state.volume === undefined) return;
          const steps = volumeStepper(e);
          if (!steps) return;
          e.preventDefault();
          nudgeVolume(steps);
        },
        { passive: false }
      );
    }
    const brightnessStepper = createWheelStepper();
    if (brightnessRow) {
      brightnessRow.addEventListener(
        "wheel",
        (e) => {
          if (!enabled || !state || state.brightness === null || state.brightness === undefined) return;
          const steps = brightnessStepper(e);
          if (!steps) return;
          e.preventDefault();
          nudgeBrightness(steps);
        },
        { passive: false }
      );
    }
    // 松开鼠标即结束按住状态（滑条外松手也能收到）
    window.addEventListener("pointerup", () => {
      if (volumeHeld) {
        volumeHeld = false;
        lastVolumeTouch = Date.now();
      }
      if (brightnessHeld) {
        brightnessHeld = false;
        lastBrightnessTouch = Date.now();
      }
    });
    if (muteBtn) muteBtn.addEventListener("click", onMuteClick);

    if (powerSeg) {
      powerSeg.addEventListener("click", (e) => {
        const btn = e.target.closest ? e.target.closest("[data-mode]") : null;
        if (btn) onPowerModeClick(btn.dataset.mode);
      });
    }

    const settingsBtn = document.getElementById("ctl-settings");
    const lockBtn = document.getElementById("ctl-lock");
    if (settingsBtn) {
      settingsBtn.addEventListener("click", () =>
        runAction(() => invoke("power_action", { action: "settings" }))
      );
    }
    if (lockBtn) {
      lockBtn.addEventListener("click", () => runAction(() => invoke("power_action", { action: "lock" })));
    }
    if (powerBtn) powerBtn.addEventListener("click", togglePowerMenu);

    if (powerMenu) {
      powerMenu.addEventListener("click", (e) => {
        const btn = e.target.closest ? e.target.closest("[data-action]") : null;
        if (btn) onPowerMenuClick(btn.dataset.action);
      });
    }

    // 电源菜单外点关闭
    document.addEventListener("mousedown", (e) => {
      if (!powerMenu || powerMenu.classList.contains("hidden")) return;
      if (powerMenu.contains(e.target)) return;
      if (powerBtn && powerBtn.contains(e.target)) return;
      hidePowerMenu();
    });
  }

  // ---------- 子视图切换 ----------

  /** kind = "wifi" | "bluetooth" | null（null 回默认视图） */
  function setSubView(kind) {
    subKind = kind || null;
    if (subKind) {
      if (mainEl) mainEl.classList.add("hidden");
      if (subEl) subEl.classList.remove("hidden");
      if (connections) connections.open(subKind);
    } else {
      if (connections) connections.close();
      if (subEl) subEl.classList.add("hidden");
      if (mainEl) mainEl.classList.remove("hidden");
    }
  }

  function backToMain() {
    setSubView(null);
    refresh({ silent: true });
    loadBtCount();
  }

  // ---------- 状态刷新 ----------

  async function refresh(opts) {
    if (!enabled) return;
    const silent = !!(opts && opts.silent);
    const first = !loadedOnce;
    try {
      const s = first
        ? await invokeRetry("get_quick_state", undefined, { attempts: 3, perTryMs: 3000 })
        : await invoke("get_quick_state");
      loadedOnce = true;
      if (!enabled) return; // 关闭入口后到达的迟到结果直接丢弃
      if (s && typeof s === "object") {
        state = s;
        render();
      }
    } catch (e) {
      if (!silent) showError(errText(e));
    }
  }

  /** 已连接蓝牙设备数（连接入口摘要「已连接 N 台」） */
  async function loadBtCount() {
    if (!enabled) return;
    if (state && state.bluetooth !== true) {
      btCount = 0;
      renderBtSub();
      return;
    }
    try {
      const list = await invoke("get_bt_devices");
      if (!enabled) return;
      const arr = Array.isArray(list) ? list : [];
      btCount = arr.filter((d) => d && d.connected).length;
    } catch (e) {
      btCount = 0;
    }
    renderBtSub();
  }

  function syncTimer() {
    if (pollTimer) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
    if (btTimer) {
      clearInterval(btTimer);
      btTimer = null;
    }
    if (!enabled) return;
    // 面板打开：2s 纠偏；关闭：20s 仅同步顶栏聚合图标
    const period = panelOpen ? 2000 : 20000;
    pollTimer = setInterval(() => refresh({ silent: !panelOpen }), period);
    // 连接入口的蓝牙计数（设备可能自行连接/断开）
    if (panelOpen) btTimer = setInterval(() => loadBtCount(), 20000);
  }

  // ---------- 渲染 ----------

  function updateSliderFill(input, pct) {
    if (!input) return;
    const p = Math.max(0, Math.min(100, Number(pct) || 0));
    input.style.setProperty("--pct", p + "%");
  }

  /** 统一刷新音量相关 UI（滑条、百分比、静音按钮、音量图标）。 */
  function applyVolumeUi(value, muted) {
    const v = clampInt(value, 0, 100);
    if (volumeSlider) {
      volumeSlider.value = String(v);
      updateSliderFill(volumeSlider, v);
    }
    if (volumePct) volumePct.textContent = v + "%";
    if (muteBtn) {
      muteBtn.classList.toggle("on", !!muted);
      muteBtn.innerHTML = icon(muted ? "volumeMuted" : "volume", { size: 16 });
    }
    if (volIcon) volIcon.innerHTML = icon(muted ? "volumeMuted" : "volume", { size: 16 });
  }

  /** 统一刷新亮度相关 UI（滑条、百分比）。 */
  function applyBrightnessUi(value) {
    const v = clampInt(value, 0, 100);
    if (brightnessSlider) {
      brightnessSlider.value = String(v);
      updateSliderFill(brightnessSlider, v);
    }
    if (brightnessPct) brightnessPct.textContent = v + "%";
  }

  /**
   * 更新磁贴开关态与可访问标签。
   * toggleBtn 为 null 时（飞行模式）整块磁贴就是按钮。
   */
  function setTile(tile, on, disabled, toggleBtn, label) {
    if (!tile) return;
    tile.classList.toggle("on", !!on);
    tile.classList.toggle("disabled", !!disabled);
    const btn = toggleBtn || tile;
    if (!btn) return;
    btn.setAttribute("aria-pressed", on ? "true" : "false");
    if (label) {
      const text = (on ? "关闭" : "开启") + label;
      btn.setAttribute("aria-label", text);
      btn.title = text;
    }
  }

  function airplaneKnown() {
    if (!state) return false;
    if (state.wifi) return true;
    return state.bluetooth !== null && state.bluetooth !== undefined;
  }

  function airplaneActive() {
    if (!state || !airplaneKnown()) return false;
    const wifiOff = !state.wifi || state.wifi.enabled === false;
    const btOff = state.bluetooth === false || state.bluetooth === null || state.bluetooth === undefined;
    return wifiOff && btOff;
  }

  /** 蓝牙磁贴状态行：不可用 / 已关闭 / 已连接 N 台 / 已开启 */
  function renderBtSub() {
    if (!btSub) return;
    const s = state;
    if (!s) btSub.textContent = "--";
    else if (s.bluetooth === null || s.bluetooth === undefined) btSub.textContent = "不可用";
    else if (!s.bluetooth) btSub.textContent = "已关闭";
    else btSub.textContent = btCount > 0 ? "已连接 " + btCount + " 台" : "已开启";
  }

  function render() {
    const s = state;
    if (!s) return;

    // Wi-Fi 磁贴（图标泡=开关，文字区=进入网络列表）
    const wifiOn = !!(s.wifi && s.wifi.enabled);
    const wifiMissing = !s.wifi;
    setTile(wifiTile, wifiOn, wifiMissing, wifiToggle, " Wi-Fi");
    if (wifiIcon) wifiIcon.innerHTML = icon(wifiOn ? "wifi" : "wifiOff", { size: 18 });
    if (wifiSub) {
      wifiSub.textContent = wifiMissing
        ? "不可用"
        : s.wifi.connected
          ? s.wifi.ssid || "已连接"
          : s.wifi.enabled
            ? "已开启"
            : "已关闭";
    }

    // 蓝牙磁贴（null = 无硬件 / 服务不可用）
    const btMissing = s.bluetooth === null || s.bluetooth === undefined;
    setTile(btTile, s.bluetooth === true, btMissing, btToggle, "蓝牙");
    renderBtSub();

    // 飞行模式（由 Wi-Fi + 蓝牙状态推导，整块单按钮）
    const airplaneOn = airplaneActive();
    setTile(airplaneTile, airplaneOn, !airplaneKnown(), null, "飞行模式");
    if (airplaneSub) airplaneSub.textContent = airplaneKnown() ? (airplaneOn ? "已开启" : "已关闭") : "不可用";

    // 音量滑条
    const muted = s.muted === true;
    if (muteBtn) {
      muteBtn.classList.toggle("on", muted);
      muteBtn.innerHTML = icon(muted ? "volumeMuted" : "volume", { size: 16 });
    }
    if (volIcon) volIcon.innerHTML = icon(muted ? "volumeMuted" : "volume", { size: 16 });
    if (volumeSlider) {
      const vol = s.volume;
      if (vol === null || vol === undefined) {
        volumeSlider.disabled = true;
        if (volumePct) volumePct.textContent = "--";
      } else {
        volumeSlider.disabled = false;
        // 拖动（或刚松开 1.2s 内）不要被轮询结果打断
        if (!volumeHeld && Date.now() - lastVolumeTouch > 1200) {
          volumeSlider.value = String(clampInt(vol, 0, 100));
          updateSliderFill(volumeSlider, vol);
          if (volumePct) volumePct.textContent = clampInt(vol, 0, 100) + "%";
        }
      }
    }

    // 亮度整行：brightness 为 null（显示器不支持 DDC/CI）时隐藏
    if (s.brightness === null || s.brightness === undefined) {
      if (brightnessRow) brightnessRow.classList.add("hidden");
    } else {
      if (brightnessRow) brightnessRow.classList.remove("hidden");
      if (!brightnessHeld && Date.now() - lastBrightnessTouch > 1200) {
        if (brightnessSlider) {
          brightnessSlider.value = String(clampInt(s.brightness, 0, 100));
          updateSliderFill(brightnessSlider, s.brightness);
        }
        if (brightnessPct) brightnessPct.textContent = clampInt(s.brightness, 0, 100) + "%";
      }
    }

    // 电源模式三档
    if (powerSeg) {
      const buttons = powerSeg.querySelectorAll("button[data-mode]");
      for (const b of buttons) {
        const on = b.dataset.mode === s.powerMode;
        b.classList.toggle("active", on);
        b.setAttribute("aria-pressed", on ? "true" : "false");
      }
    }
    if (powerSub) powerSub.textContent = POWER_MODE_LABEL[s.powerMode] || "—";

    // 电池行
    if (batText) {
      if (s.battery !== null && s.battery !== undefined) {
        let text = s.battery + "%";
        if (s.charging) text += " · 充电中";
        else if (s.acOnline) text += " · 已接通电源";
        else text += " · 使用电池";
        batText.textContent = text;
        if (batFill) {
          batFill.className =
            "ctl-bat-fill" + (s.charging ? " charging" : s.battery <= 15 ? " low" : "");
          batFill.style.width = Math.max(6, Math.min(100, s.battery)) + "%";
        }
      } else {
        batText.textContent = s.acOnline ? "已接通电源" : "电量未知";
        if (batFill) {
          batFill.className = "ctl-bat-fill";
          batFill.style.width = "0%";
        }
      }
    }

    // 顶栏聚合图标同步
    emitState(s);
  }

  // ---------- 操作 ----------

  async function runAction(fn) {
    try {
      await fn();
      scheduleCorrect(400);
    } catch (e) {
      showError(errText(e));
    }
  }

  async function runAll(jobs) {
    for (const job of jobs) {
      try {
        await job();
      } catch (e) {
        showError(errText(e));
      }
    }
    scheduleCorrect(400);
  }

  function scheduleCorrect(ms) {
    if (correctTimer) clearTimeout(correctTimer);
    correctTimer = setTimeout(() => {
      correctTimer = null;
      refresh({ silent: true });
    }, ms || 400);
  }

  /** 磁贴文字区：进入 Wi-Fi 网络列表（硬件不可用时退回系统设置） */
  function onWifiOpen() {
    if (state && !state.wifi) {
      runAction(() => invoke("power_action", { action: "settings" }));
      return;
    }
    setSubView("wifi");
  }

  /** 磁贴文字区：进入蓝牙设备列表（硬件/服务不可用时退回系统设置） */
  function onBtOpen() {
    if (state && (state.bluetooth === null || state.bluetooth === undefined)) {
      runAction(() => invoke("power_action", { action: "settings" }));
      return;
    }
    setSubView("bluetooth");
  }

  function onWifiClick() {
    if (!state) return;
    if (!state.wifi) {
      // Wi-Fi 不可用：交给系统设置处理
      runAction(() => invoke("power_action", { action: "settings" }));
      return;
    }
    const next = !state.wifi.enabled;
    state.wifi = {
      enabled: next,
      connected: next ? state.wifi.connected : false,
      ssid: next ? state.wifi.ssid : "",
    };
    render();
    runAction(() => invoke("set_wifi", { enabled: next }));
    // Wi-Fi 开关会影响蓝牙计数摘要的可用性
    setTimeout(() => loadBtCount(), 600);
  }

  function onBluetoothClick() {
    if (!state) return;
    const bt = state.bluetooth;
    if (bt === null || bt === undefined) {
      // 蓝牙不可用：打开系统设置
      runAction(() => invoke("power_action", { action: "settings" }));
      return;
    }
    const next = !bt;
    state.bluetooth = next;
    render();
    runAction(() => invoke("set_bluetooth", { enabled: next }));
    setTimeout(() => loadBtCount(), 600);
  }

  function onAirplaneClick() {
    if (!state) return;
    const hasWifi = !!state.wifi;
    const hasBt = state.bluetooth === true || state.bluetooth === false;
    if (!hasWifi && !hasBt) {
      showError("没有可用的无线设备");
      return;
    }
    const next = !airplaneActive();
    if (hasWifi) {
      state.wifi = {
        enabled: !next,
        connected: false,
        ssid: !next ? state.wifi.ssid || "" : "",
      };
    }
    if (hasBt) state.bluetooth = !next;
    render();

    const jobs = [];
    if (hasWifi) jobs.push(() => invoke("set_wifi", { enabled: !next }));
    if (hasBt) jobs.push(() => invoke("set_bluetooth", { enabled: !next }));
    runAll(jobs);
    setTimeout(() => loadBtCount(), 800);
  }

  function onMuteClick() {
    if (!state) return;
    const next = !(state.muted === true);
    state.muted = next;
    if (muteBtn) {
      muteBtn.classList.toggle("on", next);
      muteBtn.innerHTML = icon(next ? "volumeMuted" : "volume", { size: 16 });
    }
    if (volIcon) volIcon.innerHTML = icon(next ? "volumeMuted" : "volume", { size: 16 });
    runAction(() => invoke("set_muted", { muted: next }));
  }

  function onVolumeInput() {
    if (!volumeSlider || volumeSlider.disabled) return;
    const v = clampInt(volumeSlider.value, 0, 100);
    lastVolumeTouch = Date.now();
    // 拖拽优先：丢弃尚未发出的滚轮批次，避免两路写入互相覆盖
    cancelVolumeNudge();
    // 乐观更新
    if (state) {
      state.volume = v;
      state.muted = false;
    }
    applyVolumeUi(v, false);

    // 70ms 防抖：连续拖动只发最后一次
    if (volumeTimer) clearTimeout(volumeTimer);
    volumeTimer = setTimeout(() => {
      volumeTimer = null;
      lastVolumeTouch = Date.now();
      runAction(() => invoke("set_volume", { value: v }));
    }, 70);
  }

  function onVolumeCommit() {
    if (!volumeSlider || volumeSlider.disabled) return;
    if (volumeTimer) {
      clearTimeout(volumeTimer);
      volumeTimer = null;
    }
    const v = clampInt(volumeSlider.value, 0, 100);
    lastVolumeTouch = Date.now();
    runAction(() => invoke("set_volume", { value: v }));
  }

  function onBrightnessInput() {
    if (!brightnessSlider) return;
    const v = clampInt(brightnessSlider.value, 0, 100);
    lastBrightnessTouch = Date.now();
    cancelBrightnessNudge();
    if (state) state.brightness = v;
    applyBrightnessUi(v);

    if (brightnessTimer) clearTimeout(brightnessTimer);
    brightnessTimer = setTimeout(() => {
      brightnessTimer = null;
      lastBrightnessTouch = Date.now();
      runAction(() => invoke("set_brightness", { value: v }));
    }, 70);
  }

  function onBrightnessCommit() {
    if (!brightnessSlider) return;
    if (brightnessTimer) {
      clearTimeout(brightnessTimer);
      brightnessTimer = null;
    }
    const v = clampInt(brightnessSlider.value, 0, 100);
    lastBrightnessTouch = Date.now();
    runAction(() => invoke("set_brightness", { value: v }));
  }

  // ---------- 滚轮相对调节（相对 IPC，避免轮询状态过期导致数值跳变） ----------

  /** 丢弃尚未发出的音量滚轮批次（滑条拖拽优先）。 */
  function cancelVolumeNudge() {
    if (volumeNudgeTimer) {
      clearTimeout(volumeNudgeTimer);
      volumeNudgeTimer = null;
    }
    const batch = volumeNudge;
    volumeNudge = null;
    if (batch) {
      for (const waiter of batch.waiters) waiter(null);
    }
  }

  function cancelBrightnessNudge() {
    if (brightnessNudgeTimer) {
      clearTimeout(brightnessNudgeTimer);
      brightnessNudgeTimer = null;
    }
    const batch = brightnessNudge;
    brightnessNudge = null;
    if (batch) {
      for (const waiter of batch.waiters) waiter(null);
    }
  }

  /** 滚轮开始前取消尚未发出的拖拽 set_volume，避免两路写入互相覆盖。 */
  function cancelVolumeDrag() {
    if (volumeTimer) {
      clearTimeout(volumeTimer);
      volumeTimer = null;
    }
  }

  function cancelBrightnessDrag() {
    if (brightnessTimer) {
      clearTimeout(brightnessTimer);
      brightnessTimer = null;
    }
  }

  /**
   * 滚轮调节音量：notches 正数=增大，每档 WHEEL_STEP%。
   * @returns {Promise<{value:number, muted:boolean}|null>} 后端权威值
   */
  function nudgeVolume(notches) {
    if (!enabled || !Number(notches)) return Promise.resolve(null);
    if (!state || state.volume === null || state.volume === undefined) {
      refresh({ silent: true });
      return Promise.resolve(null);
    }
    cancelVolumeDrag();
    if (!volumeNudge) {
      volumeNudge = { delta: 0, value: clampInt(state.volume, 0, 100), waiters: [] };
    }
    const batch = volumeNudge;
    const step = Math.round(Number(notches)) * WHEEL_STEP;
    batch.delta += step;
    batch.value = clampInt(batch.value + step, 0, 100);
    state.volume = batch.value;
    state.muted = false;
    lastVolumeTouch = Date.now();
    applyVolumeUi(batch.value, false);
    emitState(state);

    const promise = new Promise((resolve) => batch.waiters.push(resolve));
    if (volumeNudgeTimer) clearTimeout(volumeNudgeTimer);
    volumeNudgeTimer = setTimeout(flushVolumeNudge, 80);
    return promise;
  }

  function flushVolumeNudge() {
    volumeNudgeTimer = null;
    const batch = volumeNudge;
    volumeNudge = null;
    if (!batch) return;
    if (!batch.delta) {
      for (const waiter of batch.waiters) waiter(null);
      return;
    }
    invoke("nudge_volume", { delta: batch.delta })
      .then((r) => {
        const value = r && typeof r.value === "number" ? clampInt(r.value, 0, 100) : null;
        // 只有没有更新批次时才回写，避免旧响应把新预览顶回去
        if (value !== null && !volumeNudge && state) {
          state.volume = value;
          state.muted = !!r.muted;
          lastVolumeTouch = Date.now();
          applyVolumeUi(value, !!r.muted);
          emitState(state);
        }
        for (const waiter of batch.waiters) {
          waiter(value === null ? null : { value, muted: !!(r && r.muted) });
        }
      })
      .catch((e) => {
        showError(errText(e));
        for (const waiter of batch.waiters) waiter(null);
        refresh({ silent: true });
      });
  }

  /**
   * 滚轮调节亮度：notches 正数=增大，每档 WHEEL_STEP%。
   * @returns {Promise<number|null>} 后端权威亮度
   */
  function nudgeBrightness(notches) {
    if (!enabled || !Number(notches)) return Promise.resolve(null);
    if (!state || state.brightness === null || state.brightness === undefined) {
      refresh({ silent: true });
      return Promise.resolve(null);
    }
    cancelBrightnessDrag();
    if (!brightnessNudge) {
      brightnessNudge = { delta: 0, value: clampInt(state.brightness, 0, 100), waiters: [] };
    }
    const batch = brightnessNudge;
    const step = Math.round(Number(notches)) * WHEEL_STEP;
    batch.delta += step;
    batch.value = clampInt(batch.value + step, 0, 100);
    state.brightness = batch.value;
    lastBrightnessTouch = Date.now();
    applyBrightnessUi(batch.value);

    const promise = new Promise((resolve) => batch.waiters.push(resolve));
    if (brightnessNudgeTimer) clearTimeout(brightnessNudgeTimer);
    brightnessNudgeTimer = setTimeout(flushBrightnessNudge, 80);
    return promise;
  }

  function flushBrightnessNudge() {
    brightnessNudgeTimer = null;
    const batch = brightnessNudge;
    brightnessNudge = null;
    if (!batch) return;
    if (!batch.delta) {
      for (const waiter of batch.waiters) waiter(null);
      return;
    }
    invoke("nudge_brightness", { delta: batch.delta })
      .then((value) => {
        const current = typeof value === "number" ? clampInt(value, 0, 100) : null;
        if (current !== null && !brightnessNudge && state) {
          state.brightness = current;
          lastBrightnessTouch = Date.now();
          applyBrightnessUi(current);
        }
        for (const waiter of batch.waiters) waiter(current);
      })
      .catch((e) => {
        showError(errText(e));
        for (const waiter of batch.waiters) waiter(null);
        refresh({ silent: true });
      });
  }

  function onPowerModeClick(mode) {
    if (!mode) return;
    if (state) state.powerMode = mode;
    render();
    runAction(() => invoke("set_power_mode", { mode }));
  }

  // ---------- 电源菜单（关机/重启需 3 秒内二次确认） ----------

  function showPowerMenu() {
    if (!powerMenu) return;
    powerMenu.classList.remove("hidden");
    if (powerBtn) powerBtn.classList.add("open");
  }

  function hidePowerMenu() {
    if (powerMenu) powerMenu.classList.add("hidden");
    if (powerBtn) powerBtn.classList.remove("open");
    resetConfirm();
  }

  function togglePowerMenu() {
    if (!powerMenu) return;
    if (powerMenu.classList.contains("hidden")) showPowerMenu();
    else hidePowerMenu();
  }

  function menuButton(action) {
    if (!powerMenu) return null;
    return powerMenu.querySelector('[data-action="' + action + '"]');
  }

  function actionLabel(action) {
    return action === "shutdown" ? "关机" : "重启";
  }

  function resetConfirm() {
    if (!confirmState) return;
    clearTimeout(confirmState.timer);
    const btn = confirmState.btn;
    if (btn) {
      btn.textContent = actionLabel(confirmState.action);
      btn.classList.remove("confirm");
    }
    confirmState = null;
  }

  // 退出 easy-dash：首次点击进入 3 秒确认态，再次点击执行（托盘菜单异常时的备用入口）
  let quitArmed = null;

  function confirmAppQuit(btn) {
    if (!btn) return;
    if (quitArmed && quitArmed.btn === btn) {
      clearTimeout(quitArmed.timer);
      quitArmed = null;
      btn.textContent = "退出 easy-dash";
      btn.classList.remove("confirm");
      hidePowerMenu();
      runAction(() => invoke("quit_app"));
      return;
    }
    if (quitArmed) {
      clearTimeout(quitArmed.timer);
      quitArmed.btn.textContent = "退出 easy-dash";
      quitArmed.btn.classList.remove("confirm");
      quitArmed = null;
    }
    btn.textContent = "确认退出？";
    btn.classList.add("confirm");
    const timer = setTimeout(() => {
      if (!quitArmed) return;
      quitArmed.btn.textContent = "退出 easy-dash";
      quitArmed.btn.classList.remove("confirm");
      quitArmed = null;
    }, 3000);
    quitArmed = { btn, timer };
  }

  function onPowerMenuClick(action) {
    if (!action) return;
    if (action === "shutdown" || action === "restart") {
      confirmPowerAction(action, menuButton(action));
      return;
    }
    if (action === "quit-app") {
      confirmAppQuit(menuButton("quit-app"));
      return;
    }
    hidePowerMenu();
    // 睡眠 / 注销：立即执行
    runAction(() => invoke("power_action", { action }));
  }

  function confirmPowerAction(action, btn) {
    if (!btn) return;
    if (confirmState && confirmState.action === action) {
      // 3 秒内第二次点击：真正执行
      clearTimeout(confirmState.timer);
      confirmState = null;
      btn.textContent = actionLabel(action);
      btn.classList.remove("confirm");
      hidePowerMenu();
      runAction(() => invoke("power_action", { action }));
      return;
    }
    resetConfirm();
    btn.textContent = action === "shutdown" ? "确认关机？" : "确认重启？";
    btn.classList.add("confirm");
    confirmState = {
      action,
      btn,
      timer: setTimeout(() => {
        btn.textContent = actionLabel(action);
        btn.classList.remove("confirm");
        confirmState = null;
      }, 3000),
    };
  }

  // ---------- 内联提示（面板内红字 / 中性提示，3s 消失，不弹 alert） ----------

  function flash(text, isError) {
    if (!errEl) return;
    errEl.textContent = text;
    errEl.classList.toggle("danger", !!isError);
    errEl.classList.remove("hidden");
    if (errorTimer) clearTimeout(errorTimer);
    errorTimer = setTimeout(() => {
      errorTimer = null;
      if (errEl) errEl.classList.add("hidden");
    }, 3000);
  }

  function showError(msg) {
    flash("操作失败: " + msg, true);
  }

  function showNotice(msg) {
    flash(String(msg), false);
  }

  function hideError() {
    if (errorTimer) {
      clearTimeout(errorTimer);
      errorTimer = null;
    }
    if (errEl) {
      errEl.classList.add("hidden");
      errEl.textContent = "";
      errEl.classList.remove("danger");
    }
  }

  // ---------- 对外接口 ----------

  return {
    onOpen() {
      if (!el) return;
      panelOpen = true;
      hideError();
      // 每次打开都回到默认视图（连接子视图不保留）
      if (subKind) setSubView(null);
      syncTimer();
      refresh({ silent: false });
      loadBtCount();
      renderTheme(getThemeState());
      refreshMulti();
    },
    onClose() {
      panelOpen = false;
      setSubView(null);
      hidePowerMenu();
      hideError();
      syncTimer();
    },
    setEnabled(v) {
      const next = !!v;
      if (next === enabled) {
        if (enabled) syncTimer();
        return;
      }
      enabled = next;
      if (!enabled) {
        panelOpen = false;
        cancelVolumeNudge();
        cancelBrightnessNudge();
        cancelVolumeDrag();
        cancelBrightnessDrag();
        setSubView(null);
        hidePowerMenu();
        hideError();
        syncTimer();
        state = null;
        btCount = 0;
      } else {
        syncTimer();
        refresh({ silent: true });
        loadBtCount();
      }
    },
    isEnabled() {
      return enabled;
    },
    /** 顶栏音量滚轮入口：notches 正数=增大（每档 WHEEL_STEP%），返回后端权威值 */
    nudgeVolume,
    /** 滚轮亮度入口（预留给将来顶栏亮度图标） */
    nudgeBrightness,
  };
}

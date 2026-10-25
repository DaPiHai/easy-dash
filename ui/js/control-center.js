// 控制中心面板：磁贴网格（Wi-Fi/蓝牙/飞行模式/深色模式）+ 连接入口 + 音量/亮度滑条
// + 电源模式三档 + 电池行与电源动作。
// 连接入口点击后切到由 connections.js 渲染的子视图（Wi-Fi 网络 / 蓝牙设备）。
// 操作全部「乐观更新 + 面板打开时每 2s 轮询 get_quick_state 纠偏」。
import { invoke, invokeRetry } from "./api.js";
import { createConnections } from "./connections.js";

const PANEL_HTML = `
  <div class="cc-error hidden" id="cc-error"></div>
  <div class="cc-main" id="cc-main">
    <div class="cc-grid">
      <button class="cc-tile" id="cc-wifi" type="button">
        <span class="cc-tile-icon" id="cc-wifi-icon"></span>
        <span class="cc-tile-name">Wi-Fi</span>
        <span class="cc-tile-sub" id="cc-wifi-sub">--</span>
      </button>
      <button class="cc-tile" id="cc-bt" type="button">
        <span class="cc-tile-icon" id="cc-bt-icon"></span>
        <span class="cc-tile-name">蓝牙</span>
        <span class="cc-tile-sub" id="cc-bt-sub">--</span>
      </button>
      <button class="cc-tile" id="cc-airplane" type="button">
        <span class="cc-tile-icon" id="cc-airplane-icon"></span>
        <span class="cc-tile-name">飞行模式</span>
        <span class="cc-tile-sub" id="cc-airplane-sub">--</span>
      </button>
      <button class="cc-tile" id="cc-dark" type="button">
        <span class="cc-tile-icon" id="cc-dark-icon"></span>
        <span class="cc-tile-name">深色模式</span>
        <span class="cc-tile-sub" id="cc-dark-sub">--</span>
      </button>
    </div>
    <div class="cc-conns">
      <button class="cc-conn" id="cc-wifi-row" type="button">
        <span class="cc-conn-icon" id="cc-wifi-row-icon"></span>
        <span class="cc-conn-main">
          <span class="cc-conn-name">Wi-Fi</span>
          <span class="cc-conn-sub" id="cc-wifi-row-sub">--</span>
        </span>
        <span class="cc-conn-arrow">›</span>
      </button>
      <button class="cc-conn" id="cc-bt-row" type="button">
        <span class="cc-conn-icon" id="cc-bt-row-icon"></span>
        <span class="cc-conn-main">
          <span class="cc-conn-name">蓝牙</span>
          <span class="cc-conn-sub" id="cc-bt-row-sub">--</span>
        </span>
        <span class="cc-conn-arrow">›</span>
      </button>
    </div>
    <div class="cc-rows">
      <div class="cc-slider-row">
        <span class="cc-sl-icon" id="cc-vol-icon"></span>
        <input class="cc-slider" id="cc-volume" type="range" min="0" max="100" step="1" value="50" />
        <span class="cc-pct" id="cc-volume-pct">--</span>
        <button class="cc-mute" id="cc-mute" type="button" title="静音"></button>
      </div>
      <div class="cc-slider-row" id="cc-brightness-row">
        <span class="cc-sl-icon" id="cc-sun-icon"></span>
        <input class="cc-slider" id="cc-brightness" type="range" min="0" max="100" step="1" value="50" />
        <span class="cc-pct" id="cc-brightness-pct">--</span>
      </div>
    </div>
    <div class="cc-seg" id="cc-power">
      <button type="button" data-mode="efficiency">节能</button>
      <button type="button" data-mode="balanced">平衡</button>
      <button type="button" data-mode="performance">高性能</button>
    </div>
  </div>
  <div class="cc-sub hidden" id="cc-sub"></div>
  <div class="cc-battery">
    <span class="cc-bat-icon"><span class="cc-bat-fill" id="cc-bat-fill"></span></span>
    <span class="cc-bat-text" id="cc-bat-text">电量未知</span>
    <span class="cc-actions">
      <button class="cc-action" id="cc-settings" type="button">设置</button>
      <button class="cc-action" id="cc-lock" type="button">锁屏</button>
      <button class="cc-action" id="cc-power-btn" type="button">电源</button>
    </span>
  </div>
  <div class="cc-power-menu hidden" id="cc-power-menu">
    <button type="button" data-action="sleep">睡眠</button>
    <button type="button" data-action="shutdown">关机</button>
    <button type="button" data-action="restart">重启</button>
    <button type="button" data-action="signout">注销</button>
  </div>
`;

// 内联图标（currentColor 跟随磁贴状态变化）
function svgStroke(inner) {
  return (
    '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" ' +
    'stroke-width="2" stroke-linecap="round" stroke-linejoin="round">' +
    inner +
    "</svg>"
  );
}
function svgFill(inner) {
  return '<svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor" stroke="none">' + inner + "</svg>";
}

const WIFI_PATHS =
  '<path d="M5 12.55a11 11 0 0 1 14.08 0"/>' +
  '<path d="M1.42 9a16 16 0 0 1 21.16 0"/>' +
  '<path d="M8.53 16.11a6 6 0 0 1 6.95 0"/>' +
  '<circle cx="12" cy="20" r="1.2" fill="currentColor" stroke="none"/>';

/** 顶栏聚合图标与控制中心磁贴共用（topbar.js 也 import 它） */
export const ICONS = {
  wifi: svgStroke(WIFI_PATHS),
  wifiOff: svgStroke(WIFI_PATHS + '<path d="m2 2 20 20"/>'),
  bluetooth: svgStroke('<path d="m7 7 10 10-5 5V2l5 5L7 17"/>'),
  airplane: svgFill(
    '<path d="M17.8 19.2 16 11l3.5-3.5C21 6 21.5 4 21 3c-1-.5-3 0-4.5 1.5L13 8 4.8 6.2' +
      'c-.5-.1-1 .1-1.3.5l-.3.5c-.2.5-.1 1 .3 1.3L9 12l-2 3H4l-1 1 3 2 2 3 1-1v-3l3-2 3.5 5.3' +
      'c.3.4.8.5 1.3.3l.5-.2c.4-.3.6-.7.5-1.2z"/>'
  ),
  moon: svgStroke('<path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z"/>'),
  volume: svgStroke(
    '<path d="M11 5 6 9H2v6h4l5 4V5Z"/>' +
      '<path d="M15.5 8.5a5 5 0 0 1 0 7"/>' +
      '<path d="M18.5 5.5a9 9 0 0 1 0 13"/>'
  ),
  volumeMuted: svgStroke(
    '<path d="M11 5 6 9H2v6h4l5 4V5Z"/><path d="m23 9-6 6"/><path d="m17 9 6 6"/>'
  ),
  sun: svgStroke(
    '<circle cx="12" cy="12" r="4"/>' +
      '<path d="M12 2v2"/><path d="M12 20v2"/>' +
      '<path d="m4.93 4.93 1.41 1.41"/><path d="m17.66 17.66 1.41 1.41"/>' +
      '<path d="M2 12h2"/><path d="M20 12h2"/>' +
      '<path d="m6.34 17.66-1.41 1.41"/><path d="m19.07 4.93-1.41 1.41"/>'
  ),
};

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
  let btSub = null;
  let airplaneTile = null;
  let airplaneSub = null;
  let darkTile = null;
  let darkSub = null;
  let wifiRowIcon = null;
  let wifiRowSub = null;
  let btRowIcon = null;
  let btRowSub = null;
  let volIcon = null;
  let volumeSlider = null;
  let volumePct = null;
  let muteBtn = null;
  let brightnessRow = null;
  let brightnessSlider = null;
  let brightnessPct = null;
  let segEl = null;
  let batFill = null;
  let batText = null;
  let powerBtn = null;
  let powerMenu = null;

  if (el) {
    el.innerHTML = PANEL_HTML;
    collectRefs();
    setupConnections();
    bindDom();
    setStaticIcons();
  }

  // ---------- 初始化 ----------

  function collectRefs() {
    errEl = document.getElementById("cc-error");
    mainEl = document.getElementById("cc-main");
    subEl = document.getElementById("cc-sub");
    wifiTile = document.getElementById("cc-wifi");
    wifiIcon = document.getElementById("cc-wifi-icon");
    wifiSub = document.getElementById("cc-wifi-sub");
    btTile = document.getElementById("cc-bt");
    btSub = document.getElementById("cc-bt-sub");
    airplaneTile = document.getElementById("cc-airplane");
    airplaneSub = document.getElementById("cc-airplane-sub");
    darkTile = document.getElementById("cc-dark");
    darkSub = document.getElementById("cc-dark-sub");
    wifiRowIcon = document.getElementById("cc-wifi-row-icon");
    wifiRowSub = document.getElementById("cc-wifi-row-sub");
    btRowIcon = document.getElementById("cc-bt-row-icon");
    btRowSub = document.getElementById("cc-bt-row-sub");
    volIcon = document.getElementById("cc-vol-icon");
    volumeSlider = document.getElementById("cc-volume");
    volumePct = document.getElementById("cc-volume-pct");
    muteBtn = document.getElementById("cc-mute");
    brightnessRow = document.getElementById("cc-brightness-row");
    brightnessSlider = document.getElementById("cc-brightness");
    brightnessPct = document.getElementById("cc-brightness-pct");
    segEl = document.getElementById("cc-power");
    batFill = document.getElementById("cc-bat-fill");
    batText = document.getElementById("cc-bat-text");
    powerBtn = document.getElementById("cc-power-btn");
    powerMenu = document.getElementById("cc-power-menu");
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
    const btIcon = document.getElementById("cc-bt-icon");
    const airplaneIcon = document.getElementById("cc-airplane-icon");
    const darkIcon = document.getElementById("cc-dark-icon");
    const sunIcon = document.getElementById("cc-sun-icon");
    if (wifiIcon) wifiIcon.innerHTML = ICONS.wifi;
    if (btIcon) btIcon.innerHTML = ICONS.bluetooth;
    if (airplaneIcon) airplaneIcon.innerHTML = ICONS.airplane;
    if (darkIcon) darkIcon.innerHTML = ICONS.moon;
    if (sunIcon) sunIcon.innerHTML = ICONS.sun;
    if (volIcon) volIcon.innerHTML = ICONS.volume;
    if (muteBtn) muteBtn.innerHTML = ICONS.volume;
    if (wifiRowIcon) wifiRowIcon.innerHTML = ICONS.wifi;
    if (btRowIcon) btRowIcon.innerHTML = ICONS.bluetooth;
  }

  function bindDom() {
    if (wifiTile) wifiTile.addEventListener("click", onWifiClick);
    if (btTile) btTile.addEventListener("click", onBluetoothClick);
    if (airplaneTile) airplaneTile.addEventListener("click", onAirplaneClick);
    if (darkTile) darkTile.addEventListener("click", onDarkClick);

    const wifiRow = document.getElementById("cc-wifi-row");
    const btRow = document.getElementById("cc-bt-row");
    if (wifiRow) wifiRow.addEventListener("click", () => setSubView("wifi"));
    if (btRow) btRow.addEventListener("click", () => setSubView("bluetooth"));

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

    if (segEl) {
      segEl.addEventListener("click", (e) => {
        const btn = e.target.closest ? e.target.closest("[data-mode]") : null;
        if (btn) onPowerModeClick(btn.dataset.mode);
      });
    }

    const settingsBtn = document.getElementById("cc-settings");
    const lockBtn = document.getElementById("cc-lock");
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
      renderConnRows();
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
    renderConnRows();
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

  function setTile(tile, on, disabled) {
    if (!tile) return;
    tile.classList.toggle("on", !!on);
    tile.classList.toggle("disabled", !!disabled);
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

  /** 两个连接入口行的摘要 */
  function renderConnRows() {
    const s = state;
    if (wifiRowSub) {
      if (!s) wifiRowSub.textContent = "--";
      else if (!s.wifi) wifiRowSub.textContent = "不可用";
      else if (s.wifi.connected) wifiRowSub.textContent = "已连接 " + (s.wifi.ssid || "");
      else if (s.wifi.enabled) wifiRowSub.textContent = "未连接";
      else wifiRowSub.textContent = "已关闭";
    }
    if (btRowSub) {
      if (!s) btRowSub.textContent = "--";
      else if (s.bluetooth === null || s.bluetooth === undefined) btRowSub.textContent = "不可用";
      else if (!s.bluetooth) btRowSub.textContent = "已关闭";
      else btRowSub.textContent = btCount > 0 ? "已连接 " + btCount + " 台" : "已开启";
    }
    if (wifiRowIcon) {
      const on = !!(s && s.wifi && s.wifi.enabled);
      wifiRowIcon.innerHTML = on ? ICONS.wifi : ICONS.wifiOff;
    }
    if (btRowIcon) btRowIcon.innerHTML = ICONS.bluetooth;
  }

  function render() {
    const s = state;
    if (!s) return;

    // Wi-Fi 磁贴
    const wifiOn = !!(s.wifi && s.wifi.enabled);
    const wifiMissing = !s.wifi;
    setTile(wifiTile, wifiOn, wifiMissing);
    if (wifiIcon) wifiIcon.innerHTML = wifiOn ? ICONS.wifi : ICONS.wifiOff;
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
    setTile(btTile, s.bluetooth === true, btMissing);
    if (btSub) btSub.textContent = btMissing ? "不可用" : s.bluetooth ? "已开启" : "已关闭";

    // 飞行模式（由 Wi-Fi + 蓝牙状态推导）
    const airplaneOn = airplaneActive();
    setTile(airplaneTile, airplaneOn, !airplaneKnown());
    if (airplaneSub) airplaneSub.textContent = airplaneKnown() ? (airplaneOn ? "已开启" : "已关闭") : "不可用";

    // 深色模式
    const darkMissing = s.darkMode === null || s.darkMode === undefined;
    setTile(darkTile, s.darkMode === true, darkMissing);
    if (darkSub) darkSub.textContent = darkMissing ? "不可用" : s.darkMode ? "已开启" : "已关闭";

    // 音量滑条
    const muted = s.muted === true;
    if (muteBtn) {
      muteBtn.classList.toggle("on", muted);
      muteBtn.innerHTML = muted ? ICONS.volumeMuted : ICONS.volume;
    }
    if (volIcon) volIcon.innerHTML = muted ? ICONS.volumeMuted : ICONS.volume;
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
    if (segEl) {
      const buttons = segEl.querySelectorAll("button[data-mode]");
      for (const b of buttons) b.classList.toggle("active", b.dataset.mode === s.powerMode);
    }

    // 连接入口摘要
    renderConnRows();

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
            "cc-bat-fill" + (s.charging ? " charging" : s.battery <= 15 ? " low" : "");
          batFill.style.width = Math.max(6, Math.min(100, s.battery)) + "%";
        }
      } else {
        batText.textContent = s.acOnline ? "已接通电源" : "电量未知";
        if (batFill) {
          batFill.className = "cc-bat-fill";
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

  function onDarkClick() {
    if (!state) return;
    const next = !(state.darkMode === true);
    state.darkMode = next;
    render();
    runAction(() => invoke("set_dark_mode", { dark: next }));
  }

  function onMuteClick() {
    if (!state) return;
    const next = !(state.muted === true);
    state.muted = next;
    if (muteBtn) {
      muteBtn.classList.toggle("on", next);
      muteBtn.innerHTML = next ? ICONS.volumeMuted : ICONS.volume;
    }
    if (volIcon) volIcon.innerHTML = next ? ICONS.volumeMuted : ICONS.volume;
    runAction(() => invoke("set_muted", { muted: next }));
  }

  function onVolumeInput() {
    if (!volumeSlider || volumeSlider.disabled) return;
    const v = clampInt(volumeSlider.value, 0, 100);
    lastVolumeTouch = Date.now();
    // 乐观更新
    if (state) {
      state.volume = v;
      state.muted = false;
    }
    if (volumePct) volumePct.textContent = v + "%";
    updateSliderFill(volumeSlider, v);
    if (muteBtn) {
      muteBtn.classList.remove("on");
      muteBtn.innerHTML = ICONS.volume;
    }
    if (volIcon) volIcon.innerHTML = ICONS.volume;

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
    if (state) state.brightness = v;
    if (brightnessPct) brightnessPct.textContent = v + "%";
    updateSliderFill(brightnessSlider, v);

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

  function onPowerMenuClick(action) {
    if (!action) return;
    if (action === "shutdown" || action === "restart") {
      confirmPowerAction(action, menuButton(action));
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

  // ---------- 错误提示（面板内红字，3s 消失，不弹 alert） ----------

  function showError(msg) {
    if (!errEl) return;
    errEl.textContent = "操作失败: " + msg;
    errEl.classList.remove("hidden");
    if (errorTimer) clearTimeout(errorTimer);
    errorTimer = setTimeout(() => {
      errorTimer = null;
      if (errEl) errEl.classList.add("hidden");
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
  };
}

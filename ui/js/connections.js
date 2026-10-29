// 控制中心子视图：Wi-Fi 网络列表 / 蓝牙设备列表（挂到 #ctl-sub）。
// 由 control-center.js 提供三个回调：
//   - onError(msg)   显示错误（复用控制中心的 ctl-error 内联提示）
//   - onChanged()    连接状态变化后让控制中心刷新 get_quick_state / 设备计数
//   - onBack()       点「返回」回到控制中心默认视图
// 样式前缀 conn-（见 ui/css/panels.css），不再使用旧的 cv- 类。
import { invoke, invokeRetry, escapeHtml } from "./api.js";
import { icon } from "./icons.js";

/** 连接后轮询间隔 / 超时 */
const POLL_MS = 1500;
const CONNECT_TIMEOUT_MS = 10000;

function errText(e) {
  if (typeof e === "string") return e;
  if (e && typeof e.message === "string") return e.message;
  try {
    return String(e);
  } catch (_) {
    return "未知错误";
  }
}

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

/** 信号 0-100 → 4 格信号柱（宽度/高度由 CSS 控制，颜色随 token） */
function signalHtml(signal) {
  const q = Number(signal) || 0;
  const level = q >= 75 ? 4 : q >= 50 ? 3 : q >= 25 ? 2 : 1;
  let html = "";
  for (let i = 0; i < 4; i++) {
    html += '<span class="conn-bar' + (i < level ? " on" : "") + '"></span>';
  }
  return html;
}

/**
 * 创建连接子视图控制器。
 * @param {{host: HTMLElement, onError?: Function, onChanged?: Function, onBack?: Function}} opts
 * @returns {{open: Function, close: Function, isOpen: Function}}
 */
export function createConnections({ host, onError, onChanged, onBack } = {}) {
  const fail = typeof onError === "function" ? onError : () => {};
  const changed = typeof onChanged === "function" ? onChanged : () => {};
  const back = typeof onBack === "function" ? onBack : () => {};

  let current = null; // "wifi" | "bluetooth" | null
  let gen = 0; // 打开代次：切走后旧请求/轮询的结果直接丢弃
  let busy = false;
  let expandedSsid = null;
  let wifiList = [];
  let btList = [];
  let statusTimer = null;

  function q(sel) {
    return host ? host.querySelector(sel) : null;
  }

  function alive(g) {
    return !!host && current !== null && g === gen;
  }

  // ---------- 通用骨架 ----------

  function viewHtml(title, actionLabel) {
    return (
      '<div class="conn-head">' +
      '<button class="conn-back" type="button" data-conn-back="1">' +
      icon("chevronLeft", { size: 14 }) +
      "<span>返回</span></button>" +
      '<span class="conn-title">' +
      escapeHtml(title) +
      "</span>" +
      '<button class="conn-action" type="button" id="conn-settings">' +
      escapeHtml(actionLabel) +
      "</button>" +
      "</div>" +
      '<div class="conn-status" id="conn-status"></div>' +
      '<div class="conn-hint hidden" id="conn-hint"></div>' +
      '<div class="conn-list" id="conn-list"></div>'
    );
  }

  /** 渲染骨架并绑定返回 / 系统设置按钮 */
  function mountView(g, title, actionLabel, settingsPage) {
    if (!host) return;
    host.innerHTML = viewHtml(title, actionLabel);
    const backBtn = q("[data-conn-back]");
    if (backBtn) {
      backBtn.addEventListener("click", () => back());
    }
    const settingsBtn = q("#conn-settings");
    if (settingsBtn) {
      settingsBtn.addEventListener("click", () => {
        try {
          invoke("open_settings_page", { page: settingsPage }).catch((e) => fail(errText(e)));
        } catch (e) {
          fail(errText(e));
        }
      });
    }
    void g;
  }

  function listEl() {
    return q("#conn-list");
  }

  function setStatus(msg) {
    const el = q("#conn-status");
    if (!el) return;
    el.textContent = msg || "";
    el.classList.toggle("hidden", !msg);
    if (statusTimer) {
      clearTimeout(statusTimer);
      statusTimer = null;
    }
  }

  function scheduleStatusClear(ms) {
    if (statusTimer) clearTimeout(statusTimer);
    statusTimer = setTimeout(() => {
      statusTimer = null;
      setStatus("");
    }, ms || 4000);
  }

  function setListEmpty(msg) {
    const el = listEl();
    if (el) el.innerHTML = '<div class="conn-empty">' + escapeHtml(msg) + "</div>";
  }

  function showHint(msg) {
    const el = q("#conn-hint");
    if (!el) return;
    el.textContent = msg;
    el.classList.remove("hidden");
  }

  // ==================== Wi-Fi ====================

  async function openWifi(g) {
    expandedSsid = null;
    wifiList = [];
    if (!host) return;
    mountView(g, "Wi-Fi", "打开系统设置", "network-wifi");
    setStatus("扫描中…");
    setListEmpty("扫描中…");
    try {
      const list = await invokeRetry(
        "get_wifi_networks",
        { forceScan: true },
        { attempts: 2, perTryMs: 6000 }
      );
      if (!alive(g)) return;
      setStatus("");
      renderWifi(list);
    } catch (e) {
      if (!alive(g)) return;
      setStatus("");
      setListEmpty("无法读取 Wi-Fi 列表");
      fail(errText(e));
    }
  }

  function renderWifi(list) {
    const el = listEl();
    if (!el) return;
    wifiList = (Array.isArray(list) ? list : [])
      .filter((n) => n && typeof n.ssid === "string" && n.ssid)
      .sort(
        (a, b) =>
          (b.connected ? 1 : 0) - (a.connected ? 1 : 0) || (Number(b.signal) || 0) - (Number(a.signal) || 0)
      );
    if (!wifiList.length) {
      setListEmpty("没有扫描到网络");
      return;
    }
    el.innerHTML = wifiList.map(wifiRowHtml).join("");
  }

  function wifiRowHtml(n) {
    const connected = !!n.connected;
    const expanded = expandedSsid === n.ssid && !connected;
    const lock = n.secure
      ? '<span class="conn-lock" title="加密网络">' + icon("lockKeyhole", { size: 12 }) + "</span>"
      : "";
    const badge = connected ? '<span class="conn-badge">已连接</span>' : "";
    const btn = connected
      ? '<button class="conn-btn danger" type="button" data-act="disconnect">断开</button>'
      : '<button class="conn-btn" type="button" data-act="connect">连接</button>';
    const pwd = expanded
      ? '<div class="conn-pwd"><input class="conn-input" type="password" placeholder="输入 Wi-Fi 密码" ' +
        'autocomplete="off" spellcheck="false" /><button class="conn-btn primary" type="button" ' +
        'data-act="pwd-connect">连接</button></div>'
      : "";
    return (
      '<div class="conn-row' +
      (connected ? " connected" : "") +
      (expanded ? " expanded" : "") +
      '" data-ssid="' +
      escapeHtml(n.ssid) +
      '">' +
      '<span class="conn-signal">' + signalHtml(n.signal) + "</span>" +
      '<span class="conn-name">' + escapeHtml(n.ssid) + "</span>" +
      lock +
      badge +
      '<span class="conn-right">' + btn + "</span>" +
      pwd +
      "</div>"
    );
  }

  function bindWifiList() {
    const el = listEl();
    if (!el) return;
    el.addEventListener("click", (e) => {
      const row = e.target.closest ? e.target.closest(".conn-row") : null;
      if (!row) return;
      const ssid = row.dataset.ssid || "";
      const net = wifiList.find((n) => n.ssid === ssid);
      if (!net) return;
      const actEl = e.target.closest ? e.target.closest("[data-act]") : null;
      const act = actEl ? actEl.dataset.act : "";

      if (act === "disconnect") {
        disconnectWifi(ssid);
        return;
      }
      if (act === "pwd-connect") {
        submitPassword(row, net);
        return;
      }
      if (act === "connect" || !act) {
        if (net.connected) return; // 已连接的行只能点「断开」
        handleConnectClick(net);
      }
    });

    // 密码框回车提交
    el.addEventListener("keydown", (e) => {
      if (e.key !== "Enter") return;
      const input = e.target.closest ? e.target.closest(".conn-input") : null;
      if (!input) return;
      e.preventDefault();
      const row = input.closest(".conn-row");
      if (!row) return;
      const ssid = row.dataset.ssid || "";
      const net = wifiList.find((n) => n.ssid === ssid);
      if (net) submitPassword(row, net);
    });
  }

  function focusPasswordInput() {
    const input = q(".conn-input");
    if (input) {
      try {
        input.focus();
        input.select();
      } catch (e) {
        /* 忽略 */
      }
    }
  }

  function handleConnectClick(net) {
    if (busy) return;
    // 加密且没有保存过密码 → 行内展开密码输入
    if (net.secure && !net.saved) {
      const willExpand = expandedSsid !== net.ssid;
      expandedSsid = willExpand ? net.ssid : null;
      renderWifi(wifiList);
      if (willExpand) focusPasswordInput();
      return;
    }
    // 开放网络 / 已保存密码 → 直接连接（密码传 null）
    connectWifi(net.ssid, null);
  }

  function submitPassword(row, net) {
    if (busy) return;
    const input = row.querySelector(".conn-input");
    const pwd = input ? input.value : "";
    if (!pwd) {
      fail("请输入 Wi-Fi 密码");
      if (input) input.focus();
      return;
    }
    connectWifi(net.ssid, pwd);
  }

  async function connectWifi(ssid, password) {
    if (busy) return;
    busy = true;
    const g = gen;
    setStatus("正在连接 " + ssid + "…");
    try {
      await invoke("connect_wifi", { ssid, password: password == null ? null : password });
      if (!alive(g)) return;
      await waitWifiConnected(ssid, g);
    } catch (e) {
      if (!alive(g)) return;
      setStatus("");
      fail(errText(e));
    } finally {
      busy = false;
    }
  }

  /** 连接后每 1.5s 轮询，直到该 SSID connected 或 10s 超时 */
  async function waitWifiConnected(ssid, g) {
    const deadline = Date.now() + CONNECT_TIMEOUT_MS;
    while (Date.now() < deadline) {
      await sleep(POLL_MS);
      if (!alive(g)) return;
      try {
        const list = await invoke("get_wifi_networks", { forceScan: false });
        if (!alive(g)) return;
        const hit = (Array.isArray(list) ? list : []).find((n) => n && n.ssid === ssid && n.connected);
        if (hit) {
          expandedSsid = null;
          renderWifi(list);
          setStatus("已连接到 " + ssid);
          scheduleStatusClear();
          changed();
          return;
        }
      } catch (e) {
        /* 轮询失败继续等 */
      }
    }
    if (!alive(g)) return;
    setStatus("");
    fail("连接超时：请检查密码，或靠近路由器后重试");
  }

  async function disconnectWifi(ssid) {
    if (busy) return;
    busy = true;
    const g = gen;
    setStatus("正在断开…");
    try {
      await invoke("disconnect_wifi");
      if (!alive(g)) return;
      await sleep(1000);
      if (!alive(g)) return;
      try {
        const list = await invoke("get_wifi_networks", { forceScan: false });
        if (!alive(g)) return;
        renderWifi(list);
      } catch (e) {
        /* 列表刷新失败不影响断开结果 */
      }
      setStatus("已断开" + (ssid ? " " + ssid : ""));
      scheduleStatusClear();
      changed();
    } catch (e) {
      if (!alive(g)) return;
      setStatus("");
      fail(errText(e));
    } finally {
      busy = false;
    }
  }

  // ==================== 蓝牙 ====================

  async function openBluetooth(g) {
    if (!host) return;
    btList = [];
    mountView(g, "蓝牙", "添加设备", "bluetooth");
    setStatus("读取设备…");
    setListEmpty("读取设备…");
    await loadBtDevices(g, true);
  }

  async function loadBtDevices(g, first) {
    const el = listEl();
    if (!el) return;
    try {
      const list = first
        ? await invokeRetry("get_bt_devices", undefined, { attempts: 2, perTryMs: 4000 })
        : await invoke("get_bt_devices");
      if (!alive(g)) return;
      btList = (Array.isArray(list) ? list : []).filter((d) => d && typeof d === "object");
      renderBt();
      // 首刷的「读取设备…」也要清掉，否则会一直挂在列表上方
      setStatus("");
    } catch (e) {
      if (!alive(g)) return;
      setStatus("");
      setListEmpty("无法读取蓝牙设备");
      fail(errText(e));
    }
  }

  function renderBt() {
    const el = listEl();
    if (!el) return;
    if (!btList.length) {
      setListEmpty("没有已配对的设备");
      return;
    }
    el.innerHTML = btList
      .map((d) => {
        const connected = !!d.connected;
        return (
          '<div class="conn-row' +
          (connected ? " connected" : "") +
          '" data-addr="' +
          String(d.address == null ? "" : d.address) +
          '">' +
          '<span class="conn-name">' + escapeHtml(d.name || "未知设备") + "</span>" +
          (connected ? '<span class="conn-badge">已连接</span>' : "") +
          '<span class="conn-right"><button class="conn-btn ' +
          (connected ? "danger" : "primary") +
          '" type="button" data-act="toggle">' +
          (connected ? "断开" : "连接") +
          "</button></span>" +
          "</div>"
        );
      })
      .join("");
  }

  function bindBtList() {
    const el = listEl();
    if (!el) return;
    el.addEventListener("click", (e) => {
      const btn = e.target.closest ? e.target.closest("[data-act]") : null;
      if (!btn || btn.dataset.act !== "toggle") return;
      const row = btn.closest(".conn-row");
      if (!row) return;
      const address = Number(row.dataset.addr);
      if (!Number.isFinite(address)) return;
      const dev = btList.find((d) => Number(d.address) === address);
      if (!dev) return;
      toggleBt(address, !dev.connected);
    });
  }

  async function toggleBt(address, connect) {
    if (busy) return;
    busy = true;
    const g = gen;
    setStatus(connect ? "正在连接设备…" : "正在断开设备…");
    hideHint();
    try {
      await invoke("set_bt_connected", { address, connect });
      if (!alive(g)) return;
      // 系统建立/释放连接需要一点时间
      await sleep(connect ? 1200 : 800);
      if (!alive(g)) return;
      await loadBtDevices(g, false);
      if (!alive(g)) return;
      setStatus(connect ? "已连接" : "已断开");
      scheduleStatusClear();
      changed();
    } catch (e) {
      if (!alive(g)) return;
      setStatus("");
      fail(errText(e));
      showHint("可尝试在 Windows 设置中连接");
    } finally {
      busy = false;
    }
  }

  function hideHint() {
    const el = q("#conn-hint");
    if (el) {
      el.textContent = "";
      el.classList.add("hidden");
    }
  }

  // ==================== 对外接口 ====================

  return {
    /** 打开子视图："wifi" | "bluetooth" */
    open(kind) {
      if (!host || (kind !== "wifi" && kind !== "bluetooth")) return;
      current = kind;
      gen++;
      busy = false;
      expandedSsid = null;
      if (statusTimer) {
        clearTimeout(statusTimer);
        statusTimer = null;
      }
      if (kind === "wifi") {
        openWifi(gen);
        bindWifiList();
      } else {
        openBluetooth(gen);
        bindBtList();
      }
    },
    /** 收起子视图（清理内容与定时器） */
    close() {
      current = null;
      gen++;
      busy = false;
      expandedSsid = null;
      if (statusTimer) {
        clearTimeout(statusTimer);
        statusTimer = null;
      }
      if (host) host.innerHTML = "";
    },
    isOpen() {
      return current !== null;
    },
  };
}

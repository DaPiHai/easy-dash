import { invoke, invokeRetry, listen, escapeHtml, basename } from "./api.js";

let config = { pinned: [] };
let windows = [];
let pinnedIcons = [];
let focusedId = 0;

const itemsEl = document.getElementById("items");
const dividerEl = document.getElementById("divider");
const tooltipEl = document.getElementById("tooltip");
const menuEl = document.getElementById("menu");

const LETTER_COLORS = ["#3584e4", "#e66100", "#2ec27e", "#9141ac", "#e01b24", "#1c71d8", "#8f4e2b"];

async function init() {
  config = await invokeRetry("get_config").catch(() => config);
  windows = await invokeRetry("list_windows").catch(() => []);
  await loadPinnedIcons();
  render();

  listen("windows-changed", (e) => {
    windows = e.payload || [];
    render();
  });
  listen("config-changed", async (e) => {
    config = e.payload || config;
    await loadPinnedIcons();
    render();
  });
  listen("foreground-changed", (e) => {
    focusedId = e.payload || 0;
    render();
  });
  listen("fullscreen-changed", (e) => {
    document.body.classList.toggle("hidden", e.payload === true);
  });
  // 工作线程补全图标后刷新（去抖，避免逐图标重渲染）
  let iconReadyTimer = null;
  listen("icon-ready", () => {
    if (iconReadyTimer) return;
    iconReadyTimer = setTimeout(async () => {
      iconReadyTimer = null;
      await loadPinnedIcons();
      render();
    }, 300);
  });
}

async function loadPinnedIcons() {
  try {
    pinnedIcons = await invokeRetry(
      "get_pinned_icons",
      { paths: config.pinned.map((p) => p.path) },
      { attempts: 3, perTryMs: 4000 }
    );
  } catch {
    pinnedIcons = [];
  }
}

/// 分组 + 合并出 Dock 条目：UWP/打包应用按 AUMID 独立分组
/// （多个应用可能共用宿主 applicationframehost.exe，不能按 exe 合并），
/// 桌面应用按进程名分组。
function buildItems() {
  const groups = new Map(); // key → { key, aumid, exe, wins[] }
  for (const w of windows) {
    const key = w.aumid ? "aumid:" + w.aumid : w.exe;
    if (!groups.has(key)) {
      groups.set(key, { key, aumid: w.aumid || null, exe: w.exe, wins: [], consumed: false });
    }
    groups.get(key).wins.push(w);
  }

  const MONIKER = "shell:AppsFolder\\";
  const items = [];
  config.pinned.forEach((p, i) => {
    let g = null;
    if (p.path.startsWith(MONIKER)) {
      const a = p.path.slice(MONIKER.length).toLowerCase();
      for (const grp of groups.values()) {
        if (grp.aumid && grp.aumid.toLowerCase() === a) {
          g = grp;
          break;
        }
      }
    } else {
      g = groups.get(basename(p.path).toLowerCase()) || null;
    }
    const wins = g ? g.wins : [];
    if (g) g.consumed = true;
    items.push({
      key: "pin:" + p.path.toLowerCase(),
      name: p.name,
      path: p.path,
      args: p.args || "",
      icon: pinnedIcons[i] || null,
      wins,
      pinned: true,
    });
  });

  // 运行中但未固定的：先按手动拖拽顺序（config.dockOrder），
  // 未手动排序的按窗口打开顺序（首次出现序号）排在后面
  const orderIdx = new Map((config.dockOrder || []).map((k, i) => [k, i]));
  const unpinned = [];
  for (const g of groups.values()) {
    if (g.consumed) continue;
    const top = g.wins[0];
    unpinned.push({
      key: g.key,
      name: top.appName || g.exe.replace(/\.exe$/i, ""),
      path: g.aumid ? MONIKER + g.aumid : top.path || "",
      args: "",
      icon: top.icon || null,
      wins: g.wins,
      pinned: false,
      openOrder: Math.min(...g.wins.map((w) => w.order || 0)),
    });
  }
  unpinned.sort((a, b) => {
    const ra = orderIdx.has(a.key) ? orderIdx.get(a.key) : null;
    const rb = orderIdx.has(b.key) ? orderIdx.get(b.key) : null;
    if (ra != null && rb != null) return ra - rb;
    if (ra != null) return -1;
    if (rb != null) return 1;
    // 顺序号唯一；再加 key 作为确定性兜底，任何情况下排序结果都稳定
    return a.openOrder - b.openOrder || (a.key < b.key ? -1 : a.key > b.key ? 1 : 0);
  });
  items.push(...unpinned);
  return items;
}

function itemHtml(item, index) {
  const running = item.wins.length > 0;
  const focused = item.wins.some((w) => w.id === focusedId);
  const icon = item.icon
    ? `<img src="${item.icon}" alt="">`
    : `<span class="letter" style="background:${LETTER_COLORS[index % LETTER_COLORS.length]}">${escapeHtml((item.name || "?").slice(0, 1).toUpperCase())}</span>`;
  const cls = ["dock-item", running ? "running" : "", focused ? "focused" : ""]
    .filter(Boolean)
    .join(" ");
  return `<button class="${cls}" data-key="${escapeHtml(item.key)}" data-name="${escapeHtml(item.name)}" aria-label="${escapeHtml(item.name)}">${icon}</button>`;
}

let currentItems = [];
let renderPending = false;

function render() {
  // 拖拽中暂停重渲染（事件到来时先挂起，结束后补渲染）
  if (dragState && dragState.moved) {
    renderPending = true;
    return;
  }
  const items = buildItems();
  currentItems = items;
  dividerEl.classList.toggle("hidden", items.length === 0);
  itemsEl.innerHTML = items.map(itemHtml).join("");

  // 绑定事件
  for (const el of itemsEl.children) {
    const item = items.find((it) => it.key === el.dataset.key);
    el.addEventListener("click", () => {
      if (suppressClick) {
        suppressClick = false;
        return;
      }
      onItemClick(item);
    });
    el.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      showMenu(item, e.clientX, e.clientY);
    });
    el.addEventListener("mouseenter", () => showTooltip(el, item));
    el.addEventListener("mousemove", () => showTooltip(el, item));
    el.addEventListener("mouseleave", () => tooltipEl.classList.add("hidden"));
  }

  magnifyLastMouse = null;
  applyMagnify();
  syncSize();
}

async function onItemClick(item) {
  if (!item) return;
  if (!item.wins.length) {
    try {
      await invoke("launch_app", { path: item.path, args: item.args });
    } catch (err) {
      console.error("启动失败", err);
    }
    return;
  }
  // 优先用组内"主窗口"（面积足够的真实窗口）作为激活/循环目标：
  // Electron 应用常有同标题的悬浮小窗排在最前，直接点它会看起来没反应
  const big = item.wins.filter(
    (w) => (w.width || 0) * (w.height || 0) >= 40000
  );
  const pool = big.length ? big : item.wins;
  const focusedWin = item.wins.find((w) => w.id === focusedId);
  if (focusedWin) {
    // 组内当前有前台窗口：多窗口时循环切换，否则最小化
    if (pool.length > 1) {
      const idx = pool.findIndex((w) => w.id === focusedWin.id);
      const next = idx >= 0 ? pool[(idx + 1) % pool.length] : pool[0];
      if (next && next.id !== focusedWin.id) {
        await invoke("activate_window", { id: next.id }).catch(() => {});
        return;
      }
    }
    await invoke("toggle_window", { id: focusedWin.id }).catch(() => {});
  } else {
    await invoke("activate_window", { id: pool[0].id }).catch(() => {});
  }
}

// ---------- tooltip ----------
function showTooltip(el, item) {
  if (dragState && dragState.moved) return;
  const title = item.wins.length
    ? item.wins[0].title
    : item.name;
  if (!title) return;
  tooltipEl.textContent = title;
  tooltipEl.classList.remove("hidden");
  const r = el.getBoundingClientRect();
  tooltipEl.style.left = Math.max(60, r.left + r.width / 2) + "px";
  tooltipEl.style.top = Math.max(18, r.top - 30) + "px";
}

// ---------- 右键菜单（固定/取消固定） ----------
function showMenu(item, x, y) {
  const entries = item.pinned
    ? [{ label: "从 Dock 取消固定", act: () => unpin(item) }]
    : item.path
      ? [{ label: "固定到 Dock", act: () => pin(item) }]
      : [];
  if (!entries.length) return;
  menuEl.innerHTML = entries
    .map((en, i) => `<button class="mi" data-i="${i}">${escapeHtml(en.label)}</button>`)
    .join("");
  menuEl.classList.remove("hidden");
  const mw = menuEl.offsetWidth, mh = menuEl.offsetHeight;
  menuEl.style.left = Math.min(x, window.innerWidth - mw - 4) + "px";
  menuEl.style.top = Math.max(4, y - mh - 8) + "px";
  menuEl.querySelectorAll(".mi").forEach((btn) => {
    btn.addEventListener("click", () => {
      menuEl.classList.add("hidden");
      entries[+btn.dataset.i].act();
    });
  });
}

document.addEventListener("mousedown", (e) => {
  if (!menuEl.contains(e.target)) menuEl.classList.add("hidden");
});

async function pin(item) {
  config.pinned.push({ name: item.name, path: item.path, args: item.args });
  await invoke("set_pinned", { pinned: config.pinned }).catch(() => {});
}

async function unpin(item) {
  config.pinned = config.pinned.filter((p) => p.path !== item.path);
  await invoke("set_pinned", { pinned: config.pinned }).catch(() => {});
}

// ---------- 悬停放大 ----------
let magnifyLastMouse = null;

itemsEl.addEventListener("mousemove", (e) => {
  magnifyLastMouse = e.clientX;
  applyMagnify();
});
itemsEl.addEventListener("mouseleave", () => {
  magnifyLastMouse = null;
  applyMagnify();
});

function applyMagnify() {
  if (dragState && dragState.moved) {
    for (const el of itemsEl.children) el.style.transform = "";
    return;
  }
  for (const el of itemsEl.children) {
    if (magnifyLastMouse == null) {
      el.style.transform = "";
      continue;
    }
    const r = el.getBoundingClientRect();
    const d = Math.abs(magnifyLastMouse - (r.left + r.width / 2));
    const s = 1 + 0.3 * Math.max(0, 1 - d / 110);
    el.style.transform = `scale(${s.toFixed(3)})`;
  }
}

// ---------- 拖拽排序 ----------
// 固定区 / 未固定区各自内部排序（拖拽不跨区）：
// 固定区排序写回 config.pinned；未固定区写回 config.dockOrder
let dragState = null;
let suppressClick = false;

itemsEl.addEventListener("pointerdown", (e) => {
  if (e.button !== 0) return;
  const btn = e.target.closest(".dock-item");
  if (!btn || !itemsEl.contains(btn)) return;
  const idx = [...itemsEl.children].indexOf(btn);
  const item = currentItems[idx];
  if (!item) return;
  const pinnedCount = currentItems.filter((it) => it.pinned).length;
  const start = item.pinned ? 0 : pinnedCount;
  const end = item.pinned ? pinnedCount : currentItems.length;
  if (end - start <= 1) return;
  const r = btn.getBoundingClientRect();
  dragState = {
    btn,
    item,
    startX: e.clientX,
    startY: e.clientY,
    moved: false,
    ghost: null,
    grabX: e.clientX - r.left,
    grabY: e.clientY - r.top,
    range: [start, end],
    afterBoundary: item.pinned ? itemsEl.children[end] || null : null,
  };
});

window.addEventListener("pointermove", (e) => {
  if (!dragState) return;
  if (!dragState.moved) {
    if (Math.hypot(e.clientX - dragState.startX, e.clientY - dragState.startY) < 5) return;
    beginDrag();
  }
  updateGhost(e.clientX, e.clientY);
  updateReorder(e.clientX);
});

window.addEventListener("pointerup", () => {
  if (!dragState) return;
  const d = dragState;
  dragState = null;
  if (!d.moved) {
    if (renderPending) {
      renderPending = false;
      render();
    }
    return;
  }
  suppressClick = true;
  if (d.ghost) d.ghost.remove();
  d.btn.classList.remove("drag-src");
  itemsEl.classList.remove("drag-active");
  commitOrder(d);
});

function beginDrag() {
  const d = dragState;
  d.moved = true;
  tooltipEl.classList.add("hidden");
  const r = d.btn.getBoundingClientRect();
  const ghost = d.btn.cloneNode(true);
  ghost.className = d.btn.className + " drag-ghost";
  ghost.style.cssText =
    "position:fixed;pointer-events:none;z-index:80;" +
    `left:${r.left}px;top:${r.top}px;width:${r.width}px;height:${r.height}px;`;
  document.body.appendChild(ghost);
  d.ghost = ghost;
  d.btn.classList.add("drag-src");
  itemsEl.classList.add("drag-active");
  magnifyLastMouse = null;
  applyMagnify();
}

function updateGhost(x, y) {
  const d = dragState;
  if (!d || !d.ghost) return;
  d.ghost.style.left = x - d.grabX + "px";
  d.ghost.style.top = y - d.grabY + "px";
}

function updateReorder(x) {
  const d = dragState;
  const [start, end] = d.range;
  const kids = [...itemsEl.children];
  let ref = null;
  for (let i = start; i < end; i++) {
    const el = kids[i];
    if (el === d.btn) continue;
    const r = el.getBoundingClientRect();
    if (x < r.left + r.width / 2) {
      ref = el;
      break;
    }
  }
  if (ref) {
    if (d.btn.nextElementSibling !== ref) itemsEl.insertBefore(d.btn, ref);
  } else if (d.btn.nextElementSibling !== d.afterBoundary) {
    itemsEl.insertBefore(d.btn, d.afterBoundary);
  }
}

async function commitOrder(d) {
  const orderedKeys = [...itemsEl.children].map((el) => el.dataset.key);
  if (d.item.pinned) {
    const byKey = new Map(config.pinned.map((p) => ["pin:" + p.path.toLowerCase(), p]));
    const newPinned = orderedKeys
      .filter((k) => byKey.has(k))
      .map((k) => byKey.get(k));
    if (newPinned.length === config.pinned.length) {
      config.pinned = newPinned;
      await invoke("set_pinned", { pinned: newPinned }).catch(() => {});
    }
  } else {
    const byKey = new Map(currentItems.map((it) => [it.key, it]));
    const newOrder = orderedKeys.filter((k) => byKey.has(k) && !byKey.get(k).pinned);
    config.dockOrder = newOrder;
    await invoke("set_dock_order", { keys: newOrder }).catch(() => {});
    renderPending = false;
    render();
  }
}

// ---------- 尺寸同步 ----------
let lastW = 0, lastH = 0;
function syncSize() {
  const dockEl = document.getElementById("dock");
  // scrollWidth 能量出超出窗口的真实内容宽度（offsetWidth 会被窗口宽度卡住导致死锁）
  const w = Math.ceil(dockEl.scrollWidth) + 20;
  const h = Math.ceil(dockEl.offsetHeight) + 14; // 顶部余量给 tooltip
  if (Math.abs(w - lastW) >= 4 || Math.abs(h - lastH) >= 2) {
    lastW = w;
    lastH = h;
    invoke("resize_dock", { width: w, height: h }).catch(() => {});
  }
}

document.getElementById("grid").addEventListener("click", () => {
  invoke("toggle_app_center").catch(() => {});
});

// ---------- 自愈：错误横幅 + 心跳 + 兜底轮询 ----------
function showError(msg) {
  let banner = document.getElementById("err-banner");
  if (!banner) {
    banner = document.createElement("div");
    banner.id = "err-banner";
    banner.style.cssText =
      "position:fixed;bottom:4px;left:50%;transform:translateX(-50%);" +
      "background:rgba(192,28,40,.92);color:#fff;font-size:11px;padding:3px 10px;" +
      "border-radius:8px;max-width:80vw;overflow:hidden;white-space:nowrap;z-index:99";
    document.body.appendChild(banner);
  }
  banner.textContent = "Dock 异常: " + msg;
}

window.addEventListener("error", (e) => showError(e.message || String(e.error)));

// ---------- 启动诊断徽标 & IPC 超时 ----------
let stageEl = null;
function setStage(msg, isError) {
  if (!stageEl) {
    stageEl = document.createElement("div");
    stageEl.id = "boot-stage";
    document.body.appendChild(stageEl);
  }
  stageEl.style.cssText =
    "position:fixed;bottom:4px;left:50%;transform:translateX(-50%);font-size:11px;" +
    "padding:3px 10px;border-radius:8px;max-width:90vw;white-space:nowrap;overflow:hidden;" +
    "z-index:99;color:#fff;background:" +
    (isError ? "rgba(192,28,40,.92)" : "rgba(53,132,228,.92)");
  stageEl.textContent = msg;
}

function withTimeout(promise, ms, label) {
  return Promise.race([
    promise,
    new Promise((_, reject) =>
      setTimeout(() => reject(new Error("IPC 超时: " + label)), ms)
    ),
  ]);
}

async function waitTauri(timeoutMs = 30000) {
  const t0 = Date.now();
  while (!(window.__TAURI__ && window.__TAURI__.core)) {
    if (Date.now() - t0 > timeoutMs) throw new Error("Tauri API 注入超时");
    await new Promise((r) => setTimeout(r, 200));
  }
}

async function main() {
  setStage("Dock 脚本已启动，等待 Tauri API…");
  // 心跳尽早开始：即使 init 因 IPC 重试而变慢，看门狗也不会误判死亡
  let hbStarted = false;
  const startHb = () => {
    if (hbStarted) return;
    hbStarted = true;
    setInterval(() => invoke("dock_heartbeat").catch(() => {}), 3000);
  };
  await waitTauri(10000);
  setStage("Tauri 就绪，初始化…");
  await init();
  startHb();
  if (stageEl) {
    stageEl.remove();
    stageEl = null;
  }
  // 兜底：每 8 秒主动拉一次窗口列表，防事件丢失导致状态陈旧
  setInterval(async () => {
    try {
      windows = await invokeRetry("list_windows", undefined, {
        attempts: 2,
        perTryMs: 5000,
      });
      render();
    } catch {}
  }, 8000);
}

main().catch((e) => setStage("Dock 启动失败: " + (e.message || e), true));

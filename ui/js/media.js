// 通知中心右栏媒体卡：Windows SMTC 当前媒体会话。
// 封面 / 标题 / 歌手 / 进度条 + 上一首·播放暂停·下一首。
// 面板打开时 start()（立即拉一次 + 每 2s 轮询），关闭时 stop()。
import { invoke, invokeRetry } from "./api.js";

/** 轮询间隔（毫秒） */
const POLL_MS = 2000;

function svg(inner, size) {
  const s = size || 16;
  return (
    '<svg width="' +
    s +
    '" height="' +
    s +
    '" viewBox="0 0 24 24" fill="none" stroke="currentColor" ' +
    'stroke-width="2" stroke-linecap="round" stroke-linejoin="round">' +
    inner +
    "</svg>"
  );
}

const ICONS = {
  prev: svg('<path d="M19 20 9 12l10-8v16Z" fill="currentColor" stroke="none"/><path d="M5 19V5"/>'),
  next: svg('<path d="m5 4 10 8-10 8V4Z" fill="currentColor" stroke="none"/><path d="M19 5v14"/>'),
  play: svg('<path d="m7 4 13 8-13 8V4Z" fill="currentColor" stroke="none"/>'),
  pause: svg('<path d="M8 4h4v16H8z" fill="currentColor" stroke="none"/><path d="M16 4h4v16h-4z" fill="currentColor" stroke="none"/>'),
  note: svg(
    '<path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>',
    22
  ),
};

const CARD_HTML = `
  <div class="mc">
    <div class="mc-error hidden" id="mc-error"></div>
    <div class="mc-top">
      <div class="mc-cover" id="mc-cover"></div>
      <div class="mc-meta">
        <div class="mc-title" id="mc-title">无媒体播放</div>
        <div class="mc-artist" id="mc-artist"></div>
      </div>
    </div>
    <div class="mc-progress hidden" id="mc-progress">
      <div class="mc-bar"><div class="mc-bar-fill" id="mc-fill"></div></div>
      <div class="mc-times"><span id="mc-pos">0:00</span><span id="mc-dur">0:00</span></div>
    </div>
    <div class="mc-controls hidden" id="mc-controls">
      <button class="mc-btn" id="mc-prev" type="button" title="上一首"></button>
      <button class="mc-btn mc-btn-main" id="mc-toggle" type="button" title="播放/暂停"></button>
      <button class="mc-btn" id="mc-next" type="button" title="下一首"></button>
    </div>
  </div>
`;

function errText(e) {
  if (typeof e === "string") return e;
  if (e && typeof e.message === "string") return e.message;
  try {
    return String(e);
  } catch (_) {
    return "未知错误";
  }
}

/** 毫秒 → m:ss（未知返回 "--:--"） */
function fmtTime(ms) {
  const n = Number(ms);
  if (!Number.isFinite(n) || n <= 0) return "0:00";
  const total = Math.floor(n / 1000);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return m + ":" + (s < 10 ? "0" + s : String(s));
}

/**
 * 创建媒体卡并挂到 container 上。
 * @param {HTMLElement} container 右栏的媒体卡容器
 * @returns {{start: Function, stop: Function, refresh: Function}}
 */
export function createMediaCard(container) {
  if (!container) {
    return { start() {}, stop() {}, refresh() {} };
  }
  container.innerHTML = CARD_HTML;

  // 引用必须等 innerHTML 落地后再取
  const coverEl = document.getElementById("mc-cover");
  const titleEl = document.getElementById("mc-title");
  const artistEl = document.getElementById("mc-artist");
  const progressEl = document.getElementById("mc-progress");
  const fillEl = document.getElementById("mc-fill");
  const posEl = document.getElementById("mc-pos");
  const durEl = document.getElementById("mc-dur");
  const controlsEl = document.getElementById("mc-controls");
  const prevBtn = document.getElementById("mc-prev");
  const toggleBtn = document.getElementById("mc-toggle");
  const nextBtn = document.getElementById("mc-next");
  const errorEl = document.getElementById("mc-error");

  let started = false;
  let timer = null;
  let state = null; // 最近一次的 MediaInfo（null = 无会话）
  let busy = false;
  let lastCover = ""; // 上次渲染的封面（同一 data URL 不重复重建 <img>）

  // 静态图标
  if (prevBtn) prevBtn.innerHTML = ICONS.prev;
  if (nextBtn) nextBtn.innerHTML = ICONS.next;
  if (toggleBtn) toggleBtn.innerHTML = ICONS.play;

  bindButtons();

  // ---------- 绑定 ----------

  function bindButtons() {
    if (prevBtn) prevBtn.addEventListener("click", () => doAction("prev"));
    if (nextBtn) nextBtn.addEventListener("click", () => doAction("next"));
    if (toggleBtn) toggleBtn.addEventListener("click", () => doAction("toggle"));
  }

  // ---------- 渲染 ----------

  function showError(msg) {
    if (!errorEl) return;
    errorEl.textContent = msg;
    errorEl.classList.remove("hidden");
  }

  function hideError() {
    if (errorEl) {
      errorEl.classList.add("hidden");
      errorEl.textContent = "";
    }
  }

  function render(s) {
    const info = s && typeof s === "object" ? s : null;
    const playing = !!(info && info.playing);

    if (!info) {
      // 无会话
      if (coverEl && lastCover !== "__empty__") {
        lastCover = "__empty__";
        coverEl.innerHTML = '<span class="mc-note">' + ICONS.note + "</span>";
        coverEl.classList.add("empty");
      }
      if (titleEl) titleEl.textContent = "无媒体播放";
      if (artistEl) artistEl.textContent = "";
      if (progressEl) progressEl.classList.add("hidden");
      if (controlsEl) controlsEl.classList.add("hidden");
      if (toggleBtn) toggleBtn.innerHTML = ICONS.play;
      return;
    }

    // 封面（同一 data URL 不重复重建 <img>，避免每 2s 闪烁）
    if (coverEl) {
      const thumb = info.thumbnail ? String(info.thumbnail) : "";
      if (thumb) {
        if (thumb !== lastCover) {
          lastCover = thumb;
          coverEl.classList.remove("empty");
          coverEl.innerHTML = '<img src="' + thumb.replace(/"/g, "&quot;") + '" alt="">';
        }
      } else if (lastCover !== "__empty__") {
        lastCover = "__empty__";
        coverEl.classList.add("empty");
        coverEl.innerHTML = '<span class="mc-note">' + ICONS.note + "</span>";
      }
    }
    if (titleEl) titleEl.textContent = info.title || "未知曲目";
    const sub = [info.artist, info.album].filter((x) => x && String(x).trim()).join(" · ");
    if (artistEl) artistEl.textContent = sub || info.sourceApp || "";

    // 进度：时长未知（0 / 缺失）时隐藏整块
    const dur = Number(info.durationMs) || 0;
    const pos = Math.min(Number(info.positionMs) || 0, dur > 0 ? dur : Number(info.positionMs) || 0);
    if (progressEl) {
      if (dur > 0) {
        progressEl.classList.remove("hidden");
        const pct = Math.max(0, Math.min(100, (pos / dur) * 100));
        if (fillEl) fillEl.style.width = pct.toFixed(2) + "%";
        if (posEl) posEl.textContent = fmtTime(pos);
        if (durEl) durEl.textContent = fmtTime(dur);
      } else {
        progressEl.classList.add("hidden");
      }
    }

    if (controlsEl) controlsEl.classList.remove("hidden");
    if (toggleBtn) toggleBtn.innerHTML = playing ? ICONS.pause : ICONS.play;
    if (toggleBtn) toggleBtn.title = playing ? "暂停" : "播放";
  }

  // ---------- 数据 ----------

  async function fetchState(first) {
    if (!started) return;
    try {
      const s = first
        ? await invokeRetry("get_media_state", undefined, { attempts: 2, perTryMs: 3000 })
        : await invoke("get_media_state");
      if (!started) return;
      hideError();
      state = s && typeof s === "object" ? s : null;
      render(state);
    } catch (e) {
      if (!started) return;
      // 首次就失败大概率是 SMTC 不可用：保留占位并给一行小字
      showError("无法读取媒体状态");
    }
  }

  async function doAction(action) {
    if (busy) return;
    busy = true;
    // 播放/暂停先乐观切换图标
    if (action === "toggle" && toggleBtn) {
      const playing = !!(state && state.playing);
      toggleBtn.innerHTML = playing ? ICONS.play : ICONS.pause;
    }
    try {
      const playing = await invoke("media_action", { action });
      // 后端返回动作后的播放状态（true=在播放）
      if (typeof playing === "boolean") {
        if (state) state.playing = playing;
        if (toggleBtn) toggleBtn.innerHTML = playing ? ICONS.pause : ICONS.play;
      }
    } catch (e) {
      showError("媒体控制失败: " + errText(e));
    } finally {
      busy = false;
    }
    // 立即刷新一次真实状态（稍等系统更新）
    setTimeout(() => fetchState(false), 150);
  }

  // ---------- 对外接口 ----------

  return {
    start() {
      if (started) return;
      started = true;
      fetchState(true);
      timer = setInterval(() => fetchState(false), POLL_MS);
    },
    stop() {
      started = false;
      if (timer) {
        clearInterval(timer);
        timer = null;
      }
    },
    refresh() {
      fetchState(false);
    },
  };
}

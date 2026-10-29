// 通知中心右栏媒体卡：Windows SMTC 当前媒体会话。
// 64px 封面 + 页内模糊封面底 + 标题/歌手 + 进度条 + 圆形控制键。
// 面板打开时 start()（立即拉一次 + 每 2s 轮询），关闭时 stop()。
// 样式前缀 med-（见 ui/css/panels.css）。
import { invoke, invokeRetry } from "./api.js";
import { icon } from "./icons.js";

/** 轮询间隔（毫秒） */
const POLL_MS = 2000;

const CARD_HTML = `
  <div class="med">
    <div class="med-bg" id="med-bg"></div>
    <div class="med-error hidden" id="med-error"></div>
    <div class="med-top">
      <div class="med-cover empty" id="med-cover">${icon("music", { size: 24 })}</div>
      <div class="med-meta">
        <div class="med-title" id="med-title">无媒体播放</div>
        <div class="med-artist" id="med-artist"></div>
      </div>
    </div>
    <div class="med-progress hidden" id="med-progress">
      <div class="med-bar"><div class="med-bar-fill" id="med-fill"></div></div>
      <div class="med-times"><span id="med-pos">0:00</span><span id="med-dur">0:00</span></div>
    </div>
    <div class="med-controls hidden" id="med-controls">
      <button class="med-btn" id="med-prev" type="button" title="上一首"></button>
      <button class="med-btn med-btn-main" id="med-toggle" type="button" title="播放/暂停"></button>
      <button class="med-btn" id="med-next" type="button" title="下一首"></button>
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

/** 毫秒 → m:ss（未知返回 "0:00"） */
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
  const bgEl = document.getElementById("med-bg");
  const coverEl = document.getElementById("med-cover");
  const titleEl = document.getElementById("med-title");
  const artistEl = document.getElementById("med-artist");
  const progressEl = document.getElementById("med-progress");
  const fillEl = document.getElementById("med-fill");
  const posEl = document.getElementById("med-pos");
  const durEl = document.getElementById("med-dur");
  const controlsEl = document.getElementById("med-controls");
  const prevBtn = document.getElementById("med-prev");
  const toggleBtn = document.getElementById("med-toggle");
  const nextBtn = document.getElementById("med-next");
  const errorEl = document.getElementById("med-error");

  let started = false;
  let timer = null;
  let state = null; // 最近一次的 MediaInfo（null = 无会话）
  let busy = false;
  let lastCover = ""; // 上次渲染的封面（同一 data URL 不重复重建 <img>）

  // 静态图标
  if (prevBtn) prevBtn.innerHTML = icon("skipBack", { size: 15 });
  if (nextBtn) nextBtn.innerHTML = icon("skipForward", { size: 15 });
  if (toggleBtn) toggleBtn.innerHTML = icon("play", { size: 16 });

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

  /** 模糊封面底：无封面时清空（空态保持纯色卡） */
  function setBackdrop(thumb) {
    if (!bgEl) return;
    if (thumb) {
      if (thumb === lastCover && bgEl.classList.contains("on")) return;
      bgEl.style.backgroundImage = 'url("' + thumb.replace(/["\\]/g, "") + '")';
      bgEl.classList.add("on");
    } else if (bgEl.classList.contains("on")) {
      bgEl.style.backgroundImage = "";
      bgEl.classList.remove("on");
    }
  }

  function setCover(thumb) {
    if (!coverEl) return;
    if (thumb) {
      if (thumb === lastCover) return;
      lastCover = thumb;
      coverEl.classList.remove("empty");
      coverEl.innerHTML = '<img src="' + thumb.replace(/"/g, "&quot;") + '" alt="">';
    } else if (lastCover !== "__empty__") {
      lastCover = "__empty__";
      coverEl.classList.add("empty");
      coverEl.innerHTML = icon("music", { size: 24 });
    }
  }

  function render(s) {
    const info = s && typeof s === "object" ? s : null;
    const playing = !!(info && info.playing);

    if (!info) {
      // 无会话
      setBackdrop("");
      setCover("");
      if (titleEl) titleEl.textContent = "无媒体播放";
      if (artistEl) artistEl.textContent = "";
      if (progressEl) progressEl.classList.add("hidden");
      if (controlsEl) controlsEl.classList.add("hidden");
      if (toggleBtn) toggleBtn.innerHTML = icon("play", { size: 16 });
      return;
    }

    // 封面 + 模糊底（同一 data URL 不重复重建 <img>，避免每 2s 闪烁）
    const thumb = info.thumbnail ? String(info.thumbnail) : "";
    setBackdrop(thumb);
    setCover(thumb);
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
    if (toggleBtn) {
      toggleBtn.innerHTML = icon(playing ? "pause" : "play", { size: 16 });
      toggleBtn.title = playing ? "暂停" : "播放";
    }
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
      toggleBtn.innerHTML = icon(playing ? "play" : "pause", { size: 16 });
    }
    try {
      const playing = await invoke("media_action", { action });
      // 后端返回动作后的播放状态（true=在播放）
      if (typeof playing === "boolean") {
        if (state) state.playing = playing;
        if (toggleBtn) toggleBtn.innerHTML = icon(playing ? "pause" : "play", { size: 16 });
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

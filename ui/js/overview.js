import "./theme.js";
import "./cursor-glow.js";
import "./glass.js";
import { beginSurfaceOpen } from "./surface-open.js";
import { invoke, listen, escapeHtml } from "./api.js";

const gridEl = document.getElementById("grid");
const emptyEl = document.getElementById("empty");

// 与 Dock 一致的字母占位配色
const LETTER_COLORS = ["#3584e4", "#e66100", "#2ec27e", "#9141ac", "#e01b24", "#1c71d8", "#8f4e2b"];

function hueGradient(seed) {
  let h = 0;
  for (const c of seed) h = (h * 31 + c.charCodeAt(0)) % 360;
  return `linear-gradient(135deg, hsl(${h},45%,30%), hsl(${(h + 60) % 360},45%,18%))`;
}

function render(items) {
  gridEl.innerHTML = "";
  emptyEl.classList.toggle("hidden", items.length > 0);

  items.forEach((item, i) => {
    const card = document.createElement("div");
    card.className = "card";
    // 打开动画的错峰延迟（与 motion.css §11 的 24ms / 上限 10 一致）
    card.style.animationDelay = `${Math.min(i, 10) * 24}ms`;

    const thumb = item.image
      ? `<img class="thumb" src="${item.image}" alt="">`
      : `<div class="thumb fallback" style="background:${hueGradient(item.exe || item.title)}">
           ${item.icon ? `<img src="${item.icon}" alt="">` : ""}
         </div>`;

    card.innerHTML = `
      ${thumb}
      <div class="info">
        ${item.icon ? `<img class="icon" src="${item.icon}" alt="">` : `<span class="icon"></span>`}
        <span class="title">${escapeHtml(item.title)}</span>
      </div>`;

    card.addEventListener("click", async () => {
      try {
        await invoke("activate_window", { id: item.id });
        await invoke("close_overview");
      } catch {}
    });
    gridEl.appendChild(card);
  });
}

listen("overview-data", (e) => {
  // 先落到关闭态（遮罩 opacity 0），玻璃就绪后由 surface-open.js 播入场
  beginSurfaceOpen();
  render(e.payload || []);
});

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") invoke("close_overview").catch(() => {});
});

document.getElementById("backdrop").addEventListener("mousedown", (e) => {
  if (e.target.id === "backdrop") invoke("close_overview").catch(() => {});
});

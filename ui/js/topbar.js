import { invoke, invokeRetry, listen } from "./api.js";

const dateEl = document.getElementById("date");
const clockEl = document.getElementById("clock");
const netEl = document.getElementById("net");
const batFill = document.getElementById("bat-fill");
const batPct = document.getElementById("bat-pct");

function tick() {
  const now = new Date();
  dateEl.textContent = now.toLocaleDateString("zh-CN", {
    month: "long",
    day: "numeric",
    weekday: "short",
  });
  clockEl.textContent = now.toLocaleTimeString("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
}
tick();
setInterval(tick, 1000);

document.getElementById("activities").addEventListener("click", () => {
  invoke("toggle_overview").catch(() => {});
});

const WIFI_ON = `<svg width="16" height="16" viewBox="0 0 16 16"><circle cx="8" cy="13" r="1.3" fill="#fff"/><path d="M5.2 10.2a4 4 0 0 1 5.6 0" stroke="#fff" stroke-width="1.4" fill="none" stroke-linecap="round"/><path d="M2.8 7.6a7.4 7.4 0 0 1 10.4 0" stroke="#fff" stroke-width="1.4" fill="none" stroke-linecap="round"/></svg>`;
const WIFI_OFF = `<svg width="16" height="16" viewBox="0 0 16 16"><path d="M5.2 10.2a4 4 0 0 1 5.6 0" stroke="rgba(255,255,255,.55)" stroke-width="1.4" fill="none" stroke-linecap="round"/><path d="M2.8 7.6a7.4 7.4 0 0 1 10.4 0" stroke="rgba(255,255,255,.55)" stroke-width="1.4" fill="none" stroke-linecap="round"/><path d="M2 2l12 12" stroke="#fff" stroke-width="1.3" stroke-linecap="round"/></svg>`;

function renderStatus(s) {
  netEl.innerHTML = s.network ? WIFI_ON : WIFI_OFF;
  if (s.battery != null) {
    batFill.style.width = Math.max(2, Math.min(16, s.battery / 100 * 16)) + "px";
    batFill.className = "bat-fill" + (s.charging ? " charging" : s.battery <= 15 ? " low" : "");
    batPct.textContent = s.battery + "%";
  } else {
    batPct.textContent = "";
    batFill.style.width = "0";
  }
}

async function refreshStatus() {
  try {
    renderStatus(await invokeRetry("get_status", undefined, { attempts: 3, perTryMs: 3000 }));
  } catch {}
}
refreshStatus();
setInterval(refreshStatus, 30000);

listen("fullscreen-changed", (e) => {
  document.body.classList.toggle("hidden", e.payload === true);
});

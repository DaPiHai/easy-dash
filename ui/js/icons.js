// ============================================================
// 图标模块：Lucide 图标子集（内联 SVG 字符串，无第三方运行时依赖）
//
// 用法：
//   import { icon, ICONS } from "./icons.js";
//   el.innerHTML = icon("wifi", { size: 18, stroke: 1.8 });
//   topbarEl.innerHTML = ICONS.wifi;            // 固定 16px，供顶栏聚合图标使用
//
// 约定：
//   1) 所有图标 remix 成 24×24 viewBox、stroke=currentColor，颜色随 CSS color 走；
//   2) 需要实心造型的图标（播放/暂停/上一首/下一首）在 path 内自带
//      fill="currentColor" stroke="none"，其余保持描边；
//   3) 函数返回字符串而非 DOM 节点，方便模板字符串拼接与 innerHTML 复用。
//
// 来源与许可：图标路径来自 Lucide（https://lucide.dev），版本 v0.546.0。
//
//   ISC License
//
//   Copyright (c) for portions of Lucide are held by Cole Bemis 2013-2022
//   as part of Feather (MIT). All other copyright (c) for Lucide are held by
//   Lucide Contributors 2022-2025.
//
//   Permission to use, copy, modify, and/or distribute this software for any
//   purpose with or without fee is hereby granted, provided that the above
//   copyright notice and this permission notice appear in all copies.
//
//   THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
//   WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
//   MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
//   ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
//   WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
//   ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
//   OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
// ============================================================

/** 图标名 → SVG 内部路径（24×24 viewBox） */
const PATHS = {
  // ---------- 网络 / 无线 ----------
  wifi:
    '<path d="M12 20h.01"/>' +
    '<path d="M2 8.82a15 15 0 0 1 20 0"/>' +
    '<path d="M5 12.859a10 10 0 0 1 14 0"/>' +
    '<path d="M8.5 16.429a5 5 0 0 1 7 0"/>',
  wifiOff:
    '<path d="M12 20h.01"/>' +
    '<path d="M8.5 16.429a5 5 0 0 1 7 0"/>' +
    '<path d="M5 12.859a10 10 0 0 1 5.17-2.69"/>' +
    '<path d="M19 12.859a10 10 0 0 0-2.007-1.523"/>' +
    '<path d="M2 8.82a15 15 0 0 1 4.177-2.643"/>' +
    '<path d="M22 8.82a15 15 0 0 0-11.288-3.764"/>' +
    '<path d="m2 2 20 20"/>',
  bluetooth: '<path d="m7 7 10 10-5 5V2l5 5L7 17"/>',
  airplane:
    '<path d="M17.8 19.2 16 11l3.5-3.5C21 6 21.5 4 21 3c-1-.5-3 0-4.5 1.5L13 8 4.8 6.2' +
    'c-.5-.1-1 .1-1.3.5l-.3.5c-.2.5-.1 1 .3 1.3L9 12l-2 3H4l-1 1 3 2 2 3 1-1v-3l3-2 3.5 5.3' +
    'c.3.4.8.5 1.3.3l.5-.2c.4-.3.6-.7.5-1.2z"/>',
  network:
    '<rect x="16" y="16" width="6" height="6" rx="1"/>' +
    '<rect x="2" y="16" width="6" height="6" rx="1"/>' +
    '<rect x="9" y="2" width="6" height="6" rx="1"/>' +
    '<path d="M5 16v-3a1 1 0 0 1 1-1h12a1 1 0 0 1 1 1v3"/>' +
    '<path d="M12 12V8"/>',
  lock:
    '<rect width="18" height="11" x="3" y="11" rx="2" ry="2"/>' +
    '<path d="M7 11V7a5 5 0 0 1 10 0v4"/>',
  lockKeyhole:
    '<circle cx="12" cy="16" r="1"/>' +
    '<rect x="3" y="10" width="18" height="12" rx="2"/>' +
    '<path d="M7 10V7a5 5 0 0 1 10 0v3"/>',

  // ---------- 外观 / 亮度 ----------
  moon: '<path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z"/>',
  sun:
    '<circle cx="12" cy="12" r="4"/>' +
    '<path d="M12 2v2"/><path d="M12 20v2"/>' +
    '<path d="m4.93 4.93 1.41 1.41"/><path d="m17.66 17.66 1.41 1.41"/>' +
    '<path d="M2 12h2"/><path d="M20 12h2"/>' +
    '<path d="m6.34 17.66-1.41 1.41"/><path d="m19.07 4.93-1.41 1.41"/>',
  // 界面材质：标准 / 液态玻璃 / 柔光玻璃
  layers:
    '<path d="m12.83 2.18a2 2 0 0 0-1.66 0L2.6 6.08a1 1 0 0 0 0 1.83l8.58 3.91a2 2 0 0 0 1.66 0l8.58-3.9a1 1 0 0 0 0-1.83Z"/>' +
    '<path d="m22 17.65-9.17 4.16a2 2 0 0 1-1.66 0L2 17.65"/>' +
    '<path d="m22 12.65-9.17 4.16a2 2 0 0 1-1.66 0L2 12.65"/>',
  droplet:
    '<path d="M12 22a7 7 0 0 0 7-7c0-2-1-3.9-3-5.5s-3.5-4-4-6.5c-.5 2.5-2 4.9-4 6.5C6 11.1 5 13 5 15a7 7 0 0 0 7 7z"/>',
  sparkles:
    '<path d="M9.94 15.5A2 2 0 0 0 8.5 14.06l-6.13-1.58a.5.5 0 0 1 0-.96L8.5 9.94A2 2 0 0 0 9.94 8.5l1.58-6.13a.5.5 0 0 1 .96 0L14.06 8.5A2 2 0 0 0 15.5 9.94l6.13 1.58a.5.5 0 0 1 0 .96L15.5 14.06A2 2 0 0 0 14.06 15.5l-1.58 6.13a.5.5 0 0 1-.96 0Z"/>' +
    '<path d="M20 3v4"/><path d="M22 5h-4"/><path d="M4 17v2"/><path d="M5 18H3"/>',
  monitor:
    '<rect width="20" height="14" x="2" y="3" rx="2"/>' +
    '<path d="M8 21h8"/><path d="M12 17v4"/>',

  // ---------- 声音 ----------
  volume:
    '<path d="M11 5 6 9H2v6h4l5 4V5Z"/>' +
    '<path d="M15.5 8.5a5 5 0 0 1 0 7"/>' +
    '<path d="M18.5 5.5a9 9 0 0 1 0 13"/>',
  volumeMuted:
    '<path d="M11 5 6 9H2v6h4l5 4V5Z"/>' +
    '<path d="m22 9-6 6"/><path d="m16 9 6 6"/>',

  // ---------- 电源 / 电池 ----------
  power: '<path d="M12 2v10"/><path d="M18.4 6.6a9 9 0 1 1-12.77.04"/>',
  battery: '<rect width="16" height="10" x="2" y="7" rx="2"/><path d="M22 11v2"/>',
  batteryCharging:
    '<path d="M15 7h1a2 2 0 0 1 2 2v6a2 2 0 0 1-2 2h-2"/>' +
    '<path d="M6 7H4a2 2 0 0 0-2 2v6a2 2 0 0 0 2 2h1"/>' +
    '<path d="m11 7-3 5h4l-3 5"/>' +
    '<path d="M22 11v2"/>',
  settings:
    '<path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38' +
    'a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38' +
    'a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2' +
    'v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73' +
    'l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73' +
    'l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/>' +
    '<circle cx="12" cy="12" r="3"/>',

  // ---------- 箭头 / 通用 ----------
  chevronLeft: '<path d="m15 18-6-6 6-6"/>',
  chevronRight: '<path d="m9 18 6-6-6-6"/>',
  chevronDown: '<path d="m6 9 6 6 6-6"/>',
  arrowLeft: '<path d="m12 19-7-7 7-7"/><path d="M19 12H5"/>',
  x: '<path d="M18 6 6 18"/><path d="m6 6 12 12"/>',
  check: '<path d="M20 6 9 17l-5-5"/>',
  externalLink:
    '<path d="M15 3h6v6"/><path d="M10 14 21 3"/>' +
    '<path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/>',
  trash:
    '<path d="M3 6h18"/>' +
    '<path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/>' +
    '<path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>' +
    '<path d="M10 11v6"/><path d="M14 11v6"/>',
  bell:
    '<path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9"/>' +
    '<path d="M10.3 21a1.94 1.94 0 0 0 3.4 0"/>',
  bellOff:
    '<path d="M8.7 3A6 6 0 0 1 18 8a21.3 21.3 0 0 0 .6 5"/>' +
    '<path d="M17 17H3s3-2 3-9a4.67 4.67 0 0 1 .3-1.7"/>' +
    '<path d="M10.3 21a1.94 1.94 0 0 0 3.4 0"/>' +
    '<path d="m2 2 20 20"/>',
  calendar:
    '<path d="M8 2v4"/><path d="M16 2v4"/>' +
    '<rect width="18" height="18" x="3" y="4" rx="2"/>' +
    '<path d="M3 10h18"/>',

  // ---------- 媒体 ----------
  music:
    '<path d="M9 18V5l12-2v13"/>' +
    '<circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>',
  play: '<path d="m7 4 13 8-13 8V4Z" fill="currentColor" stroke="none"/>',
  pause:
    '<rect x="6" y="4" width="4" height="16" rx="1" fill="currentColor" stroke="none"/>' +
    '<rect x="14" y="4" width="4" height="16" rx="1" fill="currentColor" stroke="none"/>',
  skipBack: '<path d="M19 20 9 12l10-8v16Z" fill="currentColor" stroke="none"/><path d="M5 19V5"/>',
  skipForward: '<path d="m5 4 10 8-10 8V4Z" fill="currentColor" stroke="none"/><path d="M19 5v14"/>',
};

/** 合法图标名（调试时用来发现拼写错误） */
export const ICON_NAMES = Object.keys(PATHS);

/**
 * 生成内联 SVG 图标字符串。
 * @param {keyof typeof PATHS|string} name 图标名
 * @param {{size?: number, stroke?: number}} [opts] size：像素边长（默认 18）；stroke：描边宽度（默认 1.8）
 * @returns {string} SVG 字符串；未知图标名返回空串（不抛错，避免模板整块渲染失败）
 */
export function icon(name, { size = 18, stroke = 1.8 } = {}) {
  const inner = PATHS[name];
  if (!inner) {
    if (typeof console !== "undefined") console.warn("[图标] 未知图标:", name);
    return "";
  }
  const px = Math.max(8, Math.round(Number(size) || 18));
  const sw = Number.isFinite(Number(stroke)) && Number(stroke) > 0 ? Number(stroke) : 1.8;
  return (
    '<svg width="' +
    px +
    '" height="' +
    px +
    '" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="' +
    sw +
    '" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">' +
    inner +
    "</svg>"
  );
}

/**
 * 固定尺寸（16px）图标集合：顶栏聚合图标、控制中心磁贴用。
 * key 保持旧命名（wifi / wifiOff / volumeMuted …），topbar.js 直接 innerHTML 使用。
 */
export const ICONS = {
  wifi: icon("wifi", { size: 16 }),
  wifiOff: icon("wifiOff", { size: 16 }),
  bluetooth: icon("bluetooth", { size: 16 }),
  airplane: icon("airplane", { size: 16 }),
  moon: icon("moon", { size: 16 }),
  sun: icon("sun", { size: 16 }),
  monitor: icon("monitor", { size: 16 }),
  volume: icon("volume", { size: 16 }),
  volumeMuted: icon("volumeMuted", { size: 16 }),
  power: icon("power", { size: 16 }),
  settings: icon("settings", { size: 16 }),
  lock: icon("lock", { size: 16 }),
  battery: icon("battery", { size: 16 }),
  bell: icon("bell", { size: 16 }),
  music: icon("music", { size: 16 }),
};

// ============================================================
// 分段控件滑动指示器（iOS 分段控件：选中块滑动过去，而不是跳变）
//
// 用法：
//   import { attachSegIndicators, attachSegIndicator } from "./seg-thumb.js";
//   attachSegIndicators(panelEl);            // 自动接管 .ctl-seg / .mp-seg
//   attachSegIndicator(el).sync();           // 手动同步（布局变化后兜底）
//
// 实现说明：
// 1) 往分段容器里插一个绝对定位的 .seg-thumb，把选中按钮的几何量写成
//    --seg-x / --seg-y / --seg-w / --seg-h，位移交给 motion.css 的弹性过渡；
// 2) 选中态由各页面用 button.active 表达，这里用 MutationObserver 监听 class/aria-pressed
//    变化自动跟随，不需要各页面额外调用；
// 3) 面板隐藏（display:none）时几何为 0 → 指示器透明；重新显示后 ResizeObserver 触发重算；
// 4) 只观察按钮，不观察自己写入的 style，避免自触发循环。
// ============================================================

/** 已接管的容器（重复调用返回同一个 api，不会插第二个指示器） */
const attached = new WeakMap();

/**
 * 给一个分段容器挂上滑动指示器。
 * @param {Element|null} seg 容器（.ctl-seg / .mp-seg）
 * @returns {{sync: () => void, thumb: HTMLElement}|null}
 */
export function attachSegIndicator(seg) {
  if (!seg || typeof seg.appendChild !== "function") return null;
  if (!seg.querySelector("button")) return null; // 还没渲染出按钮
  if (attached.has(seg)) return attached.get(seg);

  const thumb = document.createElement("span");
  thumb.className = "seg-thumb";
  thumb.setAttribute("aria-hidden", "true");
  seg.appendChild(thumb);
  seg.classList.add("has-thumb");

  let raf = 0;
  let lastKey = "";

  function apply() {
    raf = 0;
    const segRect = seg.getBoundingClientRect();
    const buttons = seg.querySelectorAll("button");
    let active = null;
    for (const btn of buttons) {
      if (btn.classList.contains("active")) {
        active = btn;
        break;
      }
    }
    if (!active || !segRect.width || !segRect.height) {
      thumb.style.opacity = "0";
      lastKey = "";
      return;
    }
    const r = active.getBoundingClientRect();
    if (!r.width || !r.height) {
      thumb.style.opacity = "0";
      lastKey = "";
      return;
    }
    // absolute 定位原点是 padding box，getBoundingClientRect 是 border box ⇒ 减掉边框
    const x = r.left - segRect.left - seg.clientLeft;
    const y = r.top - segRect.top - seg.clientTop;
    const key = x.toFixed(1) + "," + y.toFixed(1) + "," + r.width.toFixed(1) + "," + r.height.toFixed(1);
    if (key !== lastKey) {
      lastKey = key;
      thumb.style.setProperty("--seg-x", x.toFixed(1) + "px");
      thumb.style.setProperty("--seg-y", y.toFixed(1) + "px");
      thumb.style.setProperty("--seg-w", r.width.toFixed(1) + "px");
      thumb.style.setProperty("--seg-h", r.height.toFixed(1) + "px");
    }
    thumb.style.opacity = "1";
  }

  function schedule() {
    if (raf) return;
    raf = requestAnimationFrame(apply);
  }

  const mo = new MutationObserver((records) => {
    for (const rec of records) {
      if (rec.target === thumb) continue; // 自己的 style/class 变化不触发
      schedule();
      return;
    }
  });
  mo.observe(seg, { subtree: true, attributes: true, attributeFilter: ["class", "aria-pressed"] });

  const ro = new ResizeObserver(schedule);
  ro.observe(seg);
  for (const btn of seg.querySelectorAll("button")) ro.observe(btn);

  window.addEventListener("resize", schedule);
  schedule();
  // 字体/图标异步布局后兜底两次（首帧图标注入会改变按钮宽度）
  setTimeout(schedule, 120);
  setTimeout(schedule, 420);

  const api = { sync: schedule, thumb };
  attached.set(seg, api);
  return api;
}

/**
 * 批量接管某个根节点下的所有分段容器。
 * @param {Document|Element} [root]
 */
export function attachSegIndicators(root) {
  const scope = root || document;
  if (!scope || typeof scope.querySelectorAll !== "function") return [];
  const out = [];
  for (const seg of scope.querySelectorAll(".ctl-seg, .mp-seg")) {
    const api = attachSegIndicator(seg);
    if (api) out.push(api);
  }
  return out;
}

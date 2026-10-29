// 鼠标滚轮步进器：把不同 deltaMode 与触控板的连续滚动归一化成「档」。
//
// 用法：
//   const stepper = createWheelStepper();
//   el.addEventListener("wheel", (e) => {
//     const steps = stepper(e);
//     if (!steps) return;
//     e.preventDefault();
//     adjust(steps);            // 正数 = 向上滚动（增大），负数 = 向下
//   }, { passive: false });
//
// 约定：
// 1) 返回值 0 表示本次滚动还没有累计到一档，调用方不要 preventDefault；
// 2) 一次事件最多 ±maxSteps 档，避免「页面模式」滚轮一次跳到底；
// 3) ctrlKey（触控板捏合缩放）不参与调节。

/**
 * @param {{stepPx?: number, maxSteps?: number}} [opts]
 *   stepPx：每档需要的滚动像素（默认 100，约等于 Windows 鼠标滚轮一格）
 *   maxSteps：单次事件最多档数（默认 3）
 * @returns {(e: WheelEvent) => number} 正数=向上（增大），负数=向下
 */
export function createWheelStepper({ stepPx = 100, maxSteps = 3 } = {}) {
  const step = Math.max(1, Number(stepPx) || 100);
  const cap = Math.max(1, Math.round(Number(maxSteps) || 3));
  let acc = 0;

  return function stepper(e) {
    if (!e || e.ctrlKey) return 0;
    let dy = Number(e.deltaY) || 0;
    // deltaMode：0=像素，1=行（约 16px），2=页（约 100px）
    if (e.deltaMode === 1) dy *= 16;
    else if (e.deltaMode === 2) dy *= 100;
    if (!dy) return 0;

    acc += dy;
    // deltaY < 0 = 向上滚动 = 正档数
    let steps = Math.trunc(-acc / step);
    if (steps > cap) steps = cap;
    else if (steps < -cap) steps = -cap;
    acc += steps * step;
    // 异常设备/页面模式可能留下超大余额：直接丢弃，避免下一次暴走
    if (Math.abs(acc) > step * 2) acc = 0;
    return steps;
  };
}

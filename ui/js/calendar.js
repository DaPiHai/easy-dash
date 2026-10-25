// 中文月历渲染（纯原生 Date 计算，不引第三方依赖）。
// 周一为第一列，表头「一 二 三 四 五 六 日」，今天高亮，非当月日期淡化显示。

const WEEKDAY_CN = ["一", "二", "三", "四", "五", "六", "日"];

/** 当前年月日（月从 0 开始，与 Date.getMonth() 一致） */
export function todayInfo() {
  const d = new Date();
  return { year: d.getFullYear(), month: d.getMonth(), day: d.getDate() };
}

/** 月份视图平移：delta 为 -1（上个月）/ +1（下个月） */
export function shiftView(view, delta) {
  const base = (view && typeof view.month === "number" ? view.month : 0) + (delta || 0);
  const month = ((base % 12) + 12) % 12;
  const yearDelta = Math.floor(base / 12);
  return {
    year: (view && view.year ? view.year : new Date().getFullYear()) + yearDelta,
    month,
  };
}

/** 「2026年10月」 */
export function formatMonthTitle(view) {
  return view.year + "年" + (view.month + 1) + "月";
}

/**
 * 渲染月历到容器。
 * @param {HTMLElement} container 容器（会被 innerHTML 覆盖）
 * @param {{year:number, month:number}} view 要显示的年月
 */
export function renderCalendar(container, view) {
  if (!container || !view) return;
  const today = todayInfo();

  // 当月 1 号是周几（0=周日）；转换到以周一为第一列的偏移
  const firstWeekday = new Date(view.year, view.month, 1).getDay();
  const offset = (firstWeekday + 6) % 7;
  // 当月天数：下个月第 0 天 = 当月最后一天
  const daysInMonth = new Date(view.year, view.month + 1, 0).getDate();
  // 网格总格数取 7 的整数倍（35 或 42）
  const cellCount = Math.ceil((offset + daysInMonth) / 7) * 7;

  let cells = "";
  for (let i = 0; i < cellCount; i++) {
    // 用 Date 溢出规则直接算出前后月的日期
    const d = new Date(view.year, view.month, 1 - offset + i);
    const isOther = d.getMonth() !== view.month;
    const isToday =
      d.getFullYear() === today.year && d.getMonth() === today.month && d.getDate() === today.day;
    const cls = ["cal-day", isOther ? "other" : "", isToday ? "today" : ""].filter(Boolean).join(" ");
    cells += '<div class="' + cls + '">' + d.getDate() + "</div>";
  }

  let weekdays = "";
  for (const w of WEEKDAY_CN) weekdays += '<div class="cal-wd">' + w + "</div>";

  container.innerHTML =
    '<div class="cal-head">' +
    '<button class="cal-nav" type="button" data-cal-dir="-1" aria-label="上个月">‹</button>' +
    '<span class="cal-title">' + formatMonthTitle(view) + "</span>" +
    '<button class="cal-nav" type="button" data-cal-dir="1" aria-label="下个月">›</button>' +
    "</div>" +
    '<div class="cal-grid">' + weekdays + cells + "</div>";
}

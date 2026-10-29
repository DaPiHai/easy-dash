// 中文月历渲染（纯原生 Date 计算 + lunar.js 农历换算，不引第三方依赖）。
// 周一为第一列，表头「一 二 三 四 五 六 日」，今天 accent 胶囊高亮，非当月日期淡化显示。
// 第二行：法定节假日优先（休息日红字节日短名 / 调休琥珀「班」角标），
// 无节日时显示农历（初一显示月名，其余显示日名）。
// 叠加 cn-holidays.js 的法定节假日数据；农历数据来自 ui/js/lunar.js。
// 样式前缀 cal-（见 ui/css/panels.css）。
import { escapeHtml } from "./api.js";
import { icon } from "./icons.js";
import { holidayFor } from "./cn-holidays.js";
import { solarToLunar, formatLunarShort, formatLunarFull, ganzhiYear } from "./lunar.js";

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

/** 节日名过长时截断（单元格约 45px 宽，9px 字号最多放 4 个汉字） */
function shortName(name) {
  const s = String(name || "").trim();
  return s.length > 4 ? s.slice(0, 4) : s;
}

/**
 * 当前显示月份所属的农历年 → 「丙午马年」。
 * 取当月 15 号换算（1 月初可能还属上一个农历年，月中一定落在该农历年的月份内）；
 * 取不到时回退到当月 1 号；再失败返回空串（超出 lunar.js 范围）。
 */
function ganzhiOfMonth(view) {
  const mid = solarToLunar(view.year, view.month + 1, 15);
  const lunar = mid || solarToLunar(view.year, view.month + 1, 1);
  if (!lunar || typeof lunar.lunarYear !== "number") return "";
  return ganzhiYear(lunar.lunarYear);
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
    const y = d.getFullYear();
    const m = d.getMonth();
    const day = d.getDate();
    const isOther = m !== view.month;
    const isToday = y === today.year && m === today.month && day === today.day;
    // 节假日：off=true 休息日；off=false 调休上班。
    // 只标记「当月」的日期——前后月溢出的淡化日期不标，避免看起来像本月节日。
    const holiday = isOther ? null : holidayFor(y, m, day);
    const isOff = !!(holiday && holiday.off === true);
    const isWork = !!(holiday && holiday.off === false);
    // 农历：前后月也换算（淡化显示），超出 lunar.js 支持范围时为 null
    const lunar = solarToLunar(y, m + 1, day);

    const cls = [
      "cal-day",
      isOther ? "other" : "",
      isToday ? "today" : "",
      isOff ? "is-off" : "",
      isWork ? "is-work" : "",
    ]
      .filter(Boolean)
      .join(" ");

    let inner = '<span class="cal-day-num">' + day + "</span>";
    // 第二行：节日优先（休息日红字节日短名 / 调休「班」角标），否则农历
    if (isOff) {
      inner += '<span class="cal-holiday">' + escapeHtml(shortName(holiday.name)) + "</span>";
    } else if (isWork) {
      inner += '<span class="cal-work">班</span>';
    } else if (lunar) {
      inner += '<span class="cal-lunar">' + escapeHtml(formatLunarShort(lunar)) + "</span>";
    }

    // title：公历 + 完整农历 + 节日全名
    let titleText = y + "年" + (m + 1) + "月" + day + "日";
    if (lunar) titleText += " · " + formatLunarFull(lunar);
    if (holiday) titleText += " · " + holiday.name;
    const titleAttr = ' title="' + escapeHtml(titleText) + '"';

    cells += '<div class="' + cls + '"' + titleAttr + ">" + inner + "</div>";
  }

  let weekdays = "";
  for (const w of WEEKDAY_CN) weekdays += '<div class="cal-wd">' + w + "</div>";

  const ganzhi = ganzhiOfMonth(view);
  const titleHtml =
    '<span class="cal-title">' +
    formatMonthTitle(view) +
    (ganzhi ? '<span class="cal-ganzhi"> · ' + escapeHtml(ganzhi) + "</span>" : "") +
    "</span>";

  container.innerHTML =
    '<div class="cal-head">' +
    '<button class="cal-nav" type="button" data-cal-dir="-1" aria-label="上个月">' +
    icon("chevronLeft", { size: 14 }) +
    "</button>" +
    titleHtml +
    '<button class="cal-nav" type="button" data-cal-dir="1" aria-label="下个月">' +
    icon("chevronRight", { size: 14 }) +
    "</button>" +
    "</div>" +
    '<div class="cal-grid">' + weekdays + cells + "</div>";
}

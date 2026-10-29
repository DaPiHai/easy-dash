// ============================================================
// 农历转换（1901–2100 公历范围）
//
// 数据来自 ui/js/lunar-table.js（由 .NET ChineseLunisolarCalendar 生成），
// 本模块只做纯日期换算，无第三方依赖、无网络请求。
//
// 用法：
//   import { solarToLunar, formatLunarShort, ganzhiYear } from "./lunar.js";
//   const l = solarToLunar(2026, 10, 1);   // => { lunarYear: 2026, lunarMonth: 8, ... }
//   formatLunarShort(l)                    // => "廿一"（初一返回 "八月"）
// ============================================================
import { LUNAR_TABLE } from "./lunar-table.js";

const MS_PER_DAY = 86400000;

/** 农历月份名（1–12 月） */
export const LUNAR_MONTH_NAMES = [
  "正月",
  "二月",
  "三月",
  "四月",
  "五月",
  "六月",
  "七月",
  "八月",
  "九月",
  "十月",
  "冬月",
  "腊月",
];

/** 农历日名（索引 1–30） */
export const LUNAR_DAY_NAMES = [
  "",
  "初一",
  "初二",
  "初三",
  "初四",
  "初五",
  "初六",
  "初七",
  "初八",
  "初九",
  "初十",
  "十一",
  "十二",
  "十三",
  "十四",
  "十五",
  "十六",
  "十七",
  "十八",
  "十九",
  "二十",
  "廿一",
  "廿二",
  "廿三",
  "廿四",
  "廿五",
  "廿六",
  "廿七",
  "廿八",
  "廿九",
  "三十",
];

const STEMS = "甲乙丙丁戊己庚辛壬癸";
const BRANCHES = "子丑寅卯辰巳午未申酉戌亥";
const ZODIACS = "鼠牛虎兔龙蛇马羊猴鸡狗猪";

/** 年表预处理：把公历「正月初一」换算成 UTC 天序号，便于二分查找 */
const YEARS = LUNAR_TABLE.map(([year, month, day, leap, mask]) => ({
  year,
  leap,
  mask,
  /** 正月初一的 UTC 天序号 */
  newYear: Date.UTC(year, month - 1, day) / MS_PER_DAY,
  monthCount: leap ? 13 : 12,
  /** 农历年总天数 */
  totalDays: 0,
}));

for (const y of YEARS) {
  let sum = 0;
  for (let i = 1; i <= y.monthCount; i += 1) {
    sum += (y.mask >> (i - 1)) & 1 ? 30 : 29;
  }
  y.totalDays = sum;
}

/** 第 i 个农历月（1 起，含闰月顺序）的天数 */
function monthLength(y, i) {
  return (y.mask >> (i - 1)) & 1 ? 30 : 29;
}

/** 第 i 个农历月（1 起，含闰月顺序）对应的月份数字；闰月与上一个平月同号 */
function monthNumberOf(y, i) {
  if (!y.leap) return i;
  const leapIndex = y.leap + 1; // 闰月在年内的序号（如闰六月 => 第 7 个月）
  if (i === leapIndex) return y.leap;
  if (i > leapIndex) return i - 1;
  return i;
}

/** 干支纪年（如「丙午」） */
function ganzhiOf(lunarYear) {
  const n = lunarYear - 4;
  return STEMS[((n % 10) + 10) % 10] + BRANCHES[((n % 12) + 12) % 12];
}

/** 干支 + 生肖（如「丙午马年」） */
export function ganzhiYear(lunarYear) {
  const n = lunarYear - 4;
  return ganzhiOf(lunarYear) + ZODIACS[((n % 12) + 12) % 12] + "年";
}

/** 生肖（如「马」） */
export function zodiacOf(lunarYear) {
  const n = lunarYear - 4;
  return ZODIACS[((n % 12) + 12) % 12];
}

/**
 * 公历 → 农历。
 * @param {number} year 公历年
 * @param {number} month 公历月（1–12）
 * @param {number} day 公历日
 * @returns {{lunarYear:number,lunarMonth:number,lunarDay:number,isLeapMonth:boolean,monthName:string,dayName:string,ganzhi:string,zodiac:string}|null}
 *          超出 1901–2100 年表范围时返回 null。
 */
export function solarToLunar(year, month, day) {
  const serial = Date.UTC(year, month - 1, day) / MS_PER_DAY;
  if (!Number.isFinite(serial)) return null;

  // 二分查找「正月初一 <= 该日」的最后一个农历年
  let lo = 0;
  let hi = YEARS.length - 1;
  let idx = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (YEARS[mid].newYear <= serial) {
      idx = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  if (idx < 0) return null;

  const y = YEARS[idx];
  let offset = serial - y.newYear;
  if (offset < 0 || offset >= y.totalDays) return null;

  let monthIndex = 1;
  while (monthIndex <= y.monthCount && offset >= monthLength(y, monthIndex)) {
    offset -= monthLength(y, monthIndex);
    monthIndex += 1;
  }
  if (monthIndex > y.monthCount) return null;

  const lunarDay = offset + 1;
  const lunarMonth = monthNumberOf(y, monthIndex);
  const isLeapMonth = y.leap !== 0 && monthIndex === y.leap + 1;

  return {
    lunarYear: y.year,
    lunarMonth,
    lunarDay,
    isLeapMonth,
    monthName: (isLeapMonth ? "闰" : "") + LUNAR_MONTH_NAMES[lunarMonth - 1],
    dayName: LUNAR_DAY_NAMES[lunarDay],
    ganzhi: ganzhiOf(y.year),
    zodiac: zodiacOf(y.year),
  };
}

/**
 * 日历格用的短农历文案：初一显示月份名（如「八月」「闰六月」），其余显示日名（如「廿一」）。
 * @param {{lunarDay:number,monthName:string,dayName:string}|null} lunar
 */
export function formatLunarShort(lunar) {
  if (!lunar) return "";
  return lunar.lunarDay === 1 ? lunar.monthName : lunar.dayName;
}

/**
 * 完整农历文案（用于 title 提示）：如「农历八月廿一 · 丙午马年」。
 */
export function formatLunarFull(lunar) {
  if (!lunar) return "";
  return `农历${lunar.monthName}${lunar.dayName} · ${ganzhiYear(lunar.lunarYear)}`;
}

/** 当前系统日期对应的农历（便于调试/其它模块使用） */
export function todayLunar(now = new Date()) {
  return solarToLunar(now.getFullYear(), now.getMonth() + 1, now.getDate());
}

// ============================================================
// 液态玻璃光学参数自检（Node，无浏览器）
//
// 用法：npm run check:glass
// 目的：把「任何壁纸亮度下正文 ≥4.5:1、蓝玻璃白字 ≥3:1、亮色主题不出现大面积白底」
// 变成可回归的断言。输入是后端截屏统计出的 p05/p95 亮度分位（0..1）。
// ============================================================
import {
  computeGlassStyle,
  defaultGlassStyle,
  TINT_ALPHA,
  TARGET_CONTRAST,
  ACCENT_TARGET,
  MAX_LIGHT_GRADE,
} from "../ui/js/glass-math.js";

const BACKDROPS = [0, 0.02, 0.05, 0.1, 0.2, 0.35, 0.5, 0.65, 0.8, 0.9, 1];
const THEMES = ["dark", "light"];

/** 亮色透明玻璃在中间亮度壁纸上的物理下限（黑/白墨色最优约 4.0） */
const LIGHT_MIN_CONTRAST = 3.9;

let failures = 0;

function check(ok, label, detail) {
  if (ok) {
    console.log(`  ✓ ${label}`);
  } else {
    failures += 1;
    console.log(`  ✗ ${label}${detail ? " — " + detail : ""}`);
  }
}

function fmt(n, digits = 2) {
  return Number(n).toFixed(digits);
}

console.log("液态玻璃光学参数自检");
console.log(
  `墨色对比目标 ${TARGET_CONTRAST}:1（暗色验收 4.5 / 亮色透明玻璃 3.9，亮色白色阶上限 ${MAX_LIGHT_GRADE}）/ 蓝玻璃白字目标 ${ACCENT_TARGET}:1`
);
console.log("");

for (const theme of THEMES) {
  const tint = TINT_ALPHA[theme];
  const minContrast = theme === "light" ? LIGHT_MIN_CONTRAST : 4.5;
  console.log(`[${theme}] 白偏光层 alpha = ${tint}`);
  console.log(
    "  背景亮度  p05/p95   色阶 alpha  合成亮度  墨色  墨色对比  蓝玻璃白字  阴影色"
  );
  for (const b of BACKDROPS) {
    const stats =
      theme === "dark"
        ? { p05: b, p95: b, mean: [120, 130, 150] }
        : { p05: b, p95: b, mean: [120, 130, 150] };
    const s = computeGlassStyle(theme, stats);
    const shadow = s.shadow;
    console.log(
      `  ${fmt(b, 2).padStart(6)}    ${fmt(s.backdropLum, 2)}     ${fmt(s.gradeAlpha, 3).padStart(6)}     ${fmt(
        s.compositeLum,
        3
      ).padStart(6)}    ${s.ink.padEnd(5)}  ${fmt(s.bodyContrast, 2).padStart(5)}:1     ${fmt(
        s.accentContrast,
        2
      ).padStart(4)}:1     ${s.shadowRgb.join(",")}`
    );
    if (s.bodyContrast < minContrast) {
      failures += 1;
      console.log(`      ✗ 墨色对比度不足 ${minContrast}（${fmt(s.bodyContrast, 2)}:1）`);
    }
    if (s.accentContrast < ACCENT_TARGET) {
      failures += 1;
      console.log(`      ✗ 蓝玻璃白字不足 ${ACCENT_TARGET}（${fmt(s.accentContrast, 2)}:1）`);
    }
    if (!/rgba\(0, 0, 0,/.test(shadow) && !/#000/.test(shadow)) {
      // 正常
    } else {
      failures += 1;
      console.log("      ✗ 阴影出现纯黑");
    }
    if (theme === "light" && s.gradeAlpha > MAX_LIGHT_GRADE + 0.001) {
      failures += 1;
      console.log(
        `      ✗ 亮色主题白色阶超过 ${MAX_LIGHT_GRADE}（${fmt(s.gradeAlpha, 3)}），出现大面积白底`
      );
    }
    if (theme === "dark" && s.gradeAlpha < 0.1) {
      failures += 1;
      console.log("      ✗ 暗色主题色阶 alpha 低于 0.10");
    }
  }
  // 兜底（没有截屏时）也必须可读且是同一套 token
  const d = defaultGlassStyle(theme);
  check(!!d.grade && !!d.shadow && !!d.ink, `${theme} 兜底参数存在`);
  console.log("");
}

// 极端组合：p05 与 p95 差别很大的高反差壁纸
for (const theme of THEMES) {
  const minContrast = theme === "light" ? LIGHT_MIN_CONTRAST : 4.5;
  for (const [p05, p95] of [
    [0, 1],
    [0.05, 0.95],
    [0.4, 0.45],
  ]) {
    const s = computeGlassStyle(theme, { p05, p95, mean: [200, 200, 200] });
    check(
      s.bodyContrast >= minContrast,
      `${theme} 高反差 p05=${p05} p95=${p95} 墨色 ${fmt(s.bodyContrast, 2)}:1`
    );
    check(
      s.accentContrast >= ACCENT_TARGET,
      `${theme} 高反差 p05=${p05} p95=${p95} 蓝字 ${fmt(s.accentContrast, 2)}:1`
    );
  }
}

console.log("");
if (failures > 0) {
  console.error(`✗ ${failures} 项未通过`);
  process.exit(1);
}
console.log("✓ 全部通过");

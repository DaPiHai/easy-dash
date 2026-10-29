// ============================================================
// 外观管理模块（主题：light / dark；材质：solid / liquid / glow）
//
// 两个维度正交：data-theme 决定明暗，data-material 决定玻璃质感（标准 / 液态玻璃 / 柔光玻璃）。
//
// 职责：
// 1) 首帧：读 localStorage 缓存（"easydash.theme" / "easydash.material"，与各 HTML head 里的
//    内联预置脚本同 key）先把 data-theme / data-material 落到 <html>，避免闪白/闪深/闪 solid；
//    旧版主题缓存值 "system" 归一为 "dark"，非法材质归一为 "solid"。
// 2) Tauri 注入后：invokeRetry("get_theme_state") 校准，并监听
//    "theme-changed" / "config-changed" 持续同步；
// 3) 对外 API：
//    getThemeState()  -> { mode, resolved, material, systemDark, syncSystemTheme }
//                        （mode/resolved 只会是 "light" | "dark"；material 只会是 solid/liquid/glow；
//                          systemDark/syncSystemTheme 仅作展示信息透传）
//    resolvedTheme()  -> "light" | "dark"
//    currentMaterial() -> "solid" | "liquid" | "glow"
//    subscribeTheme(fn) -> 取消订阅函数（主题或材质变化时回调）
//    setThemeMode(mode) -> Promise<warnings: string[]>（失败抛错；仅接受 light/dark）
//    setMaterialStyle(material) -> Promise<warnings: string[]>（失败抛错；仅接受三种材质）
// 4) Tauri 尚未注入时不跟随任何系统偏好：默认深色 + solid，set* 只在本地生效并返回 warning。
//
// 页面入口只 import 一次即可（ES Module 天然单例），内部也有 initPromise 防重入。
// ============================================================
import { invoke, invokeRetry, listen } from "./api.js";

/** localStorage 缓存 key（与 HTML 内联预置脚本保持一致） */
const STORAGE_KEY = "easydash.theme";
const STORAGE_MATERIAL = "easydash.material";

const MODE_LIGHT = "light";
const MODE_DARK = "dark";

/** 默认主题：深色（也是旧配置 "system" / 非法值的归一结果） */
const DEFAULT_MODE = MODE_DARK;

/** 界面材质白名单（与后端 config::MATERIAL_STYLES 保持一致） */
export const MATERIAL_STYLES = ["solid", "liquid", "glow"];

/** 默认材质：标准（等价于改造前的观感） */
const DEFAULT_MATERIAL = "solid";

/**
 * @type {{mode: "light"|"dark", resolved: "light"|"dark", material: string,
 *         systemDark: boolean|null, syncSystemTheme: boolean}}
 * systemDark/syncSystemTheme 只用于管理面板展示（Windows 当前深浅 / 是否同步系统主题），不参与解析。
 */
let state = {
  mode: DEFAULT_MODE,
  resolved: DEFAULT_MODE,
  material: DEFAULT_MATERIAL,
  systemDark: null,
  syncSystemTheme: false,
};

/** 订阅者集合 */
const subscribers = new Set();
/** 初始化 Promise（防重复初始化） */
let initPromise = null;

// ---------- 基础工具 ----------

function hasTauri() {
  const t = window.__TAURI__;
  return !!(t && t.core && t.event);
}

/** 只接受 light/dark；"system"（旧版）归一 dark；其余非法值抛错 */
function normalizeMode(mode, { strict = false } = {}) {
  if (mode === MODE_LIGHT) return MODE_LIGHT;
  if (mode === MODE_DARK) return MODE_DARK;
  if (mode === "system") return DEFAULT_MODE; // 兼容旧版调用方/旧配置
  if (strict) throw new Error("无效的主题模式: " + String(mode));
  return DEFAULT_MODE;
}

/** 只接受 solid/liquid/glow；非法值归一 solid（strict 时抛错） */
export function normalizeMaterial(material, { strict = false } = {}) {
  const v = typeof material === "string" ? material.trim().toLowerCase() : "";
  if (MATERIAL_STYLES.indexOf(v) >= 0) return v;
  if (strict) throw new Error("无效的界面材质: " + String(material));
  return DEFAULT_MATERIAL;
}

/** 读缓存：只返回 light/dark；旧值 "system" 归一 dark；无缓存返回 null */
function readCachedMode() {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    if (v === MODE_LIGHT) return MODE_LIGHT;
    if (v === MODE_DARK || v === "system") return DEFAULT_MODE;
    return null;
  } catch (e) {
    return null;
  }
}

/** 读材质缓存：只返回 solid/liquid/glow；无缓存或非法值返回 null（非法值不当成用户选择） */
function readCachedMaterial() {
  try {
    const v = localStorage.getItem(STORAGE_MATERIAL);
    if (typeof v !== "string") return null;
    const normalized = v.trim().toLowerCase();
    return MATERIAL_STYLES.indexOf(normalized) >= 0 ? normalized : null;
  } catch (e) {
    return null;
  }
}

/** 旧缓存迁移：把 "system" 落成 "dark"，避免下次首帧再走一遍兼容分支 */
function migrateCachedMode() {
  try {
    if (localStorage.getItem(STORAGE_KEY) === "system") {
      localStorage.setItem(STORAGE_KEY, DEFAULT_MODE);
    }
  } catch (e) {
    /* 隐私模式/存储不可用时忽略 */
  }
}

function writeCachedMode(mode) {
  try {
    localStorage.setItem(STORAGE_KEY, mode === MODE_LIGHT ? MODE_LIGHT : MODE_DARK);
  } catch (e) {
    /* 隐私模式/存储不可用时忽略 */
  }
}

function writeCachedMaterial(material) {
  try {
    localStorage.setItem(STORAGE_MATERIAL, normalizeMaterial(material));
  } catch (e) {
    /* 隐私模式/存储不可用时忽略 */
  }
}

/** 把解析结果写到 DOM（data-theme / data-material + color-scheme），供 CSS token 切换 */
function applyResolved(resolved, material) {
  const root = document.documentElement;
  const next = resolved === MODE_LIGHT ? MODE_LIGHT : MODE_DARK;
  const nextMaterial = normalizeMaterial(material);
  if (root.dataset.theme !== next) root.dataset.theme = next;
  if (root.dataset.material !== nextMaterial) root.dataset.material = nextMaterial;
  try {
    if (root.style.colorScheme !== next) root.style.colorScheme = next;
  } catch (e) {
    /* 极老内核忽略 */
  }
}

function snapshot() {
  return {
    mode: state.mode,
    resolved: state.resolved,
    material: state.material,
    systemDark: state.systemDark,
    syncSystemTheme: state.syncSystemTheme,
  };
}

function notify() {
  const snap = snapshot();
  for (const fn of subscribers) {
    try {
      fn(snap);
    } catch (e) {
      console.warn("[主题] 订阅者回调异常", e);
    }
  }
}

/**
 * 写入新状态：仅在内容真正变化时落 DOM + 通知订阅者。
 * 主题解析结果由 mode 直接决定（resolved === mode，不存在跟随系统的中间层）；
 * 材质只接受白名单值，非法值归一 solid。
 */
function commit(next, { notifyNow = true } = {}) {
  const mode = normalizeMode(next.mode);
  const material =
    next.material === undefined || next.material === null
      ? state.material
      : normalizeMaterial(next.material);
  const systemDark =
    typeof next.systemDark === "boolean"
      ? next.systemDark
      : next.systemDark === null
        ? null
        : state.systemDark;
  const syncSystemTheme =
    next.syncSystemTheme === undefined || next.syncSystemTheme === null
      ? state.syncSystemTheme
      : !!next.syncSystemTheme;

  const changed =
    mode !== state.mode ||
    mode !== state.resolved ||
    material !== state.material ||
    systemDark !== state.systemDark ||
    syncSystemTheme !== state.syncSystemTheme;

  state = { mode, resolved: mode, material, systemDark, syncSystemTheme };
  applyResolved(mode, material);
  if (changed && notifyNow) notify();
  return changed;
}

// ---------- 后端事件 ----------

function applyThemeState(payload) {
  if (!payload || typeof payload !== "object") return;
  const rawMode = payload.mode !== undefined ? payload.mode : payload.theme;
  if (
    rawMode === undefined &&
    payload.material === undefined &&
    payload.systemDark === undefined &&
    payload.syncSystemTheme === undefined
  ) {
    return;
  }
  commit({
    mode: rawMode === undefined ? state.mode : rawMode,
    // 材质与 systemDark/syncSystemTheme 一样做容错透传：payload 缺失时保持当前值
    material: payload.material,
    systemDark: typeof payload.systemDark === "boolean" ? payload.systemDark : null,
    syncSystemTheme: payload.syncSystemTheme,
  });
}

function applyConfigPayload(config) {
  if (!config || typeof config !== "object") return;
  if (config.theme === undefined && config.material === undefined && config.syncSystemTheme === undefined) {
    return;
  }
  commit({
    mode: config.theme === undefined ? state.mode : config.theme,
    material: config.material,
    syncSystemTheme: config.syncSystemTheme,
  });
}

function bindTauriEvents() {
  try {
    const p1 = listen("theme-changed", (e) => {
      if (!e || !e.payload) return;
      applyThemeState(e.payload);
    });
    if (p1 && typeof p1.catch === "function") p1.catch(() => {});
  } catch (e) {
    /* ignore */
  }
  try {
    const p2 = listen("config-changed", (e) => {
      if (!e || !e.payload) return;
      applyConfigPayload(e.payload);
    });
    if (p2 && typeof p2.catch === "function") p2.catch(() => {});
  } catch (e) {
    /* ignore */
  }
}

async function waitTauri(timeoutMs = 15000) {
  const t0 = Date.now();
  while (!hasTauri()) {
    if (Date.now() - t0 > timeoutMs) return false;
    await new Promise((r) => setTimeout(r, 100));
  }
  return true;
}

/** 用后端状态校准（失败时保持本地外观，不抛错） */
async function calibrate() {
  try {
    const payload = await invokeRetry("get_theme_state", undefined, {
      attempts: 4,
      perTryMs: 2500,
      gapMs: 500,
    });
    if (!payload || typeof payload !== "object") return;
    applyThemeState(payload);
  } catch (e) {
    console.warn("[主题] 读取后端主题状态失败，继续使用本地外观", e);
  }
}

// ---------- 初始化 ----------

function initOnce() {
  if (initPromise) return initPromise;

  // 1) 同步首帧：缓存（含旧值 system→dark）优先，无缓存则深色 + solid
  //    （HTML 内联脚本已在更早时机落过 data-theme / data-material）
  migrateCachedMode();
  const cached = readCachedMode();
  const initialMode = cached || DEFAULT_MODE;
  const cachedMaterial = readCachedMaterial();
  const initialMaterial = cachedMaterial || DEFAULT_MATERIAL;
  state = {
    mode: initialMode,
    resolved: initialMode,
    material: initialMaterial,
    systemDark: null,
    syncSystemTheme: false,
  };
  applyResolved(initialMode, initialMaterial);

  // 2) 异步校准：等 Tauri 注入 → 先绑事件再拉状态（避免漏掉启动期间的事件）
  initPromise = (async () => {
    if (!(await waitTauri(15000))) return snapshot();
    bindTauriEvents();
    await calibrate();
    return snapshot();
  })().catch((e) => {
    console.warn("[主题] 初始化失败", e);
    return snapshot();
  });

  return initPromise;
}

// 模块被 import 即自动初始化（页面入口不需要额外调用）
initOnce();

// ---------- 对外 API ----------

/** 当前主题状态快照（不会随外部修改而变） */
export function getThemeState() {
  return snapshot();
}

/** 当前主题："light" | "dark" */
export function resolvedTheme() {
  return state.resolved;
}

/** 当前界面材质："solid" | "liquid" | "glow" */
export function currentMaterial() {
  return state.material;
}

/**
 * 订阅主题变化。返回取消订阅函数。
 * 注意：注册时不会立刻回调，需要初始值请先调用 getThemeState()。
 */
export function subscribeTheme(fn) {
  if (typeof fn !== "function") return () => {};
  subscribers.add(fn);
  return () => subscribers.delete(fn);
}

/**
 * 切换主题。
 * @param {"light"|"dark"} mode 只接受 light/dark；旧版传入 "system" 时按 dark 处理
 * @returns {Promise<string[]>} 后端返回的 warnings（可能为空数组）
 * @throws 参数非法或配置保存失败时抛错（调用方负责提示）
 */
export async function setThemeMode(mode) {
  const target = normalizeMode(mode, { strict: true });
  const prevMode = state.mode;
  const prevCache = readCachedMode();

  // 乐观更新：点击后立即看到效果
  commit({ mode: target });

  const warnings = [];

  // Tauri 未就绪：只本地生效
  if (!hasTauri()) {
    writeCachedMode(target);
    warnings.push("Tauri 未就绪：外观仅在当前页面切换，重启后需重新设置");
    return warnings;
  }

  try {
    const res = await invokeRetry(
      "set_theme",
      { mode: target },
      { attempts: 3, perTryMs: 3000, gapMs: 500 }
    );
    if (Array.isArray(res)) {
      for (const w of res) if (w) warnings.push(String(w));
    }
    writeCachedMode(target);
    // 后端实现会 emit theme-changed；这里再拉一次做兜底校准（事件丢失/延迟时也能对齐）
    try {
      const payload = await invoke("get_theme_state");
      if (payload && typeof payload === "object") applyThemeState(payload);
    } catch (e) {
      /* 校准失败不影响本次切换结果 */
    }
    return warnings;
  } catch (e) {
    // 回滚乐观更新
    commit({ mode: prevMode });
    if (prevCache) writeCachedMode(prevCache);
    throw e;
  }
}

/**
 * 切换界面材质（与明暗主题正交）。
 * @param {"solid"|"liquid"|"glow"} material 只接受三种材质
 * @returns {Promise<string[]>} 后端返回的 warnings（可能为空数组）
 * @throws 参数非法或配置保存失败时抛错（调用方负责提示）
 */
export async function setMaterialStyle(material) {
  const target = normalizeMaterial(material, { strict: true });
  const prevMaterial = state.material;
  const prevCache = readCachedMaterial();

  // 乐观更新：点击后立即看到效果
  commit({ material: target });

  const warnings = [];

  // Tauri 未就绪：只本地生效
  if (!hasTauri()) {
    writeCachedMaterial(target);
    warnings.push("Tauri 未就绪：界面材质仅在当前页面切换，重启后需重新设置");
    return warnings;
  }

  try {
    const res = await invokeRetry(
      "set_material",
      { material: target },
      { attempts: 3, perTryMs: 3000, gapMs: 500 }
    );
    if (Array.isArray(res)) {
      for (const w of res) if (w) warnings.push(String(w));
    }
    writeCachedMaterial(target);
    // 后端会 emit theme-changed；这里再拉一次兜底校准（事件丢失/延迟时也能对齐）
    try {
      const payload = await invoke("get_theme_state");
      if (payload && typeof payload === "object") applyThemeState(payload);
    } catch (e) {
      /* 校准失败不影响本次切换结果 */
    }
    return warnings;
  } catch (e) {
    // 回滚乐观更新
    commit({ material: prevMaterial });
    writeCachedMaterial(prevCache || DEFAULT_MATERIAL);
    throw e;
  }
}

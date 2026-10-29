//! Dock 智能自动隐藏。
//!
//! 语义（与 macOS / GNOME Dash-to-Dock 的 intellihide 一致）：
//! - 普通窗口压到 Dock 区域时自动滑下隐藏，窗口移开/还原后恢复常驻；
//! - 隐藏后鼠标移到 Dock 底边（屏幕/工作区底边）时临时滑出，指针离开后再次隐藏；
//! - 无遮挡时常驻显示；全屏隐藏优先级更高，全屏期间底边不唤出。
//!
//! 为什么需要独立线程：`monitor.rs` 的 400ms 轮询足够做窗口列表/全屏状态，
//! 但底边唤出需要 100ms 级响应，因此这里用 80ms 的轻量 cursor 轮询，
//! 窗口矩形枚举仍按 240ms 节流，避免高频 EnumWindows。
//!
//! 穿透：CSS 隐藏只是视觉隐藏，置顶透明窗口仍会拦截鼠标，所以隐藏时必须
//! `set_ignore_cursor_events(true)`；托盘点击的 `DockIgnoreGuard` 结束后也走
//! `restore_tray_ignore` 按当前状态恢复，而不是无条件取消穿透。
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use crate::app_state::AppState;
use crate::surfaces;
use crate::winapi;

/// cursor 轮询周期（底边唤出响应）
const TICK_MS: u64 = 80;
/// 普通窗口矩形枚举节流（遮挡判定）
const SCAN_MS: u64 = 240;
/// 遮挡出现后的隐藏延迟（避免拖动窗口经过 Dock 时闪烁）
const HIDE_DELAY_MS: u64 = 260;
/// 指针离开 Dock 后的保持时间
const HOVER_GRACE_MS: u64 = 450;
/// 底边唤出带：距 Dock 底边的上下容差（物理像素）
const EDGE_PX: i32 = 4;
/// 底边唤出带：Dock 左右各扩展（物理像素）
const REVEAL_PAD_PX: i32 = 24;
/// 遮挡判定时 Dock 窗口矩形的内缩（去掉窗口留白）
const OVERLAP_INSET_PX: i32 = 8;
/// 窗口与 Dock 的最小交集（宽/高，物理像素）
const MIN_INTERSECT_PX: i32 = 4;

/// 事件：Dock 是否因「智能隐藏」而隐藏（bool）
const EVENT_AUTOHIDE: &str = "dock-autohide-changed";

#[derive(Debug, Clone, Copy, Default)]
struct DockState {
    /// 因窗口遮挡而隐藏（不含全屏隐藏）
    auto_hidden: bool,
    /// 全屏隐藏（由 surfaces::emit_fullscreen_states 同步）
    fullscreen_hidden: bool,
    /// 上一轮遮挡判定结果
    overlap: bool,
    /// 本轮遮挡开始时间（0 = 无遮挡）
    overlap_since_ms: u64,
    /// 最后一次指针进入 Dock / 底边唤出带的时间
    last_interact_ms: u64,
    /// 当前是否已对窗口应用「指针穿透」
    applied_ignore: bool,
}

static STATES: OnceLock<Mutex<HashMap<String, DockState>>> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();

fn states() -> &'static Mutex<HashMap<String, DockState>> {
    STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_ms() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// 两个物理矩形是否有至少 `min_px` 宽/高的交集（负坐标同样成立）。
fn rects_intersect(a: RECT, b: RECT, min_px: i32) -> bool {
    let w = a.right.min(b.right) - a.left.max(b.left);
    let h = a.bottom.min(b.bottom) - a.top.max(b.top);
    w >= min_px && h >= min_px
}

/// 指针是否落在 Dock 底边的唤出带内。
fn hit_bottom_edge(dock: RECT, cursor: POINT) -> bool {
    cursor.x >= dock.left - REVEAL_PAD_PX
        && cursor.x <= dock.right + REVEAL_PAD_PX
        && cursor.y >= dock.bottom - EDGE_PX
        && cursor.y <= dock.bottom + EDGE_PX
}

/// 指针是否在矩形内（含边界）。
fn point_in_rect(p: POINT, r: RECT) -> bool {
    p.x >= r.left && p.x <= r.right && p.y >= r.top && p.y <= r.bottom
}

/// 去掉窗口留白后的 Dock 命中矩形。
fn inset_rect(r: RECT, px: i32) -> RECT {
    let left = r.left + px;
    let top = r.top + px;
    let right = (r.right - px).max(left + 1);
    let bottom = (r.bottom - px).max(top + 1);
    RECT {
        left,
        top,
        right,
        bottom,
    }
}

/// 纯状态机：当前是否应因遮挡而隐藏。
///
/// - 全屏由调用方通过 `fullscreen_hidden` 另行处理（这里只算遮挡）；
/// - 遮挡出现后先等 `HIDE_DELAY_MS`；
/// - 指针最近 `HOVER_GRACE_MS` 内活动过则保持显示。
fn should_hide(
    now: u64,
    overlap: bool,
    overlap_since: u64,
    last_interact: u64,
    hide_delay_ms: u64,
    hover_grace_ms: u64,
) -> bool {
    if !overlap {
        return false;
    }
    now.saturating_sub(overlap_since) >= hide_delay_ms
        && now.saturating_sub(last_interact) >= hover_grace_ms
}

fn should_ignore(state: &DockState) -> bool {
    state.auto_hidden || state.fullscreen_hidden
}

/// 启动 Dock 智能隐藏线程（进程生命周期内常驻）。
pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        let mut last_scan = Instant::now()
            .checked_sub(Duration::from_secs(10))
            .unwrap_or_else(Instant::now);
        let mut window_rects: Vec<RECT> = Vec::new();
        loop {
            let started = Instant::now();
            tick(&app, &mut last_scan, &mut window_rects);
            let elapsed = started.elapsed();
            std::thread::sleep(Duration::from_millis(TICK_MS).saturating_sub(elapsed));
        }
    });
}

fn tick(app: &AppHandle, last_scan: &mut Instant, window_rects: &mut Vec<RECT>) {
    let labels = surfaces::dock_labels(app);
    if labels.is_empty() {
        return;
    }

    let (auto_hide, fullscreen_hide) = app
        .state::<AppState>()
        .config
        .lock()
        .map(|c| (c.dock_auto_hide, c.fullscreen_hide))
        .unwrap_or((true, true));

    if !auto_hide {
        reset_states(app, &labels);
        return;
    }

    // 用户关闭「全屏自动隐藏」时，前台全屏应用不应再通过「窗口遮挡」把 Dock
    // 藏起来：全屏检测仍由 monitor 线程缓存，直接复用它的显示器 key。
    let fg_fullscreen = if fullscreen_hide {
        None
    } else {
        app.state::<AppState>()
            .fullscreen_monitor
            .lock()
            .ok()
            .and_then(|g| g.clone())
    };

    // 窗口矩形枚举按 SCAN_MS 节流；cursor 每轮都读，保证唤出及时。
    if last_scan.elapsed() >= Duration::from_millis(SCAN_MS) {
        *last_scan = Instant::now();
        *window_rects = winapi::enumerate::normal_window_rects(std::process::id());
    }

    let mut cursor = POINT::default();
    let cursor = unsafe { GetCursorPos(&mut cursor) }.ok().map(|_| cursor);

    let now = now_ms();
    let mut apply_ignore: Vec<(String, bool)> = Vec::new();

    for label in &labels {
        let Some(win) = app.get_webview_window(label) else {
            remove_state(label);
            continue;
        };
        // 配置关闭副屏 Dock 时不参与智能隐藏；窗口本身是 native hide，
        // 这里清空状态，避免重新开启时沿用旧状态。
        if !surfaces::dock_visible_for(&win) {
            let mut changed = false;
            if let Some(st) = states().lock().ok().and_then(|mut m| m.remove(label)) {
                changed = st.auto_hidden;
            }
            if changed {
                let _ = app.emit_to(label.as_str(), EVENT_AUTOHIDE, false);
            }
            let _ = win.set_ignore_cursor_events(false);
            continue;
        }

        let Some(rect) = winapi::common::window_physical_rect(&win) else {
            continue;
        };
        let hit = inset_rect(rect, OVERLAP_INSET_PX);
        let skip_for_fullscreen = match fg_fullscreen.as_deref() {
            Some(fg) => surfaces::monitor_of_window(&win)
                .map(|m| surfaces::monitor_key(&m))
                .map(|dock| fg == dock)
                .unwrap_or(false),
            None => false,
        };
        let overlap_now = !skip_for_fullscreen
            && window_rects
                .iter()
                .any(|w| rects_intersect(*w, hit, MIN_INTERSECT_PX));

        let (inside, edge) = match cursor {
            Some(p) => (point_in_rect(p, rect), hit_bottom_edge(rect, p)),
            None => (false, false),
        };

        let mut hidden_changed = None;
        {
            let mut guard = match states().lock() {
                Ok(g) => g,
                Err(e) => e.into_inner(),
            };
            let st = guard.entry(label.clone()).or_default();
            let was_hidden = st.auto_hidden;

            if overlap_now && !st.overlap {
                st.overlap_since_ms = now;
            }
            if !overlap_now {
                st.overlap_since_ms = 0;
            }
            st.overlap = overlap_now;

            // 指针在 Dock 内（仅显示状态）或命中底边唤出带时，刷新交互时间。
            // 全屏期间不响应底边唤出，避免压在全屏视频/游戏上。
            let interact = (!st.auto_hidden && inside) || (edge && !st.fullscreen_hidden);
            if interact {
                st.last_interact_ms = now;
            }

            st.auto_hidden = should_hide(
                now,
                st.overlap,
                st.overlap_since_ms,
                st.last_interact_ms,
                HIDE_DELAY_MS,
                HOVER_GRACE_MS,
            );
            if was_hidden != st.auto_hidden {
                hidden_changed = Some(st.auto_hidden);
            }

            let ignore_now = should_ignore(st);
            if ignore_now != st.applied_ignore {
                st.applied_ignore = ignore_now;
                apply_ignore.push((label.clone(), ignore_now));
            }
        }

        if let Some(hidden) = hidden_changed {
            eprintln!("[easy-dash] Dock 智能隐藏 {label}: hidden={hidden} (overlap={overlap_now})");
            let _ = app.emit_to(label.as_str(), EVENT_AUTOHIDE, hidden);
        }
    }

    for (label, ignore) in apply_ignore {
        if let Some(win) = app.get_webview_window(&label) {
            let _ = win.set_ignore_cursor_events(ignore);
        }
    }

    // 显示器拔掉/窗口销毁后清掉陈旧状态
    let alive: std::collections::HashSet<&str> = labels.iter().map(|s| s.as_str()).collect();
    if let Ok(mut guard) = states().lock() {
        guard.retain(|label, _| alive.contains(label.as_str()));
    }
}

fn remove_state(label: &str) {
    if let Ok(mut guard) = states().lock() {
        guard.remove(label);
    }
}

/// 全屏隐藏状态同步（由 `surfaces::emit_fullscreen_states` 调用）。
/// 只负责穿透与底边唤出抑制，CSS 隐藏仍走既有 `fullscreen-changed`。
pub fn set_fullscreen_hidden(app: &AppHandle, label: &str, hidden: bool) {
    let mut apply = None;
    {
        let mut guard = match states().lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        let st = guard.entry(label.to_string()).or_default();
        if st.fullscreen_hidden != hidden {
            st.fullscreen_hidden = hidden;
        }
        let want = should_ignore(st);
        if want != st.applied_ignore {
            st.applied_ignore = want;
            apply = Some(want);
        }
    }
    if let Some(want) = apply {
        if let Some(win) = app.get_webview_window(label) {
            let _ = win.set_ignore_cursor_events(want);
        }
    }
}

/// 配置热更新：关闭时立即恢复常驻；开启时由下一轮线程判定接管。
pub fn on_config_changed(app: &AppHandle, enabled: bool) {
    if !enabled {
        let labels = surfaces::dock_labels(app);
        reset_states(app, &labels);
    }
}

/// 清空所有 Dock 的「遮挡隐藏」，保留全屏隐藏与穿透状态。
fn reset_states(app: &AppHandle, labels: &[String]) {
    for label in labels {
        let mut emit = None;
        let mut apply = None;
        {
            let mut guard = match states().lock() {
                Ok(g) => g,
                Err(e) => e.into_inner(),
            };
            let st = guard.entry(label.clone()).or_default();
            if st.auto_hidden {
                st.auto_hidden = false;
                emit = Some(false);
            }
            st.overlap = false;
            st.overlap_since_ms = 0;
            let want = should_ignore(st);
            if want != st.applied_ignore {
                st.applied_ignore = want;
                apply = Some(want);
            }
        }
        if let Some(hidden) = emit {
            let _ = app.emit_to(label.as_str(), EVENT_AUTOHIDE, hidden);
        }
        if let Some(want) = apply {
            if let Some(win) = app.get_webview_window(label) {
                let _ = win.set_ignore_cursor_events(want);
            }
        }
    }
}

/// 供 `get_dock_autohide_state` 查询：某块 Dock 当前是否处于智能隐藏。
pub fn is_auto_hidden(label: &str) -> bool {
    states()
        .lock()
        .ok()
        .and_then(|g| g.get(label).map(|s| s.auto_hidden))
        .unwrap_or(false)
}

/// 托盘点击 guard 结束时恢复穿透：按当前自动隐藏/全屏隐藏状态决定，
/// 不能无条件 `false`（否则自动隐藏中的 Dock 会突然挡住桌面底部点击）。
pub fn restore_tray_ignore(win: &tauri::WebviewWindow) {
    let want = states()
        .lock()
        .ok()
        .and_then(|g| {
            g.get(win.label())
                .map(|s| s.auto_hidden || s.fullscreen_hidden)
        })
        .unwrap_or(false);
    let _ = win.set_ignore_cursor_events(want);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
        RECT {
            left: l,
            top: t,
            right: r,
            bottom: b,
        }
    }

    #[test]
    fn rect_intersection_respects_min_size() {
        let dock = rect(400, 900, 1200, 1000);
        // 3px 擦边不触发
        assert!(!rects_intersect(rect(500, 997, 900, 1100), dock, 4));
        // 4px 交集触发
        assert!(rects_intersect(rect(500, 996, 900, 1100), dock, 4));
        // 完全覆盖
        assert!(rects_intersect(rect(0, 0, 1920, 1080), dock, 4));
        // 不相交
        assert!(!rects_intersect(rect(0, 0, 1920, 800), dock, 4));
    }

    #[test]
    fn rect_intersection_supports_negative_origins() {
        let dock = rect(-1920 + 400, 900, -1920 + 1200, 1000);
        assert!(rects_intersect(rect(-1920, 0, 0, 1080), dock, 4));
    }

    #[test]
    fn bottom_edge_hit_is_limited_to_dock_width() {
        let dock = rect(400, 900, 1200, 1000);
        assert!(hit_bottom_edge(dock, POINT { x: 800, y: 998 }));
        assert!(hit_bottom_edge(dock, POINT { x: 380, y: 996 }));
        assert!(hit_bottom_edge(dock, POINT { x: 1220, y: 1002 }));
        // 水平超出扩展范围
        assert!(!hit_bottom_edge(dock, POINT { x: 300, y: 998 }));
        // 离底边太远
        assert!(!hit_bottom_edge(dock, POINT { x: 800, y: 900 }));
    }

    #[test]
    fn inset_rect_keeps_positive_size() {
        let r = inset_rect(rect(0, 0, 10, 10), 8);
        assert!(r.right > r.left && r.bottom > r.top);
    }

    #[test]
    fn hide_state_machine_waits_for_delay_and_grace() {
        // 无遮挡：始终显示
        assert!(!should_hide(10_000, false, 0, 0, 260, 480));
        // 遮挡刚开始：还没到隐藏延迟
        assert!(!should_hide(10_000, true, 9_900, 0, 260, 480));
        // 遮挡超过延迟、指针也早已离开：隐藏
        assert!(should_hide(10_500, true, 9_900, 9_000, 260, 480));
        // 遮挡超过延迟，但指针刚活动过：保持显示
        assert!(!should_hide(10_500, true, 9_900, 10_300, 260, 480));
        // 指针离开超过 grace：隐藏
        assert!(should_hide(10_900, true, 9_900, 10_300, 260, 480));
    }

    #[test]
    fn ignore_state_includes_fullscreen_and_autohide() {
        let mut st = DockState::default();
        assert!(!should_ignore(&st));
        st.auto_hidden = true;
        assert!(should_ignore(&st));
        st.auto_hidden = false;
        st.fullscreen_hidden = true;
        assert!(should_ignore(&st));
    }
}

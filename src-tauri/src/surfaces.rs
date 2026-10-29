//! 多显示器窗口编排。
//!
//! 每块显示器各有一块顶栏（`topbar-<device>`），Dock（`dock-<device>`）默认只在
//! 主屏，可在控制中心配置为每屏显示。显示器拔插 / 改分辨率 / 改缩放 / 换主屏时，
//! `monitor` 线程检测到签名变化后调用 `request_reconcile`，在主线程重建或移动窗口。
//!
//! 关键点：
//! - 所有跨屏定位使用**物理像素**（`PhysicalPosition`/`PhysicalSize`），缩放取该
//!   显示器 `scale_factor()`；不要在混合 DPI 下用主屏逻辑坐标推断。
//! - 配置关闭副屏窗口时只 `hide()`（不销毁），避免从副屏控制中心操作时把自身窗口
//!   销毁；真正拔掉显示器时才 `destroy()`。
//! - `raise` 类操作必须过滤 `is_visible()`：`raise_topmost` 带 `SWP_SHOWWINDOW`，
//!   否则会把配置关闭的窗口重新显示出来。
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use crate::app_state::AppState;
use crate::config::Config;
use crate::winapi;

/// 顶栏常态高度（逻辑像素），与 `commands::BAR_H` / `panels.js` 保持一致
pub const TOPBAR_HEIGHT: f64 = 32.0;
/// Dock 初始占位高度（逻辑像素）；首帧由 dock.js 调 resize_dock 校正
const DOCK_INIT_HEIGHT: f64 = 76.0;
/// 管理面板逻辑尺寸（外框，含 30px 阴影空间）
const MANAGER_WIDTH: f64 = 1060.0;
const MANAGER_HEIGHT: f64 = 760.0;

pub const TOPBAR_PREFIX: &str = "topbar-";
pub const DOCK_PREFIX: &str = "dock-";
pub const OVERVIEW_LABEL: &str = "overview";
pub const LAUNCHER_LABEL: &str = "launcher";
pub const MANAGER_LABEL: &str = "manager";

/// 显示器注册表：device key → 已创建的窗口
#[derive(Debug, Clone, Default)]
struct SurfaceEntry {
    signature: String,
    topbar_created: bool,
    dock_created: bool,
}

static REGISTRY: OnceLock<Mutex<HashMap<String, SurfaceEntry>>> = OnceLock::new();
static RECONCILE_PENDING: AtomicBool = AtomicBool::new(false);

fn registry() -> &'static Mutex<HashMap<String, SurfaceEntry>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn registry_clone() -> HashMap<String, SurfaceEntry> {
    registry()
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

fn registry_store(next: HashMap<String, SurfaceEntry>) {
    if let Ok(mut guard) = registry().lock() {
        *guard = next;
    }
}

/// 显示器稳定 key：Windows 下 `Monitor::name()` 即 `\\.\DISPLAY1`。
pub fn monitor_key(monitor: &tauri::Monitor) -> String {
    if let Some(name) = monitor.name() {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let p = monitor.position();
    let s = monitor.size();
    format!("{}+{}-{}x{}", p.x, p.y, s.width, s.height)
}

/// 窗口 label 只允许字母/数字/-/_；把 `\\.\DISPLAY1` 变成 `DISPLAY1`。
fn label_part(key: &str) -> String {
    let mut out = String::new();
    for c in key.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if c == '-' || c == '_' {
            out.push(c);
        }
    }
    if out.is_empty() {
        out.push_str("monitor");
    }
    // 避免超长/极端 device 名（Tauri label 也会出现在日志里）
    out.truncate(48);
    out
}

pub fn topbar_label(key: &str) -> String {
    format!("{TOPBAR_PREFIX}{}", label_part(key))
}

pub fn dock_label(key: &str) -> String {
    format!("{DOCK_PREFIX}{}", label_part(key))
}

fn monitor_signature(monitor: &tauri::Monitor) -> String {
    let p = monitor.position();
    let s = monitor.size();
    format!("{}x{}+{}+{}@{:.3}", s.width, s.height, p.x, p.y, monitor.scale_factor())
}

fn monitor_list(app: &AppHandle) -> Vec<tauri::Monitor> {
    let mut list = app.available_monitors().unwrap_or_default();
    if list.is_empty() {
        if let Ok(Some(primary)) = app.primary_monitor() {
            list.push(primary);
        }
    }
    list
}

pub fn primary_key(app: &AppHandle) -> Option<String> {
    app.primary_monitor()
        .ok()
        .flatten()
        .map(|m| monitor_key(&m))
        .or_else(|| monitor_list(app).first().map(monitor_key))
}

fn monitor_by_key(app: &AppHandle, key: &str) -> Option<tauri::Monitor> {
    monitor_list(app).into_iter().find(|m| monitor_key(m) == key)
}

fn is_primary(app: &AppHandle, key: &str) -> bool {
    primary_key(app).as_deref() == Some(key)
}

/// 窗口是否位于主屏（托盘 UIA 回退模式只在主屏显示托盘）。
pub fn is_primary_window(win: &WebviewWindow) -> bool {
    monitor_of_window(win)
        .map(|m| is_primary(win.app_handle(), &monitor_key(&m)))
        .unwrap_or(true)
}

/// 配置副本（避免持锁跨窗口调用）
fn config_of(app: &AppHandle) -> Config {
    app.state::<AppState>()
        .config
        .lock()
        .map(|c| c.clone())
        .unwrap_or_default()
}

/// 主屏顶栏窗口；没有时回退任一顶栏
pub fn primary_topbar_window(app: &AppHandle) -> Option<WebviewWindow> {
    if let Some(key) = primary_key(app) {
        if let Some(win) = app.get_webview_window(&topbar_label(&key)) {
            return Some(win);
        }
    }
    topbar_labels(app)
        .into_iter()
        .find_map(|label| app.get_webview_window(&label))
}

pub fn topbar_labels(app: &AppHandle) -> Vec<String> {
    registry_clone()
        .into_iter()
        .filter(|(_, e)| e.topbar_created)
        .map(|(key, _)| topbar_label(&key))
        .filter(|label| app.get_webview_window(label).is_some())
        .collect()
}

pub fn dock_labels(app: &AppHandle) -> Vec<String> {
    registry_clone()
        .into_iter()
        .filter(|(_, e)| e.dock_created)
        .map(|(key, _)| dock_label(&key))
        .filter(|label| app.get_webview_window(label).is_some())
        .collect()
}

/// 所有 surface 窗口 label（含隐藏的），用于重建 own_hwnds
pub fn all_surface_labels(app: &AppHandle) -> Vec<String> {
    let mut out = vec![
        OVERVIEW_LABEL.to_string(),
        LAUNCHER_LABEL.to_string(),
        MANAGER_LABEL.to_string(),
    ];
    out.extend(topbar_labels(app));
    out.extend(dock_labels(app));
    out
}

/// 只把当前可见的顶栏/Dock 抢回 topmost 顶端。
/// 注意：`raise_topmost` 会带 SWP_SHOWWINDOW，必须先过滤隐藏窗口。
pub fn raise_all_visible(app: &AppHandle) {
    for label in all_surface_labels(app) {
        let Some(win) = app.get_webview_window(&label) else {
            continue;
        };
        if !win.is_visible().unwrap_or(false) {
            continue;
        }
        if let Some(hwnd) = winapi::common::tauri_hwnd(&win) {
            winapi::common::raise_topmost(hwnd);
        }
    }
}

/// 通过窗口 label 反查显示器 key；静态窗口/未知 label 返回 None。
fn label_monitor_key(label: &str) -> Option<String> {
    registry_clone().into_iter().find_map(|(key, e)| {
        let matches = (e.topbar_created && topbar_label(&key) == label)
            || (e.dock_created && dock_label(&key) == label);
        matches.then_some(key)
    })
}

/// 根据 Win32 物理矩形在 Tauri 显示器列表中找到对应 Monitor。
/// 不依赖 `monitor_from_point` / `current_monitor` 的坐标语义（不同平台/版本可能给逻辑坐标）。
fn monitor_matching_rect(app: &AppHandle, rect: windows::Win32::Foundation::RECT) -> Option<tauri::Monitor> {
    let w = (rect.right - rect.left).max(1) as u32;
    let h = (rect.bottom - rect.top).max(1) as u32;
    monitor_list(app).into_iter().find(|m| {
        let p = m.position();
        let s = m.size();
        p.x == rect.left && p.y == rect.top && s.width == w && s.height == h
    })
}

/// 窗口所在显示器。优先用注册表（隐藏窗口 current_monitor 可能不可靠），
/// 再用 Win32 物理矩形匹配 Tauri 显示器，最后回退 Tauri 的 current_monitor。
pub fn monitor_of_window(win: &WebviewWindow) -> Option<tauri::Monitor> {
    let app = win.app_handle();
    if let Some(key) = label_monitor_key(win.label()) {
        if let Some(m) = monitor_by_key(app, &key) {
            return Some(m);
        }
    }
    if let Some(hwnd) = winapi::common::tauri_hwnd(win) {
        if let Some(rect) = winapi::monitors::from_window(hwnd) {
            if let Some(m) = monitor_matching_rect(app, rect.monitor) {
                return Some(m);
            }
        }
    }
    win.current_monitor()
        .ok()
        .flatten()
        .or_else(|| app.primary_monitor().ok().flatten())
}

/// 鼠标所在显示器；热键 / 托盘菜单没有调用窗口时使用。
pub fn cursor_monitor(app: &AppHandle) -> Option<tauri::Monitor> {
    let mut pt = POINT::default();
    if unsafe { GetCursorPos(&mut pt) }.is_ok() {
        if let Some(rect) = winapi::monitors::from_point(pt.x, pt.y) {
            if let Some(m) = monitor_matching_rect(app, rect.monitor) {
                return Some(m);
            }
        }
    }
    app.primary_monitor().ok().flatten()
}

/// 显示器顶部（物理像素）
fn monitor_origin(monitor: &tauri::Monitor) -> (i32, i32) {
    let p = monitor.position();
    (p.x, p.y)
}

fn topbar_physical_height(monitor: &tauri::Monitor) -> u32 {
    (TOPBAR_HEIGHT * monitor.scale_factor()).round().max(1.0) as u32
}

/// 按显示器摆顶栏（创建/显示器布局变化时用；会把面板高度复位到 32px）。
pub fn place_topbar(win: &WebviewWindow, monitor: &tauri::Monitor) {
    let (x, y) = monitor_origin(monitor);
    winapi::common::set_window_rect(win, x, y, monitor.size().width, topbar_physical_height(monitor));
}

/// 面板展开/收起：只改高度，保留当前 x/y 与宽度
/// （顶栏可能因该屏任务栏停靠在上/左/右而避让，不能强制回 (0,0)）。
pub fn resize_topbar_in_place(win: &WebviewWindow, monitor: &tauri::Monitor, height_logical: f64) {
    let height = ((height_logical.max(TOPBAR_HEIGHT)) * monitor.scale_factor())
        .round()
        .max(1.0) as u32;
    let pos = win
        .outer_position()
        .unwrap_or_else(|_| PhysicalPosition::new(monitor.position().x, monitor.position().y));
    let width = win
        .outer_size()
        .map(|s| s.width)
        .unwrap_or_else(|_| monitor.size().width);
    winapi::common::set_window_rect(win, pos.x, pos.y, width, height);
}

/// 顶栏面板展开/收起（命令入口）：按调用窗口所在显示器缩放，并限制在屏幕内。
pub fn resize_topbar_window(win: &WebviewWindow, height_logical: f64) {
    let Some(monitor) = monitor_of_window(win) else {
        return;
    };
    let max_h = monitor.size().height as f64 / monitor.scale_factor();
    resize_topbar_in_place(win, &monitor, height_logical.min(max_h));
}

/// Dock 逻辑尺寸是否可信。
///
/// 前端在页面被隐藏/布局未就绪时会量出 0×0，加上固定留白后约 `20×14`；
/// 正常最小内容（只剩九点格按钮）≈`82×70`，所以 60×40 的阈值不会误伤。
fn dock_size_is_sane(width: f64, height: f64) -> bool {
    width.is_finite() && height.is_finite() && width >= 60.0 && height >= 40.0
}

/// Dock 重新量尺寸（命令入口）：CSS 逻辑像素 → 物理像素，底部居中于所在显示器。
pub fn resize_dock_window(app: &AppHandle, win: &WebviewWindow, width: f64, height: f64) {
    // 兜底：前端在页面隐藏/布局未就绪时会量出 0×0（历史 bug：body 被 `.hidden`
    // 工具类 display:none），算出来约 20×14。这个尺寸会把 Dock 真窗口缩成屏幕
    // 底部一个小方块且不会自愈，直接丢弃，保留上一次的正确尺寸。
    if !dock_size_is_sane(width, height) {
        return;
    }
    let Some(monitor) = monitor_of_window(win) else {
        return;
    };
    let cfg = config_of(app);
    let scale = monitor.scale_factor();
    let screen_w = monitor.size().width as f64;
    let screen_h = monitor.size().height as f64;
    let w = (width.max(1.0) * scale).round().min(screen_w).max(1.0) as u32;
    let h = (height.max(1.0) * scale).round().min(screen_h).max(1.0) as u32;
    let bottom = dock_bottom_physical(app, &monitor, &cfg);
    let x = monitor.position().x + ((monitor.size().width as i32 - w as i32) / 2).max(0);
    let y = bottom - h as i32;
    winapi::common::set_window_rect(win, x, y, w, h);
}

/// Dock 首帧校正完成后显示（配置关闭时忽略）。
pub fn show_dock_window(win: &WebviewWindow) {
    if !dock_visible_for(win) {
        return;
    }
    // Dock 是底部面板：只在第一次 show 前抓桌面，之后复用。
    // 不能每次 autohide 重显都重截：此时屏幕上可能是活动概览/窗口，会把它们的颜色烘进 Dock。
    let is_dock = win.label().starts_with(DOCK_PREFIX);
    let publish = is_dock && !has_cached_backdrop(win);
    let backdrop = if publish {
        capture_surface_backdrop(win, false)
    } else {
        None
    };
    let _ = win.show();
    if publish {
        publish_surface_backdrop(win.app_handle(), win.label(), backdrop);
    }
    if let Some(hwnd) = crate::winapi::common::tauri_hwnd(win) {
        crate::winapi::common::raise_topmost(hwnd);
    }
}

fn dock_bottom_physical(app: &AppHandle, monitor: &tauri::Monitor, cfg: &Config) -> i32 {
    let hidden = crate::winapi::taskbar::is_hidden() || cfg.hide_taskbar;
    if hidden {
        return monitor.position().y + monitor.size().height as i32;
    }
    let key = monitor_key(monitor);
    if is_primary(app, &key) {
        if let Some(bottom) = crate::winapi::taskbar::workarea_bottom_physical() {
            return bottom;
        }
    }
    let wa = monitor.work_area();
    wa.position.y + wa.size.height as i32
}

/// 按显示器把 Dock 贴到底部居中（保留 dock.js 已经量好的尺寸）。
pub fn place_dock(win: &WebviewWindow, app: &AppHandle, monitor: &tauri::Monitor, cfg: &Config) {
    let scale = monitor.scale_factor();
    let fallback_w = (900.0 * scale).round() as u32;
    let fallback_h = (DOCK_INIT_HEIGHT * scale).round() as u32;
    let size = win.outer_size().unwrap_or(PhysicalSize::new(fallback_w, fallback_h));
    let w = size.width.min(monitor.size().width).max(1);
    let h = size.height.max(1);
    let x = monitor.position().x + ((monitor.size().width as i32 - w as i32) / 2).max(0);
    let y = dock_bottom_physical(app, monitor, cfg) - h as i32;
    winapi::common::set_window_rect(win, x, y, w, h);
}

/// 概览/应用中心：铺满目标显示器
pub fn place_overlay(win: &WebviewWindow, monitor: &tauri::Monitor) {
    let p = monitor.position();
    winapi::common::set_window_rect(
        win,
        p.x,
        p.y,
        monitor.size().width,
        monitor.size().height,
    );
}

/// 管理面板：在目标显示器工作区居中，并按工作区限制尺寸。
pub fn place_manager(win: &WebviewWindow, monitor: &tauri::Monitor) {
    let wa = monitor.work_area();
    let scale = monitor.scale_factor();
    let margin = 40.0_f64;
    let max_w = (wa.size.width as f64 / scale - margin).max(480.0);
    let max_h = (wa.size.height as f64 / scale - margin).max(360.0);
    let w = MANAGER_WIDTH.min(max_w);
    let h = MANAGER_HEIGHT.min(max_h);
    let pw = (w * scale).round() as u32;
    let ph = (h * scale).round() as u32;
    let x = wa.position.x + ((wa.size.width as i32 - pw as i32) / 2).max(0);
    let y = wa.position.y + ((wa.size.height as i32 - ph as i32) / 2).max(0);
    winapi::common::set_window_rect(win, x, y, pw, ph);
}

/// 隐藏 → 显示；顶栏 / Dock 在 show 之前抓一次桌面（窗口级玻璃底片）。
///
/// Dock 的常规入口是 [`show_dock_window`]，但配置热更新 / 多屏重建走这里，
/// 所以两种前缀都覆盖；概览 / 应用中心 / 管理面板有自己的 show_at 入口。
fn show_if_hidden(win: &WebviewWindow) {
    if win.is_visible().unwrap_or(false) {
        return;
    }
    let label = win.label();
    let is_surface = label.starts_with(TOPBAR_PREFIX) || label.starts_with(DOCK_PREFIX);
    // 顶栏/Dock 是常驻 chrome：底片只取一次（启动/新显示器），之后 show 复用。
    // 否则从全屏/活动概览/autohide 重显时会截到概览和窗口缩略图，
    // 顶栏/Dock 的颜色会跟着打开窗口的颜色变。
    let publish = is_surface && !has_cached_backdrop(win);
    let backdrop = if publish {
        capture_surface_backdrop(win, false)
    } else {
        None
    };
    let _ = win.show();
    if publish {
        publish_surface_backdrop(win.app_handle(), label, backdrop);
    }
}

/// 顶栏/Dock 是否已有缓存的桌面底片。
fn has_cached_backdrop(win: &WebviewWindow) -> bool {
    win.app_handle()
        .state::<AppState>()
        .surface_backdrops
        .lock()
        .map(|guard| guard.contains_key(win.label()))
        .unwrap_or(false)
}

/// show 之前抓取窗口客户区桌面取样并缓存到 AppState（key = 窗口 label）。
///
/// 只对隐藏窗口生效：窗口已可见时截到的是它自己（玻璃套玻璃会反馈变糊）。
/// 只在 `material=liquid` 时取样：solid / glow 不消费玻璃底图，不能因为显示窗口
/// 就在后台截屏（隐私 / 性能）；切回 liquid 时前端用 `glass_backdrop` 补一张。
/// `force=false` 时若缓存已有该 label 的底片则直接复用，不重新截屏；
/// 管理面板/概览/应用中心每次打开用 `force=true`，顶栏/Dock 用 `false`。
/// 返回 Some 时调用方应在 `show()` 之后用 [`publish_surface_backdrop`] 推送，
/// 前端也可用 `surface_backdrop` 命令回读（事件早于页面加载时兜底）。
pub fn capture_surface_backdrop(
    win: &WebviewWindow,
    force: bool,
) -> Option<winapi::glass::GlassBackdrop> {
    let debug = std::env::var_os("EASYDASH_GLASS_DEBUG").is_some();
    if win.is_visible().unwrap_or(false) {
        if debug {
            glass_debug(win, "skip-visible");
        }
        return None;
    }
    let app = win.app_handle();
    let liquid = app
        .state::<AppState>()
        .config
        .lock()
        .map(|cfg| cfg.material == "liquid")
        .unwrap_or(false);
    if !liquid {
        if debug {
            glass_debug(win, "skip-not-liquid");
        }
        return None;
    }
    if !force {
        if let Ok(guard) = app.state::<AppState>().surface_backdrops.lock() {
            if let Some(cached) = guard.get(win.label()) {
                return Some(cached.clone());
            }
        }
    }
    let backdrop = if prefers_wallpaper_backdrop(win.label()) {
        winapi::glass::wallpaper_backdrop(win).or_else(|| winapi::glass::window_backdrop(win))
    } else {
        winapi::glass::window_backdrop(win)
    };
    if debug {
        crate::commands::log_tray_click(&format!(
            "ts_ms={} mode=glass-capture label={} outer={:?} ok={} mean={:?}",
            crate::app_state::now_ms(),
            win.label(),
            win.outer_size().ok().map(|s| (s.width, s.height)),
            backdrop.is_some(),
            backdrop.as_ref().map(|b| b.mean),
        ));
    }
    let Some(backdrop) = backdrop else {
        return None;
    };
    if let Ok(mut guard) = app.state::<AppState>().surface_backdrops.lock() {
        guard.insert(win.label().to_string(), backdrop.clone());
    }
    Some(backdrop)
}

/// 顶栏 / Dock 是常驻 chrome：底片优先取**壁纸**，避免启动瞬间截到前台窗口的标题栏
/// （最大化窗口会把顶栏玻璃变成一整条纯色）。
pub fn prefers_wallpaper_backdrop(label: &str) -> bool {
    label.starts_with(TOPBAR_PREFIX) || label.starts_with(DOCK_PREFIX)
}

fn glass_debug(win: &WebviewWindow, why: &str) {
    crate::commands::log_tray_click(&format!(
        "ts_ms={} mode=glass-capture label={} note={}",
        crate::app_state::now_ms(),
        win.label(),
        why
    ));
}

/// show 之后把窗口级取样推给该窗口前端（`surface-backdrop`，负载为 camelCase GlassBackdrop）。
pub fn publish_surface_backdrop(
    app: &AppHandle,
    label: &str,
    backdrop: Option<winapi::glass::GlassBackdrop>,
) {
    if let Some(backdrop) = backdrop {
        let _ = app.emit_to(label, "surface-backdrop", &backdrop);
    }
}

fn hide_if_visible(win: &WebviewWindow) {
    if win.is_visible().unwrap_or(false) {
        let _ = win.hide();
    }
}

/// 创建一块顶栏（隐藏状态；几何/显示由调用方决定）。
/// WebView2 的磁盘缓存会把旧的 ui/css/js 留下来：升级后即使 exe 是新的，
/// 顶栏/Dock 仍可能加载旧 CSS（表现为浅色模式还是 .55 白玻璃）。
/// 本地界面资源很小，直接禁用 HTTP 缓存，保证每次启动都读新构建。
const WEBVIEW2_BROWSER_ARGS: &str = "--disable-http-cache";

/// WebView2 附加参数：默认只有 [`WEBVIEW2_BROWSER_ARGS`]；设置环境变量
/// `EASYDASH_WEBVIEW2_ARGS` 可追加调试参数（例如
/// `--remote-debugging-port=9333` 供 CDP/DevTools 连进来排查样式问题）。
fn webview2_args() -> String {
    match std::env::var("EASYDASH_WEBVIEW2_ARGS") {
        Ok(extra) if !extra.trim().is_empty() => {
            format!("{WEBVIEW2_BROWSER_ARGS} {}", extra.trim())
        }
        _ => WEBVIEW2_BROWSER_ARGS.to_string(),
    }
}

fn create_topbar(
    app: &AppHandle,
    label: &str,
    monitor: &tauri::Monitor,
) -> tauri::Result<WebviewWindow> {
    let h = topbar_physical_height(monitor) as f64 / monitor.scale_factor();
    let args = webview2_args();
    let win = WebviewWindowBuilder::new(app, label, WebviewUrl::App("topbar.html".into()))
        .title("easy-dash topbar")
        .additional_browser_args(args.as_str())
        .inner_size(800.0, h)
        .position(0.0, 0.0)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .closable(false)
        .focused(false)
        .visible(false)
        .build()?;
    if let Some(hwnd) = winapi::common::tauri_hwnd(&win) {
        winapi::common::remove_win11_chrome(hwnd);
    }
    place_topbar(&win, monitor);
    Ok(win)
}

/// 创建一块 Dock（隐藏；dock.js 首次 syncSize 后才 show）。
fn create_dock(
    app: &AppHandle,
    label: &str,
    monitor: &tauri::Monitor,
    cfg: &Config,
) -> tauri::Result<WebviewWindow> {
    let args = webview2_args();
    let win = WebviewWindowBuilder::new(app, label, WebviewUrl::App("dock.html".into()))
        .title("easy-dash dock")
        .additional_browser_args(args.as_str())
        .inner_size(900.0, DOCK_INIT_HEIGHT)
        .position(0.0, 0.0)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .closable(false)
        .focused(false)
        .visible(false)
        .build()?;
    if let Some(hwnd) = winapi::common::tauri_hwnd(&win) {
        winapi::common::remove_win11_chrome(hwnd);
    }
    place_dock(&win, app, monitor, cfg);
    Ok(win)
}

fn build_hidden(
    app: &AppHandle,
    label: &str,
    url: &str,
    width: f64,
    height: f64,
) -> tauri::Result<WebviewWindow> {
    let args = webview2_args();
    let win = WebviewWindowBuilder::new(app, label, WebviewUrl::App(url.into()))
        .title(format!("easy-dash {label}"))
        .additional_browser_args(args.as_str())
        .inner_size(width, height)
        .position(0.0, 0.0)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .closable(false)
        .focused(false)
        .visible(false)
        .build()?;
    if let Some(hwnd) = winapi::common::tauri_hwnd(&win) {
        winapi::common::remove_win11_chrome(hwnd);
    }
    Ok(win)
}

fn ensure_static_windows(app: &AppHandle) -> tauri::Result<()> {
    if app.get_webview_window(OVERVIEW_LABEL).is_none() {
        build_hidden(app, OVERVIEW_LABEL, "overview.html", 1280.0, 720.0)?;
    }
    if app.get_webview_window(LAUNCHER_LABEL).is_none() {
        build_hidden(app, LAUNCHER_LABEL, "launcher.html", 1280.0, 720.0)?;
    }
    if app.get_webview_window(MANAGER_LABEL).is_none() {
        let win = build_hidden(app, MANAGER_LABEL, "manager.html", MANAGER_WIDTH, MANAGER_HEIGHT)?;
        if let Some(monitor) = app.primary_monitor().ok().flatten() {
            let _ = win.hide();
            place_manager(&win, &monitor);
        }
    }
    Ok(())
}

/// 重建 `own_hwnds`（全屏检测/窗口枚举排除自身用）。
pub fn refresh_own_hwnds(app: &AppHandle) {
    let mut own = Vec::new();
    for label in all_surface_labels(app) {
        if let Some(win) = app.get_webview_window(&label) {
            if let Some(hwnd) = winapi::common::tauri_hwnd(&win) {
                own.push(hwnd.0 as isize);
            }
        }
    }
    let state = app.state::<AppState>();
    if let Ok(mut guard) = state.own_hwnds.lock() {
        *guard = own;
    };
}

/// 启动时创建所有 surface（主线程）。
pub fn init(app: &AppHandle) -> tauri::Result<()> {
    // 必须先于顶栏 AppBar 注册记录原始工作区：退出时恢复的应是任务栏可见、
    // 无顶栏预留的桌面工作区，否则主屏顶部会留下 48px 空白。
    crate::winapi::taskbar::capture_original_workarea();
    reconcile(app, true)
}

/// 配置热更新后即时应用显隐/AppBar；只有缺少窗口时才排一次 reconcile。
pub fn apply_config(app: &AppHandle) {
    let cfg = config_of(app);
    let primary = primary_key(app);
    let entries = registry_clone();
    let mut need_reconcile = false;

    for (key, entry) in &entries {
        let is_primary = primary.as_deref() == Some(key.as_str());
        let want_topbar = is_primary || cfg.topbar_all_monitors;
        let want_dock = is_primary || cfg.dock_all_monitors;

        if want_topbar && !entry.topbar_created {
            need_reconcile = true;
        }
        if want_dock && !entry.dock_created {
            need_reconcile = true;
        }

        let topbar = app.get_webview_window(&topbar_label(key));
        if let Some(win) = &topbar {
            if want_topbar {
                show_if_hidden(win);
                if cfg.reserve_topbar_space {
                    crate::winapi::appbar::register_topbar(win);
                } else {
                    crate::winapi::appbar::unregister_topbar(win);
                }
            } else {
                crate::winapi::appbar::unregister_topbar(win);
                hide_if_visible(win);
            }
        }

        let dock = app.get_webview_window(&dock_label(key));
        if let Some(win) = &dock {
            if want_dock {
                show_if_hidden(win);
                if let Some(monitor) = monitor_by_key(app, key) {
                    place_dock(win, app, &monitor, &cfg);
                }
            } else {
                hide_if_visible(win);
            }
        }
    }

    if need_reconcile {
        request_reconcile(app);
    }

    if let Some(win) = primary_topbar_window(app) {
        crate::winapi::taskbar::set_topbar_height(if cfg.reserve_topbar_space {
            crate::winapi::taskbar::topbar_physical_height(&win)
        } else {
            0
        });
    }
    reposition_all_docks(app);
    sync_desktop_icon_inset(app);
    emit_fullscreen_states(app);
    refresh_own_hwnds(app);
}

/// 在后台线程请求一次 reconcile（带去重）。
///
/// 不在主线程直接建窗：运行时动态 `WebviewWindowBuilder::build()` 若落在
/// 事件循环任务里，Tauri/WebView2 初始化可能重入卡死；后台线程发建窗请求，
/// 主线程只负责处理消息，是 Tauri 推荐的动态建窗方式。
pub fn request_reconcile(app: &AppHandle) {
    if RECONCILE_PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app2 = app.clone();
    std::thread::spawn(move || {
        // 让当前命令（尤其是 update_preferences 的 IPC 回复）先返回
        std::thread::sleep(std::time::Duration::from_millis(30));
        if let Err(e) = reconcile(&app2, false) {
            eprintln!("[easy-dash] 多屏窗口重建失败：{e}");
        }
        RECONCILE_PENDING.store(false, Ordering::SeqCst);
    });
}

/// 重建/移动所有 per-monitor 窗口。`close_overlays` 在显示器布局变化时为 true。
pub fn reconcile(app: &AppHandle, close_overlays: bool) -> tauri::Result<()> {
    let monitors = monitor_list(app);
    let cfg = config_of(app);
    let primary = primary_key(app).or_else(|| monitors.first().map(monitor_key));
    let mut entries = registry_clone();
    let mut alive: Vec<String> = Vec::new();

    for monitor in &monitors {
        let key = monitor_key(monitor);
        alive.push(key.clone());
        let is_primary = primary.as_deref() == Some(key.as_str());
        let want_topbar = is_primary || cfg.topbar_all_monitors;
        let want_dock = is_primary || cfg.dock_all_monitors;
        let signature = monitor_signature(monitor);

        let entry = entries.entry(key.clone()).or_default();
        let layout_changed = entry.signature != signature;

        // ---- 顶栏 ----
        if entry.topbar_created && app.get_webview_window(&topbar_label(&key)).is_none() {
            // 窗口被外部销毁（WebView 崩溃/手动关闭）：允许重建
            entry.topbar_created = false;
        }
        if !entry.topbar_created {
            // 无论配置是否开启都先创建（隐藏）：这样控制中心/管理面板切换时只需
            // show/hide，不在命令线程里动态创建 WebView，避免运行中建窗卡死。
            let win = create_topbar(app, &topbar_label(&key), monitor)?;
            entry.topbar_created = true;
            if want_topbar {
                show_if_hidden(&win);
                if cfg.reserve_topbar_space {
                    crate::winapi::appbar::register_topbar(&win);
                }
            }
        } else if let Some(win) = app.get_webview_window(&topbar_label(&key)) {
            if layout_changed {
                place_topbar(&win, monitor);
            }
            if want_topbar {
                show_if_hidden(&win);
                if cfg.reserve_topbar_space {
                    crate::winapi::appbar::register_topbar(&win);
                } else {
                    crate::winapi::appbar::unregister_topbar(&win);
                }
            } else {
                crate::winapi::appbar::unregister_topbar(&win);
                hide_if_visible(&win);
            }
        }

        // ---- Dock ----
        if entry.dock_created && app.get_webview_window(&dock_label(&key)).is_none() {
            entry.dock_created = false;
        }
        if !entry.dock_created {
            // 同顶栏：始终预创建隐藏 Dock，配置切换只 show/hide。
            let win = create_dock(app, &dock_label(&key), monitor, &cfg)?;
            entry.dock_created = true;
            if !want_dock {
                hide_if_visible(&win);
            }
        } else if let Some(win) = app.get_webview_window(&dock_label(&key)) {
            if layout_changed {
                place_dock(&win, app, monitor, &cfg);
            }
            if !want_dock {
                hide_if_visible(&win);
            }
            // want_dock 时由 dock.js 首帧校正后调 show_dock（避免占位尺寸闪现）
        }

        entry.signature = signature;
    }

    // 已被拔掉的显示器：注销 AppBar 后销毁窗口
    let removed: Vec<String> = entries
        .keys()
        .filter(|k| !alive.contains(k))
        .cloned()
        .collect();
    for key in removed {
        if let Some(entry) = entries.remove(&key) {
            if entry.topbar_created {
                if let Some(win) = app.get_webview_window(&topbar_label(&key)) {
                    crate::winapi::appbar::unregister_topbar(&win);
                    let _ = win.destroy();
                }
            }
            if entry.dock_created {
                if let Some(win) = app.get_webview_window(&dock_label(&key)) {
                    let _ = win.destroy();
                }
            }
        }
    }

    registry_store(entries);
    ensure_static_windows(app)?;
    refresh_own_hwnds(app);

    if let Some(win) = primary_topbar_window(app) {
        let px = if cfg.reserve_topbar_space {
            crate::winapi::taskbar::topbar_physical_height(&win)
        } else {
            0
        };
        crate::winapi::taskbar::set_topbar_height(px);
    }

    reposition_all_docks(app);
    sync_desktop_icon_inset(app);

    if close_overlays {
        crate::overview::hide(app);
        crate::app_center::hide(app);
        crate::manager::hide(app);
    }

    // 新创建/重新显示的窗口需要知道当前全屏状态
    emit_fullscreen_states(app);
    if let Some(monitor) = app.primary_monitor().ok().flatten() {
        if let Some(win) = app.get_webview_window(OVERVIEW_LABEL) {
            // 只校正尺寸，不显示
            if !win.is_visible().unwrap_or(false) {
                place_overlay(&win, &monitor);
            }
        }
        if let Some(win) = app.get_webview_window(LAUNCHER_LABEL) {
            if !win.is_visible().unwrap_or(false) {
                place_overlay(&win, &monitor);
            }
        }
    }
    Ok(())
}

/// 桌面图标避让：把 Explorer 桌面图标视图整体下移主屏顶栏的**常态**高度。
///
/// Windows 的桌面图标从桌面视图客户区左上角开始排，`SPI_SETWORKAREA` 不会移动它们，
/// 置顶顶栏因此会盖住第一行图标。这里按 `32 逻辑像素 × DPI`（不含面板展开高度）
/// 下移；顶栏窗口不存在时归零（移回整屏）。幂等，可重复调用。
pub fn sync_desktop_icon_inset(app: &AppHandle) {
    let inset = primary_topbar_window(app)
        .and_then(|win| crate::winapi::common::tauri_hwnd(&win))
        .map(|hwnd| {
            let scale = crate::winapi::monitors::dpi_scale(hwnd);
            (crate::winapi::common::TOPBAR_LOGICAL_HEIGHT * scale)
                .round()
                .max(1.0) as i32
        })
        .unwrap_or(0);
    crate::winapi::desktop::apply(inset);
}

/// 重新贴所有 Dock（任务栏显隐 / 工作区变化后由 commands 调用）。
pub fn reposition_all_docks(app: &AppHandle) {
    let cfg = config_of(app);
    for (key, entry) in registry_clone() {
        if !entry.dock_created {
            continue;
        }
        let Some(win) = app.get_webview_window(&dock_label(&key)) else {
            continue;
        };
        if let Some(monitor) = monitor_by_key(app, &key) {
            place_dock(&win, app, &monitor, &cfg);
        }
    }
}

/// Dock 是否允许显示（配置关闭时 dock.js 的 show_dock 应忽略）。
pub fn dock_visible_for(win: &WebviewWindow) -> bool {
    let app = win.app_handle();
    let Some(monitor) = monitor_of_window(win) else {
        return true;
    };
    let key = monitor_key(&monitor);
    let cfg = config_of(app);
    is_primary(app, &key) || cfg.dock_all_monitors
}

/// 当前全屏应用是否落在该窗口所在显示器。
pub fn fullscreen_state_for(win: &WebviewWindow) -> bool {
    let app = win.app_handle();
    let cfg = config_of(app);
    if !cfg.fullscreen_hide {
        return false;
    }
    let Some(monitor) = monitor_of_window(win) else {
        return false;
    };
    let key = monitor_key(&monitor);
    let state = app.state::<AppState>();
    // 前台是 easy-dash 自己的窗口时无法用“前台全屏”判断（例如刚在管理面板里
    // 打开副屏顶栏），回退到 monitor 线程缓存；否则实时判断，避免缓存延迟导致
    // 顶栏重新显示后短暂/持续压在全屏窗口上。
    let own = state.own_hwnds.lock().map(|g| g.clone()).unwrap_or_default();
    let fg = crate::winapi::fullscreen::foreground_id();
    if fg != 0 && own.contains(&fg) {
        return state
            .fullscreen_monitor
            .lock()
            .map(|g| g.as_deref() == Some(key.as_str()))
            .unwrap_or(false);
    }
    crate::winapi::fullscreen::foreground_fullscreen_monitor(&own)
        .as_deref()
        .map(|fullscreen_key| fullscreen_key == key)
        .unwrap_or(false)
}

/// 把当前全屏状态按窗口 label 推送（payload 仍是 bool）。
pub fn emit_fullscreen_states(app: &AppHandle) {
    let cfg = config_of(app);
    let current = app
        .state::<AppState>()
        .fullscreen_monitor
        .lock()
        .map(|g| g.clone())
        .unwrap_or(None);

    let topbars = topbar_labels(app);
    for label in topbars {
        let key = label_monitor_key(&label);
        let on = cfg.fullscreen_hide
            && key
                .as_deref()
                .zip(current.as_deref())
                .map(|(a, b)| a == b)
                .unwrap_or(false);
        let _ = app.emit_to(&label, "fullscreen-changed", on);
    }
    // Dock 除了 CSS 隐藏，还要把穿透状态也交给 dock_autohide 统一管理
    // （隐藏的透明窗口仍会拦截鼠标，全屏时底部中央点击不能被 Dock 吃掉）。
    let docks = dock_labels(app);
    for label in docks {
        let key = label_monitor_key(&label);
        let on = cfg.fullscreen_hide
            && key
                .as_deref()
                .zip(current.as_deref())
                .map(|(a, b)| a == b)
                .unwrap_or(false);
        let _ = app.emit_to(&label, "fullscreen-changed", on);
        crate::dock_autohide::set_fullscreen_hidden(app, &label, on);
    }
}

#[cfg(test)]
mod tests {
    use super::{dock_size_is_sane, label_part};

    #[test]
    fn label_part_strips_device_separators() {
        assert_eq!(label_part(r"\\.\DISPLAY1"), "DISPLAY1");
        assert_eq!(label_part("MONITOR-2"), "MONITOR-2");
        assert_eq!(label_part(""), "monitor");
        assert_eq!(label_part("////"), "monitor");
    }

    #[test]
    fn label_part_avoids_duplicate_whitespace() {
        assert_eq!(label_part(r"\\.\DISPLAY 1"), "DISPLAY1");
        assert_eq!(label_part("显示器-1"), "-1");
    }

    #[test]
    fn degenerate_dock_sizes_are_rejected() {
        // 全屏隐藏时 body 被 display:none，syncSize 量出 0×0 → 20×14（历史 bug）
        assert!(!dock_size_is_sane(20.0, 14.0));
        assert!(!dock_size_is_sane(0.0, 0.0));
        assert!(!dock_size_is_sane(f64::NAN, 80.0));
        assert!(!dock_size_is_sane(383.0, f64::INFINITY));
        // 只有九点格按钮的最小 Dock / 常规尺寸都要放行
        assert!(dock_size_is_sane(82.0, 70.0));
        assert!(dock_size_is_sane(383.0, 94.0));
    }
}

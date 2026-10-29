//! 前端可调用的 Tauri 命令
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

use crate::app_state::AppState;
use crate::config;
use crate::types::{
    AppEntry, BtDevice, ManagerInfo, MediaInfo, NotificationItem, QuickState, TaskbarState,
    ThemeState, TrayItem, VolumeLevel, WifiNetwork,
};
use crate::winapi;

/// 顶栏常态高度（逻辑像素）
const BAR_H: f64 = 32.0;

#[tauri::command]
pub fn list_windows(state: State<AppState>) -> Vec<winapi::enumerate::WinEntry> {
    winapi::enumerate::list_windows(std::process::id(), &state)
}

#[tauri::command]
pub fn toggle_overview(app: AppHandle, window: WebviewWindow) {
    let target = crate::surfaces::monitor_of_window(&window)
        .or_else(|| crate::surfaces::cursor_monitor(&app));
    if let Some(monitor) = target {
        crate::overview::toggle_at(&app, monitor);
    }
}

#[tauri::command]
pub fn close_overview(app: AppHandle) {
    crate::overview::hide(&app);
}

/// 控制中心「多屏」卡片 / 管理面板「多屏」页用
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
    pub primary: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorLayout {
    pub count: usize,
    pub primary: String,
    pub monitors: Vec<MonitorInfo>,
}

#[tauri::command]
pub fn get_monitor_layout(app: AppHandle) -> MonitorLayout {
    let primary = crate::surfaces::primary_key(&app).unwrap_or_default();
    let mut list = app.available_monitors().unwrap_or_default();
    if list.is_empty() {
        if let Ok(Some(monitor)) = app.primary_monitor() {
            list.push(monitor);
        }
    }
    let monitors = list
        .into_iter()
        .map(|monitor| {
            let name = monitor.name().cloned().unwrap_or_default();
            let position = monitor.position();
            let size = monitor.size();
            MonitorInfo {
                primary: !primary.is_empty() && name == primary,
                name,
                x: position.x,
                y: position.y,
                width: size.width,
                height: size.height,
                scale_factor: monitor.scale_factor(),
            }
        })
        .collect::<Vec<_>>();
    MonitorLayout {
        count: monitors.len(),
        primary,
        monitors,
    }
}

/// 顶栏/Dock 启动时取一次当前显示器是否处于全屏（避免新窗口错过事件）
#[tauri::command]
pub fn get_fullscreen_state(window: WebviewWindow) -> bool {
    crate::surfaces::fullscreen_state_for(&window)
}

/// Dock 当前是否被「智能隐藏」（窗口遮挡）隐藏。
/// 页面加载 / 心跳 reload / 运行时重新显示副屏 Dock 后用它补状态。
#[tauri::command]
pub fn get_dock_autohide_state(window: WebviewWindow) -> bool {
    crate::dock_autohide::is_auto_hidden(window.label())
}

#[tauri::command]
pub fn activate_window(app: AppHandle, id: i64) {
    // 激活窗口时应用中心必须先收起，否则应用会被全屏网格盖住
    crate::app_center::hide(&app);
    winapi::activate::activate_id(id);
}

/// Dock 点击：点击前已是前台 → 最小化；最小化中 → 还原；否则激活
#[tauri::command]
pub fn toggle_window(state: State<AppState>, app: AppHandle, id: i64) {
    crate::app_center::hide(&app);
    let last_fg = state
        .last_foreign_fg
        .load(std::sync::atomic::Ordering::Relaxed);
    winapi::activate::toggle_id(id, last_fg);
}

#[tauri::command]
pub fn launch_app(app: AppHandle, path: String, args: Option<String>) -> Result<(), String> {
    // Dock / 固定项启动应用时收起应用中心
    crate::app_center::hide(&app);
    winapi::launcher::launch(&path, args.as_deref().unwrap_or(""))
}

#[tauri::command]
pub fn get_config(state: State<AppState>) -> config::Config {
    state.config.lock().unwrap().clone()
}

#[tauri::command]
pub fn set_pinned(
    app: AppHandle,
    state: State<AppState>,
    pinned: Vec<config::PinnedApp>,
) -> Result<(), String> {
    {
        let mut cfg = state.config.lock().unwrap();
        cfg.pinned = pinned;
        config::save(&app, &cfg)?;
    }
    let cfg = state.config.lock().unwrap().clone();
    let _ = app.emit("config-changed", &cfg);
    Ok(())
}

#[tauri::command]
pub fn reload_config(app: AppHandle) -> Result<(), String> {
    do_reload_config(&app)
}

/// 托盘「重载配置」与 reload_config 命令共用
pub fn do_reload_config(app: &AppHandle) -> Result<(), String> {
    let cfg = config::load_or_init(app)?;
    {
        let state = app.state::<AppState>();
        *state.config.lock().unwrap() = cfg.clone();
    }
    // 重新注册热键（概览 + 应用中心）
    for warning in crate::register_hotkeys(app, &cfg) {
        eprintln!("[easy-dash] {warning}");
    }
    // 顶栏高度（工作区预留带）跟随新配置：reserveTopbarSpace 可能变化
    crate::winapi::taskbar::update_topbar_height(app);
    // 任务栏隐藏开关跟随新配置（失败只记录日志，watchdog 会继续重试）
    if let Err(e) = crate::winapi::taskbar::apply(cfg.hide_taskbar) {
        eprintln!("[easy-dash] 任务栏隐藏应用失败：{e}");
    }
    // 工作区可能变化：Dock 重新贴住新的工作区底边
    reposition_dock(app);
    // 多屏配置（每屏顶栏 / 每屏 Dock）跟随新配置
    crate::surfaces::apply_config(app);
    // Dock 智能隐藏开关跟随新配置（关闭时立即恢复常驻）
    crate::dock_autohide::on_config_changed(app, cfg.dock_auto_hide);
    // 主题状态：v0.2.2 起 mode 只由配置决定（不再跟随系统）
    let system_dark =
        crate::winapi::theme::dark_mode().or_else(|| app.state::<AppState>().system_dark_opt());
    let theme_state = theme_state_of(&cfg, system_dark);
    let _ = app.emit("theme-changed", &theme_state);
    let _ = app.emit("config-changed", &cfg);
    Ok(())
}

#[tauri::command]
pub fn get_status() -> serde_json::Value {
    let (battery, charging, network) = winapi::power::status();
    serde_json::json!({ "battery": battery, "charging": charging, "network": network })
}

/// 固定应用启动项的图标（Rust 侧缓存）。支持 exe 路径与
/// shell:AppsFolder\<AUMID>（UWP 固定项）两种形式。
/// 打包应用图标提取放到工作线程（可能阻塞），本次先返回缓存，完成时发 icon-ready。
#[tauri::command]
pub fn get_pinned_icons(
    app: AppHandle,
    state: State<AppState>,
    paths: Vec<String>,
) -> Vec<Option<String>> {
    const MONIKER: &str = "shell:AppsFolder\\";
    let mut out = Vec::with_capacity(paths.len());
    for p in &paths {
        let moniker_aumid = p.strip_prefix(MONIKER);
        let key = match moniker_aumid {
            Some(a) => format!("aumid:{a}"),
            None => format!("file:{}", p.to_lowercase()),
        };
        {
            let cache = state.icon_cache.lock().unwrap();
            if let Some(hit) = cache.get(&key) {
                out.push(hit.clone());
                continue;
            }
        }
        match moniker_aumid {
            Some(a) => {
                // 工作线程提取（AUMID AppsFolder 图标），避免主线程阻塞
                let aumid = a.to_string();
                let key2 = key.clone();
                let app2 = app.clone();
                std::thread::spawn(move || {
                    crate::winapi::icons::ensure_com();
                    let state2 = app2.state::<AppState>();
                    match crate::winapi::icons::aumid_app_info(&aumid) {
                        Some((url, disp)) => {
                            state2
                                .icon_cache
                                .lock()
                                .unwrap()
                                .insert(key2.clone(), Some(url));
                            if !disp.is_empty() {
                                state2.aumid_names.lock().unwrap().insert(key2, disp);
                            }
                        }
                        None => {
                            state2.icon_cache.lock().unwrap().insert(key2, None);
                        }
                    }
                    let _ = app2.emit("icon-ready", 0i64);
                });
                out.push(None);
            }
            None => {
                let icon = winapi::icons::file_icon_data_url(p);
                state
                    .icon_cache
                    .lock()
                    .unwrap()
                    .insert(key, icon.clone());
                out.push(icon);
            }
        }
    }
    out
}

/// 保存未固定应用的手动排序（拖拽排序用）
#[tauri::command]
pub fn set_dock_order(app: AppHandle, state: State<AppState>, keys: Vec<String>) -> Result<(), String> {
    let mut cfg = state.config.lock().unwrap();
    cfg.dock_order = keys;
    config::save(&app, &cfg)
}

/// Dock 页面心跳（每块屏的 Dock 独立上报）
#[tauri::command]
pub fn dock_heartbeat(window: WebviewWindow, state: State<AppState>) {
    let now = crate::app_state::now_ms();
    if let Ok(mut guard) = state.dock_heartbeats.lock() {
        guard.insert(window.label().to_string(), now);
    }
}

/// Dock 渲染完成后校正窗口尺寸与底部居中位置（逻辑像素）。
/// 底部基准：隐藏任务栏 → 该显示器屏幕底边；否则 → 该显示器工作区底边。
#[tauri::command]
pub fn resize_dock(app: AppHandle, window: WebviewWindow, width: f64, height: f64) {
    crate::surfaces::resize_dock_window(&app, &window, width, height);
}

/// 任务栏显隐 / 工作区接管变化后，重新把每块屏的 Dock 贴到工作区底边。
///
/// 前端 `syncSize()` 只在**内容尺寸变化**时才调用 `resize_dock`，所以启动瞬间
/// （工作区接管前）定位到旧底边后不会再纠正 —— 表现为「刚打开 Dock 位置不对，
/// 触发某些行为（Dock 内容变化）后才恢复」。这里在 Rust 侧主动补一次。
pub fn reposition_dock(app: &tauri::AppHandle) {
    crate::surfaces::reposition_all_docks(app);
}

/// 显示 Dock（首帧尺寸/位置校正完成后再显示，避免占位尺寸/旧位置闪现）。
/// 控制中心关闭「副屏 Dock」时，副屏 Dock 的 show 请求会被忽略。
#[tauri::command]
pub fn show_dock(window: WebviewWindow) {
    crate::surfaces::show_dock_window(&window);
}

/// 退出 easy-dash：走正常退出流程（`RunEvent::Exit` 会恢复任务栏与工作区）。
///
/// 供管理面板「退出」按钮与控制中心电源菜单使用——托盘右键菜单异常时的备用入口。
#[tauri::command]
pub fn quit_app(app: AppHandle) {
    app.exit(0);
}

// ==================== 顶栏面板 / 任务托盘 / 通知中心 / 控制中心 ====================

/// 顶栏窗口高度（逻辑像素）：32 = 常态；>32 = 面板展开。
/// 只调整调用窗口（每块屏的顶栏各管各的），宽度/x/y 保持不变；
/// 面板展开期间 appbar::sync_topbar 会按窗口高度自行让路。
#[tauri::command]
pub fn resize_topbar(window: WebviewWindow, height: f64) {
    crate::surfaces::resize_topbar_window(&window, height.max(BAR_H));
}

/// 读取该窗口最近一次 show 之前缓存的窗口级桌面取样（液态玻璃 2.0）。
///
/// 事件 `surface-backdrop` 有可能早于前端监听（启动时顶栏先 show、页面后加载），
/// 前端就绪后再调一次本命令兜底。
///
/// 顶栏 / Dock 这类常驻 chrome 没缓存时**按需从壁纸补算**（`wallpaper_backdrop`）：
/// 它们不该用截屏取样（启动瞬间会截到前台窗口的标题栏，玻璃变成一条纯色），
/// 而壁纸取样与窗口可见性无关，随时都能算。
#[tauri::command]
pub fn surface_backdrop(window: WebviewWindow) -> Option<winapi::glass::GlassBackdrop> {
    let state = window.state::<AppState>();
    if let Some(cached) = state
        .surface_backdrops
        .lock()
        .ok()
        .and_then(|guard| guard.get(window.label()).cloned())
    {
        return Some(cached);
    }
    if !crate::surfaces::prefers_wallpaper_backdrop(window.label()) {
        return None;
    }
    let backdrop = winapi::glass::wallpaper_backdrop(&window)?;
    if let Ok(mut guard) = state.surface_backdrops.lock() {
        guard.insert(window.label().to_string(), backdrop.clone());
    }
    Some(backdrop)
}

/// 面板打开前取样桌面背景（液态玻璃 2.0）：返回低分辨率 JPEG data URL + 亮度分位。
///
/// `x/y/width/height` 是**顶栏窗口客户区内的 CSS 像素**（相对视口），不是屏幕物理
/// 坐标；Rust 端用 `ClientToScreen` 换算，顶栏因任务栏避让偏移也不会错位。
/// 失败（越界 / 截屏失败 / 桌面近黑）返回 null，前端回退为纯半透明表面。
#[tauri::command]
pub fn glass_backdrop(
    window: WebviewWindow,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Option<winapi::glass::GlassBackdrop> {
    winapi::glass::backdrop(&window, x, y, width, height)
}

/// 面板展开后把焦点给顶栏（保证 Esc 可用）
#[tauri::command]
pub fn focus_topbar(window: WebviewWindow) {
    let _ = window.set_focus();
}

/// 当前托盘镜像条目（缓存，含 PNG 图标）。
/// UIA 回退模式只镜像主屏任务栏，因此副屏顶栏返回空列表（避免把副屏点击映射到主屏）。
#[tauri::command]
pub fn get_tray_items(window: WebviewWindow, state: State<AppState>) -> Vec<TrayItem> {
    if !crate::tray_proxy::is_active() && !crate::surfaces::is_primary_window(&window) {
        return Vec::new();
    }
    state.tray.lock().unwrap().clone()
}

/// 请求刷新托盘（非阻塞；代理模式重新广播 TaskbarCreated）
#[tauri::command]
pub fn refresh_tray(app: AppHandle) {
    crate::tray_proxy::request_refresh_or_fallback(&app);
}

/// 托盘点击诊断日志文件名（配置目录；便于用户反馈「点击无反应」时定位）
const TRAY_CLICK_LOG: &str = "tray-click.log";
/// 日志文件超过该大小时截断重写
const TRAY_CLICK_LOG_MAX: u64 = 64 * 1024;
/// 单次点击最多等待弹窗关闭的时间
const TRAY_POPUP_MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// 追加一条托盘点击诊断日志（时间/坐标/模式/popup/耗时）。
/// 文件超过 64KB 时直接删掉重写，避免无限增长。
pub(crate) fn log_tray_click(line: &str) {
    static LOG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(dir) = winapi::taskbar::config_dir() else {
        return;
    };
    let path = dir.join(TRAY_CLICK_LOG);
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > TRAY_CLICK_LOG_MAX {
            let _ = std::fs::remove_file(&path);
        }
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = writeln!(file, "{line}");
    }
}

/// Dock 指针穿透 guard：点击/重定位期间 Dock 不响应鼠标（避免盖住原生托盘），
/// 离开作用域（含 panic）时按 dock_autohide 的当前状态恢复，而不是无条件取消穿透
/// （自动隐藏中的 Dock 必须保持穿透，否则会挡住桌面底部点击）。
struct DockIgnoreGuard(Vec<tauri::WebviewWindow>);

impl DockIgnoreGuard {
    fn new(docks: Vec<tauri::WebviewWindow>) -> Self {
        for dock in &docks {
            let _ = dock.set_ignore_cursor_events(true);
        }
        Self(docks)
    }
}

impl Drop for DockIgnoreGuard {
    fn drop(&mut self) {
        for dock in self.0.drain(..) {
            crate::dock_autohide::restore_tray_ignore(&dock);
        }
    }
}

/// 一次托盘点击尝试的结果（用于诊断日志）
struct TrayClickAttempt {
    mode: &'static str,
    popup: Option<isize>,
    popup_rect: Option<(i32, i32, i32, i32)>,
    found_ms: u64,
    held_ms: u64,
    error: Option<String>,
}

/// 点击托盘中第 index 个条目：转发为任务栏托盘上的真实鼠标点击，
/// 并把点击后新出现的应用弹窗迁移到顶栏图标下方（避免浮层出现在原生托盘位置）。
///
/// 任务栏被隐藏时，点击期间必须让任务栏保持「显示但透明」（`TaskbarHold`），
/// 否则 Shell 的托盘弹窗（Owner 是任务栏窗口）会被一起隐藏/关闭，表现为「点了没反应」。
/// 若不可见模式拿不到弹窗，会再用「可见兜底」和 PostMessage 各重试一次。
/// 每次尝试都写诊断日志到配置目录 `tray-click.log`。
#[tauri::command]
pub fn tray_click(
    app: AppHandle,
    state: State<AppState>,
    window: WebviewWindow,
    index: usize,
    button: String,
    // 顶栏图标中心 x/y（CSS 逻辑像素，相对调用窗口客户区）；
    // 后端统一换算为屏幕物理坐标，副屏/混合 DPI 下不再假设窗口在 (0,0)。
    anchor_x: Option<i32>,
    anchor_y: Option<i32>,
    // 前端渲染该项时的稳定 key（order_key）；托盘项随时增减会让 index 漂移，用它兜底
    key: Option<String>,
) -> Result<(), String> {
    // 索引漂移兜底：先按稳定 key 匹配缓存，找不到再退回 index。
    // 之前只按 index 取，图标增减后点击会静默失败（表现为「图标在但点不动」）。
    let want_key = key.unwrap_or_default();

    // CSS 客户区坐标 → 屏幕物理坐标；bar_bottom 用于把菜单夹到该屏顶栏下方
    let anchor = anchor_x
        .zip(anchor_y)
        .and_then(|(x, y)| winapi::common::client_to_screen_logical(&window, x as f64, y as f64));
    let bar_bottom = winapi::common::client_to_screen_logical(
        &window,
        0.0,
        winapi::common::TOPBAR_LOGICAL_HEIGHT,
    )
    .map(|(_, y)| y);

    // 代理模式：直接向应用窗口发通知消息，不显示/不点击系统任务栏。
    if crate::tray_proxy::is_active() {
        let resolved = if want_key.is_empty() {
            state
                .tray
                .lock()
                .ok()
                .and_then(|t| t.get(index).map(|it| it.order_key.clone()))
                .unwrap_or_default()
        } else {
            want_key.clone()
        };
        if resolved.is_empty() {
            return Err(format!("托盘项不存在（index={index} key={want_key}）"));
        }
        return match crate::tray_proxy::click(
            &resolved,
            &button,
            anchor.map(|a| a.0),
            anchor.map(|a| a.1),
            bar_bottom,
        ) {
            Ok(()) => Ok(()),
            Err(e) => {
                log_tray_click(&format!(
                    "ts_ms={} mode=proxy-error key={} btn={} err={}",
                    crate::app_state::now_ms(),
                    resolved,
                    button,
                    e
                ));
                Err(e)
            }
        };
    }

    let item = {
        let tray = state.tray.lock().unwrap_or_else(|e| e.into_inner());
        let by_key = if want_key.is_empty() {
            None
        } else {
            tray.iter().find(|it| it.order_key == want_key).cloned()
        };
        match by_key.or_else(|| tray.get(index).cloned()) {
            Some(it) => it,
            None => {
                // 写日志：以便区分「前端没派发」与「后端取不到项」
                log_tray_click(&format!(
                    "ts_ms={} idx={} key={} btn={} mode=- popup=- rect=- found_ms=0 held_ms=0 err=item-missing",
                    crate::app_state::now_ms(),
                    index,
                    want_key,
                    button
                ));
                return Err(format!(
                    "托盘项不存在（index={index} key={want_key}），已请求刷新，请稍后重试"
                ));
            }
        }
    };
    let own_hwnds = state
        .own_hwnds
        .lock()
        .map(|v| v.clone())
        .unwrap_or_default();
    let button = match button.as_str() {
        "right" => "right",
        _ => "left",
    }
    .to_string();
    let native_cx = item.x + item.w / 2;
    let anchor_x_physical = anchor.map(|a| a.0).unwrap_or(native_cx);
    let app2 = app.clone();
    std::thread::spawn(move || {
        let cx = native_cx;
        let cy = item.y + item.h / 2;
        let docks: Vec<tauri::WebviewWindow> = crate::surfaces::dock_labels(&app2)
            .into_iter()
            .filter_map(|label| app2.get_webview_window(&label))
            .collect();
        // 快照必须在点击之前拍：否则任务栏/其弹窗会被当成「新窗口」
        let before = winapi::tray::snapshot_windows();
        let started = std::time::Instant::now();
        let hold = winapi::taskbar::begin_hold(0);
        let dock_guard = DockIgnoreGuard::new(docks);
        // **只点一次**：以前失败会连续重试 3 次，应用会收到重复点击（关掉又打开），
        // 体验更差。现在一次点击 + 原生菜单显示，失败只写日志。
        let click_result = winapi::tray::click_at_held(cx, cy, &button);
        // 给 Shell 创建菜单/浮层的时间（通常 100~300ms）
        std::thread::sleep(std::time::Duration::from_millis(250));
        let popup = winapi::tray::relocate_popup(&before, anchor_x_physical, (cx, cy), &own_hwnds);
        drop(dock_guard);
        let found_ms = started.elapsed().as_millis() as u64;

        let mut attempt = TrayClickAttempt {
            mode: "native",
            popup: None,
            popup_rect: None,
            found_ms,
            held_ms: found_ms,
            error: click_result.err(),
        };
        if let Some((handle, relocated)) = popup {
            attempt.popup = Some(handle);
            // Shell 菜单/浮层在原生位置（光标旁）：把任务栏真正显示出来，
            // 否则用户只会看到光标跳到右下角、屏幕上什么也没有。
            if !relocated {
                winapi::taskbar::set_hold_alpha(255);
            }
            attempt.popup_rect = winapi::tray::window_screen_rect(handle);
            let mut held_ms = found_ms;
            while held_ms < TRAY_POPUP_MAX_WAIT.as_millis() as u64
                && winapi::tray::popup_alive(handle)
            {
                std::thread::sleep(std::time::Duration::from_millis(150));
                held_ms = started.elapsed().as_millis() as u64;
            }
            attempt.held_ms = held_ms;
        }
        drop(hold);
        log_tray_click(&format_tray_click_log(index, cx, cy, &button, &attempt));
    });
    Ok(())
}

/// 诊断日志行（时间/图标坐标/模式/弹窗/耗时）
fn format_tray_click_log(index: usize, cx: i32, cy: i32, button: &str, a: &TrayClickAttempt) -> String {
    let ts_ms = crate::app_state::now_ms();
    let popup = match a.popup {
        Some(hwnd) => format!("0x{hwnd:x}"),
        None => "-".to_string(),
    };
    let rect = match a.popup_rect {
        Some((x, y, w, h)) => format!("({x},{y},{w},{h})"),
        None => "-".to_string(),
    };
    let monitor = crate::winapi::monitors::device_name_at_point(cx, cy).unwrap_or_default();
    let error = a.error.as_deref().unwrap_or("-");
    format!(
        "ts_ms={ts_ms} idx={index} btn={button} at=({cx},{cy}) monitor={monitor} mode={} popup={popup} rect={rect} found_ms={} held_ms={} err={error}",
        a.mode, a.found_ms, a.held_ms
    )
}

/// 打开 Windows 原生通知中心
#[tauri::command]
pub fn open_system_notification_center() {
    std::thread::spawn(|| {
        let _ = winapi::tray::open_system_notification_center();
    });
}

/// 通知中心条目（缓存）
#[tauri::command]
pub fn get_notifications(state: State<AppState>) -> Vec<NotificationItem> {
    state.notifications.lock().unwrap().clone()
}

/// 请求刷新通知（非阻塞，完成后发 notifications-changed）
#[tauri::command]
pub fn refresh_notifications(app: AppHandle) {
    crate::notifications::request_refresh(&app);
}

/// 打开通知来源应用。
///
/// 前端只传分组键（通知处理器 PrimaryId），启动目标一律取自后端通知缓存，
/// 不接受前端传入的路径，避免变成任意命令执行入口。
#[tauri::command]
pub fn open_notification_app(state: State<AppState>, app_key: String) -> Result<(), String> {
    let key = app_key.trim();
    if key.is_empty() {
        return Err("通知分组为空，无法定位应用".into());
    }
    let target = {
        let items = state
            .notifications
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        items.iter().filter(|n| n.app_key == key).find_map(|n| {
            n.launch
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
    };
    let Some(target) = target else {
        return Err("该系统通知没有可启动的应用入口".into());
    };
    match winapi::launcher::launch(&target, "") {
        Ok(()) => Ok(()),
        // AppsFolder 项存在但启动被拒绝：退回可执行文件路径再试一次
        Err(first) if target.starts_with("shell:AppsFolder\\") => {
            match winapi::app_icons::launch_path(key) {
                Some(path) => winapi::launcher::launch(&path, "").map_err(|second| {
                    format!("打开应用失败：{first}；改用应用路径仍失败：{second}")
                }),
                None => Err(format!("打开应用失败：{first}")),
            }
        }
        Err(e) => Err(format!("打开应用失败：{e}")),
    }
}

/// 在 blocking 线程池执行可能阻塞的调用（COM / WinRT / DDC/CI 都在这里）
async fn blocking<T, F>(f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("后台任务失败: {e}"))?
}

#[tauri::command]
pub async fn get_quick_state() -> Result<QuickState, String> {
    blocking(|| {
        crate::controls::ensure_com();
        Ok(crate::controls::quick_state())
    })
    .await
}

#[tauri::command]
pub async fn set_volume(value: u8) -> Result<(), String> {
    blocking(move || {
        crate::controls::ensure_com();
        crate::controls::set_volume(value.min(100))
    })
    .await
}

#[tauri::command]
pub async fn set_muted(muted: bool) -> Result<(), String> {
    blocking(move || {
        crate::controls::ensure_com();
        crate::controls::set_muted(muted)
    })
    .await
}

#[tauri::command]
pub async fn set_brightness(value: u32) -> Result<(), String> {
    blocking(move || crate::controls::set_brightness(value.min(100))).await
}

/// 相对调节音量（滚轮）：delta 为百分点，返回调节后的真实音量与静音状态。
#[tauri::command]
pub async fn nudge_volume(delta: i32) -> Result<VolumeLevel, String> {
    blocking(move || {
        crate::controls::ensure_com();
        let (value, muted) = crate::controls::nudge_volume(delta)?;
        Ok(VolumeLevel { value, muted })
    })
    .await
}

/// 相对调节亮度（滚轮）：delta 为百分点，返回调节后的真实亮度。
#[tauri::command]
pub async fn nudge_brightness(delta: i32) -> Result<u32, String> {
    blocking(move || crate::controls::nudge_brightness(delta)).await
}

#[tauri::command]
pub async fn set_wifi(enabled: bool) -> Result<(), String> {
    blocking(move || {
        crate::controls::ensure_com();
        crate::controls::set_wifi(enabled)
    })
    .await
}

#[tauri::command]
pub async fn set_bluetooth(enabled: bool) -> Result<(), String> {
    blocking(move || {
        crate::controls::ensure_com();
        crate::controls::set_bluetooth(enabled)
    })
    .await
}

#[tauri::command]
pub async fn set_dark_mode(dark: bool) -> Result<(), String> {
    blocking(move || crate::controls::set_dark_mode(dark)).await
}

#[tauri::command]
pub async fn set_power_mode(mode: String) -> Result<(), String> {
    blocking(move || crate::controls::set_power_mode(&mode)).await
}

#[tauri::command]
pub async fn power_action(action: String) -> Result<(), String> {
    blocking(move || crate::controls::power_action(&action)).await
}

// ==================== 通知中心：媒体播放 / 控制中心：Wi-Fi 与蓝牙连接 ====================

/// 当前媒体播放状态（Windows SMTC）；没有会话时为 None。
#[tauri::command]
pub async fn get_media_state() -> Result<Option<MediaInfo>, String> {
    blocking(|| {
        crate::controls::ensure_com();
        Ok(winapi::media::state())
    })
    .await
}

/// 媒体控制："toggle" | "prev" | "next"
#[tauri::command]
pub async fn media_action(action: String) -> Result<bool, String> {
    blocking(move || {
        crate::controls::ensure_com();
        winapi::media::action(&action)
    })
    .await
}

/// 周边 Wi-Fi 网络列表（force_scan=true 时先触发 WlanScan）。
#[tauri::command]
pub async fn get_wifi_networks(force_scan: bool) -> Result<Vec<WifiNetwork>, String> {
    blocking(move || {
        crate::controls::ensure_com();
        winapi::wifi::networks(force_scan)
    })
    .await
}

/// 连接 Wi-Fi；已保存的网络可省略密码。
#[tauri::command]
pub async fn connect_wifi(ssid: String, password: Option<String>) -> Result<(), String> {
    blocking(move || {
        crate::controls::ensure_com();
        winapi::wifi::connect(&ssid, password.as_deref())
    })
    .await
}

#[tauri::command]
pub async fn disconnect_wifi() -> Result<(), String> {
    blocking(|| {
        crate::controls::ensure_com();
        winapi::wifi::disconnect()
    })
    .await
}

/// 已配对蓝牙设备列表。
#[tauri::command]
pub async fn get_bt_devices() -> Result<Vec<BtDevice>, String> {
    blocking(|| {
        crate::controls::ensure_com();
        Ok(winapi::bt::devices())
    })
    .await
}

/// 连接 / 断开指定蓝牙设备（地址为 48bit 数值）。
#[tauri::command]
pub async fn set_bt_connected(address: u64, connect: bool) -> Result<(), String> {
    blocking(move || {
        crate::controls::ensure_com();
        winapi::bt::set_connected(address, connect)
    })
    .await
}

/// 打开指定 Windows 设置页（network-wifi / bluetooth / network / default）。
#[tauri::command]
pub async fn open_settings_page(page: String) -> Result<(), String> {
    blocking(move || {
        let uri = match page.as_str() {
            "network-wifi" => "ms-settings:network-wifi",
            "bluetooth" => "ms-settings:bluetooth",
            "network" => "ms-settings:network",
            _ => "ms-settings:",
        };
        winapi::launcher::launch(uri, "")
    })
    .await
}

// ==================== 应用中心 ====================

#[tauri::command]
pub fn toggle_app_center(app: AppHandle, window: WebviewWindow) {
    let target = crate::surfaces::monitor_of_window(&window)
        .or_else(|| crate::surfaces::cursor_monitor(&app));
    if let Some(monitor) = target {
        crate::app_center::toggle_at(&app, monitor);
    }
}

#[tauri::command]
pub fn close_app_center(app: AppHandle) {
    crate::app_center::hide(&app);
}

/// 应用中心列表（缓存）。缓存为空或超过 5 分钟时触发后台刷新，UI 通过
/// `apps-changed` 事件收到通知后重新拉取。
#[tauri::command]
pub fn get_apps(app: AppHandle, state: State<AppState>) -> Vec<AppEntry> {
    let (apps, refreshed) = {
        let apps = state.apps.lock().unwrap().clone();
        (
            apps,
            state
                .apps_refreshed_ms
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    };
    let stale = refreshed == 0
        || crate::app_state::now_ms().saturating_sub(refreshed) > 5 * 60 * 1000;
    if apps.is_empty() || stale {
        crate::apps::request_refresh(&app);
    }
    apps
}

#[tauri::command]
pub fn refresh_apps(app: AppHandle) {
    crate::apps::request_refresh(&app);
}

/// 启动应用中心里的某个应用；成功后才收起应用中心。
#[tauri::command]
pub fn launch_app_entry(app: AppHandle, state: State<AppState>, id: String) -> Result<(), String> {
    let entry = {
        let apps = state.apps.lock().unwrap();
        apps.iter().find(|a| a.id == id).cloned()
    };
    let Some(entry) = entry else {
        return Err("应用不存在（列表可能已更新，请刷新后重试）".into());
    };
    winapi::launcher::launch(&entry.launch, &entry.args)?;
    crate::app_center::hide(&app);
    Ok(())
}

// ==================== 排序 / 导入应用 ====================

/// 保存顶栏托盘图标自定义顺序（order = TrayItem.orderKey 列表）
#[tauri::command]
pub fn set_tray_order(
    app: AppHandle,
    state: State<AppState>,
    order: Vec<String>,
) -> Result<(), String> {
    {
        let mut cfg = state.config.lock().unwrap();
        cfg.tray_order = order;
        config::save(&app, &cfg)?;
    }
    let cfg = state.config.lock().unwrap().clone();
    let _ = app.emit("config-changed", &cfg);
    if crate::tray_proxy::is_active() {
        crate::tray_proxy::reapply_order();
    } else {
        crate::tray_mirror::request_refresh(&app);
    }
    Ok(())
}

/// 保存应用中心自定义顺序（order = AppEntry.id 列表），并立即重排缓存
#[tauri::command]
pub fn set_app_order(
    app: AppHandle,
    state: State<AppState>,
    order: Vec<String>,
) -> Result<(), String> {
    {
        let mut cfg = state.config.lock().unwrap();
        cfg.app_order = order;
        config::save(&app, &cfg)?;
    }
    let cfg = state.config.lock().unwrap().clone();
    let _ = app.emit("config-changed", &cfg);
    crate::apps::reapply_order(&app);
    Ok(())
}

/// 打开系统文件选择框，返回选中的 exe/lnk/url 路径（取消返回 None）
#[tauri::command]
pub async fn pick_import_file(app: AppHandle) -> Result<Option<String>, String> {
    let owner = app
        .get_webview_window(crate::surfaces::LAUNCHER_LABEL)
        .and_then(|w| winapi::common::tauri_hwnd(&w))
        .map(|h| h.0 as isize)
        .unwrap_or(0);
    blocking(move || winapi::file_dialog::pick_app_file(owner)).await
}

/// 导入一个应用到应用中心（写入 config.importedApps + 缓存）
#[tauri::command]
pub async fn import_app(app: AppHandle, path: String) -> Result<AppEntry, String> {
    let app2 = app.clone();
    blocking(move || import_app_inner(&app2, &path)).await
}

fn import_app_inner(app: &AppHandle, raw_path: &str) -> Result<AppEntry, String> {
    let path = winapi::imports::validate_import_path(raw_path)?;
    let entry = winapi::imports::make_entry(&path, "", "")?;
    let state = app.state::<AppState>();

    {
        let cfg = state.config.lock().unwrap();
        if cfg
            .imported_apps
            .iter()
            .any(|a| winapi::imports::import_id(&a.path) == entry.id)
        {
            return Err("该应用已在应用中心中".into());
        }
    }
    {
        let apps = state.apps.lock().unwrap();
        if apps.iter().any(|a| a.id == entry.id || same_launch(&a.launch, &entry.launch)) {
            return Err("该应用已在应用中心中".into());
        }
    }

    {
        let mut cfg = state.config.lock().unwrap();
        cfg.imported_apps.push(config::ImportedApp {
            name: entry.name.clone(),
            path: path.clone(),
            args: String::new(),
        });
        config::save(app, &cfg)?;
    }
    crate::apps::cache_upsert(app, entry.clone());
    let cfg = state.config.lock().unwrap().clone();
    let _ = app.emit("config-changed", &cfg);
    Ok(entry)
}

/// 移除一个导入应用（不会卸载程序，只从应用中心移除）
#[tauri::command]
pub fn remove_imported_app(app: AppHandle, state: State<AppState>, id: String) -> Result<(), String> {
    {
        let mut cfg = state.config.lock().unwrap();
        let before = cfg.imported_apps.len();
        cfg.imported_apps
            .retain(|a| winapi::imports::import_id(&a.path) != id);
        if cfg.imported_apps.len() == before {
            return Err("未找到该导入应用".into());
        }
        cfg.app_order.retain(|x| x != &id);
        config::save(&app, &cfg)?;
    }
    crate::apps::cache_remove(&app, &id);
    let cfg = state.config.lock().unwrap().clone();
    let _ = app.emit("config-changed", &cfg);
    Ok(())
}

fn same_launch(a: &str, b: &str) -> bool {
    a.replace('/', "\\").eq_ignore_ascii_case(&b.replace('/', "\\"))
}

// ==================== 主题 / 任务栏 ====================

/// 由配置推导前端使用的主题状态。
/// v0.2.2 起「跟随系统」已移除：`mode` 只会是 light/dark，`resolved` 与之一致；
/// `systemDark` 字段仅保留兼容前端（不再参与解析）。
pub(crate) fn theme_state_of(cfg: &config::Config, system_dark: Option<bool>) -> ThemeState {
    let mode = config::normalize_theme(&cfg.theme);
    let material = config::normalize_material(&cfg.material);
    ThemeState {
        resolved: mode.clone(),
        mode,
        material,
        system_dark,
        sync_system_theme: cfg.sync_system_theme,
    }
}

/// 当前主题状态。`systemDark` 仅为兼容前端字段：实时读一次注册表（几十微秒），
/// 读失败时用缓存值兜底；不影响主题模式。
#[tauri::command]
pub fn get_theme_state(state: State<AppState>) -> ThemeState {
    let system_dark = winapi::theme::dark_mode().or_else(|| state.system_dark_opt());
    state.set_system_dark(system_dark);
    let cfg = state.config.lock().unwrap().clone();
    theme_state_of(&cfg, system_dark)
}

/// 设置主题模式："light" | "dark"（旧值 "system" 等非法输入归一为 "dark"）。
///
/// 顺序：先保存配置 + 推送 theme-changed/config-changed（应用内主题立即生效），
/// 再在开启「同步系统主题」时写 Windows 注册表；写失败只作为 warning 返回，
/// 不阻断（应用内主题已经生效）。
#[tauri::command]
pub fn set_theme(
    app: AppHandle,
    state: State<AppState>,
    mode: String,
) -> Result<Vec<String>, String> {
    if mode.trim().is_empty() {
        return Err("主题模式不能为空".into());
    }
    let normalized = config::normalize_theme(&mode);

    let mut next = state.config.lock().unwrap().clone();
    next.theme = normalized;
    config::save(&app, &next)?;
    *state.config.lock().unwrap() = next.clone();

    let theme_state = theme_state_of(&next, state.system_dark_opt());
    let _ = app.emit("theme-changed", &theme_state);
    let _ = app.emit("config-changed", &next);

    let mut warnings = Vec::new();
    if next.sync_system_theme {
        let dark = next.theme == "dark";
        match winapi::theme::set_dark_mode(dark) {
            Ok(()) => state.set_system_dark(Some(dark)),
            Err(e) => warnings.push(format!("同步系统主题失败：{e}")),
        }
    }
    Ok(warnings)
}

/// 设置界面材质："solid" | "liquid" | "glow"（非法值归一为 "solid"）。
/// 只影响应用内观感，不触碰 Windows 主题注册表（那是 set_theme + syncSystemTheme 的职责）。
#[tauri::command]
pub fn set_material(
    app: AppHandle,
    state: State<AppState>,
    material: String,
) -> Result<Vec<String>, String> {
    if material.trim().is_empty() {
        return Err("界面材质不能为空".into());
    }
    let normalized = config::normalize_material(&material);

    let mut next = state.config.lock().unwrap().clone();
    next.material = normalized;
    config::save(&app, &next)?;
    *state.config.lock().unwrap() = next.clone();

    let theme_state = theme_state_of(&next, state.system_dark_opt());
    let _ = app.emit("theme-changed", &theme_state);
    let _ = app.emit("config-changed", &next);
    Ok(Vec::new())
}

/// 任务栏隐藏 / 托盘桥接状态（管理面板展示、排查托盘镜像失效）
#[tauri::command]
pub fn get_taskbar_state() -> TaskbarState {
    let (hidden, bridge_ok, supported) = winapi::taskbar::state();
    TaskbarState {
        hidden,
        bridge_ok,
        supported,
    }
}

// ==================== 管理面板 ====================

#[tauri::command]
pub fn toggle_manager(app: AppHandle, window: WebviewWindow) {
    let target = crate::surfaces::monitor_of_window(&window)
        .or_else(|| crate::surfaces::cursor_monitor(&app));
    if let Some(monitor) = target {
        crate::manager::toggle_at(&app, monitor);
    }
}

#[tauri::command]
pub fn close_manager(app: AppHandle) {
    crate::manager::hide(&app);
}

#[tauri::command]
pub fn get_manager_info(app: AppHandle) -> ManagerInfo {
    ManagerInfo {
        version: app.package_info().version.to_string(),
        identifier: app.config().identifier.clone(),
        config_path: config::config_path(&app)
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
    }
}

/// 管理面板「通用」页提交的偏好补丁（所有字段可选）
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PreferencesPatch {
    pub fullscreen_hide: Option<bool>,
    pub reserve_topbar_space: Option<bool>,
    pub tray_icons: Option<bool>,
    pub notification_center: Option<bool>,
    pub control_center: Option<bool>,
    pub hide_taskbar: Option<bool>,
    pub topbar_all_monitors: Option<bool>,
    pub dock_all_monitors: Option<bool>,
    /// Dock 智能隐藏（窗口遮挡时自动滑下，底边唤出）
    pub dock_auto_hide: Option<bool>,
    pub sync_system_theme: Option<bool>,
    /// 鼠标流光（光标跟随的柔光高光）
    pub pointer_glow: Option<bool>,
    pub hotkey: Option<String>,
    pub app_center_hotkey: Option<String>,
}

impl PreferencesPatch {
    /// 把补丁应用到配置；热键做 trim + 空串校验（先校验再改，避免半更新）。
    pub fn apply(mut self, cfg: &mut config::Config) -> Result<(), String> {
        let hotkey = self.hotkey.take().map(|s| s.trim().to_owned());
        let app_center_hotkey = self.app_center_hotkey.take().map(|s| s.trim().to_owned());
        if let Some(h) = hotkey.as_deref() {
            if h.is_empty() {
                return Err("活动概览热键不能为空".into());
            }
        }
        if let Some(h) = app_center_hotkey.as_deref() {
            if h.is_empty() {
                return Err("应用中心热键不能为空".into());
            }
        }
        if let Some(v) = self.fullscreen_hide {
            cfg.fullscreen_hide = v;
        }
        if let Some(v) = self.reserve_topbar_space {
            cfg.reserve_topbar_space = v;
        }
        if let Some(v) = self.tray_icons {
            cfg.tray_icons = v;
        }
        if let Some(v) = self.notification_center {
            cfg.notification_center = v;
        }
        if let Some(v) = self.control_center {
            cfg.control_center = v;
        }
        if let Some(v) = self.hide_taskbar {
            cfg.hide_taskbar = v;
        }
        if let Some(v) = self.topbar_all_monitors {
            cfg.topbar_all_monitors = v;
        }
        if let Some(v) = self.dock_all_monitors {
            cfg.dock_all_monitors = v;
        }
        if let Some(v) = self.dock_auto_hide {
            cfg.dock_auto_hide = v;
        }
        if let Some(v) = self.sync_system_theme {
            cfg.sync_system_theme = v;
        }
        if let Some(v) = self.pointer_glow {
            cfg.pointer_glow = v;
        }
        if let Some(h) = hotkey {
            cfg.hotkey = h;
        }
        if let Some(h) = app_center_hotkey {
            cfg.app_center_hotkey = h;
        }
        Ok(())
    }
}

/// 保存通用偏好并立即应用（热键重注册、AppBar 预留开关、任务栏显隐、托盘刷新），
/// 返回热键注册/任务栏应用等警告列表供管理面板展示。
#[tauri::command]
pub fn update_preferences(
    app: AppHandle,
    state: State<AppState>,
    patch: PreferencesPatch,
) -> Result<Vec<String>, String> {
    let before = state.config.lock().unwrap().clone();
    let mut next = before.clone();
    patch.apply(&mut next)?;
    config::save(&app, &next)?;
    *state.config.lock().unwrap() = next.clone();

    let mut warnings = Vec::new();

    let multi_changed = next.topbar_all_monitors != before.topbar_all_monitors
        || next.dock_all_monitors != before.dock_all_monitors;

    if next.reserve_topbar_space != before.reserve_topbar_space || multi_changed {
        // 注册/注销 AppBar、显示/隐藏副屏窗口、重算顶栏高度都在 apply_config 内完成
        crate::surfaces::apply_config(&app);
    }

    // 任务栏隐藏开关：即时生效。失败只作为警告返回（配置已保存，看门狗会重试）。
    if next.hide_taskbar != before.hide_taskbar {
        // 隐藏后要接管工作区，先把顶栏物理高度算准（含 reserveTopbarSpace 同批变更）
        crate::winapi::taskbar::update_topbar_height(&app);
        if let Err(e) = crate::winapi::taskbar::apply(next.hide_taskbar) {
            warnings.push(format!("任务栏隐藏失败：{e}"));
        }
        // 显隐会改变工作区底边：Dock 立刻重新贴边
        reposition_dock(&app);
        // 刷新托盘数据（代理模式只重新校准/广播，UIA 模式走桥接）
        crate::tray_proxy::request_refresh_or_fallback(&app);
    }

    if next.fullscreen_hide != before.fullscreen_hide {
        crate::surfaces::emit_fullscreen_states(&app);
    }

    // Dock 智能隐藏开关：关闭时立即恢复常驻与指针命中，开启时重算一次
    if next.dock_auto_hide != before.dock_auto_hide {
        crate::dock_autohide::on_config_changed(&app, next.dock_auto_hide);
    }

    // 主题模式 / 系统主题同步开关变化：推送最新主题状态给所有窗口
    if next.theme != before.theme || next.sync_system_theme != before.sync_system_theme {
        let theme_state = theme_state_of(&next, state.system_dark_opt());
        let _ = app.emit("theme-changed", &theme_state);
    }

    warnings.extend(crate::register_hotkeys(&app, &next));
    if next.tray_icons {
        crate::tray_proxy::request_refresh_or_fallback(&app);
    }
    if next.notification_center {
        crate::notifications::request_refresh(&app);
    }
    let _ = app.emit("config-changed", &next);
    Ok(warnings)
}

/// 管理面板添加 Dock 固定应用（仅 .exe/.lnk/.url；名称自动推导）
#[tauri::command]
pub fn add_pinned_app(
    app: AppHandle,
    state: State<AppState>,
    path: String,
) -> Result<config::PinnedApp, String> {
    let path = crate::winapi::imports::validate_import_path(&path)?;
    let name = crate::winapi::imports::make_entry(&path, "", "")
        .map(|entry| entry.name)
        .unwrap_or_else(|_| {
            std::path::Path::new(&path)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone())
        });

    let mut next = state.config.lock().unwrap().clone();
    if next
        .pinned
        .iter()
        .any(|p| same_launch(&p.path, &path))
    {
        return Err("该应用已在 Dock 固定列表中".into());
    }
    let item = config::PinnedApp {
        name,
        path,
        args: String::new(),
    };
    next.pinned.push(item.clone());
    config::save(&app, &next)?;
    *state.config.lock().unwrap() = next.clone();
    let _ = app.emit("config-changed", &next);
    Ok(item)
}

#[tauri::command]
pub fn open_config_folder(app: AppHandle) -> Result<(), String> {
    let path = config::config_path(&app).ok_or("无法确定配置文件路径")?;
    crate::winapi::launcher::launch(
        "explorer.exe",
        &format!("/select,\"{}\"", path.display()),
    )
}

#[cfg(test)]
mod preference_tests {
    use super::*;

    #[test]
    fn preferences_patch_applies_only_some_fields() {
        let mut cfg = config::Config::default();
        let before_fullscreen = cfg.fullscreen_hide;
        let before_tray = cfg.tray_icons;
        let patch = PreferencesPatch {
            fullscreen_hide: Some(!before_fullscreen),
            hotkey: Some("  ctrl+alt+f2  ".into()),
            ..Default::default()
        };
        patch.apply(&mut cfg).expect("patch 应有效");
        assert_eq!(cfg.fullscreen_hide, !before_fullscreen);
        assert_eq!(cfg.hotkey, "ctrl+alt+f2");
        assert_eq!(cfg.app_center_hotkey, "alt+space");
        assert_eq!(cfg.tray_icons, before_tray);
    }

    #[test]
    fn preferences_patch_rejects_empty_hotkey() {
        let mut cfg = config::Config::default();
        let before = cfg.hotkey.clone();
        let before_fullscreen = cfg.fullscreen_hide;
        let patch = PreferencesPatch {
            hotkey: Some("   ".into()),
            fullscreen_hide: Some(!before_fullscreen),
            ..Default::default()
        };
        assert!(patch.apply(&mut cfg).is_err());
        assert_eq!(cfg.hotkey, before);
        // 先校验后应用：其它字段也不应被改
        assert_eq!(cfg.fullscreen_hide, before_fullscreen);
    }

    #[test]
    fn preferences_patch_applies_dock_auto_hide() {
        let mut cfg = config::Config::default();
        assert!(cfg.dock_auto_hide);
        let patch = PreferencesPatch {
            dock_auto_hide: Some(false),
            ..Default::default()
        };
        patch.apply(&mut cfg).expect("patch 应有效");
        assert!(!cfg.dock_auto_hide);
        // 未提交字段保持默认（尤其不能误改热键）
        assert_eq!(cfg.hotkey, "alt+f1");
        assert_eq!(cfg.app_center_hotkey, "alt+space");
    }

    #[test]
    fn preferences_patch_applies_taskbar_and_sync_switches() {
        let mut cfg = config::Config::default();
        let patch = PreferencesPatch {
            hide_taskbar: Some(false),
            sync_system_theme: Some(false),
            ..Default::default()
        };
        patch.apply(&mut cfg).expect("patch 应有效");
        assert!(!cfg.hide_taskbar);
        assert!(!cfg.sync_system_theme);
        // 未提交的字段保持不变（默认主题已改为 dark）
        assert_eq!(cfg.theme, "dark");
        assert!(cfg.tray_icons);
    }

    #[test]
    fn preferences_patch_deserializes_camel_case_taskbar_fields() {
        let patch: PreferencesPatch =
            serde_json::from_str(r#"{"hideTaskbar":false,"syncSystemTheme":false}"#).unwrap();
        assert_eq!(patch.hide_taskbar, Some(false));
        assert_eq!(patch.sync_system_theme, Some(false));
        // 其它字段缺省为 None（不误改）
        assert_eq!(patch.tray_icons, None);
        assert_eq!(patch.fullscreen_hide, None);
        assert_eq!(patch.topbar_all_monitors, None);
        assert_eq!(patch.dock_all_monitors, None);
        assert_eq!(patch.pointer_glow, None);
    }

    #[test]
    fn preferences_patch_deserializes_camel_case_pointer_glow() {
        let patch: PreferencesPatch = serde_json::from_str(r#"{"pointerGlow":false}"#).unwrap();
        assert_eq!(patch.pointer_glow, Some(false));
        // 其它字段缺省为 None（不误改）
        assert_eq!(patch.sync_system_theme, None);
        assert_eq!(patch.hide_taskbar, None);
        assert_eq!(patch.fullscreen_hide, None);

        let mut cfg = config::Config::default();
        assert!(cfg.pointer_glow, "pointerGlow 默认应为 true");
        patch.apply(&mut cfg).expect("patch 应有效");
        assert!(!cfg.pointer_glow, "patch 应关闭鼠标流光");
    }

    #[test]
    fn preferences_patch_applies_multi_monitor_switches() {
        let mut cfg = config::Config::default();
        assert!(cfg.topbar_all_monitors);
        assert!(!cfg.dock_all_monitors);
        let patch: PreferencesPatch = serde_json::from_str(
            r#"{"topbarAllMonitors":false,"dockAllMonitors":true}"#,
        )
        .unwrap();
        patch.apply(&mut cfg).expect("patch 应有效");
        assert!(!cfg.topbar_all_monitors);
        assert!(cfg.dock_all_monitors);
        // 未提交字段保持不变
        assert!(cfg.hide_taskbar);
        assert_eq!(cfg.theme, "dark");
    }

    #[test]
    fn theme_state_resolved_always_matches_mode() {
        // v0.2.2 起不再「跟随系统」：resolved 恒等于 mode，systemDark 只是兼容字段
        let cfg = config::Config::default();
        assert_eq!(cfg.theme, "dark");
        let dark = theme_state_of(&cfg, Some(false));
        assert_eq!(dark.mode, "dark");
        assert_eq!(dark.resolved, "dark", "系统是浅色也不影响用户选的深色");
        assert_eq!(dark.system_dark, Some(false), "systemDark 仍透传供前端参考");
        assert!(dark.sync_system_theme);

        let mut light = config::Config::default();
        light.theme = "light".into();
        let st = theme_state_of(&light, Some(true));
        assert_eq!(st.mode, "light");
        assert_eq!(st.resolved, "light", "系统是深色也不影响用户选的浅色");
        assert_eq!(st.system_dark, Some(true));

        // 旧配置残留 "system" 也归一为深色
        let mut legacy = config::Config::default();
        legacy.theme = "system".into();
        let st = theme_state_of(&legacy, Some(false));
        assert_eq!(st.mode, "dark");
        assert_eq!(st.resolved, "dark");

        // 读不到系统值：不再影响解析
        let unknown = theme_state_of(&cfg, None);
        assert_eq!(unknown.resolved, "dark");
        assert_eq!(unknown.system_dark, None);
    }

    #[test]
    fn theme_state_fixed_mode_ignores_system_value() {
        let mut cfg = config::Config::default();
        cfg.theme = "dark".into();
        let st = theme_state_of(&cfg, Some(false));
        assert_eq!(st.mode, "dark");
        assert_eq!(st.resolved, "dark");

        cfg.theme = "light".into();
        let st = theme_state_of(&cfg, Some(true));
        assert_eq!(st.resolved, "light");
    }

    #[test]
    fn theme_state_payload_is_camel_case_for_frontend() {
        let cfg = config::Config {
            theme: "dark".into(),
            sync_system_theme: false,
            ..Default::default()
        };
        let state = theme_state_of(&cfg, Some(true));
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(json["mode"], "dark");
        assert_eq!(json["resolved"], "dark");
        assert_eq!(json["systemDark"], true);
        assert_eq!(json["syncSystemTheme"], false);
        assert_eq!(json["material"], "solid", "未设置材质时应为 solid");

        // 材质与亮暗正交：material 独立透传给前端
        let mut glow = config::Config {
            theme: "dark".into(),
            sync_system_theme: false,
            ..Default::default()
        };
        glow.material = "glow".into();
        let state = theme_state_of(&glow, Some(true));
        assert_eq!(state.material, "glow", "material 应原样透传");
        let text = serde_json::to_string(&state).unwrap();
        assert!(
            text.contains(r#""material":"glow""#),
            "缺 camelCase material: {text}"
        );

        // 非法材质经 theme_state_of 归一为 "solid"
        let mut bad = config::Config {
            theme: "dark".into(),
            sync_system_theme: false,
            ..Default::default()
        };
        bad.material = "glass".into();
        let state = theme_state_of(&bad, Some(true));
        assert_eq!(state.material, "solid", "非法材质应归一为 solid");
    }

    #[test]
    fn taskbar_state_payload_is_camel_case_for_frontend() {
        let state = crate::types::TaskbarState {
            hidden: true,
            bridge_ok: false,
            supported: true,
        };
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(json["hidden"], true);
        assert_eq!(json["bridgeOk"], false);
        assert_eq!(json["supported"], true);
    }
}

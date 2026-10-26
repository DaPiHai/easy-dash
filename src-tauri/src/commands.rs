//! 前端可调用的 Tauri 命令
use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::app_state::AppState;
use crate::config;
use crate::types::{
    AppEntry, BtDevice, ManagerInfo, MediaInfo, NotificationItem, QuickState, TrayItem, WifiNetwork,
};
use crate::winapi;

/// 顶栏常态高度（逻辑像素）
const BAR_H: f64 = 32.0;

#[tauri::command]
pub fn list_windows(state: State<AppState>) -> Vec<winapi::enumerate::WinEntry> {
    winapi::enumerate::list_windows(std::process::id(), &state)
}

#[tauri::command]
pub fn toggle_overview(app: AppHandle) {
    crate::overview::toggle(&app);
}

#[tauri::command]
pub fn close_overview(app: AppHandle) {
    crate::overview::hide(&app);
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

/// Dock 页面心跳（页面每 3 秒调用一次）
#[tauri::command]
pub fn dock_heartbeat(state: State<AppState>) {
    state
        .dock_hb_ms
        .store(crate::app_state::now_ms(), std::sync::atomic::Ordering::Relaxed);
}

/// Dock 渲染完成后校正窗口尺寸与底部居中位置（逻辑像素）
#[tauri::command]
pub fn resize_dock(app: AppHandle, width: f64, height: f64) {
    use tauri::{LogicalPosition, LogicalSize};
    if let Some(dock) = app.get_webview_window("dock") {
        if let Ok(Some(m)) = app.primary_monitor() {
            let scale = m.scale_factor();
            let sw = m.size().width as f64 / scale;
            let sh = m.size().height as f64 / scale;
            // 宽度不超屏幕；超高截断
            let w = width.max(1.0).min(sw);
            let h = height.max(1.0).min(sh);
            let _ = dock.set_size(LogicalSize::new(w, h));
            let _ = dock.set_position(LogicalPosition::new(
                ((sw - w) / 2.0).max(0.0),
                (sh - h).max(0.0),
            ));
        }
    }
}

// ==================== 顶栏面板 / 任务托盘 / 通知中心 / 控制中心 ====================

/// 顶栏窗口高度（逻辑像素）：32 = 常态；>32 = 面板展开。
/// 展开期间 monitor 的 AppBar 周期校正会让路（见 monitor.rs）。
#[tauri::command]
pub fn resize_topbar(app: AppHandle, state: State<AppState>, height: f64) {
    use tauri::{LogicalPosition, LogicalSize};
    if let Some(tb) = app.get_webview_window("topbar") {
        if let Ok(Some(m)) = app.primary_monitor() {
            let scale = m.scale_factor();
            let sw = m.size().width as f64 / scale;
            let sh = m.size().height as f64 / scale;
            let h = height.max(BAR_H).min(sh);
            let _ = tb.set_size(LogicalSize::new(sw, h));
            let _ = tb.set_position(LogicalPosition::new(0.0, 0.0));
            state
                .topbar_panel_open
                .store(h > BAR_H + 0.5, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// 面板展开后把焦点给顶栏（保证 Esc 可用）
#[tauri::command]
pub fn focus_topbar(app: AppHandle) {
    if let Some(tb) = app.get_webview_window("topbar") {
        let _ = tb.set_focus();
    }
}

/// 当前托盘镜像条目（缓存，含 PNG 图标）
#[tauri::command]
pub fn get_tray_items(state: State<AppState>) -> Vec<TrayItem> {
    state.tray.lock().unwrap().clone()
}

/// 请求刷新托盘（非阻塞，工作线程完成后发 tray-changed）
#[tauri::command]
pub fn refresh_tray(app: AppHandle) {
    crate::tray_mirror::request_refresh(&app);
}

/// 点击托盘中第 index 个条目：转发为任务栏托盘上的真实鼠标点击，
/// 并把点击后新出现的应用弹窗迁移到顶栏图标下方（避免浮层出现在原生托盘位置）。
/// Dock 可能比任务栏更宽而盖住托盘区域，点击期间临时让 Dock 指针穿透。
#[tauri::command]
pub fn tray_click(
    app: AppHandle,
    state: State<AppState>,
    index: usize,
    button: String,
    // 顶栏图标中心 x（逻辑像素，窗口位于 0,0）；缺省时退回原生托盘位置
    anchor_x: Option<i32>,
) -> Result<(), String> {
    let item = {
        let tray = state.tray.lock().unwrap();
        tray.get(index).cloned().ok_or_else(|| "托盘项不存在".to_string())?
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
    let anchor = anchor_x.unwrap_or(native_cx);
    let app2 = app.clone();
    std::thread::spawn(move || {
        let before = winapi::tray::snapshot_windows();
        if let Some(dock) = app2.get_webview_window("dock") {
            let _ = dock.set_ignore_cursor_events(true);
        }
        let cx = native_cx;
        let cy = item.y + item.h / 2;
        let _ = winapi::tray::click_at(cx, cy, &button);
        std::thread::sleep(std::time::Duration::from_millis(150));
        if let Some(dock) = app2.get_webview_window("dock") {
            let _ = dock.set_ignore_cursor_events(false);
        }
        // 迁移应用弹出的菜单/浮层到顶栏图标下方（识别不到时保持原生位置）
        let _ = winapi::tray::relocate_popup(&before, anchor, (cx, cy), &own_hwnds);
    });
    Ok(())
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
pub fn toggle_app_center(app: AppHandle) {
    crate::app_center::toggle(&app);
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
    crate::tray_mirror::request_refresh(&app);
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
        .get_webview_window("launcher")
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

// ==================== 管理面板 ====================

#[tauri::command]
pub fn toggle_manager(app: AppHandle) {
    crate::manager::toggle(&app);
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
        if let Some(h) = hotkey {
            cfg.hotkey = h;
        }
        if let Some(h) = app_center_hotkey {
            cfg.app_center_hotkey = h;
        }
        Ok(())
    }
}

/// 保存通用偏好并立即应用（热键重注册、AppBar 预留开关、托盘刷新），
/// 返回热键注册警告列表供管理面板展示。
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

    if next.reserve_topbar_space != before.reserve_topbar_space {
        crate::winapi::appbar::unregister_all();
        if next.reserve_topbar_space {
            if let Some(topbar) = app.get_webview_window("topbar") {
                crate::winapi::appbar::register_topbar(&topbar);
            }
        }
    }

    let warnings = crate::register_hotkeys(&app, &next);
    if next.tray_icons {
        crate::tray_mirror::request_refresh(&app);
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
}

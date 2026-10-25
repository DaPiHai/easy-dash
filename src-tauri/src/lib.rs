mod app_center;
mod app_state;
mod apps;
mod commands;
mod config;
mod controls;
mod monitor;
mod notifications;
mod notify_db;
mod overview;
mod tray;
mod tray_mirror;
mod types;
mod winapi;
mod worker;

use tauri::Manager;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(app_state::AppState::new())
        .invoke_handler(tauri::generate_handler![
            commands::list_windows,
            commands::toggle_overview,
            commands::close_overview,
            commands::activate_window,
            commands::toggle_window,
            commands::launch_app,
            commands::get_config,
            commands::set_pinned,
            commands::reload_config,
            commands::get_status,
            commands::get_pinned_icons,
            commands::resize_dock,
            commands::dock_heartbeat,
            commands::set_dock_order,
            commands::resize_topbar,
            commands::focus_topbar,
            commands::get_tray_items,
            commands::refresh_tray,
            commands::tray_click,
            commands::open_system_notification_center,
            commands::get_notifications,
            commands::refresh_notifications,
            commands::get_quick_state,
            commands::set_volume,
            commands::set_muted,
            commands::set_brightness,
            commands::set_wifi,
            commands::set_bluetooth,
            commands::set_dark_mode,
            commands::set_power_mode,
            commands::power_action,
            commands::get_media_state,
            commands::media_action,
            commands::get_wifi_networks,
            commands::connect_wifi,
            commands::disconnect_wifi,
            commands::get_bt_devices,
            commands::set_bt_connected,
            commands::open_settings_page,
            commands::toggle_app_center,
            commands::close_app_center,
            commands::get_apps,
            commands::refresh_apps,
            commands::launch_app_entry,
            commands::set_tray_order,
            commands::set_app_order,
            commands::pick_import_file,
            commands::import_app,
            commands::remove_imported_app,
        ])
        .setup(|app| {
            // 加载（或初始化）配置
            let cfg = config::load_or_init(app.handle())?;
            {
                let state = app.state::<app_state::AppState>();
                *state.config.lock().unwrap() = cfg.clone();
            }

            // 创建三个窗口
            create_windows(app)?;

            // 记录自身窗口句柄（全屏检测需排除）
            {
                let state = app.state::<app_state::AppState>();
                let mut own = state.own_hwnds.lock().unwrap();
                for label in ["topbar", "dock", "overview", "launcher"] {
                    if let Some(w) = app.get_webview_window(label) {
                        if let Some(h) = winapi::common::tauri_hwnd(&w) {
                            own.push(h.0 as isize);
                        }
                    }
                }
            }

            // 顶栏 AppBar 预留空间（让最大化窗口避开顶栏）
            #[cfg(windows)]
            {
                if cfg.reserve_topbar_space {
                    if let Some(topbar) = app.get_webview_window("topbar") {
                        winapi::appbar::register_topbar(&topbar);
                    }
                }
            }

            // 托盘
            tray::create(app)?;

            // 全局热键：Alt+F1 概览 + Alt+Space 应用中心（失败只记录日志，不阻断启动）
            register_hotkeys(app.handle(), &cfg);

            // 后台轮询：窗口列表 + 全屏检测
            monitor::spawn(app.handle().clone());
            // 图标工作线程：隔离可能阻塞的 Shell COM 调用
            worker::spawn(app.handle().clone());
            // 顶栏任务托盘镜像 / 通知中心工作线程
            tray_mirror::spawn(app.handle().clone());
            notifications::spawn(app.handle().clone());
            // 应用中心数据线程（启动即后台枚举，首次打开应用中心无需等待）
            apps::spawn(app.handle().clone());
            apps::request_refresh(app.handle());
            if cfg.tray_icons {
                tray_mirror::request_refresh(app.handle());
            }
            if cfg.notification_center {
                notifications::request_refresh(app.handle());
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            if let tauri::RunEvent::Exit = event {
                #[cfg(windows)]
                {
                    winapi::appbar::unregister_all();
                }
            }
        });
}

fn create_windows(app: &tauri::App) -> tauri::Result<()> {
    use tauri::{WebviewUrl, WebviewWindowBuilder};

    let monitor = app
        .primary_monitor()?
        .ok_or_else(|| tauri::Error::WindowNotFound)?;
    let scale = monitor.scale_factor();
    let sw = monitor.size().width as f64 / scale;
    let sh = monitor.size().height as f64 / scale;

    // 顶栏
    let topbar = WebviewWindowBuilder::new(app, "topbar", WebviewUrl::App("topbar.html".into()))
        .title("easy-dash topbar")
        .inner_size(sw, 32.0)
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
        .visible(true)
        .build()?;
    if let Some(h) = winapi::common::tauri_hwnd(&topbar) {
        winapi::common::remove_win11_chrome(h);
    }

    // Dock（初值随后由前端 resize_dock 校正）
    let dock_w = 900.0_f64.min(sw);
    let dock_h = 76.0;
    let dock = WebviewWindowBuilder::new(app, "dock", WebviewUrl::App("dock.html".into()))
        .title("easy-dash dock")
        .inner_size(dock_w, dock_h)
        .position((sw - dock_w) / 2.0, sh - dock_h)
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
        .visible(true)
        .build()?;
    if let Some(h) = winapi::common::tauri_hwnd(&dock) {
        winapi::common::remove_win11_chrome(h);
    }

    // 活动概览（默认隐藏、全屏）
    let overview = WebviewWindowBuilder::new(
        app,
        "overview",
        WebviewUrl::App("overview.html".into()),
    )
    .title("easy-dash overview")
    .inner_size(sw, sh)
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
    if let Some(h) = winapi::common::tauri_hwnd(&overview) {
        winapi::common::remove_win11_chrome(h);
    }

    // 应用中心（全屏网格，默认隐藏）
    let launcher = WebviewWindowBuilder::new(
        app,
        "launcher",
        WebviewUrl::App("launcher.html".into()),
    )
    .title("easy-dash launcher")
    .inner_size(sw, sh)
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
    if let Some(h) = winapi::common::tauri_hwnd(&launcher) {
        winapi::common::remove_win11_chrome(h);
    }

    Ok(())
}

/// 注册全局热键：`hotkey` = 活动概览，`appCenterHotkey` = 应用中心。
/// 任一注册失败只记录日志（例如被其它程序占用），不阻断应用启动。
pub(crate) fn register_hotkeys(app: &tauri::AppHandle, cfg: &config::Config) {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();

    let overview_hotkey = cfg.hotkey.clone();
    if let Err(e) = gs.on_shortcut(overview_hotkey.as_str(), |app, _s, event| {
        if event.state() == ShortcutState::Pressed {
            overview::toggle(app);
        }
    }) {
        eprintln!("[easy-dash] 概览热键注册失败（{overview_hotkey}）：{e}");
    }

    let app_center_hotkey = cfg.app_center_hotkey.clone();
    if let Err(e) = gs.on_shortcut(app_center_hotkey.as_str(), |app, _s, event| {
        if event.state() == ShortcutState::Pressed {
            app_center::toggle(app);
        }
    }) {
        eprintln!("[easy-dash] 应用中心热键注册失败（{app_center_hotkey}）：{e}");
    }
}

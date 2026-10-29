mod app_center;
mod app_state;
mod apps;
mod commands;
mod config;
mod controls;
mod dock_autohide;
mod manager;
mod monitor;
mod notifications;
mod notify_db;
mod overview;
mod surfaces;
mod tray;
mod tray_mirror;
mod tray_proxy;
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
            commands::get_monitor_layout,
            commands::get_fullscreen_state,
            commands::get_dock_autohide_state,
            commands::get_pinned_icons,
            commands::resize_dock,
            commands::show_dock,
            commands::quit_app,
            commands::dock_heartbeat,
            commands::set_dock_order,
            commands::resize_topbar,
            commands::glass_backdrop,
            commands::surface_backdrop,
            commands::focus_topbar,
            commands::get_tray_items,
            commands::refresh_tray,
            commands::tray_click,
            commands::open_system_notification_center,
            commands::get_notifications,
            commands::refresh_notifications,
            commands::open_notification_app,
            commands::get_quick_state,
            commands::set_volume,
            commands::set_muted,
            commands::set_brightness,
            commands::nudge_volume,
            commands::nudge_brightness,
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
            commands::toggle_manager,
            commands::close_manager,
            commands::get_manager_info,
            commands::update_preferences,
            commands::add_pinned_app,
            commands::open_config_folder,
            commands::get_theme_state,
            commands::set_theme,
            commands::set_material,
            commands::get_taskbar_state,
        ])
        .setup(|app| {
            // 加载（或初始化）配置
            let cfg = config::load_or_init(app.handle())?;
            {
                let state = app.state::<app_state::AppState>();
                *state.config.lock().unwrap() = cfg.clone();
            }

            // 任务栏隐藏：注入配置目录（强杀残留标记 taskbar-hidden.lock 的位置），
            // 并先恢复上次异常退出遗留的隐藏状态，避免用户卡在「没有任务栏」。
            #[cfg(windows)]
            {
                if let Ok(dir) = app.path().app_config_dir() {
                    winapi::taskbar::set_config_dir(dir);
                }
                winapi::taskbar::recover_stale_lock();
            }

            // 创建静态窗口 + 每块显示器的顶栏/Dock（多屏编排集中在 surfaces 模块）
            surfaces::init(app.handle())?;

            // 工作区接管参数：隐藏任务栏后按主屏顶栏物理高度预留顶部空间
            #[cfg(windows)]
            {
                winapi::taskbar::update_topbar_height(app.handle());
            }

            // 托盘
            tray::create(app)?;

            // 全局热键：Alt+F1 概览 + Alt+Space 应用中心（失败只记录日志，不阻断启动）
            for warning in register_hotkeys(app.handle(), &cfg) {
                eprintln!("[easy-dash] {warning}");
            }

            // 后台轮询：窗口列表 + 全屏检测
            monitor::spawn(app.handle().clone());
            // Dock 智能隐藏：独立轻量线程做底边唤出/遮挡判定（80ms 级）
            dock_autohide::spawn(app.handle().clone());
            // 图标工作线程：隔离可能阻塞的 Shell COM 调用
            worker::spawn(app.handle().clone());
            // 顶栏任务托盘：优先启动托盘代理窗口（不显示/不点击系统任务栏）；
            // 失败时回退到旧的 UIA 镜像实现。
            match tray_proxy::start(app.handle().clone()) {
                Ok(()) => {
                    eprintln!("[easy-dash] 托盘代理已启动");
                }
                Err(e) => {
                    eprintln!("[easy-dash] 托盘代理启动失败，回退 UIA 镜像：{e}");
                    tray_mirror::spawn(app.handle().clone());
                }
            }
            notifications::spawn(app.handle().clone());
            // 应用中心数据线程（启动即后台枚举，首次打开应用中心无需等待）
            apps::spawn(app.handle().clone());
            apps::request_refresh(app.handle());
            if cfg.tray_icons {
                tray_proxy::request_refresh_or_fallback(app.handle());
            }
            if cfg.notification_center {
                notifications::request_refresh(app.handle());
            }

            // setup 末尾：按配置应用任务栏隐藏。
            // 失败只记录日志、不阻断启动：watchdog 会在任务栏窗口就绪后重试。
            #[cfg(windows)]
            {
                if let Err(e) = winapi::taskbar::apply(cfg.hide_taskbar) {
                    eprintln!("[easy-dash] 任务栏隐藏应用失败（稍后由看门狗重试）：{e}");
                }
                // 工作区可能已被接管/还原：Dock 需要重新贴近新的工作区底边
                // （前端 syncSize 只在内容尺寸变化时才纠正，启动瞬间不会再调）
                surfaces::reposition_all_docks(app.handle());
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            if let tauri::RunEvent::Exit = event {
                #[cfg(windows)]
                {
                    // 先销毁托盘代理并广播 TaskbarCreated，让应用图标重新注册到真实托盘，
                    // 再恢复任务栏；顺序反了会让原生托盘短暂缺图标。
                    tray_proxy::shutdown();
                    // 退出必须恢复任务栏（否则用户桌面会永久缺少任务栏）
                    winapi::taskbar::restore_all();
                    winapi::appbar::unregister_all();
                    // 桌面图标视图移回整屏（顶栏避让的下移量归零）
                    winapi::desktop::restore();
                }
            }
        });
}


/// 注册全局热键：`hotkey` = 活动概览，`appCenterHotkey` = 应用中心。
/// 返回注册失败/冲突的警告文本（调用方决定日志或展示），不阻断应用启动。
pub(crate) fn register_hotkeys(app: &tauri::AppHandle, cfg: &config::Config) -> Vec<String> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
    let gs = app.global_shortcut();    let _ = gs.unregister_all();
    let mut warnings = Vec::new();

    let overview_hotkey = cfg.hotkey.clone();
    if let Err(e) = gs.on_shortcut(overview_hotkey.as_str(), |app, _s, event| {
        if event.state() == ShortcutState::Pressed {
            overview::toggle_at_cursor(app);
        }
    }) {
        warnings.push(format!(
            "活动概览热键注册失败（{overview_hotkey}）：{e}"
        ));
    }

    let app_center_hotkey = cfg.app_center_hotkey.clone();
    if let Err(e) = gs.on_shortcut(app_center_hotkey.as_str(), |app, _s, event| {
        if event.state() == ShortcutState::Pressed {
            app_center::toggle_at_cursor(app);
        }
    }) {
        warnings.push(format!(
            "应用中心热键注册失败（{app_center_hotkey}）：{e}"
        ));
    }
    warnings
}

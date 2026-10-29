//! 应用中心窗口：显示/隐藏/切换（类似 GNOME 应用网格）。
//! 多屏：只在触发操作的那块显示器打开（Dock 九点格 = Dock 所在屏，热键 = 鼠标所在屏）。
use std::sync::atomic::Ordering;

use tauri::{AppHandle, Emitter, Manager};

use crate::app_state::AppState;
use crate::surfaces;

pub fn is_open(app: &AppHandle) -> bool {
    app.state::<AppState>()
        .app_center_open
        .load(Ordering::Relaxed)
}

pub fn toggle_at(app: &AppHandle, target: tauri::Monitor) {
    if is_open(app) {
        hide(app);
    } else {
        show_at(app, target);
    }
}

pub fn toggle_at_cursor(app: &AppHandle) {
    if is_open(app) {
        hide(app);
        return;
    }
    if let Some(monitor) = surfaces::cursor_monitor(app) {
        show_at(app, monitor);
    }
}

pub fn show_at(app: &AppHandle, target: tauri::Monitor) {
    // 与活动概览互斥：两个全屏层不叠加
    if crate::overview::is_open(app) {
        crate::overview::hide(app);
    }
    // 顶栏面板（通知中心/控制中心）会浮在应用中心之上，先收起
    let _ = app.emit("close-panels", ());

    app.state::<AppState>()
        .app_center_open
        .store(true, Ordering::Relaxed);

    if let Some(win) = app.get_webview_window(surfaces::LAUNCHER_LABEL) {
        surfaces::place_overlay(&win, &target);
        // 显示前抓桌面：应用中心的窗口级玻璃底片
        let backdrop = surfaces::capture_surface_backdrop(&win, true);
        let _ = win.show();
        // 隐藏窗口的尺寸/位置设置在某些时序下会被 show 覆盖：显示后再同步一次
        surfaces::place_overlay(&win, &target);
        let _ = win.set_focus();
        surfaces::raise_all_visible(app);
        surfaces::publish_surface_backdrop(app, surfaces::LAUNCHER_LABEL, backdrop);
        let _ = app.emit_to(surfaces::LAUNCHER_LABEL, "app-center-open", ());
    }
}

pub fn hide(app: &AppHandle) {
    if !is_open(app) {
        return;
    }
    app.state::<AppState>()
        .app_center_open
        .store(false, Ordering::Relaxed);
    if let Some(win) = app.get_webview_window(surfaces::LAUNCHER_LABEL) {
        let _ = win.hide();
        // 隐藏后把页面退回「关闭态」（遮罩 opacity 0）：下次 show 的第一帧就是透明的，
        // 不会先闪一下上一次打开的完整界面。前端在 surface-open.js 监听该事件。
        let _ = app.emit_to(surfaces::LAUNCHER_LABEL, "surface-closed", ());
    }
}

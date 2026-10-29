//! 管理面板窗口：显示/隐藏/切换（独立居中窗口）。
//! 多屏：在触发操作（顶栏齿轮）所在显示器工作区居中打开。
use tauri::{AppHandle, Emitter, Manager};

use crate::surfaces;

pub fn is_open(app: &AppHandle) -> bool {
    app.get_webview_window(surfaces::MANAGER_LABEL)
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false)
}

pub fn toggle_at(app: &AppHandle, target: tauri::Monitor) {
    if is_open(app) {
        hide(app);
    } else {
        show_at(app, target);
    }
}

pub fn show_at(app: &AppHandle, target: tauri::Monitor) {
    // 管理面板打开时收起其它全屏/浮层，避免层叠
    if crate::overview::is_open(app) {
        crate::overview::hide(app);
    }
    if crate::app_center::is_open(app) {
        crate::app_center::hide(app);
    }
    let _ = app.emit("close-panels", ());

    if let Some(win) = app.get_webview_window(surfaces::MANAGER_LABEL) {
        surfaces::place_manager(&win, &target);
        // 显示前抓桌面：管理面板的窗口级玻璃底片
        let backdrop = surfaces::capture_surface_backdrop(&win, true);
        let _ = win.show();
        surfaces::place_manager(&win, &target);
        let _ = win.set_focus();
        surfaces::publish_surface_backdrop(app, surfaces::MANAGER_LABEL, backdrop);
        let _ = app.emit_to(surfaces::MANAGER_LABEL, "manager-open", ());
    }
}

pub fn hide(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(surfaces::MANAGER_LABEL) {
        let _ = win.hide();
    }
}

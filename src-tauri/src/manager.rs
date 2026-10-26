//! 管理面板窗口：显示/隐藏/切换（独立居中窗口）。
use tauri::{AppHandle, Emitter, Manager};

pub fn is_open(app: &AppHandle) -> bool {
    app.get_webview_window("manager")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false)
}

pub fn toggle(app: &AppHandle) {
    if is_open(app) {
        hide(app);
    } else {
        show(app);
    }
}

pub fn show(app: &AppHandle) {
    // 管理面板打开时收起其它全屏/浮层，避免层叠
    if crate::overview::is_open(app) {
        crate::overview::hide(app);
    }
    if crate::app_center::is_open(app) {
        crate::app_center::hide(app);
    }
    let _ = app.emit_to("topbar", "close-panels", ());

    if let Some(win) = app.get_webview_window("manager") {
        let _ = win.show();
        let _ = win.set_focus();
        let _ = app.emit_to("manager", "manager-open", ());
    }
}

pub fn hide(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("manager") {
        let _ = win.hide();
    }
}

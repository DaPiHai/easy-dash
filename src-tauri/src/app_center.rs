//! 应用中心窗口：显示/隐藏/切换（类似 GNOME 应用网格）。
use std::sync::atomic::Ordering;

use tauri::{AppHandle, Emitter, Manager};

use crate::app_state::AppState;
use crate::winapi;

pub fn is_open(app: &AppHandle) -> bool {
    app.state::<AppState>()
        .app_center_open
        .load(Ordering::Relaxed)
}

pub fn toggle(app: &AppHandle) {
    if is_open(app) {
        hide(app);
    } else {
        show(app);
    }
}

pub fn show(app: &AppHandle) {
    // 与活动概览互斥：两个全屏层不叠加
    if crate::overview::is_open(app) {
        crate::overview::hide(app);
    }
    // 顶栏面板（通知中心/控制中心）会浮在应用中心之上，先收起
    let _ = app.emit_to("topbar", "close-panels", ());

    app.state::<AppState>()
        .app_center_open
        .store(true, Ordering::Relaxed);

    if let Some(win) = app.get_webview_window("launcher") {
        let _ = win.show();
        let _ = win.set_focus();
        fix_z_order(app);
        let _ = app.emit_to("launcher", "app-center-open", ());
    }
}

pub fn hide(app: &AppHandle) {
    if !is_open(app) {
        return;
    }
    app.state::<AppState>()
        .app_center_open
        .store(false, Ordering::Relaxed);
    if let Some(win) = app.get_webview_window("launcher") {
        let _ = win.hide();
    }
}

/// 应用中心显示后，把顶栏和 Dock 重新置于 topmost 顶端（GNOME 同款：
/// 应用网格不遮挡顶栏和 Dash）。
fn fix_z_order(app: &AppHandle) {
    for label in ["topbar", "dock"] {
        if let Some(w) = app.get_webview_window(label) {
            if let Some(h) = winapi::common::tauri_hwnd(&w) {
                winapi::common::raise_topmost(h);
            }
        }
    }
}

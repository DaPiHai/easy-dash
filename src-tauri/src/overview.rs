//! 活动概览：打开/关闭/切换，打开时抓取全部窗口快照。
//! 多屏：只在触发操作的那块显示器打开（点「活动」= 该顶栏所在屏，热键 = 鼠标所在屏）。
use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::app_state::AppState;
use crate::surfaces;
use crate::winapi;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewItem {
    pub id: i64,
    pub title: String,
    pub exe: String,
    pub icon: Option<String>,
    pub image: Option<String>,
}

pub fn is_open(app: &AppHandle) -> bool {
    app.state::<AppState>().overview_open.load(Ordering::Relaxed)
}

/// 切换概览：`target` 为触发操作所在显示器。
pub fn toggle_at(app: &AppHandle, target: tauri::Monitor) {
    if is_open(app) {
        hide(app);
    } else {
        show_at(app, target);
    }
}

/// 热键/托盘菜单入口：按鼠标所在显示器打开。
pub fn toggle_at_cursor(app: &AppHandle) {
    if is_open(app) {
        hide(app);
        return;
    }
    if let Some(monitor) = surfaces::cursor_monitor(app) {
        show_at(app, monitor);
    }
}

/// 托盘菜单「打开活动概览」：按鼠标所在显示器打开。
pub fn show_at_cursor(app: &AppHandle) {
    if let Some(monitor) = surfaces::cursor_monitor(app) {
        show_at(app, monitor);
    }
}

pub fn show_at(app: &AppHandle, target: tauri::Monitor) {
    // 与全屏应用中心互斥：先收起应用中心
    crate::app_center::hide(app);
    // 顶栏面板（通知中心/控制中心）先收起：概览打开时面板会浮在概览之上
    let _ = app.emit("close-panels", ());
    let state = app.state::<AppState>();
    let wins = winapi::enumerate::list_windows(std::process::id(), &state);
    // 概览截图缓存：最小化窗口 PrintWindow 通常取不到画面，
    // 回退到最后一次可见时缓存的截图，避免只剩图标卡片
    let mut snapshots = state.snapshots.lock().unwrap();
    let live: Vec<isize> = wins.iter().map(|w| w.id as isize).collect();
    snapshots.retain(|k, _| live.contains(k));
    let items: Vec<OverviewItem> = wins
        .iter()
        .map(|w| {
            let id64 = w.id;
            let id = id64 as isize;
            let image = if w.minimized {
                snapshots.get(&id).cloned().or_else(|| {
                    winapi::snapshot::capture_jpeg(
                        windows::Win32::Foundation::HWND(id as usize as *mut _),
                        640,
                    )
                })
            } else {
                let fresh = winapi::snapshot::capture_jpeg(
                    windows::Win32::Foundation::HWND(id as usize as *mut _),
                    640,
                );
                if let Some(img) = &fresh {
                    snapshots.insert(id, img.clone());
                }
                fresh
            };
            OverviewItem {
                id: id64,
                title: w.title.clone(),
                exe: w.exe.clone(),
                icon: w.icon.clone(),
                image,
            }
        })
        .collect();
    drop(snapshots);

    state.overview_open.store(true, Ordering::Relaxed);
    if let Some(ov) = app.get_webview_window(surfaces::OVERVIEW_LABEL) {
        surfaces::place_overlay(&ov, &target);
        // 显示前抓桌面：概览的窗口级玻璃底片
        let backdrop = surfaces::capture_surface_backdrop(&ov, true);
        let _ = ov.show();
        // 隐藏窗口的尺寸/位置设置在某些时序下会被 show 覆盖：显示后再同步一次
        surfaces::place_overlay(&ov, &target);
        let _ = ov.set_focus();
        surfaces::raise_all_visible(app);
        surfaces::publish_surface_backdrop(app, surfaces::OVERVIEW_LABEL, backdrop);
        let _ = app.emit_to(surfaces::OVERVIEW_LABEL, "overview-data", &items);
    }
}

pub fn hide(app: &AppHandle) {
    app.state::<AppState>()
        .overview_open
        .store(false, Ordering::Relaxed);
    if let Some(ov) = app.get_webview_window(surfaces::OVERVIEW_LABEL) {
        let _ = ov.hide();
        // 隐藏后把页面退回「关闭态」（遮罩 opacity 0）：下次 show 的第一帧就是透明的，
        // 不会先闪一下上一次打开的完整界面。前端在 surface-open.js 监听该事件。
        let _ = app.emit_to(surfaces::OVERVIEW_LABEL, "surface-closed", ());
    }
}

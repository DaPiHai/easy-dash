//! 活动概览：打开/关闭/切换，打开时抓取全部窗口快照
use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::app_state::AppState;
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

pub fn toggle(app: &AppHandle) {
    if is_open(app) {
        hide(app);
    } else {
        show(app);
    }
}

pub fn show(app: &AppHandle) {
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
    if let Some(ov) = app.get_webview_window("overview") {
        let _ = ov.show();
        let _ = ov.set_focus();
        fix_z_order(app);
        let _ = app.emit_to("overview", "overview-data", &items);
    }
}

pub fn hide(app: &AppHandle) {
    app.state::<AppState>()
        .overview_open
        .store(false, Ordering::Relaxed);
    if let Some(ov) = app.get_webview_window("overview") {
        let _ = ov.hide();
    }
}

/// 概览显示后，把顶栏和 Dock 重新置于 topmost 顶端（GNOME 同款：概览不遮挡顶栏和 Dash）
fn fix_z_order(app: &AppHandle) {
    for label in ["topbar", "dock"] {
        if let Some(w) = app.get_webview_window(label) {
            if let Some(h) = winapi::common::tauri_hwnd(&w) {
                winapi::common::raise_topmost(h);
            }
        }
    }
}

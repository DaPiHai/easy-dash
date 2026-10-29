//! 顶栏 AppBar 注册（多显示器版）：让最大化窗口自动避开每块屏的顶栏高度。
//!
//! - 每块显示器上的顶栏都是独立的 AppBar（`ABM_NEW` / `ABM_SETPOS`），互不影响；
//! - 注册带使用该显示器 `rcMonitor` 的绝对屏幕坐标（支持负坐标/偏置布局）；
//! - 任务栏可见且停靠在该屏上/左/右时，顶栏按该屏任务栏矩形避让；
//! - 面板展开（窗口被拉高）期间 `sync_topbar` 让路，不会把面板顶回 32px。
#![allow(non_upper_case_globals)]

use std::sync::Mutex;

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::Shell::{SHAppBarMessage, APPBARDATA};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, IsWindowVisible, SetWindowPos, HWND_TOP, HWND_TOPMOST, SWP_NOACTIVATE,
    SWP_SHOWWINDOW,
};

use super::common::{tauri_hwnd, TOPBAR_LOGICAL_HEIGHT};
use super::monitors;

const ABM_NEW: u32 = 0x0;
const ABM_REMOVE: u32 = 0x1;
const ABM_QUERYPOS: u32 = 0x2;
const ABM_SETPOS: u32 = 0x3;
const ABE_TOP: u32 = 1;
const APPBAR_CALLBACK: u32 = 0x8000 + 1; // WM_APP + 1

/// 已注册 AppBar 的窗口（退出/注销时逐个 ABM_REMOVE）
static REGISTERED: Mutex<Vec<isize>> = Mutex::new(Vec::new());

fn is_registered(hwnd: HWND) -> bool {
    REGISTERED
        .lock()
        .map(|g| g.contains(&(hwnd.0 as isize)))
        .unwrap_or(false)
}

fn mark_registered(hwnd: HWND) {
    if let Ok(mut guard) = REGISTERED.lock() {
        let v = hwnd.0 as isize;
        if !guard.contains(&v) {
            guard.push(v);
        }
    }
}

fn mark_unregistered(hwnd: HWND) {
    if let Ok(mut guard) = REGISTERED.lock() {
        guard.retain(|h| *h != hwnd.0 as isize);
    }
}

/// 该顶栏在所在显示器上的目标矩形（物理像素）。
/// 只处理任务栏停靠在上/左/右的情况；停靠底部时顶栏就是显示器顶部满宽。
fn desired_rect(hwnd: HWND) -> Option<RECT> {
    let mon = monitors::from_window(hwnd)?;
    let m = mon.monitor;
    let scale = monitors::dpi_scale(hwnd);
    let height = ((TOPBAR_LOGICAL_HEIGHT as f64) * scale).round().max(1.0) as i32;
    let mon_w = m.right - m.left;
    let mon_h = m.bottom - m.top;
    let (mut x, mut y, mut w) = (m.left, m.top, mon_w);

    // 任务栏被 easy-dash 隐藏时不参与避让（否则会按旧矩形留下空白）
    if !super::taskbar::is_hidden() {
        if let Some(tr) = monitors::taskbar_rect_on(&mon) {
            let tr_w = tr.right - tr.left;
            let tr_h = tr.bottom - tr.top;
            let at_top = tr.top <= m.top && tr.bottom < m.top + mon_h / 2;
            let at_left = tr.left <= m.left && tr.right < m.left + mon_w / 2;
            let at_right = tr.right >= m.right && tr.left > m.left + mon_w / 2;
            if at_top {
                y = tr.bottom;
            } else if at_left && tr_h > mon_h / 2 {
                x = tr.right;
                w = (m.right - x).max(1);
            } else if at_right && tr_h > mon_h / 2 {
                w = (tr.left - m.left).max(1);
            } else if tr_w > mon_w / 2 && tr_h < mon_h / 2 {
                // 底部任务栏：顶栏保持顶部，无需处理
            }
        }
    }

    let height = height.min(mon_h.max(1));
    Some(RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + height,
    })
}

/// 注册顶栏为 AppBar 并按目标矩形摆放。
pub fn register_topbar(win: &tauri::WebviewWindow) {
    let Some(hwnd) = tauri_hwnd(win) else {
        return;
    };
    let Some(target) = desired_rect(hwnd) else {
        return;
    };
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            HWND_TOP,
            target.left,
            target.top,
            target.right - target.left,
            target.bottom - target.top,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );

        let mut data = APPBARDATA::default();
        data.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
        data.hWnd = hwnd;
        data.uCallbackMessage = APPBAR_CALLBACK;
        SHAppBarMessage(ABM_NEW, &mut data);

        let mut pos = data;
        pos.uEdge = ABE_TOP;
        pos.rc = target;
        SHAppBarMessage(ABM_QUERYPOS, &mut pos);
        // 顶边 AppBar：强制使用我们算好的显示器内矩形（避免被旧条带推走）
        pos.rc = target;
        SHAppBarMessage(ABM_SETPOS, &mut pos);

        // 副屏 AppBar 新增后，已最大化窗口不一定会自己重排（Chrome/Edge 常见）
        if let Some(mon) = super::monitors::from_window(hwnd) {
            super::monitors::refit_maximized_on(&mon);
        }

        mark_registered(hwnd);
    }
}

/// 注销单个顶栏的 AppBar（隐藏/拔掉显示器时调用）。
pub fn unregister_topbar(win: &tauri::WebviewWindow) {
    let Some(hwnd) = tauri_hwnd(win) else {
        return;
    };
    unregister_hwnd(hwnd);
}

fn unregister_hwnd(hwnd: HWND) {
    unsafe {
        let mut data = APPBARDATA::default();
        data.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
        data.hWnd = hwnd;
        data.uCallbackMessage = APPBAR_CALLBACK;
        data.lParam = LPARAM(0);
        SHAppBarMessage(ABM_REMOVE, &mut data);
    }
    mark_unregistered(hwnd);
}

/// 退出时注销全部 AppBar。
pub fn unregister_all() {
    let handles: Vec<isize> = REGISTERED
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    for raw in handles {
        unregister_hwnd(HWND(raw as *mut _));
    }
}

/// 周期校正：任务栏移动 / 显示器布局变化后把顶栏摆回目标位置。
/// 面板展开时窗口高度 > 顶栏高度，直接让路。
pub fn sync_topbar(win: &tauri::WebviewWindow) {
    let Some(hwnd) = tauri_hwnd(win) else {
        return;
    };
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return;
    }
    let scale = monitors::dpi_scale(hwnd);
    let bar_h = ((TOPBAR_LOGICAL_HEIGHT as f64) * scale).round().max(1.0) as i32;

    let mut cur = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut cur) }.is_err() {
        return;
    }
    if cur.bottom - cur.top > bar_h + 2 {
        return; // 面板展开
    }
    let Some(target) = desired_rect(hwnd) else {
        return;
    };
    if cur.left == target.left
        && cur.top == target.top
        && cur.right - cur.left == target.right - target.left
        && cur.bottom - cur.top == target.bottom - target.top
    {
        return;
    }
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            target.left,
            target.top,
            target.right - target.left,
            target.bottom - target.top,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        if is_registered(hwnd) {
            let mut pos = APPBARDATA::default();
            pos.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
            pos.hWnd = hwnd;
            pos.uEdge = ABE_TOP;
            pos.rc = target;
            SHAppBarMessage(ABM_SETPOS, &mut pos);
        }
    }
}

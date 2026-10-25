//! 顶栏 AppBar 注册：让最大化窗口自动避开顶栏高度
#![allow(non_upper_case_globals)]

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::RECT;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{SHAppBarMessage, APPBARDATA};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetSystemMetrics, GetWindowRect, SetWindowPos, HWND_TOP, HWND_TOPMOST,
    SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_SHOWWINDOW,
};

use super::common::{tauri_hwnd, to_wide};

const ABM_NEW: u32 = 0x0;
const ABM_REMOVE: u32 = 0x1;
const ABM_QUERYPOS: u32 = 0x2;
const ABM_SETPOS: u32 = 0x3;
const ABE_TOP: u32 = 1;
const APPBAR_CALLBACK: u32 = 0x8000 + 1; // WM_APP + 1

static TOPBAR_HWND: AtomicIsize = AtomicIsize::new(0);

/// 注册顶栏为 AppBar 并按系统返回的工作区摆放
pub fn register_topbar(win: &tauri::WebviewWindow) {
    let Some(hwnd) = tauri_hwnd(win) else {
        return;
    };
    unsafe {
        let scale = GetDpiForWindow(hwnd) as f64 / 96.0;
        let height = (32.0 * scale).round() as i32;
        let width = GetSystemMetrics(SM_CXSCREEN);

        let mut data = APPBARDATA::default();
        data.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
        data.hWnd = hwnd;
        data.uCallbackMessage = APPBAR_CALLBACK;
        SHAppBarMessage(ABM_NEW, &mut data);

        let mut pos = data;
        pos.uEdge = ABE_TOP;
        pos.rc = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        SHAppBarMessage(ABM_QUERYPOS, &mut pos);
        // 对顶边 AppBar，强制对齐屏幕顶部
        pos.rc.left = 0;
        pos.rc.top = 0;
        pos.rc.right = width;
        pos.rc.bottom = height;
        SHAppBarMessage(ABM_SETPOS, &mut pos);

        let _ = SetWindowPos(
            hwnd,
            HWND_TOP,
            pos.rc.left,
            pos.rc.top,
            pos.rc.right - pos.rc.left,
            pos.rc.bottom - pos.rc.top,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
    TOPBAR_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
}

/// 退出时注销 AppBar
pub fn unregister_all() {
    let hwnd = TOPBAR_HWND.swap(0, Ordering::SeqCst);
    if hwnd == 0 {
        return;
    }
    unsafe {
        let mut data = APPBARDATA::default();
        data.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
        data.hWnd = HWND(hwnd as *mut _);
        data.uCallbackMessage = APPBAR_CALLBACK;
        data.lParam = LPARAM(0);
        SHAppBarMessage(ABM_REMOVE, &mut data);
    }
}

use windows::Win32::Foundation::{HWND, LPARAM};

/// 周期校正顶栏位置：任务栏被移动（顶部/左侧/右侧）后自动避让，
/// 回到底部则恢复 (0,0)。期望位置与实际不符时重摆窗口并重申 AppBar 带。
pub fn sync_topbar(win: &tauri::WebviewWindow) {
    let Some(hwnd) = tauri_hwnd(win) else {
        return;
    };
    unsafe {
        let sw = GetSystemMetrics(SM_CXSCREEN);
        let sh = GetSystemMetrics(SM_CYSCREEN);

        // 原生任务栏（Shell_TrayWnd）位置
        let tray = FindWindowW(PCWSTR(to_wide("Shell_TrayWnd").as_ptr()), None);
        let mut tr = RECT::default();
        let has_tray = match tray {
            Ok(h) if !h.is_invalid() => GetWindowRect(h, &mut tr).is_ok(),
            _ => false,
        };

        let (mut x, mut y, mut w) = (0i32, 0i32, sw);
        if has_tray {
            if tr.left <= 0 && tr.top <= 0 && tr.bottom < 300 && tr.right >= sw {
                y = tr.bottom; // 任务栏在顶部 → 顶栏排到其下方
            } else if tr.left <= 0 && tr.top <= 0 && tr.right < 300 && tr.bottom >= sh {
                x = tr.right; // 任务栏在左侧 → 顶栏右移
                w = sw - tr.right;
            } else if tr.right >= sw && tr.left > sw - 300 && tr.bottom >= sh {
                w = tr.left; // 任务栏在右侧 → 顶栏宽度收缩
            }
        }

        let scale = GetDpiForWindow(hwnd) as f64 / 96.0;
        let h = (32.0 * scale).round() as i32;

        let mut cur = RECT::default();
        if GetWindowRect(hwnd, &mut cur).is_err() {
            return;
        }
        if cur.left != x
            || cur.top != y
            || cur.right - cur.left != w
            || cur.bottom - cur.top != h
        {
            let _ = SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                x,
                y,
                w,
                h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            // 重申 AppBar 占用的横带，让最大化窗口避开新位置
            let mut pos = APPBARDATA::default();
            pos.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
            pos.hWnd = hwnd;
            pos.uEdge = ABE_TOP;
            pos.rc = RECT {
                left: x,
                top: y,
                right: x + w,
                bottom: y + h,
            };
            SHAppBarMessage(ABM_SETPOS, &mut pos);
        }
    }
}

use windows::core::PCWSTR;

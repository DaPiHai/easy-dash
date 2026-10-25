//! 全屏前台应用检测（用于自动隐藏顶栏/Dock）
#![allow(non_upper_case_globals)]

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetSystemMetrics, GetWindowLongPtrW, IsWindowVisible, GWL_STYLE,
    SM_CXSCREEN, SM_CYSCREEN, WS_CAPTION,
};

use super::common;

/// 前台窗口句柄 id（用于 Dock 指示点）
pub fn foreground_id() -> isize {
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        0
    } else {
        fg.0 as isize
    }
}

/// 前台窗口是否为全屏（铺满主显示器且无边框标题栏）
pub fn is_foreground_fullscreen(own_hwnds: &[isize]) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() || fg.0 as isize == 0 {
            return false;
        }
        if own_hwnds.contains(&(fg.0 as isize)) {
            return false;
        }
        if !IsWindowVisible(fg).as_bool() {
            return false;
        }
        let class = common::class_name(fg);
        if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd") {
            return false;
        }
        // 有标题栏的窗口不算全屏
        if GetWindowLongPtrW(fg, GWL_STYLE) & WS_CAPTION.0 as isize != 0 {
            return false;
        }
        let mut rect = RECT::default();
        let hr = DwmGetWindowAttribute(
            fg,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut _ as *mut _,
            std::mem::size_of::<RECT>() as u32,
        );
        if hr.is_err() {
            return false;
        }
        let w = GetSystemMetrics(SM_CXSCREEN);
        let h = GetSystemMetrics(SM_CYSCREEN);
        rect.left <= 0 && rect.top <= 0 && rect.right >= w && rect.bottom >= h
    }
}

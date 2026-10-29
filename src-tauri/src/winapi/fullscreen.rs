//! 全屏前台应用检测（用于按显示器自动隐藏顶栏/Dock）
#![allow(non_upper_case_globals)]

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsWindowVisible};

use super::{common, monitors};

/// 前台窗口句柄 id（用于 Dock 指示点）
pub fn foreground_id() -> isize {
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        0
    } else {
        fg.0 as isize
    }
}

/// 矩形是否完全覆盖显示器（允许 2px 误差）。
pub(crate) fn covers_monitor(rect: &RECT, monitor: &RECT) -> bool {
    let tol = 2;
    rect.left <= monitor.left + tol
        && rect.top <= monitor.top + tol
        && rect.right >= monitor.right - tol
        && rect.bottom >= monitor.bottom - tol
}

/// 前台窗口是否全屏（铺满它所在的显示器且无边框标题栏）。
/// 返回该显示器 device key（如 `\\.\DISPLAY1`），None = 非全屏/无法判断。
pub fn foreground_fullscreen_monitor(own_hwnds: &[isize]) -> Option<String> {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() || fg.0 as isize == 0 {
            return None;
        }
        if own_hwnds.contains(&(fg.0 as isize)) {
            return None;
        }
        if !IsWindowVisible(fg).as_bool() {
            return None;
        }
        let class = common::class_name(fg);
        if matches!(
            class.as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
        ) {
            return None;
        }
        // 注意：不能只凭 WS_CAPTION 排除——Chrome/Edge 的 F11 全屏仍带
        // WS_CAPTION（标题栏由 DWM/自绘隐藏），旧判断会漏掉浏览器全屏，
        // 顶栏就压在全屏视频顶部。改为只认「前台窗口完整覆盖该显示器」：
        // 最大化窗口会被顶栏 AppBar 限制在工作区内，不会覆盖顶部预留区。
        let mut rect = RECT::default();
        let hr = DwmGetWindowAttribute(
            fg,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut _ as *mut _,
            std::mem::size_of::<RECT>() as u32,
        );
        if hr.is_err() {
            return None;
        }
        let monitor = monitors::from_window(fg)?;
        if !covers_monitor(&rect, &monitor.monitor) {
            return None;
        }
        monitors::device_name(fg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
        RECT {
            left: l,
            top: t,
            right: r,
            bottom: b,
        }
    }

    #[test]
    fn exact_monitor_coverage_is_fullscreen() {
        let m = rect(0, 0, 1920, 1080);
        assert!(covers_monitor(&rect(0, 0, 1920, 1080), &m));
        assert!(covers_monitor(&rect(-2, -2, 1922, 1082), &m));
    }

    #[test]
    fn smaller_or_offset_rect_is_not_fullscreen() {
        let m = rect(2400, 1835, 5280, 3455);
        assert!(!covers_monitor(&rect(2400, 1835, 5280, 3383), &m));
        assert!(!covers_monitor(&rect(0, 0, 1920, 1080), &m));
        assert!(!covers_monitor(&rect(2400, 1835, 5270, 3455), &m));
    }

    #[test]
    fn negative_origin_monitor_supported() {
        let m = rect(-1920, 0, 0, 1080);
        assert!(covers_monitor(&rect(-1920, 0, 0, 1080), &m));
        assert!(!covers_monitor(&rect(0, 0, 1920, 1080), &m));
    }
}

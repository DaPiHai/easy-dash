//! 显示器 / 任务栏几何辅助（物理像素）。
//!
//! 多屏适配里所有窗口位置都用屏幕物理坐标；本模块只负责把 Win32 的
//! `HMONITOR` / `MONITORINFOEXW` 与任务栏窗口矩形整理成简单类型。
#![allow(non_upper_case_globals)]

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITORINFO, MONITORINFOEXW, MonitorFromPoint, MonitorFromRect,
    MonitorFromWindow, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowRect, IsWindowVisible, IsZoomed, SetWindowPos, HWND_TOP, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER,
};

use super::common;

/// 一块显示器的屏幕矩形（物理像素）
#[derive(Debug, Clone, Copy, Default)]
pub struct MonitorRect {
    pub monitor: RECT,
}

fn info_from_hmonitor(hmon: HMONITOR) -> Option<MonitorRect> {
    if hmon.0.is_null() {
        return None;
    }
    let mut mi = MONITORINFOEXW::default();
    mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    let ok = unsafe { GetMonitorInfoW(hmon, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO) };
    if !ok.as_bool() {
        return None;
    }
    Some(MonitorRect {
        monitor: mi.monitorInfo.rcMonitor,
    })
}

fn device_from_hmonitor(hmon: HMONITOR) -> Option<String> {
    if hmon.0.is_null() {
        return None;
    }
    let mut mi = MONITORINFOEXW::default();
    mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    let ok = unsafe { GetMonitorInfoW(hmon, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO) };
    if !ok.as_bool() {
        return None;
    }
    let name = String::from_utf16_lossy(&mi.szDevice);
    let name = name.trim_matches(char::from(0)).trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// 窗口所属显示器
pub fn from_window(hwnd: HWND) -> Option<MonitorRect> {
    let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    info_from_hmonitor(hmon)
}

/// 点所在显示器（物理坐标）
pub fn from_point(x: i32, y: i32) -> Option<MonitorRect> {
    let hmon = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
    info_from_hmonitor(hmon)
}

/// 窗口所属显示器 device 名（`\\.\DISPLAY1`），用于与 Tauri Monitor 对齐。
pub fn device_name(hwnd: HWND) -> Option<String> {
    let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    device_from_hmonitor(hmon)
}

/// 点所在显示器 device 名。
pub fn device_name_at_point(x: i32, y: i32) -> Option<String> {
    let hmon = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
    device_from_hmonitor(hmon)
}

struct TaskbarCtx {
    target: MonitorRect,
    found: Option<RECT>,
}

unsafe extern "system" fn taskbar_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut TaskbarCtx);
    if ctx.found.is_some() {
        return true.into();
    }
    let class = common::class_name(hwnd);
    if class != "Shell_TrayWnd" && class != "Shell_SecondaryTrayWnd" {
        return true.into();
    }
    // 自身托盘代理窗口也是 Shell_TrayWnd（0×0，置顶），必须排除
    if common::window_pid(hwnd) == std::process::id() {
        return true.into();
    }
    if !IsWindowVisible(hwnd).as_bool() {
        return true.into();
    }
    let mut rect = RECT::default();
    if GetWindowRect(hwnd, &mut rect).is_err() {
        return true.into();
    }
    if rect.right - rect.left <= 0 || rect.bottom - rect.top <= 0 {
        return true.into();
    }
    let cx = (rect.left + rect.right) / 2;
    let cy = (rect.top + rect.bottom) / 2;
    let m = &ctx.target.monitor;
    if cx >= m.left && cx < m.right && cy >= m.top && cy < m.bottom {
        ctx.found = Some(rect);
    }
    true.into()
}

/// 窗口所在显示器的任务栏矩形（仅当前显示器；隐藏/零尺寸/自身代理窗口不返回）。
pub fn taskbar_rect_on(monitor: &MonitorRect) -> Option<RECT> {
    let mut ctx = TaskbarCtx {
        target: *monitor,
        found: None,
    };
    unsafe {
        let _ = EnumWindows(
            Some(taskbar_cb),
            LPARAM(&mut ctx as *mut TaskbarCtx as isize),
        );
    }
    ctx.found
}

/// 指定显示器的工作区（物理像素）。
pub fn work_area_on(monitor: &MonitorRect) -> Option<RECT> {
    let hmon = unsafe { MonitorFromRect(&monitor.monitor, MONITOR_DEFAULTTONEAREST) };
    if hmon.0.is_null() {
        return None;
    }
    let mut mi = MONITORINFOEXW::default();
    mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    let ok = unsafe { GetMonitorInfoW(hmon, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO) };
    ok.as_bool().then_some(mi.monitorInfo.rcWork)
}

struct RefitCtx {
    target: RECT,
    work: RECT,
}

unsafe extern "system" fn refit_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut RefitCtx);
    if common::window_pid(hwnd) == std::process::id() {
        return true.into();
    }
    if !IsWindowVisible(hwnd).as_bool() || !IsZoomed(hwnd).as_bool() {
        return true.into();
    }
    let Some(mon) = from_window(hwnd) else {
        return true.into();
    };
    if mon.monitor != ctx.target {
        return true.into();
    }
    let mut rect = RECT::default();
    if GetWindowRect(hwnd, &mut rect).is_err() {
        return true.into();
    }
    // 已经在目标工作区内（含 1px 误差）就不用动，避免每次配置变更都重排窗口
    if (rect.left - ctx.work.left).abs() <= 1
        && (rect.top - ctx.work.top).abs() <= 1
        && (rect.right - ctx.work.right).abs() <= 1
        && (rect.bottom - ctx.work.bottom).abs() <= 1
    {
        return true.into();
    }
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            HWND_TOP,
            ctx.work.left,
            ctx.work.top,
            ctx.work.right - ctx.work.left,
            ctx.work.bottom - ctx.work.top,
            SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
        );
    }
    true.into()
}

/// 顶栏 AppBar 改变工作区后，强制该屏已最大化的窗口重新贴合工作区。
///
/// Windows 不一定会因为副屏 AppBar 新增而重排已最大化的窗口（Chrome/Edge
/// 尤其明显），表现为顶栏重新开启后仍压在最大化窗口顶部。这里只处理
/// `IsZoomed` 的窗口；真全屏（F11/视频全屏）通常不是最大化状态，会被跳过。
pub fn refit_maximized_on(monitor: &MonitorRect) {
    let Some(work) = work_area_on(monitor) else {
        return;
    };
    if work.right - work.left <= 0 || work.bottom - work.top <= 0 {
        return;
    }
    let mut ctx = RefitCtx {
        target: monitor.monitor,
        work,
    };
    unsafe {
        let _ = EnumWindows(
            Some(refit_cb),
            LPARAM(&mut ctx as *mut RefitCtx as isize),
        );
    }
}

/// 窗口 DPI 缩放（1.0 = 96 DPI）
pub fn dpi_scale(hwnd: HWND) -> f64 {    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 {
        1.0
    } else {
        dpi as f64 / 96.0
    }
}

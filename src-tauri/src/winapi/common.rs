//! 通用 Win32 辅助函数
#![allow(non_upper_case_globals)]

use windows::Win32::Foundation::HWND;
use windows::Win32::Foundation::POINT;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetWindowLongPtrW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId,
    GWL_EXSTYLE, WS_EX_TOOLWINDOW,
};
use windows::core::PWSTR;

/// 顶栏常态高度（逻辑像素），与 commands::BAR_H / panels.js 保持一致
pub const TOPBAR_LOGICAL_HEIGHT: f64 = 32.0;

pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

pub fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

pub fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// 取窗口所属进程的可执行文件完整路径
pub fn process_image_path(hwnd: HWND) -> Option<String> {
    let pid = window_pid(hwnd);
    if pid == 0 {
        return None;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        result.ok()?;
        if len == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

pub fn is_tool_window(hwnd: HWND) -> bool {
    unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) & WS_EX_TOOLWINDOW.0 as isize != 0 }
}

/// 当前进程是否以管理员权限运行
pub fn is_process_elevated() -> bool {
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = windows::Win32::Foundation::HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut ret = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        )
        .is_ok();
        let _ = windows::Win32::Foundation::CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

/// Tauri 窗口 → Win32 HWND
pub fn tauri_hwnd(win: &tauri::WebviewWindow) -> Option<HWND> {
    let h = win.hwnd().ok()?;
    Some(HWND(h.0))
}

/// 直接以物理像素设置窗口外框（同步 SetWindowPos）。
///
/// Tauri/tao 的 `set_size`/`set_position` 走 `SWP_ASYNCWINDOWPOS` + 事件循环消息，
/// 在「隐藏窗口先移动/改尺寸再 show」的时序下可能被 show 抢先生效，表现为窗口
/// 仍停留在初始尺寸（多屏概览/应用中心踩过）。这里直接同步设置物理矩形。
pub fn set_window_rect(win: &tauri::WebviewWindow, x: i32, y: i32, width: u32, height: u32) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, SWP_NOACTIVATE, SWP_NOREPOSITION, SWP_NOZORDER,
    };
    let Some(hwnd) = tauri_hwnd(win) else {
        return;
    };
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            x,
            y,
            width.max(1) as i32,
            height.max(1) as i32,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREPOSITION,
        );
    }
}

/// Tauri 窗口的屏幕物理矩形（`GetWindowRect`，含窗口自身边距）。
/// Dock 智能隐藏用它做遮挡判定与底边唤出命中。
pub fn window_physical_rect(win: &tauri::WebviewWindow) -> Option<RECT> {
    let hwnd = tauri_hwnd(win)?;
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    (rect.right > rect.left && rect.bottom > rect.top).then_some(rect)
}

/// 窗口客户区逻辑像素（CSS px）→ 屏幕物理像素。
/// 多屏/混合 DPI 下不能假设顶栏在 (0,0)，必须用 ClientToScreen 取真实屏幕坐标。
pub fn client_to_screen_logical(
    win: &tauri::WebviewWindow,
    x_logical: f64,
    y_logical: f64,
) -> Option<(i32, i32)> {
    let hwnd = tauri_hwnd(win)?;
    let scale = super::monitors::dpi_scale(hwnd);
    let mut pt = POINT {
        x: (x_logical * scale).round() as i32,
        y: (y_logical * scale).round() as i32,
    };
    let ok = unsafe { ClientToScreen(hwnd, &mut pt) };
    ok.as_bool().then_some((pt.x, pt.y))
}

/// 去掉 Win11 DWM 给所有顶层窗口自动绘制的浅色边框与圆角。
/// 对透明窗口（Dock）而言那层边框看起来就是一块浅色半透明背景。
pub fn remove_win11_chrome(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
    const DWMWA_WINDOW_CORNER_PREFERENCE: i32 = 33;
    const DWMWA_BORDER_COLOR: i32 = 34;
    const DWMWCP_DONOTROUND: u32 = 1;
    const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;
    unsafe {
        let mut corner = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(DWMWA_WINDOW_CORNER_PREFERENCE),
            &mut corner as *mut u32 as *mut _,
            4,
        );
        let mut color = DWMWA_COLOR_NONE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(DWMWA_BORDER_COLOR),
            &mut color as *mut u32 as *mut _,
            4,
        );
    }
}

/// 把窗口重新插到 topmost 顶端（原生任务栏会在某些时机重新置顶自己，
/// 顶栏/Dock 需要周期性抢回层级）
pub fn raise_topmost(hwnd: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    };
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

/// 为当前进程启用某个令牌特权（如 "SeShutdownPrivilege"）。成功返回 true。
///
/// 流程：打开当前进程令牌 → 查特权 LUID → AdjustTokenPrivileges 启用。
/// 注意 AdjustTokenPrivileges 即使“部分成功”（ERROR_NOT_ALL_ASSIGNED）也返回成功，
/// 因此最后必须再查一次 GetLastError。
pub fn enable_privilege(name: &str) -> bool {
    use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
    use windows::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::PCWSTR;

    let wide_name = to_wide(name);
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
        .is_err()
        {
            return false;
        }

        let mut luid = windows::Win32::Foundation::LUID::default();
        if LookupPrivilegeValueW(PCWSTR::null(), PCWSTR(wide_name.as_ptr()), &mut luid).is_err() {
            let _ = CloseHandle(token);
            return false;
        }

        let privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let adjusted = AdjustTokenPrivileges(
            token,
            false,
            Some(&privileges as *const TOKEN_PRIVILEGES),
            0,
            None,
            None,
        );
        // 部分成功（ERROR_NOT_ALL_ASSIGNED）也会返回 Ok，需要再查 GetLastError
        let error = GetLastError();
        let _ = CloseHandle(token);
        adjusted.is_ok() && error.0 == 0
    }
}

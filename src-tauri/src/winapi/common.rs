//! 通用 Win32 辅助函数
#![allow(non_upper_case_globals)]

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetWindowTextW, GetWindowLongPtrW, GetWindowThreadProcessId, GWL_EXSTYLE,
    WS_EX_TOOLWINDOW,
};
use windows::core::PWSTR;

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

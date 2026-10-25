//! 窗口激活（绕过 Windows 前台锁定限制）与最小化
#![allow(non_upper_case_globals)]

use windows::Win32::Foundation::HWND;
use windows::Win32::Foundation::BOOL;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, SetFocus, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VK_MENU,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetForegroundWindow, IsIconic, IsWindow, SetForegroundWindow, ShowWindow,
    SW_MINIMIZE, SW_RESTORE,
};

fn hwnd_of(id: i64) -> HWND {
    HWND(id as usize as *mut _)
}

/// 强制把窗口切到前台
pub fn force_foreground(hwnd: HWND) {
    unsafe {
        if !IsWindow(hwnd).as_bool() {
            return;
        }
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let fg = GetForegroundWindow();
        if fg == hwnd {
            return;
        }
        // 模拟按下 ALT，绕过 SetForegroundWindow 的前台锁
        keybd_event(VK_MENU.0 as u8, 0, KEYBD_EVENT_FLAGS(0), 0);

        let cur = GetCurrentThreadId();
        let fg_tid = if fg.is_invalid() {
            0
        } else {
            windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(fg, None)
        };
        let target_tid =
            windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(hwnd, None);

        if fg_tid != 0 && fg_tid != cur {
            let _ = AttachThreadInput(cur, fg_tid, BOOL(1));
        }
        if target_tid != 0 && target_tid != cur {
            let _ = AttachThreadInput(cur, target_tid, BOOL(1));
        }
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        if fg_tid != 0 && fg_tid != cur {
            let _ = AttachThreadInput(cur, fg_tid, BOOL(0));
        }
        if target_tid != 0 && target_tid != cur {
            let _ = AttachThreadInput(cur, target_tid, BOOL(0));
        }
        keybd_event(VK_MENU.0 as u8, 0, KEYEVENTF_KEYUP, 0);
        let _ = SetFocus(hwnd);
    }
}

pub fn activate_id(id: i64) {
    force_foreground(hwnd_of(id));
}

/// Dock 点击行为：目标已是“点击前的前台”→ 最小化；最小化状态 → 还原；否则激活。
/// `last_foreign_fg` 是点击前的真实前台（Dock 点击会把 Dock 自己变成前台，不能直接用 GetForegroundWindow 判断）
pub fn toggle_id(id: i64, last_foreign_fg: isize) {
    let hwnd = hwnd_of(id);
    unsafe {
        if !IsWindow(hwnd).as_bool() {
            return;
        }
        if IsIconic(hwnd).as_bool() {
            force_foreground(hwnd);
        } else if last_foreign_fg != 0 && last_foreign_fg == id as isize {
            let _ = ShowWindow(hwnd, SW_MINIMIZE);
        } else {
            force_foreground(hwnd);
        }
    }
}

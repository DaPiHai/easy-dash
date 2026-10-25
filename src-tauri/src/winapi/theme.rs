//! 深色模式（注册表 + WM_SETTINGCHANGE 广播）。
//!
//! Windows 把主题偏好存在 HKCU 的 Personalize 键下：
//! `AppsUseLightTheme` 控制应用，`SystemUsesLightTheme` 控制系统界面，
//! DWORD 值 0 = 深色，1 = 浅色（值缺失视为浅色）。
//! 写入后广播 `WM_SETTINGCHANGE("ImmersiveColorSet")`，让资源管理器和应用立即刷新。
#![allow(non_upper_case_globals)]

use windows::core::PCWSTR;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_READ, KEY_WRITE, REG_DWORD, REG_SAM_FLAGS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
};

use super::common::to_wide;

const PERSONALIZE_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const APPS_USE_LIGHT_THEME: &str = "AppsUseLightTheme";
const SYSTEM_USES_LIGHT_THEME: &str = "SystemUsesLightTheme";

/// 打开当前用户的主题偏好键。
fn open_personalize(access: REG_SAM_FLAGS) -> Option<HKEY> {
    let subkey = to_wide(PERSONALIZE_KEY);
    let mut hkey = HKEY::default();
    let error = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            0,
            access,
            &mut hkey,
        )
    };
    if error.is_err() {
        None
    } else {
        Some(hkey)
    }
}

/// 读取一个 DWORD 值；不存在或类型不符时返回 None。
unsafe fn read_dword(hkey: HKEY, name: &str) -> Option<u32> {
    let value_name = to_wide(name);
    let mut kind = REG_DWORD;
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let error = RegQueryValueExW(
        hkey,
        PCWSTR(value_name.as_ptr()),
        None,
        Some(&mut kind),
        Some(&mut value as *mut u32 as *mut u8),
        Some(&mut size),
    );
    if error.is_err() || kind != REG_DWORD || size != 4 {
        return None;
    }
    Some(value)
}

/// 写入一个 DWORD 值。
unsafe fn write_dword(hkey: HKEY, name: &str, value: u32) -> Result<(), String> {
    let value_name = to_wide(name);
    let bytes = value.to_le_bytes();
    let error = RegSetValueExW(
        hkey,
        PCWSTR(value_name.as_ptr()),
        0,
        REG_DWORD,
        Some(&bytes[..]),
    );
    if error.is_err() {
        Err(format!(
            "写入注册表值 {name} 失败（错误码 {}）",
            error.0
        ))
    } else {
        Ok(())
    }
}

/// 广播主题变化，通知系统与前台应用重读配色。
fn broadcast_theme_change() {
    let payload = to_wide("ImmersiveColorSet");
    let mut result = 0usize;
    unsafe {
        let _ = SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(payload.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            1000,
            Some(&mut result),
        );
    }
}

/// 当前是否深色模式。注册表键打不开时返回 None；值缺失视为浅色。
pub fn dark_mode() -> Option<bool> {
    unsafe {
        let hkey = open_personalize(KEY_READ)?;
        let apps = read_dword(hkey, APPS_USE_LIGHT_THEME);
        let system = read_dword(hkey, SYSTEM_USES_LIGHT_THEME);
        let _ = RegCloseKey(hkey);
        // 两个值都可能缺；缺省按浅色（1）处理
        let use_light = apps.or(system).unwrap_or(1);
        Some(use_light == 0)
    }
}

/// 切换深色 / 浅色模式（应用 + 系统界面一起改），并广播设置变化。
pub fn set_dark_mode(dark: bool) -> Result<(), String> {
    let value = if dark { 0u32 } else { 1u32 };
    unsafe {
        let hkey = open_personalize(KEY_READ | KEY_WRITE)
            .ok_or_else(|| "无法打开主题偏好注册表键（HKCU\\...\\Themes\\Personalize）".to_string())?;
        let apps_result = write_dword(hkey, APPS_USE_LIGHT_THEME, value);
        let system_result = write_dword(hkey, SYSTEM_USES_LIGHT_THEME, value);
        let _ = RegCloseKey(hkey);
        apps_result?;
        system_result?;
    }
    broadcast_theme_change();
    Ok(())
}

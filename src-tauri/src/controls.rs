//! 控制中心编排：聚合系统状态（QuickState）与各开关命令。
//!
//! 命令层（`commands.rs`）在 blocking 线程池里先调用 [`ensure_com`] 初始化
//! COM / WinRT，再调用这里的函数；所有 getter 在不可用时只返回 None / false，
//! 任何失败都不会 panic。
#![allow(dead_code)]

use crate::types::QuickState;
use crate::winapi;

/// 在调用线程上初始化 COM（MTA，Core Audio 用）与 WinRT（MTA，Radio 用）。
///
/// 重复调用无害（已初始化会返回 S_FALSE）；如果该线程已被别的代码初始化成
/// STA，MTA/RoInitialize 会返回 RPC_E_CHANGED_MODE，这里一并忽略——Core Audio
/// 与 Radio 在 STA 线程上仍可用。
pub fn ensure_com() {
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
}

/// 聚合控制中心所需的全部状态。逐个 getter 失败只降级为 None/false。
pub fn quick_state() -> QuickState {
    let (battery, charging, network) = winapi::power::status();
    let volume = winapi::volume::state();
    QuickState {
        volume: volume.map(|(percent, _)| percent),
        muted: volume.map(|(_, muted)| muted),
        brightness: winapi::brightness::get(),
        wifi: winapi::wifi::state(),
        bluetooth: winapi::radio::bt_state(),
        dark_mode: winapi::theme::dark_mode(),
        power_mode: winapi::power::power_mode(),
        battery,
        charging,
        ac_online: winapi::power::ac_online(),
        network,
    }
}

/// 设置系统音量（0..=100）。
pub fn set_volume(percent: u8) -> Result<(), String> {
    winapi::volume::set_volume(percent)
}

/// 设置静音状态。
pub fn set_muted(muted: bool) -> Result<(), String> {
    winapi::volume::set_muted(muted)
}

/// 设置屏幕亮度（0..=100）。
pub fn set_brightness(percent: u32) -> Result<(), String> {
    winapi::brightness::set(percent)
}

/// 打开 / 关闭 Wi-Fi。
pub fn set_wifi(enabled: bool) -> Result<(), String> {
    winapi::wifi::set_enabled(enabled)
}

/// 打开 / 关闭蓝牙。
pub fn set_bluetooth(enabled: bool) -> Result<(), String> {
    winapi::radio::set_bt(enabled)
}

/// 切换深色 / 浅色模式。
pub fn set_dark_mode(dark: bool) -> Result<(), String> {
    winapi::theme::set_dark_mode(dark)
}

/// 切换电源模式："efficiency" | "balanced" | "performance"。
pub fn set_power_mode(mode: &str) -> Result<(), String> {
    winapi::power::set_power_mode(mode)
}

/// 执行电源动作：settings / lock / sleep / shutdown / restart / signout。
pub fn power_action(action: &str) -> Result<(), String> {
    match action {
        "settings" => winapi::power::open_settings(),
        "lock" => winapi::power::lock(),
        "sleep" => winapi::power::sleep(),
        "shutdown" => winapi::power::shutdown(false),
        "restart" => winapi::power::shutdown(true),
        "signout" => winapi::power::signout(),
        other => Err(format!("未知电源动作: {other}")),
    }
}

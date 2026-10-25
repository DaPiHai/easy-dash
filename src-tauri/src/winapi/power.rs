//! 系统状态查询：电池 / 网络
#![allow(non_upper_case_globals)]

use windows::Win32::Networking::WinInet::{InternetGetConnectedState, INTERNET_CONNECTION};

/// 返回 (电池电量百分比, 是否接通电源, 网络是否连接)
pub fn status() -> (Option<u8>, bool, bool) {
    unsafe {
        let mut sps = windows::Win32::System::Power::SYSTEM_POWER_STATUS::default();
        let ok = windows::Win32::System::Power::GetSystemPowerStatus(&mut sps).is_ok();
        let (battery, charging) = if ok && sps.BatteryLifePercent <= 100 {
            (Some(sps.BatteryLifePercent), sps.ACLineStatus == 1)
        } else {
            (None, false)
        };
        let mut flags = INTERNET_CONNECTION::default();
        let network = InternetGetConnectedState(&mut flags, 0).is_ok();
        (battery, charging, network)
    }
}

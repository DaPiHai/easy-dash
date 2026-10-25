//! 系统电源：电池 / 网络状态、电源模式（overlay）、锁屏与关机等动作。
#![allow(non_upper_case_globals)]

use std::sync::OnceLock;

use windows::core::{GUID, PCSTR, PCWSTR};
use windows::Win32::Networking::WinInet::{InternetGetConnectedState, INTERNET_CONNECTION};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Shutdown::{
    ExitWindowsEx, LockWorkStation, EWX_FORCEIFHUNG, EWX_LOGOFF, EWX_REBOOT, EWX_SHUTDOWN,
    SHUTDOWN_REASON,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use super::common::{enable_privilege, to_wide};

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

// ---------------------------------------------------------------------------
// 电源模式（Windows 11 的“最佳能效 / 平衡 / 最佳性能” overlay）
//
// windows crate 0.58 没有绑定 PowerGet/SetUserConfigured{AC,DC}PowerMode，
// 运行时从 powrprof.dll 动态取地址。四个函数都返回 Win32 错误码（0 = 成功）。
// ---------------------------------------------------------------------------

/// 电源模式 GUID：最佳能效。
const MODE_EFFICIENCY: GUID = GUID::from_u128(0x961c_c777_2547_4f9d_8174_7d86_181b_8a7a);
/// 电源模式 GUID：平衡（GUID_NULL）。
const MODE_BALANCED: GUID = GUID::from_u128(0);
/// 电源模式 GUID：最佳性能。
const MODE_PERFORMANCE: GUID = GUID::from_u128(0xded5_74b5_45a0_4f42_8737_4634_5c09_c238);
/// 旧版“高性能”计划 GUID（部分机器仍会返回），归入 performance。
const MODE_PERFORMANCE_LEGACY: GUID = GUID::from_u128(0x3af9_b8d9_7c97_431d_ad78_34a8_bfea_439f);

/// powrprof.dll 里电源模式相关的四个导出函数指针。
struct PowerModeApi {
    get_ac: unsafe extern "system" fn(*mut GUID) -> u32,
    get_dc: unsafe extern "system" fn(*mut GUID) -> u32,
    set_ac: unsafe extern "system" fn(*const GUID) -> u32,
    set_dc: unsafe extern "system" fn(*const GUID) -> u32,
}

static POWER_MODE_API: OnceLock<Option<PowerModeApi>> = OnceLock::new();

/// 懒加载 powrprof.dll 的动态导出（失败缓存 None，不 panic）。
fn power_mode_api() -> Option<&'static PowerModeApi> {
    POWER_MODE_API
        .get_or_init(|| unsafe {
            let module_name = to_wide("powrprof.dll");
            let module = LoadLibraryW(PCWSTR(module_name.as_ptr())).ok()?;
            let proc = |name: &[u8]| -> windows::Win32::Foundation::FARPROC {
                GetProcAddress(module, PCSTR(name.as_ptr()))
            };
            Some(PowerModeApi {
                get_ac: std::mem::transmute(proc(b"PowerGetUserConfiguredACPowerMode\0")?),
                get_dc: std::mem::transmute(proc(b"PowerGetUserConfiguredDCPowerMode\0")?),
                set_ac: std::mem::transmute(proc(b"PowerSetUserConfiguredACPowerMode\0")?),
                set_dc: std::mem::transmute(proc(b"PowerSetUserConfiguredDCPowerMode\0")?),
            })
        })
        .as_ref()
}

/// GUID → 对外使用的稳定字符串。
fn mode_name(guid: &GUID) -> Option<&'static str> {
    if *guid == MODE_EFFICIENCY {
        Some("efficiency")
    } else if *guid == MODE_PERFORMANCE || *guid == MODE_PERFORMANCE_LEGACY {
        Some("performance")
    } else if *guid == MODE_BALANCED {
        Some("balanced")
    } else {
        None
    }
}

/// 是否接通交流电源。
pub fn ac_online() -> bool {
    unsafe {
        let mut sps = windows::Win32::System::Power::SYSTEM_POWER_STATUS::default();
        windows::Win32::System::Power::GetSystemPowerStatus(&mut sps).is_ok()
            && sps.ACLineStatus == 1
    }
}

/// 当前电源模式："efficiency" | "balanced" | "performance"。取不到时 None。
pub fn power_mode() -> Option<String> {
    let api = power_mode_api()?;
    unsafe {
        let mut guid = MODE_BALANCED;
        let code = if ac_online() {
            (api.get_ac)(&mut guid)
        } else {
            (api.get_dc)(&mut guid)
        };
        if code != 0 {
            return None;
        }
        mode_name(&guid).map(|name| name.to_string())
    }
}

/// 切换电源模式（只接受 efficiency / balanced / performance）。
pub fn set_power_mode(mode: &str) -> Result<(), String> {
    let guid = match mode {
        "efficiency" => MODE_EFFICIENCY,
        "balanced" => MODE_BALANCED,
        "performance" => MODE_PERFORMANCE,
        other => {
            return Err(format!(
                "未知电源模式: {other}（仅支持 efficiency / balanced / performance）"
            ))
        }
    };
    let api = power_mode_api()
        .ok_or_else(|| "无法加载 powrprof.dll 的电源模式接口（当前系统可能不支持）".to_string())?;
    unsafe {
        let code = if ac_online() {
            (api.set_ac)(&guid)
        } else {
            (api.set_dc)(&guid)
        };
        if code == 0 {
            Ok(())
        } else {
            Err(format!("切换电源模式失败（错误码 {code}）"))
        }
    }
}

// ---------------------------------------------------------------------------
// 电源动作
// ---------------------------------------------------------------------------

/// 锁定工作站（Win+L 效果）。
pub fn lock() -> Result<(), String> {
    unsafe { LockWorkStation().map_err(|e| format!("锁屏失败: {e}")) }
}

/// 进入睡眠（不休眠）。SetSuspendState 需要关机特权。
pub fn sleep() -> Result<(), String> {
    if !enable_privilege("SeShutdownPrivilege") {
        return Err("进入睡眠失败：无法获取 SeShutdownPrivilege 特权".into());
    }
    unsafe {
        if windows::Win32::System::Power::SetSuspendState(false, false, false).as_bool() {
            Ok(())
        } else {
            Err(format!(
                "进入睡眠失败（错误码 {}）",
                windows::Win32::Foundation::GetLastError().0
            ))
        }
    }
}

/// 关机 / 重启。先启用 SeShutdownPrivilege 再调用 ExitWindowsEx。
pub fn shutdown(restart: bool) -> Result<(), String> {
    if !enable_privilege("SeShutdownPrivilege") {
        return Err("电源操作失败：无法获取 SeShutdownPrivilege 特权".into());
    }
    let action = if restart { "重启" } else { "关机" };
    let flags = if restart {
        EWX_REBOOT | EWX_FORCEIFHUNG
    } else {
        EWX_SHUTDOWN | EWX_FORCEIFHUNG
    };
    unsafe {
        ExitWindowsEx(flags, SHUTDOWN_REASON(0)).map_err(|e| format!("{action}失败: {e}"))
    }
}

/// 注销当前用户。
pub fn signout() -> Result<(), String> {
    unsafe {
        ExitWindowsEx(EWX_LOGOFF, SHUTDOWN_REASON(0)).map_err(|e| format!("注销失败: {e}"))
    }
}

/// 打开系统「设置」应用（ms-settings: URI）。
pub fn open_settings() -> Result<(), String> {
    let verb = to_wide("open");
    let target = to_wide("ms-settings:");
    let instance = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(target.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // 返回值 > 32 表示成功
    if (instance.0 as isize) > 32 {
        Ok(())
    } else {
        Err(format!(
            "打开系统设置失败（ShellExecute 返回 {}）",
            instance.0 as isize
        ))
    }
}

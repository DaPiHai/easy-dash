//! 屏幕亮度（DDC/CI）。
//!
//! 用 `EnumDisplayMonitors` 枚举显示器，再用 `GetPhysicalMonitorsFromHMONITOR`
//! 拿到物理监视器句柄，最后通过 `Get/SetMonitorBrightness` 读写亮度。
//! 只操作第一个支持 DDC/CI 亮度的物理面板（笔记本内置屏通常就是它）。
//! 每次调用都重新枚举：显示器热插拔后不需要重启进程。
#![allow(non_upper_case_globals)]

use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitors, GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
    GetPhysicalMonitorsFromHMONITOR, SetMonitorBrightness, PHYSICAL_MONITOR,
};
use windows::Win32::Foundation::{BOOL, HANDLE, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};

/// EnumDisplayMonitors 回调：把 HMONITOR 收集进 `Vec<HMONITOR>`。
unsafe extern "system" fn collect_monitor(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let monitors = &mut *(lparam.0 as *mut Vec<HMONITOR>);
    monitors.push(hmonitor);
    BOOL(1)
}

/// 枚举所有物理监视器，对第一个让 `f` 返回 Some 的面板返回该结果。
fn with_first_supported_monitor<R>(mut f: impl FnMut(HANDLE) -> Option<R>) -> Option<R> {
    unsafe {
        let mut monitors: Vec<HMONITOR> = Vec::new();
        let ok = EnumDisplayMonitors(
            None::<&HDC>,
            None,
            Some(collect_monitor),
            LPARAM(&mut monitors as *mut Vec<HMONITOR> as isize),
        );
        if ok.0 == 0 {
            return None;
        }
        for hmonitor in monitors {
            let mut count = 0u32;
            if GetNumberOfPhysicalMonitorsFromHMONITOR(hmonitor, &mut count).is_err() || count == 0
            {
                continue;
            }
            let mut array = vec![PHYSICAL_MONITOR::default(); count as usize];
            if GetPhysicalMonitorsFromHMONITOR(hmonitor, &mut array).is_err() {
                continue;
            }
            let mut found = None;
            for monitor in &array {
                if let Some(value) = f(monitor.hPhysicalMonitor) {
                    found = Some(value);
                    break;
                }
            }
            let _ = DestroyPhysicalMonitors(&array);
            if found.is_some() {
                return found;
            }
        }
        None
    }
}

/// 读取某面板的亮度范围；不支持 DDC/CI 亮度时返回 None。
fn brightness_range(handle: HANDLE) -> Option<(u32, u32, u32)> {
    let mut minimum = 0u32;
    let mut current = 0u32;
    let mut maximum = 0u32;
    let ok = unsafe { GetMonitorBrightness(handle, &mut minimum, &mut current, &mut maximum) };
    if ok == 0 {
        None
    } else {
        Some((minimum, current, maximum))
    }
}

/// 把面板原始亮度归一化成 0..=100。
fn normalize(value: u32, minimum: u32, maximum: u32) -> u32 {
    if maximum <= minimum {
        return value.min(100);
    }
    let range = (maximum - minimum) as u64;
    let offset = value.saturating_sub(minimum) as u64;
    ((offset * 100 + range / 2) / range).min(100) as u32
}

/// 把 0..=100 映射回面板原始亮度。
fn denormalize(percent: u32, minimum: u32, maximum: u32) -> u32 {
    if maximum <= minimum {
        return percent.min(maximum.max(100));
    }
    let range = (maximum - minimum) as u64;
    minimum + (range * percent.min(100) as u64 / 100) as u32
}

/// 当前亮度（0..=100）。无支持 DDC/CI 的显示器时返回 None。
pub fn get() -> Option<u32> {
    with_first_supported_monitor(|handle| {
        let (minimum, current, maximum) = brightness_range(handle)?;
        Some(normalize(current, minimum, maximum))
    })
}

/// 设置亮度（百分比，超出 0..=100 会被夹取）。
pub fn set(percent: u32) -> Result<(), String> {
    let percent = percent.min(100);
    let result = with_first_supported_monitor(|handle| {
        let (minimum, _current, maximum) = brightness_range(handle)?;
        let target = denormalize(percent, minimum, maximum);
        let ok = unsafe { SetMonitorBrightness(handle, target) };
        if ok != 0 {
            Some(Ok(()))
        } else {
            Some(Err(format!("设置显示器亮度失败（原本值 {target}）")))
        }
    });
    match result {
        Some(result) => result,
        None => Err("未找到支持 DDC/CI 亮度控制的显示器".into()),
    }
}

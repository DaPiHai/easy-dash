//! 蓝牙射频（WinRT `Radio`）。
//!
//! 读取状态不需要授权；`SetStateAsync` 之前必须先调用一次
//! `Radio::RequestAccessAsync` 并缓存结果。调用线程需要初始化 WinRT
//! （见 `crate::controls::ensure_com`）。
#![allow(non_upper_case_globals)]

use std::sync::OnceLock;

use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState};

/// 缓存首次 `RequestAccessAsync` 的结果（RadioAccessStatus 的数值）。
static BT_ACCESS: OnceLock<i32> = OnceLock::new();

fn access_error(code: i32) -> String {
    format!(
        "蓝牙控制未获系统授权（RadioAccessStatus={code}）；\
         可在「设置 → 隐私和安全性 → 无线电」中允许桌面应用控制无线电"
    )
}

/// 确保已获得无线电控制授权（只请求一次，结果缓存）。
fn ensure_access() -> Result<(), String> {
    if let Some(code) = BT_ACCESS.get() {
        return if *code == RadioAccessStatus::Allowed.0 {
            Ok(())
        } else {
            Err(access_error(*code))
        };
    }
    let status = Radio::RequestAccessAsync()
        .map_err(|e| format!("请求蓝牙控制授权失败: {e}"))?
        .get()
        .map_err(|e| format!("等待蓝牙控制授权结果失败: {e}"))?;
    let _ = BT_ACCESS.set(status.0);
    if status == RadioAccessStatus::Allowed {
        Ok(())
    } else {
        Err(access_error(status.0))
    }
}

/// 蓝牙开关状态：Some(true)=开，Some(false)=关，None=无蓝牙硬件 / WinRT 不可用。
pub fn bt_state() -> Option<bool> {
    let radios = Radio::GetRadiosAsync().ok()?.get().ok()?;
    let count = radios.Size().ok()?;
    for index in 0..count {
        let Ok(radio) = radios.GetAt(index) else {
            continue;
        };
        let is_bluetooth = radio.Kind().map(|kind| kind == RadioKind::Bluetooth).unwrap_or(false);
        if !is_bluetooth {
            continue;
        }
        return match radio.State() {
            Ok(state) if state == RadioState::On => Some(true),
            Ok(state) if state == RadioState::Off => Some(false),
            // Disabled（被系统策略禁用）/ Unknown 都视为不可用
            _ => None,
        };
    }
    None
}

/// 打开 / 关闭蓝牙。
pub fn set_bt(enabled: bool) -> Result<(), String> {
    ensure_access()?;
    let radios = Radio::GetRadiosAsync()
        .map_err(|e| format!("枚举无线电设备失败: {e}"))?
        .get()
        .map_err(|e| format!("等待无线电设备列表失败: {e}"))?;
    let count = radios
        .Size()
        .map_err(|e| format!("读取无线电设备数量失败: {e}"))?;
    let target = if enabled {
        RadioState::On
    } else {
        RadioState::Off
    };
    for index in 0..count {
        let radio = radios
            .GetAt(index)
            .map_err(|e| format!("读取无线电设备失败: {e}"))?;
        let is_bluetooth = radio.Kind().map(|kind| kind == RadioKind::Bluetooth).unwrap_or(false);
        if !is_bluetooth {
            continue;
        }
        let status = radio
            .SetStateAsync(target)
            .map_err(|e| format!("{}蓝牙失败: {e}", if enabled { "开启" } else { "关闭" }))?
            .get()
            .map_err(|e| format!("等待蓝牙状态切换结果失败: {e}"))?;
        if status != RadioAccessStatus::Allowed {
            return Err(access_error(status.0));
        }
        return Ok(());
    }
    Err("未找到蓝牙无线电".into())
}

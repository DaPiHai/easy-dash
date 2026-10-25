//! Wi-Fi 射频与连接状态（WLAN API）。
//!
//! 用 `WlanOpenHandle(2)` 打开 WLAN 会话，取第一块无线网卡的 GUID，
//! 再通过 `WlanQueryInterface` 查询软件射频状态（`wlan_intf_opcode_radio_state`，
//! 返回 `WLAN_RADIO_STATE`）与当前连接（`wlan_intf_opcode_current_connection`）。
//! 开关 Wi-Fi 用 `WlanSetInterface` + `WLAN_PHY_RADIO_STATE`。
#![allow(non_upper_case_globals)]

use windows::Win32::Foundation::HANDLE;
use windows::Win32::NetworkManagement::WiFi::{
    dot11_radio_state_off, dot11_radio_state_on, dot11_radio_state_unknown,
    wlan_interface_state_connected, wlan_intf_opcode_current_connection,
    wlan_intf_opcode_radio_state, WlanCloseHandle, WlanEnumInterfaces, WlanFreeMemory,
    WlanOpenHandle, WlanQueryInterface, WlanSetInterface, WLAN_CONNECTION_ATTRIBUTES,
    WLAN_INTERFACE_INFO_LIST, WLAN_PHY_RADIO_STATE, WLAN_RADIO_STATE,
};

use crate::types::WifiState;

/// 一个打开的 WLAN 会话（Drop 时自动关闭句柄）。
struct WlanSession {
    handle: HANDLE,
    guid: windows::core::GUID,
}

impl Drop for WlanSession {
    fn drop(&mut self) {
        unsafe {
            let _ = WlanCloseHandle(self.handle, None);
        }
    }
}

/// 打开 WLAN 客户端会话，并取第一块无线网卡的接口 GUID。
fn open_session() -> Result<WlanSession, String> {
    unsafe {
        let mut negotiated = 0u32;
        let mut handle = HANDLE::default();
        let code = WlanOpenHandle(2, None, &mut negotiated, &mut handle);
        if code != 0 {
            return Err(format!("打开 WLAN 会话失败（错误码 {code}）"));
        }

        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        let code = WlanEnumInterfaces(handle, None, &mut list);
        if code != 0 {
            let _ = WlanCloseHandle(handle, None);
            return Err(format!("枚举无线网卡失败（错误码 {code}）"));
        }
        if list.is_null() || (*list).dwNumberOfItems == 0 {
            if !list.is_null() {
                WlanFreeMemory(list as *const core::ffi::c_void);
            }
            let _ = WlanCloseHandle(handle, None);
            return Err("未找到无线网卡".into());
        }

        let guid = (*list).InterfaceInfo[0].InterfaceGuid;
        WlanFreeMemory(list as *const core::ffi::c_void);
        Ok(WlanSession { handle, guid })
    }
}

/// 查询软件射频状态：返回 (是否有任一 PHY 打开, 第一个 PHY 的索引)。
fn query_radio_state(session: &WlanSession) -> Option<(bool, u32)> {
    unsafe {
        let mut size = 0u32;
        let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
        let code = WlanQueryInterface(
            session.handle,
            &session.guid,
            wlan_intf_opcode_radio_state,
            None,
            &mut size,
            &mut data,
            None,
        );
        if code != 0 || data.is_null() {
            return None;
        }

        let state = &*(data as *const WLAN_RADIO_STATE);
        let phy_count = (state.dwNumberOfPhys as usize).min(state.PhyRadioState.len());
        let mut enabled = phy_count == 0; // 没有 PHY 明细时按“可用”处理，避免误报关闭
        let mut first_phy_index = 0u32;
        for index in 0..phy_count {
            let phy = &state.PhyRadioState[index];
            if index == 0 {
                first_phy_index = phy.dwPhyIndex;
            }
            if phy.dot11SoftwareRadioState == dot11_radio_state_on {
                enabled = true;
            }
        }

        WlanFreeMemory(data);
        Some((enabled, first_phy_index))
    }
}

/// 查询当前连接：返回 (是否已连接, SSID)。
fn query_connection(session: &WlanSession) -> (bool, String) {
    unsafe {
        let mut size = std::mem::size_of::<WLAN_CONNECTION_ATTRIBUTES>() as u32;
        let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
        let code = WlanQueryInterface(
            session.handle,
            &session.guid,
            wlan_intf_opcode_current_connection,
            None,
            &mut size,
            &mut data,
            None,
        );
        if code != 0 || data.is_null() {
            return (false, String::new());
        }

        let attributes = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
        let connected = attributes.isState == wlan_interface_state_connected;
        let ssid = attributes.wlanAssociationAttributes.dot11Ssid;
        let ssid_len = (ssid.uSSIDLength as usize).min(ssid.ucSSID.len());
        let name = if connected && ssid_len > 0 {
            String::from_utf8_lossy(&ssid.ucSSID[..ssid_len]).to_string()
        } else {
            String::new()
        };

        WlanFreeMemory(data);
        (connected, name)
    }
}

/// 当前 Wi-Fi 状态。没有无线网卡 / WLAN 服务不可用时返回 None。
pub fn state() -> Option<WifiState> {
    let session = open_session().ok()?;
    let (enabled, _phy) = query_radio_state(&session)?;
    let (connected, ssid) = query_connection(&session);
    Some(WifiState {
        enabled,
        connected,
        ssid,
    })
}

/// 打开 / 关闭 Wi-Fi（修改软件射频状态）。
pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let session = open_session()?;
    // 沿用当前 PHY 索引；查询失败时按 0 处理
    let (_, phy_index) = query_radio_state(&session).unwrap_or((true, 0));
    let radio_state = WLAN_PHY_RADIO_STATE {
        dwPhyIndex: phy_index,
        dot11SoftwareRadioState: if enabled {
            dot11_radio_state_on
        } else {
            dot11_radio_state_off
        },
        dot11HardwareRadioState: dot11_radio_state_unknown,
    };
    let code = unsafe {
        WlanSetInterface(
            session.handle,
            &session.guid,
            wlan_intf_opcode_radio_state,
            std::mem::size_of::<WLAN_PHY_RADIO_STATE>() as u32,
            &radio_state as *const _ as *const core::ffi::c_void,
            None,
        )
    };
    if code == 0 {
        Ok(())
    } else {
        Err(format!(
            "{} Wi-Fi 失败（错误码 {code}）",
            if enabled { "开启" } else { "关闭" }
        ))
    }
}

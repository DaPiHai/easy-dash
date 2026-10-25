//! Wi-Fi 射频、状态、扫描与连接（WLAN API）。
//!
//! 用 `WlanOpenHandle(2)` 打开 WLAN 会话，取第一块无线网卡的 GUID，
//! 再通过 `WlanQueryInterface` 查询软件射频状态（`wlan_intf_opcode_radio_state`，
//! 返回 `WLAN_RADIO_STATE`）与当前连接（`wlan_intf_opcode_current_connection`）。
//! 开关 Wi-Fi 用 `WlanSetInterface` + `WLAN_PHY_RADIO_STATE`；
//! 扫描 / 连接 / 断开走 `WlanScan` / `WlanGetAvailableNetworkList` /
//! `WlanSetProfile` / `WlanConnect` / `WlanDisconnect`。
#![allow(non_upper_case_globals)]

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use windows::core::{GUID, PCSTR, PCWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::NetworkManagement::WiFi::{
    dot11_BSS_type_infrastructure, dot11_radio_state_off, dot11_radio_state_on,
    dot11_radio_state_unknown, wlan_connection_mode_profile, wlan_interface_state_connected,
    wlan_intf_opcode_current_connection, wlan_intf_opcode_radio_state, WlanCloseHandle,
    WlanDisconnect, WlanEnumInterfaces, WlanFreeMemory, WlanGetAvailableNetworkList,
    WlanGetProfileList, WlanOpenHandle, WlanQueryInterface, WlanReasonCodeToString, WlanScan,
    WlanSetInterface, WlanSetProfile, DOT11_AUTH_ALGO_80211_OPEN, DOT11_AUTH_ALGO_OWE,
    DOT11_AUTH_ALGO_RSNA, DOT11_AUTH_ALGO_WPA, DOT11_AUTH_ALGO_WPA3,
    DOT11_AUTH_ALGO_WPA3_ENT, DOT11_AUTH_ALGO_WPA3_SAE, DOT11_AUTH_ALGO_WPA_PSK,
    DOT11_AUTH_ALGORITHM, WLAN_AVAILABLE_NETWORK, WLAN_AVAILABLE_NETWORK_CONNECTED,
    WLAN_AVAILABLE_NETWORK_LIST, WLAN_CONNECTION_ATTRIBUTES, WLAN_INTERFACE_INFO_LIST,
    WLAN_PHY_RADIO_STATE, WLAN_PROFILE_INFO, WLAN_PROFILE_INFO_LIST, WLAN_RADIO_STATE,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

use super::common::to_wide;
use crate::types::{WifiNetwork, WifiState};

/// 一个打开的 WLAN 会话（Drop 时自动关闭句柄）。
struct WlanSession {
    handle: HANDLE,
    guid: GUID,
}

impl Drop for WlanSession {
    fn drop(&mut self) {
        unsafe {
            let _ = WlanCloseHandle(self.handle, None);
        }
    }
}

/// 场景扫描的等待时间（WlanScan 是异步的，给驱动一点时间刷新结果）。
const SCAN_WAIT: Duration = Duration::from_millis(1200);

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

// ==================== 周边网络与连接（控制中心 Wi-Fi 子视图） ====================

/// 一条周边网络（内部结构，额外保留认证算法用于连接时生成 profile）。
struct NetworkEntry {
    ssid: String,
    signal: u8,
    secure: bool,
    saved: bool,
    connected: bool,
    auth: DOT11_AUTH_ALGORITHM,
}

/// 生成 profile XML 时使用的认证类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProfileAuth {
    Open,
    WpaPsk,
    Wpa2Psk,
    Wpa3Sae,
}

impl ProfileAuth {
    fn authentication(self) -> &'static str {
        match self {
            ProfileAuth::Open => "open",
            ProfileAuth::WpaPsk => "WPAPSK",
            ProfileAuth::Wpa2Psk => "WPA2PSK",
            ProfileAuth::Wpa3Sae => "WPA3SAE",
        }
    }

    fn encryption(self) -> &'static str {
        match self {
            ProfileAuth::Open => "none",
            ProfileAuth::WpaPsk => "TKIP",
            ProfileAuth::Wpa2Psk => "AES",
            ProfileAuth::Wpa3Sae => "AES",
        }
    }
}

/// DOT11 认证算法 → profile 认证类型（企业认证需提前拦截）。
fn profile_auth(auth: DOT11_AUTH_ALGORITHM) -> ProfileAuth {
    if auth == DOT11_AUTH_ALGO_80211_OPEN {
        ProfileAuth::Open
    } else if auth == DOT11_AUTH_ALGO_WPA_PSK {
        ProfileAuth::WpaPsk
    } else if auth == DOT11_AUTH_ALGO_WPA3_SAE {
        ProfileAuth::Wpa3Sae
    } else {
        // RSNA_PSK(7) 及其它 PSK 变体统一按 WPA2PSK 生成
        ProfileAuth::Wpa2Psk
    }
}

/// 是否为企业 / 802.1X 认证（这类网络必须走系统设置，桌面程序无法代填凭据）。
fn is_enterprise_auth(auth: DOT11_AUTH_ALGORITHM) -> bool {
    auth == DOT11_AUTH_ALGO_RSNA
        || auth == DOT11_AUTH_ALGO_WPA
        || auth == DOT11_AUTH_ALGO_WPA3
        || auth == DOT11_AUTH_ALGO_WPA3_ENT
}

/// XML 文本转义（SSID / 密码可能包含 & < > " '）。
fn xml_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 8);
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// WPA/WPA2 预共享密钥长度校验：8~63 个字符，或 64 位十六进制。
fn valid_psk(password: &str) -> bool {
    let len = password.chars().count();
    (8..=63).contains(&len)
        || (len == 64 && password.chars().all(|c| c.is_ascii_hexdigit()))
}

/// 生成 WLAN profile XML（纯函数，方便单测）。
fn build_profile_xml(
    ssid: &str,
    password: Option<&str>,
    auth: ProfileAuth,
) -> Result<String, String> {
    if ssid.trim().is_empty() {
        return Err("SSID 不能为空".into());
    }
    let ssid_xml = xml_escape(ssid);

    let security = if auth == ProfileAuth::Open {
        format!(
            "<authEncryption>\n                \
             <authentication>{}</authentication>\n                \
             <encryption>{}</encryption>\n                \
             <useOneX>false</useOneX>\n            \
             </authEncryption>",
            auth.authentication(),
            auth.encryption()
        )
    } else {
        let password = match password {
            Some(password) if !password.is_empty() => password,
            _ => return Err("该网络需要密码".into()),
        };
        if !matches!(auth, ProfileAuth::Wpa3Sae) && !valid_psk(password) {
            return Err("Wi-Fi 密码长度不合法（WPA/WPA2 需 8~63 位，或 64 位十六进制）".into());
        }
        format!(
            "<authEncryption>\n                \
             <authentication>{}</authentication>\n                \
             <encryption>{}</encryption>\n                \
             <useOneX>false</useOneX>\n            \
             </authEncryption>\n            \
             <sharedKey>\n                \
             <keyType>passPhrase</keyType>\n                \
             <protected>false</protected>\n                \
             <keyMaterial>{}</keyMaterial>\n            \
             </sharedKey>",
            auth.authentication(),
            auth.encryption(),
            xml_escape(password)
        )
    };

    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <WLANProfile xmlns=\"http://www.microsoft.com/networking/WLAN/profile/v1\">\n    \
         <name>{ssid_xml}</name>\n    \
         <SSIDConfig>\n        \
         <SSID>\n            \
         <name>{ssid_xml}</name>\n        \
         </SSID>\n    \
         </SSIDConfig>\n    \
         <connectionType>ESS</connectionType>\n    \
         <connectionMode>auto</connectionMode>\n    \
         <MSM>\n        \
         <security>\n            \
         {security}\n        \
         </security>\n    \
         </MSM>\n\
         </WLANProfile>"
    ))
}

/// 读取定长 UTF-16 缓冲区（到第一个 NUL 为止）。
fn wide_fixed_to_string(buffer: &[u16]) -> String {
    let end = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end]).trim().to_string()
}

/// `WlanReasonCodeToString` 把原因码翻译成可读文案。
fn reason_text(reason: u32) -> String {
    if reason == 0 {
        return "未知原因".into();
    }
    let buffer = [0u16; 512];
    unsafe {
        let code = WlanReasonCodeToString(reason, &buffer, None);
        if code == 0 {
            let text = wide_fixed_to_string(&buffer);
            if !text.is_empty() {
                return format!("{text}（原因码 {reason}）");
            }
        }
    }
    format!("原因码 {reason}")
}

/// 查询周边网络（scan=true 时先触发一次扫描并等待驱动刷新）。
fn query_available(session: &WlanSession, scan: bool) -> Result<Vec<NetworkEntry>, String> {
    unsafe {
        if scan {
            // WlanScan 失败（例如射频关闭 / 正在扫描）不阻塞列表展示，直接读现有结果
            let _ = WlanScan(
                session.handle,
                &session.guid,
                None,
                None,
                None,
            );
            std::thread::sleep(SCAN_WAIT);
        }

        let mut list: *mut WLAN_AVAILABLE_NETWORK_LIST = std::ptr::null_mut();
        let code = WlanGetAvailableNetworkList(session.handle, &session.guid, 0, None, &mut list);
        if code != 0 {
            return Err(format!("获取周边 Wi-Fi 列表失败（错误码 {code}）"));
        }
        if list.is_null() {
            return Ok(Vec::new());
        }

        let count = (*list).dwNumberOfItems as usize;
        let first = core::ptr::addr_of!((*list).Network) as *const WLAN_AVAILABLE_NETWORK;
        let mut entries = Vec::with_capacity(count);
        for index in 0..count {
            let network = &*first.add(index);
            let ssid = network.dot11Ssid;
            let ssid_len = (ssid.uSSIDLength as usize).min(ssid.ucSSID.len());
            if ssid_len == 0 {
                continue; // 隐藏网络没有 SSID
            }
            let name = String::from_utf8_lossy(&ssid.ucSSID[..ssid_len])
                .trim()
                .to_string();
            if name.is_empty() {
                continue;
            }
            let auth = network.dot11DefaultAuthAlgorithm;
            entries.push(NetworkEntry {
                ssid: name,
                signal: network.wlanSignalQuality.min(100) as u8,
                // 有加密，或认证算法不是开放认证，都视为“需要密码”
                secure: network.bSecurityEnabled.as_bool() || auth != DOT11_AUTH_ALGO_80211_OPEN,
                saved: !wide_fixed_to_string(&network.strProfileName).is_empty(),
                connected: network.dwFlags & WLAN_AVAILABLE_NETWORK_CONNECTED != 0,
                auth,
            });
        }

        WlanFreeMemory(list as *const core::ffi::c_void);
        Ok(entries)
    }
}

/// 扫描并列出周边 Wi-Fi 网络（同 SSID 去重，保留信号最强的 BSS）。
pub fn networks(scan: bool) -> Result<Vec<WifiNetwork>, String> {
    let session = open_session()?;
    let entries = query_available(&session, scan)?;

    let mut merged: HashMap<String, WifiNetwork> = HashMap::new();
    for entry in entries {
        let key = entry.ssid.clone();
        let network = WifiNetwork {
            ssid: entry.ssid,
            signal: entry.signal,
            secure: entry.secure,
            saved: entry.saved,
            connected: entry.connected,
        };
        match merged.entry(key) {
            Entry::Occupied(mut slot) => {
                let current = slot.get_mut();
                current.connected |= network.connected;
                current.saved |= network.saved;
                current.secure |= network.secure;
                if network.signal > current.signal {
                    current.signal = network.signal;
                }
            }
            Entry::Vacant(slot) => {
                slot.insert(network);
            }
        }
    }

    let mut result: Vec<WifiNetwork> = merged.into_values().collect();
    // 已连接优先，其次信号强的在前，最后按名称稳定排序
    result.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then_with(|| b.signal.cmp(&a.signal))
            .then_with(|| a.ssid.cmp(&b.ssid))
    });
    Ok(result)
}

/// 已保存的 Wi-Fi 配置名列表（用于判断是否可直接连接）。
/// 目前前端通过 `networks()` 的 `saved` 字段判断；此函数保留给后续命令/调试使用。
#[allow(dead_code)]
pub fn saved_profiles() -> Vec<String> {
    match open_session() {
        Ok(session) => saved_profiles_of(&session),
        Err(_) => Vec::new(),
    }
}

/// 查询会话对应网卡的已保存配置名。
fn saved_profiles_of(session: &WlanSession) -> Vec<String> {
    unsafe {
        let mut list: *mut WLAN_PROFILE_INFO_LIST = std::ptr::null_mut();
        let code = WlanGetProfileList(session.handle, &session.guid, None, &mut list);
        if code != 0 || list.is_null() {
            return Vec::new();
        }

        let count = (*list).dwNumberOfItems as usize;
        let first = core::ptr::addr_of!((*list).ProfileInfo) as *const WLAN_PROFILE_INFO;
        let mut profiles = Vec::with_capacity(count);
        for index in 0..count {
            let name = wide_fixed_to_string(&(*first.add(index)).strProfileName);
            if !name.is_empty() {
                profiles.push(name);
            }
        }

        WlanFreeMemory(list as *const core::ffi::c_void);
        profiles
    }
}

// -------- WlanConnect 动态调用 --------
//
// windows crate 0.58 把 `WlanConnect` / `WLAN_CONNECTION_PARAMETERS` 放在
// `Win32_NetworkManagement_Ndis` feature 后面，本项目的 Cargo.toml 没有启用该 feature。
// 为了不改共享的 Cargo.toml，这里按 Windows 官方结构体布局自定义参数结构，
// 并从 wlanapi.dll 动态取 `WlanConnect`（powrprof 动态调用同款做法）。

/// 与 Windows `WLAN_CONNECTION_PARAMETERS` 布局一致的参数结构（x64 40 字节）。
#[repr(C)]
struct WlanConnectionParameters {
    /// `WLAN_CONNECTION_MODE`（0 = wlan_connection_mode_profile）
    wlan_connection_mode: i32,
    /// `LPCWSTR`，profile 名
    str_profile: *const u16,
    /// `PDOT11_SSID`，profile 模式下可为空
    dot11_ssid: *mut core::ffi::c_void,
    /// `PDOT11_BSSID_LIST`，一般为空
    desired_bssid_list: *mut core::ffi::c_void,
    /// `DOT11_BSS_TYPE`（1 = infrastructure）
    dot11_bss_type: i32,
    flags: u32,
}

type WlanConnectFn = unsafe extern "system" fn(
    HANDLE,
    *const GUID,
    *const WlanConnectionParameters,
    *const core::ffi::c_void,
) -> u32;

static WLAN_CONNECT_FN: OnceLock<Option<WlanConnectFn>> = OnceLock::new();

/// 懒加载 wlanapi.dll 的 `WlanConnect` 导出。
fn wlan_connect_fn() -> Option<WlanConnectFn> {
    *WLAN_CONNECT_FN.get_or_init(|| unsafe {
        let dll_name = to_wide("wlanapi.dll");
        let module = LoadLibraryW(PCWSTR(dll_name.as_ptr())).ok()?;
        let proc = GetProcAddress(module, PCSTR(b"WlanConnect\0".as_ptr()))?;
        let connect: WlanConnectFn = std::mem::transmute(proc);
        Some(connect)
    })
}

/// 用指定 profile 连接 Wi-Fi。
fn wlan_connect_profile(session: &WlanSession, profile: &str) -> Result<(), String> {
    let connect = wlan_connect_fn()
        .ok_or_else(|| "当前系统无法加载 wlanapi.dll 的 WlanConnect".to_string())?;
    let profile_wide = to_wide(profile);
    let parameters = WlanConnectionParameters {
        wlan_connection_mode: wlan_connection_mode_profile.0,
        str_profile: profile_wide.as_ptr(),
        dot11_ssid: std::ptr::null_mut(),
        desired_bssid_list: std::ptr::null_mut(),
        dot11_bss_type: dot11_BSS_type_infrastructure.0,
        flags: 0,
    };
    let code = unsafe {
        connect(
            session.handle,
            &session.guid,
            &parameters,
            std::ptr::null(),
        )
    };
    if code == 0 {
        Ok(())
    } else {
        Err(format!("发起 Wi-Fi 连接失败（错误码 {code}）"))
    }
}

/// 写入 WLAN profile（`WlanSetProfile`）。
fn set_profile(session: &WlanSession, xml: &str) -> Result<(), String> {
    let xml_wide = to_wide(xml);
    let mut reason = 0u32;
    let code = unsafe {
        WlanSetProfile(
            session.handle,
            &session.guid,
            0,
            PCWSTR(xml_wide.as_ptr()),
            PCWSTR::null(),
            true,
            None,
            &mut reason,
        )
    };
    if code == 0 {
        Ok(())
    } else {
        Err(format!("创建 Wi-Fi 配置失败：{}", reason_text(reason)))
    }
}

/// 连接指定 Wi-Fi（ssid + 可选密码）。
///
/// - 已有保存的 profile：直接按 profile 名连接，password 被忽略；
/// - 新网络：从周边网络列表取认证算法生成 profile，再连接；
/// - 企业 / 802.1X / OWE 网络：返回中文错误，提示去系统设置连接。
pub fn connect(ssid: &str, password: Option<&str>) -> Result<(), String> {
    let ssid = ssid.trim();
    if ssid.is_empty() {
        return Err("SSID 不能为空".into());
    }

    let session = open_session()?;

    // 1) 已有保存配置：直接连
    if saved_profiles_of(&session).iter().any(|name| name == ssid) {
        wlan_connect_profile(&session, ssid)?;
        std::thread::sleep(Duration::from_millis(300));
        return Ok(());
    }

    // 2) 新网络：先找到它，确认认证方式
    let entries = query_available(&session, false)?;
    let entry = entries
        .into_iter()
        .find(|entry| entry.ssid == ssid)
        .ok_or_else(|| format!("未找到 Wi-Fi 网络「{ssid}」，请刷新扫描后重试"))?;

    if is_enterprise_auth(entry.auth) {
        return Err(format!(
            "「{ssid}」是企业 / 802.1X 认证网络，请在系统设置中连接"
        ));
    }
    if entry.auth == DOT11_AUTH_ALGO_OWE {
        return Err(format!("「{ssid}」是 OWE 增强开放网络，请在系统设置中连接"));
    }

    let needs_password = entry.secure && entry.auth != DOT11_AUTH_ALGO_80211_OPEN;
    let password = password.filter(|value| !value.is_empty());
    if needs_password && password.is_none() {
        return Err(format!("「{ssid}」需要密码"));
    }

    let auth = profile_auth(entry.auth);
    let xml = build_profile_xml(ssid, password, auth)?;
    set_profile(&session, &xml)?;
    wlan_connect_profile(&session, ssid)?;
    // 给 WLAN 服务一点时间进入 associating/connected（前端会轮询状态）
    std::thread::sleep(Duration::from_millis(300));
    Ok(())
}

/// 断开当前 Wi-Fi。
pub fn disconnect() -> Result<(), String> {
    let session = open_session()?;
    let code = unsafe { WlanDisconnect(session.handle, &session.guid, None) };
    if code == 0 {
        Ok(())
    } else {
        Err(format!("断开 Wi-Fi 失败（错误码 {code}）"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::NetworkManagement::WiFi::DOT11_AUTH_ALGO_RSNA_PSK;

    #[test]
    fn xml_escape_handles_special_chars() {
        assert_eq!(
            xml_escape("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
        assert_eq!(xml_escape("普通中文 SSID"), "普通中文 SSID");
    }

    #[test]
    fn build_open_profile_has_no_shared_key() {
        let xml = build_profile_xml("Free WiFi & More", None, ProfileAuth::Open).unwrap();
        assert!(xml.contains("<authentication>open</authentication>"));
        assert!(xml.contains("<encryption>none</encryption>"));
        assert!(!xml.contains("<sharedKey>"));
        assert!(xml.contains("<name>Free WiFi &amp; More</name>"));
        assert!(xml.contains("<connectionType>ESS</connectionType>"));
    }

    #[test]
    fn build_wpa2_profile_contains_psk() {
        let xml = build_profile_xml("MyNet", Some("password123"), ProfileAuth::Wpa2Psk).unwrap();
        assert!(xml.contains("<authentication>WPA2PSK</authentication>"));
        assert!(xml.contains("<encryption>AES</encryption>"));
        assert!(xml.contains("<keyMaterial>password123</keyMaterial>"));
        assert!(xml.contains("<name>MyNet</name>"));
    }

    #[test]
    fn build_wpa_profile_uses_tkip() {
        let xml = build_profile_xml("OldNet", Some("password123"), ProfileAuth::WpaPsk).unwrap();
        assert!(xml.contains("<authentication>WPAPSK</authentication>"));
        assert!(xml.contains("<encryption>TKIP</encryption>"));
    }

    #[test]
    fn build_wpa3_profile_uses_sae() {
        let xml = build_profile_xml("NewNet", Some("wpa3-password"), ProfileAuth::Wpa3Sae).unwrap();
        assert!(xml.contains("<authentication>WPA3SAE</authentication>"));
        assert!(xml.contains("<encryption>AES</encryption>"));
        assert!(xml.contains("<keyMaterial>wpa3-password</keyMaterial>"));
    }

    #[test]
    fn build_psk_profile_requires_password() {
        let error = build_profile_xml("Net", None, ProfileAuth::Wpa2Psk).unwrap_err();
        assert!(error.contains("需要密码"));
    }

    #[test]
    fn build_psk_profile_rejects_short_password() {
        assert!(build_profile_xml("Net", Some("1234567"), ProfileAuth::Wpa2Psk).is_err());
        // 64 位十六进制允许
        let hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert!(build_profile_xml("Net", Some(hex), ProfileAuth::Wpa2Psk).is_ok());
    }

    #[test]
    fn password_and_ssid_are_escaped() {
        let xml = build_profile_xml("A&B", Some("p&ss<w0rd"), ProfileAuth::Wpa2Psk).unwrap();
        assert!(xml.contains("<name>A&amp;B</name>"));
        assert!(xml.contains("<keyMaterial>p&amp;ss&lt;w0rd</keyMaterial>"));
    }

    #[test]
    fn empty_ssid_is_rejected() {
        assert!(build_profile_xml("   ", Some("password123"), ProfileAuth::Wpa2Psk).is_err());
    }

    #[test]
    fn profile_auth_mapping_is_correct() {
        assert_eq!(profile_auth(DOT11_AUTH_ALGO_80211_OPEN), ProfileAuth::Open);
        assert_eq!(profile_auth(DOT11_AUTH_ALGO_WPA_PSK), ProfileAuth::WpaPsk);
        assert_eq!(profile_auth(DOT11_AUTH_ALGO_RSNA_PSK), ProfileAuth::Wpa2Psk);
        assert_eq!(profile_auth(DOT11_AUTH_ALGO_WPA3_SAE), ProfileAuth::Wpa3Sae);
    }

    #[test]
    fn enterprise_auth_detected() {
        assert!(is_enterprise_auth(DOT11_AUTH_ALGO_RSNA));
        assert!(is_enterprise_auth(DOT11_AUTH_ALGO_WPA));
        assert!(is_enterprise_auth(DOT11_AUTH_ALGO_WPA3_ENT));
        assert!(!is_enterprise_auth(DOT11_AUTH_ALGO_RSNA_PSK));
        assert!(!is_enterprise_auth(DOT11_AUTH_ALGO_WPA_PSK));
    }

    /// 只加载 wlanapi.dll 并解析导出，不会发起任何连接。
    #[test]
    fn wlan_connect_export_is_available() {
        assert!(
            wlan_connect_fn().is_some(),
            "wlanapi.dll 应导出 WlanConnect，否则未保存网络的连接功能不可用"
        );
    }

    /// 自定义参数结构必须与 Windows `WLAN_CONNECTION_PARAMETERS` 的 ABI 一致。
    /// x64：mode(i32)+pad → 3 指针 → bss(i32) + flags(u32) = 40 字节。
    #[test]
    fn connection_parameters_layout_matches_windows() {
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::size_of::<WlanConnectionParameters>(), 40);
        #[cfg(target_pointer_width = "32")]
        assert_eq!(std::mem::size_of::<WlanConnectionParameters>(), 24);
    }
}

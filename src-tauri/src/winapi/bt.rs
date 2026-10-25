//! 蓝牙已配对设备与连接（控制中心蓝牙子视图）。
//!
//! 枚举：`BluetoothFindFirstRadio` + `BluetoothFindFirstDevice`（fReturnRemembered）。
//! 连接/断开：对设备已安装的音频服务调用 `BluetoothSetServiceState`（best-effort，
//! 音频服务是否真的连上由系统蓝牙栈决定，前端靠 `devices()` 轮询连接状态）。
#![allow(non_upper_case_globals)]

use std::collections::HashMap;

use windows::core::GUID;
use windows::Win32::Devices::Bluetooth::{
    BluetoothEnumerateInstalledServices, BluetoothFindDeviceClose, BluetoothFindFirstDevice,
    BluetoothFindFirstRadio, BluetoothFindNextDevice, BluetoothFindRadioClose,
    BluetoothSetServiceState, BLUETOOTH_DEVICE_INFO, BLUETOOTH_DEVICE_SEARCH_PARAMS,
    BLUETOOTH_FIND_RADIO_PARAMS, BLUETOOTH_SERVICE_DISABLE, BLUETOOTH_SERVICE_ENABLE,
    HBLUETOOTH_DEVICE_FIND, HBLUETOOTH_RADIO_FIND,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE};

use crate::types::BtDevice;

/// 蓝牙音频相关服务 GUID（A2DP / AVRCP / HFP / HSP）。
const AUDIO_SERVICES: [GUID; 6] = [
    GUID::from_u128(0x0000_110a_0000_1000_8000_0080_5f9b_34fb), // A2DP AudioSource
    GUID::from_u128(0x0000_110b_0000_1000_8000_0080_5f9b_34fb), // A2DP AudioSink
    GUID::from_u128(0x0000_110c_0000_1000_8000_0080_5f9b_34fb), // AVRCP Target
    GUID::from_u128(0x0000_110e_0000_1000_8000_0080_5f9b_34fb), // AVRCP Controller
    GUID::from_u128(0x0000_111e_0000_1000_8000_0080_5f9b_34fb), // Handsfree
    GUID::from_u128(0x0000_1108_0000_1000_8000_0080_5f9b_34fb), // Headset
];

fn is_audio_service(guid: &GUID) -> bool {
    AUDIO_SERVICES.contains(guid)
}

/// 读取定长 UTF-16 缓冲区（到第一个 NUL 为止）。
fn fixed_utf16_to_string(buffer: &[u16]) -> String {
    let end = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end]).trim().to_string()
}

/// 第一个蓝牙适配器 + 查找句柄（Drop 时一起关闭）。
struct RadioGuard {
    find: HBLUETOOTH_RADIO_FIND,
    radio: HANDLE,
}

impl RadioGuard {
    unsafe fn open() -> Option<Self> {
        let params = BLUETOOTH_FIND_RADIO_PARAMS {
            dwSize: std::mem::size_of::<BLUETOOTH_FIND_RADIO_PARAMS>() as u32,
        };
        let mut radio = HANDLE::default();
        let find = BluetoothFindFirstRadio(&params, &mut radio).ok()?;
        Some(Self { find, radio })
    }
}

impl Drop for RadioGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = BluetoothFindRadioClose(self.find);
            let _ = CloseHandle(self.radio);
        }
    }
}

/// 设备查找句柄（Drop 时关闭）。
struct DeviceFindGuard(HBLUETOOTH_DEVICE_FIND);

impl Drop for DeviceFindGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = BluetoothFindDeviceClose(self.0);
        }
    }
}

/// 设备搜索参数：只返回已配对 / 已记住 / 已连接的设备，不发起 inquiry。
fn device_search_params(radio: HANDLE) -> BLUETOOTH_DEVICE_SEARCH_PARAMS {
    BLUETOOTH_DEVICE_SEARCH_PARAMS {
        dwSize: std::mem::size_of::<BLUETOOTH_DEVICE_SEARCH_PARAMS>() as u32,
        fReturnAuthenticated: true.into(),
        fReturnRemembered: true.into(),
        fReturnUnknown: false.into(),
        fReturnConnected: true.into(),
        fIssueInquiry: false.into(),
        cTimeoutMultiplier: 1,
        hRadio: radio,
    }
}

fn new_device_info() -> BLUETOOTH_DEVICE_INFO {
    BLUETOOTH_DEVICE_INFO {
        dwSize: std::mem::size_of::<BLUETOOTH_DEVICE_INFO>() as u32,
        ..Default::default()
    }
}

/// `BLUETOOTH_DEVICE_INFO` → 前端用的 `BtDevice`。
unsafe fn device_from_info(info: &BLUETOOTH_DEVICE_INFO) -> Option<BtDevice> {
    let address = info.Address.Anonymous.ullLong;
    if address == 0 {
        return None;
    }
    let name = fixed_utf16_to_string(&info.szName);
    Some(BtDevice {
        name: if name.is_empty() {
            format!("未知设备 {:012X}", address & 0xFFFF_FFFF_FFFF)
        } else {
            name
        },
        address,
        connected: info.fConnected.as_bool(),
    })
}

/// 用第一个适配器枚举已配对的设备。
unsafe fn collect_devices(radio: HANDLE) -> Vec<BtDevice> {
    let mut devices = Vec::new();
    let search = device_search_params(radio);
    let mut info = new_device_info();
    let Ok(find) = BluetoothFindFirstDevice(&search, &mut info) else {
        return devices;
    };
    let find = DeviceFindGuard(find);
    loop {
        if let Some(device) = device_from_info(&info) {
            devices.push(device);
        }
        if BluetoothFindNextDevice(find.0, &mut info).is_err() {
            break;
        }
    }
    devices
}

/// 同地址去重（连接状态取或、名称取非空），已连接的排前面、其余按名称排序。
fn merge_devices(devices: Vec<BtDevice>) -> Vec<BtDevice> {
    let mut merged: HashMap<u64, BtDevice> = HashMap::new();
    for device in devices {
        match merged.get_mut(&device.address) {
            Some(current) => {
                current.connected |= device.connected;
                if current.name.starts_with("未知设备") && !device.name.starts_with("未知设备") {
                    current.name = device.name;
                }
            }
            None => {
                merged.insert(device.address, device);
            }
        }
    }
    let mut result: Vec<BtDevice> = merged.into_values().collect();
    result.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then_with(|| a.name.cmp(&b.name))
    });
    result
}

/// 已配对蓝牙设备列表（含连接状态）。没有蓝牙适配器时返回空列表。
pub fn devices() -> Vec<BtDevice> {
    unsafe {
        let Some(radio) = RadioGuard::open() else {
            return Vec::new();
        };
        merge_devices(collect_devices(radio.radio))
    }
}

/// 在已配对设备里按地址找设备信息。
unsafe fn find_device(radio: HANDLE, address: u64) -> Option<BLUETOOTH_DEVICE_INFO> {
    let search = device_search_params(radio);
    let mut info = new_device_info();
    let Ok(find) = BluetoothFindFirstDevice(&search, &mut info) else {
        return None;
    };
    let find = DeviceFindGuard(find);
    loop {
        if info.Address.Anonymous.ullLong == address {
            return Some(info);
        }
        if BluetoothFindNextDevice(find.0, &mut info).is_err() {
            return None;
        }
    }
}

/// 枚举设备已安装的服务 GUID（先取数量，再取列表）。
unsafe fn installed_services(
    radio: HANDLE,
    info: &BLUETOOTH_DEVICE_INFO,
) -> Result<Vec<GUID>, String> {
    let mut count = 0u32;
    // 第一次调用用于取所需数量（缓冲区为空时返回 ERROR_MORE_DATA 并写入 count）
    let _ = BluetoothEnumerateInstalledServices(radio, info, &mut count, None);
    if count == 0 {
        return Ok(Vec::new());
    }

    let mut services = vec![GUID::default(); count as usize];
    let mut actual = count;
    let code =
        BluetoothEnumerateInstalledServices(radio, info, &mut actual, Some(services.as_mut_ptr()));
    if code != 0 {
        return Err(format!("枚举蓝牙服务失败（错误码 {code}）"));
    }
    services.truncate((actual as usize).min(services.len()));
    Ok(services)
}

/// 连接 / 断开指定蓝牙地址。
///
/// 做法：只对设备上“已安装”的音频服务逐个 `BluetoothSetServiceState`，
/// 至少一个成功即视为成功；设备没有音频服务时提示去系统设置。
pub fn set_connected(address: u64, connect: bool) -> Result<(), String> {
    unsafe {
        let Some(radio) = RadioGuard::open() else {
            return Err("未找到蓝牙适配器".into());
        };
        let info = find_device(radio.radio, address)
            .ok_or_else(|| "未找到该蓝牙设备（请确认已在系统中配对）".to_string())?;

        let installed = installed_services(radio.radio, &info)?;
        let audio_services: Vec<GUID> = installed
            .into_iter()
            .filter(|guid| is_audio_service(guid))
            .collect();
        if audio_services.is_empty() {
            return Err("该设备没有可自动连接的音频服务，请在系统设置中连接".into());
        }

        let flag = if connect {
            BLUETOOTH_SERVICE_ENABLE
        } else {
            BLUETOOTH_SERVICE_DISABLE
        };
        let mut succeeded = 0usize;
        let mut last_error = 0u32;
        for service in &audio_services {
            let code = BluetoothSetServiceState(radio.radio, &info, service, flag);
            if code == 0 {
                succeeded += 1;
            } else {
                last_error = code;
            }
        }

        if succeeded > 0 {
            Ok(())
        } else {
            Err(format!(
                "{}蓝牙音频服务失败（错误码 {last_error}）",
                if connect { "连接" } else { "断开" }
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, address: u64, connected: bool) -> BtDevice {
        BtDevice {
            name: name.to_string(),
            address,
            connected,
        }
    }

    #[test]
    fn audio_service_guids_are_recognized() {
        assert!(is_audio_service(&GUID::from_u128(
            0x0000_110a_0000_1000_8000_0080_5f9b_34fb
        )));
        assert!(is_audio_service(&GUID::from_u128(
            0x0000_1108_0000_1000_8000_0080_5f9b_34fb
        )));
        // 非音频服务（HID 0x1124）不匹配
        assert!(!is_audio_service(&GUID::from_u128(
            0x0000_1124_0000_1000_8000_0080_5f9b_34fb
        )));
    }

    #[test]
    fn merge_devices_dedupes_by_address() {
        let merged = merge_devices(vec![
            device("Buds", 0xAA, false),
            device("Buds", 0xAA, true),
            device("Phone", 0xBB, false),
        ]);
        assert_eq!(merged.len(), 2);
        // 已连接的排前面
        assert_eq!(merged[0].address, 0xAA);
        assert!(merged[0].connected);
        assert_eq!(merged[1].name, "Phone");
    }

    #[test]
    fn merge_devices_prefers_real_name() {
        let merged = merge_devices(vec![
            device("未知设备 0000000000AA", 0xAA, false),
            device("Buds", 0xAA, false),
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].name, "Buds");
    }

    #[test]
    fn fixed_utf16_stops_at_nul() {
        let mut buffer = [0u16; 8];
        for (index, unit) in "耳机".encode_utf16().enumerate() {
            buffer[index] = unit;
        }
        assert_eq!(fixed_utf16_to_string(&buffer), "耳机");
        assert_eq!(fixed_utf16_to_string(&[0u16; 4]), "");
    }
}

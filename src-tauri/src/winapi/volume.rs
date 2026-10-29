//! 系统音量（Core Audio）。
//!
//! 通过 `IMMDeviceEnumerator` 找到默认播放设备（eRender / eConsole），再激活
//! `IAudioEndpointVolume` 读写主音量（0.0~1.0 标量）与静音状态。
//! 调用前需要先初始化 COM（见 `crate::controls::ensure_com`）。
#![allow(non_upper_case_globals)]

use std::ptr;

use windows::core::{IUnknown, Result as WinResult};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

/// 打开默认播放设备的音量控制接口。
fn endpoint_volume() -> WinResult<IAudioEndpointVolume> {
    unsafe {
        // CLSID_MMDeviceEnumerator 在 windows crate 里就叫 MMDeviceEnumerator
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(
            &MMDeviceEnumerator,
            None::<&IUnknown>,
            CLSCTX_INPROC_SERVER,
        )?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        device.Activate::<IAudioEndpointVolume>(CLSCTX_INPROC_SERVER, None)
    }
}

/// 返回 (音量 0..=100, 是否静音)。无音频端点 / COM 未初始化时返回 None。
pub fn state() -> Option<(u8, bool)> {
    let volume = endpoint_volume().ok()?;
    unsafe {
        let scalar = volume.GetMasterVolumeLevelScalar().ok()?;
        let muted = volume.GetMute().ok()?.as_bool();
        let percent = (scalar * 100.0).round().clamp(0.0, 100.0) as u8;
        Some((percent, muted))
    }
}

/// 设置主音量（百分比，超出 0..=100 会被夹取）。
pub fn set_volume(percent: u8) -> Result<(), String> {
    let volume = endpoint_volume().map_err(|e| format!("打开默认音频端点失败: {e}"))?;
    let scalar = percent.min(100) as f32 / 100.0;
    unsafe {
        volume
            .SetMasterVolumeLevelScalar(scalar, ptr::null())
            .map_err(|e| format!("设置系统音量失败: {e}"))
    }
}

/// 设置静音状态。
pub fn set_muted(muted: bool) -> Result<(), String> {
    let volume = endpoint_volume().map_err(|e| format!("打开默认音频端点失败: {e}"))?;
    unsafe {
        volume
            .SetMute(muted, ptr::null())
            .map_err(|e| format!("设置静音状态失败: {e}"))
    }
}

/// 把目标音量夹取到 0..=100（纯函数，便于单测）。
pub fn clamp_percent(value: i32) -> u8 {
    value.clamp(0, 100) as u8
}

/// 相对调节音量（滚轮）：读当前值 → 夹取 → 设置。
///
/// 返回 (新音量, 调节后是否静音)。当前是静音且新音量 > 0 时自动解除静音，
/// 与 Windows 音量滚轮的直觉一致。
pub fn nudge(delta: i32) -> Result<(u8, bool), String> {
    let (current, muted) = state().ok_or_else(|| String::from("未找到可用的音频输出设备"))?;
    let next = clamp_percent(current as i32 + delta);
    set_volume(next)?;
    let muted_after = if next > 0 && muted {
        set_muted(false)?;
        false
    } else {
        muted
    };
    Ok((next, muted_after))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_percent_stays_in_range() {
        assert_eq!(clamp_percent(-50), 0);
        assert_eq!(clamp_percent(0), 0);
        assert_eq!(clamp_percent(37), 37);
        assert_eq!(clamp_percent(100), 100);
        assert_eq!(clamp_percent(101), 100);
        assert_eq!(clamp_percent(i32::MAX), 100);
        assert_eq!(clamp_percent(i32::MIN), 0);
    }
}

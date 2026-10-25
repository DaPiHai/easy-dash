//! 跨模块共享的 DTO（Rust ↔ 前端）
use serde::Serialize;

/// 顶栏任务托盘中的一个条目（镜像 Windows 托盘）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayItem {
    /// 托盘图标提示文本（来自 UIA Name）
    pub name: String,
    /// "icon" = 应用托盘图标；"overflow" = 「显示隐藏的图标」按钮
    pub kind: String,
    /// PNG data URL（截图裁剪得到，可能为 None）
    pub icon: Option<String>,
    /// 屏幕物理像素坐标（用于 SendInput 点击）
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// UIA 枚举出的托盘条目几何信息（尚未取图标）
#[derive(Debug, Clone)]
pub struct TrayGeom {
    pub name: String,
    pub kind: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// 需要截图取图标的区域（Image 子元素，通常 16×16；None 表示取不到）
    pub icon_rect: Option<(i32, i32, i32, i32)>,
}

/// 通知中心条目（只读镜像自 Windows 通知历史）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationItem {
    pub id: i64,
    pub app_name: String,
    pub app_icon: Option<String>,
    pub title: String,
    pub body: String,
    /// Unix 毫秒时间戳
    pub ts_ms: u64,
}

/// Wi-Fi 状态
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WifiState {
    pub enabled: bool,
    pub connected: bool,
    pub ssid: String,
}

/// 控制中心聚合状态
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct QuickState {
    /// 0..=100；None = 无音频端点
    pub volume: Option<u8>,
    pub muted: Option<bool>,
    /// 0..=100；None = 该显示器不支持 DDC/CI 亮度
    pub brightness: Option<u32>,
    pub wifi: Option<WifiState>,
    /// None = 无蓝牙硬件 / WinRT Radio 不可用
    pub bluetooth: Option<bool>,
    pub dark_mode: Option<bool>,
    /// "efficiency" | "balanced" | "performance"
    pub power_mode: Option<String>,
    pub battery: Option<u8>,
    pub charging: bool,
    pub ac_online: bool,
    pub network: bool,
}

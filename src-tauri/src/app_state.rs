use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Mutex;

use crate::config::Config;
use crate::types::{AppEntry, NotificationItem, TrayItem};

/// 全局共享状态
pub struct AppState {
    /// 应用图标缓存：aumid:/exe 键 → PNG data URL（None 表示提取失败）
    pub icon_cache: Mutex<HashMap<String, Option<String>>>,
    /// AUMID → 应用显示名（"设置"/"计算器" 等，来自 AppsFolder）
    pub aumid_names: Mutex<HashMap<String, String>>,
    /// hwnd → AUMID 粘性缓存：AUMID 属性读取偶发返回空值，
    /// 若不固定会导致窗口分组来回翻转、事件风暴拖垮 WebView
    pub hwnd_aumid: Mutex<HashMap<isize, String>>,
    /// hwnd → 首次出现的顺序序号：Dock 未固定应用按窗口打开顺序稳定排序。
    /// 用单调递增序号而非时间戳——同一毫秒内出现的多个窗口会产生并列，
    /// 并列时排序退化为枚举（Z）顺序，表现为"聚焦哪个哪个跑到前面"
    pub win_order: Mutex<HashMap<isize, u64>>,
    /// 窗口顺序序号发号器（单调递增，永不并列）
    pub order_seq: AtomicU64,
    /// hwnd → 最后一次非最小化时的窗口尺寸。
    /// 最小化窗口的 GetWindowRect 是 (-32000,...) 无效值，
    /// 需要缓存真实尺寸供过滤与点击目标选择使用
    pub win_size: Mutex<HashMap<isize, (i32, i32)>>,
    /// hwnd → 最后一次可见时的截图 data URL（最小化窗口在概览里回退用它，
    /// 避免只剩图标卡片）
    pub snapshots: Mutex<HashMap<isize, String>>,
    /// 图标解析队列：Shell COM 调用（AUMID/AppsFolder）可能在无消息泵线程上
    /// 永久阻塞，绝不能在监控线程/主线程热路径同步执行
    pub icon_tx: Sender<isize>,
    pub icon_rx: Mutex<Option<Receiver<isize>>>,
    /// 已入队、待解析的窗口（去重）
    pub icon_pending: Mutex<HashSet<isize>>,
    /// 概览是否处于打开状态
    pub overview_open: AtomicBool,
    /// 当前配置（内存缓存）
    pub config: Mutex<Config>,
    /// 本应用的窗口句柄（十进制 isize）
    pub own_hwnds: Mutex<Vec<isize>>,
    /// 最近一个非本应用的前台窗口（点击 Dock 时 Dock 会抢走前台，
    /// 最小化/激活切换需依据点击前的真实前台判断）
    pub last_foreign_fg: AtomicIsize,
    /// Dock 页面心跳时间戳（毫秒）：页面每 3 秒上报一次；
    /// 超时说明页面 JS 死了或渲染进程崩溃，看门狗会 reload
    pub dock_hb_ms: AtomicU64,
    /// 顶栏面板是否展开（展开时窗口被拉高，AppBar 周期校正必须让路，
    /// 否则 2 秒后窗口会被 sync_topbar 顶回 32px）
    pub topbar_panel_open: AtomicBool,
    /// 系统托盘镜像条目缓存（含截图图标）
    pub tray: Mutex<Vec<TrayItem>>,
    /// 托盘刷新请求通道（前端/启动时投递，工作线程消费）
    pub tray_tx: Sender<()>,
    pub tray_rx: Mutex<Option<Receiver<()>>>,
    /// 托盘镜像是否可用（非 Win11 XAML 任务栏 / 任务栏隐藏时为 false）
    pub tray_available: AtomicBool,
    /// 通知中心条目缓存
    pub notifications: Mutex<Vec<NotificationItem>>,
    /// 通知刷新请求通道
    pub notif_tx: Sender<()>,
    pub notif_rx: Mutex<Option<Receiver<()>>>,
    /// 应用中心是否处于打开状态
    pub app_center_open: AtomicBool,
    /// 应用中心条目缓存（Start Menu 快捷方式 + 打包应用）
    pub apps: Mutex<Vec<AppEntry>>,
    /// 应用列表刷新请求通道
    pub apps_tx: Sender<()>,
    pub apps_rx: Mutex<Option<Receiver<()>>>,
    /// 后台是否正在枚举应用列表（避免启动阶段重复排队两次枚举）
    pub apps_refreshing: AtomicBool,
    /// 应用列表最近一次刷新完成时间（毫秒；0 = 尚未完成）
    pub apps_refreshed_ms: AtomicU64,
}

impl AppState {
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<isize>();
        let (tray_tx, tray_rx) = std::sync::mpsc::channel::<()>();
        let (notif_tx, notif_rx) = std::sync::mpsc::channel::<()>();
        let (apps_tx, apps_rx) = std::sync::mpsc::channel::<()>();
        Self {
            icon_cache: Mutex::new(HashMap::new()),
            aumid_names: Mutex::new(HashMap::new()),
            hwnd_aumid: Mutex::new(HashMap::new()),
            win_order: Mutex::new(HashMap::new()),
            order_seq: AtomicU64::new(1),
            win_size: Mutex::new(HashMap::new()),
            snapshots: Mutex::new(HashMap::new()),
            icon_tx: tx,
            icon_rx: Mutex::new(Some(rx)),
            icon_pending: Mutex::new(HashSet::new()),
            overview_open: AtomicBool::new(false),
            config: Mutex::new(Config::default()),
            own_hwnds: Mutex::new(Vec::new()),
            last_foreign_fg: AtomicIsize::new(0),
            dock_hb_ms: AtomicU64::new(now_ms()),
            topbar_panel_open: AtomicBool::new(false),
            tray: Mutex::new(Vec::new()),
            tray_tx,
            tray_rx: Mutex::new(Some(tray_rx)),
            tray_available: AtomicBool::new(false),
            notifications: Mutex::new(Vec::new()),
            notif_tx,
            notif_rx: Mutex::new(Some(notif_rx)),
            app_center_open: AtomicBool::new(false),
            apps: Mutex::new(Vec::new()),
            apps_tx,
            apps_rx: Mutex::new(Some(apps_rx)),
            apps_refreshing: AtomicBool::new(false),
            apps_refreshed_ms: AtomicU64::new(0),
        }
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

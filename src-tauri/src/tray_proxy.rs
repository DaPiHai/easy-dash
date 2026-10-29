//! 顶栏任务托盘：**托盘代理窗口**实现（替代 UIA 镜像 + 任务栏点击）。
//!
//! 原理（YASB legacy systray 方案）：
//! 1. 在 easy-dash 进程内注册类名 `Shell_TrayWnd` 的顶层窗口并置顶；
//!    `shell32!Shell_NotifyIcon` 用 `FindWindow("Shell_TrayWnd")` 找到它，
//!    所有应用的 `NIM_ADD/MODIFY/DELETE/SETVERSION` 都进入本进程；
//! 2. 收到 `WM_COPYDATA` 后解析 `SHELLTRAYDATA`（32 位布局），更新本地图标表，
//!    同时把消息原样转发给真正的 `Shell_TrayWnd`，保证原生托盘继续可用；
//! 3. 启动/Explorer 重启后广播 `TaskbarCreated`，让已运行应用重新注册图标；
//! 4. 点击时**不显示任务栏、不移动光标去点原生图标**，而是把
//!    `WM_LBUTTONUP` / `WM_RBUTTONUP` / `NIN_SELECT` / `WM_CONTEXTMENU`
//!    通过 `SendNotifyMessage` 直接发给应用窗口的 `uCallbackMessage`。
//!    用户光标本来就在顶栏图标上，菜单自然出现在顶栏下方。
//!
//! 安全点：代理窗口在 High 完整性进程里，`Shell_NotifyIcon` 调用方通常是
//! Medium；必须 `ChangeWindowMessageFilterEx(WM_COPYDATA, MSGFLT_ALLOW)`，
//! 否则 UIPI 会静默丢弃消息（现象：顶栏一个托盘图标都没有）。

#![allow(non_snake_case)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, BOOL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONULL};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, EnumWindows, GetClassNameW, GetCursorPos, GetForegroundWindow,
    GetMessageW,
    GetSystemMetrics, GetWindow, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
    IsWindow, IsWindowVisible, KillTimer, PostMessageW, PostQuitMessage, RegisterClassW,
    RegisterWindowMessageW, SendMessageW, SendNotifyMessageW, SetCursorPos, SetTimer, SetWindowPos,
    TranslateMessage, CHANGEFILTERSTRUCT, CW_USEDEFAULT, GWL_EXSTYLE, GWL_STYLE, GW_OWNER, HICON,
    HMENU, HWND_BROADCAST, HWND_TOPMOST, MSG, MSGFLT_ALLOW, SM_CXSCREEN, SM_CYSCREEN,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, WM_APP, WM_CLOSE, WM_CONTEXTMENU,
    WM_COPYDATA, WM_DESTROY, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_TIMER,
    WM_USER, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::app_state::{now_ms, AppState};
use crate::types::TrayItem;

const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIM_SETFOCUS: u32 = 3;
const NIM_SETVERSION: u32 = 4;

const NIF_MESSAGE: u32 = 0x0000_0001;
const NIF_ICON: u32 = 0x0000_0002;
const NIF_TIP: u32 = 0x0000_0004;
const NIF_STATE: u32 = 0x0000_0008;
const NIF_GUID: u32 = 0x0000_0020;

const NIN_SELECT: u32 = 0x0000_0400;
/// `NOTIFYICON_VERSION_4`：Win7 起的托盘回调协议——左键只发 `NIN_SELECT`，
/// 右键只发 `WM_CONTEXTMENU`，不再发 `WM_LBUTTONUP` / `WM_RBUTTONUP`。
const NOTIFYICON_VERSION_4: u32 = 4;
const WM_TRAY_REFRESH: u32 = WM_APP + 0x51;
const WM_TRAY_STOP: u32 = WM_APP + 0x52;
const WM_TRAY_ASYNC: u32 = WM_USER + 372;
const TIMER_ID: usize = 1;
const TIMER_MS: u32 = 200;
const SHELLTRAY_MAGIC: u32 = 0x3475_3423;

static DEBUG_LOGS: AtomicU32 = AtomicU32::new(0);

fn debug_log_proxy(tag: &str, line: &str) {
    if std::env::var_os("EASYDASH_TRAY_DEBUG").is_none() {
        return;
    }
    if DEBUG_LOGS.fetch_add(1, Ordering::Relaxed) >= 12 {
        return;
    }
    crate::commands::log_tray_click(&format!(
        "ts_ms={} mode=proxy-debug-{tag} {line}",
        now_ms()
    ));
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NotifyIconData32 {
    cb_size: u32,
    hwnd: u32,
    uid: u32,
    uflags: u32,
    callback_message: u32,
    hicon: u32,
    sz_tip: [u16; 128],
    dw_state: u32,
    dw_state_mask: u32,
    sz_info: [u16; 256],
    u_version: u32,
    sz_info_title: [u16; 64],
    dw_info_flags: u32,
    guid_item: [u8; 16],
    h_balloon_icon: u32,
}

impl Default for NotifyIconData32 {
    fn default() -> Self {
        Self {
            cb_size: 0,
            hwnd: 0,
            uid: 0,
            uflags: 0,
            callback_message: 0,
            hicon: 0,
            sz_tip: [0; 128],
            dw_state: 0,
            dw_state_mask: 0,
            sz_info: [0; 256],
            u_version: 0,
            sz_info_title: [0; 64],
            dw_info_flags: 0,
            guid_item: [0; 16],
            h_balloon_icon: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ShellTrayData {
    magic: u32,
    message_type: u32,
    icon_data: NotifyIconData32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct WinNotifyIconIdentifier {
    magic: u32,
    message: u32,
    callback_size: u32,
    padding: u32,
    window_handle: u32,
    uid: u32,
    guid_item: [u8; 16],
}

#[derive(Clone)]
struct ProxyIcon {
    id: String,
    hwnd: isize,
    uid: u32,
    callback: u32,
    version: u32,
    hicon: u32,
    icon: Option<String>,
    tip: String,
    exe: String,
    guid: Option<[u8; 16]>,
    state: u32,
    seq: u64,
    order_key: String,
}

struct ProxyStore {
    icons: HashMap<String, ProxyIcon>,
    /// `NIM_SETVERSION` 先于 `NIM_ADD` 到达时的版本暂存（按 icon id）。
    pending_versions: HashMap<String, u32>,
    seq: u64,
    sig: String,
}

impl ProxyStore {
    fn new() -> Self {
        Self {
            icons: HashMap::new(),
            pending_versions: HashMap::new(),
            seq: 1,
            sig: String::new(),
        }
    }
}

struct ProxyShared {
    app: AppHandle,
    hwnd: AtomicIsize,
    real_tray: AtomicIsize,
    thread_id: AtomicU32,
    alive: AtomicBool,
    last_broadcast_ms: AtomicU64,
    store: Mutex<ProxyStore>,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
}

static PROXY: Mutex<Option<Arc<ProxyShared>>> = Mutex::new(None);

fn proxy_arc() -> Option<Arc<ProxyShared>> {
    PROXY.lock().ok().and_then(|g| g.clone())
}

pub fn is_active() -> bool {
    proxy_arc()
        .map(|p| p.alive.load(Ordering::SeqCst) && p.hwnd.load(Ordering::SeqCst) != 0)
        .unwrap_or(false)
}

/// 启动托盘代理；失败时调用方回退到旧 UIA 实现。
pub fn start(app: AppHandle) -> Result<(), String> {
    {
        let guard = PROXY.lock().map_err(|e| e.to_string())?;
        if guard.is_some() {
            return Ok(());
        }
    }

    let shared = Arc::new(ProxyShared {
        app,
        hwnd: AtomicIsize::new(0),
        real_tray: AtomicIsize::new(0),
        thread_id: AtomicU32::new(0),
        alive: AtomicBool::new(false),
        last_broadcast_ms: AtomicU64::new(0),
        store: Mutex::new(ProxyStore::new()),
        join: Mutex::new(None),
    });

    {
        let mut guard = PROXY.lock().map_err(|e| e.to_string())?;
        *guard = Some(shared.clone());
    }

    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let thread_shared = shared.clone();
    let tx_err = tx.clone();
    let handle = std::thread::Builder::new()
        .name("tray-proxy".into())
        .spawn(move || {
            let result = run_proxy(thread_shared.clone(), tx);
            if let Err(e) = result {
                let _ = tx_err.send(Err(e.clone()));
                eprintln!("[tray-proxy] {e}");
            }
        })
        .map_err(|e| format!("启动托盘代理线程失败：{e}"))?;

    if let Ok(mut join) = shared.join.lock() {
        *join = Some(handle);
    }

    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {
            crate::commands::log_tray_click(&format!(
                "ts_ms={} mode=proxy-start hwnd=0x{:x} real=0x{:x} err=-",
                now_ms(),
                shared.hwnd.load(Ordering::SeqCst),
                shared.real_tray.load(Ordering::SeqCst)
            ));
            Ok(())
        }
        Ok(Err(e)) => {
            if let Ok(mut g) = PROXY.lock() {
                g.take();
            }
            Err(e)
        }
        Err(_) => {
            if let Ok(mut g) = PROXY.lock() {
                g.take();
            }
            Err("托盘代理窗口初始化超时".into())
        }
    }
}

/// 退出：销毁代理窗口并广播 TaskbarCreated，让应用重新注册到真实托盘。
pub fn shutdown() {
    let shared = PROXY.lock().ok().and_then(|mut g| g.take());
    let Some(shared) = shared else {
        return;
    };
    let hwnd = shared.hwnd.swap(0, Ordering::SeqCst);
    if hwnd != 0 {
        unsafe {
            let _ = PostMessageW(hwnd_of(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
    let join = shared.join.lock().ok().and_then(|mut g| g.take());
    if let Some(handle) = join {
        let _ = handle.join();
    }
    broadcast_taskbar_created();
    crate::commands::log_tray_click(&format!(
        "ts_ms={} mode=proxy-stop err=-",
        now_ms()
    ));
}

/// 请求重新广播 TaskbarCreated（图标增减/Explorer 重启后）。
pub fn request_refresh() {
    let Some(shared) = proxy_arc() else {
        return;
    };
    let hwnd = shared.hwnd.load(Ordering::SeqCst);
    if hwnd != 0 {
        unsafe {
            let _ = PostMessageW(hwnd_of(hwnd), WM_TRAY_REFRESH, WPARAM(0), LPARAM(0));
        }
    }
}

/// 托盘刷新入口：代理可用时重新广播，否则回退旧 UIA 镜像刷新。
pub fn request_refresh_or_fallback(app: &AppHandle) {
    if is_active() {
        request_refresh();
    } else {
        crate::tray_mirror::request_refresh(app);
    }
}

/// 重新计算顺序并推送给前端（`set_tray_order` 后调用）。
pub fn reapply_order() {
    if let Some(shared) = proxy_arc() {
        rebuild_and_emit(&shared);
    }
}

/// 点击顶栏托盘图标：直接向应用窗口发送通知消息。
///
/// `anchor` 为顶栏图标中心（屏幕物理像素）。右键先把光标移到该点，
/// 应用 `TrackPopupMenu` 时菜单就出现在顶栏下方；真实鼠标点击时
/// 光标本来就在那里，等价于无操作。
pub fn click(
    key: &str,
    button: &str,
    anchor_x: Option<i32>,
    anchor_y: Option<i32>,
    bar_bottom: Option<i32>,
) -> Result<(), String> {
    let shared = proxy_arc().ok_or_else(|| "托盘代理未启动".to_string())?;
    let icon = {
        let store = shared.store.lock().map_err(|e| e.to_string())?;
        store
            .icons
            .values()
            .find(|i| i.order_key == key)
            .cloned()
            .ok_or_else(|| {
                let keys: Vec<&str> = store.icons.values().map(|i| i.order_key.as_str()).collect();
                format!("托盘项不存在：{key}（现有：{keys:?}）")
            })?
    };

    let hwnd = hwnd_of(icon.hwnd);
    if !unsafe { IsWindow(hwnd) }.as_bool() {
        if let Ok(mut store) = shared.store.lock() {
            store.icons.remove(&icon.id);
        }
        rebuild_and_emit(&shared);
        return Err("托盘图标已失效（应用可能已退出）".into());
    }
    if icon.callback == 0 {
        return Err(format!("托盘项没有回调消息：{}", icon.tip));
    }

    let right = button == "right";
    // 先取窗口类：Qt 的托盘消息窗口只认 NIN_SELECT / WM_CONTEXTMENU，
    // 即使 uVersion 没解析到也必须按 v4 协议发送（见 uses_v4_protocol）。
    let mut class_buf = [0u16; 128];
    let class_len = unsafe { GetClassNameW(hwnd, &mut class_buf) };
    let hwnd_class = String::from_utf16_lossy(&class_buf[..class_len.max(0) as usize]);
    let v4 = uses_v4_protocol(icon.version, &hwnd_class);

    if right {
        if let (Some(x), Some(y)) = (anchor_x, anchor_y) {
            unsafe {
                let _ = SetCursorPos(x, y);
            }
        }
    }

    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let _ = AllowSetForegroundWindow(pid);
    }

    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    // v4 的 wParam 是事件屏幕坐标（Qt 直接拿它当菜单坐标）；优先用顶栏图标中心，
    // 光标在「点击 → invoke → 后端」这段时间里可能已经偏了几像素。
    let (ev_x, ev_y) = match (anchor_x, anchor_y) {
        (Some(x), Some(y)) => (x, y),
        _ => (pt.x, pt.y),
    };
    let wp: usize = if v4 {
        pack_i32(ev_x, ev_y) as u32 as usize
    } else {
        icon.uid as usize
    };
    // 记录点击前的顶层窗口，用于右键后识别“新出现且超出屏幕”的菜单。
    let popup_before = if right {
        snapshot_top_windows()
    } else {
        Vec::new()
    };

    let send = |msg: u32| -> bool {
        // v4 的 lParam 低 16 位是消息、高 16 位是 uID；v0–v3 高 16 位为 0。
        let lp = if v4 {
            pack_i32(msg as i32, icon.uid as i32)
        } else {
            pack_i32(msg as i32, 0)
        };
        unsafe { SendNotifyMessageW(hwnd, icon.callback, WPARAM(wp), LPARAM(lp as isize)).is_ok() }
    };

    // Qt / douyin_tray 的 v0 托盘只有收到 0x7B 才会弹菜单。
    let legacy_context = !v4 && (hwnd_class.starts_with("Qt") || hwnd_class == "TRAY_CLASS");
    let messages = click_messages(right, v4, legacy_context);
    let ok = messages.iter().all(|m| send(*m));

    crate::commands::log_tray_click(&format!(
        "ts_ms={} mode=proxy btn={} key={} name={:?} hwnd=0x{:x} uid={} ver={} proto={} cls={} msgs={:?} cursor=({}, {}) ok={} err=-",
        now_ms(),
        button,
        key,
        icon.tip,
        icon.hwnd,
        icon.uid,
        icon.version,
        if v4 { "v4" } else { "legacy" },
        hwnd_class,
        messages
            .iter()
            .map(|m| format!("0x{m:x}"))
            .collect::<Vec<_>>(),
        pt.x,
        pt.y,
        ok
    ));

    if ok && right {
        let ax = anchor_x.unwrap_or(pt.x);
        let ay = anchor_y.unwrap_or(pt.y);
        std::thread::spawn(move || rescue_offscreen_popup(pid, ax, ay, bar_bottom, popup_before));
    }
    if ok && !right {
        // 应用可能被唤到「所有显示器之外」（显示器拔掉后仍记得旧坐标），
        // 窗口 ShowWindow 了但用户看不到，表现同样是「点了没反应」。
        let ax = anchor_x.unwrap_or(ev_x);
        let ay = anchor_y.unwrap_or(ev_y);
        let key = key.to_string();
        std::thread::spawn(move || rescue_offscreen_foreground(pid, ax, ay, &key));
    }

    if ok {
        Ok(())
    } else {
        Err(format!("通知消息发送失败：{}", icon.tip))
    }
}

fn hwnd_of(v: isize) -> HWND {
    HWND(v as *mut c_void)
}

fn hicon_of(v: u32) -> HICON {
    HICON(v as isize as *mut c_void)
}

fn pack_i32(low: i32, high: i32) -> i32 {
    ((high & 0xffff) << 16) | (low & 0xffff)
}

/// `NOTIFYICONDATA.uVersion` 与 `uTimeout` 共用同一 union 字段，而且只有
/// `NIM_SETVERSION` 才有版本语义：`NIM_ADD` / `NIM_MODIFY` 里读到的可能是超时毫秒
/// （例如「悟空」的 5000）或未初始化的垃圾值（例如「小米互联」的 4015578873）。
/// 只接受 1..=4 这种真实存在的版本号。
fn sane_notify_version(raw: u32) -> Option<u32> {
    (1..=NOTIFYICON_VERSION_4).contains(&raw).then_some(raw)
}

/// 是否按 `NOTIFYICON_VERSION_4` 协议收发回调消息。
///
/// - `uVersion >= 4`：Explorer 对左键只发 `NIN_SELECT`、对右键只发 `WM_CONTEXTMENU`；
/// - Qt（`QSystemTrayIcon` 的窗口过程）只处理 `NIN_SELECT` / `NIN_KEYSELECT` /
///   `WM_CONTEXTMENU` / `WM_LBUTTONDBLCLK`，**完全忽略 `WM_LBUTTONUP`**。
///   微信 / 抖音等 Qt 应用即使 uVersion 没解析到（0），也必须按 v4 发送，
///   否则左键点了完全没反应（2026-10-29 真机定位：微信 `ver=0` 只收到 legacy 序列）。
fn uses_v4_protocol(version: u32, hwnd_class: &str) -> bool {
    version >= NOTIFYICON_VERSION_4 || hwnd_class.starts_with("Qt")
}

/// 一次托盘点击要发送的回调消息（顺序敏感，且必须与真实 Explorer 一致：
/// v4 只发一个语义消息，legacy 才发按下/抬起，避免应用被激活两次而"开了又关"）。
fn click_messages(right: bool, v4: bool, legacy_context: bool) -> Vec<u32> {
    if v4 {
        return vec![if right { WM_CONTEXTMENU } else { NIN_SELECT }];
    }
    let mut msgs = if right {
        vec![WM_RBUTTONDOWN, WM_RBUTTONUP]
    } else {
        vec![WM_LBUTTONDOWN, WM_LBUTTONUP]
    };
    if right && legacy_context {
        // Qt / douyin_tray 的 v0 托盘只有收到 0x7B 才会弹菜单。
        // 菜单可能先出现在主窗口左下角或屏幕上方，再由 rescue_offscreen_popup 锚到顶栏。
        msgs.push(WM_CONTEXTMENU);
    }
    msgs
}

fn utf16z(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end]).trim().to_string()
}

fn exe_of(hwnd: isize) -> String {
    unsafe {
        let hwnd = hwnd_of(hwnd);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return String::new();
        }
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(process);
        if !ok {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        Path::new(&path)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    }
}

/// Shell 自己注册在通知区域里的系统图标（电量/音量/蓝牙设备/安全删除硬件等）
/// 由 explorer.exe（部分系统版本为 ShellHost/SystemSettings）持有 hWnd。
/// 顶栏右侧已经有网络/音量/电池状态区，这些图标不应再镜像到托盘。
fn is_shell_owned(exe: &str) -> bool {
    matches!(
        exe.to_ascii_lowercase().as_str(),
        "explorer"
            | "shellhost"
            | "shellexperiencehost"
            | "startmenuexperiencehost"
            | "systemsettings"
    )
}

fn rebuild_and_emit(shared: &Arc<ProxyShared>) {
    let order = shared
        .app
        .state::<AppState>()
        .config
        .lock()
        .map(|cfg| cfg.tray_order.clone())
        .unwrap_or_default();

    let (items, sig) = {
        let mut store = match shared.store.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut records: Vec<ProxyIcon> = store.icons.values().cloned().collect();
        if records.is_empty() {
            store.sig.clear();
            (Vec::<TrayItem>::new(), String::new())
        } else {
            records.sort_by_key(|r| r.seq);
            // 复用注册表匹配逻辑：tooltip「微信」→ order_key「weixin」，
            // 与旧 UIA 实现/用户已保存的 trayOrder 保持兼容。
            let mut tmp_items: Vec<TrayItem> = records
                .iter()
                .map(|r| TrayItem {
                    name: if r.tip.is_empty() {
                        if r.exe.is_empty() {
                            "托盘图标".to_string()
                        } else {
                            r.exe.clone()
                        }
                    } else {
                        r.tip.clone()
                    },
                    kind: "icon".to_string(),
                    icon: r.icon.clone(),
                    order_key: String::new(),
                    x: 0,
                    y: 0,
                    w: 0,
                    h: 0,
                })
                .collect();
            crate::winapi::app_icons::assign_order_keys_for_proxy(&mut tmp_items);
            for (r, item) in records.iter_mut().zip(tmp_items.iter()) {
                r.order_key = item.order_key.clone();
                if let Some(orig) = store.icons.get_mut(&r.id) {
                    orig.order_key = r.order_key.clone();
                }
            }

            let mut rank: HashMap<&str, usize> = HashMap::new();
            for (i, key) in order.iter().enumerate() {
                let key = key.trim();
                if !key.is_empty() {
                    rank.entry(key).or_insert(i);
                }
            }
            records.sort_by_key(|r| {
                rank.get(r.order_key.as_str())
                    .map(|p| (0u8, *p as i64))
                    .unwrap_or((1u8, r.seq as i64))
            });

            let items: Vec<TrayItem> = records
                .iter()
                .map(|r| TrayItem {
                    name: if r.tip.is_empty() {
                        if r.exe.is_empty() {
                            "托盘图标".to_string()
                        } else {
                            r.exe.clone()
                        }
                    } else {
                        r.tip.clone()
                    },
                    kind: "icon".to_string(),
                    icon: r.icon.clone(),
                    order_key: r.order_key.clone(),
                    x: 0,
                    y: 0,
                    w: 0,
                    h: 0,
                })
                .collect();

            let sig = items
                .iter()
                .map(|i| {
                    format!(
                        "{}|{}|{}",
                        i.order_key,
                        i.name,
                        i.icon.as_deref().map(|s| s.len()).unwrap_or(0)
                    )
                })
                .collect::<Vec<_>>()
                .join(";");
            store.sig = sig.clone();
            (items, sig)
        }
    };

    let state = shared.app.state::<AppState>();
    state.tray_available.store(true, Ordering::Relaxed);
    if let Ok(mut tray) = state.tray.lock() {
        tray.clone_from(&items);
    }
    static LAST_SIG: Mutex<String> = Mutex::new(String::new());
    let changed = {
        let mut last = LAST_SIG.lock().unwrap_or_else(|e| e.into_inner());
        if *last == sig {
            false
        } else {
            *last = sig;
            true
        }
    };
    if changed {
        let _ = shared.app.emit("tray-changed", &items);
        if std::env::var_os("EASYDASH_TRAY_DEBUG").is_some() {
            crate::commands::log_tray_click(&format!(
                "ts_ms={} mode=proxy-items keys={:?}",
                now_ms(),
                items.iter().map(|i| i.order_key.as_str()).collect::<Vec<_>>()
            ));
        }
    }
}

fn broadcast_taskbar_created() {
    unsafe {
        let msg = RegisterWindowMessageW(w!("TaskbarCreated"));
        if msg != 0 {
            let _ = SendNotifyMessageW(HWND_BROADCAST, msg, WPARAM(0), LPARAM(0));
        }
    }
}

fn request_broadcast(shared: &Arc<ProxyShared>, force: bool) {
    let now = now_ms();
    let last = shared.last_broadcast_ms.load(Ordering::Relaxed);
    if !force && now.saturating_sub(last) < 1500 {
        return;
    }
    shared.last_broadcast_ms.store(now, Ordering::Relaxed);
    broadcast_taskbar_created();
}

fn find_real_tray(self_hwnd: isize) -> isize {
    struct Ctx {
        self_hwnd: isize,
        found: isize,
    }
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut Ctx);
        if hwnd.0 as isize == ctx.self_hwnd {
            return BOOL(1);
        }
        let mut buf = [0u16; 128];
        let n = GetClassNameW(hwnd, &mut buf);
        if n > 0 && String::from_utf16_lossy(&buf[..n as usize]) == "Shell_TrayWnd" {
            ctx.found = hwnd.0 as isize;
            return BOOL(0);
        }
        BOOL(1)
    }

    let mut ctx = Ctx {
        self_hwnd,
        found: 0,
    };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut ctx as *mut _ as isize));
    }
    ctx.found
}

fn snapshot_top_windows() -> Vec<isize> {
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        if IsWindowVisible(hwnd).as_bool() {
            let out = &mut *(lparam.0 as *mut Vec<isize>);
            out.push(hwnd.0 as isize);
        }
        BOOL(1)
    }
    let mut out = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

/// 右键菜单兜底：部分应用按任务栏在底部的习惯用 `TPM_BOTTOMALIGN` 定位，
/// 光标在顶栏 y=16 时菜单会翻到屏幕上方。点击后轮询新出现的菜单/浮层，
/// 只要覆盖顶栏或超出**锚点所在显示器**就把它们夹到该屏顶栏下方；正常显示的菜单不动。
///
/// 不要求菜单窗口属于点击目标进程：微信/Qt/Electron 的菜单常由辅助进程创建。
/// 左键激活后的兜底：应用把主窗口唤到「所有显示器之外」时（典型场景：显示器拔掉 /
/// 分辨率变化后仍记得旧坐标），`ShowWindow` 会把窗口显示出来但用户完全看不到，
/// 表现同样是「点了没反应」；而 easy-dash 又藏了任务栏，用户没有别的地方能找回窗口。
///
/// 只在「点击后 ~700ms 成为前台、属于被点击应用、且完全不与任何显示器相交」时，
/// 才把它夹回顶栏所在显示器的工作区；其它情况一律不动。
fn rescue_offscreen_foreground(pid: u32, anchor_x: i32, anchor_y: i32, key: &str) {
    std::thread::sleep(Duration::from_millis(700));
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return;
        }
        let mut fg_pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut fg_pid));
        if fg_pid != pid {
            return;
        }
        // MONITOR_DEFAULTTONULL：窗口不与任何显示器相交时返回 NULL
        if !MonitorFromWindow(fg, MONITOR_DEFAULTTONULL).0.is_null() {
            return;
        }
        let Some(mon) = crate::winapi::monitors::from_point(anchor_x, anchor_y) else {
            return;
        };
        let Some(work) = crate::winapi::monitors::work_area_on(&mon) else {
            return;
        };
        let mut rect = RECT::default();
        if GetWindowRect(fg, &mut rect).is_err() {
            return;
        }
        let max_w = (work.right - work.left).max(1);
        let max_h = (work.bottom - work.top).max(1);
        let w = (rect.right - rect.left).clamp(1, max_w);
        let h = (rect.bottom - rect.top).clamp(1, max_h);
        let x = (anchor_x - w / 2).clamp(work.left, work.right - w);
        let y = (anchor_y - h / 2).clamp(work.top, work.bottom - h);
        let _ = SetWindowPos(
            fg,
            HWND(std::ptr::null_mut()),
            x,
            y,
            w,
            h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        crate::commands::log_tray_click(&format!(
            "ts_ms={} mode=proxy-offscreen-rescue key={} hwnd=0x{:x} from=({},{},{},{}) to=({},{},{},{}) err=-",
            now_ms(),
            key,
            fg.0 as isize,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            x,
            y,
            w,
            h
        ));
    }
}

fn rescue_offscreen_popup(
    _target_pid: u32,
    anchor_x: i32,
    anchor_y: i32,
    bar_bottom: Option<i32>,
    before: Vec<isize>,
) {
    // 锚点所在显示器的物理矩形；取不到时退回主屏
    let (ml, mt, mr, mb) = crate::winapi::monitors::from_point(anchor_x, anchor_y)
        .map(|m| (m.monitor.left, m.monitor.top, m.monitor.right, m.monitor.bottom))
        .unwrap_or_else(|| unsafe {
            (
                0,
                0,
                GetSystemMetrics(SM_CXSCREEN),
                GetSystemMetrics(SM_CYSCREEN),
            )
        });
    let sw = mr - ml;
    let sh = mb - mt;
    let min_y = bar_bottom.unwrap_or(mt + 32);
    let own_pid = unsafe { GetCurrentProcessId() };
    let deadline = std::time::Instant::now() + Duration::from_millis(1500);
    let mut scanned = 0u32;
    loop {
        for handle in snapshot_top_windows() {
            if before.contains(&handle) {
                continue;
            }
            let hwnd = hwnd_of(handle);
            if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
                continue;
            }
            let mut pid = 0u32;
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
            }
            let mut class_buf = [0u16; 128];
            let n = unsafe { GetClassNameW(hwnd, &mut class_buf) };
            let class = String::from_utf16_lossy(&class_buf[..n.max(0) as usize]);
            if class.is_empty()
                || class == "Shell_TrayWnd"
                || class == "Shell_SecondaryTrayWnd"
                || class == "Progman"
                || class == "WorkerW"
                || class == "tooltips_class32"
            {
                continue;
            }
            let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
            let exstyle = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
            let owner = unsafe { GetWindow(hwnd, GW_OWNER) }.unwrap_or_default();
            let is_menu = class == "#32768";
            let is_owned_popup = (style & WS_POPUP.0 as u32) != 0 && !owner.is_invalid();
            let is_tool_popup = (exstyle & WS_EX_TOOLWINDOW.0 as u32) != 0
                && (style & WS_POPUP.0 as u32) != 0;
            let is_app_popup = (style & WS_POPUP.0 as u32) != 0
                && (class.starts_with("Chrome_WidgetWin") || class.starts_with("Qt"));
            let is_qt_popup = class.starts_with("Qt");
            let is_chrome = class.starts_with("Chrome_WidgetWin");
            let is_topmost = (exstyle & WS_EX_TOPMOST.0 as u32) != 0;
            if !(is_menu
                || is_owned_popup
                || is_tool_popup
                || is_app_popup
                || is_qt_popup
                || (is_chrome && is_topmost))
            {
                continue;
            }
            // 自己的 topbar/dock/overview 等新窗口不参与；本进程的 #32768 托盘菜单要参与。
            if pid == own_pid && !is_menu {
                continue;
            }
            let mut rect = RECT::default();
            if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
                continue;
            }
            let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
            if w < 8 || h < 8 || w > sw || h > sh {
                continue;
            }
            // Qt/Electron 的主窗口也是顶层窗口；只处理菜单尺寸的浮层。
            if (is_qt_popup || is_chrome) && (w > sw * 3 / 4 || h > sh * 3 / 4) {
                continue;
            }
            let center_x = (rect.left + rect.right) / 2;
            let near_anchor = (center_x - anchor_x).abs() < 800
                || is_menu
                || class.starts_with("Qt")
                || (is_chrome && is_topmost);
            if !near_anchor {
                continue;
            }
            if scanned < 8 {
                scanned += 1;
                if std::env::var_os("EASYDASH_TRAY_DEBUG").is_some() {
                    crate::commands::log_tray_click(&format!(
                        "ts_ms={} mode=proxy-popup-scan pid={pid} class={class} rect=({},{},{},{})",
                        now_ms(),
                        rect.left,
                        rect.top,
                        rect.right,
                        rect.bottom
                    ));
                }
            }
            // 已经在锚点显示器内且不覆盖顶栏：不动
            if rect.left >= ml
                && rect.top >= min_y
                && rect.right <= mr
                && rect.bottom <= mb
                && !class.starts_with("Qt")
            {
                continue;
            }
            // Qt 菜单即使“在屏幕内”也可能被应用放到主窗口左下角；
            // 对 Qt 浮层统一锚到该屏顶栏图标下方。
            let force_anchor = class.starts_with("Qt");
            let mut x = if force_anchor || rect.left < ml {
                (anchor_x - w / 2).clamp(ml + 8, (mr - w - 8).max(ml + 8))
            } else {
                rect.left
            };
            if x + w > mr - 8 {
                x = (mr - w - 8).max(ml + 8);
            }
            let mut y = if force_anchor { min_y } else { rect.top };
            if y < min_y {
                y = min_y;
            }
            if y + h > mb - 8 {
                y = (mb - h - 8).max(mt + 8);
            }
            if x == rect.left && y == rect.top {
                continue;
            }
            let _ = unsafe {
                SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    x,
                    y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                )
            };
            crate::commands::log_tray_click(&format!(
                "ts_ms={} mode=proxy-popup-rescue class={class} from=({},{}) to=({x},{y}) size={w}x{h}",
                now_ms(),
                rect.left,
                rect.top
            ));
            return;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

fn forward_to_real(
    shared: &Arc<ProxyShared>,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let mut real = shared.real_tray.load(Ordering::SeqCst);
    // Explorer 重启后旧句柄失效：先重新找真实托盘，避免把注册消息丢进死句柄。
    if real == 0 || !unsafe { IsWindow(hwnd_of(real)) }.as_bool() {
        let self_hwnd = shared.hwnd.load(Ordering::SeqCst);
        let found = find_real_tray(self_hwnd);
        if found != 0 {
            shared.real_tray.store(found, Ordering::SeqCst);
            real = found;
        }
    }
    if real == 0 {
        return LRESULT(0);
    }
    unsafe {
        if msg == WM_TRAY_ASYNC {
            let _ = PostMessageW(hwnd_of(real), msg, wparam, lparam);
            LRESULT(0)
        } else {
            SendMessageW(hwnd_of(real), msg, wparam, lparam)
        }
    }
}

fn apply_icon_message(shared: &Arc<ProxyShared>, data: &ShellTrayData) {
    let nid = &data.icon_data;
    let hwnd = nid.hwnd as isize;
    let guid = if nid.uflags & NIF_GUID != 0 {
        Some(nid.guid_item)
    } else {
        None
    };
    let id = match guid {
        Some(g) => format!(
            "g:{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            g[0], g[1], g[2], g[3], g[4], g[5], g[6], g[7], g[8], g[9], g[10], g[11], g[12], g[13], g[14], g[15]
        ),
        None => format!("w:{hwnd}:{}", nid.uid),
    };

    if data.message_type == NIM_DELETE {
        if let Ok(mut store) = shared.store.lock() {
            store.icons.remove(&id);
            store.pending_versions.remove(&id);
        }
        rebuild_and_emit(shared);
        return;
    }
    if data.message_type == NIM_SETFOCUS {
        return;
    }
    if data.message_type == NIM_SETVERSION {
        if std::env::var_os("EASYDASH_TRAY_DEBUG").is_some() {
            crate::commands::log_tray_click(&format!(
                "ts_ms={} mode=proxy-setversion hwnd=0x{:x} uid={} version={}",
                now_ms(),
                nid.hwnd,
                nid.uid,
                nid.u_version
            ));
        }
        let Some(version) = sane_notify_version(nid.u_version) else {
            return;
        };
        if let Ok(mut store) = shared.store.lock() {
            if let Some(icon) = store.icons.get_mut(&id) {
                icon.version = version;
            } else {
                // 少数应用先发 NIM_SETVERSION 再发 NIM_ADD，暂存给后面的 ADD 用。
                store.pending_versions.insert(id.clone(), version);
            }
        }
        return;
    }
    if data.message_type != NIM_ADD && data.message_type != NIM_MODIFY {
        return;
    }

    // 不少应用（Qt/微信等）不会补发 NIM_SETVERSION，但会在 NIM_ADD / NIM_MODIFY
    // 里带上正确的 uVersion。只依赖 SETVERSION 会一直读成 0，导致 v4 应用左键无反应。
    let payload_version = sane_notify_version(nid.u_version);
    let tip = utf16z(&nid.sz_tip);
    if data.message_type == NIM_ADD && DEBUG_LOGS.load(Ordering::Relaxed) < 12 {
        debug_log_proxy(
            "add",
            &format!(
                "magic=0x{:x} msg={} cbSize={} hwnd=0x{:x} uid={} flags=0x{:x} cb=0x{:x} ver={} tip={:?} guid={}",
                data.magic,
                data.message_type,
                nid.cb_size,
                nid.hwnd,
                nid.uid,
                nid.uflags,
                nid.callback_message,
                nid.u_version,
                tip,
                guid.is_some()
            ),
        );
    }
    let exe = shared
        .store
        .lock()
        .ok()
        .and_then(|s| s.icons.get(&id).map(|i| i.exe.clone()))
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| exe_of(hwnd));
    if is_shell_owned(&exe) {
        debug_log_proxy(
            "skip-shell",
            &format!("exe={exe} hwnd=0x{:x} uid={} tip={:?}", nid.hwnd, nid.uid, tip),
        );
        if let Ok(mut store) = shared.store.lock() {
            store.icons.remove(&id);
        }
        rebuild_and_emit(shared);
        return;
    }

    let icon_url = if nid.uflags & NIF_ICON != 0 && nid.hicon != 0 {
        crate::winapi::icons::icon_data_url(hicon_of(nid.hicon))
    } else {
        None
    };

    {
        let Ok(mut store) = shared.store.lock() else {
            return;
        };
        let version_seed = payload_version.or_else(|| store.pending_versions.remove(&id));
        if !store.icons.contains_key(&id) {
            let seq = store.seq;
            store.seq = store.seq.saturating_add(1);
            store.icons.insert(
                id.clone(),
                ProxyIcon {
                    id: id.clone(),
                    hwnd,
                    uid: nid.uid,
                    callback: nid.callback_message,
                    version: version_seed.unwrap_or(0),
                    hicon: nid.hicon,
                    icon: icon_url.clone(),
                    tip: tip.clone(),
                    exe: exe.clone(),
                    guid,
                    state: nid.dw_state,
                    seq,
                    order_key: String::new(),
                },
            );
        } else if let Some(icon) = store.icons.get_mut(&id) {
            // 版本只升级不降级：NIM_MODIFY 里的 uVersion 可能是 uTimeout（0/5000），
            // 不能把已经从 SETVERSION / ADD 拿到的 4 冲掉。
            if icon.version == 0 {
                if let Some(v) = version_seed {
                    icon.version = v;
                }
            }
            if nid.uflags & NIF_MESSAGE != 0 {
                icon.callback = nid.callback_message;
            }
            if nid.uflags & NIF_TIP != 0 {
                icon.tip = tip.clone();
            }
            if nid.uflags & NIF_STATE != 0 {
                icon.state = nid.dw_state;
            }
            if nid.uflags & NIF_ICON != 0 && nid.hicon != 0 && nid.hicon != icon.hicon {
                icon.hicon = nid.hicon;
                icon.icon = icon_url.clone();
            }
            if nid.uflags & NIF_GUID != 0 {
                icon.guid = guid;
            }
        }
    }
    rebuild_and_emit(shared);
}

fn handle_copy_data(shared: &Arc<ProxyShared>, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let cds = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
    if cds.dwData == 1 {
        if cds.cbData as usize >= std::mem::size_of::<ShellTrayData>() && !cds.lpData.is_null() {
            let data = unsafe { &*(cds.lpData as *const ShellTrayData) };
            if data.magic != SHELLTRAY_MAGIC && DEBUG_LOGS.load(Ordering::Relaxed) < 12 {
                debug_log_proxy(
                    "magic",
                    &format!(
                        "unexpected magic=0x{:x} msg={} cbSize={} cbData={}",
                        data.magic, data.message_type, data.icon_data.cb_size, cds.cbData
                    ),
                );
            }
            let forwarded = forward_to_real(shared, WM_COPYDATA, wparam, lparam);
            apply_icon_message(shared, data);
            return forwarded;
        }
        debug_log_proxy(
            "short",
            &format!("dwData=1 cbData={} (too small)", cds.cbData),
        );
        return forward_to_real(shared, WM_COPYDATA, wparam, lparam);
    }
    if cds.dwData == 3 {
        if cds.cbData as usize >= std::mem::size_of::<WinNotifyIconIdentifier>()
            && !cds.lpData.is_null()
        {
            let idf = unsafe { &*(cds.lpData as *const WinNotifyIconIdentifier) };
            let mut pt = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut pt);
            }
            // 返回一个以光标为中心、16×16 的有效图标矩形。旧实现 top=pt.y+1 /
            // bottom=pt.y-1 是倒置矩形，部分应用据此定位菜单会翻到屏幕上方。
            return match idf.message {
                1 => LRESULT(pack_i32(pt.x - 8, pt.y - 8) as isize),
                2 => LRESULT(pack_i32(pt.x + 8, pt.y + 8) as isize),
                _ => LRESULT(0),
            };
        }
        return LRESULT(0);
    }
    debug_log_proxy(
        "unknown",
        &format!("dwData={} cbData={} forward", cds.dwData, cds.cbData),
    );
    forward_to_real(shared, WM_COPYDATA, wparam, lparam)
}

unsafe extern "system" fn proxy_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let shared = proxy_arc();
    match msg {
        WM_COPYDATA => {
            if let Some(shared) = shared.as_ref() {
                return handle_copy_data(shared, wparam, lparam);
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if let Some(shared) = shared.as_ref() {
                let _ = SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
                if shared.real_tray.load(Ordering::SeqCst) == 0
                    || !IsWindow(hwnd_of(shared.real_tray.load(Ordering::SeqCst))).as_bool()
                {
                    let real = find_real_tray(hwnd.0 as isize);
                    if real != 0 {
                        shared.real_tray.store(real, Ordering::SeqCst);
                        request_broadcast(shared, true);
                    }
                }
            }
            LRESULT(0)
        }
        WM_TRAY_REFRESH => {
            if let Some(shared) = shared.as_ref() {
                let real = find_real_tray(hwnd.0 as isize);
                if real != 0 {
                    shared.real_tray.store(real, Ordering::SeqCst);
                }
                request_broadcast(shared, true);
            }
            LRESULT(0)
        }
        WM_TRAY_STOP | WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe {
                let _ = KillTimer(hwnd, TIMER_ID);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => {
            if let Some(shared) = shared.as_ref() {
                if msg >= WM_USER {
                    let real = shared.real_tray.load(Ordering::SeqCst);
                    if real != 0 {
                        return forward_to_real(shared, msg, wparam, lparam);
                    }
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }
}

fn run_proxy(shared: Arc<ProxyShared>, tx: mpsc::Sender<Result<(), String>>) -> Result<(), String> {
    unsafe {
        let hinstance = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandle 失败：{e}"))?;
        let class_name = crate::winapi::common::to_wide("Shell_TrayWnd");
        let wnd_class = WNDCLASSW {
            lpfnWndProc: Some(proxy_wndproc),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            hInstance: HINSTANCE(hinstance.0),
            ..Default::default()
        };
        let atom = RegisterClassW(&wnd_class);
        if atom == 0 {
            eprintln!("[tray-proxy] RegisterClassW 返回 0（可能类已存在），继续尝试创建窗口");
        }

        let title = crate::winapi::common::to_wide("easy-dash tray proxy");
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            CW_USEDEFAULT,
            0,
            CW_USEDEFAULT,
            0,
            HWND(std::ptr::null_mut()),
            HMENU(std::ptr::null_mut()),
            HINSTANCE(hinstance.0),
            None,
        )
        .map_err(|e| format!("创建托盘代理窗口失败：{e}"))?;

        if hwnd.0.is_null() {
            return Err("创建托盘代理窗口返回空句柄".into());
        }

        // High 完整性进程必须显式放行 Medium 应用的 WM_COPYDATA，否则 UIPI 丢弃。
        // 放行失败时必须销毁代理窗口并回退：否则应用会把图标注册到代理却被过滤，
        // 真实托盘和顶栏两边都收不到。
        if let Err(e) =
            ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, None::<*mut CHANGEFILTERSTRUCT>)
        {
            let _ = DestroyWindow(hwnd);
            return Err(format!("ChangeWindowMessageFilterEx(WM_COPYDATA) 失败：{e}"));
        }

        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );

        let real = find_real_tray(hwnd.0 as isize);
        shared.hwnd.store(hwnd.0 as isize, Ordering::SeqCst);
        shared.real_tray.store(real, Ordering::SeqCst);
        shared.thread_id.store(GetCurrentThreadId(), Ordering::SeqCst);
        shared.alive.store(true, Ordering::SeqCst);

        // 500ms 重新置顶：防止 Explorer 任务栏重建后抢走 FindWindow 的第一顺位
        let _ = SetTimer(hwnd, TIMER_ID, TIMER_MS, None);

        request_broadcast(&shared, true);

        let _ = tx.send(Ok(()));
        drop(tx);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND(std::ptr::null_mut()), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            if !shared.alive.load(Ordering::SeqCst) {
                break;
            }
        }

        shared.alive.store(false, Ordering::SeqCst);
        let _ = DestroyWindow(hwnd);
        shared.hwnd.store(0, Ordering::SeqCst);
        broadcast_taskbar_created();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_union_ignores_timeout_and_garbage() {
        // 合法版本号
        assert_eq!(sane_notify_version(3), Some(3));
        assert_eq!(sane_notify_version(4), Some(4));
        // uVersion 与 uTimeout 共用字段：没有版本语义时读到的是超时毫秒或垃圾值
        assert_eq!(sane_notify_version(0), None);
        assert_eq!(sane_notify_version(5000), None); // 悟空
        assert_eq!(sane_notify_version(4_015_578_873), None); // 小米互联
    }

    #[test]
    fn v4_protocol_detection() {
        // 微信：真机 NIM_ADD 带 uVersion=4，但旧实现写死 0 → 左键点了没反应
        assert!(uses_v4_protocol(4, "Qt51514WxTrayIconMessageWindowClass"));
        // 微信/抖音等 Qt 应用即使 uVersion 没拿到，也只认 NIN_SELECT
        assert!(uses_v4_protocol(0, "Qt51514WxTrayIconMessageWindowClass"));
        assert!(uses_v4_protocol(0, "Qt5QWindowIcon"));
        // 老 Qt（douyin_tray）用自定义类名，按 legacy 处理
        assert!(!uses_v4_protocol(0, "TRAY_CLASS"));
        // Realtek 明确声明 v3：legacy 消息
        assert!(!uses_v4_protocol(3, "Afx:0000000140000000:0"));
    }

    #[test]
    fn left_click_uses_nin_select_only_for_v4() {
        assert_eq!(click_messages(false, true, false), vec![NIN_SELECT]);
        // Qt v0 右击才需要 0x7B；左键仍旧是按下/抬起
        assert_eq!(
            click_messages(false, false, false),
            vec![WM_LBUTTONDOWN, WM_LBUTTONUP]
        );
    }

    #[test]
    fn right_click_matches_shell_protocol() {
        // v4：只发 WM_CONTEXTMENU（真实 Explorer 不会再发 WM_RBUTTONDOWN/UP）
        assert_eq!(click_messages(true, true, true), vec![WM_CONTEXTMENU]);
        assert_eq!(click_messages(true, true, false), vec![WM_CONTEXTMENU]);
        // legacy：按下/抬起
        assert_eq!(
            click_messages(true, false, false),
            vec![WM_RBUTTONDOWN, WM_RBUTTONUP]
        );
        // legacy + Qt/TRAY_CLASS：补 0x7B 才会弹菜单
        assert_eq!(
            click_messages(true, false, true),
            vec![WM_RBUTTONDOWN, WM_RBUTTONUP, WM_CONTEXTMENU]
        );
    }
}

//! Windows 任务栏隐藏 / 恢复 + 跨进程托盘桥接。
//!
//! 设计要点：
//!
//! - **隐藏（v0.2.2 起）**：默认走「隐身」模式——给 `Shell_TrayWnd`（以及每块副屏的
//!   `Shell_SecondaryTrayWnd`）置 `WS_EX_LAYERED` + alpha=0 + `WS_EX_TRANSPARENT`，
//!   窗口**保持 visible**（用户看不见、鼠标穿透到下层）。
//!   不用 `SW_HIDE` 的原因：任务栏一旦「隐藏→显示」跳变（托盘镜像每 10s 桥接一次、
//!   每次托盘点击都会 `SW_SHOW`/`SW_HIDE`），Explorer 会按它仍注册的 AppBar 把底部
//!   48px 重新算回工作区，最大化窗口被压回 1032px 且不会自己长回来（真机复现）。
//! - **隐藏（兜底）**：隐身不可用（置 layered/穿透被 UIPI 拒绝）时退回
//!   `WS_EX_LAYERED` alpha=0 + `ShowWindow(SW_HIDE)`，两种手段叠加。
//! - **恢复**：`ShowWindow(SW_SHOWNOACTIVATE)` + 还原我们修改前记下的 exstyle
//!   （去掉 layered/transparent，避免把任务栏原本的 layered/透明属性一起抹掉）。
//! - **watchdog**：monitor 线程每 400ms 调用一次，补回被 Explorer 重置的
//!   layered/alpha/穿透风格、维持隐身、Explorer 重启后重新发现窗口（旧 HWND 用
//!   `IsWindow` 过滤）。
//! - **桥接** `with_taskbar_visible`：UIA 读取与真实鼠标点击都要求任务栏窗口
//!   可被命中。隐身模式下窗口本来就 visible，桥接只做「临时去掉鼠标穿透」+
//!   保证 alpha（不改可见性 → 不触发 Explorer 重算工作区）；SW_HIDE 兜底路径下
//!   才真正 `SW_SHOWNOACTIVATE` / `SW_HIDE` 切换。
//!   全局互斥 + 线程内嵌套计数 + RAII guard，panic 时也会复原。
//! - **自动降级**：桥接扫描连续失败达到阈值后切「持久 alpha=0」模式：任务栏
//!   窗口一直保持 visible、只是完全透明，UIA / 点击不再依赖显示/隐藏切换。
//! - **强杀恢复**：隐藏成功时在配置目录写 `taskbar-hidden.lock`；下次启动
//!   `recover_stale_lock()` 先恢复，避免用户卡在“没有任务栏”的状态。
//! - **排障开关**：环境变量 `EASYDASH_DISABLE_TASKBAR_HIDE=1` 完全跳过隐藏；
//!   `EASYDASH_TASKBAR_BRIDGE_VISIBLE=1` 让桥接期间真正渲染任务栏（UIA 在
//!   alpha=0 下不可用时用于验证）。
//!
//! 本模块进程以管理员权限运行（requireAdministrator 清单），因此
//! `SetWindowLongPtrW` 可以跨进程修改 Explorer 的任务栏窗口。
//!
//! ## 工作区接管（v0.2.2）
//!
//! 任务栏作为 AppBar 占用底部屏幕空间：即便它不可见，Windows 工作区仍保留
//! 底部条带，最大化窗口就盖不满屏幕。因此隐藏成功后还要：
//! 1. `SPI_GETWORKAREA` 保存**原始**工作区（首次接管时，写进强杀标记）；
//! 2. `SPI_SETWORKAREA` 设为 `{0, 顶栏物理高度, 屏宽, 屏高}`（未预留顶栏时为 0）；
//! 3. watchdog 每 ≥1s 检查一次，被 Shell/Explorer 重置时重新应用；
//! 4. `restore_all()` 恢复原始工作区（正常退出 / 关闭开关 / 强杀后重启）。
//!
//! 关键：**必须避免任务栏窗口的可见性跳变**（见上文隐身模式），否则 Explorer
//! 会在每次 `SW_SHOW` 后把底部条带重新算回工作区，最大化窗口随之被压回。
//!
//! 注意：`SPI_SETWORKAREA` 只影响主显示器。为了释放副屏底部条带，隐藏任务栏时
//! 会临时设置任务栏 `ABS_AUTOHIDE`，让 Explorer 重算所有显示器工作区；原状态
//! 存内存 + 强杀标记，退出/下次启动恢复。见 `ensure_autohide_for_secondary()`。
//!
//! ## 托盘点击保持（v0.2.2）
//!
//! 托盘弹窗（Shell 的浮出菜单/音量/网络面板）Owner 是任务栏窗口：点击后如果
//! 任务栏被隐藏，弹窗会一起被隐藏/立即关闭，表现为「点托盘图标没反应」。
//! 因此 `begin_hold(alpha)` 提供一个可长期持有的 guard：任务栏保持「显示但
//! alpha=0 + 可命中」（用户不可见、UIA 与命中测试可用），`TaskbarHold` drop 时
//! 再恢复隐身（alpha=0 + 穿透）。引用计数（`BRIDGE_DEPTH`）保证多个线程同时
//! 持有时不会互相把任务栏藏掉。
#![allow(non_upper_case_globals)]

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use tauri::Manager;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, BOOL};
use windows::Win32::UI::Shell::{SHAppBarMessage, APPBARDATA};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetLayeredWindowAttributes, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect,
    IsWindow, IsWindowVisible, SetLayeredWindowAttributes, SetWindowLongPtrW, ShowWindow,
    SystemParametersInfoW, GWL_EXSTYLE, LWA_ALPHA, SM_CXSCREEN, SM_CYSCREEN, SPIF_SENDCHANGE,
    SPI_GETWORKAREA, SPI_SETWORKAREA, SW_HIDE, SW_SHOWNOACTIVATE, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    WS_EX_LAYERED, WS_EX_TRANSPARENT,
};

/// 主屏任务栏窗口类名
const PRIMARY_CLASS: &str = "Shell_TrayWnd";
/// 副屏任务栏窗口类名
const SECONDARY_CLASS: &str = "Shell_SecondaryTrayWnd";
/// 排障开关：完全跳过任务栏隐藏
const DISABLE_ENV: &str = "EASYDASH_DISABLE_TASKBAR_HIDE";
/// 桥接时是否真正渲染任务栏（默认 alpha=0，肉眼不可见）
const BRIDGE_VISIBLE_ENV: &str = "EASYDASH_TASKBAR_BRIDGE_VISIBLE";
/// 强杀残留标记文件名（放在配置目录）
const MARKER_FILE: &str = "taskbar-hidden.lock";
/// Shell AppBar：查询/设置任务栏状态（自动隐藏释放副屏工作区条带）
const ABM_GETSTATE: u32 = 0x4;
const ABM_SETSTATE: u32 = 0xA;
const ABS_AUTOHIDE: u32 = 0x1;
/// 当前会话由我们临时开启的 auto-hide 前状态；u32::MAX = 未改动
static ORIGINAL_TASKBAR_STATE: AtomicU32 = AtomicU32::new(u32::MAX);
/// 周期复查 auto-hide 是否被 Explorer 重置
static LAST_AUTOHIDE_CHECK: Mutex<Option<Instant>> = Mutex::new(None);
/// 桥接显示后等待任务栏 XAML/UIA 树就绪的时间
const BRIDGE_SETTLE_MS: u64 = 40;
/// 桥接扫描连续失败多少次后自动降级为「持久 alpha=0」模式
const BRIDGE_FAIL_LIMIT: u32 = 3;
/// watchdog 重新发现任务栏窗口的最小间隔（隐藏状态稳定时不必每 tick 枚举）
const WATCHDOG_SCAN_INTERVAL: Duration = Duration::from_millis(1000);
/// 工作区纠正的最小间隔（避免每秒都广播 WM_SETTINGCHANGE 刷屏）
const WORKAREA_CHECK_INTERVAL: Duration = Duration::from_millis(1000);
/// 多屏 auto-hide 复查间隔（Explorer 重启/用户改设置后补回）
const AUTOHIDE_CHECK_INTERVAL: Duration = Duration::from_secs(5);
/// 工作区比较容差（像素）：差异 ≤1px 视为相同
const WORKAREA_TOL: i32 = 1;

/// 配置目录（用于 marker 文件），setup 时由 lib.rs 注入
static CONFIG_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
/// 用户配置是否希望隐藏任务栏（apply 时写入，watchdog 据此工作）
static WANT_HIDDEN: AtomicBool = AtomicBool::new(false);
/// 当前是否真的处于隐藏状态（视觉不可见）
static HIDDEN: AtomicBool = AtomicBool::new(false);
/// 降级模式：持久 alpha=0（任务栏窗口保持 visible，但完全透明）
static PERSISTENT_ALPHA: AtomicBool = AtomicBool::new(false);
/// 主模式「隐身」：任务栏窗口保持 **visible**，但 alpha=0 且鼠标穿透。
///
/// 为什么不用 `SW_HIDE`（v0.2.2 真机结论）：任务栏一旦「隐藏→显示」跳变
/// （托盘镜像每 10s 桥接一次、每次托盘点击都会 SW_SHOW/SW_HIDE），Explorer 会
/// 立刻按它仍注册的 AppBar 把底部 48px 重新算回工作区；最大化窗口随之被压回
/// 1032px，且**不会自己长回来**。保持 visible 就没有跳变，工作区稳定在
/// `{0, 顶栏高, 屏宽, 屏高}`。
static VISIBLE_MODE: AtomicBool = AtomicBool::new(false);
/// 桥接/保持显示的层数（跨线程引用计数，watchdog 见 >0 时让路）
static BRIDGE_DEPTH: AtomicU32 = AtomicU32::new(0);
/// 保持显示期间任务栏使用的 alpha（0=不可见；255=可见兜底）
static HOLD_ALPHA: AtomicU8 = AtomicU8::new(0);
/// 连续桥接失败次数
static BRIDGE_FAILS: AtomicU32 = AtomicU32::new(0);
/// 桥接串行化：同一时刻只允许一个线程真正切换任务栏可见性
static BRIDGE_LOCK: Mutex<()> = Mutex::new(());
/// 我们修改过的窗口 → 修改前的 exstyle（恢复时精确还原；"没动过" = 不在表里）
static ORIG_EXSTYLE: Mutex<Option<HashMap<isize, isize>>> = Mutex::new(None);
/// 上一次 watchdog 枚举任务栏窗口的时间（限流）
static LAST_SCAN: Mutex<Option<Instant>> = Mutex::new(None);
/// 接管前的原始工作区（None = 尚未接管；恢复时写回并清空）
static ORIG_WORKAREA: Mutex<Option<RECT>> = Mutex::new(None);
/// 顶栏物理高度（reserveTopbarSpace=false 时为 0），由 lib.rs / commands.rs 推送
static TOPBAR_HEIGHT: AtomicI32 = AtomicI32::new(0);
/// 上一次工作区检查时间（限流 ≥1s）
static LAST_WORKAREA_CHECK: Mutex<Option<Instant>> = Mutex::new(None);

thread_local! {
    /// 本线程桥接嵌套深度（>0 表示本线程已经让任务栏处于显示状态）
    static LOCAL_BRIDGE: Cell<u32> = const { Cell::new(0) };
}

// ==================== 基础判断 ====================

/// 是否属于任务栏窗口类
fn is_taskbar_class(class: &str) -> bool {
    class == PRIMARY_CLASS || class == SECONDARY_CLASS
}

/// 排障环境变量是否要求完全跳过隐藏
fn disabled_by_env() -> bool {
    static DISABLED: OnceLock<bool> = OnceLock::new();
    *DISABLED.get_or_init(|| {
        matches!(
            std::env::var(DISABLE_ENV).as_deref(),
            Ok("1") | Ok("true") | Ok("TRUE")
        )
    })
}

/// 桥接期间任务栏的 alpha：默认 0（不可见），环境变量可改为 255（可见，排障用）
fn bridge_alpha() -> u8 {
    static ALPHA: OnceLock<u8> = OnceLock::new();
    *ALPHA.get_or_init(|| match std::env::var(BRIDGE_VISIBLE_ENV).as_deref() {
        Ok("1") | Ok("true") | Ok("TRUE") => 255,
        _ => 0,
    })
}

// ==================== 窗口发现 ====================

/// 枚举所有任务栏窗口（主屏 + 所有副屏）。返回的句柄可能已失效
/// （Explorer 重启），需要时用 `IsWindow` 过滤。
pub fn discover() -> Vec<HWND> {
    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let out = &mut *(lparam.0 as *mut Vec<HWND>);
        if is_taskbar_class(&super::common::class_name(hwnd)) {
            out.push(hwnd);
        }
        true.into()
    }

    let mut out: Vec<HWND> = Vec::new();
    let ctx = &mut out as *mut Vec<HWND>;
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(ctx as isize));
    }
    out
}

/// 仍然存活的、存活任务栏窗口列表
fn live_windows() -> Vec<HWND> {
    discover()
        .into_iter()
        .filter(|hwnd| unsafe { IsWindow(*hwnd) }.as_bool())
        .collect()
}

// ==================== exstyle / alpha 操作 ====================

fn exstyle(hwnd: HWND) -> isize {
    unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) }
}

fn layered_bit() -> isize {
    WS_EX_LAYERED.0 as isize
}

/// 记录窗口被我们修改前的 exstyle（只记第一次，恢复后移除）
fn remember_exstyle(hwnd: HWND, ex: isize) {
    if let Ok(mut guard) = ORIG_EXSTYLE.lock() {
        guard
            .get_or_insert_with(HashMap::new)
            .entry(hwnd.0 as isize)
            .or_insert(ex);
    }
}

fn take_exstyle(hwnd: HWND) -> Option<isize> {
    ORIG_EXSTYLE
        .lock()
        .ok()
        .and_then(|mut g| g.as_mut().and_then(|map| map.remove(&(hwnd.0 as isize))))
}

/// 读取当前 layered alpha；窗口没有 `WS_EX_LAYERED` 或读取失败返回 None
fn current_alpha(hwnd: HWND) -> Option<u8> {
    if exstyle(hwnd) & layered_bit() == 0 {
        return None;
    }
    let mut key = COLORREF(0);
    let mut alpha = 255u8;
    let mut flags = LWA_ALPHA;
    unsafe {
        GetLayeredWindowAttributes(hwnd, Some(&mut key), Some(&mut alpha), Some(&mut flags)).ok()?;
    }
    Some(alpha)
}

/// 把任务栏设为指定 alpha（同时确保 `WS_EX_LAYERED` 已置位）
fn set_alpha(hwnd: HWND, alpha: u8) -> bool {
    let ex = exstyle(hwnd);
    if ex == 0 {
        return false;
    }
    remember_exstyle(hwnd, ex);
    if ex & layered_bit() == 0 {
        unsafe {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | layered_bit());
        }
    }
    unsafe {
        SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA).is_ok()
    }
}

/// 只在 alpha 不是 0 时才动窗口，避免每 400ms 触发一次 `WM_STYLECHANGED`
fn ensure_alpha0(hwnd: HWND) -> bool {
    if current_alpha(hwnd) == Some(0) {
        return true;
    }
    set_alpha(hwnd, 0)
}

// ==================== 隐身模式（visible + alpha=0 + 鼠标穿透） ====================

fn transparent_bit() -> isize {
    WS_EX_TRANSPARENT.0 as isize
}

/// 开关鼠标穿透（保留 `WS_EX_LAYERED`）。返回操作后是否达到期望状态。
///
/// 隐身模式下必须穿透：任务栏窗口虽然 visible，用户点击应落到它下面的窗口
/// （最大化窗口的底部条带仍然可点）。托盘点击时反过来临时关闭穿透，
/// 让真实指针事件能命中任务栏——只改 exstyle、不改可见性，不会触发
/// Explorer 重算工作区。
fn set_click_through(hwnd: HWND, on: bool) -> bool {
    let ex = exstyle(hwnd);
    if ex == 0 {
        return false;
    }
    remember_exstyle(hwnd, ex);
    let want = if on {
        ex | transparent_bit()
    } else {
        ex & !transparent_bit()
    };
    if want != ex {
        unsafe {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want);
        }
    }
    let now = exstyle(hwnd);
    if on {
        now & transparent_bit() != 0
    } else {
        now & transparent_bit() == 0
    }
}

/// 让任务栏进入「隐身」状态：visible + alpha=0 + 鼠标穿透。
///
/// 返回 false 表示该窗口没能进入隐身状态（调用方决定是否整体降级）。
fn enter_invisible(hwnd: HWND) -> bool {
    ensure_alpha0(hwnd);
    let click_through = set_click_through(hwnd, true);
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }
    click_through && unsafe { IsWindowVisible(hwnd) }.as_bool() && current_alpha(hwnd) == Some(0)
}

/// 隐身模式下窗口是否已就绪（visible + alpha=0 + 穿透）
fn invisible_state_ok(hwnd: HWND) -> bool {
    unsafe { IsWindowVisible(hwnd) }.as_bool()
        && current_alpha(hwnd) == Some(0)
        && exstyle(hwnd) & transparent_bit() != 0
}

// ==================== 工作区接管（让最大化窗口占满全屏） ====================

/// 期望工作区：`{0, 顶栏物理高度, 屏宽, 屏高}`（纯函数，便于单测）
fn desired_workarea_rect(screen_w: i32, screen_h: i32, topbar_h: i32) -> RECT {
    let sh = screen_h.max(0);
    let top = topbar_h.max(0).min(sh);
    RECT {
        left: 0,
        top,
        right: screen_w.max(0),
        bottom: sh,
    }
}

/// 两个矩形在容差内是否相同（纯函数）
fn workarea_matches(a: &RECT, b: &RECT, tol: i32) -> bool {
    (a.left - b.left).abs() <= tol
        && (a.top - b.top).abs() <= tol
        && (a.right - b.right).abs() <= tol
        && (a.bottom - b.bottom).abs() <= tol
}

/// 标记文件里的工作区行
fn format_workarea_line(rect: &RECT) -> String {
    format!(
        "workarea={},{},{},{}",
        rect.left, rect.top, rect.right, rect.bottom
    )
}

/// 标记文件里的任务栏 AppBar 状态行（用于强杀后恢复 auto-hide）
fn format_taskbar_state_line(state: u32) -> String {
    format!("taskbar_state={state}")
}

/// 解析 `taskbar_state=N`；旧标记没有该行 → None
fn parse_marker_taskbar_state(text: &str) -> Option<u32> {
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("taskbar_state=") else {
            continue;
        };
        return rest.trim().parse::<u32>().ok();
    }
    None
}

/// 查询任务栏 AppBar 状态位（ABS_AUTOHIDE / ABS_ALWAYSONTOP）
fn taskbar_appbar_state() -> u32 {
    let mut data = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        ..Default::default()
    };
    unsafe { SHAppBarMessage(ABM_GETSTATE, &mut data) as u32 }
}

/// 设置任务栏 AppBar 状态位；失败只记录日志，不阻断隐藏流程
fn set_taskbar_appbar_state(state: u32) {
    let Some(hwnd) = live_windows()
        .into_iter()
        .find(|hwnd| {
            super::common::class_name(*hwnd) == PRIMARY_CLASS
                && super::common::window_pid(*hwnd) != std::process::id()
        })
    else {
        log_hide_failure("设置任务栏 auto-hide 失败：未找到主任务栏窗口");
        return;
    };
    let mut data = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        lParam: LPARAM(state as isize),
        ..Default::default()
    };
    unsafe {
        SHAppBarMessage(ABM_SETSTATE, &mut data);
    }
}

/// 多显示器且副屏任务栏仍占着工作区时，临时开启任务栏 auto-hide。
///
/// Windows 的 `SPI_SETWORKAREA` 只作用于主屏；副屏最大化窗口因此永远停在
/// 任务栏上沿，底部留下一条空白。`ABM_SETSTATE(ABS_AUTOHIDE)` 会让 Explorer
/// 以“任务栏可自动隐藏”重新计算**所有**显示器的工作区，副屏即可铺满。
/// 原状态记在内存 + marker 里，退出/下次启动恢复。
fn ensure_autohide_for_secondary() {
    let has_secondary = live_windows().iter().any(|hwnd| {
        super::common::class_name(*hwnd) == SECONDARY_CLASS
    });
    if !has_secondary {
        return;
    }
    let state = taskbar_appbar_state();
    if state & ABS_AUTOHIDE != 0 {
        return;
    }
    let _ = ORIGINAL_TASKBAR_STATE.compare_exchange(
        u32::MAX,
        state,
        Ordering::SeqCst,
        Ordering::SeqCst,
    );
    set_taskbar_appbar_state(state | ABS_AUTOHIDE);
}

/// 在改动 auto-hide 之前记住主屏原始工作区，避免把“全屏工作区”当成原始值。
fn remember_original_workarea() {
    if let Ok(mut guard) = ORIG_WORKAREA.lock() {
        if guard.is_none() {
            *guard = get_workarea();
        }
    }
}

/// 启动早期（注册顶栏 AppBar / 隐藏任务栏之前）记录真正的桌面工作区。
/// 否则 `ORIG_WORKAREA` 会带上顶栏预留的 48px，退出后主屏顶部留白。
pub fn capture_original_workarea() {
    remember_original_workarea();
}

/// 恢复任务栏 auto-hide 原状态（正常退出或强杀后的 marker 恢复）。
fn restore_taskbar_state() {
    let mut original = ORIGINAL_TASKBAR_STATE.swap(u32::MAX, Ordering::SeqCst);
    if original == u32::MAX {
        original = read_marker_taskbar_state().unwrap_or(u32::MAX);
    }
    if original != u32::MAX {
        set_taskbar_appbar_state(original);
    }
}

/// 解析标记文件里的 `workarea=l,t,r,b` 行（兼容没有该行的旧标记 → None）
fn parse_marker_workarea(text: &str) -> Option<RECT> {
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("workarea=") else {
            continue;
        };
        let nums: Vec<i32> = rest
            .split(',')
            .filter_map(|part| part.trim().parse::<i32>().ok())
            .collect();
        if nums.len() != 4 {
            return None;
        }
        let rect = RECT {
            left: nums[0],
            top: nums[1],
            right: nums[2],
            bottom: nums[3],
        };
        if rect.right - rect.left <= 0 || rect.bottom - rect.top <= 0 {
            return None;
        }
        return Some(rect);
    }
    None
}

/// 读取当前主屏工作区（SPI_GETWORKAREA）
fn get_workarea() -> Option<RECT> {
    let mut rect = RECT::default();
    unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut rect as *mut RECT as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .ok()?;
    Some(rect)
}

/// 设置主屏工作区。
///
/// `notify=false` 时静默设置（不广播 WM_SETTINGCHANGE / SPIF_SENDCHANGE）。
/// 实测：广播会让 Explorer 立刻按仍注册的 AppBar（隐藏的任务栏）重算工作区，
/// 把我们刚设的值覆盖回原始值（真机探针 A 变体）；静默设置才会真正生效，
/// watchdog 以 ≥1s 间隔复查并在被重置时再次纠正（旧窗口需重新最大化才会铺满）。
fn set_workarea(rect: &RECT, notify: bool) -> bool {
    let flags = if notify {
        SPIF_SENDCHANGE
    } else {
        SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)
    };
    unsafe {
        SystemParametersInfoW(
            SPI_SETWORKAREA,
            0,
            Some(rect as *const RECT as *mut core::ffi::c_void),
            flags,
        )
    }
    .is_ok()
}

/// 主显示器屏幕尺寸（物理像素）
fn primary_screen_size() -> (i32, i32) {
    unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
}

/// 当前期望的工作区（按已推送的顶栏高度计算）
fn desired_workarea() -> RECT {
    let (sw, sh) = primary_screen_size();
    desired_workarea_rect(sw, sh, TOPBAR_HEIGHT.load(Ordering::Relaxed))
}

/// 主屏任务栏窗口矩形（用于兜底推算原始工作区；任务栏不可见时返回 None）
fn primary_taskbar_rect() -> Option<RECT> {
    let hwnd = discover()
        .into_iter()
        .find(|hwnd| super::common::class_name(*hwnd) == PRIMARY_CLASS)?;
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return None;
    }
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    if rect.right - rect.left <= 0 || rect.bottom - rect.top <= 0 {
        return None;
    }
    Some(rect)
}

/// 兜底：按（已恢复可见的）主屏任务栏矩形推算原始工作区。
/// 只在没有内存记录、也没有标记文件记录时使用（例如 v0.2.1 的旧标记）。
fn estimate_original_workarea() -> RECT {
    let (sw, sh) = primary_screen_size();
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: sw,
        bottom: sh,
    };
    if let Some(tb) = primary_taskbar_rect() {
        let is_bottom = tb.bottom >= sh - 2 && tb.top > sh / 2;
        let is_top = tb.top <= 2 && tb.bottom < sh / 2;
        let is_left = tb.left <= 2 && tb.right < sw / 2;
        let is_right = tb.right >= sw - 2 && tb.left > sw / 2;
        if is_bottom {
            rect.bottom = tb.top;
        } else if is_top {
            rect.top = tb.bottom;
        } else if is_left {
            rect.left = tb.right;
        } else if is_right {
            rect.right = tb.left;
        }
    }
    rect
}

/// 接管主屏工作区：首次调用保存原始矩形，之后设置成 `desired`。
/// `force=false` 时受 1 秒限流约束（watchdog 用），避免每秒广播刷屏。
fn enforce_workarea(force: bool) {
    let Some(current) = get_workarea() else {
        return;
    };
    let desired = desired_workarea();
    if workarea_matches(&current, &desired, WORKAREA_TOL) {
        return; // 已经是目标值（原始值在首次接管时已保存）
    }
    // 首次接管前保存原始工作区；之后不再覆盖，避免把我们的目标当成原始值
    if let Ok(mut guard) = ORIG_WORKAREA.lock() {
        if guard.is_none() {
            *guard = Some(current);
        }
    }
    if !force {
        let now = Instant::now();
        if let Ok(mut guard) = LAST_WORKAREA_CHECK.lock() {
            if let Some(at) = *guard {
                if now.duration_since(at) < WORKAREA_CHECK_INTERVAL {
                    return;
                }
            }
            *guard = Some(now);
        }
    } else if let Ok(mut guard) = LAST_WORKAREA_CHECK.lock() {
        *guard = Some(Instant::now());
    }
    if !set_workarea(&desired, false) {
        log_hide_failure("设置工作区失败（SPI_SETWORKAREA），底部可能仍留任务栏条带");
    }
}

/// 恢复原始工作区：内存记录 → 标记文件 → 按任务栏矩形兜底推算。
fn restore_workarea() {
    let saved = ORIG_WORKAREA.lock().ok().and_then(|mut g| g.take());
    let target = saved
        .or_else(read_marker_workarea)
        .unwrap_or_else(estimate_original_workarea);
    if let Some(current) = get_workarea() {
        if workarea_matches(&current, &target, WORKAREA_TOL) {
            if let Ok(mut guard) = LAST_WORKAREA_CHECK.lock() {
                *guard = None;
            }
            return;
        }
    }
    set_workarea(&target, true);
    if let Ok(mut guard) = LAST_WORKAREA_CHECK.lock() {
        *guard = None;
    }
}

/// 读取标记文件里保存的原始工作区
fn read_marker_workarea() -> Option<RECT> {
    let path = marker_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    parse_marker_workarea(&text)
}

fn read_marker_taskbar_state() -> Option<u32> {
    let path = marker_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    parse_marker_taskbar_state(&text)
}

/// 读取顶栏窗口当前物理高度（GetWindowRect）。
/// 顶栏面板展开（>200px）时工作区预留仍按常态高度（32 逻辑像素 × DPI）计算。
pub fn topbar_physical_height(win: &tauri::WebviewWindow) -> i32 {
    let Some(hwnd) = super::common::tauri_hwnd(win) else {
        return 0;
    };
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return 0;
    }
    let height = rect.bottom - rect.top;
    if height <= 0 {
        return 0;
    }
    if height > 200 {
        let scale = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) } as f64 / 96.0;
        return (32.0 * scale).round() as i32;
    }
    height
}

/// 设置顶栏物理高度（0 = 不预留顶栏空间）。隐藏状态会立即纠正工作区。
pub fn set_topbar_height(px: i32) {
    let next = px.max(0);
    let prev = TOPBAR_HEIGHT.swap(next, Ordering::SeqCst);
    if prev != next && (HIDDEN.load(Ordering::SeqCst) || PERSISTENT_ALPHA.load(Ordering::SeqCst)) {
        enforce_workarea(true);
    }
}

/// 按「配置 reserveTopbarSpace + 主屏顶栏实际物理高度」更新工作区接管参数。
/// lib.rs setup、commands::update_preferences、do_reload_config 调用。
pub fn update_topbar_height(app: &tauri::AppHandle) {
    let reserve = app
        .state::<crate::app_state::AppState>()
        .config
        .lock()
        .map(|cfg| cfg.reserve_topbar_space)
        .unwrap_or(false);
    let px = if reserve {
        crate::surfaces::primary_topbar_window(app)
            .map(|win| topbar_physical_height(&win))
            .unwrap_or(0)
    } else {
        0
    };
    set_topbar_height(px);
}

// ==================== 隐藏 / 恢复 ====================

/// 隐藏失败日志限流（watchdog 每 tick 都会重试，避免刷屏）
static LAST_HIDE_FAIL_LOG: Mutex<Option<Instant>> = Mutex::new(None);

/// 隐藏失败日志（30 秒最多一条）
fn log_hide_failure(msg: &str) {
    if let Ok(mut guard) = LAST_HIDE_FAIL_LOG.lock() {
        let due = guard
            .as_ref()
            .map(|at| at.elapsed() >= Duration::from_secs(30))
            .unwrap_or(true);
        if due {
            *guard = Some(Instant::now());
            eprintln!("[easy-dash] {msg}");
        }
    }
}

/// 任务栏窗口是否全部不可见（没有窗口时返回 false）
fn windows_all_invisible(wins: &[HWND]) -> bool {
    !wins.is_empty()
        && wins
            .iter()
            .all(|h| !unsafe { IsWindowVisible(*h) }.as_bool())
}

fn hide_one(hwnd: HWND) -> bool {
    ensure_alpha0(hwnd);
    if unsafe { IsWindowVisible(hwnd) }.as_bool() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
    // 跨进程 ShowWindow 可能被 UIPI 拒绝（返回成功但窗口仍可见），这里以实际状态为准
    !unsafe { IsWindowVisible(hwnd) }.as_bool()
}

/// 恢复单个窗口。`force` 表示“我们（或上次运行）确实隐藏过它”，
/// 即使没有 exstyle 记录（例如上次进程被强杀、Explorer 重启换了 HWND）
/// 也要把 alpha / 可见性还原。
fn restore_one(hwnd: HWND, force: bool) {
    let saved = take_exstyle(hwnd);
    let ex = exstyle(hwnd);
    match saved {
        Some(orig) => unsafe {
            // 原本就是 layered 的窗口：alpha 也一起还原成不透明
            if orig & layered_bit() != 0 {
                let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
            }
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, orig);
        },
        None => {
            // 没有原始记录：只有在确认是我们把 alpha 置 0 的情况下才动风格
            if force && ex & (layered_bit() | transparent_bit()) != 0 && current_alpha(hwnd) == Some(0)
            {
                unsafe {
                    let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex & !layered_bit() & !transparent_bit());
                }
            }
        }
    }
    if force || saved.is_some() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }
}

fn marker_path() -> Option<PathBuf> {
    CONFIG_DIR
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .map(|dir| dir.join(MARKER_FILE))
}

fn marker_exists() -> bool {
    marker_path().map(|p| p.exists()).unwrap_or(false)
}

/// 写入强杀残留标记（隐藏成功后调用）。
///
/// 同时记录**原始工作区**：`SPI_SETWORKAREA` 不是持久设置，进程被强杀后
/// 下次启动只能靠这条记录把工作区还原（否则底部条带一直缺失）。
fn mark_hidden() {
    let Some(path) = marker_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let workarea = ORIG_WORKAREA
        .lock()
        .ok()
        .and_then(|guard| *guard)
        .or_else(get_workarea);
    let mut text = String::from("easy-dash: windows taskbar is hidden\n");
    let orig_state = ORIGINAL_TASKBAR_STATE.load(Ordering::SeqCst);
    if orig_state != u32::MAX {
        text.push_str(&format_taskbar_state_line(orig_state));
        text.push('\n');
    }
    if let Some(rect) = workarea {
        text.push_str(&format_workarea_line(&rect));
        text.push('\n');
    }
    let _ = std::fs::write(&path, text);
}

/// 删除强杀残留标记（恢复后调用）
fn clear_marker() {
    if let Some(path) = marker_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// 主屏当前工作区底部（物理像素）。任务栏显隐/工作区接管会改变它，
/// Dock 需要贴着工作区底边而不是屏幕底边，否则任务栏可见时会被压住、
/// 启动瞬间（接管前）也会定位到旧工作区底部。
pub fn workarea_bottom_physical() -> Option<i32> {
    get_workarea().map(|r| r.bottom)
}

/// 注入配置目录（marker 文件位置）；lib.rs setup 时调用。
pub fn set_config_dir(dir: PathBuf) {
    if let Ok(mut guard) = CONFIG_DIR.lock() {
        *guard = Some(dir);
    }
}

/// 配置目录（托盘点击诊断日志等复用）
pub fn config_dir() -> Option<PathBuf> {
    CONFIG_DIR.lock().ok().and_then(|guard| guard.clone())
}

/// 上次运行被强杀（marker 还在）时先恢复任务栏。
/// 必须在 `apply` 之前调用：用户即使把配置改成“不隐藏”，也不会卡在无任务栏状态。
pub fn recover_stale_lock() {
    if marker_exists() {
        eprintln!("[easy-dash] 检测到上次异常退出遗留的任务栏隐藏标记，先恢复任务栏");
        restore_all();
    }
}

/// 按配置应用隐藏 / 恢复。返回 Err 表示当前无法隐藏（找不到任务栏窗口等），
/// 但调用方仍会保留用户的配置意图：watchdog 会在窗口出现后自动重试。
pub fn apply(hide: bool) -> Result<(), String> {
    if !hide {
        restore_all();
        return Ok(());
    }
    if disabled_by_env() {
        WANT_HIDDEN.store(false, Ordering::SeqCst);
        HIDDEN.store(false, Ordering::SeqCst);
        eprintln!("[easy-dash] {DISABLE_ENV}=1 已禁用任务栏隐藏");
        return Ok(());
    }
    // 先记住“用户想要隐藏”，即使本次窗口还没就绪，watchdog 也会继续尝试
    WANT_HIDDEN.store(true, Ordering::SeqCst);
    PERSISTENT_ALPHA.store(false, Ordering::SeqCst);
    VISIBLE_MODE.store(false, Ordering::SeqCst);
    BRIDGE_FAILS.store(0, Ordering::SeqCst);

    let wins = live_windows();
    if wins.is_empty() {
        return Err("未找到任务栏窗口（Shell_TrayWnd），稍后由看门狗重试".into());
    }

    // 崩溃残留自愈：上个进程可能死在隐身状态（alpha=0 + 穿透）却没留下可用的
    // 原始 exstyle 记录。此时把「去掉 layered+穿透」当作恢复目标，否则恢复后
    // 会出现「任务栏可见但点不动」的假正常状态。
    for hwnd in &wins {
        let ex = exstyle(*hwnd);
        if current_alpha(*hwnd) == Some(0) && ex & transparent_bit() != 0 {
            remember_exstyle(*hwnd, ex & !layered_bit() & !transparent_bit());
        }
    }

    // 主路径：隐身接管（visible + alpha=0 + 鼠标穿透），不做 SW_HIDE。
    // 保持「可见」就消除了 SW_SHOW/SW_HIDE 跳变，Explorer 不会按 AppBar
    // 把底部 48px 重新算回工作区，最大化窗口才能稳定铺满全屏。
    let mut invisible = 0usize;
    for hwnd in &wins {
        if enter_invisible(*hwnd) {
            invisible += 1;
        }
    }
    if invisible > 0 {
        VISIBLE_MODE.store(true, Ordering::SeqCst);
        HIDDEN.store(true, Ordering::SeqCst);
        if invisible < wins.len() {
            log_hide_failure(&format!(
                "部分任务栏窗口未能进入隐身模式（{invisible}/{}），可能残留可见任务栏",
                wins.len()
            ));
        }
        // 接管工作区：先记住原始值，再让副屏也释放任务栏条带
        remember_original_workarea();
        ensure_autohide_for_secondary();
        enforce_workarea(true);
        mark_hidden();
        return Ok(());
    }

    // 兜底：隐身不可用（例如置 layered/穿透被 UIPI 拒绝）时退回 SW_HIDE。
    // 注意这条路径仍有「工作区被 Explorer 重置」的已知抖动，仅作降级。
    let mut hidden = 0usize;
    let mut last_error = 0u32;
    for hwnd in &wins {
        if hide_one(*hwnd) {
            hidden += 1;
        } else {
            last_error = unsafe { windows::Win32::Foundation::GetLastError() }.0;
        }
    }
    if hidden == 0 {
        HIDDEN.store(false, Ordering::SeqCst);
        let msg = format!(
            "隐藏任务栏失败（Win32 错误码 {last_error}）；请确认 easy-dash 与 explorer.exe 完整性级别一致，后台会持续重试"
        );
        log_hide_failure(&msg);
        return Err(msg);
    }
    VISIBLE_MODE.store(false, Ordering::SeqCst);
    HIDDEN.store(true, Ordering::SeqCst);
    // 接管工作区：先记住原始值，再让副屏也释放任务栏条带
    remember_original_workarea();
    ensure_autohide_for_secondary();
    enforce_workarea(true);
    mark_hidden();
    Ok(())
}

/// 恢复任务栏（退出 / 关闭开关时调用）。重复调用无副作用。
pub fn restore_all() {
    // 与 begin_hold / TaskbarHold::drop 串行：否则「恢复写到一半」可能被 10s 一次的
    // 托盘桥接重新置成 alpha=0/穿透，而标记已被删除 → 用户桌面永久失去任务栏。
    let _bridge_guard: MutexGuard<'static, ()> =
        BRIDGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let was_hidden = HIDDEN.load(Ordering::SeqCst)
        || PERSISTENT_ALPHA.load(Ordering::SeqCst)
        || VISIBLE_MODE.load(Ordering::SeqCst)
        || ORIGINAL_TASKBAR_STATE.load(Ordering::SeqCst) != u32::MAX
        || marker_exists();
    // 先关掉“想隐藏”：否则 watchdog 可能在 restore_one 清掉 layered 之后、
    // restore_all 结束之前又把任务栏藏回去（marker 已删但任务栏不可见）。
    WANT_HIDDEN.store(false, Ordering::SeqCst);
    HIDDEN.store(false, Ordering::SeqCst);
    VISIBLE_MODE.store(false, Ordering::SeqCst);
    PERSISTENT_ALPHA.store(false, Ordering::SeqCst);
    if was_hidden {
        for hwnd in live_windows() {
            restore_one(hwnd, true);
        }
    } else {
        // 没隐藏过：只还原我们自己动过风格的窗口（正常不会发生）
        for hwnd in live_windows() {
            if ORIG_EXSTYLE
                .lock()
                .ok()
                .map(|g| {
                    g.as_ref()
                        .map(|m| m.contains_key(&(hwnd.0 as isize)))
                        .unwrap_or(false)
                })
                .unwrap_or(false)
            {
                restore_one(hwnd, true);
            }
        }
    }
    if let Ok(mut guard) = ORIG_EXSTYLE.lock() {
        *guard = None;
    }
    // 恢复原始工作区（必须在 clear_marker 之前读标记里的记录）。
    // 只有确实隐藏/接管过才动工作区：未隐藏时的 apply(false) 绝不能乱改系统设置。
    if was_hidden {
        // 先恢复 auto-hide 原状态：Explorer 会重算所有屏工作区，再把主屏改回原值
        restore_taskbar_state();
        restore_workarea();
    }
    PERSISTENT_ALPHA.store(false, Ordering::SeqCst);
    VISIBLE_MODE.store(false, Ordering::SeqCst);
    HIDDEN.store(false, Ordering::SeqCst);
    WANT_HIDDEN.store(false, Ordering::SeqCst);
    BRIDGE_FAILS.store(0, Ordering::SeqCst);
    clear_marker();
}

// ==================== watchdog ====================

fn should_scan(force: bool) -> bool {
    if force {
        return true;
    }
    let guard = LAST_SCAN.lock().unwrap_or_else(|e| e.into_inner());
    match *guard {
        Some(at) if at.elapsed() < WATCHDOG_SCAN_INTERVAL => false,
        _ => true,
    }
}

fn mark_scanned() {
    if let Ok(mut guard) = LAST_SCAN.lock() {
        *guard = Some(Instant::now());
    }
}

/// 周期看门狗（monitor 线程每 400ms 调用）：
/// - 桥接进行中直接让路（否则会把正在被 UIA 使用的窗口重新藏起来）；
/// - 补回被 Explorer 重置的 layered/alpha/穿透风格；
/// - 任务栏窗口被重新显示时再藏一次；
/// - Explorer 重启后重新发现窗口（旧 HWND 已被 `IsWindow` 过滤掉）；
/// - 隐身模式保持「visible + alpha=0 + 穿透」，降级模式保持「可见 + alpha=0」。
pub fn watchdog() {
    // 恢复/桥接持有 BRIDGE_LOCK 时直接让路：尤其 restore_all 清掉 layered 后，
    // watchdog 若并发进入会把任务栏重新藏回去，造成“退出后任务栏不恢复”。
    let Ok(_bridge_guard) = BRIDGE_LOCK.try_lock() else {
        return;
    };
    if !WANT_HIDDEN.load(Ordering::Relaxed) {
        return;
    }
    if BRIDGE_DEPTH.load(Ordering::Relaxed) > 0 {
        return;
    }
    // 工作区纠正独立于窗口枚举：Shell/Explorer 可能把工作区改回「任务栏可见」状态
    if HIDDEN.load(Ordering::Relaxed)
        || PERSISTENT_ALPHA.load(Ordering::Relaxed)
        || VISIBLE_MODE.load(Ordering::Relaxed)
    {
        enforce_workarea(false);
    }

    // 多屏 auto-hide 被 Explorer 重置时补回；否则副屏底部条带会重新出现
    if ORIGINAL_TASKBAR_STATE.load(Ordering::Relaxed) != u32::MAX {
        let due = {
            let mut guard = LAST_AUTOHIDE_CHECK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            match *guard {
                Some(at) if at.elapsed() < AUTOHIDE_CHECK_INTERVAL => false,
                _ => {
                    *guard = Some(Instant::now());
                    true
                }
            }
        };
        if due {
            let state = taskbar_appbar_state();
            if state & ABS_AUTOHIDE == 0 {
                set_taskbar_appbar_state(state | ABS_AUTOHIDE);
                enforce_workarea(true);
            }
        }
    }

    // 隐身模式：只要维持 visible + alpha=0 + 穿透即可，不做任何 SW_HIDE/SW_SHOW
    if VISIBLE_MODE.load(Ordering::Relaxed) {
        if !should_scan(false) {
            return;
        }
        let wins = live_windows();
        if wins.is_empty() {
            return;
        }
        mark_scanned();
        let mut failed = 0u32;
        for hwnd in &wins {
            if !invisible_state_ok(*hwnd) && !enter_invisible(*hwnd) {
                failed += 1;
            }
        }
        if failed > 0 {
            log_hide_failure(
                "watchdog 无法维持任务栏隐身（Win32 权限/完整性不一致？），将持续重试",
            );
        }
        // 隐身模式下任务栏窗口始终 visible，但用户视觉上不可见
        HIDDEN.store(true, Ordering::Relaxed);
        return;
    }

    let was_hidden = HIDDEN.load(Ordering::Relaxed);
    // 还没隐藏成功时每个 tick 都重新发现（等待 Explorer/任务栏就绪）
    if !should_scan(!was_hidden) {
        return;
    }
    let wins = live_windows();
    if wins.is_empty() {
        return;
    }
    mark_scanned();

    let persistent = PERSISTENT_ALPHA.load(Ordering::Relaxed);
    let mut failed = 0u32;
    for hwnd in &wins {
        ensure_alpha0(*hwnd);
        let visible = unsafe { IsWindowVisible(*hwnd) }.as_bool();
        if persistent {
            // 降级模式：保持 visible（UIA / 命中测试始终可用）
            if !visible {
                unsafe {
                    let _ = ShowWindow(*hwnd, SW_SHOWNOACTIVATE);
                }
            }
        } else if visible {
            unsafe {
                let _ = ShowWindow(*hwnd, SW_HIDE);
            }
            if unsafe { IsWindowVisible(*hwnd) }.as_bool() {
                failed += 1;
            }
        }
    }
    // 以实际可见性为准，避免跨进程操作被 UIPI 拒绝时误报“已隐藏”
    let hidden_now = if persistent {
        true
    } else {
        windows_all_invisible(&wins)
    };
    HIDDEN.store(hidden_now, Ordering::Relaxed);
    if hidden_now {
        if !was_hidden {
            // 首次成功（可能是 apply 失败后由 watchdog 补上）：补写强杀标记
            mark_hidden();
        }
    } else if failed > 0 {
        log_hide_failure(
            "watchdog 无法隐藏任务栏（Win32 权限/完整性不一致？），将持续重试",
        );
    }
}

// ==================== 保持显示：TaskbarHold（桥接 / 托盘点击共用） ====================

fn needs_bridge() -> bool {
    HIDDEN.load(Ordering::Relaxed) && !PERSISTENT_ALPHA.load(Ordering::Relaxed)
}

/// 让所有任务栏窗口以指定 alpha 显示（alpha=0 时肉眼不可见但可命中）。
///
/// 注意：**不改鼠标穿透**——枚举只用 UIA，不需要命中测试；真实点击由
/// `clickable_guard()` 单独关穿透。否则每次 10s 枚举都会在底部制造死区。
fn show_taskbar(alpha: u8) {
    for hwnd in live_windows() {
        set_alpha(hwnd, alpha);
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }
}

/// 把所有任务栏窗口的 alpha 调整为指定值（用于可见兜底升级）
fn set_taskbar_alpha(alpha: u8) {
    for hwnd in live_windows() {
        set_alpha(hwnd, alpha);
    }
}

/// 保持显示期间提升/降低任务栏 alpha（托盘右键菜单等「必须真正可见」的兜底场景）。
///
/// 仅在有 hold 时生效；hold 结束后由 `hide_taskbar_again()` 统一恢复隐身。
pub fn set_hold_alpha(alpha: u8) {
    if BRIDGE_DEPTH.load(Ordering::Relaxed) == 0 {
        return;
    }
    HOLD_ALPHA.store(alpha, Ordering::SeqCst);
    set_taskbar_alpha(alpha);
}

/// 重新隐藏任务栏（保持显示结束且用户仍希望隐藏时）
fn hide_taskbar_again() {
    if !WANT_HIDDEN.load(Ordering::Relaxed) {
        return;
    }
    if VISIBLE_MODE.load(Ordering::Relaxed) {
        // 隐身模式：恢复 visible + alpha=0 + 穿透，不做可见性切换
        for hwnd in live_windows() {
            enter_invisible(hwnd);
        }
        return;
    }
    if PERSISTENT_ALPHA.load(Ordering::Relaxed) {
        // 降级模式不切换可见性，只确保仍是全透明（并保持穿透，底部才点得到下层窗口）
        for hwnd in live_windows() {
            ensure_alpha0(hwnd);
            set_click_through(hwnd, true);
        }
        return;
    }
    for hwnd in live_windows() {
        ensure_alpha0(hwnd);
        if unsafe { IsWindowVisible(hwnd) }.as_bool() {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
}

/// 让任务栏保持「显示」状态的 RAII guard，Drop 时（若已无其它持有者）恢复隐藏。
///
/// 用途：
/// - 桥接（`with_taskbar_visible`）：UIA 枚举 / 真实鼠标点击需要任务栏可见；
/// - 托盘点击（`begin_hold`）：Shell 托盘弹窗 Owner 是任务栏窗口，点击后必须
///   保持任务栏显示（alpha=0 不可见）直到弹窗关闭，否则弹窗会被一起隐藏。
///
/// 多个线程可同时持有：内部引用计数（`BRIDGE_DEPTH`）保证最后一个 drop 才真正
/// `SW_HIDE`；持有期间 `watchdog` 自动让路，不会把任务栏抢着藏回去。
pub struct TaskbarHold {
    /// false = 未隐藏 / 降级模式下无需切换可见性（空 guard）
    active: bool,
}

impl Drop for TaskbarHold {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        LOCAL_BRIDGE.with(|depth| depth.set(depth.get().saturating_sub(1)));
        // 引用计数归零才恢复隐藏；saturating 防下溢
        let prev = BRIDGE_DEPTH.try_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
            Some(value.saturating_sub(1))
        });
        if !matches!(prev, Ok(1)) {
            return;
        }
        let _lock: MutexGuard<'static, ()> = BRIDGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // 双检：释放锁前可能有新持有者进来
        if BRIDGE_DEPTH.load(Ordering::SeqCst) == 0 {
            HOLD_ALPHA.store(0, Ordering::SeqCst);
            hide_taskbar_again();
        }
    }
}

/// 显示任务栏并保持，返回的 guard drop 时恢复隐藏。
///
/// - 未隐藏 / 持久 alpha 降级模式：直接返回空 guard（不改变可见性）；
/// - `alpha=0`：显示但完全透明（用户不可见，UIA/命中测试可用，托盘点击用）；
/// - `alpha=255`：真正可见（可见兜底重试用）。
///
/// 锁只在切换可见性时短暂持有：长时间 hold（等弹窗关闭）不会阻塞其它调用。
pub fn begin_hold(alpha: u8) -> TaskbarHold {
    if !WANT_HIDDEN.load(Ordering::Relaxed) {
        return TaskbarHold { active: false };
    }
    // 隐身模式：窗口本来就 visible，无需切换可见性；同时强制 alpha=0——
    // 调用方传 255 是给 SW_HIDE 模式的「可见兜底」，在隐身模式下只会让
    // 用户看到任务栏闪现，必须忽略。
    let visible_mode = VISIBLE_MODE.load(Ordering::Relaxed);
    if !visible_mode && !needs_bridge() {
        // 尚未隐藏成功 / 已降级为持久 alpha：不切换可见性（原行为）
        return TaskbarHold { active: false };
    }
    // 注意：**不要**把 alpha 夹成 0。托盘点击的「可见兜底」会用 alpha=255 把任务栏
    // 真正显示出来——alpha=0 时 Shell 的托盘菜单常常画不出来/不可见（用户看到的
    // 就是「光标跳到右下角但什么都没出现」）。只有最后一次兜底才会走到 255。
    let first = BRIDGE_DEPTH.fetch_add(1, Ordering::SeqCst) == 0;
    {
        let _lock: MutexGuard<'static, ()> = BRIDGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if first {
            HOLD_ALPHA.store(alpha, Ordering::SeqCst);
            show_taskbar(alpha);
        } else if alpha > HOLD_ALPHA.load(Ordering::SeqCst) {
            // 并发持有者要求更“可见”的模式（例如另一路点击需要 255）→ 升级
            HOLD_ALPHA.store(alpha, Ordering::SeqCst);
            set_taskbar_alpha(alpha);
        }
    }
    LOCAL_BRIDGE.with(|depth| depth.set(depth.get() + 1));
    if first {
        // 等任务栏 XAML/UIA 就绪后再返回（调用方紧接着点击/枚举）
        std::thread::sleep(Duration::from_millis(BRIDGE_SETTLE_MS));
    }
    TaskbarHold { active: true }
}

/// 「可点击」提升 guard：只在真正要发指针事件时临时关掉鼠标穿透，
/// drop 时恢复穿透。UIA 枚举（每 10s 一次）不应关穿透，否则底部 48px
/// 会在扫描期间变成点不到的「死区」。
pub struct ClickableGuard {
    restore: bool,
}

impl Drop for ClickableGuard {
    fn drop(&mut self) {
        if !self.restore {
            return;
        }
        for hwnd in live_windows() {
            set_click_through(hwnd, true);
        }
    }
}

/// 在「任务栏可被鼠标命中」窗口期内执行点击；未隐藏时返回空 guard。
pub fn clickable_guard() -> ClickableGuard {
    if !WANT_HIDDEN.load(Ordering::Relaxed) {
        return ClickableGuard { restore: false };
    }
    for hwnd in live_windows() {
        set_click_through(hwnd, false);
    }
    ClickableGuard { restore: true }
}

/// 在“任务栏可见”状态下执行 f（UIA 枚举 / 真实鼠标点击必须在可见状态下进行）。
///
/// 未隐藏 / 降级模式时直接执行，不做任何切换。
/// 桥接期间任务栏保持 alpha=0（肉眼不可见），仅对 UIA 与命中测试可见。
#[allow(dead_code)] // 兼容旧调用点 / 真机冒烟测试使用
pub fn with_taskbar_visible<T>(f: impl FnOnce() -> T) -> T {
    let _hold = begin_hold(bridge_alpha());
    f()
}

/// 桥接版 `Result` 包装：自动上报扫描结果，连续失败后切降级模式。
/// 嵌套调用只执行不重复计数（由最外层负责判定）。
pub fn bridged<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let nested = LOCAL_BRIDGE.with(|depth| depth.get()) > 0;
    let used = needs_bridge() && !nested;
    let ok;
    let result = {
        let _hold = begin_hold(bridge_alpha());
        let out = f();
        ok = out.is_ok();
        out
    };
    if used {
        note_bridge_outcome(ok);
    }
    result
}

/// 连续桥接失败计数：达到阈值后自动降级为「持久 alpha=0」模式
fn note_bridge_outcome(ok: bool) {
    if HIDDEN.load(Ordering::SeqCst) && !PERSISTENT_ALPHA.load(Ordering::SeqCst) {
        let fails = next_fail_count(BRIDGE_FAILS.load(Ordering::SeqCst), ok);
        BRIDGE_FAILS.store(fails, Ordering::SeqCst);
        if !ok && should_fallback(fails) {
            enter_persistent_alpha();
        }
    }
}

/// 纯逻辑：一次扫描结果后的连续失败计数
fn next_fail_count(current: u32, ok: bool) -> u32 {
    if ok {
        0
    } else {
        current.saturating_add(1)
    }
}

/// 纯逻辑：连续失败是否达到降级阈值
fn should_fallback(fails: u32) -> bool {
    fails >= BRIDGE_FAIL_LIMIT
}

/// 纯逻辑：`bridge_ok` = 系统支持且未降级
fn bridge_ok_of(supported: bool, persistent: bool) -> bool {
    supported && !persistent
}

/// 切到「持久 alpha=0」模式：任务栏窗口保持 visible，但完全透明。
/// 用于 SW_HIDE ↔ SW_SHOW 切换下 UIA 拿不到元素的场景（仍可读 UIA / 点击）。
fn enter_persistent_alpha() {
    eprintln!(
        "[easy-dash] 任务栏桥接连续失败 {} 次，降级为持久 alpha=0 模式（顶栏托盘仍可用）",
        BRIDGE_FAIL_LIMIT
    );
    PERSISTENT_ALPHA.store(true, Ordering::SeqCst);
    VISIBLE_MODE.store(false, Ordering::SeqCst);
    BRIDGE_FAILS.store(0, Ordering::SeqCst);
    for hwnd in live_windows() {
        ensure_alpha0(hwnd);
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }
    HIDDEN.store(true, Ordering::SeqCst);
    mark_hidden();
    // 降级模式同样接管工作区（弹窗/托盘仍要在“全屏”桌面下工作）
    enforce_workarea(true);
}

// ==================== 对外状态 ====================

/// 任务栏当前是否不可见（隐身模式 / 隐藏模式 / 降级模式都算）。
///
/// 以实际窗口可见性为准（而非缓存标志），这样跨进程操作被系统拒绝时
/// 顶栏 AppBar 避让与管理面板状态都不会误判。
pub fn is_hidden() -> bool {
    if !WANT_HIDDEN.load(Ordering::Relaxed) {
        return false;
    }
    if PERSISTENT_ALPHA.load(Ordering::Relaxed) || VISIBLE_MODE.load(Ordering::Relaxed) {
        return true;
    }
    windows_all_invisible(&live_windows())
}

/// 桥接是否正在显示任务栏（排障 / 其它模块可用于让路）
#[allow(dead_code)]
pub fn bridge_active() -> bool {
    BRIDGE_DEPTH.load(Ordering::Relaxed) > 0
}

/// 纯函数：当前系统是否支持自动隐藏任务栏（供 state 复用一次窗口枚举结果）
fn supported_flag(wins: &[HWND]) -> bool {
    !disabled_by_env() && !wins.is_empty()
}

/// 供 `get_taskbar_state` 使用的状态：(hidden, bridge_ok, supported)
pub fn state() -> (bool, bool, bool) {
    let wins = live_windows();
    let supported = supported_flag(&wins);
    let persistent = PERSISTENT_ALPHA.load(Ordering::Relaxed);
    let visible_mode = VISIBLE_MODE.load(Ordering::Relaxed);
    // HIDDEN 标志表示「我们成功隐藏过」；桥接/托盘点击保持期间窗口会临时可见，
    // 这里仍按“已隐藏”上报，避免管理面板状态在每次托盘刷新时闪烁。
    let hidden = WANT_HIDDEN.load(Ordering::Relaxed)
        && (persistent
            || visible_mode
            || HIDDEN.load(Ordering::Relaxed)
            || windows_all_invisible(&wins));
    // 隐身模式下窗口始终 visible，UIA/点击不需要桥接切换 → 托盘桥接视为可用
    let bridge_ok = bridge_ok_of(supported, persistent && !visible_mode);
    (hidden, bridge_ok, supported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taskbar_classes_are_recognized() {
        assert!(is_taskbar_class("Shell_TrayWnd"));
        assert!(is_taskbar_class("Shell_SecondaryTrayWnd"));
        assert!(!is_taskbar_class("Progman"));
        assert!(!is_taskbar_class(""));
        assert!(!is_taskbar_class("shell_traywnd"));
    }

    #[test]
    fn success_resets_consecutive_failures() {
        assert_eq!(next_fail_count(0, true), 0);
        assert_eq!(next_fail_count(2, true), 0);
    }

    #[test]
    fn failures_accumulate_saturating() {
        assert_eq!(next_fail_count(0, false), 1);
        assert_eq!(next_fail_count(1, false), 2);
        assert_eq!(next_fail_count(u32::MAX, false), u32::MAX);
    }

    #[test]
    fn fallback_only_at_threshold() {
        assert!(!should_fallback(0));
        assert!(!should_fallback(BRIDGE_FAIL_LIMIT - 1));
        assert!(should_fallback(BRIDGE_FAIL_LIMIT));
        assert!(should_fallback(BRIDGE_FAIL_LIMIT + 10));
    }

    #[test]
    fn bridge_ok_requires_support_and_no_fallback() {
        assert!(bridge_ok_of(true, false));
        assert!(!bridge_ok_of(true, true));
        assert!(!bridge_ok_of(false, false));
        assert!(!bridge_ok_of(false, true));
    }

    #[test]
    fn bridge_needs_hidden_but_not_fallback() {
        // 直接验证判据表达式本身（不触碰真实任务栏状态）
        let needs = |hidden: bool, persistent: bool| hidden && !persistent;
        assert!(needs(true, false));
        assert!(!needs(true, true));
        assert!(!needs(false, false));
    }

    // ---------- 工作区接管（纯逻辑，不触碰真实桌面） ----------

    #[test]
    fn desired_workarea_uses_topbar_height() {
        // 预留顶栏：从顶栏下方开始，底部到屏幕最底（释放任务栏条带）
        let rect = desired_workarea_rect(2560, 1440, 40);
        assert_eq!(
            (rect.left, rect.top, rect.right, rect.bottom),
            (0, 40, 2560, 1440)
        );
        // reserveTopbarSpace=false：从屏幕最顶端开始
        let full = desired_workarea_rect(1920, 1080, 0);
        assert_eq!(
            (full.left, full.top, full.right, full.bottom),
            (0, 0, 1920, 1080)
        );
        // 异常高度夹取：负值 → 0；超过屏高 → 屏高
        assert_eq!(desired_workarea_rect(1920, 1080, -5).top, 0);
        assert_eq!(desired_workarea_rect(1920, 1080, 5000).top, 1080);
        assert_eq!(desired_workarea_rect(1920, 1080, 32).bottom, 1080);
    }

    #[test]
    fn workarea_match_tolerates_one_pixel() {
        let target = RECT {
            left: 0,
            top: 32,
            right: 1920,
            bottom: 1080,
        };
        let near = RECT {
            left: 0,
            top: 33,
            right: 1919,
            bottom: 1080,
        };
        assert!(workarea_matches(&target, &near, WORKAREA_TOL));
        let off = RECT {
            left: 0,
            top: 64,
            right: 1920,
            bottom: 1040,
        };
        assert!(!workarea_matches(&target, &off, WORKAREA_TOL));
    }

    #[test]
    fn marker_workarea_round_trip() {
        let rect = RECT {
            left: 0,
            top: 32,
            right: 2560,
            bottom: 1400,
        };
        let text = format!(
            "easy-dash: windows taskbar is hidden\n{}\n",
            format_workarea_line(&rect)
        );
        assert_eq!(parse_marker_workarea(&text), Some(rect));
        // 旧标记（v0.2.1 只有一行说明）→ None，交给兜底推算
        assert_eq!(
            parse_marker_workarea("easy-dash: windows taskbar is hidden\n"),
            None
        );
        // 损坏 / 非法矩形 → None
        assert_eq!(parse_marker_workarea("workarea=1,2,3\n"), None);
        assert_eq!(parse_marker_workarea("workarea=10,20,10,40\n"), None);
        assert_eq!(parse_marker_workarea(""), None);
    }

    #[test]
    fn marker_taskbar_state_round_trip() {
        assert_eq!(
            parse_marker_taskbar_state("easy-dash: windows taskbar is hidden\ntaskbar_state=0\n"),
            Some(0)
        );
        assert_eq!(
            parse_marker_taskbar_state(&format_taskbar_state_line(
                0x1 | 0x2
            )),
            Some(3)
        );
        assert_eq!(parse_marker_taskbar_state("taskbar_state=oops\n"), None);
        assert_eq!(parse_marker_taskbar_state("workarea=0,0,1,1\n"), None);
        assert_eq!(parse_marker_taskbar_state(""), None);
    }

    /// 真机冒烟（手动执行，会短暂隐藏/恢复桌面任务栏并改动工作区）：
    /// 在**管理员终端**里执行（不要设置 EASY_DASH_NO_ADMIN，否则清单不带提升权限，
    /// 跨进程 ShowWindow 会被 UIPI 拒绝）：
    /// `cargo test --lib -- --ignored smoke_real_taskbar --nocapture`
    ///
    /// 验证：隐藏后 Shell_TrayWnd/SecondaryTrayWnd 全部不可见 → 工作区底部条带被释放
    /// （bottom == 屏高）→ 桥接期间 UIA 可枚举托盘且结束后重新隐藏 → watchdog 不会把
    /// 任务栏弄回可见 → restore 后恢复可见且工作区还原。
    #[test]
    #[ignore = "会短暂隐藏/恢复真实任务栏并改动工作区，手动执行"]
    fn smoke_real_taskbar_hide_restore_and_bridge() {
        /// (visible, alpha, click_through)
        fn snapshot() -> Vec<(bool, Option<u8>, bool)> {
            live_windows()
                .iter()
                .map(|h| {
                    (
                        unsafe { IsWindowVisible(*h) }.as_bool(),
                        current_alpha(*h),
                        exstyle(*h) & transparent_bit() != 0,
                    )
                })
                .collect()
        }

        /// 隐身模式：窗口 visible，但 alpha=0 且鼠标穿透
        fn assert_invisible(rows: &[(bool, Option<u8>, bool)], stage: &str) {
            assert!(
                rows.iter()
                    .all(|(vis, alpha, through)| *vis && *alpha == Some(0) && *through),
                "{stage}: 任务栏应「visible + alpha=0 + 穿透」：{rows:?}"
            );
        }

        let before = live_windows();
        assert!(!before.is_empty(), "找不到 Shell_TrayWnd/SecondaryTrayWnd");
        eprintln!("[smoke] taskbar windows={before:?} state={:?}", snapshot());

        let original_workarea = get_workarea();
        let (screen_w, screen_h) = primary_screen_size();
        eprintln!(
            "[smoke] screen={screen_w}x{screen_h} original_workarea={original_workarea:?}"
        );

        // 测试失败/恐慌时也恢复任务栏
        struct RestoreGuard;
        impl Drop for RestoreGuard {
            fn drop(&mut self) {
                restore_all();
            }
        }
        let _guard = RestoreGuard;

        apply(true).expect("apply(true) 应成功");
        std::thread::sleep(Duration::from_millis(500));
        eprintln!("[smoke] after hide state={:?}", snapshot());
        assert_invisible(&snapshot(), "apply 后");
        assert!(is_hidden(), "is_hidden() 应为 true");

        // 工作区接管：底部任务栏条带应被释放（工作区 bottom == 屏幕高）
        if let Some(wa) = get_workarea() {
            eprintln!("[smoke] workarea after hide={wa:?}");
            assert_eq!(
                wa.bottom, screen_h,
                "隐藏后工作区底部应到屏幕底部（否则最大化窗口底部留条带）"
            );
            assert_eq!(wa.left, 0, "隐藏后工作区左侧应到屏幕左缘");
            assert_eq!(
                wa.top, TOPBAR_HEIGHT.load(Ordering::Relaxed),
                "隐藏后工作区顶部应等于顶栏预留高度"
            );
        }

        // 桥接：临时显示（alpha=0）→ UIA 枚举 → 自动重新隐藏
        let items = with_taskbar_visible(|| crate::winapi::tray::enumerate());
        match &items {
            Ok(list) => eprintln!("[smoke] bridge enumerate ok: {} 个托盘条目", list.len()),
            Err(e) => eprintln!("[smoke] bridge enumerate 失败（可能降级持久模式）：{e}"),
        }
        assert_invisible(&snapshot(), "桥接后");
        if let Some(wa) = get_workarea() {
            assert_eq!(wa.bottom, screen_h, "桥接结束后工作区底部仍应是屏幕底部");
        }
        eprintln!("[smoke] bridge 后 state={:?}", state());

        // watchdog 不应把任务栏弄回可见，也不应反复改写工作区
        watchdog();
        std::thread::sleep(Duration::from_millis(300));
        assert_invisible(&snapshot(), "watchdog 后");
        if let Some(wa) = get_workarea() {
            assert_eq!(wa.bottom, screen_h, "watchdog 后工作区底部仍应是屏幕底部");
        }

        restore_all();
        std::thread::sleep(Duration::from_millis(500));
        let restored = snapshot();
        eprintln!("[smoke] after restore state={restored:?} state={:?}", state());
        assert!(
            restored
                .iter()
                .all(|(vis, alpha, through)| *vis && *alpha != Some(0) && !*through),
            "恢复后任务栏应可见、不透明、无穿透：{restored:?}"
        );
        assert!(!is_hidden(), "is_hidden() 应为 false");

        // 工作区还原：应与接管前一致（容差 1px）
        if let (Some(original), Some(wa)) = (original_workarea, get_workarea()) {
            eprintln!("[smoke] workarea after restore={wa:?} (original={original:?})");
            assert!(
                workarea_matches(&original, &wa, WORKAREA_TOL),
                "恢复后工作区应还原为接管前：got {wa:?}, want {original:?}"
            );
        }
    }
}

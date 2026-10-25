//! Win11 XAML 任务栏托盘镜像：UIA 枚举 + 真实鼠标点击 + 系统通知中心。
//!
//! Win11（含 26H2）的任务栏是 XAML/Compositor 架构：Shell_TrayWnd 子树里
//! 已经没有 ToolbarWindow32，经典的 TB_GETBUTTON / TRAYDATA 方案完全失效，
//! 只能通过 UI Automation 读取托盘按钮：
//! - 应用托盘图标：AutomationId == "NotifyItemIcon"，Name == 提示文本
//!   （实测部分应用的工具提示为空串），其子元素里有一个 ClassName == "Image"
//!   的图标元素（实际像素坐标另存 icon_rect，供截图使用）；
//! - 「显示隐藏的图标」：AutomationId == "SystemTrayIcon" 且
//!   ClassName == "SystemTray.NormalButton"；注意输入法指示器「任务栏输入指示」
//!   共用同一组属性，只能靠"托盘区最左/在所有图标左侧"区分；
//! - 系统时钟（通知中心入口）：ClassName == "SystemTray.OmniButton"。
//!
//! 这些 UIA 按钮虽然对外暴露了 InvokePattern（XAML Button 的默认实现），但
//! 调用它并不会展开托盘浮出菜单，因此点击必须发送真实鼠标事件：
//! GetCursorPos 保存 → SetCursorPos → mouse_event → 复原光标。
//!
//! 线程要求：UIA 是跨进程调用，在没有消息泵的线程上可能长时间阻塞，
//! 调用方必须保证本模块不在主线程/热路径上同步调用（见 tray_mirror.rs）。
#![allow(non_upper_case_globals)]
#![allow(dead_code)]

use std::cell::Cell;
use std::sync::Mutex;
use std::time::Duration;

use windows::core::{BSTR, IUnknown};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCondition, IUIAutomationElement,
    IUIAutomationElementArray, TreeScope_Children, TreeScope_Descendants,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    mouse_event, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetCursorPos, GetWindowRect, SetCursorPos,
};

use crate::types::TrayGeom;

/// RPC_E_CHANGED_MODE：线程已用另一种套间模型初始化 COM，可继续使用
const RPC_E_CHANGED_MODE: u32 = 0x8001_0106;

thread_local! {
    /// 本线程是否已（由本模块）确认 COM 可用；避免重复 CoInitializeEx 破坏
    /// 其它模块（icons/worker）在同一线程上的初始化计数
    static COM_READY: Cell<bool> = const { Cell::new(false) };
}

/// 确保当前线程 COM 可用（MTA 优先）。返回 false 表示该线程根本无法用 COM。
///
/// 工作线程请自己先调 CoInitializeEx(MTA)；这里的 thread_local 标记只看
/// 本模块自己有没有初始化过，重复初始化由 COM 计数兜底（S_FALSE 无害）。
fn ensure_com() -> bool {
    COM_READY.with(|ready| {
        if ready.get() {
            return true;
        }
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // S_OK/S_FALSE = 成功；RPC_E_CHANGED_MODE = 线程已是 STA（如 Tauri 主线程），
        // COM 本身可用，UIA 客户端对象照样能创建
        let ok = hr.is_ok() || (hr.0 as u32) == RPC_E_CHANGED_MODE;
        if ok {
            ready.set(true);
        }
        ok
    })
}

/// 创建 IUIAutomation 实例（调用线程必须已初始化 COM）
fn create_automation() -> Result<IUIAutomation, String> {
    if !ensure_com() {
        return Err("当前线程 COM 初始化失败，无法使用 UI Automation".into());
    }
    unsafe {
        CoCreateInstance(
            &CUIAutomation,
            None::<&IUnknown>,
            CLSCTX_INPROC_SERVER,
        )
        .map_err(|e| format!("创建 CUIAutomation 失败: {e}"))
    }
}

/// 任务栏窗口（Shell_TrayWnd）。任务栏被系统隐藏时返回 None。
fn taskbar_hwnd() -> Option<HWND> {
    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let found = &mut *(lparam.0 as *mut Option<HWND>);
        if found.is_none() && super::common::class_name(hwnd) == "Shell_TrayWnd" {
            *found = Some(hwnd);
        }
        // 继续枚举：即使找到也让它走完，避免提前终止影响系统缓存
        true.into()
    }

    let mut found: Option<HWND> = None;
    let ctx = &mut found as *mut Option<HWND>;
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(ctx as isize));
    }
    found
}

/// 任务栏所在显示器的矩形（屏幕物理像素）
fn monitor_rect(hwnd: HWND) -> RECT {
    unsafe {
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(hmon, &mut mi).as_bool() {
            mi.rcMonitor
        } else {
            RECT::default()
        }
    }
}

fn rect_tuple(r: RECT) -> (i32, i32, i32, i32) {
    (r.left, r.top, r.right - r.left, r.bottom - r.top)
}

/// rect 是否落在 outer 内（允许 tol 像素误差）；outer 非法时返回 false
fn rect_inside(rect: RECT, outer: RECT, tol: i32) -> bool {
    if outer.right - outer.left <= 0 || outer.bottom - outer.top <= 0 {
        return false;
    }
    rect.left >= outer.left - tol
        && rect.top >= outer.top - tol
        && rect.right <= outer.right + tol
        && rect.bottom <= outer.bottom + tol
}

/// 兜底过滤（拿不到任务栏窗口矩形时）：矩形中心必须落在屏幕下 1/4 且水平在屏内。
/// 注意：这只在 GetWindowRect 失败时使用，正常情况用任务栏窗口矩形做包含判断，
/// 因此任务栏停靠在上/左/右边缘时也不会误杀。
fn near_bottom_band(rect: RECT, screen: RECT) -> bool {
    if screen.bottom - screen.top <= 0 {
        return true; // 屏幕信息也拿不到时不做二次过滤
    }
    let cy = (rect.top + rect.bottom) / 2;
    cy > screen.top + (screen.bottom - screen.top) * 3 / 4
        && rect.left >= screen.left
        && rect.right <= screen.right
}

/// 一次任务栏扫描的结果（复用 UIA 实例与条件对象，避免每个元素都跨进程新建）
struct TrayScan {
    elements: IUIAutomationElementArray,
    /// True 条件：UIA 的 FindAll/FindFirst 必须带条件对象，这里表示"全部匹配"
    cond: IUIAutomationCondition,
    /// 任务栏窗口自身矩形（屏幕物理像素）；GetWindowRect 失败时为零矩形
    taskbar: RECT,
    /// 任务栏所在显示器矩形
    screen: RECT,
}

fn scan_taskbar() -> Result<TrayScan, String> {
    let automation = create_automation()?;
    let hwnd = taskbar_hwnd().ok_or_else(|| "未找到任务栏窗口 Shell_TrayWnd".to_string())?;
    let root = unsafe { automation.ElementFromHandle(hwnd) }
        .map_err(|e| format!("UIA ElementFromHandle 失败: {e}"))?;
    let cond = unsafe { automation.CreateTrueCondition() }
        .map_err(|e| format!("UIA CreateTrueCondition 失败: {e}"))?;
    let elements = unsafe { root.FindAll(TreeScope_Descendants, &cond) }
        .map_err(|e| format!("UIA FindAll 失败: {e}"))?;

    let mut taskbar = RECT::default();
    let taskbar_ok = unsafe { GetWindowRect(hwnd, &mut taskbar) }.is_ok();
    Ok(TrayScan {
        elements,
        cond,
        taskbar: if taskbar_ok { taskbar } else { RECT::default() },
        screen: monitor_rect(hwnd),
    })
}

fn bstr_value(r: windows::core::Result<BSTR>) -> String {
    r.map(|b| b.to_string()).unwrap_or_default()
}

/// 应用图标按钮的子元素里找 ClassName == "Image" 的图标区域
fn icon_rect_of(
    el: &IUIAutomationElement,
    cond: &IUIAutomationCondition,
) -> Option<(i32, i32, i32, i32)> {
    let kids = unsafe { el.FindAll(TreeScope_Children, cond) }.ok()?;
    let n = unsafe { kids.Length() }.ok()?.max(0);
    for i in 0..n {
        let Ok(kid) = (unsafe { kids.GetElement(i) }) else {
            continue;
        };
        if bstr_value(unsafe { kid.CurrentClassName() }) != "Image" {
            continue;
        }
        if unsafe { kid.CurrentIsOffscreen() }
            .map(|b| b.as_bool())
            .unwrap_or(true)
        {
            continue;
        }
        let Ok(rect) = (unsafe { kid.CurrentBoundingRectangle() }) else {
            continue;
        };
        let (x, y, w, h) = rect_tuple(rect);
        // 高 DPI 下 16 逻辑像素会变成 20~32 物理像素，过滤明显异常尺寸即可
        if w <= 0 || h <= 0 || w > 64 || h > 64 {
            continue;
        }
        return Some((x, y, w, h));
    }
    None
}

/// 单个 UIA 元素 → TrayGeom（只认应用图标与「显示隐藏的图标」）
fn geom_of(el: &IUIAutomationElement, scan: &TrayScan) -> Option<TrayGeom> {
    if unsafe { el.CurrentIsOffscreen() }
        .map(|b| b.as_bool())
        .unwrap_or(true)
    {
        return None;
    }
    let rect = unsafe { el.CurrentBoundingRectangle() }.ok()?;
    let (x, y, w, h) = rect_tuple(rect);
    if w <= 0 || h <= 0 {
        return None;
    }
    // 必须落在任务栏窗口内（含 4px 误差）；任务栏窗口矩形拿不到时退化为
    // "屏幕底部 1/4 区域"判断
    if !rect_inside(rect, scan.taskbar, 4) && !near_bottom_band(rect, scan.screen) {
        return None;
    }

    let automation_id = bstr_value(unsafe { el.CurrentAutomationId() });
    if automation_id == "NotifyItemIcon" {
        let name = bstr_value(unsafe { el.CurrentName() });
        Some(TrayGeom {
            name: if name.trim().is_empty() {
                "托盘图标".to_string()
            } else {
                name
            },
            kind: "icon".to_string(),
            x,
            y,
            w,
            h,
            icon_rect: icon_rect_of(el, &scan.cond),
        })
    } else if automation_id == "SystemTrayIcon"
        && bstr_value(unsafe { el.CurrentClassName() }) == "SystemTray.NormalButton"
    {
        // 「显示隐藏的图标」按钮候选：本身不是应用图标，不需要截图。
        // 这里先全部收下，真正的按钮由 pick_overflow 按"最左"规则挑选
        // （输入法指示器属性完全相同，只有位置能区分）
        Some(TrayGeom {
            name: "显示隐藏的图标".to_string(),
            kind: "overflow".to_string(),
            x,
            y,
            w,
            h,
            icon_rect: None,
        })
    } else {
        None
    }
}

/// 从候选里挑出真正的「显示隐藏的图标」按钮。
///
/// 实测（Win11 26H2）：输入法指示器「任务栏输入指示」与它共用
/// AutomationId == "SystemTrayIcon" + ClassName == "SystemTray.NormalButton"，
/// 属性上无法区分，只能靠位置：真正的按钮位于托盘区最左、且在所有应用图标左侧。
/// 极端情况（托盘里一个应用图标都没有）：无法区分两者，退化为取最左候选，
/// 此时若系统恰好没有隐藏图标，会多出一个点不动的 overflow 按钮，无害。
fn pick_overflow(candidates: &[TrayGeom], icons: &[TrayGeom]) -> Option<TrayGeom> {
    let min_icon_x = icons.iter().map(|g| g.x).min();
    let mut best: Option<&TrayGeom> = None;
    for c in candidates {
        // 有应用图标时：只有位于全部图标左侧的候选才可能是「显示隐藏的图标」
        if let Some(ix) = min_icon_x {
            if c.x >= ix {
                continue;
            }
        }
        if best.map(|b| c.x < b.x).unwrap_or(true) {
            best = Some(c);
        }
    }
    best.cloned()
}

/// 枚举任务栏托盘中的图标（按屏幕 x 升序，overflow 最左）。
///
/// 必须在已初始化 COM 的线程上调用；建议放在专用工作线程（tray_mirror）。
pub fn enumerate() -> Result<Vec<TrayGeom>, String> {
    let scan = scan_taskbar()?;
    let n = unsafe { scan.elements.Length() }
        .map_err(|e| format!("UIA 读取元素数量失败: {e}"))?
        .max(0);
    let mut icons: Vec<TrayGeom> = Vec::new();
    let mut overflow_candidates: Vec<TrayGeom> = Vec::new();
    for i in 0..n {
        let Ok(el) = (unsafe { scan.elements.GetElement(i) }) else {
            continue;
        };
        let Some(g) = geom_of(&el, &scan) else {
            continue;
        };
        if g.kind == "icon" {
            icons.push(g);
        } else {
            overflow_candidates.push(g);
        }
    }
    let mut result = icons;
    if let Some(overflow) = pick_overflow(&overflow_candidates, &result) {
        result.push(overflow);
    }
    result.sort_by_key(|g| g.x);
    Ok(result)
}

/// 在屏幕物理坐标处发一次真实鼠标点击（"left" | "right"）。
///
/// 这些托盘按钮虽然对外暴露 InvokePattern，但 UIA 调用不会展开托盘浮出菜单/
/// 通知中心，必须发真实指针事件；光标是全局共享资源，因此先保存当前位置，
/// 点完立即复原，并用互斥量串行化并发点击。
pub fn click_at(x: i32, y: i32, button: &str) -> Result<(), String> {
    static CLICK_LOCK: Mutex<()> = Mutex::new(());
    let _guard = CLICK_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let (down, up) = if button == "right" {
        (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP)
    } else {
        (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP)
    };

    unsafe {
        let mut saved = POINT::default();
        let saved_ok = GetCursorPos(&mut saved).is_ok();
        SetCursorPos(x, y).map_err(|e| format!("移动光标到 ({x},{y}) 失败: {e}"))?;
        // 等系统处理 WM_MOUSEMOVE / 任务栏悬停状态，避免点击落在旧位置
        std::thread::sleep(Duration::from_millis(30));
        mouse_event(down, 0, 0, 0, 0);
        std::thread::sleep(Duration::from_millis(20));
        mouse_event(up, 0, 0, 0, 0);
        // 给弹出菜单/浮出窗口留出响应时间，再把光标还回去
        std::thread::sleep(Duration::from_millis(50));
        if saved_ok {
            let _ = SetCursorPos(saved.x, saved.y);
        }
    }
    Ok(())
}

/// 打开 Windows 原生通知中心（左键点任务栏系统时钟按钮 SystemTray.OmniButton）
pub fn open_system_notification_center() -> Result<(), String> {
    let scan = scan_taskbar()?;
    let n = unsafe { scan.elements.Length() }
        .map_err(|e| format!("UIA 读取元素数量失败: {e}"))?
        .max(0);
    for i in 0..n {
        let Ok(el) = (unsafe { scan.elements.GetElement(i) }) else {
            continue;
        };
        if bstr_value(unsafe { el.CurrentClassName() }) != "SystemTray.OmniButton" {
            continue;
        }
        if unsafe { el.CurrentIsOffscreen() }
            .map(|b| b.as_bool())
            .unwrap_or(true)
        {
            continue;
        }
        let Ok(rect) = (unsafe { el.CurrentBoundingRectangle() }) else {
            continue;
        };
        let (x, y, w, h) = rect_tuple(rect);
        if w <= 0 || h <= 0 {
            continue;
        }
        return click_at(x + w / 2, y + h / 2, "left");
    }
    Err("未找到任务栏系统时钟按钮（SystemTray.OmniButton）".into())
}

/// 当前系统是否支持托盘镜像（UIA 能拿到任务栏可访问性树，且任务栏当前可见）。
/// 任务栏被自动隐藏（窗口整体移出屏幕）/ 非 Win11 XAML 任务栏 / UIA 被策略禁用
/// 时返回 false。
pub fn is_available() -> bool {
    let scan = match scan_taskbar() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[tray] UI Automation 不可用：{e}");
            return false;
        }
    };
    // 屏幕信息拿不到时不做可见性判断，避免误报不可用
    if scan.screen.bottom - scan.screen.top <= 0 || scan.screen.right - scan.screen.left <= 0 {
        return true;
    }
    // GetWindowRect 失败（零矩形）时同样不因此判定不可用
    if scan.taskbar.right - scan.taskbar.left <= 0 || scan.taskbar.bottom - scan.taskbar.top <= 0 {
        return true;
    }
    // 自动隐藏的任务栏会被移到屏幕外：此时 UIA 里没有可见托盘图标
    scan.taskbar.top < scan.screen.bottom && scan.taskbar.bottom > scan.screen.top
}

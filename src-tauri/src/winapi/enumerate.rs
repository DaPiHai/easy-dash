//! 顶层可见窗口枚举（GNOME 概览的窗口来源）
#![allow(non_upper_case_globals)]

use serde::Serialize;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, IsIconic, IsWindow, IsWindowVisible,
};

use super::common;
use crate::app_state::AppState;

/// 系统窗口类黑名单
const EXCLUDED_CLASSES: &[&str] = &[
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Progman",
    "WorkerW",
    "NotifyIconOverflowWindow",
    "TopLevelWindowForOverflowXamlIsland",
    "XamlExplorerHostIslandWindow",
    "Windows.UI.Core.CoreWindow",
    "Windows.UI.Composition.DesktopWindowContentBridge",
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WinEntry {
    pub id: i64,
    pub title: String,
    pub exe: String,
    pub path: String,
    pub icon: Option<String>,
    pub minimized: bool,
    /// AppUserModelID（UWP/打包应用），作为独立分组与固定/启动标识
    pub aumid: Option<String>,
    /// 应用显示名（"设置"/"计算器"；桌面应用为 exe 基名）
    pub app_name: String,
    /// 窗口首次出现时间（毫秒），用于稳定排序（打开顺序而非聚焦顺序）
    pub order: u64,
    /// 窗口尺寸（物理像素）
    pub width: i32,
    pub height: i32,
}

struct CollectCtx<'a> {
    own_pid: u32,
    hwnds: &'a mut Vec<HWND>,
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut CollectCtx);
    ctx.hwnds.push(hwnd);
    true.into()
}

fn is_cloaked(hwnd: HWND) -> bool {
    cloaked_value(hwnd) != 0
}

/// cloaked 值：0=正常，1=应用主动隐藏，2=Shell 挂起（UWP 失焦挂起/其他虚拟桌面），4=继承
fn cloaked_value(hwnd: HWND) -> u32 {
    let mut cloaked: u32 = 0;
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        )
    };
    if hr.is_ok() {
        cloaked
    } else {
        0
    }
}

/// 窗口物理尺寸（优先 DWM 扩展边框，排除不可见阴影边框）
fn window_size(hwnd: HWND) -> (i32, i32) {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    let mut rect = RECT::default();
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut _ as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
    };
    if hr.is_err() {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect);
        }
    }
    (rect.right - rect.left, rect.bottom - rect.top)
}

fn accept(hwnd: HWND, own_pid: u32) -> bool {
    if unsafe { !IsWindowVisible(hwnd).as_bool() } {
        return false;
    }
    if common::window_pid(hwnd) == own_pid {
        return false;
    }
    if common::is_tool_window(hwnd) {
        return false;
    }
    // cloaked=2（Shell 挂起）要保留：UWP 应用失焦挂起后窗口仍应留在 Dock 里，
    // 否则会"消失-重现"导致顺序跳动；任务栏同样保留这些窗口
    let cv = cloaked_value(hwnd);
    if cv != 0 && cv != 2 {
        return false;
    }
    let class = common::class_name(hwnd);
    !EXCLUDED_CLASSES.contains(&class.as_str())
}

/// 枚举当前可切换的顶层窗口列表（按 Z 序）。
/// 只读缓存，不做任何 Shell COM 调用（那些可能永久阻塞，放到图标工作线程）。
pub fn list_windows(own_pid: u32, st: &AppState) -> Vec<WinEntry> {
    let mut hwnds: Vec<HWND> = Vec::new();
    let mut ctx = CollectCtx {
        own_pid,
        hwnds: &mut hwnds,
    };
    unsafe { let _ = EnumWindows(Some(enum_cb), LPARAM(&mut ctx as *mut _ as isize)); }

    let mut out = Vec::new();
    for hwnd in hwnds {
        if !accept(hwnd, own_pid) {
            continue;
        }
        let id = hwnd.0 as isize;
        let minimized = unsafe { IsIconic(hwnd).as_bool() };
        // 最小化窗口的窗口矩形是 (-32000,...) 无效值，用最后一次正常尺寸；
        // 尺寸过滤仅对非最小化窗口生效（Electron 同标题悬浮小窗，
        // 如 ZCode 的 250x64 标题条，会干扰点击目标选择）
        let (width, height) = {
            let mut sizes = st.win_size.lock().unwrap();
            if minimized {
                sizes.get(&id).copied().unwrap_or((400, 300))
            } else {
                let s = window_size(hwnd);
                if s.0 > 1 && s.1 > 1 {
                    sizes.insert(id, s);
                }
                s
            }
        };
        if width < 100 || height < 80 {
            continue;
        }
        let title = common::window_title(hwnd);
        if title.trim().is_empty() {
            continue;
        }
        let full_path = common::process_image_path(hwnd);
        let exe = full_path
            .as_deref()
            .map(|p| {
                p.rsplit(['\\', '/'])
                    .next()
                    .unwrap_or("")
                    .to_lowercase()
            })
            .unwrap_or_default();
        let class = common::class_name(hwnd);
        let (aumid, icon, app_name) =
            icon_meta(&class, &exe, full_path.as_deref().unwrap_or(""), &title, id, st);
        let order = *st.win_order.lock().unwrap().entry(id).or_insert_with(|| {
            st.order_seq
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        });
        out.push(WinEntry {
            id: id as i64,
            title,
            exe,
            path: full_path.unwrap_or_default(),
            icon,
            minimized,
            aumid,
            app_name,
            order,
            width,
            height,
        });
    }
    // 只清理真正销毁的窗口：挂起（cloaked）/ 暂时隐藏的窗口必须保留
    // 打开顺序、AUMID 记忆与尺寸缓存
    st.hwnd_aumid
        .lock()
        .unwrap()
        .retain(|k, _| unsafe { IsWindow(HWND(*k as usize as *mut _)).as_bool() });
    st.win_order
        .lock()
        .unwrap()
        .retain(|k, _| unsafe { IsWindow(HWND(*k as usize as *mut _)).as_bool() });
    st.win_size
        .lock()
        .unwrap()
        .retain(|k, _| unsafe { IsWindow(HWND(*k as usize as *mut _)).as_bool() });
    out
}

/// 窗口图标 + 分组 AUMID + 应用名（纯缓存读取，缺失时把窗口交给图标工作线程）。
/// 分组 AUMID 只对 UWP 宿主窗口暴露（ApplicationFrameWindow /
/// ApplicationFrameHost）：多个 UWP 应用共用宿主进程，必须按 AUMID 拆分；
/// 桌面应用（浏览器每个窗口/配置文件都可能带 AUMID）必须按 exe 分组。
fn icon_meta(
    class: &str,
    exe: &str,
    full_path: &str,
    title: &str,
    id: isize,
    st: &AppState,
) -> (Option<String>, Option<String>, String) {
    // 打包应用（WindowsApps 目录，如计算器/商店）与其 ApplicationFrameHost
    // 宿主是同一个应用：必须都按 AUMID 分组才能合并成一个 Dock 条目
    let is_uwp_host = class.eq_ignore_ascii_case("ApplicationFrameWindow")
        || exe == "applicationframehost.exe"
        || full_path.to_lowercase().contains("\\windowsapps\\");
    let aumid = if is_uwp_host {
        st.hwnd_aumid.lock().unwrap().get(&id).cloned()
    } else {
        None
    };
    let exe_key = if exe.is_empty() {
        format!("hwnd:{id}")
    } else {
        exe.to_string()
    };
    let cache_key = match &aumid {
        Some(a) => format!("aumid:{a}"),
        None => exe_key.clone(),
    };

    let icon = st.icon_cache.lock().unwrap().get(&cache_key).cloned().flatten();
    let app_name = match &aumid {
        Some(a) => st
            .aumid_names
            .lock()
            .unwrap()
            .get(&format!("aumid:{a}"))
            .cloned()
            .filter(|s| !s.trim().is_empty())
            // UWP 窗口标题通常就是应用名（"设置"/"计算器"）
            .or_else(|| (!title.trim().is_empty()).then(|| title.to_string()))
            .unwrap_or_else(|| exe.trim_end_matches(".exe").to_string()),
        None => exe.trim_end_matches(".exe").to_string(),
    };

    // 缓存缺失 → 交给图标工作线程（去重），本轮先显示占位符
    if icon.is_none() {
        let mut pending = st.icon_pending.lock().unwrap();
        if pending.insert(id) {
            let _ = st.icon_tx.send(id);
        }
    }

    let group_aumid = if is_uwp_host { aumid } else { None };
    (group_aumid, icon, app_name)
}

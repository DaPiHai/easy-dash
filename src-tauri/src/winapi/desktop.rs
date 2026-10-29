//! 桌面图标避让：把 Explorer 桌面图标视图整体下移顶栏高度。
//!
//! ## 问题
//!
//! Windows 的桌面图标视图（`Progman`/`WorkerW` → `SHELLDLL_DefView` → `SysListView32`）
//! 始终铺满整个（虚拟）屏幕，图标从客户区左上角开始排；`SPI_SETWORKAREA` 只影响
//! 最大化窗口与任务栏，**不会**把桌面图标推下去。真机实测：工作区已是
//! `(0,32)-(1920,1080)`，回收站的图标矩形仍是 `(0,5)-(76,77)`（客户区坐标 = 屏幕坐标），
//! 于是置顶的 32px 顶栏把第一行图标的上半部分盖住（截图里的「回收站」缺了盖子）。
//!
//! ## 做法
//!
//! 把 `SysListView32` 的窗口矩形整体下移 `inset`（底边不动、高度同步收缩），图标在
//! 客户区里的坐标不变 → 屏幕上整体下移；退出/关闭顶栏时移回。目标矩形由**虚拟屏
//! 矩形**推导而不是当前矩形，所以：
//! - 重复调用幂等（不会叠加偏移）；
//! - 强杀残留、改分辨率后重新进入都能自愈。
//!
//! 多屏时桌面视图是一整块虚拟屏，整体下移对每块屏都成立：每块屏的顶栏都贴在该屏
//! 顶部，图标下移 `inset` 后正好落在顶栏底边下方（各屏 DPI 不同时按主屏高度近似）。

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, FindWindowW, GetClassNameW, GetSystemMetrics, GetWindowRect,
    SetWindowPos, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER,
};

/// 整个虚拟屏幕的物理矩形（多屏时是所有显示器的并集，原点可能为负）
fn virtual_rect() -> RECT {
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        RECT {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        }
    }
}

fn window_rect(hwnd: HWND) -> Option<RECT> {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    (rect.right > rect.left && rect.bottom > rect.top).then_some(rect)
}

/// 枚举 WorkerW 找 `SHELLDLL_DefView`（Win10/11 多桌面、壁纸轮播时它可能挂在 WorkerW 下）
unsafe extern "system" fn find_defview_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let target = &mut *(lparam.0 as *mut HWND);
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut buf);
    if n <= 0 {
        return true.into();
    }
    if String::from_utf16_lossy(&buf[..n as usize]) != "WorkerW" {
        return true.into();
    }
    if let Ok(defview) = FindWindowExW(
        hwnd,
        HWND::default(),
        w!("SHELLDLL_DefView"),
        PCWSTR::null(),
    ) {
        *target = defview;
        return false.into();
    }
    true.into()
}

fn find_defview() -> Option<HWND> {
    unsafe {
        if let Ok(progman) = FindWindowW(w!("Progman"), PCWSTR::null()) {
            if let Ok(defview) = FindWindowExW(
                progman,
                HWND::default(),
                w!("SHELLDLL_DefView"),
                PCWSTR::null(),
            ) {
                return Some(defview);
            }
        }
        let mut found = HWND::default();
        let _ = EnumWindows(Some(find_defview_cb), LPARAM(&mut found as *mut _ as isize));
        (!found.0.is_null()).then_some(found)
    }
}

/// 桌面图标列表视图（找不到时返回 None：Explorer 未就绪 / 重启中）
fn find_listview() -> Option<HWND> {
    let defview = find_defview()?;
    unsafe {
        FindWindowExW(
            defview,
            HWND::default(),
            w!("SysListView32"),
            PCWSTR::null(),
        )
        .ok()
    }
}

/// Explorer 桌面视图的「自然」矩形（未下移时的整屏矩形）。
///
/// 只有当当前矩形看起来没被下移过（顶边贴虚拟屏顶边、高度接近整屏）才采用它，
/// 否则用虚拟屏矩形：这样强杀残留 / 改分辨率后不会把已经下移的矩形当成基准再叠一次。
fn base_rect_for(cur: RECT, virt: RECT) -> RECT {
    let full_height = virt.bottom - virt.top;
    let looks_natural =
        cur.top == virt.top && (cur.bottom - cur.top) >= full_height - 4 && cur.left == virt.left;
    if looks_natural {
        cur
    } else {
        virt
    }
}

/// 按 inset 下移后的目标矩形（底边不动；inset 超屏时按高度-1 收敛）
fn target_rect(base: RECT, inset: i32) -> RECT {
    let height = base.bottom - base.top;
    if inset <= 0 || height <= 0 {
        return base;
    }
    let inset = inset.min(height - 1);
    RECT {
        left: base.left,
        top: base.top + inset,
        right: base.right,
        bottom: base.bottom,
    }
}

fn apply_now(inset: i32) {
    let Some(listview) = find_listview() else {
        return;
    };
    let Some(cur) = window_rect(listview) else {
        return;
    };
    let base = base_rect_for(cur, virtual_rect());
    let target = target_rect(base, inset);
    if cur == target {
        return;
    }
    unsafe {
        let _ = SetWindowPos(
            listview,
            None,
            target.left,
            target.top,
            target.right - target.left,
            target.bottom - target.top,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        );
    }
}

/// 设置桌面图标下移量（物理像素；0 = 恢复整屏）。
///
/// 由 `surfaces::sync_desktop_icon_inset()` 在顶栏创建 / 配置热更新 / 显示器重排后
/// 调用，monitor 线程每 2s 也会再调一次，补回被 Explorer 重置的矩形
/// （F5 刷新桌面、Explorer 重启、改分辨率）。幂等：已经是目标矩形时只做一次
/// `FindWindow` + `GetWindowRect`，开销可忽略。
pub fn apply(inset_px: i32) {
    apply_now(inset_px.max(0));
}

/// 退出时把桌面图标视图移回整屏（正常退出 / 关闭顶栏）。
pub fn restore() {
    apply_now(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
        RECT {
            left: l,
            top: t,
            right: r,
            bottom: b,
        }
    }

    #[test]
    fn base_rect_keeps_natural_fullscreen_rect() {
        let virt = rect(0, 0, 1920, 1080);
        assert_eq!(base_rect_for(rect(0, 0, 1920, 1080), virt), virt);
    }

    #[test]
    fn base_rect_falls_back_to_virtual_when_already_inset() {
        // 强杀残留：视图已下移 32px，不能再拿它当基准
        let virt = rect(0, 0, 1920, 1080);
        assert_eq!(base_rect_for(rect(0, 32, 1920, 1080), virt), virt);
        // 分辨率变小后残留的旧矩形
        assert_eq!(base_rect_for(rect(0, 0, 1280, 720), virt), virt);
        // 多屏负坐标虚拟屏
        let virt2 = rect(-1920, -200, 1920, 880);
        assert_eq!(base_rect_for(rect(0, 0, 1920, 1080), virt2), virt2);
    }

    #[test]
    fn target_rect_moves_top_and_keeps_bottom() {
        let base = rect(0, 0, 1920, 1080);
        let t = target_rect(base, 32);
        assert_eq!(t, rect(0, 32, 1920, 1080));
        assert_eq!(target_rect(base, 48), rect(0, 48, 1920, 1080));
    }

    #[test]
    fn target_rect_is_identity_without_inset() {
        let base = rect(0, 0, 1920, 1080);
        assert_eq!(target_rect(base, 0), base);
        assert_eq!(target_rect(base, -10), base);
    }

    #[test]
    fn target_rect_clamps_to_height() {
        let base = rect(0, 0, 1920, 32);
        // 不会把视图压成 0 高度
        assert_eq!(target_rect(base, 999), rect(0, 31, 1920, 32));
    }

    #[test]
    fn target_rect_idempotent_when_reapplied_to_result() {
        let base = rect(0, 0, 1920, 1080);
        let once = target_rect(base, 32);
        // 以虚拟屏矩形为基准重复应用，结果不变（watchdog 不会叠加偏移）
        assert_eq!(target_rect(base_rect_for(once, base), 32), once);
    }
}

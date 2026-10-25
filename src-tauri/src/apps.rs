//! 应用中心数据：后台枚举 + 合并去重 + 本地化排序 + 缓存。
//!
//! 枚举与图标提取（Start Menu / WinRT 打包应用 / Shell 图标）都很慢，
//! 全部放在专用工作线程里串行执行，主线程只读缓存。
use std::cmp::Ordering as CmpOrdering;
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use tauri::{AppHandle, Emitter, Manager};
use windows::core::PCWSTR;
use windows::Win32::Foundation::LPARAM;
use windows::Win32::Globalization::{
    CompareStringEx, COMPARE_STRING_FLAGS, CSTR_EQUAL, CSTR_GREATER_THAN, CSTR_LESS_THAN,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

use crate::app_state::AppState;
use crate::types::AppEntry;
use crate::winapi;

/// 启动应用中心数据线程（整个进程只应调用一次；Receiver 已被 take 走时直接退出）。
pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        // WinRT（PackageManager）与 Shell 图标都要求先初始化 COM/WinRT；失败也继续
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }

        let state = app.state::<AppState>();
        let rx: Receiver<()> = match state.apps_rx.lock().map(|mut g| g.take()) {
            Ok(Some(rx)) => rx,
            _ => return,
        };

        while rx.recv().is_ok() {
            refresh_once(&app);
        }
    });
}

/// 请求一次后台刷新（非阻塞；完成后发 apps-changed）。
/// 正在枚举时直接忽略——启动阶段 UI 的 get_apps 与 setup 的首次请求会同时到达，
/// 不忽略会重复跑两次全量枚举（每次约 1.5–2.5s）。
pub fn request_refresh(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.apps_refreshing.load(Ordering::Relaxed) {
        return;
    }
    let _ = state.apps_tx.send(());
}

fn refresh_once(app: &AppHandle) {
    let started = Instant::now();
    app.state::<AppState>()
        .apps_refreshing
        .store(true, Ordering::Relaxed);

    let cfg = app
        .state::<AppState>()
        .config
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();

    let classic = winapi::start_apps::enumerate();
    let packaged = winapi::packaged_apps::enumerate();
    // 应用中心手动导入的应用：与 Start Menu / 打包应用一起参与去重与排序
    let imported: Vec<AppEntry> = cfg
        .imported_apps
        .iter()
        .filter_map(|item| match winapi::imports::make_entry(&item.path, &item.name, &item.args) {
            Ok(entry) => Some(entry),
            Err(e) => {
                eprintln!("[apps] 跳过无效导入应用（{}）：{e}", item.path);
                None
            }
        })
        .collect();
    let classic: Vec<AppEntry> = classic.into_iter().chain(imported).collect();

    let mut apps = merge_apps(classic, packaged);
    normalize_uwp_icons(&mut apps);
    apply_app_order(&mut apps, &cfg.app_order);

    let count = apps.len();
    {
        let st = app.state::<AppState>();
        *st.apps.lock().unwrap_or_else(|e| e.into_inner()) = apps;
        st.apps_refreshed_ms
            .store(crate::app_state::now_ms(), Ordering::Relaxed);
        st.apps_refreshing.store(false, Ordering::Relaxed);
    }
    eprintln!(
        "[apps] 应用列表已刷新：{count} 个（{} ms）",
        started.elapsed().as_millis()
    );
    let _ = app.emit("apps-changed", ());
}

/// 按配置里的自定义顺序重排（app_order 中出现的按首次出现位置；
/// 未出现的按本地化名称排序追加在后）。纯函数，便于单测。
pub fn apply_app_order(apps: &mut [AppEntry], order: &[String]) {
    let mut rank: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (i, id) in order.iter().enumerate() {
        let id = id.trim();
        if !id.is_empty() {
            rank.entry(id).or_insert(i);
        }
    }
    apps.sort_by(|a, b| match (rank.get(a.id.as_str()), rank.get(b.id.as_str())) {
        (Some(ra), Some(rb)) => ra.cmp(rb),
        (Some(_), None) => CmpOrdering::Less,
        (None, Some(_)) => CmpOrdering::Greater,
        (None, None) => compare_names(&a.name, &b.name),
    });
}

/// 按当前配置的自定义顺序重排缓存并通知前端（用于 set_app_order，不重新枚举）。
pub fn reapply_order(app: &AppHandle) {
    let cfg = app
        .state::<AppState>()
        .config
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    {
        let st = app.state::<AppState>();
        let mut apps = st.apps.lock().unwrap_or_else(|e| e.into_inner());
        apply_app_order(&mut apps, &cfg.app_order);
    }
    let _ = app.emit("apps-changed", ());
}

/// 把新导入的应用插入缓存（已存在则替换同名 id），按配置顺序重排并通知前端。
pub fn cache_upsert(app: &AppHandle, entry: AppEntry) {
    let cfg = app
        .state::<AppState>()
        .config
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    {
        let st = app.state::<AppState>();
        let mut apps = st.apps.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = apps.iter().position(|a| a.id == entry.id) {
            apps[pos] = entry;
        } else {
            apps.push(entry);
        }
        apply_app_order(&mut apps, &cfg.app_order);
    }
    let _ = app.emit("apps-changed", ());
}

/// 从缓存移除某个应用（移除导入应用时使用）并通知前端。
pub fn cache_remove(app: &AppHandle, id: &str) {
    {
        let st = app.state::<AppState>();
        let mut apps = st.apps.lock().unwrap_or_else(|e| e.into_inner());
        apps.retain(|a| a.id != id);
    }
    let _ = app.emit("apps-changed", ());
}

/// 合并经典应用与打包应用：按 id 去重，另外对「同名 + 同启动目标」的重复项去重
/// （用户/公共 Start Menu 常出现同一快捷方式）。纯函数，便于单测。
pub fn merge_apps(classic: Vec<AppEntry>, packaged: Vec<AppEntry>) -> Vec<AppEntry> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(classic.len() + packaged.len());
    for entry in classic.into_iter().chain(packaged) {
        let id = entry.id.trim().to_lowercase();
        if id.is_empty() || !seen.insert(id) {
            continue;
        }
        let key = format!(
            "{}|{}",
            entry.name.trim().to_lowercase(),
            entry.launch.trim().replace('/', "\\").to_lowercase()
        );
        if !seen.insert(key) {
            continue;
        }
        out.push(entry);
    }
    out
}

/// 打包应用的 Logo 资源常带大块透明留白（150×150 资源里图形只占中间一小块），
/// 直接显示会明显偏小；这里统一裁掉留白并缩放到 48px 方形画布。
fn normalize_uwp_icons(apps: &mut [AppEntry]) {
    for entry in apps.iter_mut().filter(|e| e.kind == "uwp") {
        if let Some(url) = entry.icon.as_deref() {
            if let Some(fixed) = winapi::icons::normalize_app_icon(url, 48) {
                entry.icon = Some(fixed);
            }
        }
    }
}

pub fn compare_names(a: &str, b: &str) -> CmpOrdering {
    if a == b {
        return CmpOrdering::Equal;
    }
    let wa: Vec<u16> = a.encode_utf16().chain(std::iter::once(0)).collect();
    let wb: Vec<u16> = b.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let r = CompareStringEx(
            PCWSTR::null(),
            COMPARE_STRING_FLAGS(0),
            &wa,
            &wb,
            None,
            None,
            LPARAM(0),
        );
        if r == CSTR_LESS_THAN {
            CmpOrdering::Less
        } else if r == CSTR_GREATER_THAN {
            CmpOrdering::Greater
        } else if r == CSTR_EQUAL {
            CmpOrdering::Equal
        } else {
            fallback_compare(a, b)
        }
    }
}

fn fallback_compare(a: &str, b: &str) -> CmpOrdering {
    a.to_lowercase().cmp(&b.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str, launch: &str) -> AppEntry {
        AppEntry {
            id: id.into(),
            name: name.into(),
            icon: None,
            launch: launch.into(),
            args: String::new(),
            kind: "classic".into(),
        }
    }

    #[test]
    fn merge_dedupes_by_id() {
        let classic = vec![app("lnk:a", "A", r"C:\a.lnk"), app("lnk:dup", "A", r"C:\a.lnk")];
        let packaged = vec![app("lnk:dup", "A", r"C:\a.lnk")];
        let merged = merge_apps(classic, packaged);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].id, "lnk:a");
    }

    #[test]
    fn merge_dedupes_same_name_and_launch_ignoring_case() {
        let classic = vec![
            app("lnk:1", "Notepad", r"C:\Windows\notepad.lnk"),
            app("lnk:2", "notepad", r"c:/windows/NOTEPAD.LNK"),
        ];
        let merged = merge_apps(classic, vec![]);
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn merge_keeps_same_name_different_launch() {
        let classic = vec![
            app("lnk:1", "Tool", r"C:\a\tool.lnk"),
            app("lnk:2", "Tool", r"C:\b\tool.lnk"),
        ];
        assert_eq!(merge_apps(classic, vec![]).len(), 2);
    }

    #[test]
    fn compare_is_reflexive_and_orders_letters() {
        assert_eq!(compare_names("abc", "abc"), CmpOrdering::Equal);
        assert_eq!(compare_names("Alpha", "Beta"), CmpOrdering::Less);
        assert_eq!(compare_names("Beta", "Alpha"), CmpOrdering::Greater);
    }

    #[test]
    fn apply_app_order_puts_ranked_first_and_appends_unknowns() {
        let mut apps = vec![
            app("a", "Alpha", "a.exe"),
            app("b", "Beta", "b.exe"),
            app("c", "Gamma", "c.exe"),
        ];
        apply_app_order(&mut apps, &["c".into(), "a".into()]);
        let ids: Vec<&str> = apps.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, vec!["c", "a", "b"]);

        // 空 order 时按名称排序
        let mut apps = vec![app("b", "Beta", "b.exe"), app("a", "Alpha", "a.exe")];
        apply_app_order(&mut apps, &[]);
        assert_eq!(apps[0].id, "a");
    }
}

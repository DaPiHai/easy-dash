//! 任务托盘镜像工作线程（编排 UIA 枚举 + 图标截图 + 缓存）。
//!
//! UI Automation 的跨进程调用以及屏幕截图都可能在没有消息泵的线程上阻塞，
//! 因此全部放在本专用线程里串行执行，绝不在主线程/IPC 线程上同步调用。
//! 所有失败都只降级（tray_available = false、图标为 None），绝不 panic：
//! 一旦该线程被 panic 杀死，顶栏托盘会永久失去刷新能力。
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;

use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use crate::app_state::AppState;
use crate::types::{TrayGeom, TrayItem};
use crate::winapi;

/// 启动托盘镜像线程（整个进程只应调用一次，Receiver 已被 take 走时直接退出）。
pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        // MTA：托盘 UIA 调用随时可能跨进程，消息泵不是必需的；失败也继续
        // （RPC_E_CHANGED_MODE 等由 CoInitializeEx 自身返回，这里不视为致命）
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }

        let state = app.state::<AppState>();
        let rx: Receiver<()> = match state.tray_rx.lock().map(|mut g| g.take()) {
            Ok(Some(rx)) => rx,
            // 没有 Receiver：要么已经启动过线程，要么状态被提前消费掉
            _ => return,
        };

        // 图标缓存：key = name+x+y+w+h。托盘图标只在悬停/换图标时才会变，
        // 命中直接复用，避免每次刷新都截图
        let mut icon_cache: HashMap<String, Option<String>> = HashMap::new();

        while rx.recv().is_ok() {
            let (items, available) = refresh_once(&app, &mut icon_cache);
            {
                let st = app.state::<AppState>();
                if available {
                    st.tray
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone_from(&items);
                }
                // 枚举失败：保留旧缓存（前端显示降级提示），只翻转可用标志
                st.tray_available.store(available, Ordering::Relaxed);
            }
            // 无论成功失败都通知前端重绘
            let _ = app.emit("tray-changed", &items);
        }
    });
}

/// 请求一次托盘刷新（非阻塞；工作线程完成后发 tray-changed 事件）
pub fn request_refresh(app: &AppHandle) {
    let _ = app.state::<AppState>().tray_tx.send(());
}

/// 枚举一次托盘并补齐图标。返回 (条目列表, 枚举是否成功)。
///
/// 枚举失败时返回缓存里的旧条目（可能是空），由调用方决定是否覆盖状态。
fn refresh_once(
    app: &AppHandle,
    icon_cache: &mut HashMap<String, Option<String>>,
) -> (Vec<TrayItem>, bool) {
    let geoms = match winapi::tray::enumerate() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("[tray-mirror] 托盘枚举失败，降级为旧数据：{e}");
            let st = app.state::<AppState>();
            let old = st
                .tray
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            return (old, false);
        }
    };

    // 清掉已消失条目的缓存，防止长时间运行后无限增长
    let live: HashSet<String> = geoms.iter().map(cache_key).collect();
    icon_cache.retain(|k, _| live.contains(k));

    let mut items = Vec::with_capacity(geoms.len());
    for geom in geoms {
        let icon = icon_for(&geom, icon_cache);
        items.push(TrayItem {
            name: geom.name,
            kind: geom.kind,
            icon,
            x: geom.x,
            y: geom.y,
            w: geom.w,
            h: geom.h,
        });
    }
    (items, true)
}

/// 缓存键：位置/尺寸变化（托盘重排、分辨率切换）会自然失效重新截图
fn cache_key(g: &TrayGeom) -> String {
    format!("{}|{}|{}|{}|{}", g.name, g.x, g.y, g.w, g.h)
}

/// 取（或复用）单个条目的图标截图
fn icon_for(g: &TrayGeom, cache: &mut HashMap<String, Option<String>>) -> Option<String> {
    // 「显示隐藏的图标」按钮本身没有应用图标，不需要截图
    if g.kind != "icon" {
        return None;
    }
    let key = cache_key(g);
    if let Some(hit) = cache.get(&key) {
        return hit.clone();
    }
    let icon = g
        .icon_rect
        .and_then(|(x, y, w, h)| winapi::snapshot::capture_screen_region_png(x, y, w, h));
    // 截图失败也写入缓存：否则每轮刷新都对同一区域重复尝试，白耗 CPU
    cache.insert(key, icon.clone());
    icon
}

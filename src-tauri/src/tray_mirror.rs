//! 任务托盘镜像工作线程（编排 UIA 枚举 + 图标解析 + 缓存）。
//!
//! UI Automation 的跨进程调用以及注册表/版本信息读取都可能在没有消息泵的
//! 线程上阻塞，因此全部放在本专用线程里串行执行，绝不在主线程/IPC 线程上
//! 同步调用。所有失败都只降级（tray_available = false、图标为 None），
//! 绝不 panic：一旦该线程被 panic 杀死，顶栏托盘会永久失去刷新能力。
//!
//! 图标来源：`NotifyIconSettings` 的真实透明 PNG（见 winapi::app_icons）。
//! 其中 UIA 名称为空的托盘项（部分应用不设置 tooltip）通过屏幕图标视觉匹配
//! 识别；QQ 等登录状态图标用 exe 图标代替静态快照，避免显示成离线状态。
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;

use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use crate::app_state::AppState;
use crate::types::TrayItem;
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

        // 图标缓存已移除：注册表条目与图标解码在 app_icons 内部 OnceLock 缓存，
        // 视觉匹配只对「名称为空」的条目执行，刷新成本可忽略。
        while rx.recv().is_ok() {
            let (items, available) = refresh_once(&app);
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
fn refresh_once(app: &AppHandle) -> (Vec<TrayItem>, bool) {
    let geoms = match winapi::tray::enumerate() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("[tray-mirror] 托盘枚举失败，降级为旧数据：{e}");
            let st = app.state::<AppState>();
            let old = st.tray.lock().unwrap_or_else(|e| e.into_inner()).clone();
            return (old, false);
        }
    };

    let mut items: Vec<TrayItem> = geoms
        .into_iter()
        .map(|geom| TrayItem {
            name: geom.name,
            kind: geom.kind,
            icon: None,
            x: geom.x,
            y: geom.y,
            w: geom.w,
            h: geom.h,
        })
        .collect();

    // 名称匹配 + 无名项视觉匹配，一次补齐图标（也会给无名项补上显示名）
    winapi::app_icons::resolve_tray_items(&mut items);
    (items, true)
}

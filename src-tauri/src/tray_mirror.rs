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
use std::collections::HashMap;
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
            // 占位，随后由 app_icons::resolve_tray_items 填稳定排序 key
            order_key: String::new(),
            x: geom.x,
            y: geom.y,
            w: geom.w,
            h: geom.h,
        })
        .collect();

    // 名称匹配 + 无名项视觉匹配，一次补齐图标（也会给无名项补上显示名）
    winapi::app_icons::resolve_tray_items(&mut items);

    // 按用户自定义顺序重排。必须发生在写 state.tray / emit 之前，
    // 保证 tray_click(index) 的下标与前端看到的顺序一致。
    let order = app
        .state::<AppState>()
        .config
        .lock()
        .map(|cfg| cfg.tray_order.clone())
        .unwrap_or_default();
    apply_tray_order(&mut items, &order);

    (items, true)
}

/// 按用户配置的托盘顺序重排（前端拖拽排序的后端落地）。
///
/// - `overflow` 永远排最前（多个 overflow 之间保持枚举相对顺序）；
/// - `kind == "icon"` 的条目按 `order` 中**首次出现**的 order_key 位置排序；
/// - 不在 `order` 里的条目按原生 x 坐标追加在后。
/// `sort_by` 是稳定排序，因此权值相同的条目保持原顺序。
pub fn apply_tray_order(items: &mut Vec<TrayItem>, order: &[String]) {
    let mut rank: HashMap<&str, usize> = HashMap::new();
    for (index, key) in order.iter().enumerate() {
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        // 重复 key 以第一次出现的位置为准
        rank.entry(key).or_insert(index);
    }
    items.sort_by(|a, b| order_rank(a, &rank).cmp(&order_rank(b, &rank)));
}

/// 排序权值：(分组, 组内位置)。
/// 0 = overflow（overflow 之间权值相同 → 稳定排序保持相对顺序）；
/// 1 = 在 order 中（位置 = 首次出现下标）；
/// 2 = 未列出（位置 = 原生 x）。
fn order_rank(item: &TrayItem, rank: &HashMap<&str, usize>) -> (u8, i64) {
    if item.kind == "overflow" {
        return (0, 0);
    }
    match rank.get(item.order_key.as_str()) {
        Some(&position) => (1, position as i64),
        None => (2, item.x as i64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: &str, order_key: &str, x: i32) -> TrayItem {
        TrayItem {
            name: format!("{order_key}-{x}"),
            kind: kind.into(),
            icon: None,
            order_key: order_key.into(),
            x,
            y: 0,
            w: 32,
            h: 48,
        }
    }

    fn keys(items: &[TrayItem]) -> Vec<&str> {
        items.iter().map(|i| i.order_key.as_str()).collect()
    }

    fn names(items: &[TrayItem]) -> Vec<&str> {
        items.iter().map(|i| i.name.as_str()).collect()
    }

    #[test]
    fn overflow_first_then_custom_order() {
        let mut items = vec![
            item("icon", "weixin", 100),
            item("overflow", "overflow", 0),
            item("icon", "qq", 60),
            item("icon", "douyintray", 80),
        ];
        apply_tray_order(&mut items, &["qq".to_string(), "weixin".to_string()]);
        assert_eq!(keys(&items), vec!["overflow", "qq", "weixin", "douyintray"]);
    }

    #[test]
    fn unlisted_items_follow_in_native_x_order() {
        let mut items = vec![
            item("icon", "b", 200),
            item("icon", "a", 100),
            item("icon", "c", 300),
        ];
        apply_tray_order(&mut items, &[]);
        assert_eq!(keys(&items), vec!["a", "b", "c"]);
    }

    #[test]
    fn listed_items_come_before_unlisted_regardless_of_x() {
        let mut items = vec![item("icon", "c", 10), item("icon", "a", 300)];
        apply_tray_order(&mut items, &["c".to_string()]);
        assert_eq!(keys(&items), vec!["c", "a"]);
    }

    #[test]
    fn duplicate_order_entries_use_first_position() {
        let mut items = vec![item("icon", "b", 50), item("icon", "a", 10)];
        apply_tray_order(
            &mut items,
            &["b".to_string(), "a".to_string(), "b".to_string()],
        );
        assert_eq!(keys(&items), vec!["b", "a"]);
    }

    #[test]
    fn overflow_keeps_relative_order() {
        let mut items = vec![
            item("overflow", "overflow", 30),
            item("icon", "a", 100),
            item("overflow", "overflow", 20),
        ];
        apply_tray_order(&mut items, &["a".to_string()]);
        // overflow 之间保持枚举顺序（不按 x 重排）
        assert_eq!(names(&items), vec!["overflow-30", "overflow-20", "a-100"]);
    }

    /// 本机冒烟：真实 UIA 托盘枚举 + 真实注册表图标匹配 → 打印 order_key 与排序效果。
    /// 手动执行：`cargo test --lib -- --ignored smoke_real_tray_order --nocapture`
    #[test]
    #[ignore]
    fn smoke_real_tray_order() {
        let geoms = match crate::winapi::tray::enumerate() {
            Ok(g) => g,
            Err(e) => {
                eprintln!("[tray-order] 托盘枚举失败（无桌面会话？）: {e}");
                return;
            }
        };
        let mut items: Vec<TrayItem> = geoms
            .into_iter()
            .map(|geom| TrayItem {
                name: geom.name,
                kind: geom.kind,
                icon: None,
                order_key: String::new(),
                x: geom.x,
                y: geom.y,
                w: geom.w,
                h: geom.h,
            })
            .collect();
        crate::winapi::app_icons::resolve_tray_items(&mut items);
        let native: Vec<String> = items
            .iter()
            .map(|i| format!("{}={}", i.order_key, i.name))
            .collect();
        eprintln!("[tray-order] 原生顺序（按 x）: {native:?}");
        if items.is_empty() {
            eprintln!("[tray-order] 本机没有可见托盘图标，跳过排序校验");
            return;
        }
        assert!(
            items.iter().all(|i| !i.order_key.is_empty()),
            "每个条目都应有 order_key"
        );

        // 模拟用户把第一个应用图标拖到最前（overflow 之后）
        let first_icon_key = items
            .iter()
            .find(|i| i.kind == "icon")
            .map(|i| i.order_key.clone());
        let order: Vec<String> = first_icon_key.clone().into_iter().collect();
        apply_tray_order(&mut items, &order);
        let reordered: Vec<String> = items.iter().map(|i| i.order_key.clone()).collect();
        eprintln!("[tray-order] 应用自定义顺序 {order:?} 后: {reordered:?}");
        if items.iter().any(|i| i.kind == "overflow") {
            assert_eq!(items[0].kind, "overflow", "overflow 应永远排最前");
        }
        if let Some(key) = first_icon_key {
            let idx = items
                .iter()
                .position(|i| i.order_key == key)
                .unwrap_or(usize::MAX);
            let overflow_count = items.iter().filter(|i| i.kind == "overflow").count();
            assert_eq!(idx, overflow_count, "被拖到最前的图标应紧跟 overflow");
        }
    }
}

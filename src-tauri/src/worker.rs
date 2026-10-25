//! 图标工作线程：串行执行 AUMID 读取 / AppsFolder 图标 / 窗口图标提取。
//!
//! 这些 Shell COM 调用在无消息泵的线程上可能永久阻塞，
//! 因此绝不能放在监控线程（热路径）或主线程（IPC 处理）上。
//! 本线程以 MTA 初始化 COM；即使某个调用卡死，也只损失图标，不影响
//! 窗口枚举、事件推送、看门狗等核心功能。
use std::sync::mpsc::Receiver;

use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::Foundation::HWND;

use crate::app_state::AppState;
use crate::winapi;

pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        // STA + 消息泵：AppsFolder 缩略图工厂需要 STA；泵保证分派送来的回调
        winapi::icons::ensure_com();
        let state = app.state::<AppState>();
        let rx: Receiver<isize> = match state.icon_rx.lock().unwrap().take() {
            Some(rx) => rx,
            None => return,
        };
        while let Ok(id) = rx.recv() {
            resolve(id, &app);
            pump_messages();
            app.state::<AppState>()
                .icon_pending
                .lock()
                .unwrap()
                .remove(&id);
            let _ = app.emit("icon-ready", id);
        }
    });
}

fn pump_messages() {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn resolve(id: isize, app: &AppHandle) {
    let hwnd = HWND(id as usize as *mut _);
    let state = app.state::<AppState>();
    let full_path = winapi::common::process_image_path(hwnd).unwrap_or_default();
    let exe = full_path
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or("")
        .to_lowercase();
    let class = winapi::common::class_name(hwnd);
    let is_uwp_host = class.eq_ignore_ascii_case("ApplicationFrameWindow")
        || exe == "applicationframehost.exe"
        || full_path.to_lowercase().contains("\\windowsapps\\");

    // UWP 宿主窗口：AUMID → AppsFolder 图标（桌面应用不看 AUMID：
    // 浏览器等的 AUMID 是无效值，只能靠窗口/exe 图标）
    if is_uwp_host {
        if let Some(a) = winapi::icons::window_aumid(hwnd).filter(|s| !s.trim().is_empty()) {
            state.hwnd_aumid.lock().unwrap().insert(id, a.clone());
            let key = format!("aumid:{a}");
            if !state.icon_cache.lock().unwrap().contains_key(&key) {
                match winapi::icons::aumid_app_info(&a) {
                    Some((url, disp)) => {
                        state
                            .icon_cache
                            .lock()
                            .unwrap()
                            .insert(key.clone(), Some(url));
                        if !disp.is_empty() {
                            state
                                .aumid_names
                                .lock()
                                .unwrap()
                                .insert(key.clone(), disp);
                        }
                    }
                    None => {
                        eprintln!("[icon-worker] AppsFolder 图标失败: {a}");
                        state.icon_cache.lock().unwrap().insert(key.clone(), None);
                    }
                }
            }
            return;
        }
    }

    // 桌面窗口（或无 AUMID 的 UWP 宿主）：WM_GETICON → 类图标 → exe 文件图标
    let key = if exe.is_empty() {
        format!("hwnd:{id}")
    } else {
        exe
    };
    if state.icon_cache.lock().unwrap().contains_key(&key) {
        return;
    }
    let icon = winapi::icons::window_icon_data_url(hwnd);
    state.icon_cache.lock().unwrap().insert(key, icon);
}

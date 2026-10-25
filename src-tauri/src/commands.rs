//! 前端可调用的 Tauri 命令
use tauri::{AppHandle, Emitter, Manager, State};

use crate::app_state::AppState;
use crate::config;
use crate::winapi;

#[tauri::command]
pub fn list_windows(state: State<AppState>) -> Vec<winapi::enumerate::WinEntry> {
    winapi::enumerate::list_windows(std::process::id(), &state)
}

#[tauri::command]
pub fn toggle_overview(app: AppHandle) {
    crate::overview::toggle(&app);
}

#[tauri::command]
pub fn close_overview(app: AppHandle) {
    crate::overview::hide(&app);
}

#[tauri::command]
pub fn activate_window(_app: AppHandle, id: i64) {
    winapi::activate::activate_id(id);
}

/// Dock 点击：点击前已是前台 → 最小化；最小化中 → 还原；否则激活
#[tauri::command]
pub fn toggle_window(state: State<AppState>, _app: AppHandle, id: i64) {
    let last_fg = state
        .last_foreign_fg
        .load(std::sync::atomic::Ordering::Relaxed);
    winapi::activate::toggle_id(id, last_fg);
}

#[tauri::command]
pub fn launch_app(path: String, args: Option<String>) -> Result<(), String> {
    winapi::launcher::launch(&path, args.as_deref().unwrap_or(""))
}

#[tauri::command]
pub fn get_config(state: State<AppState>) -> config::Config {
    state.config.lock().unwrap().clone()
}

#[tauri::command]
pub fn set_pinned(
    app: AppHandle,
    state: State<AppState>,
    pinned: Vec<config::PinnedApp>,
) -> Result<(), String> {
    {
        let mut cfg = state.config.lock().unwrap();
        cfg.pinned = pinned;
        config::save(&app, &cfg)?;
    }
    let cfg = state.config.lock().unwrap().clone();
    let _ = app.emit("config-changed", &cfg);
    Ok(())
}

#[tauri::command]
pub fn reload_config(app: AppHandle) -> Result<(), String> {
    do_reload_config(&app)
}

/// 托盘「重载配置」与 reload_config 命令共用
pub fn do_reload_config(app: &AppHandle) -> Result<(), String> {
    let cfg = config::load_or_init(app)?;
    {
        let state = app.state::<AppState>();
        *state.config.lock().unwrap() = cfg.clone();
    }
    // 重新注册热键
    {
        use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
        let gs = app.global_shortcut();
        let _ = gs.unregister_all();
        let hotkey = cfg.hotkey.clone();
        let _ = gs.on_shortcut(hotkey.as_str(), |app, _s, event| {
            if event.state() == ShortcutState::Pressed {
                crate::overview::toggle(app);
            }
        });
    }
    let _ = app.emit("config-changed", &cfg);
    Ok(())
}

#[tauri::command]
pub fn get_status() -> serde_json::Value {
    let (battery, charging, network) = winapi::power::status();
    serde_json::json!({ "battery": battery, "charging": charging, "network": network })
}

/// 固定应用启动项的图标（Rust 侧缓存）。支持 exe 路径与
/// shell:AppsFolder\<AUMID>（UWP 固定项）两种形式。
/// 打包应用图标提取放到工作线程（可能阻塞），本次先返回缓存，完成时发 icon-ready。
#[tauri::command]
pub fn get_pinned_icons(
    app: AppHandle,
    state: State<AppState>,
    paths: Vec<String>,
) -> Vec<Option<String>> {
    const MONIKER: &str = "shell:AppsFolder\\";
    let mut out = Vec::with_capacity(paths.len());
    for p in &paths {
        let moniker_aumid = p.strip_prefix(MONIKER);
        let key = match moniker_aumid {
            Some(a) => format!("aumid:{a}"),
            None => format!("file:{}", p.to_lowercase()),
        };
        {
            let cache = state.icon_cache.lock().unwrap();
            if let Some(hit) = cache.get(&key) {
                out.push(hit.clone());
                continue;
            }
        }
        match moniker_aumid {
            Some(a) => {
                // 工作线程提取（AUMID AppsFolder 图标），避免主线程阻塞
                let aumid = a.to_string();
                let key2 = key.clone();
                let app2 = app.clone();
                std::thread::spawn(move || {
                    crate::winapi::icons::ensure_com();
                    let state2 = app2.state::<AppState>();
                    match crate::winapi::icons::aumid_app_info(&aumid) {
                        Some((url, disp)) => {
                            state2
                                .icon_cache
                                .lock()
                                .unwrap()
                                .insert(key2.clone(), Some(url));
                            if !disp.is_empty() {
                                state2.aumid_names.lock().unwrap().insert(key2, disp);
                            }
                        }
                        None => {
                            state2.icon_cache.lock().unwrap().insert(key2, None);
                        }
                    }
                    let _ = app2.emit("icon-ready", 0i64);
                });
                out.push(None);
            }
            None => {
                let icon = winapi::icons::file_icon_data_url(p);
                state
                    .icon_cache
                    .lock()
                    .unwrap()
                    .insert(key, icon.clone());
                out.push(icon);
            }
        }
    }
    out
}

/// 保存未固定应用的手动排序（拖拽排序用）
#[tauri::command]
pub fn set_dock_order(app: AppHandle, state: State<AppState>, keys: Vec<String>) -> Result<(), String> {
    let mut cfg = state.config.lock().unwrap();
    cfg.dock_order = keys;
    config::save(&app, &cfg)
}

/// Dock 页面心跳（页面每 3 秒调用一次）
#[tauri::command]
pub fn dock_heartbeat(state: State<AppState>) {
    state
        .dock_hb_ms
        .store(crate::app_state::now_ms(), std::sync::atomic::Ordering::Relaxed);
}

/// Dock 渲染完成后校正窗口尺寸与底部居中位置（逻辑像素）
#[tauri::command]
pub fn resize_dock(app: AppHandle, width: f64, height: f64) {
    use tauri::{LogicalPosition, LogicalSize};
    if let Some(dock) = app.get_webview_window("dock") {
        if let Ok(Some(m)) = app.primary_monitor() {
            let scale = m.scale_factor();
            let sw = m.size().width as f64 / scale;
            let sh = m.size().height as f64 / scale;
            // 宽度不超屏幕；超高截断
            let w = width.max(1.0).min(sw);
            let h = height.max(1.0).min(sh);
            let _ = dock.set_size(LogicalSize::new(w, h));
            let _ = dock.set_position(LogicalPosition::new(
                ((sw - w) / 2.0).max(0.0),
                (sh - h).max(0.0),
            ));
        }
    }
}

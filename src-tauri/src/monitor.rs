//! 后台轮询：窗口列表变化推送 + 全屏检测
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

use crate::app_state::AppState;
use crate::winapi;

pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        // shell 图标提取（UWP AUMID 通道）需要 COM
        winapi::icons::ensure_com();
        let own_pid = std::process::id();
        let mut last_sig = String::new();
        let mut last_fullscreen = false;
        let mut last_fg: isize = 0;
        let mut tick: u64 = 0;
        let mut pending_emit = false;
        let mut last_emit = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(10))
            .unwrap_or_else(std::time::Instant::now);
        loop {
            let state = app.state::<AppState>();
            tick += 1;

            // 窗口列表（按需推送，最快 1 秒一次，防事件风暴拖垮 WebView）
            let wins = winapi::enumerate::list_windows(own_pid, &state);
            let sig: String = wins
                .iter()
                .map(|w| {
                    format!(
                        "{}\u{1f}{}\u{1f}{}\u{1f}{}\n",
                        w.id,
                        w.title,
                        w.minimized,
                        w.aumid.as_deref().unwrap_or("")
                    )
                })
                .collect();
            if sig != last_sig {
                last_sig = sig;
                pending_emit = true;
            }
            if pending_emit && last_emit.elapsed() >= Duration::from_millis(1000) {
                last_emit = std::time::Instant::now();
                pending_emit = false;
                let _ = app.emit("windows-changed", &wins);
            }

            // 前台窗口变化（Dock 指示点）；自己的窗口抢前台时忽略
            let own = state.own_hwnds.lock().unwrap().clone();
            let fg_id = winapi::fullscreen::foreground_id();
            if fg_id != 0 && !own.contains(&fg_id) {
                state
                    .last_foreign_fg
                    .store(fg_id, std::sync::atomic::Ordering::Relaxed);
                if fg_id != last_fg {
                    last_fg = fg_id;
                    let _ = app.emit("foreground-changed", fg_id);
                }
            }

            // 全屏检测
            let fs = winapi::fullscreen::is_foreground_fullscreen(&own);
            if fs != last_fullscreen {
                last_fullscreen = fs;
                let hide_enabled = state.config.lock().unwrap().fullscreen_hide;
                let _ = app.emit("fullscreen-changed", fs && hide_enabled);
            }

            // 看门狗：Dock 心跳超时（页面 JS 死亡/渲染进程崩溃）→ reload 自愈
            if tick % 5 == 0 {
                let now = crate::app_state::now_ms();
                let last = state.dock_hb_ms.load(std::sync::atomic::Ordering::Relaxed);
                if now.saturating_sub(last) > 15_000 {
                    if let Some(dock) = app.get_webview_window("dock") {
                        if dock.is_visible().unwrap_or(false) {
                            eprintln!("[easy-dash] Dock 心跳超时，reload 自愈");
                            let _ = dock.eval("location.reload()");
                            state.dock_hb_ms.store(now, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                }
            }

            // 周期性把顶栏/Dock 抢回 topmost 顶端（约 2 秒一次），
            // 同时校正顶栏位置（任务栏被移动后自动避让）
            if tick % 5 == 0 {
                if let Some(topbar) = app.get_webview_window("topbar") {
                    winapi::appbar::sync_topbar(&topbar);
                }
                for label in ["topbar", "dock"] {
                    if let Some(w) = app.get_webview_window(label) {
                        if let Some(h) = winapi::common::tauri_hwnd(&w) {
                            winapi::common::raise_topmost(h);
                        }
                    }
                }
            }

            drop(state);
            std::thread::sleep(Duration::from_millis(400));
        }
    });
}

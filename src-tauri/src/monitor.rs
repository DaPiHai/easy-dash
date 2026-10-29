//! 后台轮询：窗口列表变化推送 + 按显示器全屏检测 + 多屏布局变化检测
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

use crate::app_state::AppState;
use crate::surfaces;
use crate::winapi;

pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        // shell 图标提取（UWP AUMID 通道）需要 COM
        winapi::icons::ensure_com();
        let own_pid = std::process::id();
        let mut last_sig = String::new();
        let mut last_fullscreen: Option<String> = None;
        let mut last_fg: isize = 0;
        let mut tick: u64 = 0;
        let mut pending_emit = false;
        // 上一次工作区底边（发生变化时通知 Dock 重新贴边）
        let mut last_wa_bottom: i32 = -1;
        let mut last_emit = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(10))
            .unwrap_or_else(std::time::Instant::now);
        // 显示器布局签名（每 ~2s 比对一次）
        let mut last_monitor_sig = String::new();
        // 布局签名首轮初始化
        let mut monitor_sig_ready = false;
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

            // 全屏检测：按前台窗口所在显示器判定，只影响该屏
            let fs = winapi::fullscreen::foreground_fullscreen_monitor(&own);
            if fs != last_fullscreen {
                last_fullscreen = fs.clone();
                if let Ok(mut guard) = state.fullscreen_monitor.lock() {
                    *guard = fs;
                }
                surfaces::emit_fullscreen_states(&app);
            }

            // 任务栏隐藏看门狗：补回被 Explorer 重置的风格、重新 SW_HIDE、
            // Explorer 重启后重新发现窗口；托盘桥接期间自动让路。
            winapi::taskbar::watchdog();

            // 工作区底边变化（任务栏隐身接管/还原、显示设置变化）→ 通知 Dock 重新贴边。
            let wa_bottom = winapi::taskbar::workarea_bottom_physical().unwrap_or(0);
            if wa_bottom != last_wa_bottom {
                last_wa_bottom = wa_bottom;
                let _ = app.emit("workarea-changed", wa_bottom);
            }

            // 显示器布局（拔插/改分辨率/改缩放/换主屏）：签名变化后主线程重建窗口
            if tick % 5 == 0 {
                let mut sig = String::new();
                if let Ok(monitors) = app.available_monitors() {
                    let mut list: Vec<(String, i32, i32, u32, u32, f64)> = monitors
                        .iter()
                        .map(|m| {
                            let p = m.position();
                            let s = m.size();
                            (
                                m.name().cloned().unwrap_or_default(),
                                p.x,
                                p.y,
                                s.width,
                                s.height,
                                m.scale_factor(),
                            )
                        })
                        .collect();
                    list.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
                    for (name, x, y, w, h, scale) in list {
                        sig.push_str(&format!("{name}|{w}x{h}+{x}+{y}@{scale:.3};"));
                    }
                }
                if let Ok(Some(primary)) = app.primary_monitor() {
                    sig.push_str("primary=");
                    sig.push_str(primary.name().map(|s| s.as_str()).unwrap_or(""));
                }
                if monitor_sig_ready && sig != last_monitor_sig {
                    surfaces::request_reconcile(&app);
                }
                last_monitor_sig = sig;
                monitor_sig_ready = true;
            }

            // 看门狗：每块屏的 Dock 心跳超时（页面 JS 死亡/渲染进程崩溃）→ reload 自愈
            if tick % 5 == 0 {
                let now = crate::app_state::now_ms();
                let labels = surfaces::dock_labels(&app);
                let stale: Vec<String> = {
                    let mut heartbeats = match state.dock_heartbeats.lock() {
                        Ok(g) => g,
                        Err(e) => e.into_inner(),
                    };
                    let mut stale = Vec::new();
                    for label in &labels {
                        let last = *heartbeats.entry(label.clone()).or_insert(now);
                        if now.saturating_sub(last) > 15_000 {
                            stale.push(label.clone());
                            heartbeats.insert(label.clone(), now);
                        }
                    }
                    stale
                };
                for label in stale {
                    if let Some(dock) = app.get_webview_window(&label) {
                        if dock.is_visible().unwrap_or(false) {
                            eprintln!("[easy-dash] Dock 心跳超时，reload 自愈: {label}");
                            let _ = dock.eval("location.reload()");
                        }
                    }
                }
            }

            // 主题：v0.2.2 起移除「跟随系统」，主题只由用户配置决定。

            // 周期性把顶栏/Dock 抢回 topmost 顶端（约 2 秒一次），
            // 同时校正顶栏位置（任务栏被移动后自动避让）。
            // 顶栏面板展开时 appbar::sync_topbar 会自己让路。
            if tick % 5 == 0 {
                for label in surfaces::topbar_labels(&app) {
                    if let Some(topbar) = app.get_webview_window(&label) {
                        winapi::appbar::sync_topbar(&topbar);
                    }
                }
                surfaces::raise_all_visible(&app);
                // 桌面图标避让看门狗：Explorer 重启/刷新桌面（F5）/改分辨率后，
                // 桌面视图矩形会被重置回整屏，这里按当前顶栏高度重新下移（幂等）。
                surfaces::sync_desktop_icon_inset(&app);
            }

            drop(state);
            std::thread::sleep(Duration::from_millis(400));
        }
    });
}

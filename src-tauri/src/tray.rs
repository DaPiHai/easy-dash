//! 系统托盘图标与菜单
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::App;

use crate::overview;

pub fn create(app: &App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "打开活动概览", true, None::<&str>)?;
    let reload = MenuItem::with_id(app, "reload", "重载配置", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 easy-dash", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &reload, &quit])?;

    let _tray = TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("easy-dash")
        .menu(&menu)
        .menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => overview::show(app),
            "reload" => {
                let _ = crate::commands::do_reload_config(app);
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                overview::toggle(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

//! 通过 ShellExecuteW 启动应用（支持 .exe / .lnk / 文件夹 / 一次性 URI）
#![allow(non_upper_case_globals)]

use windows::core::PCWSTR;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use super::common::{is_process_elevated, to_wide};

pub fn launch(path: &str, args: &str) -> Result<(), String> {
    // easy-dash 以管理员运行时，直接 ShellExecute 会让目标应用继承管理员权限；
    // 经 explorer.exe 转一手可让应用以普通权限启动（shell 会自动降权）
    if is_process_elevated() && args.trim().is_empty() {
        return launch_via_explorer(path);
    }
    launch_direct(path, args)
}

fn launch_via_explorer(path: &str) -> Result<(), String> {
    let exe = to_wide("explorer.exe");
    let verb = to_wide("open");
    let target = to_wide(&format!("\"{}\"", path));
    let inst = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(exe.as_ptr()),
            PCWSTR(target.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if (inst.0 as isize) > 32 {
        Ok(())
    } else {
        // explorer 转发失败时退回直接启动
        launch_direct(path, "")
    }
}

fn launch_direct(path: &str, args: &str) -> Result<(), String> {
    let file = to_wide(path);
    let verb = to_wide("open");
    let params = if args.trim().is_empty() {
        Vec::new()
    } else {
        to_wide(args.trim())
    };
    let inst = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            if params.is_empty() {
                PCWSTR::null()
            } else {
                PCWSTR(params.as_ptr())
            },
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // 返回值 > 32 表示成功
    if (inst.0 as isize) > 32 {
        Ok(())
    } else {
        Err(format!("启动失败（ShellExecute 返回 {}）", inst.0 as isize))
    }
}

//! 系统「打开文件」对话框（`GetOpenFileNameW`），用于应用中心「导入应用」。
//!
//! 该函数是同步阻塞的模态对话框，必须放在 blocking 线程里调用（commands.rs 已如此）。
//! 取消（或对话框失败）都返回 `Ok(None)`，由前端按“用户取消”处理，不 produce panic。
#![allow(non_upper_case_globals)]

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OPENFILENAMEW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR,
    OFN_PATHMUSTEXIST,
};

/// 打开系统文件选择框，让用户选一个 .exe / .lnk / .url。
///
/// - `owner`：父窗口 HWND 的数值（0 = 无 owner）。由 launcher 窗口传入，
///   这样对话框会以主界面为中心并保持模态。
/// - 返回 `Ok(Some(路径))` 表示选中；`Ok(None)` 表示取消 / 对话框无法显示。
pub fn pick_app_file(owner: isize) -> Result<Option<String>, String> {
    // lpstrFilter：每一段以 '\0' 分隔，整段以双 '\0' 结尾
    let filter: Vec<u16> = "应用程序\0*.exe;*.lnk;*.url\0所有文件\0*.*\0\0"
        .encode_utf16()
        .collect();
    let title: Vec<u16> = "导入应用到应用中心\0".encode_utf16().collect();

    // 长路径（含 \\?\ 前缀）也能放下；GetOpenFileNameW 就地写入这个缓冲区
    let mut file_buffer = vec![0u16; 32768];

    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: HWND(owner as *mut core::ffi::c_void),
        lpstrFilter: PCWSTR(filter.as_ptr()),
        nFilterIndex: 1,
        lpstrFile: PWSTR(file_buffer.as_mut_ptr()),
        nMaxFile: file_buffer.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR | OFN_EXPLORER,
        lCustData: LPARAM(0),
        ..Default::default()
    };

    let picked = unsafe { GetOpenFileNameW(&mut dialog) };
    if picked.0 == 0 {
        // 用户取消，或对话框创建失败（如无交互桌面）；统一按取消处理
        return Ok(None);
    }

    let end = file_buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(file_buffer.len());
    if end == 0 {
        return Ok(None);
    }
    Ok(Some(String::from_utf16_lossy(&file_buffer[..end])))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 会弹出真实的系统文件选择框（阻塞等待人工操作），默认 ignore。
    ///
    /// 手动执行：
    /// ```text
    /// cargo test --manifest-path src-tauri/Cargo.toml --lib pick_app_file_smoke -- --ignored --nocapture
    /// ```
    /// 在弹出的对话框里选一个 .exe/.lnk/.url，测试会打印选中路径；选「取消」则打印 None。
    #[test]
    #[ignore = "会弹出系统文件选择框，需要人工交互，手动运行"]
    fn pick_app_file_smoke() {
        let picked = pick_app_file(0).expect("GetOpenFileNameW 调用本身不应返回 Err");
        println!("pick_app_file 选中: {picked:?}");
    }
}

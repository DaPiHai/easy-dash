//! 通知 → 应用启动目标解析。
//!
//! Windows 通知库只给出处理器 PrimaryId（AUMID / exe 路径 / 裸名称），这里把它
//! 转换成可交给 ShellExecute 的启动目标：
//! - 打包应用 / 带 AUMID 的桌面应用 → `shell:AppsFolder\<AUMID>`（与开始菜单一致）；
//! - 路径 / 能按名称命中通知区域注册表的应用 → 可执行文件路径。
//!
//! 只做「解析 + 存在性校验」，不启动任何程序；任何失败都返回 None。
#![allow(dead_code)]

use windows::core::PCWSTR;
use windows::Win32::System::Com::IBindCtx;
use windows::Win32::UI::Shell::{IShellItem, SHCreateItemFromParsingName};

use super::app_icons;
use super::common::to_wide;

/// `shell:AppsFolder\` 前缀（与打包应用枚举保持一致）
const APPS_FOLDER: &str = "shell:AppsFolder\\";

/// 由通知处理器标识生成 AppsFolder 启动目标（纯函数，不访问系统）。
///
/// 只接受「像 AUMID 的标识」：
/// - `app:desktop` / `app:immersive` 的 PrimaryId（如 "QQ"、"dev.zcode.app"）；
/// - 任意类型但含 `!` 的 AUMID（打包应用）。
///
/// 路径、GUID（`{...}`）、含空白或分隔符的标识一律返回 None，交给
/// [`app_icons::launch_path`] 另行解析。
pub fn apps_folder_target(primary_id: &str, handler_type: &str) -> Option<String> {
    let id = primary_id.trim();
    if id.is_empty()
        || id.chars().any(char::is_whitespace)
        || id.contains('\\')
        || id.contains('/')
        || id.starts_with('{')
    {
        return None;
    }
    let app_type = matches!(handler_type.trim(), "app:desktop" | "app:immersive");
    if !app_type && !id.contains('!') {
        return None;
    }
    Some(format!("{APPS_FOLDER}{id}"))
}

/// 校验 AppsFolder 目标真实存在（创建 Shell 项，不启动应用）。
///
/// 失败（未注册的 AUMID / 无权限 / Shell 未初始化）都返回 false。
pub fn apps_folder_exists(moniker: &str) -> bool {
    if !moniker.starts_with(APPS_FOLDER) {
        return false;
    }
    let wide = to_wide(moniker);
    let item: windows::core::Result<IShellItem> =
        unsafe { SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None::<&IBindCtx>) };
    item.is_ok()
}

/// 解析通知处理器 → 启动目标（优先 AppsFolder，不存在时退回可执行文件路径）。
pub fn target_for(primary_id: &str, handler_type: &str) -> Option<String> {
    apps_folder_target(primary_id, handler_type)
        .filter(|target| apps_folder_exists(target))
        .or_else(|| app_icons::launch_path(primary_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apps_folder_target_for_app_handlers() {
        assert_eq!(
            apps_folder_target("QQ", "app:desktop").as_deref(),
            Some(r"shell:AppsFolder\QQ")
        );
        assert_eq!(
            apps_folder_target("dev.zcode.app", "app:desktop").as_deref(),
            Some(r"shell:AppsFolder\dev.zcode.app")
        );
        // 打包应用即使 handler_type 是 app:system，也允许尝试 AppsFolder
        assert_eq!(
            apps_folder_target("Microsoft.WindowsNotepad_8wekyb3d8bbwe!App", "app:system")
                .as_deref(),
            Some(r"shell:AppsFolder\Microsoft.WindowsNotepad_8wekyb3d8bbwe!App")
        );
    }

    #[test]
    fn apps_folder_target_rejects_non_aumid() {
        // 系统 Toast 处理器不是应用
        assert_eq!(
            apps_folder_target("Windows.SystemToast.Audio", "app:system"),
            None
        );
        // GUID 处理器无法映射
        assert_eq!(
            apps_folder_target("{61E2A762-852C-4B14-956E-36D624BE4307}", "app:desktop"),
            None
        );
        // 路径 / 含空白标识交给 app_icons 处理
        assert_eq!(apps_folder_target(r"C:\Tools\a.exe", "app:desktop"), None);
        assert_eq!(
            apps_folder_target("Microsoft Windows App", "app:desktop"),
            None
        );
        assert_eq!(apps_folder_target("", "app:desktop"), None);
    }

    /// 真机校验：本机 AppsFolder 里的常见应用应可解析（依赖已安装应用，手动执行）。
    #[test]
    #[ignore = "依赖本机已安装应用（QQ / 记事本），手动执行"]
    fn smoke_apps_folder_exists() {
        // 与通知工作线程一致：STA + 消息泵（shell 项创建需要 COM）
        crate::winapi::icons::ensure_com();
        assert!(apps_folder_exists(r"shell:AppsFolder\QQ"), "本机应安装 QQ");
        assert!(apps_folder_exists(
            r"shell:AppsFolder\Microsoft.WindowsNotepad_8wekyb3d8bbwe!App"
        ));
        assert!(!apps_folder_exists(r"shell:AppsFolder\__no_such_app__"));
    }
}

//! 应用中心「导入应用」：路径校验、稳定 id、`AppEntry` 构造。
//!
//! 只支持 `.exe` / `.lnk` / `.url` 三类；导入项的 id 由规范化路径推导，
//! 保证同一文件无论大小写 / 斜杠写法都得到同一个 id（与 config.imported_apps 对账）。
#![allow(dead_code)]

use std::path::Path;

use crate::types::AppEntry;

/// 支持导入的扩展名白名单（小写、不带点）。
const ALLOWED_EXTENSIONS: [&str; 3] = ["exe", "lnk", "url"];

/// 去掉 `canonicalize` 产生的 `\\?\` 前缀；`\\?\UNC\` 还原为普通 UNC 路径。
fn strip_verbatim_prefix(path: &str) -> String {
    let trimmed = path.trim();
    if let Some(rest) = trimmed.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = trimmed.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        trimmed.to_string()
    }
}

/// 规范化路径：去 `\\?\` 前缀、反斜杠统一、转小写。
fn normalize_path(path: &str) -> String {
    strip_verbatim_prefix(path).replace('/', "\\").to_lowercase()
}

/// 导入项稳定 id：`import:<规范化路径小写>`。
pub fn import_id(path: &str) -> String {
    format!("import:{}", normalize_path(path))
}

/// 校验并规范化导入路径（存在 + 扩展名白名单），返回去掉 `\\?\` 的规范路径。
pub fn validate_import_path(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("导入路径不能为空".into());
    }

    let canonical = std::fs::canonicalize(raw)
        .map_err(|e| format!("文件不存在或无法访问：{raw}（{e}）"))?;
    let path = strip_verbatim_prefix(&canonical.to_string_lossy());

    if !Path::new(&path).is_file() {
        return Err(format!("只能导入文件，不能导入文件夹：{path}"));
    }

    let extension = Path::new(&path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_lowercase())
        .unwrap_or_default();
    if !ALLOWED_EXTENSIONS.contains(&extension.as_str()) {
        return Err("仅支持导入 .exe / .lnk / .url 文件".into());
    }

    Ok(path)
}

/// 文件名去扩展名（用于没有 FileDescription / 自定义名称时的兜底显示名）。
fn fallback_name(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .filter(|stem| !stem.trim().is_empty())
        .unwrap_or_else(|| path.to_string())
}

/// 由文件路径构造导入项 `AppEntry`。
///
/// - 显示名：`name` 非空优先 → `resolve_identity` 的 FileDescription/主名 → 文件名去扩展名；
/// - 图标：`resolve_identity.path` 的图标 → Shell 48px 图标 → `SHGetFileInfo` 大图标（都失败则 None）；
/// - `args` 原样保留（导入项可带启动参数）。
pub fn make_entry(path: &str, name: &str, args: &str) -> Result<AppEntry, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("导入路径不能为空".into());
    }
    if !Path::new(path).is_file() {
        return Err(format!("导入目标不存在或不是文件：{path}"));
    }

    // 图标提取走 Shell（IShellItemImageFactory / SHGetFileInfo）。应用中心刷新线程已初始化
    // COM（MTA）；命令层的 blocking 线程可能还没初始化，这里补一次（重复初始化无害，
    // 已有 COM 时 CoInitializeEx 返回 S_FALSE / RPC_E_CHANGED_MODE 均被忽略）。
    super::icons::ensure_com();

    let identity = super::app_icons::resolve_identity(path);

    let display_name = if !name.trim().is_empty() {
        name.trim().to_string()
    } else {
        match &identity {
            Some(identity) if !identity.name.trim().is_empty() => identity.name.trim().to_string(),
            _ => fallback_name(path),
        }
    };

    let icon = identity
        .as_ref()
        .and_then(|identity| identity.icon.clone())
        .or_else(|| super::icons::shell_item_icon_png(path, 48))
        .or_else(|| super::icons::file_icon_data_url(path));

    Ok(AppEntry {
        id: import_id(path),
        name: display_name,
        icon,
        launch: path.to_string(),
        args: args.to_string(),
        kind: "import".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 本机 %SystemRoot%\System32\notepad.exe 路径（Windows 一定存在）。
    fn notepad_path() -> String {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
        format!(r"{root}\System32\notepad.exe")
    }

    /// 在临时目录造一个指定扩展名的文件，返回路径（测试结束自行删除）。
    fn temp_file(extension: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "easy-dash-import-test-{}-{}.{}",
            std::process::id(),
            extension,
            extension
        ));
        let mut file = std::fs::File::create(&path).expect("创建临时文件失败");
        file.write_all(b"test").ok();
        path
    }

    #[test]
    fn import_id_normalizes_case_and_slashes() {
        assert_eq!(
            import_id(r"C:/Apps/MyApp.EXE"),
            r"import:c:\apps\myapp.exe"
        );
        assert_eq!(
            import_id(r"C:\Apps\MyApp.exe"),
            r"import:c:\apps\myapp.exe"
        );
    }

    #[test]
    fn import_id_strips_verbatim_prefix() {
        assert_eq!(
            import_id(r"\\?\C:\Apps\MyApp.exe"),
            r"import:c:\apps\myapp.exe"
        );
        assert_eq!(
            import_id(r"\\?\UNC\server\share\app.exe"),
            r"import:\\server\share\app.exe"
        );
    }

    #[test]
    fn validate_rejects_missing_path() {
        let error = validate_import_path(r"C:\__easy_dash_no_such_file__.exe").unwrap_err();
        assert!(error.contains("不存在"), "错误文案应说明不存在：{error}");
    }

    #[test]
    fn validate_rejects_empty_path() {
        assert!(validate_import_path("   ").is_err());
    }

    #[test]
    fn validate_rejects_disallowed_extension() {
        let path = temp_file("txt");
        let error = validate_import_path(path.to_string_lossy().as_ref()).unwrap_err();
        let _ = std::fs::remove_file(&path);
        assert!(
            error.contains("仅支持导入"),
            "错误文案应说明扩展名白名单：{error}"
        );
    }

    #[test]
    fn validate_rejects_directory() {
        let dir = std::env::temp_dir();
        let error = validate_import_path(dir.to_string_lossy().as_ref()).unwrap_err();
        assert!(error.contains("文件夹"), "错误文案应说明不能导入文件夹：{error}");
    }

    #[test]
    fn validate_accepts_notepad_and_strips_prefix() {
        let notepad = notepad_path();
        let path = validate_import_path(&notepad).expect("本机应存在 notepad.exe");
        assert!(!path.starts_with(r"\\?\"), "应去掉 \\\\?\\ 前缀：{path}");
        assert!(
            path.to_lowercase().ends_with(r"\system32\notepad.exe"),
            "返回的应是规范化路径：{path}"
        );
    }

    #[test]
    fn make_entry_builds_import_entry_for_notepad() {
        let path = notepad_path();
        let entry = make_entry(&path, "", "").expect("notepad.exe 应能构造导入项");
        assert_eq!(entry.kind, "import");
        assert_eq!(entry.launch, path);
        assert!(!entry.name.trim().is_empty(), "显示名不应为空");
        assert!(entry.id.starts_with("import:"));
        assert!(entry.args.is_empty());
        // icon 允许为 None（无 Shell/COM 的极端环境），但正常桌面应能取到
        if let Some(icon) = &entry.icon {
            assert!(icon.starts_with("data:image/png;base64,"), "图标应是 PNG data URL");
        }
    }

    #[test]
    fn make_entry_prefers_custom_name_and_keeps_args() {
        let path = notepad_path();
        let entry =
            make_entry(&path, "我的记事本", "--new-window").expect("notepad.exe 应能构造导入项");
        assert_eq!(entry.name, "我的记事本");
        assert_eq!(entry.args, "--new-window");
    }

    #[test]
    fn make_entry_rejects_missing_file() {
        let error = make_entry(r"C:\__easy_dash_no_such_file__.exe", "", "").unwrap_err();
        assert!(error.contains("不存在"), "错误文案应说明不存在：{error}");
    }

    #[test]
    fn import_id_is_stable_for_same_file_via_different_spellings() {
        let path = notepad_path();
        let a = import_id(&path);
        let b = import_id(&path.replace('\\', "/").to_uppercase());
        assert_eq!(a, b, "大小写 / 斜杠写法不应改变 id");
    }
}

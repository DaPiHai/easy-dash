//! 经典桌面应用（Start Menu 快捷方式）枚举。
//!
//! 数据源是两个 Start Menu 的 Programs 目录（用户 + 公共）：
//! - `%APPDATA%\Microsoft\Windows\Start Menu\Programs`
//! - `%ProgramData%\Microsoft\Windows\Start Menu\Programs`
//!
//! 只收 `.lnk` / `.url`，跳过 `启动`/`Startup` 目录与隐藏项；图标先用 Shell
//! 图标工厂（`IShellItemImageFactory`）取 48px 透明 PNG，失败回退 32px 文件图标。
//!
//! 本函数在应用中心后台线程（MTA + RoInitialize）上调用；
//! 目录读取失败、图标提取失败都只跳过/降级，绝不 panic。
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::types::AppEntry;
use crate::winapi;

/// 递归深度上限（Start Menu 一般不超过 4 层；同时防御符号链接成环）
const MAX_DEPTH: usize = 6;
/// 应用中心图标请求尺寸（前端按 48px 展示）
const ICON_SIZE: u32 = 48;

/// 枚举 Start Menu（用户 + 公共）里的 .lnk/.url 快捷方式。
pub fn enumerate() -> Vec<AppEntry> {
    let mut entries = entries_from_candidates(scan_shortcuts());
    // 图标解析最慢（每个快捷方式一次 Shell 调用），放在去重之后，只对留下来的条目做
    for entry in &mut entries {
        entry.icon = winapi::icons::shell_item_icon_png(&entry.launch, ICON_SIZE)
            .or_else(|| winapi::icons::file_icon_data_url(&entry.launch));
    }
    entries
}

/// 收集两个 Start Menu 根目录下的所有快捷方式绝对路径。
/// 返回 (根目录序号, 路径)：根目录序号用于「跨根目录同名去重」。
fn scan_shortcuts() -> Vec<(usize, PathBuf)> {
    let mut out = Vec::new();
    for (root_index, root) in start_menu_roots().into_iter().enumerate() {
        collect_shortcuts(&root, root_index, 0, &mut out);
    }
    out
}

/// 两个 Start Menu 根目录（用户目录在前，同名项优先保留用户的）
fn start_menu_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(p) = std::env::var("APPDATA") {
        roots.push(Path::new(&p).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Ok(p) = std::env::var("ProgramData") {
        roots.push(Path::new(&p).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    roots
}

/// 递归收集快捷方式绝对路径（目录读不了就跳过，不报错）
fn collect_shortcuts(dir: &Path, root_index: usize, depth: usize, out: &mut Vec<(usize, PathBuf)>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // 隐藏项：以 '.' 开头（Start Menu 里很少见，跳过更干净）
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        // path.is_dir() 会跟随符号链接/junction（Start Menu 里可能有）
        if path.is_dir() {
            if is_skipped_dir(&name) {
                continue;
            }
            collect_shortcuts(&path, root_index, depth + 1, out);
        } else if is_shortcut(&name) {
            out.push((root_index, path));
        }
    }
}

// ==================== 纯函数（可单测，不依赖本机 Start Menu 内容） ====================

/// 候选（根序号 + 路径）→ AppEntry（不含图标）：计算显示名/id/launch 并去重。
///
/// 去重三条规则：
/// 1. id（路径小写、斜杠统一）—— 完全相同（仅大小写/斜杠不同）的快捷方式；
/// 2. 「同名 + 同 launch」；
/// 3. **跨根目录同名**：同一应用常在「用户 Start Menu」和「公共 Start Menu」
///    各放一份快捷方式，两者路径不同（id 也不同）但显示名相同，保留先扫到的
///    （用户目录优先）。同一根目录内的同名项不合并（可能是不同应用）。
fn entries_from_candidates(candidates: Vec<(usize, PathBuf)>) -> Vec<AppEntry> {
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut seen_name_launch: HashSet<String> = HashSet::new();
    let mut name_root: HashMap<String, usize> = HashMap::new();
    let mut out = Vec::with_capacity(candidates.len());
    for (root_index, path) in candidates {
        let Some(file_name) = path.file_name() else {
            continue;
        };
        let file_name = file_name.to_string_lossy();
        let name = display_name(&file_name);
        if name.is_empty() {
            continue;
        }
        let launch = path.to_string_lossy().to_string();
        if launch.trim().is_empty() {
            continue;
        }
        let id = entry_id(&launch);
        if !seen_ids.insert(id.clone()) {
            continue;
        }
        if !seen_name_launch.insert(dedupe_key(&name, &launch)) {
            continue;
        }
        let name_key = name.trim().to_lowercase();
        match name_root.get(&name_key) {
            Some(&prev_root) if prev_root != root_index => continue,
            Some(_) => {}
            None => {
                name_root.insert(name_key, root_index);
            }
        }
        out.push(AppEntry {
            id,
            name,
            icon: None,
            launch,
            // 内置 Start Menu 快捷方式没有额外启动参数
            args: String::new(),
            kind: "classic".to_string(),
        });
    }
    out
}

/// 文件名 → 显示名：去扩展名后 trim（"Visual Studio Code.lnk" → "Visual Studio Code"）
fn display_name(file_name: &str) -> String {
    Path::new(file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().trim().to_string())
        .unwrap_or_default()
}

/// 是否为快捷方式扩展名（`.lnk` / `.url`，大小写不敏感）
fn is_shortcut(file_name: &str) -> bool {
    matches!(
        Path::new(file_name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .as_deref(),
        Some("lnk") | Some("url")
    )
}

/// 需要整体跳过的目录名：`启动` / `Startup`（含其子项）
fn is_skipped_dir(name: &str) -> bool {
    let n = name.trim().to_lowercase();
    n == "启动" || n == "startup"
}

/// 稳定 id："lnk:" + 反斜杠统一后的小写绝对路径
fn entry_id(path: &str) -> String {
    format!("lnk:{}", path.trim().replace('/', "\\").to_lowercase())
}

/// 「同名 + 同启动目标」去重键（忽略大小写与斜杠方向）
fn dedupe_key(name: &str, path: &str) -> String {
    format!(
        "{}|{}",
        name.trim().to_lowercase(),
        path.trim().replace('/', "\\").to_lowercase()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    /// 单根目录场景（根序号统一为 0）
    fn entries(paths: Vec<PathBuf>) -> Vec<AppEntry> {
        entries_from_candidates(paths.into_iter().map(|p| (0, p)).collect())
    }

    #[test]
    fn display_name_strips_extension_and_trims() {
        assert_eq!(display_name("微信.lnk"), "微信");
        assert_eq!(display_name("Visual Studio Code.lnk"), "Visual Studio Code");
        assert_eq!(display_name("  Tool  .lnk"), "Tool");
        assert_eq!(display_name("a.URL"), "a");
        assert_eq!(display_name("带空格的 名字.lnk"), "带空格的 名字");
        assert_eq!(display_name(" .lnk"), "");
    }

    #[test]
    fn shortcut_extensions_are_case_insensitive() {
        assert!(is_shortcut("a.lnk"));
        assert!(is_shortcut("a.LNK"));
        assert!(is_shortcut("a.url"));
        assert!(is_shortcut("a.Url"));
        assert!(!is_shortcut("a.exe"));
        assert!(!is_shortcut("a.lnk.txt"));
        assert!(!is_shortcut("lnk"));
        assert!(!is_shortcut("a.URL.txt"));
    }

    #[test]
    fn startup_dirs_are_skipped() {
        assert!(is_skipped_dir("启动"));
        assert!(is_skipped_dir("Startup"));
        assert!(is_skipped_dir("STARTUP"));
        assert!(is_skipped_dir(" startup "));
        assert!(!is_skipped_dir("Programs"));
        assert!(!is_skipped_dir("启动器"));
        assert!(!is_skipped_dir(""));
    }

    #[test]
    fn entry_id_is_lowercase_with_backslash() {
        assert_eq!(entry_id(r"C:\Tools\App.lnk"), r"lnk:c:\tools\app.lnk");
        assert_eq!(entry_id("C:/Tools/App.lnk"), r"lnk:c:\tools\app.lnk");
        assert_eq!(entry_id(r"  C:\A.LNK  "), r"lnk:c:\a.lnk");
    }

    #[test]
    fn duplicate_paths_are_removed() {
        let entries = entries(vec![
            p(r"C:\Start\App.lnk"),
            p(r"C:\Start\APP.LNK"),
            p("C:/Start/App.lnk"),
        ]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, r"lnk:c:\start\app.lnk");
        assert_eq!(entries[0].name, "App");
        assert_eq!(entries[0].kind, "classic");
    }

    #[test]
    fn same_name_same_root_different_folder_is_kept() {
        let entries = entries(vec![
            p(r"C:\Start Menu\Programs\Tool\Tool.lnk"),
            p(r"C:\Start Menu\Programs\Utilities\Tool.lnk"),
        ]);
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.name == "Tool"));
    }

    #[test]
    fn same_name_across_roots_is_deduped() {
        // 真实场景：用户 Start Menu 与公共 Start Menu 各有一份同名快捷方式，
        // 路径不同（id 也不重复），只保留先扫到的用户目录那条
        let entries = entries_from_candidates(vec![
            (
                0,
                p(r"C:\Users\me\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\ZCode.lnk"),
            ),
            (
                1,
                p(r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\ZCode.lnk"),
            ),
        ]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "ZCode");
        assert!(entries[0].launch.contains(r"C:\Users\me\"));
    }

    #[test]
    fn cross_root_dedupe_does_not_merge_different_names() {
        let entries = entries_from_candidates(vec![
            (0, p(r"C:\Users\me\Start Menu\Programs\ZCode.lnk")),
            (1, p(r"C:\ProgramData\Start Menu\Programs\ZCode2.lnk")),
        ]);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn empty_display_name_is_skipped() {
        let entries = entries(vec![
            p(r"C:\Start\ .lnk"),
            p(r"C:\Start\Notepad.lnk"),
        ]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Notepad");
    }

    #[test]
    fn non_shortcut_paths_are_kept_by_mapper_but_scanner_filters() {
        // entries_from_candidates 只负责映射/去重；扩展名过滤在扫描阶段做，
        // 这里确认映射阶段不会误丢合法的中文名与深层路径
        let entries = entries(vec![p(r"D:\工具\电子书阅读器\小说下载器.lnk")]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "小说下载器");
        assert_eq!(entries[0].launch, r"D:\工具\电子书阅读器\小说下载器.lnk");
    }

    /// 本机冒烟：真实 Start Menu 枚举数量 + 前几条 + 图标形态。
    /// 手动执行：`cargo test --lib -- --ignored smoke_start_apps --nocapture`
    #[test]
    #[ignore]
    fn smoke_start_apps() {
        // 测试线程没有初始化 COM，Shell 图标工厂需要
        crate::winapi::icons::ensure_com();
        let entries = enumerate();
        eprintln!("[start-apps] 经典应用数量: {}", entries.len());
        assert!(
            entries.len() >= 20,
            "本机 Start Menu 应能枚举出至少 20 个快捷方式，实际 {}",
            entries.len()
        );
        let with_icon = entries.iter().filter(|e| e.icon.is_some()).count();
        let png = entries
            .iter()
            .filter(|e| {
                e.icon
                    .as_deref()
                    .map(|u| u.starts_with("data:image/png;base64,"))
                    .unwrap_or(false)
            })
            .count();
        eprintln!(
            "[start-apps] 有图标 {with_icon}/{}，其中 PNG {png}",
            entries.len()
        );
        for e in entries.iter().take(8) {
            eprintln!(
                "[start-apps] {} | kind={} | icon={} 字节 | {}",
                e.name,
                e.kind,
                e.icon.as_deref().map(|u| u.len()).unwrap_or(0),
                e.launch
            );
        }
        assert!(png > 0, "至少部分图标应来自 Shell 图标工厂（PNG data URL）");
        assert!(entries.iter().all(|e| e.id.starts_with("lnk:")));
        assert!(entries.iter().all(|e| e.kind == "classic"));
        assert!(entries.iter().all(|e| !e.name.trim().is_empty()));

        // 公共 Start Menu 应参与枚举（存在时至少贡献若干条）
        if let Ok(pd) = std::env::var("ProgramData") {
            let prefix = format!(r"{pd}\Microsoft\Windows\Start Menu\Programs").to_lowercase();
            let common = entries
                .iter()
                .filter(|e| e.launch.to_lowercase().starts_with(&prefix))
                .count();
            eprintln!("[start-apps] 来自公共 Start Menu 的条目: {common}");
        }

        // 图标质量抽样：解码前 12 个，统计尺寸（Shell 工厂应是 48px 正方形）
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let mut widths: Vec<u32> = Vec::new();
        for e in entries.iter().filter(|e| e.icon.is_some()).take(12) {
            let url = e.icon.as_deref().unwrap_or("");
            let b64 = url.split(',').nth(1).unwrap_or("");
            let Ok(bytes) = STANDARD.decode(b64) else {
                continue;
            };
            let Ok(img) = image::load_from_memory(&bytes) else {
                continue;
            };
            let img = img.to_rgba8();
            assert_eq!(img.width(), img.height(), "{} 的图标应为正方形", e.name);
            assert!(img.width() <= 48, "{} 的图标不应超过 48px", e.name);
            widths.push(img.width());
        }
        let n48 = widths.iter().filter(|w| **w == 48).count();
        eprintln!(
            "[start-apps] 抽样图标尺寸（宽）: {widths:?}，其中 48px {n48}/{}",
            widths.len()
        );
        assert!(n48 >= 8, "至少 8 个采样图标应为 48px");
    }
}

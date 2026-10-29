use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tauri::Manager;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinnedApp {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub args: String,
}

/// 应用中心里用户手动导入的应用（不写入 Windows 开始菜单）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedApp {
    /// 显示名；空则按文件版本信息/主名推导
    #[serde(default)]
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub args: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub pinned: Vec<PinnedApp>,
    pub hotkey: String,
    pub fullscreen_hide: bool,
    pub reserve_topbar_space: bool,
    /// 未固定应用的手动排序（窗口分组 key 列表）；不在列表中的按打开顺序排在后面
    pub dock_order: Vec<String>,
    /// 顶栏任务托盘图标镜像（仅 Win11 XAML 任务栏可用）
    pub tray_icons: bool,
    /// 顶栏时钟 → 通知中心
    pub notification_center: bool,
    /// 顶栏右侧 → 控制中心
    pub control_center: bool,
    /// 应用中心全局热键（默认 Alt+Space）
    pub app_center_hotkey: String,
    /// 顶栏托盘图标自定义顺序（TrayItem.orderKey 列表；不在列表中的追加在后）
    pub tray_order: Vec<String>,
    /// 应用中心自定义顺序（AppEntry.id 列表；不在列表中的按名称排序追加）
    pub app_order: Vec<String>,
    /// 应用中心手动导入的应用
    pub imported_apps: Vec<ImportedApp>,
    /// 隐藏 Windows 原生任务栏（托盘图标镜像到顶栏）
    pub hide_taskbar: bool,
    /// 在多显示器上每块屏都显示顶栏（主屏始终显示）
    pub topbar_all_monitors: bool,
    /// 在多显示器上每块屏都显示 Dock（主屏始终显示）
    pub dock_all_monitors: bool,
    /// Dock 智能隐藏：普通窗口压到 Dock 区域时自动滑下隐藏，
    /// 鼠标移到 Dock 底边时临时滑出（默认开启）
    pub dock_auto_hide: bool,
    /// 主题模式："light" | "dark"（v0.2.2 起不再支持「跟随系统」）
    pub theme: String,
    /// 选择浅色/深色时是否同步修改 Windows 系统主题
    pub sync_system_theme: bool,
    /// 鼠标流光（光标跟随的柔光高光）是否开启
    pub pointer_glow: bool,
    /// 界面材质："solid" | "liquid" | "glow"（与 light/dark 正交的观感维度）
    pub material: String,
}

/// 合法的主题模式白名单（「跟随系统」已移除，只有显式浅色/深色）
pub const THEME_MODES: [&str; 2] = ["light", "dark"];

/// 主题模式归一化：去空白 + 转小写；"system" 与任何非法值一律归一为 "dark"
/// （旧配置里的 "system" 在加载时即迁移为深色，避免出现前端不认识的模式）
pub fn normalize_theme(mode: &str) -> String {
    let normalized = mode.trim().to_ascii_lowercase();
    if THEME_MODES.contains(&normalized.as_str()) {
        normalized
    } else {
        "dark".to_string()
    }
}

/// 合法的界面材质白名单（solid = 当前标准观感，liquid = 液态玻璃，glow = 柔光玻璃）
pub const MATERIAL_STYLES: [&str; 3] = ["solid", "liquid", "glow"];

/// 材质归一化：去空白 + 转小写；任何非法值一律归一为 "solid"
pub fn normalize_material(style: &str) -> String {
    let normalized = style.trim().to_ascii_lowercase();
    if MATERIAL_STYLES.contains(&normalized.as_str()) {
        normalized
    } else {
        "solid".to_string()
    }
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            pinned: vec![
                PinnedApp {
                    name: "文件资源管理器".into(),
                    path: r"C:\Windows\explorer.exe".into(),
                    args: String::new(),
                },
                PinnedApp {
                    name: "Microsoft Edge".into(),
                    path: r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe".into(),
                    args: String::new(),
                },
                PinnedApp {
                    name: "记事本".into(),
                    path: r"C:\Windows\System32\notepad.exe".into(),
                    args: String::new(),
                },
                PinnedApp {
                    name: "PowerShell".into(),
                    path: r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe".into(),
                    args: String::new(),
                },
            ],
            hotkey: "alt+f1".into(),
            fullscreen_hide: true,
            reserve_topbar_space: true,
            dock_order: Vec::new(),
            tray_icons: default_true(),
            notification_center: default_true(),
            control_center: default_true(),
            app_center_hotkey: "alt+space".into(),
            tray_order: Vec::new(),
            app_order: Vec::new(),
            imported_apps: Vec::new(),
            hide_taskbar: default_true(),
            topbar_all_monitors: default_true(),
            dock_all_monitors: false,
            dock_auto_hide: default_true(),
            theme: "dark".into(),
            sync_system_theme: default_true(),
            pointer_glow: default_true(),
            material: "solid".into(),
        }
    }
}

impl Config {
    /// 修正非法字段（主题白名单 + 界面材质白名单）。加载与保存前调用，保证
    /// 内存/磁盘上的值始终是前端认识的枚举值。pointer_glow 等 bool 外观开关
    /// 没有白名单，无需归一。
    pub fn normalize(&mut self) {
        self.theme = normalize_theme(&self.theme);
        self.material = normalize_material(&self.material);
    }
}

pub fn config_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_config_dir().ok()?;
    Some(dir.join("config.json"))
}

/// 读取配置；不存在时写入默认配置
pub fn load_or_init(app: &tauri::AppHandle) -> Result<Config, String> {
    let path = config_path(app).ok_or("无法确定配置目录")?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if path.exists() {
        let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        // 宽松解析：失败时回退默认配置
        match serde_json::from_str::<Config>(&text) {
            Ok(mut cfg) => {
                // 主题/材质白名单迁移：旧配置的 "system" / 非法值归一为 "dark"、"solid"
                cfg.normalize();
                Ok(cfg)
            }
            Err(e) => {
                eprintln!("[easy-dash] 配置解析失败，使用默认配置: {e}");
                let cfg = Config::default();
                let _ = fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap());
                Ok(cfg)
            }
        }
    } else {
        let cfg = Config::default();
        fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap())
            .map_err(|e| e.to_string())?;
        Ok(cfg)
    }
}

pub fn save(app: &tauri::AppHandle, cfg: &Config) -> Result<(), String> {
    let path = config_path(app).ok_or("无法确定配置目录")?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    // 写盘前归一化，避免把非法主题模式落到配置里
    let mut cfg = cfg.clone();
    cfg.normalize();
    fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_taskbar_and_theme_fields_have_documented_defaults() {
        let cfg = Config::default();
        assert!(cfg.hide_taskbar, "hideTaskbar 默认应为 true");
        assert!(cfg.topbar_all_monitors, "副屏顶栏默认开启");
        assert!(!cfg.dock_all_monitors, "副屏 Dock 默认关闭（Dock 只在主屏）");
        assert!(cfg.dock_auto_hide, "dockAutoHide 默认应为 true");
        assert_eq!(cfg.theme, "dark", "theme 默认应为 dark（跟随系统已移除）");
        assert!(cfg.sync_system_theme, "syncSystemTheme 默认应为 true");
        assert!(cfg.pointer_glow, "pointerGlow 默认应为 true");
        assert_eq!(cfg.material, "solid", "material 默认应为 solid");
    }

    #[test]
    fn old_config_without_new_fields_deserializes_with_defaults() {
        // 旧版本配置文件：没有 hideTaskbar / theme / syncSystemTheme
        let text = r#"{"hotkey":"ctrl+alt+f9","fullscreenHide":false,"trayIcons":false}"#;
        let cfg: Config = serde_json::from_str(text).expect("旧配置应能反序列化");
        assert_eq!(cfg.hotkey, "ctrl+alt+f9");
        assert!(!cfg.fullscreen_hide);
        assert!(!cfg.tray_icons);
        // 新字段回落到默认值
        assert!(cfg.hide_taskbar);
        assert!(cfg.topbar_all_monitors);
        assert!(!cfg.dock_all_monitors);
        assert!(cfg.dock_auto_hide, "旧配置缺 dockAutoHide 时应回落到 true");
        assert_eq!(cfg.theme, "dark");
        assert!(cfg.sync_system_theme);
        assert!(cfg.pointer_glow, "旧配置缺 pointerGlow 时应回落到 true");
        assert_eq!(cfg.material, "solid", "旧配置缺 material 时应回落到 solid");
    }

    #[test]
    fn legacy_system_theme_is_migrated_to_dark() {
        // v0.2.0/v0.2.1 的配置可能是 "system"：加载时迁移为深色
        let text = r#"{"theme":"system"}"#;
        let mut cfg: Config = serde_json::from_str(text).expect("配置应能反序列化");
        assert_eq!(cfg.theme, "system", "反序列化阶段先保留原值");
        cfg.normalize();
        assert_eq!(cfg.theme, "dark", "load_or_init 的归一化应把 system 迁移为 dark");
    }

    #[test]
    fn theme_whitelist_normalizes_invalid_values() {
        assert_eq!(normalize_theme("light"), "light");
        assert_eq!(normalize_theme("dark"), "dark");
        assert_eq!(normalize_theme("  DARK  "), "dark");
        assert_eq!(normalize_theme("Light"), "light");
        // 「跟随系统」已移除：旧值与任何非法/空值一律归一为深色
        assert_eq!(normalize_theme("system"), "dark");
        assert_eq!(normalize_theme("SYSTEM"), "dark");
        assert_eq!(normalize_theme(""), "dark");
        assert_eq!(normalize_theme("blue"), "dark");
        assert_eq!(normalize_theme("null"), "dark");
    }

    #[test]
    fn config_normalize_fixes_invalid_theme_from_disk() {
        let mut cfg = Config {
            theme: "purple".into(),
            ..Default::default()
        };
        cfg.normalize();
        assert_eq!(cfg.theme, "dark");
    }

    #[test]
    fn material_default_is_solid() {
        let cfg = Config::default();
        assert_eq!(cfg.material, "solid", "material 默认值应为 solid");
        assert_eq!(MATERIAL_STYLES.len(), 3, "材质白名单应只有 3 个合法值");
    }

    #[test]
    fn material_whitelist_normalizes_invalid_values() {
        assert_eq!(normalize_material("solid"), "solid");
        assert_eq!(normalize_material("liquid"), "liquid");
        assert_eq!(normalize_material("glow"), "glow");
        assert_eq!(normalize_material("lIqUid"), "liquid", "大小写应归一");
        assert_eq!(normalize_material("  GLOW  "), "glow", "空白应被去掉");
        // 任何非法/空值一律归一为 "solid"
        assert_eq!(normalize_material(""), "solid", "空串应归一为 solid");
        assert_eq!(normalize_material("glass"), "solid", "未知材质应归一为 solid");
        assert_eq!(normalize_material("null"), "solid", "字符串 null 应归一为 solid");
    }

    #[test]
    fn old_config_without_material_deserializes_to_solid() {
        // 旧版本配置文件：没有 material 字段
        let text = r#"{"theme":"light","syncSystemTheme":false}"#;
        let mut cfg: Config = serde_json::from_str(text).expect("旧配置应能反序列化");
        assert_eq!(cfg.material, "solid", "缺 material 时应回落到 solid");
        cfg.normalize();
        assert_eq!(cfg.material, "solid", "归一化后仍应是 solid");
        assert_eq!(cfg.theme, "light");
    }

    #[test]
    fn config_normalize_fixes_invalid_material_from_disk() {
        let mut cfg = Config {
            material: "glass".into(),
            ..Default::default()
        };
        cfg.normalize();
        assert_eq!(cfg.material, "solid", "磁盘上的非法材质应被归一为 solid");
    }

    #[test]
    fn config_material_round_trips_with_camel_case() {
        let cfg = Config {
            material: "glow".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).expect("序列化失败");
        assert!(
            json.contains(r#""material":"glow""#),
            "缺 material（camelCase）: {json}"
        );
        let back: Config = serde_json::from_str(&json).expect("反序列化失败");
        assert_eq!(back.material, "glow", "material 应能往返");
    }

    #[test]
    fn config_pointer_glow_round_trips_with_camel_case() {
        let cfg = Config {
            pointer_glow: false,
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).expect("序列化失败");
        assert!(
            json.contains(r#""pointerGlow":false"#),
            "缺 pointerGlow（camelCase）: {json}"
        );
        let back: Config = serde_json::from_str(&json).expect("反序列化失败");
        assert!(!back.pointer_glow, "pointerGlow 应能往返");
    }

    #[test]
    fn config_dock_auto_hide_round_trips_with_camel_case() {
        let cfg = Config {
            dock_auto_hide: false,
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).expect("序列化失败");
        assert!(
            json.contains(r#""dockAutoHide":false"#),
            "缺 dockAutoHide（camelCase）: {json}"
        );
        let back: Config = serde_json::from_str(&json).expect("反序列化失败");
        assert!(!back.dock_auto_hide, "dockAutoHide 应能往返");
    }

    #[test]
    fn config_new_fields_round_trip_with_camel_case_names() {
        let cfg = Config {
            hide_taskbar: false,
            theme: "dark".into(),
            sync_system_theme: false,
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).expect("序列化失败");
        assert!(json.contains("\"hideTaskbar\":false"), "缺 hideTaskbar: {json}");
        assert!(json.contains("\"theme\":\"dark\""), "缺 theme: {json}");
        assert!(
            json.contains("\"syncSystemTheme\":false"),
            "缺 syncSystemTheme: {json}"
        );
        assert!(
            json.contains("\"topbarAllMonitors\":true"),
            "缺 topbarAllMonitors: {json}"
        );
        assert!(
            json.contains("\"dockAllMonitors\":false"),
            "缺 dockAllMonitors: {json}"
        );
        let back: Config = serde_json::from_str(&json).expect("反序列化失败");
        assert!(!back.hide_taskbar);
        assert_eq!(back.theme, "dark");
        assert!(!back.sync_system_theme);
    }
}

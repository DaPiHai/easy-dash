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
        }
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
            Ok(cfg) => Ok(cfg),
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
    fs::write(&path, serde_json::to_string_pretty(cfg).unwrap()).map_err(|e| e.to_string())
}

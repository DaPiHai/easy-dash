//! 应用图标解析（托盘镜像 / 通知中心共用）。
//!
//! 托盘真实透明图标不来自屏幕截图，而来自系统「通知区域图标」设置：
//! `HKCU\Control Panel\NotifyIconSettings\<hash>` 每条含
//! - `InitialTooltip`：用户看到的提示文本（可能为空 / 带换行）
//! - `ExecutablePath`：可执行文件路径（可能是 KNOWNFOLDERID 前缀的形式）
//! - `IconSnapshot`：REG_BINARY，16×16 或 32×32 带 alpha 的 PNG（真实透明图标）
//! - `IsPromoted`：是否常驻托盘
//!
//! UIA 给出的托盘工具提示与上述字段并不一致（如「小米互联服务」vs
//! `MiPcContinuity.exe`），因此这里做：归一化（小写、去空白与标点）+
//! 中文别名扩展 + 打分匹配 tooltip / exe 主名 / FileDescription。
//!
//! 所有注册表与版本信息读取都容错：失败只返回 None，绝不 panic。
//! 托盘工作线程是 MTA，因此不使用 STA-only 的 Shell 扩展调用
//! （ExtractIconExW / SHGetFileInfo 在 MTA 下可用，且失败只降级为无图标）。
#![allow(dead_code)]
#![allow(non_upper_case_globals)]

use std::sync::OnceLock;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};

use super::common::to_wide;

/// 解析出的应用身份（显示名 + 图标）
#[derive(Debug, Clone)]
pub struct AppIdentity {
    pub name: String,
    pub icon: Option<String>,
}

/// 匹配得分低于该值视为没匹配上（宁可无图标，也不给错图标）
const MIN_SCORE: i32 = 55;

/// 图标最长边上限：注册表里个别 IconSnapshot 是 256px 大图，缩小以免 IPC 负载过大
const ICON_MAX_EDGE: u32 = 48;

/// 一条「通知区域图标」注册表记录。
///
/// 同时保留两套字段：
/// - 归一化字段（`tooltip` / `tooltip_first` / `exe_stem` / `description`）只用于打分；
/// - `*_raw` 是注册表/版本信息里的**原始文本**，只用于展示。
/// 二者不能混用：把归一化键当显示名会输出 "qq"/"zcode" 这类小写拼接串。
#[derive(Debug, Clone)]
struct NotifyEntry {
    /// 完整提示文本（归一化，仅打分用）
    tooltip: String,
    /// 提示文本首行（归一化，仅打分用；有的提示带「版本号: …」多行）
    tooltip_first: String,
    /// 展开 KNOWNFOLDERID 后的可执行文件路径
    exe_path: Option<String>,
    /// exe 主名（归一化，如 "weixin" / "douyintray"，仅打分用）
    exe_stem: String,
    /// 文件版本信息里的 FileDescription（归一化，仅打分用）
    description: String,
    /// 提示文本首行原文（展示用，如 "野狐围棋"）
    tooltip_first_raw: String,
    /// FileDescription 原文（展示用，如 "ZCode" / "QQ" / "Docker Desktop"）
    description_raw: String,
    /// exe 主名原文（展示用，如 "douyin_tray"）
    stem_raw: String,
    /// 真实透明图标（PNG data URL）
    icon: Option<String>,
    /// 是否常驻托盘（用于同分时微弱加权）
    promoted: bool,
}

/// 按托盘提示文本解析真实透明图标（PNG data URL）。
///
/// 例：" QQ: 3384865082 声音: 开启" → QQ 的 IconSnapshot；
/// "Docker Desktop is running in Resource Saver mode" → Docker Desktop 的图标。
pub fn resolve_tray_icon(name: &str) -> Option<String> {
    let entries = entries();
    let entry = best_match(name, entries)?;
    // IconSnapshot 缺失时退回 exe 文件图标（同样带 alpha）
    entry
        .icon
        .clone()
        .or_else(|| entry.exe_path.as_deref().and_then(exe_icon))
}

/// 按通知处理器 PrimaryId 解析显示名与图标。
///
/// `primary_id` 可能是 AUMID（`包家族名!AppId`）、exe 绝对路径，或只有
/// 可执行文件名（如 "QQ"/"Weixin"）——三种形态分别处理。
pub fn resolve_identity(primary_id: &str) -> Option<AppIdentity> {
    let id = primary_id.trim();
    if id.is_empty() {
        return None;
    }

    // 1) 打包应用 AUMID → 包清单里的真实图标 + 本地化显示名
    if id.contains('!') {
        if let Some((icon, name)) = super::icons::aumid_app_info(id) {
            return Some(AppIdentity {
                name: if name.trim().is_empty() {
                    aumid_display_name(id)
                } else {
                    name
                },
                icon: Some(icon),
            });
        }
        return None;
    }

    // 2) 文件路径 → FileDescription 作为显示名
    let looks_like_path =
        id.contains('\\') || id.contains('/') || id.to_lowercase().ends_with(".exe");
    if looks_like_path {
        let path = expand_known_folder(id).unwrap_or_else(|| id.to_string());
        let name = file_description(&path)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                let stem = stem_of(&path);
                if stem.is_empty() {
                    id.to_string()
                } else {
                    stem
                }
            });
        return Some(AppIdentity {
            name,
            icon: exe_icon(&path),
        });
    }

    // 3) 只有名字（处理器名 / exe 主名）：先按注册表条目匹配，再退回原名。
    //    展示名必须用原始文本（FileDescription → tooltip 首行 → exe 主名 →
    //    传入 id），归一化键只用于打分，否则 "QQ" 会显示成 "qq"
    let entries = entries();
    if let Some(entry) = best_match(id, entries) {
        let icon = entry
            .icon
            .clone()
            .or_else(|| entry.exe_path.as_deref().and_then(exe_icon));
        let name = display_name_from(entry).unwrap_or_else(|| id.to_string());
        return Some(AppIdentity { name, icon });
    }
    Some(AppIdentity {
        name: id.to_string(),
        icon: None,
    })
}

/// 命中注册表条目后的展示名优先级：
/// 原始 FileDescription（"ZCode"/"QQ"/"Docker Desktop"）→
/// 原始提示文本首行（"野狐围棋"）→ exe 主名原文（"douyin_tray"）。
/// 返回 None 表示三者都为空，由调用方回退到 primary_id 原样。
fn display_name_from(entry: &NotifyEntry) -> Option<String> {
    for s in [
        entry.description_raw.as_str(),
        entry.tooltip_first_raw.as_str(),
        entry.stem_raw.as_str(),
    ] {
        let s = s.trim();
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    None
}

/// 按 exe 路径取文件图标（带 alpha 的 PNG data URL）。
pub fn exe_icon(path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    let wpath = to_wide(path);
    unsafe {
        let mut large = HICON::default();
        let count = ExtractIconExW(PCWSTR(wpath.as_ptr()), 0, Some(&mut large), None, 1);
        if count > 0 && !large.is_invalid() {
            let url = super::icons::icon_data_url(large);
            let _ = DestroyIcon(large);
            if url.is_some() {
                return url;
            }
        }
    }
    // 回退：Shell 文件图标（SHGetFileInfo）。MTA 下偶发失败，只降级为 None
    super::icons::file_icon_data_url(path)
}

// ==================== 注册表条目加载（OnceLock 缓存） ====================

/// 通知区域图标记录（整个进程只读一次；用户在系统设置里改图标需重启才生效）
fn entries() -> &'static Vec<NotifyEntry> {
    static CACHE: OnceLock<Vec<NotifyEntry>> = OnceLock::new();
    CACHE.get_or_init(load_entries)
}

fn load_entries() -> Vec<NotifyEntry> {
    let mut out: Vec<NotifyEntry> = Vec::new();
    let parent = r"Control Panel\NotifyIconSettings";
    let wparent = to_wide(parent);
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(wparent.as_ptr()),
            0,
            KEY_READ,
            &mut hkey,
        ) != ERROR_SUCCESS
        {
            eprintln!("[app-icons] 打不开 NotifyIconSettings（无法取托盘真实图标）");
            return out;
        }
        let mut index = 0u32;
        loop {
            let mut name_buf = [0u16; 256];
            let mut name_len = name_buf.len() as u32;
            let err = RegEnumKeyExW(
                hkey,
                index,
                windows::core::PWSTR(name_buf.as_mut_ptr()),
                &mut name_len,
                None,
                windows::core::PWSTR::null(),
                None,
                None,
            );
            if err != ERROR_SUCCESS {
                break;
            }
            index += 1;
            let sub = format!(
                "{parent}\\{}",
                String::from_utf16_lossy(&name_buf[..name_len as usize])
            );
            if let Some(entry) = read_entry(&sub) {
                out.push(entry);
            }
        }
        let _ = RegCloseKey(hkey);
    }
    out
}

fn read_entry(sub: &str) -> Option<NotifyEntry> {
    let raw_tooltip = reg_value_string(sub, "InitialTooltip").unwrap_or_default();
    let raw_path = reg_value_string(sub, "ExecutablePath");
    let promoted = reg_value_dword(sub, "IsPromoted")
        .or_else(|| reg_value_dword(sub, "isPromoted"))
        .unwrap_or(0)
        != 0;
    let icon = reg_value_bytes(sub, "IconSnapshot").and_then(|b| snapshot_icon(&b));

    let exe_path = raw_path.as_deref().and_then(expand_known_folder);
    // 只有路径真实存在时才读版本信息（很多条目是已卸载应用的残留）
    let exists = exe_path
        .as_deref()
        .map(|p| std::path::Path::new(p).is_file())
        .unwrap_or(false);
    let stem_raw = exe_path.as_deref().map(stem_of).unwrap_or_default();
    let raw_description = if exists {
        exe_path
            .as_deref()
            .and_then(file_description)
            .unwrap_or_default()
    } else {
        String::new()
    };
    let exe_stem = normalize(&stem_raw);
    let description = normalize(&raw_description);

    let tooltip = normalize(&raw_tooltip);
    let tooltip_first_raw = raw_tooltip
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    let tooltip_first = normalize(&tooltip_first_raw);
    if tooltip.is_empty() && exe_stem.is_empty() && icon.is_none() {
        return None;
    }
    Some(NotifyEntry {
        tooltip,
        tooltip_first,
        exe_path,
        exe_stem,
        description,
        tooltip_first_raw,
        description_raw: raw_description,
        stem_raw,
        icon,
        promoted,
    })
}

fn reg_value_bytes(subkey: &str, value: &str) -> Option<Vec<u8>> {
    let sub = to_wide(subkey);
    let val = to_wide(value);
    unsafe {
        let mut size = 0u32;
        let r = RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            PCWSTR(val.as_ptr()),
            RRF_RT_REG_BINARY,
            None,
            None,
            Some(&mut size),
        );
        if r != ERROR_SUCCESS || size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let r = RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            PCWSTR(val.as_ptr()),
            RRF_RT_REG_BINARY,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        );
        if r != ERROR_SUCCESS || size == 0 {
            return None;
        }
        buf.truncate(size.min(buf.len() as u32) as usize);
        Some(buf)
    }
}

fn reg_value_string(subkey: &str, value: &str) -> Option<String> {
    let sub = to_wide(subkey);
    let val = to_wide(value);
    unsafe {
        let mut size = 0u32;
        // REG_EXPAND_SZ 与 REG_SZ 都接受：路径里偶尔会带环境变量
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ;
        let r = RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            PCWSTR(val.as_ptr()),
            flags,
            None,
            None,
            Some(&mut size),
        );
        if r != ERROR_SUCCESS || size < 2 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let r = RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            PCWSTR(val.as_ptr()),
            flags,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        );
        if r != ERROR_SUCCESS || size < 2 {
            return None;
        }
        let words: Vec<u16> = buf
            .chunks_exact(2)
            .take(size as usize / 2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let s = String::from_utf16_lossy(&words);
        let s = s.trim_end_matches('\0').trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

fn reg_value_dword(subkey: &str, value: &str) -> Option<u32> {
    let sub = to_wide(subkey);
    let val = to_wide(value);
    unsafe {
        let mut d: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let r = RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            PCWSTR(val.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut d as *mut u32 as *mut _),
            Some(&mut size),
        );
        if r == ERROR_SUCCESS {
            Some(d)
        } else {
            None
        }
    }
}

// ==================== 路径 / 图标 / 版本信息 ====================

/// 展开 KNOWNFOLDERID 前缀的路径：
/// `{6D809377-6AF0-444B-8957-A3773F02200E}\Tencent\QQNT\QQ.exe`
fn expand_known_folder(raw: &str) -> Option<String> {
    let s = raw.trim();
    if let Some(rest) = s.strip_prefix('{') {
        let close = rest.find('}')?;
        let guid = rest[..close].to_uppercase();
        let tail = &rest[close + 1..];
        let base = match guid.as_str() {
            // %ProgramFiles%
            "6D809377-6AF0-444B-8957-A3773F02200E" => {
                std::env::var("ProgramFiles").ok()?
            }
            // %ProgramFiles(x86)%
            "7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E" => std::env::var("ProgramFiles(x86)")
                .or_else(|_| std::env::var("ProgramFiles"))
                .ok()?,
            // %SystemRoot%
            "F38BF404-1D43-42F2-9305-67DE0B28FC23" => std::env::var("SystemRoot").ok()?,
            // %SystemRoot%\System32
            "1AC14E77-02E7-4E5D-B744-2EB1AE5198B7" => {
                format!("{}\\System32", std::env::var("SystemRoot").ok()?)
            }
            _ => return None,
        };
        let tail = tail.trim_start_matches(['\\', '/']);
        if tail.is_empty() {
            return Some(base);
        }
        return Some(format!("{}\\{}", base.trim_end_matches('\\'), tail));
    }
    Some(s.to_string())
}

fn stem_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// 读 exe 版本信息里的 FileDescription（"QQ" / "Docker Desktop" / "Weixin" …）
fn file_description(path: &str) -> Option<String> {
    version_string(path, "FileDescription").or_else(|| version_string(path, "ProductName"))
}

fn version_string(path: &str, key: &str) -> Option<String> {
    let wpath = to_wide(path);
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(wpath.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut block = vec![0u8; size as usize];
        if GetFileVersionInfoW(
            PCWSTR(wpath.as_ptr()),
            0,
            size,
            block.as_mut_ptr() as *mut _,
        )
        .is_err()
        {
            return None;
        }

        // 先取翻译表里的第一条 (language, codepage)
        let mut candidates: Vec<(u16, u16)> = Vec::new();
        let sub = to_wide(r"\VarFileInfo\Translation");
        let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut len: u32 = 0;
        if VerQueryValueW(
            block.as_ptr() as *const _,
            PCWSTR(sub.as_ptr()),
            &mut ptr,
            &mut len,
        )
        .as_bool()
            && len >= 4
            && !ptr.is_null()
        {
            let p = ptr as *const u16;
            let pairs = (len as usize / 4).min(4);
            for i in 0..pairs {
                let lang = *p.add(i * 2);
                let cp = *p.add(i * 2 + 1);
                if lang != 0 {
                    candidates.push((lang, cp));
                }
            }
        }
        // 兜底：en-US Unicode、zh-CN Unicode、en-US Latin1
        candidates.push((0x0409, 0x04B0));
        candidates.push((0x0804, 0x04B0));
        candidates.push((0x0409, 0x04E4));

        for (lang, cp) in candidates {
            let sub = to_wide(&format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\{key}"));
            let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
            let mut len: u32 = 0;
            if VerQueryValueW(
                block.as_ptr() as *const _,
                PCWSTR(sub.as_ptr()),
                &mut ptr,
                &mut len,
            )
            .as_bool()
                && len > 1
                && !ptr.is_null()
            {
                // 注意：字符串值的 pulLen 是「字符数」（含结尾 NUL），不是字节数。
                // 曾经误按字节数取 len/2 个 u16，导致 "QQ" 被截成 "Q"、"ZCode"
                // 被截成 "ZCo"。这里再按版本块大小夹一下，防御异常 provider。
                let chars = (len as usize).min(block.len() / 2);
                let mut s =
                    String::from_utf16_lossy(std::slice::from_raw_parts(ptr as *const u16, chars));
                s = s.trim_end_matches('\0').trim().to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
        None
    }
}

/// 读 PNG 尺寸（IHDR）；非 PNG 返回 None
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.len() < 24 || bytes[..8] != SIG {
        return None;
    }
    let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h))
}

/// IconSnapshot（PNG 字节）→ PNG data URL。小图直接内嵌，大图缩到 ICON_MAX_EDGE
fn snapshot_icon(bytes: &[u8]) -> Option<String> {
    if let Some((w, h)) = png_size(bytes) {
        if w <= 64 && h <= 64 {
            return Some(format!("data:image/png;base64,{}", STANDARD.encode(bytes)));
        }
        let img = image::load_from_memory(bytes).ok()?;
        return resized_png(img);
    }
    let img = image::load_from_memory(bytes).ok()?;
    resized_png(img)
}

fn resized_png(img: image::DynamicImage) -> Option<String> {
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return None;
    }
    let img = if w > ICON_MAX_EDGE || h > ICON_MAX_EDGE {
        let scale = ICON_MAX_EDGE as f32 / w.max(h) as f32;
        let nw = ((w as f32 * scale).round() as u32).max(1);
        let nh = ((h as f32 * scale).round() as u32).max(1);
        img.resize(nw, nh, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(img.to_rgba8())
        .write_to(
            &mut std::io::Cursor::new(&mut png),
            image::ImageFormat::Png,
        )
        .ok()?;
    Some(format!("data:image/png;base64,{}", STANDARD.encode(png)))
}

// ==================== 归一化 / 别名 / 打分（纯函数，有单测） ====================

/// 归一化：只保留字母、数字（含 CJK 汉字），全部小写。
/// 这样 " QQ: 3384865082 " → "qq3384865082"，"野狐围棋: V533343630" → "野狐围棋v533343630"
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_alphanumeric() {
            if let Some(low) = c.to_lowercase().next() {
                out.push(low);
            }
        }
    }
    out
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// 中文名 → 英文 exe 名/描述里的关键词。归一化后按「包含」触发，
/// 用于 `InitialTooltip` 为空的条目（实测「微信」「抖音」等都可能为空）。
const ALIASES: &[(&str, &[&str])] = &[
    ("微信", &["weixin", "wechat"]),
    ("企业微信", &["wxwork", "wework", "wecom"]),
    ("微信输入法", &["wechatinput", "weixininput"]),
    ("QQ", &["qq", "qqnt", "tencentqq"]),
    ("QQ音乐", &["qqmusic"]),
    ("抖音", &["douyin", "douyintray"]),
    ("抖音直播伴侣", &["douyinlive"]),
    ("小米互联服务", &["mipccontinuity", "xiaomi", "micontinuity"]),
    ("野狐围棋", &["foxwq", "fox"]),
    ("悟空", &["wukong", "safemon"]),
    ("必应壁纸", &["bingwallpaper", "bing"]),
    ("任务管理器", &["taskmgr", "taskmanager"]),
    ("Realtek高清晰音频管理器", &["ravcpl", "realtek"]),
    ("网易云音乐", &["cloudmusic", "netease"]),
    ("钉钉", &["dingtalk"]),
    ("百度网盘", &["baidunetdisk", "netdisk"]),
    ("哔哩哔哩", &["bilibili"]),
    ("迅雷", &["thunder"]),
    ("阿里旺旺", &["aliww", "wangwang"]),
    ("输入法", &["inputmethod", "ime"]),
];

/// 查询串里出现的中文别名 → 追加英文关键词（用于与 exe 名/描述比对）
fn alias_tokens(q: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (alias, tokens) in ALIASES {
        let a = normalize(alias);
        if a.is_empty() || !q.contains(&a) {
            continue;
        }
        for t in *tokens {
            if !out.iter().any(|x| x == t) {
                out.push((*t).to_string());
            }
        }
    }
    out
}

/// 单字段打分：完全相等 > 前缀 > 包含
fn score_field(q: &str, field: &str) -> i32 {
    let (ql, fl) = (char_len(q), char_len(field));
    if fl == 0 || ql == 0 {
        return 0;
    }
    if q == field {
        return 100;
    }
    if fl >= 2 && q.starts_with(field) {
        return 85;
    }
    if fl >= 3 && q.contains(field) {
        return 75;
    }
    if ql >= 2 && field.starts_with(q) {
        return 70;
    }
    if ql >= 3 && field.contains(q) {
        return 65;
    }
    0
}

/// 别名关键词与单字段的打分（比直接字段匹配略低一档）
fn score_token(token: &str, field: &str) -> i32 {
    let (tl, fl) = (char_len(token), char_len(field));
    if tl == 0 || fl == 0 {
        return 0;
    }
    if field == token {
        return 70;
    }
    if tl >= 3 && field.contains(token) {
        return 55;
    }
    if fl >= 3 && token.contains(field) {
        return 50;
    }
    0
}

fn score_entry(q: &str, tokens: &[String], e: &NotifyEntry) -> i32 {
    let fields: [&str; 4] = [&e.tooltip, &e.tooltip_first, &e.exe_stem, &e.description];
    let mut best = 0i32;
    for f in fields {
        best = best.max(score_field(q, f));
    }
    if best < 100 {
        for t in tokens {
            for f in fields {
                best = best.max(score_token(t, f));
            }
        }
    }
    // 常驻托盘的条目更可能是当前 UIA 里看到的那个（同分时的微弱加权）
    if best >= MIN_SCORE && e.promoted {
        best += 2;
    }
    best
}

/// 在条目集合里找最佳匹配（供 resolve_tray_icon / resolve_identity / 单测共用）
fn best_match<'a>(name: &str, entries: &'a [NotifyEntry]) -> Option<&'a NotifyEntry> {
    let q = normalize(name);
    if char_len(&q) < 2 {
        return None;
    }
    let tokens = alias_tokens(&q);
    let mut best: Option<(i32, &NotifyEntry)> = None;
    for e in entries {
        let s = score_entry(&q, &tokens, e);
        if s < MIN_SCORE {
            continue;
        }
        if best.map(|(bs, _)| s > bs).unwrap_or(true) {
            best = Some((s, e));
        }
    }
    best.map(|(_, e)| e)
}

/// AUMID 的可读兜底名（取包家族名最后一个下划线前的主体，去掉版本/架构噪音）
fn aumid_display_name(aumid: &str) -> String {
    let family = aumid.split('!').next().unwrap_or(aumid);
    let base = family.rsplit_once('_').map(|(n, _)| n).unwrap_or(family);
    base.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tooltip: &str, stem: &str, desc: &str) -> NotifyEntry {
        NotifyEntry {
            tooltip: normalize(tooltip),
            tooltip_first: normalize(tooltip.lines().next().unwrap_or("")),
            exe_path: None,
            exe_stem: normalize(stem),
            description: normalize(desc),
            tooltip_first_raw: tooltip.lines().next().unwrap_or("").trim().to_string(),
            description_raw: desc.to_string(),
            stem_raw: stem.to_string(),
            icon: None,
            promoted: false,
        }
    }

    #[test]
    fn normalize_keeps_cjk_and_strips_punct() {
        assert_eq!(normalize(" QQ: 3384865082 声音: 开启"), "qq3384865082声音开启");
        assert_eq!(normalize("野狐围棋: V533343630"), "野狐围棋v533343630");
        assert_eq!(normalize(" DeepSeek Harness "), "deepseekharness");
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn alias_expands_chinese_names() {
        let t = alias_tokens(&normalize("微信"));
        assert!(t.iter().any(|x| x == "weixin"));
        let t = alias_tokens(&normalize("企业微信"));
        assert!(t.iter().any(|x| x == "wxwork"));
        let t = alias_tokens(&normalize("抖音"));
        assert!(t.iter().any(|x| x == "douyin"));
        // 无关名字不应产生别名 token
        assert!(alias_tokens(&normalize("任务栏输入指示")).is_empty());
    }

    #[test]
    fn match_real_uia_names() {
        let entries = vec![
            entry("野狐围棋", "foxwq", "野狐围棋"),
            entry("", "qq", "QQ"),
            entry("", "weixin", "Weixin"),
            entry("", "deepseekharness", "DeepSeek Harness"),
            entry("", "dockerdesktop", "Docker Desktop"),
            entry("抖音", "douyintray", "douyin_tray"),
            entry("小米互联服务", "mipccontinuity", "MI PcContinuity"),
        ];
        let find = |name: &str| best_match(name, &entries).map(|e| e.exe_stem.clone());
        assert_eq!(find("野狐围棋: V533343630").as_deref(), Some("foxwq"));
        assert_eq!(find(" QQ: 3384865082 声音: 开启").as_deref(), Some("qq"));
        assert_eq!(find("微信").as_deref(), Some("weixin"));
        assert_eq!(find(" DeepSeek Harness").as_deref(), Some("deepseekharness"));
        assert_eq!(
            find("Docker Desktop is running in Resource Saver mode").as_deref(),
            Some("dockerdesktop")
        );
        assert_eq!(find("抖音").as_deref(), Some("douyintray"));
        assert_eq!(find("小米互联服务").as_deref(), Some("mipccontinuity"));
        // 输入法指示器不是应用托盘图标，不应误配到任何应用
        assert_eq!(
            find("任务栏输入指示 中文模式\n\n单击右键以查看更多选项"),
            None
        );
    }

    #[test]
    fn no_match_for_unrelated_text() {
        let entries = vec![entry("微信", "weixin", "Weixin")];
        assert!(best_match("随便一个不认识的应用", &entries).is_none());
        assert!(best_match("", &entries).is_none());
    }

    /// 展示名必须用原始文本：修复前 resolve_identity("QQ") 会返回归一化键 "qq"，
    /// resolve_identity("dev.zcode.app") 会返回 "zcode"。
    #[test]
    fn display_name_uses_raw_text_not_normalized_key() {
        // FileDescription 原文优先（"QQ" 而不是 "qq"）
        assert_eq!(
            display_name_from(&entry("", "QQ", "QQ")).as_deref(),
            Some("QQ")
        );
        assert_eq!(
            display_name_from(&entry("", "ZCode", "ZCode")).as_deref(),
            Some("ZCode")
        );
        assert_eq!(
            display_name_from(&entry("", "Docker Desktop", "Docker Desktop")).as_deref(),
            Some("Docker Desktop")
        );
        // 没有 FileDescription：用原始 tooltip 首行（多行提示只取第一行）
        assert_eq!(
            display_name_from(&entry("野狐围棋\n版本号: 26.0.1.1007", "foxwq", "")).as_deref(),
            Some("野狐围棋")
        );
        // 再退到 exe 主名原文
        assert_eq!(
            display_name_from(&entry("", "douyin_tray", "")).as_deref(),
            Some("douyin_tray")
        );
        // 三者全空 → None（调用方回退 primary_id 原样）
        assert_eq!(display_name_from(&entry("", "", "")), None);
    }

    /// 命中注册表条目后，展示名与打分互不干扰：打分用归一化键，展示用原文
    #[test]
    fn matched_entry_keeps_original_case_in_display_name() {
        let entries = vec![
            entry("", "qq", "QQ"),
            entry("", "zcode", "ZCode"),
        ];
        let qq = best_match("QQ", &entries).expect("应命中 QQ");
        assert_eq!(display_name_from(qq).as_deref(), Some("QQ"));
        let zcode = best_match("dev.zcode.app", &entries).expect("应命中 ZCode");
        assert_eq!(display_name_from(zcode).as_deref(), Some("ZCode"));
    }

    /// 本机冒烟：读真实注册表，确认能解出带 alpha 的 PNG data URL。
    /// 依赖当前桌面环境，默认忽略；手动执行：
    /// `cargo test --lib -- --ignored smoke_registry --nocapture`
    #[test]
    #[ignore]
    fn smoke_registry_icons() {
        let entries = entries();
        eprintln!("[app-icons] 注册表条目数: {}", entries.len());
        if entries.is_empty() {
            return; // 无托盘图标的环境不算失败
        }
        let with_icon = entries
            .iter()
            .filter(|e| {
                e.icon
                    .as_deref()
                    .map(|u| u.starts_with("data:image/png;base64,"))
                    .unwrap_or(false)
            })
            .count();
        eprintln!("[app-icons] 可解出 PNG 图标的条目数: {with_icon}");
        assert!(with_icon > 0, "没有任何条目能解出 PNG data URL");
        for name in ["微信", "抖音", "小米互联服务", " QQ: 3384865082 声音: 开启"] {
            let icon = resolve_tray_icon(name);
            eprintln!(
                "[app-icons] {name:?} -> {}",
                icon.as_deref()
                    .map(|u| format!("{} 字节 data URL", u.len()))
                    .unwrap_or_else(|| "无匹配".to_string())
            );
        }
        // 展示名回归检查：必须是原始文本（"QQ"/"ZCode"），不能是归一化键
        for id in ["QQ", "dev.zcode.app", "Weixin", "Docker Desktop.exe"] {
            match resolve_identity(id) {
                Some(ident) => eprintln!(
                    "[app-icons] resolve_identity({id:?}) -> name={:?} icon={}",
                    ident.name,
                    if ident.icon.is_some() { "有" } else { "无" }
                ),
                None => eprintln!("[app-icons] resolve_identity({id:?}) -> None"),
            }
        }
        // 版本信息长度回归：VerQueryValue 的 pulLen 是字符数，误当字节数会把
        // "QQ" 截成 "Q"。用系统文件的 ProductName（很长）检查是否被截断。
        if let Ok(root) = std::env::var("SystemRoot") {
            let sys = format!("{root}\\explorer.exe");
            if let Some(pn) = version_string(&sys, "ProductName") {
                eprintln!("[app-icons] explorer.exe ProductName = {pn:?}");
                assert!(
                    pn.chars().count() >= 20,
                    "版本信息字符串疑似被截断: {pn:?}"
                );
                assert!(!pn.contains('\0'), "版本信息字符串残留 NUL");
            }
        }
    }
}

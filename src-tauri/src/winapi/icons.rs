//! 应用图标提取：窗口图标 / UWP AUMID / exe 文件图标 → PNG data URL
#![allow(non_upper_case_globals)]

use base64::{engine::general_purpose::STANDARD, Engine as _};
use windows::core::{GUID, PCWSTR};
use windows::Win32::Foundation::{BOOL, HWND};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC,
    BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
};
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
use windows::Win32::UI::Shell::PropertiesSystem::{
    IPropertyStore, PROPERTYKEY, SHGetPropertyStoreForWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, EnumChildWindows, GetClassLongPtrW, GetIconInfo, SendMessageTimeoutW,
    GET_CLASS_LONG_INDEX, HICON, ICONINFO, SMTO_ABORTIFHUNG, WM_GETICON,
};

const GCLP_HICON: i32 = -14;
const GCLP_HICONSM: i32 = -34;

// PKEY_AppUserModel_ID = {9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}, 5
const PKEY_APPUSERMODEL_ID: (u128, u32) = (0x9F4C_2855_9F79_4B39_A8D0_E1D4_2DE1_D5F3, 5);

/// 在当前线程初始化 COM（shell 图标提取需要）。重复调用无害。
pub fn ensure_com() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

fn png_data_url(rgba: Vec<u8>, w: i32, h: i32) -> Option<String> {
    let mut img = image::RgbaImage::new(w as u32, h as u32);
    img.copy_from_slice(&rgba);
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(format!("data:image/png;base64,{}", STANDARD.encode(png)))
}

/// 读取 32bpp HBITMAP（自顶向下）→ RGBA
fn bitmap_to_rgba(hdc_owner: windows::Win32::Graphics::Gdi::HDC, hbmp: HBITMAP) -> Option<(Vec<u8>, i32, i32)> {
    unsafe {
        let mut bm = BITMAP::default();
        if GetObjectW(hbmp, std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) == 0 {
            return None;
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight);
        if w <= 0 || h <= 0 || w > 2048 || h > 2048 {
            return None;
        }
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w;
        bmi.bmiHeader.biHeight = -h; // 自顶向下
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = 0;

        let mut buf = vec![0u8; (w * h * 4) as usize];
        if GetDIBits(
            hdc_owner,
            hbmp,
            0,
            h as u32,
            Some(buf.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        ) == 0
        {
            return None;
        }
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2); // BGRA → RGBA
        }
        Some((buf, w, h))
    }
}

/// HICON → RGBA 像素。正确处理：
/// - 32bpp 带 alpha 的现代图标（直接使用 alpha 通道）
/// - 全零 alpha 的老式图标（用 AND 掩码推导透明度，掩码高度可能是 h 或 2h）
/// - 破损 alpha（alpha=0 但有颜色）修复为不透明
fn hicon_to_rgba(hicon: HICON) -> Option<(Vec<u8>, i32, i32)> {
    unsafe {
        let mut info = ICONINFO::default();
        if GetIconInfo(hicon, &mut info).is_err() {
            return None;
        }
        let result = (|| {
            if info.hbmColor.is_invalid() {
                return None; // 纯黑白图标走不到这，直接放弃
            }
            let hdc_screen = GetDC(None);
            let mem = CreateCompatibleDC(hdc_screen);
            let out = (|| {
                let (buf, w, h) = bitmap_to_rgba(mem, info.hbmColor)?;

                let has_alpha = buf.chunks_exact(4).any(|p| p[3] != 0);
                if has_alpha {
                    // 破损 alpha 修复：alpha=0 但像素有颜色 → 不透明
                    let mut fixed = buf;
                    for px in fixed.chunks_exact_mut(4) {
                        if px[3] == 0 && (px[0] | px[1] | px[2]) != 0 {
                            px[3] = 255;
                        }
                    }
                    Some((fixed, w, h))
                } else {
                    // 老式图标：用 AND 掩码推导 alpha
                    let mut mbm = BITMAP::default();
                    let mask_known = GetObjectW(
                        HGDIOBJ(info.hbmMask.0),
                        std::mem::size_of::<BITMAP>() as i32,
                        Some(&mut mbm as *mut _ as *mut _),
                    ) != 0;
                    // 黑白图标的掩码高度是 2h（上半 AND 下半 XOR），彩色图标是 h
                    let mask_h = if mask_known { mbm.bmHeight } else { h };
                    let row_bytes = ((w as usize) + 31) / 32 * 4;

                    let mut mbmi = BITMAPINFO::default();
                    mbmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                    mbmi.bmiHeader.biWidth = w;
                    mbmi.bmiHeader.biHeight = -mask_h; // 自顶向下
                    mbmi.bmiHeader.biPlanes = 1;
                    mbmi.bmiHeader.biBitCount = 1;
                    mbmi.bmiHeader.biCompression = 0;

                    let mut mbuf = vec![0u8; row_bytes * mask_h.max(0) as usize];
                    let mask_ok = GetDIBits(
                        mem,
                        info.hbmMask,
                        0,
                        mask_h.max(0) as u32,
                        Some(mbuf.as_mut_ptr() as *mut _),
                        &mut mbmi,
                        DIB_RGB_COLORS,
                    ) != 0;

                    let mut out = buf;
                    for (i, px) in out.chunks_exact_mut(4).enumerate() {
                        let x = i % w as usize;
                        let y = i / w as usize;
                        let and_y = if mask_h >= 2 * h { y + h as usize } else { y };
                        let a = if mask_ok {
                            match mbuf.get(and_y * row_bytes + x / 8) {
                                Some(byte) => {
                                    // AND 掩码：1 = 透明
                                    if (byte >> (7 - x % 8)) & 1 == 1 { 0 } else { 255 }
                                }
                                None => 255,
                            }
                        } else {
                            255 // 掩码读不到就当作不透明
                        };
                        px[3] = a;
                    }
                    Some((out, w, h))
                }
            })();
            let _ = DeleteDC(mem);
            let _ = ReleaseDC(None, hdc_screen);
            out
        })();

        if !info.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
        }
        if !info.hbmMask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        }
        result
    }
}

pub fn icon_data_url(hicon: HICON) -> Option<String> {
    if hicon.is_invalid() {
        return None;
    }
    let (rgba, w, h) = hicon_to_rgba(hicon)?;
    png_data_url(rgba, w, h)
}

/// 读取窗口的 AppUserModelID（UWP/打包应用非空）。
/// 有些 UWP 窗口的 AUMID 挂在子窗口（CoreWindow）上，顶层读不到时探测子窗口。
pub fn window_aumid(hwnd: HWND) -> Option<String> {
    if let Some(s) = aumid_of_hwnd(hwnd) {
        return Some(s);
    }
    unsafe {
        let mut children: Vec<HWND> = Vec::new();
        let ctx = &mut children;
        let _ = EnumChildWindows(
            hwnd,
            Some(enum_child_cb),
            windows::Win32::Foundation::LPARAM(ctx as *mut _ as isize),
        );
        for ch in children {
            if let Some(s) = aumid_of_hwnd(ch) {
                return Some(s);
            }
        }
    }
    None
}

unsafe extern "system" fn enum_child_cb(hwnd: HWND, lparam: windows::Win32::Foundation::LPARAM) -> BOOL {
    let children = &mut *(lparam.0 as *mut Vec<HWND>);
    children.push(hwnd);
    true.into()
}

fn aumid_of_hwnd(hwnd: HWND) -> Option<String> {
    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd).ok()?;
        let key = PROPERTYKEY {
            fmtid: GUID::from_u128(PKEY_APPUSERMODEL_ID.0),
            pid: PKEY_APPUSERMODEL_ID.1,
        };
        let pv = store.GetValue(&key).ok()?;
        let s = pv.to_string();
        if s.trim().is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

/// 通过包清单提取打包应用（UWP/商店应用）的真实图标和应用显示名。
/// 纯注册表 + 文件读取，不用 Shell COM（BindToHandler/GetImage 在非主线程
/// 会报 MK_E_NOOBJECT，且无消息泵线程上的跨进程调用可能永久阻塞）。
/// 返回 (icon data URL, 显示名)。
pub fn aumid_app_info(aumid: &str) -> Option<(String, String)> {
    let (family, app_id) = aumid.split_once('!').unwrap_or((aumid, ""));
    let root = match find_package_root(family) {
        Some(r) => r,
        None => {
            eprintln!("[icon-worker] 找不到包根目录: {family}");
            return None;
        }
    };
    let manifest = match std::fs::read_to_string(root.join("AppxManifest.xml")) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[icon-worker] 读取 AppxManifest 失败 ({}): {e}", root.display());
            return None;
        }
    };
    let logo_rel = match find_app_logo(&manifest, app_id) {
        Some(l) => l,
        None => {
            eprintln!("[icon-worker] manifest 中找不到应用图标资源: {aumid}");
            return None;
        }
    };
    let logo_path = match resolve_logo_file(&root, &logo_rel) {
        Some(p) => p,
        None => {
            eprintln!("[icon-worker] 图标文件不存在: {logo_rel}");
            return None;
        }
    };
    let data = match std::fs::read(&logo_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[icon-worker] 读取图标文件失败 ({}): {e}", logo_path.display());
            return None;
        }
    };
    let img = match image::load_from_memory(&data) {
        Ok(i) => i.to_rgba8(),
        Err(e) => {
            eprintln!("[icon-worker] 图标解码失败 ({}): {e}", logo_path.display());
            return None;
        }
    };
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    let url = format!("data:image/png;base64,{}", STANDARD.encode(png));
    let name = find_display_name(&manifest);
    Some((url, name))
}

/// 在注册表 AppModel 仓库里按包家族名找包根目录（键名带版本号，需前缀匹配）
fn find_package_root(family: &str) -> Option<std::path::PathBuf> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_READ, REG_SZ,
    };
    unsafe {
        let base = super::common::to_wide(
            r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages",
        );
        let mut hkey = HKEY::default();
        let open_err = RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(base.as_ptr()), 0, KEY_READ, &mut hkey);
        if open_err.is_err() {
            eprintln!("[icon-worker] RegOpenKeyExW 失败: code={}", open_err.0);
            return None;
        }
        // 键名形如 Name_版本_架构__发布者：包家族名是 Name_发布者，
        // 因此前缀取 Name 部分（最后一个下划线之前）
        let name_part = family.rsplit_once('_').map(|(n, _)| n).unwrap_or(family);
        let prefix = format!("{}_", name_part).to_lowercase();
        let mut result = None;
        let mut index = 0u32;
        loop {
            let mut name_buf = [0u16; 512];
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
            if err.is_err() {
                break;
            }
            index += 1;
            let name = String::from_utf16_lossy(&name_buf[..name_len as usize]).to_lowercase();
            if !name.starts_with(&prefix) {
                continue;
            }
            // 打开匹配到的包键，读它自己的 PackageRootFolder 值
            let original_name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
            let sub = super::common::to_wide(&original_name);
            let mut child = HKEY::default();
            if RegOpenKeyExW(hkey, PCWSTR(sub.as_ptr()), 0, KEY_READ, &mut child).is_err() {
                continue;
            }
            let value_name = super::common::to_wide("PackageRootFolder");
            let mut data_buf = [0u8; 2048];
            let mut data_len = data_buf.len() as u32;
            let mut ty = REG_SZ;
            let ok = RegQueryValueExW(
                child,
                PCWSTR(value_name.as_ptr()),
                None,
                Some(&mut ty),
                Some(data_buf.as_mut_ptr()),
                Some(&mut data_len),
            )
            .is_ok();
            let _ = RegCloseKey(child);
            if ok {
                let wlen = data_len as usize / 2;
                let s = String::from_utf16_lossy(
                    &std::slice::from_raw_parts(data_buf.as_ptr() as *const u16, wlen),
                );
                let s = s.trim_end_matches('\0').trim().to_string();
                if !s.is_empty() {
                    result = Some(std::path::PathBuf::from(s));
                    break;
                }
            }
        }
        let _ = RegCloseKey(hkey);
        result
    }
}

/// 在 manifest 里定位指定应用的图标资源（Square44x44Logo 优先，其次 Logo）
fn find_app_logo(manifest: &str, app_id: &str) -> Option<String> {
    let id_attr = format!("Id=\"{app_id}\"");
    let mut rest = manifest;
    loop {
        let pos = rest.find("<Application")?;
        let tag_end = pos + rest[pos..].find('>')?;
        let tag = &rest[pos..tag_end];
        let matches_id = app_id.is_empty()
            || tag.contains(&id_attr)
            || tag.contains(&format!("Id='{app_id}'"));
        if matches_id {
            let block_end = rest[pos..]
                .find("</Application>")
                .map(|e| e + pos)
                .unwrap_or(rest.len());
            let block = &rest[pos..block_end];
            for key in ["Square44x44Logo=\"", "Logo=\"", "Square44x44Logo='", "Logo='"] {
                if let Some(i) = block.find(key) {
                    let quote = key.chars().last()?;
                    let s = i + key.len();
                    if let Some(e) = block[s..].find(quote) {
                        return Some(block[s..s + e].to_string());
                    }
                }
            }
        }
        rest = &rest[tag_end + 1..];
    }
}

/// 解析图标文件真实路径：精确 → scale 变体 → 同目录同名前缀最大文件
fn resolve_logo_file(
    root: &std::path::Path,
    rel: &str,
) -> Option<std::path::PathBuf> {
    let rel = rel.replace('/', "\\");
    let exact = root.join(&rel);
    if exact.is_file() {
        return Some(exact);
    }
    let path = std::path::PathBuf::from(&rel);
    let stem = path.file_stem()?.to_string_lossy().to_string();
    let dir = root.join(path.parent().unwrap_or_else(|| std::path::Path::new("")));
    for s in ["200", "150", "125", "100"] {
        for suffix in [".scale-%s.png", "_scale-%s.png", "%s.png"].iter() {
            let cand = dir.join(format!("{stem}{}", suffix.replace("%s", s)));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    // 目录扫描：同前缀 png 里取最大
    let mut best: Option<(u64, std::path::PathBuf)> = None;
    let rd = std::fs::read_dir(&dir).ok()?;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with(&stem)
            && name.to_lowercase().ends_with(".png")
            && !name.contains("contrast")
        {
            let sz = e.metadata().map(|m| m.len()).unwrap_or(0);
            if best.as_ref().map(|(b, _)| sz > *b).unwrap_or(true) {
                best = Some((sz, e.path()));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// manifest <Properties><DisplayName> 的纯文本值（ms-resource 形式返回空，前端用窗口标题兜底）
fn find_display_name(manifest: &str) -> String {
    let Some(i) = manifest.find("<DisplayName>") else {
        return String::new();
    };
    let s = i + "<DisplayName>".len();
    let Some(e) = manifest[s..].find("</DisplayName>") else {
        return String::new();
    };
    let v = manifest[s..s + e].trim().to_string();
    if v.starts_with("ms-resource:") {
        String::new()
    } else {
        v
    }
}

/// 窗口图标：WM_GETICON → 类图标 → exe 文件图标
pub fn window_icon_data_url(hwnd: HWND) -> Option<String> {
    unsafe {
        let mut res: usize = 0;
        for wparam in [1usize, 0usize] {
            // 1 = ICON_BIG, 0 = ICON_SMALL
            let ok = SendMessageTimeoutW(
                hwnd,
                WM_GETICON,
                windows::Win32::Foundation::WPARAM(wparam),
                windows::Win32::Foundation::LPARAM(0),
                SMTO_ABORTIFHUNG,
                150,
                Some(&mut res),
            );
            if ok != windows::Win32::Foundation::LRESULT(0) && res != 0 {
                if let Some(url) = icon_data_url(HICON(res as *mut _)) {
                    return Some(url);
                }
            }
        }
        for index in [GCLP_HICON, GCLP_HICONSM] {
            let gcl = GetClassLongPtrW(hwnd, GET_CLASS_LONG_INDEX(index));
            if gcl != 0 {
                if let Some(url) = icon_data_url(HICON(gcl as *mut _)) {
                    return Some(url);
                }
            }
        }
    }
    // 回退：进程 exe 的文件图标
    if let Some(path) = super::common::process_image_path(hwnd) {
        return file_icon_data_url(&path);
    }
    None
}

/// 从文件路径提取图标（用于固定应用的启动项）
pub fn file_icon_data_url(path: &str) -> Option<String> {
    let wpath = super::common::to_wide(path);
    let mut sfi = SHFILEINFOW::default();
    let ok = unsafe {
        SHGetFileInfoW(
            PCWSTR(wpath.as_ptr()),
            FILE_ATTRIBUTE_NORMAL,
            Some(&mut sfi as *mut _ as *mut _),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    if ok == 0 || sfi.hIcon.is_invalid() {
        return None;
    }
    let url = icon_data_url(sfi.hIcon);
    unsafe {
        let _ = DestroyIcon(sfi.hIcon);
    }
    url
}

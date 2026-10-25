//! 应用图标提取：窗口图标 / UWP AUMID / exe 文件图标 → PNG data URL
#![allow(non_upper_case_globals)]

use base64::{engine::general_purpose::STANDARD, Engine as _};
use windows::core::{GUID, PCWSTR};
use windows::Win32::Foundation::{BOOL, HWND, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC,
    BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
};
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
use windows::Win32::System::Com::{CoInitializeEx, IBindCtx, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SHGetFileInfoW, SHFILEINFOW,
    SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY, SHGFI_ICON, SHGFI_LARGEICON,
};
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

/// 通过 Shell 图标工厂取任意尺寸的透明图标（PNG data URL，size 约 48–64）。
///
/// 走 `IShellItemImageFactory`：比 SHGetFileInfo（只有 32px 系统图标尺寸）清晰得多，
/// 且返回带 alpha 的 32bpp 位图，浅色背景上不会出现白底。
/// 该函数会在应用中心后台线程（MTA + RoInitialize）上调用；
/// 任何失败只返回 None，由上层回退 `file_icon_data_url`。
pub fn shell_item_icon_png(path: &str, size: u32) -> Option<String> {
    let size = size.clamp(16, 256);
    let (mut rgba, w, h) = shell_image_rgba(path, size)?;
    // Shell 返回的位图可能是预乘 alpha，也可能是"全零 alpha 的不透明图"
    normalize_icon_alpha(&mut rgba);

    let mut img = image::RgbaImage::from_raw(w as u32, h as u32, rgba)?;
    // 应用中心只需要 48px：BIGGERSIZEOK 可能返回 256px 大图，这里缩下去（保透明）
    if w as u32 > 48 || h as u32 > 48 {
        let scale = 48.0 / (w.max(h) as f32);
        let nw = ((w as f32 * scale).round() as u32).max(1);
        let nh = ((h as f32 * scale).round() as u32).max(1);
        img = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Lanczos3);
    }
    let (ow, oh) = (img.width() as i32, img.height() as i32);
    png_data_url(img.into_raw(), ow, oh)
}

/// 用 Shell 图标工厂取原始 RGBA（已做 BGRA→RGBA，未做 alpha 归一化）。
/// 单独拆出来便于冒烟测试观察 Shell 返回的 alpha 形态。
fn shell_image_rgba(path: &str, size: u32) -> Option<(Vec<u8>, i32, i32)> {
    let wpath = super::common::to_wide(path);
    // Shell 项创建失败（路径不存在 / 无权限）直接放弃
    let factory: IShellItemImageFactory = unsafe {
        SHCreateItemFromParsingName(PCWSTR(wpath.as_ptr()), None::<&IBindCtx>)
    }
    .ok()?;
    let hbmp: HBITMAP = unsafe {
        factory.GetImage(
            SIZE {
                cx: size as i32,
                cy: size as i32,
            },
            SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK,
        )
    }
    .ok()?;

    // HBITMAP → RGBA：借一个内存 DC 做 GetDIBits（bitmap_to_rgba 内部已交换 BGRA→RGBA）
    let raw = unsafe {
        let hdc_screen = GetDC(None);
        let mem = CreateCompatibleDC(hdc_screen);
        let out = bitmap_to_rgba(mem, hbmp);
        let _ = DeleteDC(mem);
        let _ = ReleaseDC(None, hdc_screen);
        out
    };
    unsafe {
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
    }
    raw
}

/// 归一化 Shell 图标的 alpha：
/// - 全零 alpha（Shell 有时返回不带 alpha 的帧）→ 视为不透明，避免整图变透明；
/// - 否则若所有半透明像素的颜色都不超过 alpha，判定为预乘 alpha 并还原；
/// - 已存在「颜色 > alpha」的像素说明是直通 alpha，保持原样。
fn normalize_icon_alpha(buf: &mut [u8]) {
    let mut any_alpha = false;
    let mut any_translucent = false;
    let mut channel_gt_alpha = false;
    for px in buf.chunks_exact(4) {
        let a = px[3];
        if a != 0 {
            any_alpha = true;
        }
        if a != 0 && a != 255 {
            any_translucent = true;
            if px[0].max(px[1]).max(px[2]) > a {
                channel_gt_alpha = true;
            }
        }
    }
    if !any_alpha {
        for px in buf.chunks_exact_mut(4) {
            px[3] = 255;
        }
        return;
    }
    if any_translucent && !channel_gt_alpha {
        for px in buf.chunks_exact_mut(4) {
            let a = px[3] as u32;
            if a > 0 && a < 255 {
                for c in 0..3 {
                    px[c] = ((px[c] as u32 * 255 / a).min(255)) as u8;
                }
            }
        }
    }
}

/// 把一张图标 PNG data URL 规范化：裁掉四周透明留白，再等比缩放到
/// `target` 尺寸并居中（留 2px 透明边距）。
///
/// UWP 的 Square44x44Logo 资源往往自带大块透明 padding（例如 150×150 的
/// 资源里图形只占中间一小块），直接按 48px 显示会明显偏小。桌面应用图标
/// 通常已铺满，不走这条路径（仅应用中心对 uwp 条目调用）。
pub fn normalize_app_icon(data_url: &str, target: u32) -> Option<String> {
    let b64 = data_url.rsplit_once("base64,").map(|(_, b)| b)?;
    let bytes = STANDARD.decode(b64).ok()?;
    let img = image::load_from_memory(&bytes).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 || w > 1024 || h > 1024 {
        return None;
    }

    // 透明留白边界（alpha > 8 视为有效图形）
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (w, h, 0u32, 0u32);
    let mut found = false;
    for (x, y, p) in img.enumerate_pixels() {
        if p[3] > 8 {
            found = true;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    if !found {
        return None;
    }

    let target = target.clamp(16, 256);
    let bw = (max_x - min_x + 1).max(1);
    let bh = (max_y - min_y + 1).max(1);
    let pad = 2u32;
    let inner = target.saturating_sub(pad * 2).max(1);
    let scale = (inner as f32 / bw as f32).min(inner as f32 / bh as f32);
    let nw = ((bw as f32 * scale).round() as u32).clamp(1, inner);
    let nh = ((bh as f32 * scale).round() as u32).clamp(1, inner);

    let cropped = image::imageops::crop_imm(&img, min_x, min_y, bw, bh).to_image();
    let resized = image::imageops::resize(&cropped, nw, nh, image::imageops::FilterType::Lanczos3);
    let mut canvas = image::RgbaImage::new(target, target);
    image::imageops::overlay(
        &mut canvas,
        &resized,
        ((target - nw) / 2) as i64,
        ((target - nh) / 2) as i64,
    );

    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(canvas)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(format!("data:image/png;base64,{}", STANDARD.encode(png)))
}

#[cfg(test)]
mod shell_icon_tests {
    use super::*;

    /// 在目录里递归找第一个 .lnk（冒烟测试用，深度有限）
    fn first_shortcut(dir: &std::path::Path, depth: usize) -> Option<std::path::PathBuf> {
        if depth > 4 {
            return None;
        }
        let mut dirs = Vec::new();
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                dirs.push(p);
            } else if p
                .extension()
                .map(|x| x.eq_ignore_ascii_case("lnk"))
                .unwrap_or(false)
            {
                return Some(p);
            }
        }
        dirs.sort();
        dirs.into_iter()
            .find_map(|d| first_shortcut(&d, depth + 1))
    }

    /// 本机冒烟：Shell 图标工厂取真实 .lnk 的 48px 透明 PNG。
    /// 手动执行：`cargo test --lib -- --ignored smoke_shell_item_icon --nocapture`
    #[test]
    #[ignore]
    fn smoke_shell_item_icon() {
        ensure_com();
        let mut roots = Vec::new();
        if let Ok(p) = std::env::var("APPDATA") {
            roots.push(format!(r"{p}\Microsoft\Windows\Start Menu\Programs"));
        }
        if let Ok(p) = std::env::var("ProgramData") {
            roots.push(format!(r"{p}\Microsoft\Windows\Start Menu\Programs"));
        }
        let lnk = match roots
            .iter()
            .map(std::path::Path::new)
            .find_map(|root| first_shortcut(root, 0))
        {
            Some(p) => p,
            None => {
                eprintln!("[icons] 本机 Start Menu 没找到 .lnk，跳过");
                return;
            }
        };
        let path = lnk.to_string_lossy().to_string();
        eprintln!("[icons] 测试快捷方式: {path}");

        // 观察 Shell 返回的原始 alpha 形态（确认预乘/直通判定是否被触发）
        if let Some((raw, w, h)) = shell_image_rgba(&path, 48) {
            let (mut translucent, mut gt, mut zero, mut opaque) = (0usize, 0usize, 0usize, 0usize);
            for px in raw.chunks_exact(4) {
                match px[3] {
                    0 => zero += 1,
                    255 => opaque += 1,
                    a => {
                        translucent += 1;
                        if px[0].max(px[1]).max(px[2]) > a {
                            gt += 1;
                        }
                    }
                }
            }
            eprintln!(
                "[icons] 原始 {w}x{h}：半透明 {translucent}、全透明 {zero}、不透明 {opaque}、通道>alpha {gt}"
            );
        }

        let url = shell_item_icon_png(&path, 48).expect("应能取到 PNG 图标");
        assert!(url.starts_with("data:image/png;base64,"), "应为 PNG data URL");
        let b64 = url.split(',').nth(1).expect("data URL 格式");
        let bytes = STANDARD.decode(b64).expect("base64 解码");
        let img = image::load_from_memory(&bytes)
            .expect("PNG 解码")
            .to_rgba8();
        eprintln!(
            "[icons] 输出 {}x{}，PNG {} 字节",
            img.width(),
            img.height(),
            bytes.len()
        );
        assert_eq!(img.width(), img.height(), "图标应为正方形");
        assert!(img.width() >= 16 && img.width() <= 48, "尺寸应在 16..=48");
        let opaque = img.pixels().filter(|p| p[3] == 255).count();
        let transparent = img.pixels().filter(|p| p[3] == 0).count();
        assert!(opaque > 0, "图标应包含不透明像素");
        eprintln!(
            "[icons] 输出 alpha：不透明 {opaque}、全透明 {transparent} / {}",
            img.width() * img.height()
        );
    }

    /// UWP Logo 留白裁剪：生成 64×64 画布、中间 20×20 图形，规范化到 48px 后
    /// 图形应铺满约 44px（2px 边距）且保持居中。
    #[test]
    fn normalize_app_icon_trims_padding() {
        let mut img = image::RgbaImage::new(64, 64);
        for y in 22..42 {
            for x in 22..42 {
                img.put_pixel(x, y, image::Rgba([200, 40, 40, 255]));
            }
        }
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("编码 PNG");
        let url = format!("data:image/png;base64,{}", STANDARD.encode(png));

        let out = normalize_app_icon(&url, 48).expect("应能规范化");
        let bytes = STANDARD
            .decode(out.split(',').nth(1).expect("data URL"))
            .expect("base64");
        let out_img = image::load_from_memory(&bytes).expect("PNG 解码").to_rgba8();
        assert_eq!((out_img.width(), out_img.height()), (48, 48));

        let (mut min_x, mut min_y, mut max_x, mut max_y) = (48u32, 48u32, 0u32, 0u32);
        for (x, y, p) in out_img.enumerate_pixels() {
            if p[3] > 8 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
        let bw = max_x - min_x + 1;
        let bh = max_y - min_y + 1;
        assert!((40..=44).contains(&bw), "图形应铺满内部（宽 {bw}）");
        assert!((40..=44).contains(&bh), "图形应铺满内部（高 {bh}）");
        assert!(min_x >= 2 && min_y >= 2, "应保留 2px 透明边距");
    }
}

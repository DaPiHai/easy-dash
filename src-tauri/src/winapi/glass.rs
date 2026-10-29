//! 液态玻璃 2.0：面板打开前取样桌面背景 → 低分辨率 JPEG + 亮度分位
//!
//! 控制中心 / 通知中心是顶栏窗口里的 `.panel`，WebView2 透明窗口无法用
//! `backdrop-filter` 对桌面采样（见 docs/design-system.md §1）。所以在面板展开前
//! 由 Rust 抓一块「面板将要占据的屏幕矩形」，前端把它当玻璃的背景底片：
//! JPEG 做模糊底图，p05/p95 决定玻璃的明暗映射与叠加色。
//!
//! 入参是**顶栏窗口客户区内的 CSS 像素**（相对视口），不是屏幕物理坐标：
//! 顶栏会因任务栏避让 / 工作区接管而偏移，只能靠 `ClientToScreen` 取真实原点。
//!
//! 两个入口：`backdrop` 取样顶栏窗口里某个面板矩形（控制中心/通知中心），
//! `window_backdrop` 取样整个窗口客户区（Dock / 概览 / 应用中心 / 管理面板在
//! show 之前各抓一次，缓存到 AppState 供前端做窗口级玻璃底片）。
#![allow(non_upper_case_globals)]

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Serialize;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_SUCCESS, HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetSystemMetrics, SystemParametersInfoW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SYSTEM_PARAMETERS_INFO_ACTION,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

use super::common;
use super::snapshot;

/// 取样图目标最大宽度（缩放后）：再大对玻璃底图没有意义，只会撑大 data URL
const MAX_WIDTH: u32 = 480;
/// 截屏单边上限（物理像素）：4K 屏上展开的面板也远小于它
const MAX_CAPTURE_SIDE: i32 = 4096;
/// JPEG 质量：玻璃本身要模糊，画质低一点反而更省内存
const JPEG_QUALITY: u8 = 72;
/// 近黑像素的单通道阈值与判定占比（超过即视为没抓到桌面）
const BLACK_CHANNEL: u8 = 8;
const BLACK_RATIO: f32 = 0.95;
/// 裁剪后短边下限：再小没有取样意义（面板被拖出屏幕时）
const MIN_SIDE: i32 = 16;

/// 面板背后的桌面取样结果（前端据此做液态玻璃的底色与明暗映射）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlassBackdrop {
    /// `data:image/jpeg;base64,...`（宽 <= 480，保持原始比例）
    pub data_url: String,
    /// 线性相对亮度的 5% 分位（0..1）
    pub p05: f32,
    /// 线性相对亮度的 95% 分位（0..1）
    pub p95: f32,
    /// 缩放后小图的平均 RGB
    pub mean: [u8; 3],
    /// 缩放后小图的像素宽
    pub width: u32,
    /// 缩放后小图的像素高
    pub height: u32,
}

/// 截取顶栏窗口客户区内 `x/y/width/height`（CSS px）对应的桌面区域。
///
/// 失败一律返回 None（调用方不需要区分原因，前端回退为纯半透明表面）：
/// 窗口/HWND 不可用、入参非法、裁剪后不足 16px、截屏失败、近黑桌面。
pub fn backdrop(
    window: &tauri::WebviewWindow,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Option<GlassBackdrop> {
    // 前端可能传 NaN/负数（面板尚未布局完），先挡掉再进 Win32
    if !(x.is_finite() && y.is_finite() && width.is_finite() && height.is_finite())
        || width <= 0.0
        || height <= 0.0
    {
        return None;
    }

    let hwnd = common::tauri_hwnd(window)?;
    let scale = dpi_scale(window, hwnd);

    // 客户区原点：顶栏可能被任务栏避让推离 (0,0)，不能假设显示器坐标
    let mut origin = POINT::default();
    let ok = unsafe { ClientToScreen(hwnd, &mut origin) };
    if !ok.as_bool() {
        return None;
    }

    let (px, py, pw, ph) = physical_rect(origin.x, origin.y, scale, x, y, width, height)?;
    let (px, py, pw, ph) = clamp_rect(px, py, pw, ph, virtual_screen())?;
    capture_rect(px, py, pw, ph)
}

/// 截取窗口**整个客户区**的桌面取样（窗口级玻璃底片）。
///
/// 供各 surface 在 `show()` 之前调用：此时窗口还不可见，截到的是它将要覆盖的
/// 桌面；已可见的窗口不要调（截到的是自己）。客户区尺寸用 `GetClientRect`
/// （物理像素，不含标题栏/边框），原点用 `ClientToScreen`。
pub fn window_backdrop(window: &tauri::WebviewWindow) -> Option<GlassBackdrop> {
    let hwnd = common::tauri_hwnd(window)?;

    let mut client = RECT::default();
    unsafe { GetClientRect(hwnd, &mut client) }.ok()?;
    let w = client.right - client.left;
    let h = client.bottom - client.top;
    if w <= 0 || h <= 0 {
        return None;
    }

    let mut origin = POINT::default();
    let ok = unsafe { ClientToScreen(hwnd, &mut origin) };
    if !ok.as_bool() {
        return None;
    }

    let (px, py, pw, ph) = clamp_rect(origin.x, origin.y, w, h, virtual_screen())?;
    capture_rect(px, py, pw, ph)
}

/// 屏幕物理矩形 → 截屏 → 近黑检测 → 统计 / JPEG data URL。
///
/// 面板取样与窗口取样共用；矩形由调用方负责夹到虚拟屏。
fn capture_rect(x: i32, y: i32, w: i32, h: i32) -> Option<GlassBackdrop> {
    if w > MAX_CAPTURE_SIDE || h > MAX_CAPTURE_SIDE {
        return None;
    }
    let (rgba, cw, ch) = snapshot::capture_rgba_max(x, y, w, h, MAX_CAPTURE_SIDE)?;
    if near_black_ratio(&rgba) > BLACK_RATIO {
        return None;
    }
    build_backdrop(rgba, cw as u32, ch as u32)
}

/// 窗口 DPI 缩放（物理 / CSS）：`GetDpiForWindow` 失败（返回 0）时回退 Tauri。
fn dpi_scale(window: &tauri::WebviewWindow, hwnd: HWND) -> f64 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi > 0 {
        return dpi as f64 / 96.0;
    }
    window.scale_factor().unwrap_or(1.0).max(0.25)
}

// ---------- 壁纸底片（顶栏 / Dock 这类常驻 chrome） ----------

/// `SystemParametersInfoW(SPI_GETDESKWALLPAPER)`：当前壁纸文件路径。
const SPI_GETDESKWALLPAPER: u32 = 0x0073;
const WALLPAPER_KEY: &str = r"Control Panel\Desktop";
/// 壁纸解码后的单边上限（8K 也够用，避免超大图吃内存）
const MAX_WALLPAPER_SIDE: u32 = 8192;

/// Windows 壁纸铺法（`WallpaperStyle` + `TileWallpaper`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WallpaperFit {
    Center,
    Stretch,
    Fit,
    Fill,
    Tile,
}

/// 虚拟屏像素 → 壁纸像素的映射；`None` = 该点落在壁纸外（用桌面背景色填充）。
#[derive(Debug, Clone, Copy, PartialEq)]
struct WallpaperMap {
    fit: WallpaperFit,
    img_w: f64,
    img_h: f64,
    off_x: f64,
    off_y: f64,
    scale_x: f64,
    scale_y: f64,
}

impl WallpaperMap {
    fn new(fit: WallpaperFit, img_w: f64, img_h: f64, screen_w: f64, screen_h: f64) -> Self {
        let (scale_x, scale_y, off_x, off_y) = match fit {
            WallpaperFit::Stretch => (screen_w / img_w, screen_h / img_h, 0.0, 0.0),
            WallpaperFit::Fit => {
                let s = (screen_w / img_w).min(screen_h / img_h);
                (s, s, (screen_w - img_w * s) / 2.0, (screen_h - img_h * s) / 2.0)
            }
            WallpaperFit::Fill => {
                let s = (screen_w / img_w).max(screen_h / img_h);
                (s, s, (screen_w - img_w * s) / 2.0, (screen_h - img_h * s) / 2.0)
            }
            WallpaperFit::Center => (
                1.0,
                1.0,
                (screen_w - img_w) / 2.0,
                (screen_h - img_h) / 2.0,
            ),
            WallpaperFit::Tile => (1.0, 1.0, 0.0, 0.0),
        };
        Self {
            fit,
            img_w,
            img_h,
            off_x,
            off_y,
            scale_x,
            scale_y,
        }
    }

    /// 虚拟屏坐标 → 壁纸像素坐标。
    fn to_image(&self, sx: f64, sy: f64) -> Option<(u32, u32)> {
        let ix = (sx - self.off_x) / self.scale_x;
        let iy = (sy - self.off_y) / self.scale_y;
        if self.fit == WallpaperFit::Tile {
            let x = ix.rem_euclid(self.img_w);
            let y = iy.rem_euclid(self.img_h);
            return Some((x as u32, y as u32));
        }
        if ix < 0.0 || iy < 0.0 || ix >= self.img_w || iy >= self.img_h {
            return None;
        }
        Some((ix as u32, iy as u32))
    }
}

fn wallpaper_path() -> Option<std::path::PathBuf> {
    let mut buf = vec![0u16; 1024];
    unsafe {
        SystemParametersInfoW(
            SYSTEM_PARAMETERS_INFO_ACTION(SPI_GETDESKWALLPAPER),
            buf.len() as u32,
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .ok()?;
    let s = String::from_utf16_lossy(&buf);
    let s = s.trim_end_matches(char::from(0)).trim();
    (!s.is_empty()).then(|| std::path::PathBuf::from(s))
}

/// 读 HKCU 下的字符串值（REG_SZ / REG_EXPAND_SZ）。
fn reg_string(subkey: &str, value: &str) -> Option<String> {
    let sub = common::to_wide(subkey);
    let val = common::to_wide(value);
    unsafe {
        let mut size = 0u32;
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
        let mut buf = vec![0u8; size as usize + 2];
        let mut got = buf.len() as u32;
        let r = RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            PCWSTR(val.as_ptr()),
            flags,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut got),
        );
        if r != ERROR_SUCCESS {
            return None;
        }
        buf.truncate((got as usize).min(buf.len()));
        let wide: Vec<u16> = buf
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let s = String::from_utf16_lossy(&wide);
        Some(s.trim_end_matches(char::from(0)).trim().to_string())
    }
}

fn wallpaper_fit() -> WallpaperFit {
    let tile = reg_string(WALLPAPER_KEY, "TileWallpaper")
        .map(|s| s.trim() == "1")
        .unwrap_or(false);
    if tile {
        return WallpaperFit::Tile;
    }
    match reg_string(WALLPAPER_KEY, "WallpaperStyle").and_then(|s| s.trim().parse::<u32>().ok()) {
        Some(0) => WallpaperFit::Center,
        Some(2) => WallpaperFit::Stretch,
        Some(6) => WallpaperFit::Fit,
        Some(10) | Some(22) => WallpaperFit::Fill,
        _ => WallpaperFit::Fill,
    }
}

/// 桌面纯色背景（fit / center 留边处显示的颜色）。
fn desktop_background() -> [u8; 3] {
    const DEFAULT: [u8; 3] = [0, 0, 0];
    let Some(s) = reg_string(r"Control Panel\Colors", "Background") else {
        return DEFAULT;
    };
    let mut parts = s.split_whitespace().map(|p| p.parse::<u16>().ok());
    match (parts.next(), parts.next(), parts.next()) {
        (Some(Some(r)), Some(Some(g)), Some(Some(b))) => {
            [r.min(255) as u8, g.min(255) as u8, b.min(255) as u8]
        }
        _ => DEFAULT,
    }
}

/// 用**壁纸**取样窗口客户区，作为顶栏 / Dock 的玻璃底片。
///
/// 为什么不用屏幕截屏：顶栏 / Dock 是常驻 chrome，底片在一次启动里只取一次；
/// 启动瞬间屏幕上很可能正好压着最大化窗口（甚至是刚启动时的前台窗口标题栏），
/// 截屏就会把那条纯色烘进玻璃——表现为「顶栏变成一整条纯色、玻璃感消失」。
/// 壁纸与窗口无关，既符合设计意图（玻璃透出壁纸），也不会跟着打开窗口的底色变。
pub fn wallpaper_backdrop(window: &tauri::WebviewWindow) -> Option<GlassBackdrop> {
    let hwnd = common::tauri_hwnd(window)?;
    let mut client = RECT::default();
    unsafe { GetClientRect(hwnd, &mut client) }.ok()?;
    let mut origin = POINT::default();
    if !unsafe { ClientToScreen(hwnd, &mut origin) }.as_bool() {
        return None;
    }
    let vscreen = virtual_screen();
    let (px, py, pw, ph) = clamp_rect(origin.x, origin.y, client.right, client.bottom, vscreen)?;

    let path = wallpaper_path()?;
    let img = image::open(&path).ok()?;
    let img = if img.width() > MAX_WALLPAPER_SIDE || img.height() > MAX_WALLPAPER_SIDE {
        img.resize(
            MAX_WALLPAPER_SIDE,
            MAX_WALLPAPER_SIDE,
            image::imageops::FilterType::Triangle,
        )
    } else {
        img
    };
    let img = img.to_rgba8();
    let (iw, ih) = (img.width() as f64, img.height() as f64);
    if iw <= 0.0 || ih <= 0.0 {
        return None;
    }
    if pw <= 0 || ph <= 0 {
        return None;
    }
    let map = WallpaperMap::new(wallpaper_fit(), iw, ih, vscreen.2 as f64, vscreen.3 as f64);
    let rgba = compose_wallpaper_crop(&img, &map, vscreen.0, vscreen.1, px, py, pw, ph, desktop_background());
    build_backdrop(rgba, pw as u32, ph as u32)
}

/// 按壁纸映射把 `rect`（虚拟屏坐标）裁成 RGBA（不缩放，交给 `build_backdrop`）。
fn compose_wallpaper_crop(
    img: &image::RgbaImage,
    map: &WallpaperMap,
    vsx: i32,
    vsy: i32,
    px: i32,
    py: i32,
    pw: i32,
    ph: i32,
    bg: [u8; 3],
) -> Vec<u8> {
    let (cw, ch) = (pw.max(0) as u32, ph.max(0) as u32);
    let mut rgba = vec![0u8; (cw * ch * 4) as usize];
    for row in 0..ch {
        for col in 0..cw {
            let sx = (px - vsx) as f64 + col as f64;
            let sy = (py - vsy) as f64 + row as f64;
            let color = match map.to_image(sx, sy) {
                Some((ix, iy)) => {
                    let p = img.get_pixel(ix, iy);
                    [p[0], p[1], p[2]]
                }
                None => bg,
            };
            let off = ((row * cw + col) * 4) as usize;
            rgba[off] = color[0];
            rgba[off + 1] = color[1];
            rgba[off + 2] = color[2];
            rgba[off + 3] = 255;
        }
    }
    rgba
}

/// 桌面虚拟屏（物理像素）：(left, top, width, height)。多屏时 left/top 可为负。
fn virtual_screen() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

/// CSS 矩形（相对客户区 0,0）→ 屏幕物理矩形。
///
/// 偏移与尺寸分别按 DPI 缩放后四舍五入：先缩放再平移，避免把原点的小数部分
/// 带进尺寸里；与 CSS 布局的取整方式一致。
pub fn physical_rect(
    origin_x: i32,
    origin_y: i32,
    scale: f64,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Option<(i32, i32, i32, i32)> {
    if !scale.is_finite() || scale <= 0.0 || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let left = origin_x + (x * scale).round() as i32;
    let top = origin_y + (y * scale).round() as i32;
    // f64 → i32 在 Rust 里是饱和转换，超大值不会 UB
    let w = (width * scale).round() as i32;
    let h = (height * scale).round() as i32;
    (w > 0 && h > 0).then_some((left, top, w, h))
}

/// 把物理矩形夹到虚拟屏范围（bounds = 虚拟屏 left/top/width/height）。
///
/// 裁剪后任一边小于 [`MIN_SIDE`] 返回 None（面板完全在屏外 / 只剩一条边）。
pub fn clamp_rect(
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    bounds: (i32, i32, i32, i32),
) -> Option<(i32, i32, i32, i32)> {
    let (bx, by, bw, bh) = bounds;
    if w <= 0 || h <= 0 || bw <= 0 || bh <= 0 {
        return None;
    }
    let left = x.max(bx);
    let top = y.max(by);
    let right = (x + w).min(bx + bw);
    let bottom = (y + h).min(by + bh);
    let cw = right - left;
    let ch = bottom - top;
    (cw >= MIN_SIDE && ch >= MIN_SIDE).then_some((left, top, cw, ch))
}

/// 按宽 <= `max_w` 等比缩小，返回 (宽, 高)。
///
/// 已经够窄就原样返回；极端窄高（比如 4096x1）缩放后最细一边仍保留 1px，
/// 避免 0 尺寸让 `image::imageops::resize` 报错。
pub fn downscale_dims(w: u32, h: u32, max_w: u32) -> (u32, u32) {
    if w == 0 || h == 0 || max_w == 0 || w <= max_w {
        return (w, h);
    }
    let nh = (h as f64 * max_w as f64 / w as f64).round() as u32;
    (max_w, nh.max(1))
}

/// 近黑像素（各通道 < 8）占比。桌面被锁屏 / 全屏画面遮挡时接近 1.0。
pub fn near_black_ratio(rgba: &[u8]) -> f32 {
    let total = rgba.len() / 4;
    if total == 0 {
        return 1.0;
    }
    let black = rgba
        .chunks_exact(4)
        .filter(|px| px[0] < BLACK_CHANNEL && px[1] < BLACK_CHANNEL && px[2] < BLACK_CHANNEL)
        .count();
    black as f32 / total as f32
}

/// 平均 RGB（u8，向下取整）；alpha 不参与。
pub fn mean_rgb(rgba: &[u8]) -> [u8; 3] {
    let total = rgba.len() / 4;
    if total == 0 {
        return [0, 0, 0];
    }
    let (mut r, mut g, mut b) = (0u64, 0u64, 0u64);
    for px in rgba.chunks_exact(4) {
        r += u64::from(px[0]);
        g += u64::from(px[1]);
        b += u64::from(px[2]);
    }
    let n = total as u64;
    [(r / n) as u8, (g / n) as u8, (b / n) as u8]
}

/// sRGB 8bit 通道 → 线性光（相对亮度公式里用的那种线性化）。
fn srgb_to_linear(c: u8) -> f32 {
    let v = c as f32 / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// 256-bin 线性相对亮度直方图：先逐通道 sRGB→linear，
/// 再按 Y = 0.2126R + 0.7152G + 0.0722B 合成，bin = round(Y*255)。
pub fn luma_histogram(rgba: &[u8]) -> [u64; 256] {
    // 通道只有 256 种取值，先建查找表避免每像素三次 powf
    let lut: [f32; 256] = std::array::from_fn(|i| srgb_to_linear(i as u8));
    let mut hist = [0u64; 256];
    for px in rgba.chunks_exact(4) {
        let y = 0.2126 * lut[px[0] as usize]
            + 0.7152 * lut[px[1] as usize]
            + 0.0722 * lut[px[2] as usize];
        let bin = (y.clamp(0.0, 1.0) * 255.0).round() as usize;
        hist[bin.min(255)] += 1;
    }
    hist
}

/// 直方图分位（`frac` ∈ 0..1）：累计像素数首次达到 `total * frac` 的 bin，
/// 返回 bin 中心值 / 255（封顶 1.0）。空直方图返回 0。
pub fn percentile_from_histogram(hist: &[u64; 256], frac: f32) -> f32 {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return 0.0;
    }
    let target = ((total as f64) * f64::from(frac.clamp(0.0, 1.0))).ceil().max(1.0) as u64;
    let mut acc = 0u64;
    for (i, count) in hist.iter().enumerate() {
        acc += count;
        if acc >= target {
            return ((i as f32 + 0.5) / 255.0).min(1.0);
        }
    }
    1.0
}

/// 截屏 RGBA → 宽度不超过 `max_w` 的小图（Triangle 采样，保持比例）。
fn shrink(rgba: Vec<u8>, w: u32, h: u32, max_w: u32) -> Option<image::RgbaImage> {
    let img = image::RgbaImage::from_raw(w, h, rgba)?;
    let (tw, th) = downscale_dims(w, h, max_w);
    if tw == 0 || th == 0 {
        return None;
    }
    if tw == w && th == h {
        return Some(img);
    }
    Some(image::imageops::resize(
        &img,
        tw,
        th,
        image::imageops::FilterType::Triangle,
    ))
}

/// 截屏 RGBA → 玻璃取样结果（统计 + JPEG data URL）。纯 CPU，不碰 Win32。
pub fn build_backdrop(rgba: Vec<u8>, w: u32, h: u32) -> Option<GlassBackdrop> {
    let small = shrink(rgba, w, h, MAX_WIDTH)?;
    let mean = mean_rgb(small.as_raw());
    let hist = luma_histogram(small.as_raw());
    let p05 = percentile_from_histogram(&hist, 0.05);
    let p95 = percentile_from_histogram(&hist, 0.95);

    // JPEG 不支持 alpha：先转 RGB 再编码（encode 直接吃 Rgba8 会报 Unsupported）
    let rgb = image::DynamicImage::ImageRgba8(small).into_rgb8();
    let mut jpg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, JPEG_QUALITY)
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .ok()?;
    Some(GlassBackdrop {
        data_url: format!("data:image/jpeg;base64,{}", STANDARD.encode(jpg)),
        p05,
        p95,
        mean,
        width: rgb.width(),
        height: rgb.height(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscale_dims_keeps_ratio() {
        // 常见面板：宽超限 → 按 480 等比缩
        assert_eq!(downscale_dims(1024, 1536, 480), (480, 720));
        assert_eq!(downscale_dims(840, 1208, 480), (480, 690));
        // 已经够窄：原样返回
        assert_eq!(downscale_dims(300, 120, 480), (300, 120));
        assert_eq!(downscale_dims(480, 900, 480), (480, 900));
        // 极端窄高：最细一边也保留 1px，不能出现 0
        assert_eq!(downscale_dims(4096, 1, 480), (480, 1));
        // 退化输入不 panic
        assert_eq!(downscale_dims(0, 0, 480), (0, 0));
        assert_eq!(downscale_dims(100, 100, 0), (100, 100));
    }

    #[test]
    fn percentile_from_histogram_on_known_ramp() {
        // 每个 bin 恰好 1 个像素（总 256）：5% → 13 个像素落在 bin 12（中心 12.5）
        let mut ramp = [0u64; 256];
        for slot in ramp.iter_mut() {
            *slot = 1;
        }
        assert!((percentile_from_histogram(&ramp, 0.05) - 12.5 / 255.0).abs() < 1e-6);
        assert!((percentile_from_histogram(&ramp, 0.95) - 243.5 / 255.0).abs() < 1e-6);

        // 所有像素同一个 bin：分位就落在这个 bin
        let mut flat = [0u64; 256];
        flat[100] = 1000;
        assert!((percentile_from_histogram(&flat, 0.05) - 100.5 / 255.0).abs() < 1e-6);
        assert!((percentile_from_histogram(&flat, 0.95) - 100.5 / 255.0).abs() < 1e-6);

        // 全黑 → bin 0 中心；全白封顶 1.0
        let mut black = [0u64; 256];
        black[0] = 7;
        let mut white = [0u64; 256];
        white[255] = 7;
        assert!((percentile_from_histogram(&black, 0.95) - 0.5 / 255.0).abs() < 1e-6);
        assert_eq!(percentile_from_histogram(&white, 0.95), 1.0);
        // 空直方图
        assert_eq!(percentile_from_histogram(&[0u64; 256], 0.5), 0.0);
    }

    #[test]
    fn mean_rgb_averages_rgb_only() {
        let px = [10u8, 20, 30, 255, 20, 40, 60, 255];
        assert_eq!(mean_rgb(&px), [15, 30, 45]);
        // 向下取整
        let px = [0u8, 0, 0, 255, 1, 1, 1, 255, 1, 1, 1, 255];
        assert_eq!(mean_rgb(&px), [0, 0, 0]);
        // alpha 不参与
        assert_eq!(mean_rgb(&[255u8, 255, 255, 0]), [255, 255, 255]);
        assert_eq!(mean_rgb(&[]), [0, 0, 0]);
    }

    #[test]
    fn luma_histogram_orders_black_gray_white() {
        let mut buf = Vec::new();
        for px in [[0u8, 0, 0, 255], [128, 128, 128, 255], [255, 255, 255, 255]] {
            buf.extend_from_slice(&px);
        }
        let hist = luma_histogram(&buf);
        assert_eq!(hist.iter().sum::<u64>(), 3);
        assert_eq!(hist[0], 1); // 黑
        assert_eq!(hist[55], 1); // sRGB 128 → 线性 ≈ 0.2159 → bin 55
        assert_eq!(hist[255], 1); // 白
    }

    #[test]
    fn clamp_rect_intersects_virtual_screen() {
        let screen = (0, 0, 1920, 1080);
        // 完全在屏内：原样返回
        assert_eq!(
            clamp_rect(100, 32, 480, 690, screen),
            Some((100, 32, 480, 690))
        );
        // 右侧越界 → 裁宽
        assert_eq!(
            clamp_rect(1800, 100, 480, 600, screen),
            Some((1800, 100, 120, 600))
        );
        // 副屏负坐标：左上越界 → 平移并裁高
        let desktop = (-1920, 0, 3840, 1080);
        assert_eq!(
            clamp_rect(-100, -50, 480, 690, desktop),
            Some((-100, 0, 480, 640))
        );
        // 完全在屏外 / 裁完不足 16px → None
        assert_eq!(clamp_rect(3000, 100, 480, 690, screen), None);
        assert_eq!(clamp_rect(1910, 100, 480, 690, screen), None);
        // 非法输入
        assert_eq!(clamp_rect(10, 10, 0, 690, screen), None);
        assert_eq!(clamp_rect(10, 10, 480, 690, (0, 0, 0, 0)), None);
    }

    #[test]
    fn physical_rect_applies_dpi_and_origin() {
        // 100%：原点 (0,32) + CSS 偏移
        assert_eq!(
            physical_rect(0, 32, 1.0, 0.0, 0.0, 480.0, 690.0),
            Some((0, 32, 480, 690))
        );
        // 150%：偏移与尺寸分别缩放后取整（333*1.5=499.5 → 500）
        assert_eq!(
            physical_rect(-1920, 0, 1.5, 10.0, 20.0, 333.0, 444.0),
            Some((-1905, 30, 500, 666))
        );
        // 非法缩放 / 尺寸
        assert_eq!(physical_rect(0, 0, 0.0, 0.0, 0.0, 10.0, 10.0), None);
        assert_eq!(physical_rect(0, 0, 1.0, 0.0, 0.0, 0.0, 10.0), None);
    }

    /// 事件/命令负载契约：字段名必须是前端约定的 camelCase
    #[test]
    fn glass_backdrop_serializes_camel_case() {
        let bd = GlassBackdrop {
            data_url: "data:image/jpeg;base64,x".into(),
            p05: 0.12,
            p95: 0.74,
            mean: [1, 2, 3],
            width: 4,
            height: 5,
        };
        let json = serde_json::to_value(&bd).expect("序列化失败");
        assert_eq!(json["dataUrl"], "data:image/jpeg;base64,x");
        // f32 → JSON 存的是 f64 精确值（0.119999997…），按 f32 比较
        let p05 = json["p05"].as_f64().expect("p05 缺失") as f32;
        let p95 = json["p95"].as_f64().expect("p95 缺失") as f32;
        assert!((p05 - 0.12).abs() < 1e-6, "p05={p05}");
        assert!((p95 - 0.74).abs() < 1e-6, "p95={p95}");
        assert_eq!(json["mean"], serde_json::json!([1, 2, 3]));
        assert_eq!(json["width"], 4);
        assert_eq!(json["height"], 5);
        // 不能漏出 snake_case 键
        assert!(json.get("data_url").is_none());
    }

    /// 超大矩形在进入 GDI 之前就被拒（不分配 GB 级缓冲）
    #[test]
    fn capture_rect_rejects_oversize_before_gdi() {
        assert!(capture_rect(0, 0, MAX_CAPTURE_SIDE + 1, 100).is_none());
        assert!(capture_rect(0, 0, 100, MAX_CAPTURE_SIDE + 1).is_none());
    }

    /// 真实桌面冒烟：手动 `cargo test --lib smoke_glass_backdrop -- --ignored --nocapture`
    /// 需要可见桌面（锁屏 / 无会话时会失败），PNG 落到 src-tauri/target/tmp 便于肉眼确认。
    #[test]
    #[ignore = "需要真实桌面"]
    fn smoke_glass_backdrop() {
        use windows::Win32::UI::WindowsAndMessaging::{SM_CXSCREEN, SM_CYSCREEN};

        // 面板典型尺寸：420x604（屏幕中心取样，避开任务栏与顶栏）
        let (sw, sh) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        let w = 420.min(sw);
        let h = 604.min(sh);
        assert!(w >= MIN_SIDE && h >= MIN_SIDE, "屏幕太小：{sw}x{sh}");
        let x = (sw - w) / 2;
        let y = (sh - h) / 2;

        let (rgba, cw, ch) = snapshot::capture_rgba_max(x, y, w, h, MAX_CAPTURE_SIDE)
            .unwrap_or_else(|| panic!("截屏失败（{x},{y} {w}x{h}）"));
        let ratio = near_black_ratio(&rgba);
        assert!(ratio < BLACK_RATIO, "屏幕中心几乎全黑（ratio={ratio:.3}）");

        let small = shrink(rgba.clone(), cw as u32, ch as u32, MAX_WIDTH).expect("缩图失败");
        let out = build_backdrop(rgba, cw as u32, ch as u32).expect("编码失败");
        // 共享管线（window_backdrop 走这里）也要能出图：尺寸必须一致，
        // 平均色不比较（两次截屏之间桌面可能变化：视频/动画）
        let via_rect = capture_rect(x, y, w, h).expect("capture_rect 失败");
        assert_eq!((via_rect.width, via_rect.height), (out.width, out.height));

        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/tmp");
        std::fs::create_dir_all(&dir).expect("创建 target/tmp 失败");
        let png = dir.join("glass-smoke.png");
        image::DynamicImage::ImageRgba8(small)
            .save(&png)
            .expect("保存 PNG 失败");

        println!(
            "冒烟：屏幕中心 ({x},{y}) {}x{} → {}x{}，近黑 {:.1}%，mean={:?} p05={:.3} p95={:.3}",
            cw,
            ch,
            out.width,
            out.height,
            ratio * 100.0,
            out.mean,
            out.p05,
            out.p95
        );
        println!("dataUrl 前缀：{}…", out.data_url.chars().take(48).collect::<String>());
        println!("PNG：{}", png.display());
        assert!(!out.data_url.is_empty());
        assert!(out.p05 <= out.p95, "分位应单调：p05={} p95={}", out.p05, out.p95);
    }
}

#[cfg(test)]
mod wallpaper_tests {
    use super::{WallpaperFit, WallpaperMap};

    fn map(fit: WallpaperFit, iw: f64, ih: f64, sw: f64, sh: f64) -> WallpaperMap {
        WallpaperMap::new(fit, iw, ih, sw, sh)
    }

    #[test]
    fn fill_scales_to_cover_and_crops_overflow() {
        // 1000×500 壁纸铺到 800×600：按 max(0.8, 1.2)=1.2 放大到 1200×600，左右各溢出 200
        let m = map(WallpaperFit::Fill, 1000.0, 500.0, 800.0, 600.0);
        assert!((m.off_x + 200.0).abs() < 1e-9, "off_x={}", m.off_x);
        assert!(m.off_y.abs() < 1e-9, "off_y={}", m.off_y);
        assert_eq!(m.to_image(0.0, 0.0), Some((166, 0)));
        assert_eq!(m.to_image(799.0, 599.0), Some((832, 499)));
    }

    #[test]
    fn fit_letterboxes_with_background() {
        // 1000×500 铺到 800×600：按 min(0.8, 1.2)=0.8 缩小到 800×400，上下各留 100
        let m = map(WallpaperFit::Fit, 1000.0, 500.0, 800.0, 600.0);
        assert!((m.off_y - 100.0).abs() < 1e-9, "off_y={}", m.off_y);
        assert_eq!(m.to_image(10.0, 20.0), None, "上留边应落在壁纸外");
        assert_eq!(m.to_image(10.0, 150.0), Some((12, 62)));
        assert_eq!(m.to_image(10.0, 580.0), None, "下留边应落在壁纸外");
    }

    #[test]
    fn tile_wraps_instead_of_clamping() {
        let m = map(WallpaperFit::Tile, 100.0, 50.0, 800.0, 600.0);
        assert_eq!(m.to_image(0.0, 0.0), Some((0, 0)));
        assert_eq!(m.to_image(150.0, 60.0), Some((50, 10)));
        assert_eq!(m.to_image(250.0, 120.0), Some((50, 20)));
    }

    #[test]
    fn stretch_and_center() {
        let stretch = map(WallpaperFit::Stretch, 1000.0, 500.0, 800.0, 600.0);
        assert!((stretch.scale_x - 0.8).abs() < 1e-9);
        assert!((stretch.scale_y - 1.2).abs() < 1e-9);
        assert_eq!(stretch.to_image(400.0, 300.0), Some((500, 250)));

        let center = map(WallpaperFit::Center, 200.0, 100.0, 800.0, 600.0);
        assert_eq!(center.to_image(300.0, 250.0), Some((0, 0)));
        assert_eq!(center.to_image(500.0, 250.0), None, "超出居中壁纸的部分");
    }
}

#[cfg(test)]
mod wallpaper_real_tests {
    /// 真机冒烟（手动）：读真实壁纸 / WallpaperStyle / 虚拟屏，算出顶栏条的底片并打印统计。
    /// `cargo test --lib wallpaper_real_smoke -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn wallpaper_real_smoke() {
        let Some(path) = super::wallpaper_path() else {
            println!("没有壁纸文件（纯色桌面），应当回退截屏");
            return;
        };
        let img = image::open(&path).expect("壁纸解码失败");
        let img = img.to_rgba8();
        let vs = super::virtual_screen();
        let fit = super::wallpaper_fit();
        let map = super::WallpaperMap::new(
            fit,
            img.width() as f64,
            img.height() as f64,
            vs.2 as f64,
            vs.3 as f64,
        );
        let (px, py, pw, ph) = (vs.0, vs.1, 1920.min(vs.2), 32);
        let rgba = super::compose_wallpaper_crop(
            &img,
            &map,
            vs.0,
            vs.1,
            px,
            py,
            pw,
            ph,
            super::desktop_background(),
        );
        let out = super::build_backdrop(rgba, pw as u32, ph as u32).expect("编码失败");
        println!(
            "壁纸 {path:?} {}x{} fit={fit:?} vs={vs:?} rect=({px},{py},{pw},{ph}) mean={:?} p05={:.3} p95={:.3} 小图={}x{}",
            img.width(),
            img.height(),
            out.mean,
            out.p05,
            out.p95,
            out.width,
            out.height
        );
        assert!(!out.data_url.is_empty());
    }
}

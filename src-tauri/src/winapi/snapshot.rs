//! 窗口截图（PrintWindow）→ JPEG data URL，用于概览缩略图
#![allow(non_upper_case_globals)]

use base64::{engine::general_purpose::STANDARD, Engine as _};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
    GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HGDIOBJ,
    SRCCOPY,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

/// 抓取窗口当前画面，缩放到 max_w 宽以内，返回 JPEG data URL。
/// 内容全黑（硬件加速窗口无法抓取）时返回 None → 前端回退为图标卡片。
/// 最小化窗口也尝试抓取：Win11 上 PW_RENDERFULLCONTENT 对许多最小化窗口
/// 仍能取到 Z 序最后一帧画面。
pub fn capture_jpeg(hwnd: HWND, max_w: u32) -> Option<String> {
    unsafe {
        let mut rect = RECT::default();
        let hr = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut _ as *mut _,
            std::mem::size_of::<RECT>() as u32,
        );
        if hr.is_err() {
            let _ = GetWindowRect(hwnd, &mut rect);
        }
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        if w < 80 || h < 60 || w > 8000 || h > 8000 {
            return None;
        }

        let hdc_screen = GetDC(None);
        let mem = CreateCompatibleDC(hdc_screen);
        let bmp = CreateCompatibleBitmap(hdc_screen, w, h);
        let old = SelectObject(mem, HGDIOBJ(bmp.0));
        let ok = PrintWindow(hwnd, mem, PRINT_WINDOW_FLAGS(2));

        let mut result = None;
        if ok.as_bool() {
            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = w;
            bmi.bmiHeader.biHeight = -h;
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = 0; // BI_RGB

            let mut buf = vec![0u8; (w * h * 4) as usize];
            if GetDIBits(
                mem,
                bmp,
                0,
                h as u32,
                Some(buf.as_mut_ptr() as *mut _),
                &mut bmi,
                DIB_RGB_COLORS,
            ) != 0
            {
                // 黑屏检测：>99% 近黑像素视为抓取失败
                let total = buf.len() / 4;
                let mut black = 0usize;
                for px in buf.chunks_exact(4) {
                    if px[0] < 8 && px[1] < 8 && px[2] < 8 {
                        black += 1;
                    }
                }
                if black < total * 99 / 100 {
                    for px in buf.chunks_exact_mut(4) {
                        px.swap(0, 2); // BGRA → RGBA
                    }
                    let mut img =
                        image::RgbaImage::from_raw(w as u32, h as u32, buf).unwrap();
                    if w as u32 > max_w {
                        let nh = ((h as f64) * (max_w as f64 / w as f64)).round() as u32;
                        img = image::imageops::resize(
                            &img,
                            max_w,
                            nh.max(1),
                            image::imageops::FilterType::Triangle,
                        );
                    }
                    let mut jpg = Vec::new();
                    if image::DynamicImage::ImageRgba8(img)
                        .write_to(&mut std::io::Cursor::new(&mut jpg), image::ImageFormat::Jpeg)
                        .is_ok()
                    {
                        result = Some(format!(
                            "data:image/jpeg;base64,{}",
                            STANDARD.encode(jpg)
                        ));
                    }
                }
            }
        }

        SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem);
        let _ = ReleaseDC(None, hdc_screen);
        result
    }
}

/// 抓取屏幕指定区域（物理像素）→ PNG data URL（用于顶栏托盘图标镜像）。
///
/// 只截托盘图标的 16~48px 小区块，因此对区域大小做上限保护（>512 视为非法）。
/// 进程必须是 Per-Monitor DPI 感知（Tauri 默认），否则 BitBlt 的屏幕坐标会被
/// 系统按逻辑像素缩放，与 UIA 给出的物理像素 rect 对不上。
pub fn capture_screen_region_png(x: i32, y: i32, w: i32, h: i32) -> Option<String> {
    if w <= 0 || h <= 0 || w > 512 || h > 512 {
        return None;
    }
    unsafe {
        let hdc_screen = GetDC(None);
        if hdc_screen.is_invalid() {
            return None;
        }
        let mem = CreateCompatibleDC(hdc_screen);
        let bmp = CreateCompatibleBitmap(hdc_screen, w, h);
        if mem.is_invalid() || bmp.is_invalid() {
            // 兜底释放：正常几乎不会走到
            if !bmp.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(bmp.0));
            }
            if !mem.is_invalid() {
                let _ = DeleteDC(mem);
            }
            let _ = ReleaseDC(None, hdc_screen);
            return None;
        }

        let old = SelectObject(mem, HGDIOBJ(bmp.0));
        let copied = BitBlt(mem, 0, 0, w, h, hdc_screen, x, y, SRCCOPY).is_ok();
        // GetDIBits 要求位图未被选入 DC，先恢复旧对象再取像素
        SelectObject(mem, old);

        let mut result = None;
        if copied {
            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = w;
            bmi.bmiHeader.biHeight = -h; // 负高度 = 自顶向下
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = 0; // BI_RGB

            let mut buf = vec![0u8; (w * h * 4) as usize];
            if GetDIBits(
                mem,
                bmp,
                0,
                h as u32,
                Some(buf.as_mut_ptr() as *mut _),
                &mut bmi,
                DIB_RGB_COLORS,
            ) != 0
            {
                // BGRA → RGBA；屏幕截图不带 alpha（BitBlt 填 0），
                // 这里统一改成不透明，否则顶栏上的图标会变成全透明
                for px in buf.chunks_exact_mut(4) {
                    px.swap(0, 2);
                    px[3] = 255;
                }
                result = rgba_png_data_url(buf, w, h);
            }
        }

        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem);
        let _ = ReleaseDC(None, hdc_screen);
        result
    }
}

/// RGBA 像素 → PNG data URL（截图取图标专用；不走 image 的 panic 路径）
fn rgba_png_data_url(buf: Vec<u8>, w: i32, h: i32) -> Option<String> {
    if w <= 0 || h <= 0 || buf.len() != (w as usize) * (h as usize) * 4 {
        return None;
    }
    let img = image::RgbaImage::from_raw(w as u32, h as u32, buf)?;
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(format!("data:image/png;base64,{}", STANDARD.encode(png)))
}


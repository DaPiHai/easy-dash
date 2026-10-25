//! Windows 打包应用（UWP / 商店应用）枚举。
//!
//! 通过 WinRT `PackageManager::FindPackages()` → `Package::GetAppListEntriesAsync()`
//! 拿到**已本地化**的应用名与 Logo，启动目标为 `shell:AppsFolder\<AUMID>`。
//!
//! 调用线程必须已初始化 COM / WinRT（`apps.rs` 的工作线程已做
//! CoInitializeEx(MTA) + RoInitialize(MTA)）；这里再容错调一次，重复初始化无害。
//! 所有 WinRT 调用都按 Result/Option 兜底：单个包 / 条目 / logo 失败只跳过该条，
//! 不会让整体枚举退化为空列表，也不会 panic。
#![allow(dead_code)]

use std::collections::HashSet;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use windows::ApplicationModel::Core::AppListEntry;
use windows::ApplicationModel::{AppDisplayInfo, Package};
use windows::core::HSTRING;
use windows::Foundation::Collections::IIterable;
use windows::Foundation::Size;
use windows::Management::Deployment::PackageManager;
use windows::Storage::Streams::DataReader;

use crate::types::AppEntry;

/// 图标请求尺寸（应用中心卡片 48px；GetLogo 会返回最接近的可用资源）
const LOGO_SIZE: f32 = 48.0;
/// 单张 logo 读取上限：1MB（48px 图标远小于此，仅防御异常资源）
const MAX_ICON_BYTES: u64 = 1024 * 1024;
/// 启动目标前缀
const APPS_FOLDER: &str = "shell:AppsFolder\\";

/// 枚举当前用户的打包应用（只返回带 Start 菜单入口、名称可读的条目）。
pub fn enumerate() -> Vec<AppEntry> {
    // 容错：WinRT 必须在已初始化 COM 的线程上使用（调用方通常已初始化）
    crate::controls::ensure_com();

    let manager = match PackageManager::new() {
        Ok(manager) => manager,
        Err(e) => {
            eprintln!("[packaged-apps] PackageManager::new 失败：{e}");
            return Vec::new();
        }
    };
    let packages = match find_packages(&manager) {
        Ok(packages) => packages,
        Err(e) => {
            eprintln!("[packaged-apps] 枚举包失败：{e}");
            return Vec::new();
        }
    };

    // IIterable 的 IntoIterator 内部会 unwrap First() 的结果；这里手工迭代，
    // 让「取迭代器失败 / 推进失败」也能优雅退出而不是 panic。
    let iterator = match packages.First() {
        Ok(iterator) => iterator,
        Err(e) => {
            eprintln!("[packaged-apps] 打开包迭代器失败：{e}");
            return Vec::new();
        }
    };

    let mut out: Vec<AppEntry> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut package_count = 0usize;
    loop {
        // 空集合时 Current() 会失败：跳过，交给 MoveNext 判断结束
        if let Ok(package) = iterator.Current() {
            package_count += 1;
            collect_package(&package, &mut out, &mut seen);
        }
        match iterator.MoveNext() {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                eprintln!("[packaged-apps] 推进包迭代器失败：{e}");
                break;
            }
        }
    }

    eprintln!(
        "[packaged-apps] 遍历 {package_count} 个包，枚举到 {} 个打包应用",
        out.len()
    );
    out
}

/// 取出当前用户的包集合。
///
/// 实测注意：在 Win11 26H2 的非打包桌面进程里 `FindPackages()` 会返回
/// E_ACCESSDENIED（0x80070005），而 `FindPackagesByUserSecurityId("")`
/// （空 SID = 当前用户，与 WinRT `FindPackagesForUser("")` 同一个方法）
/// 仍可正常枚举；因此这里「SID 变体优先、标准调用兜底」，两者都失败才返回 Err。
fn find_packages(manager: &PackageManager) -> windows::core::Result<IIterable<Package>> {
    let current_user = HSTRING::from("");
    match manager.FindPackagesByUserSecurityId(&current_user) {
        Ok(packages) => Ok(packages),
        Err(e) => {
            eprintln!("[packaged-apps] FindPackagesByUserSecurityId 失败：{e}；回退 FindPackages");
            manager.FindPackages()
        }
    }
}

/// 处理一个包：遍历它的 Start 菜单入口（框架包/资源包没有入口，自然为空）
fn collect_package(package: &Package, out: &mut Vec<AppEntry>, seen: &mut HashSet<String>) {
    // 框架包 / 资源包 / 捆绑包一定没有 Start 菜单入口：提前跳过，省一次异步调用。
    // 查询失败时按「不是」处理，继续走正常流程（容错优先）。
    if package.IsFramework().unwrap_or(false) || package.IsResourcePackage().unwrap_or(false) {
        return;
    }

    let entries = match package.GetAppListEntriesAsync() {
        Ok(operation) => match operation.get() {
            Ok(entries) => entries,
            Err(e) => {
                eprintln!("[packaged-apps] 等待 AppListEntry 失败：{e}");
                return;
            }
        },
        Err(e) => {
            // 个别包不允许查询入口（框架包等）：跳过即可
            eprintln!("[packaged-apps] GetAppListEntriesAsync 失败：{e}");
            return;
        }
    };

    let count = match entries.Size() {
        Ok(count) => count,
        Err(e) => {
            eprintln!("[packaged-apps] 读取入口数量失败：{e}");
            return;
        }
    };
    for index in 0..count {
        let Ok(entry) = entries.GetAt(index) else {
            continue;
        };
        if let Some(app) = entry_to_app(&entry, seen) {
            out.push(app);
        }
    }
}

/// 把一个 AppListEntry 转成 AppEntry；无 AUMID / 名称为空或未本地化时返回 None
fn entry_to_app(entry: &AppListEntry, seen: &mut HashSet<String>) -> Option<AppEntry> {
    let aumid = entry
        .AppUserModelId()
        .map(|s| s.to_string_lossy().trim().to_string())
        .unwrap_or_default();
    if aumid.is_empty() {
        return None;
    }
    // AUMID 去重（同一应用可能同时出现在多个包条目里）
    let dedupe_key = aumid.to_lowercase();
    if seen.contains(&dedupe_key) {
        return None;
    }

    let display_info = match entry.DisplayInfo() {
        Ok(info) => info,
        Err(e) => {
            eprintln!("[packaged-apps] DisplayInfo 失败（{aumid}）：{e}");
            return None;
        }
    };
    let name = display_info
        .DisplayName()
        .map(|s| s.to_string_lossy().trim().to_string())
        .unwrap_or_default();
    // "ms-resource:" 是未解析的资源占位符（通常框架包/无语言资源）：
    // 这种名字前端无法展示，直接跳过
    if name.is_empty() || name.starts_with("ms-resource:") {
        return None;
    }
    seen.insert(dedupe_key);

    // Logo 优先走 AppDisplayInfo.GetLogo（48px 官方图标），失败回退包清单图标
    let icon = logo_data_url(&display_info)
        .or_else(|| crate::winapi::icons::aumid_app_info(&aumid).map(|(url, _)| url));

    Some(AppEntry {
        id: format!("uwp:{aumid}"),
        name,
        icon,
        launch: format!("{APPS_FOLDER}{aumid}"),
        // 打包应用没有额外启动参数
        args: String::new(),
        kind: String::from("uwp"),
    })
}

/// AppDisplayInfo.GetLogo(48×48) → 读取流 → data URL；任何失败返回 None
fn logo_data_url(info: &AppDisplayInfo) -> Option<String> {
    let logo = info
        .GetLogo(Size {
            Width: LOGO_SIZE,
            Height: LOGO_SIZE,
        })
        .ok()?;
    let stream = logo.OpenReadAsync().ok()?.get().ok()?;
    let content_type = stream
        .ContentType()
        .ok()?
        .to_string_lossy()
        .to_ascii_lowercase();
    let size = stream.Size().ok()?;
    if size == 0 {
        return None;
    }

    // 截断到上限；LoadAsync 返回实际可读字节数
    let want = size.min(MAX_ICON_BYTES) as u32;
    let input = stream.GetInputStreamAt(0).ok()?;
    let reader = DataReader::CreateDataReader(&input).ok()?;
    let loaded = reader.LoadAsync(want).ok()?.get().ok()?;
    if loaded == 0 {
        return None;
    }
    let mut buf = vec![0u8; loaded as usize];
    reader.ReadBytes(&mut buf).ok()?;

    encode_image_data_url(&buf, &content_type)
}

/// 图片字节 → data URL：PNG/JPEG 原样透传（省一次编解码），其它格式解码后转 PNG。
fn encode_image_data_url(bytes: &[u8], content_type: &str) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    match content_type {
        "image/png" => Some(format!("data:image/png;base64,{}", STANDARD.encode(bytes))),
        "image/jpeg" | "image/jpg" => {
            Some(format!("data:image/jpeg;base64,{}", STANDARD.encode(bytes)))
        }
        _ => {
            // 其它格式（image/bmp 等，或 content type 缺失）：统一转 PNG
            let image = image::load_from_memory(bytes).ok()?;
            let mut png = Vec::new();
            image
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .ok()?;
            Some(format!("data:image/png;base64,{}", STANDARD.encode(png)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::encode_image_data_url;
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    /// 1×1 红色 RGBA PNG（纯内存生成，不依赖本机环境）
    fn tiny_png() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("生成 PNG 不应失败");
        png
    }

    #[test]
    fn png_is_passed_through() {
        let bytes = tiny_png();
        let url = encode_image_data_url(&bytes, "image/png").expect("PNG 应透传");
        let encoded = url
            .strip_prefix("data:image/png;base64,")
            .expect("PNG 前缀正确");
        assert_eq!(STANDARD.decode(encoded).unwrap(), bytes);
    }

    #[test]
    fn jpeg_content_type_passthrough() {
        // 只验证 content type 透传逻辑：JPEG 字节内容不做解码校验
        let fake = vec![0xFFu8, 0xD8, 0xFF, 0xD9];
        let url = encode_image_data_url(&fake, "image/jpeg").expect("JPEG 应透传");
        assert!(url.starts_with("data:image/jpeg;base64,"));
        // 大小写不同的 content type 已在调用方统一小写；这里再验证一次 jpg 别名
        assert!(encode_image_data_url(&fake, "image/jpg")
            .unwrap()
            .starts_with("data:image/jpeg;base64,"));
    }

    #[test]
    fn other_types_are_recoded_to_png() {
        // content type 缺失但字节是 PNG：image crate 解码后统一输出 PNG
        let bytes = tiny_png();
        let url = encode_image_data_url(&bytes, "").expect("应转码成 PNG");
        assert!(url.starts_with("data:image/png;base64,"));
        // 有 data URL 前缀但内容无法解码 → None（不 panic）
        assert!(encode_image_data_url(b"not an image", "image/bmp").is_none());
        assert!(encode_image_data_url(&[], "image/png").is_none());
    }

    /// 实机冒烟：打印本机打包应用数量与示例（依赖本机已安装的商店应用）。
    /// 手动验证：`cargo test --manifest-path src-tauri/Cargo.toml --lib -- --ignored smoke_packaged --nocapture`
    #[test]
    #[ignore = "依赖本机已安装的打包应用，手动执行"]
    fn smoke_packaged_apps() {
        crate::controls::ensure_com();
        let started = std::time::Instant::now();
        let apps = super::enumerate();
        eprintln!(
            "[smoke] packaged apps={} elapsed={}ms",
            apps.len(),
            started.elapsed().as_millis()
        );
        for app in apps.iter().take(12) {
            eprintln!(
                "[smoke] name={:?} icon_len={} id={}",
                app.name,
                app.icon.as_ref().map(|u| u.len()).unwrap_or(0),
                app.id
            );
        }
        assert!(
            !apps.is_empty(),
            "本机应能列出打包应用（为空可能是 PackageManager 权限/环境问题）"
        );
        // 名称必须是本地化名，不能是未解析的资源占位符
        assert!(
            apps.iter().all(|a| !a.name.starts_with("ms-resource:")),
            "不应出现未解析的 ms-resource 名称"
        );
        // id / launch / kind 契约检查
        assert!(apps.iter().all(|a| a.id.starts_with("uwp:")
            && a.launch.starts_with("shell:AppsFolder\\")
            && a.kind == "uwp"));
    }
}

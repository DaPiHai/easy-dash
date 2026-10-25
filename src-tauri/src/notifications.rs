//! 通知中心：读取 Windows 通知历史（wpndatabase.db）并解析为前端条目。
//!
//! 工作线程（STA + 消息泵）负责：接收刷新请求 → 读库 → 解析 toast XML →
//! 映射应用名/图标 → 写入 `AppState.notifications` 并 `emit("notifications-changed")`。
//!
//! 为什么单独开线程：图标提取走 Shell（SHGetFileInfo 等）与注册表/包清单读取，
//! 在无消息泵的线程上可能永久阻塞；绝不能放在 IPC 主线程或监控热路径上。
use std::collections::HashMap;
use std::sync::mpsc::Receiver;

use tauri::{AppHandle, Emitter, Manager};

use crate::app_state::AppState;
use crate::notify_db;
use crate::types::NotificationItem;
use crate::winapi;

/// 一次刷新最多读取/展示的条数
const MAX_ITEMS: usize = 30;
/// 图标缓存条目上限：超出后整体清空重建，避免长期运行无限增长
const ICON_CACHE_MAX: usize = 64;
/// FILETIME 的 Unix 纪元偏移（1601-01-01 → 1970-01-01，单位 100ns）
const EPOCH_DIFF_100NS: i64 = 116_444_736_000_000_000;

/// 启动通知中心工作线程（整个进程只调用一次）
pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        // STA + 消息泵：Shell 图标提取在 STA 线程上需要分派消息，
        // 否则跨进程调用可能永久阻塞（与 worker.rs 一致）
        winapi::icons::ensure_com();
        // 取出刷新请求的接收端（整个进程只取一次）
        let rx: Receiver<()> = {
            let state = app.state::<AppState>();
            let mut guard = match state.notif_rx.lock() {
                Ok(guard) => guard,
                // 锁中毒：放弃启动，不影响窗口枚举等核心功能
                Err(_) => return,
            };
            match guard.take() {
                Some(rx) => rx,
                None => return, // 已被别的线程接管（防重复启动）
            }
        };
        // 图标 / AUMID 显示名缓存：工作线程私有，无需加锁
        let mut icon_cache: HashMap<String, Option<String>> = HashMap::new();
        let mut name_cache: HashMap<String, String> = HashMap::new();
        while rx.recv().is_ok() {
            refresh(&app, &mut icon_cache, &mut name_cache);
            // 每轮处理完都泵一次消息，避免 STA 上挂起的 Shell 回调堆积
            pump_messages();
        }
    });
}

/// 请求刷新通知（非阻塞：投递到工作线程即可）
pub fn request_refresh(app: &AppHandle) {
    let _ = app.state::<AppState>().notif_tx.send(());
}

/// 执行一次刷新：读库 → 解析 → 写缓存 → 通知前端。失败只记日志，不 panic。
fn refresh(
    app: &AppHandle,
    icon_cache: &mut HashMap<String, Option<String>>,
    name_cache: &mut HashMap<String, String>,
) {
    let items = match build_items(icon_cache, name_cache) {
        Ok(items) => items,
        Err(e) => {
            // 读库失败（系统锁库 / 无权限 / 无历史）不该打断工作线程
            eprintln!("[notifications] {e}");
            return;
        }
    };
    // 写缓存：锁中毒时本轮放弃，不上报半截数据
    {
        let state = app.state::<AppState>();
        let mut guard = match state.notifications.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        *guard = items.clone();
    }
    let _ = app.emit("notifications-changed", &items);
}

/// 读库并把每一行解析成前端条目
fn build_items(
    icon_cache: &mut HashMap<String, Option<String>>,
    name_cache: &mut HashMap<String, String>,
) -> Result<Vec<NotificationItem>, String> {
    let (raw, handlers) = notify_db::load_recent(MAX_ITEMS)?;
    let mut items: Vec<NotificationItem> = Vec::with_capacity(raw.len());
    for r in &raw {
        // 没有文本内容的通知（纯进度条 / 媒体控制 / 纯图片）跳过
        let (title, body) = match extract_toast_text(&r.payload) {
            Some(t) => t,
            None => continue,
        };
        let (primary_id, handler_type) = match handlers.get(&r.handler_id) {
            Some(h) => (h.primary_id.as_str(), h.handler_type.as_str()),
            // 处理器表缺行（理论上不会）：仍展示，应用名回落「系统」
            None => ("", ""),
        };
        let (app_name, app_icon) =
            resolve_identity(primary_id, handler_type, icon_cache, name_cache);
        items.push(NotificationItem {
            id: r.id,
            app_name,
            app_icon,
            title,
            body,
            ts_ms: fil_time_to_unix_ms(r.arrival_ft),
        });
    }
    // 库内已按到达时间倒序；这里再排一次，保证前端展示顺序稳定
    items.sort_by(|a, b| b.ts_ms.cmp(&a.ts_ms));
    Ok(items)
}

/// 解析条目的应用显示名与图标：命中缓存就不再做 Shell / 包清单调用。
fn resolve_identity(
    primary_id: &str,
    handler_type: &str,
    icon_cache: &mut HashMap<String, Option<String>>,
    name_cache: &mut HashMap<String, String>,
) -> (String, Option<String>) {
    let mut name = app_name_from(primary_id, handler_type);
    let key = primary_id.trim();

    let icon = if let Some(hit) = icon_cache.get(key) {
        hit.clone()
    } else {
        let icon = if is_aumid(key) {
            // 打包应用：从包清单读真实图标；顺带拿到本地化显示名
            match winapi::icons::aumid_app_info(key) {
                Some((url, disp)) => {
                    if !disp.trim().is_empty() {
                        name_cache.insert(key.to_string(), disp);
                    }
                    Some(url)
                }
                None => None,
            }
        } else if looks_like_path(key) {
            winapi::icons::file_icon_data_url(key)
        } else {
            // "QQ" 这类只有处理器名、没有可执行文件路径：前端用字母占位
            None
        };
        // 简单限长：满了整体清空，实现简单且不会无限增长
        if icon_cache.len() >= ICON_CACHE_MAX {
            icon_cache.clear();
        }
        icon_cache.insert(key.to_string(), icon.clone());
        icon
    };

    // 包清单里的显示名（如「设置」）比 AUMID 前缀可读，优先使用
    if let Some(disp) = name_cache.get(key) {
        if !disp.trim().is_empty() {
            name = disp.clone();
        }
    }
    (name, icon)
}

/// Windows FILETIME（自 1601-01-01 起的 100ns 计数）→ Unix 毫秒。
/// 0 / 负数 / 早于 1970 年的异常值统一返回 0；不会 panic。
pub fn fil_time_to_unix_ms(ft: i64) -> u64 {
    if ft <= 0 {
        return 0;
    }
    let unix_100ns = ft.saturating_sub(EPOCH_DIFF_100NS);
    if unix_100ns <= 0 {
        return 0;
    }
    (unix_100ns / 10_000) as u64
}

/// 从 toast XML 中按出现顺序提取所有 `<text ...>内容</text>`：
/// 第一个非空文本作为标题，其余用 `\n` 连接为正文；没有任何文本返回 None。
///
/// payload 里还有 `<image>` / `<audio>` / `<binding>` 等标签，只认 `<text`，
/// 并排除 `<textbox>` 这类同前缀标签；内容做 XML 实体解码并去掉首尾空白。
pub fn extract_toast_text(payload: &str) -> Option<(String, String)> {
    // 用 ASCII 小写副本匹配标签：只改 ASCII 字节，长度与原文一致，下标可直接复用
    let lower = payload.to_ascii_lowercase();
    let mut texts: Vec<String> = Vec::new();
    let mut pos = 0usize;
    while let Some(rel) = lower[pos..].find("<text") {
        let open = pos + rel;
        // 标签名必须紧跟 '>' 或空白，排除 <textbox>/<textual> 之类
        let after_open = &lower[open + 5..];
        let is_text_tag = match after_open.chars().next() {
            None => true,
            Some(c) => c == '>' || c.is_ascii_whitespace(),
        };
        if !is_text_tag {
            pos = open + 5;
            continue;
        }
        // 找开标签的 '>'（跳过属性）
        let Some(gt_rel) = lower[open..].find('>') else {
            break; // 标签没闭合：payload 残缺，放弃后续
        };
        let tag_end = open + gt_rel;
        // 自闭合 <text ... /> 没有内容
        if payload[open..tag_end].trim_end().ends_with('/') {
            pos = tag_end + 1;
            continue;
        }
        let content_start = tag_end + 1;
        // 找结束标签 </text（允许 </text > 的写法）
        let Some(close_rel) = lower[content_start..].find("</text") else {
            break;
        };
        let close = content_start + close_rel;
        let text = decode_entities(&payload[content_start..close]);
        let text = text.trim();
        if !text.is_empty() {
            texts.push(text.to_string());
        }
        pos = close + "</text".len();
    }

    if texts.is_empty() {
        return None;
    }
    let title = texts.remove(0);
    let body = texts.join("\n");
    if title.is_empty() && body.is_empty() {
        return None;
    }
    Some((title, body))
}

/// 解码 XML 实体：命名实体（amp/lt/gt/quot/apos/nbsp，大小写不敏感）
/// 与数字实体（`&#123;` / `&#x1F600;`）。无法识别时原样保留。
pub fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(idx) = rest.find('&') {
        out.push_str(&rest[..idx]);
        rest = &rest[idx..];
        // '&' 是 ASCII，rest[1..] 一定落在字符边界上
        let decoded = match rest[1..].find(';') {
            Some(rel) => {
                let end = rel + 1; // 实体正文 rest[1..end]
                let entity = &rest[1..end];
                named_entity(entity)
                    .or_else(|| numeric_entity(entity))
                    .map(|c| (c, end + 1))
            }
            None => None, // 后面没有 ';'，不可能是完整实体
        };
        match decoded {
            Some((c, consumed)) => {
                out.push(c);
                rest = &rest[consumed..];
            }
            None => {
                // 不是可识别实体：保留 '&' 继续扫描
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// XML 命名实体（大小写不敏感，容忍手写 payload 的大小写差异）
fn named_entity(entity: &str) -> Option<char> {
    let c = if entity.eq_ignore_ascii_case("amp") {
        '&'
    } else if entity.eq_ignore_ascii_case("lt") {
        '<'
    } else if entity.eq_ignore_ascii_case("gt") {
        '>'
    } else if entity.eq_ignore_ascii_case("quot") {
        '"'
    } else if entity.eq_ignore_ascii_case("apos") {
        '\''
    } else if entity.eq_ignore_ascii_case("nbsp") {
        '\u{a0}'
    } else {
        return None;
    };
    Some(c)
}

/// 数字实体：`#123`（十进制）或 `#x1F600` / `#X1f600`（十六进制）
fn numeric_entity(entity: &str) -> Option<char> {
    let num = entity.strip_prefix('#')?;
    let code = match num.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => num.parse::<u32>().ok()?,
    };
    // 代理区/超范围码位按无效实体处理
    char::from_u32(code)
}

/// 由通知处理器信息推导应用显示名（纯函数，便于单测）。
///
/// 规则：
/// - AUMID（含 `!`，形如 `PackageFamilyName!AppId`）：取包家族名并去掉
///   末尾的 `_<publisherId>`（13 位字母数字），例如
///   `windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft...` → `windows.immersivecontrolpanel`；
/// - exe 路径 / 裸 exe 名（app:desktop 常见）：取文件名 stem 并去掉 `.exe`；
/// - 其它（如 `"QQ"`、`"dev.zcode.app"`）：原样返回；
/// - 空 → `"系统"`。
pub fn app_name_from(primary_id: &str, handler_type: &str) -> String {
    let p = primary_id.trim();
    if p.is_empty() {
        return String::from("系统");
    }

    // 打包应用 AUMID：先按 '!' 切出包家族名，再去发布者哈希
    if p.contains('!') {
        let family = p.split('!').next().unwrap_or(p);
        return finish_name(strip_publisher_suffix(family));
    }

    // app:desktop 的 PrimaryId 可能直接是 exe 完整路径（如 C:\...\QQ.exe）
    let desktop = handler_type.trim().eq_ignore_ascii_case("app:desktop");
    if looks_like_exe(p) || (desktop && (p.contains('\\') || p.contains('/'))) {
        return finish_name(exe_stem(p));
    }

    // 普通处理器标识：原样展示
    p.to_string()
}

/// 名称为空时统一回落「系统」
fn finish_name(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        String::from("系统")
    } else {
        s.to_string()
    }
}

/// 去掉包家族名的发布者哈希后缀（`_8wekyb3d8bbwe` / `_cw5n1h2txyewy`，13 位字母数字）
fn strip_publisher_suffix(family: &str) -> &str {
    match family.rsplit_once('_') {
        Some((head, tail))
            if !head.is_empty()
                && tail.len() == 13
                && tail.bytes().all(|b| b.is_ascii_alphanumeric()) =>
        {
            head
        }
        _ => family,
    }
}

/// 是否像 exe 文件（大小写不敏感比较 `.exe` 后缀）
fn looks_like_exe(p: &str) -> bool {
    p.len() > 4 && p.as_bytes()[p.len() - 4..].eq_ignore_ascii_case(b".exe")
}

/// 取路径/文件名的 stem（去目录与 `.exe` 后缀）
fn exe_stem(p: &str) -> &str {
    let file = p.rsplit(['\\', '/']).next().unwrap_or(p);
    if looks_like_exe(file) {
        // 后缀是 ASCII，len-4 一定是字符边界
        &file[..file.len() - 4]
    } else {
        file
    }
}

/// AUMID 形如 `PackageFamilyName!AppId`
fn is_aumid(s: &str) -> bool {
    s.contains('!')
}

/// 是否是可以直接取文件图标的 exe 路径/文件名
fn looks_like_path(s: &str) -> bool {
    s.contains('\\') || s.contains('/') || looks_like_exe(s)
}

/// 泵一次线程消息（与 worker.rs 完全一致的写法）
fn pump_messages() {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    // SAFETY: 标准 Win32 消息泵调用；MSG 由 PeekMessageW 填充
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== FILETIME → Unix 毫秒 ====================

    #[test]
    fn fil_time_zero_and_invalid() {
        assert_eq!(fil_time_to_unix_ms(0), 0);
        assert_eq!(fil_time_to_unix_ms(-1), 0);
        assert_eq!(fil_time_to_unix_ms(i64::MIN), 0);
        // 恰好等于纪元差值：1970-01-01T00:00:00Z
        assert_eq!(fil_time_to_unix_ms(EPOCH_DIFF_100NS), 0);
        // 早于 1970 的值同样归零
        assert_eq!(fil_time_to_unix_ms(EPOCH_DIFF_100NS - 10_000), 0);
    }

    #[test]
    fn fil_time_normal_value() {
        // 实测样例：134373833532971338 → 约 1.79e12 ms
        let ms = fil_time_to_unix_ms(134_373_833_532_971_338);
        assert_eq!(ms, 1_792_909_753_297);
        let approx = 1.79e12;
        assert!((ms as f64 - approx).abs() < 1.0e10);
        // 10_000 个 100ns = 1ms
        assert_eq!(fil_time_to_unix_ms(EPOCH_DIFF_100NS + 10_000), 1);
        // 极大值不溢出、不 panic
        assert!(fil_time_to_unix_ms(i64::MAX) > 1_000_000_000_000);
    }

    // ==================== XML 实体解码 ====================

    #[test]
    fn decode_named_entities() {
        assert_eq!(decode_entities("a &amp; b"), "a & b");
        assert_eq!(decode_entities("&lt;tag&gt;"), "<tag>");
        assert_eq!(decode_entities("&quot;x&quot; &apos;y&apos;"), "\"x\" 'y'");
        assert_eq!(decode_entities("&amp;lt;"), "&lt;"); // 只解一层
        assert_eq!(decode_entities("A&amp;B&amp;C"), "A&B&C");
    }

    #[test]
    fn decode_numeric_entities() {
        assert_eq!(decode_entities("&#65;&#66;"), "AB");
        assert_eq!(decode_entities("&#x1F600;"), "\u{1F600}");
        assert_eq!(decode_entities("&#X42;"), "B");
        assert_eq!(decode_entities("&#20320;&#x597d;"), "你好");
    }

    #[test]
    fn decode_entities_keeps_plain_text() {
        assert_eq!(decode_entities(""), "");
        assert_eq!(decode_entities("没有实体 & 符号"), "没有实体 & 符号");
        // 不完整的 / 未知实体原样保留
        assert_eq!(decode_entities("a & b; c"), "a & b; c");
        assert_eq!(decode_entities("&unknown;"), "&unknown;");
        assert_eq!(decode_entities("结尾的 &"), "结尾的 &");
        // 非法码位（代理区）保留原样
        assert_eq!(decode_entities("&#xD800;"), "&#xD800;");
    }

    // ==================== toast XML 文本抽取 ====================

    #[test]
    fn extract_title_and_body() {
        let xml = r#"<toast><visual><binding template="ToastGeneric">
            <text>标题</text>
            <text>正文一</text>
            <text>正文二</text>
        </binding></visual><audio silent="true"/></toast>"#;
        let (title, body) = extract_toast_text(xml).expect("应抽到文本");
        assert_eq!(title, "标题");
        assert_eq!(body, "正文一\n正文二");
    }

    #[test]
    fn extract_with_attributes_and_case() {
        let xml = r#"<TOAST><VISUAL><BINDING TEMPLATE="ToastGeneric">
            <TEXT hint-maxLines="1">  邮件  </TEXT>
            <Text hint-wrap="true">正文</Text>
        </BINDING></VISUAL></TOAST>"#;
        let (title, body) = extract_toast_text(xml).expect("大小写不敏感");
        assert_eq!(title, "邮件");
        assert_eq!(body, "正文");
    }

    #[test]
    fn extract_decodes_entities_inside_text() {
        let xml = "<toast><visual><binding><text>新邮件来自 GitHub &lt;noreply@github.com&gt;</text>\
                   <text>A &amp; B &#20320;&#x597d;</text></binding></visual></toast>";
        let (title, body) = extract_toast_text(xml).expect("应抽到文本");
        assert_eq!(title, "新邮件来自 GitHub <noreply@github.com>");
        assert_eq!(body, "A & B 你好");
    }

    #[test]
    fn extract_skips_non_text_tags_and_self_closing() {
        // <textbox> 不是 <text>；自闭合 <text/> 没有内容
        let xml = r#"<toast><textbox>不是正文</textbox><visual><binding>
            <image placement="appLogoOverride" src="C:\a.png"/>
            <text hint="x"/>
            <text>真标题</text><text>真正文</text>
        </binding></visual></toast>"#;
        let (title, body) = extract_toast_text(xml).expect("应抽到文本");
        assert_eq!(title, "真标题");
        assert_eq!(body, "真正文");
    }

    #[test]
    fn extract_image_before_text_is_ok() {
        // 图片/音频标签乱序、位于 text 之前都不影响抽取顺序
        let xml = r#"<toast><visual><binding><image src="x"/><text>t1</text>
            </binding></visual><audio silent="true"/><text>t2</text></toast>"#;
        let (title, body) = extract_toast_text(xml).expect("应抽到文本");
        assert_eq!(title, "t1");
        assert_eq!(body, "t2");
    }

    #[test]
    fn extract_returns_none_without_text() {
        assert_eq!(extract_toast_text(""), None);
        assert_eq!(
            extract_toast_text(r#"<toast><visual><audio silent="true"/></visual></toast>"#),
            None
        );
        // 只有空白文本也算没有内容
        assert_eq!(
            extract_toast_text("<toast><text>   </text><text>\t</text></toast>"),
            None
        );
        // 残缺 payload（标签没闭合）不 panic
        assert_eq!(extract_toast_text("<toast><text>没有闭合"), None);
    }

    #[test]
    fn extract_real_qq_payload() {
        // 实测 payload 片段：launch 属性、CRLF、内嵌 image、实体
        let xml = "<toast launch=\"ntqq://x\" activationType=\"protocol\">\r\n        <visual>\r\n            <binding template=\"ToastGeneric\">\r\n                <text hint-maxLines=\"1\">宝宝宝宝宝宝</text>\r\n                <text hint-maxLines=\"1\">你有1条新通知</text>\r\n                <image placement=\"appLogoOverride\" src=\"C:\\a.png\"/>\r\n            </binding>\r\n        </visual>\r\n        <audio silent=\"true\"/>\r\n    </toast>";
        let (title, body) = extract_toast_text(xml).expect("应抽到文本");
        assert_eq!(title, "宝宝宝宝宝宝");
        assert_eq!(body, "你有1条新通知");
    }

    // ==================== 应用名映射 ====================

    #[test]
    fn app_name_from_exe_path() {
        assert_eq!(
            app_name_from(r"C:\Program Files\Tencent\QQNT\QQ.exe", "app:desktop"),
            "QQ"
        );
        // 大小写不敏感去后缀 / 正斜杠路径
        assert_eq!(app_name_from("D:/Apps/WeChat/WeChat.EXE", "app:desktop"), "WeChat");
        // 裸 exe 文件名
        assert_eq!(app_name_from("notepad.exe", "app:desktop"), "notepad");
        // 只有目录（没有扩展名）：取最后一段
        assert_eq!(app_name_from(r"C:\Tools\bin", "app:desktop"), "bin");
    }

    #[test]
    fn app_name_from_aumid() {
        assert_eq!(
            app_name_from(
                "windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel",
                "app:immersive"
            ),
            "windows.immersivecontrolpanel"
        );
        assert_eq!(
            app_name_from("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", ""),
            "Microsoft.WindowsCalculator"
        );
        // 没有发布者后缀：保留原样
        assert_eq!(
            app_name_from("Some.Package!App", "app:immersive"),
            "Some.Package"
        );
        // 下划线后不是 13 位字母数字：不当发布者哈希
        assert_eq!(
            app_name_from("My_App!App", "app:immersive"),
            "My_App"
        );
    }

    #[test]
    fn app_name_from_plain_and_empty() {
        assert_eq!(app_name_from("QQ", "app:desktop"), "QQ");
        assert_eq!(app_name_from("dev.zcode.app", "app:desktop"), "dev.zcode.app");
        assert_eq!(
            app_name_from("Microsoft.Windows.DynamicLightingToastHandler", "app:system"),
            "Microsoft.Windows.DynamicLightingToastHandler"
        );
        // 空 / 纯空白 → 系统
        assert_eq!(app_name_from("", "app:desktop"), "系统");
        assert_eq!(app_name_from("   ", ""), "系统");
        // 以目录分隔符结尾：rsplit 得到空文件名 → 系统
        assert_eq!(app_name_from("C:\\dir\\", "app:desktop"), "系统");
    }
}

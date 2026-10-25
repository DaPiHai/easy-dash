//! Windows SMTC（System Media Transport Controls）媒体播放状态与控制。
//!
//! 数据源：`GlobalSystemMediaTransportControlsSessionManager` 的当前会话。
//! 提供播放器/标题/艺术家/专辑/封面/进度/播放状态，以及播放暂停、上一首、下一首控制。
//!
//! 线程约定：调用线程必须已初始化 COM / WinRT（`commands.rs` 的 blocking 线程池
//! 在调用前会执行 `crate::controls::ensure_com`）；这里再调一次做容错（重复初始化无害）。
//! 所有 WinRT 调用失败都降级为 `None` / 中文 `Err`，不会 panic。
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use windows::core::HSTRING;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use windows::Storage::Streams::{DataReader, IRandomAccessStreamReference};

use crate::types::MediaInfo;

/// 封面读取上限：1MB（避免为超大封面做无谓的解码 / 编码）
const MAX_THUMB_BYTES: u64 = 1024 * 1024;
/// 播放器显示名缓存条目上限（AUMID → 名称）
const SOURCE_APP_CACHE_MAX: usize = 16;

/// 当前媒体会话状态；没有会话 / 读不到会话时返回 `None`。
///
/// 注意：只有「会话管理器或当前会话不可用」才返回 None；会话存在但元数据读取
/// 失败时返回字段为空的 `MediaInfo`（前端可据此决定是否隐藏媒体卡）。
pub fn state() -> Option<MediaInfo> {
    // 容错：调用方线程未必初始化过 WinRT（重复初始化无害）
    crate::controls::ensure_com();

    let manager = SessionManager::RequestAsync().ok()?.get().ok()?;
    // 没有任何应用在 SMTC 上注册会话：windows crate 会把 null 引用映射成 Err
    let session = manager.GetCurrentSession().ok()?;

    let mut info = MediaInfo::default();

    // 播放器 AUMID → 显示名（如 Spotify.exe 的 AUMID → "Spotify"）
    if let Ok(aumid) = session.SourceAppUserModelId() {
        info.source_app = source_app_name(&aumid.to_string_lossy());
    }

    // 媒体属性：标题 / 艺术家 / 专辑 / 封面
    if let Some(props) = session
        .TryGetMediaPropertiesAsync()
        .ok()
        .and_then(|op| op.get().ok())
    {
        info.title = hstring(props.Title());
        info.artist = hstring(props.Artist());
        info.album = hstring(props.AlbumTitle());
        // Thumbnail() 在没有封面时会返回 Err（很便宜），失败即视为无封面
        let thumb_ref = props.Thumbnail().ok();
        info.thumbnail = cached_thumbnail(
            &info.title,
            &info.artist,
            &info.album,
            thumb_ref.as_ref(),
        );
    }

    // 播放状态：WinRT 枚举 Playing = 4（Closed/Opened/Changing/Stopped/Paused 都算未播放）
    if let Ok(playback) = session.GetPlaybackInfo() {
        if let Ok(status) = playback.PlaybackStatus() {
            info.playing = status == PlaybackStatus::Playing;
        }
    }

    // 时间轴：TimeSpan 单位是 100ns；部分播放器用 StartTime 表示片段起点（非 0）
    if let Ok(timeline) = session.GetTimelineProperties() {
        let start = timeline.StartTime().map(|t| t.Duration).unwrap_or(0);
        let end = timeline.EndTime().map(|t| t.Duration).unwrap_or(0);
        let position = timeline.Position().map(|t| t.Duration).unwrap_or(0);
        let (position_ms, duration_ms) = timeline_ms(start, position, end);
        info.position_ms = position_ms;
        info.duration_ms = duration_ms;
    }

    Some(info)
}

/// 媒体控制：`"toggle"`（播放/暂停）、`"prev"`（上一首）、`"next"`（下一首）。
/// 返回系统是否接受了该操作；未知动作返回中文 Err。
pub fn action(action: &str) -> Result<bool, String> {
    // 先校验动作：未知动作不需要碰 WinRT，也能给出准确错误
    let verb = action.trim().to_ascii_lowercase();
    if !matches!(verb.as_str(), "toggle" | "prev" | "next") {
        return Err(format!(
            "未知的媒体控制动作：{action}（支持 toggle / prev / next）"
        ));
    }

    // 容错：与 state() 相同，调用线程未初始化 WinRT 时也能工作
    crate::controls::ensure_com();

    let manager = SessionManager::RequestAsync()
        .map_err(|e| format!("获取媒体会话管理器失败：{e}"))?
        .get()
        .map_err(|e| format!("等待媒体会话管理器失败：{e}"))?;
    let session = manager
        .GetCurrentSession()
        .map_err(|_| String::from("当前没有可控制的媒体会话（没有应用在播放音乐/视频）"))?;

    let operation = match verb.as_str() {
        "toggle" => session.TryTogglePlayPauseAsync(),
        "prev" => session.TrySkipPreviousAsync(),
        _ => session.TrySkipNextAsync(),
    };

    operation
        .map_err(|e| format!("发起媒体控制失败：{e}"))?
        .get()
        .map_err(|e| format!("等待媒体控制结果失败：{e}"))
}

// ==================== 内部辅助 ====================

/// HSTRING → 去首尾空白的 String（读取失败返回空串）
fn hstring(value: windows::core::Result<HSTRING>) -> String {
    match value {
        Ok(s) => s.to_string_lossy().trim().to_string(),
        Err(_) => String::new(),
    }
}

/// 把 SMTC 时间轴（TimeSpan，单位 100ns）换算成 `(position_ms, duration_ms)`。
///
/// - `start` 可能非 0（播放器用片段起点），位置与时长都相对 start；
/// - 负数 / 为零 / 早于 start 的值统一置 0；
/// - 位置超过时长（流媒体元数据滞后）时截断到时长，避免前端进度条溢出。
fn timeline_ms(start: i64, position: i64, end: i64) -> (u64, u64) {
    let duration_100ns = end.saturating_sub(start);
    let position_100ns = position.saturating_sub(start);

    let duration_ms = if duration_100ns > 0 {
        (duration_100ns / 10_000) as u64
    } else {
        0
    };
    let mut position_ms = if position_100ns > 0 {
        (position_100ns / 10_000) as u64
    } else {
        0
    };
    if duration_ms > 0 && position_ms > duration_ms {
        position_ms = duration_ms;
    }
    (position_ms, duration_ms)
}

/// AUMID → 播放器显示名。前端每隔几秒轮询媒体状态，这里做一层小缓存，
/// 避免每次都走注册表 / 包清单解析。
fn source_app_name(aumid: &str) -> String {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

    let aumid = aumid.trim();
    if aumid.is_empty() {
        return String::new();
    }
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(name) = guard.get(aumid) {
            return name.clone();
        }
    }

    let name = resolve_source_app(aumid);
    if let Ok(mut guard) = cache.lock() {
        if guard.len() < SOURCE_APP_CACHE_MAX {
            guard.insert(aumid.to_string(), name.clone());
        }
    }
    name
}

/// 优先用 app_icons 的显示名（能拿到本地化名称），失败回退纯函数映射
fn resolve_source_app(aumid: &str) -> String {
    if let Some(identity) = crate::winapi::app_icons::resolve_identity(aumid) {
        let name = identity.name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
    }
    crate::notifications::app_name_from(aumid, "app:immersive")
}

/// 封面缓存：同一首歌（标题/艺术家/专辑相同）轮询时复用 data URL，
/// 避免每次刷新都重新读流 + base64。只缓存成功结果（失败下次再试，防止
/// 播放器刚启动时封面未就绪就被永久判为无封面）。
fn cached_thumbnail(
    title: &str,
    artist: &str,
    album: &str,
    reference: Option<&IRandomAccessStreamReference>,
) -> Option<String> {
    static CACHE: OnceLock<Mutex<Option<(String, String)>>> = OnceLock::new();

    let reference = reference?;
    let key = format!("{title}|{artist}|{album}");
    let cache = CACHE.get_or_init(|| Mutex::new(None));

    if let Ok(guard) = cache.lock() {
        if let Some((cached_key, url)) = guard.as_ref() {
            if *cached_key == key {
                return Some(url.clone());
            }
        }
    }

    let url = thumbnail_data_url(reference)?;
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((key, url.clone()));
    }
    Some(url)
}

/// 读取封面流并转 data URL：
/// PNG/JPEG 原样透传（省一次编解码），其它格式用 image crate 解码后转 PNG。
fn thumbnail_data_url(reference: &IRandomAccessStreamReference) -> Option<String> {
    let stream = reference.OpenReadAsync().ok()?.get().ok()?;
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
    let want = size.min(MAX_THUMB_BYTES) as u32;
    let input = stream.GetInputStreamAt(0).ok()?;
    let reader = DataReader::CreateDataReader(&input).ok()?;
    let loaded = reader.LoadAsync(want).ok()?.get().ok()?;
    if loaded == 0 {
        return None;
    }
    let mut buf = vec![0u8; loaded as usize];
    reader.ReadBytes(&mut buf).ok()?;

    match content_type.as_str() {
        "image/png" => Some(format!("data:image/png;base64,{}", STANDARD.encode(&buf))),
        "image/jpeg" | "image/jpg" => {
            Some(format!("data:image/jpeg;base64,{}", STANDARD.encode(&buf)))
        }
        // 其它（image/bmp 等）：统一解码成 PNG，保证前端 <img> 可显示
        _ => {
            let image = image::load_from_memory(&buf).ok()?;
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
    use super::timeline_ms;

    /// 100ns → 毫秒的常规换算
    #[test]
    fn timeline_normal() {
        // start=0，时长 3 分 20 秒，位置 1 分 5 秒
        let duration = 200u64 * 1000 * 10_000;
        let position = 65u64 * 1000 * 10_000;
        assert_eq!(
            timeline_ms(0, position as i64, duration as i64),
            (65_000, 200_000)
        );
    }

    /// start 非 0（片段起点）时，位置与时长都相对 start
    #[test]
    fn timeline_with_start_offset() {
        let start = 5u64 * 1000 * 10_000; // 5 秒
        let end = start + 60u64 * 1000 * 10_000; // 再 60 秒
        let position = start + 10u64 * 1000 * 10_000; // 距起点 10 秒
        assert_eq!(
            timeline_ms(start as i64, position as i64, end as i64),
            (10_000, 60_000)
        );
    }

    /// 0 / 负数 / 位置早于 start / 无效时长都归 0，不 panic
    #[test]
    fn timeline_invalid_values() {
        assert_eq!(timeline_ms(0, 0, 0), (0, 0));
        assert_eq!(timeline_ms(-1, -1, -1), (0, 0));
        assert_eq!(timeline_ms(0, -5, 1_000), (0, 0));
        // 位置早于 start
        assert_eq!(timeline_ms(1_000, 500, 2_000), (0, 0));
        // 时长比 start 还小（异常数据）
        assert_eq!(timeline_ms(10_000, 10_000, 5_000), (0, 0));
    }

    /// 位置超过时长时截断，避免进度条溢出
    #[test]
    fn timeline_position_clamped() {
        let end = 60u64 * 1000 * 10_000;
        let position = 90u64 * 1000 * 10_000;
        assert_eq!(timeline_ms(0, position as i64, end as i64), (60_000, 60_000));
    }

    /// 极值不溢出（i64::MIN / i64::MAX 用饱和运算兜底）
    #[test]
    fn timeline_extremes() {
        let (position, duration) = timeline_ms(0, i64::MAX, i64::MAX);
        assert!(duration > 0);
        assert_eq!(position, duration);
        // 饱和减法：不 panic、位置不超时长
        let (position, duration) = timeline_ms(i64::MIN, i64::MAX, i64::MAX);
        assert!(duration > 0);
        assert!(position <= duration);
    }

    /// 未知动作在碰 WinRT 之前就被拒绝（因此本测试与机器状态无关）
    #[test]
    fn action_unknown_is_rejected() {
        let err = super::action("rewind").expect_err("未知动作应返回 Err");
        assert!(err.contains("未知的媒体控制动作"), "错误信息应说明未知动作：{err}");
        // 大小写 / 空白容错后仍属未知动作
        assert!(super::action("  PLAY  ").is_err());
    }

    /// 真实调用 SMTC：依赖本机是否有媒体会话与 WinRT 可用，因此默认 `#[ignore]`。
    /// 手动验证：`cargo test --manifest-path src-tauri/Cargo.toml --lib -- --ignored --nocapture`
    #[test]
    #[ignore = "依赖本机 SMTC 会话，手动执行"]
    fn smoke_state() {
        crate::controls::ensure_com();
        match super::state() {
            Some(info) => eprintln!(
                "[smoke] media: app={:?} title={:?} artist={:?} album={:?} playing={} pos={} dur={} thumb={}",
                info.source_app,
                info.title,
                info.artist,
                info.album,
                info.playing,
                info.position_ms,
                info.duration_ms,
                info.thumbnail.as_ref().map(|t| t.len()).unwrap_or(0)
            ),
            None => eprintln!("[smoke] media: 当前没有 SMTC 会话"),
        }
    }
}

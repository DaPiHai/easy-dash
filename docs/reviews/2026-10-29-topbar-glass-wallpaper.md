# 顶栏/Dock 玻璃「变成一整条纯色」定位与修复

- 日期：2026-10-29
- 现象（用户反馈「顶栏样式消失了」）：顶栏变成一条**纯色浅灰条**，看不到壁纸/玻璃质感。
- 结论：液态玻璃的**底片取样对象错了**——顶栏在 AppBar 把最大化窗口推下去之前就截了屏。
  已改为直接采样壁纸（`src-tauri/src/winapi/glass.rs`）。

## 1. 定位过程

1. 先确认不是 CSS 没加载：`ui/css/topbar.css` 里 `.clock-btn { position:absolute; left:50% }`，
   实际渲染中时钟确实居中、活动按钮有 pill 内边距 → **布局 CSS 生效**，缺的是玻璃视觉。
2. 壁纸顶条（1920×32）平均色 `#696672`（深紫灰，Bing 壁纸 2026-10-29），而顶栏渲染出来是
   `#F5F6F9`（≈ 其下方 DSH 窗口标题栏 `#F9FAFB`）。说明玻璃底片取的是**前台窗口**，不是壁纸。
3. 代码路径确认（`src-tauri/src/surfaces.rs`）：
   - `show_if_hidden()` 先 `capture_surface_backdrop()`（`window_backdrop` = `BitBlt` 屏幕矩形），**再** `show()`；
   - `appbar::register_topbar()` 在 `show_if_hidden()` **之后**才注册 AppBar；
   - 所以取样那一刻，最大化窗口还压在 y=0..32 上，截到的就是它的标题栏；而底片只取一次并缓存，
     之后不会再更新 → 整条顶栏始终是那一条纯色。
   - 同一问题也影响 Dock（启动时它下方是任务栏/窗口）。
4. 交叉验证：`cargo test --lib wallpaper_real_smoke -- --ignored --nocapture` 用真实壁纸算出
   顶栏条底片 `mean=[104,101,112]`（`#686570`）→ 修复后应是深色玻璃，而不是浅灰纯色。

## 2. 修复

`src-tauri/src/winapi/glass.rs` 新增 `wallpaper_backdrop()`：

- `SystemParametersInfoW(SPI_GETDESKWALLPAPER)` 取壁纸文件；
- `HKCU\Control Panel\Desktop\WallpaperStyle` / `TileWallpaper` 还原铺法：
  `fill(10)/span(22)`、`stretch(2)`、`fit(6)`、`center(0)`、`tile(1)`，留边用
  `HKCU\Control Panel\Colors\Background`；
- 虚拟屏像素 → 壁纸像素映射（`WallpaperMap`，纯函数 + 4 个单测），按窗口矩形裁剪成 RGBA；
- 复用 `build_backdrop()`（缩放 ≤480 宽、p05/p95、JPEG data URL）。

`src-tauri/src/surfaces.rs::capture_surface_backdrop()`：顶栏 / Dock（`topbar*` / `dock*`）优先
`wallpaper_backdrop()`，失败回退原截屏路径；面板 / 概览 / 应用中心 / 管理面板仍用截屏
（它们本来就该透出背后窗口）。

### 2.1 前端还有一处覆盖（真正的「最后一根稻草」）

只改 Rust 后顶栏仍然发白。用 `--remote-debugging-port` + CDP 读 DOM 发现：

```
data-material=liquid  data-glass=live  data-ink=dark
--lg-backdrop 长度 1146（480x8 JPEG，mean=#F8F9FB ← 浅色！）
```

`ui/js/glass.js::recaptureSurface()` 在页面加载 2 帧后把顶栏 `visibility:hidden`、
用 `glass_backdrop` **截屏**补一张——那一刻 AppBar 刚注册、最大化窗口还没让位，
截到的仍是前台窗口标题栏，直接把 Rust 给的壁纸底片**覆盖**掉了。

修复：顶栏 / Dock 不再走 `recaptureSurface`（截屏），统一用 `refreshSurface()`
（`surface_backdrop` 命令，缓存缺失时 Rust 按需 `wallpaper_backdrop` 现场算壁纸）；
管理面板 / 概览 / 应用中心的「打开补截」保持原样（它们确实该截屏取背后桌面）。
`commands.rs::surface_backdrop` 也相应改为：缓存缺失且是 chrome 表面时按需从壁纸补算。

### 2.2 修复后的 DOM 证据

```
data-glass=live  data-ink=light        ← 深色壁纸自动切浅色文字
--lg-backdrop 长度 1438（480x8 JPEG，mean=#696571 ← 壁纸顶条）
--lg-specular rgba(255,255,255,0.18)   ← 由 0.72 自适应降到 0.18
```

## 3. 验证

- `cargo test --lib`：**158 passed / 0 failed / 14 ignored**（新增 4 个壁纸映射单测）。
- 真机冒烟：`wallpaper_real_smoke` 输出 `fit=Fill vs=(0,0,1920,1080) rect=(0,0,1920,32) mean=[104,101,112] p05=0.029 p95=0.425`。
- 真机运行：`glass-capture` 日志 `topbar-DISPLAY1 ok=true mean=[104,101,112]`、`dock-DISPLAY1 mean=[69,74,26]`；
  桌面截图里顶栏从 `#F5F6F9` 浅灰条变成**深色玻璃条 + 浅色文字**（`data-ink=light`），壁纸顶部的紫灰色透出来。
  修复后截图：`docs/reviews/assets/topbar-glass-after-fix.png`（1920×32 顶栏 2× 放大）。
- 面板 / 概览等未改动，仍按打开时截屏取样。

## 4. 调试开关


- `EASYDASH_GLASS_DEBUG=1`：玻璃底片取样结果（label / 尺寸 / mean / 跳过原因）写入
  `%APPDATA%\com.easydash.app\tray-click.log`。
- `EASYDASH_WEBVIEW2_ARGS="--remote-debugging-port=9333"`：给 WebView2 追加参数，可用 CDP 读
  `document.styleSheets` / 计算样式 / 控制台错误（注意：以管理员运行时 `Start-Process -Verb RunAs`
  不会继承环境变量，需要用 .cmd 包一层再提权）。

## 5. 相关文件

- `src-tauri/src/winapi/glass.rs`：`wallpaper_backdrop` / `WallpaperMap` / `wallpaper_path` / `wallpaper_fit` / `desktop_background`。
- `src-tauri/src/surfaces.rs`：`capture_surface_backdrop` 分流 + `prefers_wallpaper_backdrop` + 调试日志。
- `CHANGELOG.md` / `README.md` 已知限制。

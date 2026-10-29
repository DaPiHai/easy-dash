# 顶栏托盘「微信左键点了没反应」定位与修复

- 日期：2026-10-29
- 现象：easy-dash 顶栏托盘里点微信（Weixin.exe）左键，完全没反应；右键菜单正常。
- 结论：代理发给 v4 应用的托盘回调消息用错了协议。已修复 `src-tauri/src/tray_proxy.rs`。

## 1. 现场证据（用户机器 `%APPDATA%\com.easydash.app\tray-click.log`）

```
mode=proxy btn=left key=weixin name="微信" hwnd=0x21612 uid=0 ver=0 cursor=(1572, 15) ok=true
```

`ok=true` 说明消息确实发出去了；`ver=0` 说明代理把微信当成了 legacy（v0–v3）图标。

## 2. 用独立探针复刻托盘代理（不启动 easy-dash）

`%TEMP%\easydash-trayprobe\ProxyProbe.cs`：注册类名 `Shell_TrayWnd` 的伪托盘窗口 →
广播 `TaskbarCreated` → 解析 `WM_COPYDATA / SHELLTRAYDATA`，并把消息原样转发给真实托盘。

微信的注册消息：

```
COPYDATA magic=0x34753423 type=0 cbData=1484 cbSize=956 hwnd=0x21612 uid=0
         flags=0x87 cb=0x8385 ver=4 tip=微信 cls=Qt51514WxTrayIconMessageWindowClass
COPYDATA magic=0x34753423 type=4 ... ver=4 tip=微信 cls=Qt51514WxTrayIconMessageWindowClass
```

即：微信 **真的声明了 `NOTIFYICON_VERSION_4`**（ADD 里就带 `uVersion=4`），
回调消息 `0x8385`（`WM_APP+0x385`），窗口过程是 Qt 的（类名 `Qt51514...`）。
旧实现却在 `NIM_ADD` 时把 `version` 写死成 0，只靠 `NIM_SETVERSION` 更新；
真机日志里一直是 `ver=0`，于是走了 legacy 分支。

## 3. 为什么 legacy 消息对微信无效

Qt 5.15 `qwindowssystemtrayicon.cpp`（`QWindowsSystemTrayIcon::winEvent`）只处理：

- `NIN_SELECT` / `NIN_KEYSELECT` → `activated(Trigger)`
- `WM_LBUTTONDBLCLK` → `activated(DoubleClick)`
- `WM_CONTEXTMENU` → 右键菜单
- `WM_MBUTTONUP`

**没有 `WM_LBUTTONUP` / `WM_LBUTTONDOWN` 分支**——Qt 图标在真实 Explorer 上左键收到的就是
`NIN_SELECT`（微软文档：`uVersion >= NOTIFYICON_VERSION_4` 时鼠标选中发 `NIN_SELECT`）。

## 4. 真机实验（同一台机器、微信正在运行）

向微信托盘窗口 `0x21612`（回调 `0x8385`）发送不同序列，观察主窗口 `0x21620`：

| 变体 | 消息 | 结果 |
| --- | --- | --- |
| A | `NIN_SELECT`，`wParam=MAKELPARAM(x,y)`，`lParam=MAKELPARAM(0x400,0)` | **窗口可见 + 成为前台窗口** ✅ |
| B | `NIN_SELECT`，`wParam=uid`（legacy 写法） | 也可用（Qt 不看 wParam） |
| C | `WM_LBUTTONUP`（legacy） | 无反应 ❌ |
| D | `WM_LBUTTONDOWN` + `WM_LBUTTONUP` | 无反应 ❌ |

## 5. 修复（`tray_proxy.rs`）

1. `version` 改为从 `NIM_ADD` / `NIM_MODIFY` 的 `uVersion` 读取；该字段与 `uTimeout` 是 union，
   只接受 1..=4（`sane_notify_version`），避免「悟空 5000」「小米互联 4015578873」这类垃圾值；
   `NIM_SETVERSION` 先到时暂存 `pending_versions`；版本只升级不降级。
2. 协议按真实 Explorer 行为发送（`click_messages`）：
   - v4：左键 **只发 `NIN_SELECT`**，右键 **只发 `WM_CONTEXTMENU`**，`wParam` 为顶栏图标中心屏幕坐标，
     `lParam` 高 16 位为 uID；
   - legacy（v0–v3）：仍发按下/抬起；Qt / `TRAY_CLASS` 的 v0 右键补 `0x7B`。
   - 不再对 v4 同时发 `WM_LBUTTONUP` + `NIN_SELECT`：那会把应用激活两次（开→又关）。
3. Qt 窗口类（`Qt*`）即使 `uVersion` 没解析到也按 v4 发送（Qt 只认 `NIN_SELECT`）。
4. 新增「越界窗口兜底」`rescue_offscreen_foreground`：左键激活 ~700ms 后，如果前台窗口属于被点击应用、
   却完全不与任何显示器相交（典型：显示器拔掉/改分辨率后应用仍记得旧坐标），把它夹回顶栏所在屏工作区。
   本次定位中微信主窗口就停在 `(1750,1253)-(3359,2261)`（屏幕 1920×1080），窗口其实「打开了」但看不到。
   日志：`mode=proxy-offscreen-rescue`。
5. 诊断日志每行新增 `proto=v4|legacy`、`cls=`、`msgs=`。

## 6. 验证

- `cargo test --lib`：**154 passed / 0 failed / 13 ignored**；新增 4 个单测
  （版本 union 过滤、v4/Qt 判定、左键/右键消息序列）。
- `cargo check --all-targets`：0 warning。
- 真机：用探针按修复后的协议（`NIN_SELECT`）点微信，微信窗口可见并置为前台；
  微信主窗口的旧越界坐标也用 `SetWindowPos` 修正到屏幕内，之后托盘激活即在屏幕上出现。
- 交付物已重建：`src-tauri/target/release/easy-dash.exe`、
  `src-tauri/target/release/bundle/nsis/easy-dash_0.3.0_x64-setup.exe`（12:09）。
- 未完成：在 easy-dash GUI 里点一次顶栏微信图标的人工验证。本机非提升进程启动 Tauri 会
  `WebView2 error 0x800700AA / 0x8000FFFF`（见 `HANDOFF.md` §6），必须由用户以管理员运行
  （UAC）后按 `tray-click.log` 里 `proto=v4`、`msgs=["0x400"]` 确认。

## 7. 复现/清理

- 探针源码：`%TEMP%\easydash-trayprobe\`（`ProxyProbe.cs` / `driver.ps1` / `move-test.ps1`）；
  运行期间会把 `Shell_TrayWnd` 抢过来，退出前会再次广播 `TaskbarCreated` 并把消息转发给真实托盘。
- 本次实验期间临时改过用户 `config.json`（`hideTaskbar=false`）并已还原；临时 WebView2 profile 已删除。

# easy-dash

轻量级 Windows 桌面美化工具，界面风格参考 GNOME：

- **顶栏**：屏幕顶部通栏，含「活动」按钮、时钟日期、网络/音量/电池状态
- **Dock 栏**：底部居中胶囊应用栏，固定应用 + 运行指示点，悬停放大动效，右键固定/取消固定
- **活动概览**：全屏概览所有打开的窗口（实时截图缩略图），点击切换，`Esc` 关闭
- **全局热键**：`Alt+F1` 打开/关闭活动概览
- **全屏自动隐藏**：前台应用全屏时，顶栏与 Dock 自动滑出隐藏
- **系统托盘**：左键切换概览，右键菜单（打开概览 / 重载配置 / 退出）

## 技术栈

Tauri 2（Rust + WebView2）+ 原生 HTML/CSS/JS（无前端框架、无打包器，`window.__TAURI__` 全局注入）。

Win32 能力：窗口枚举过滤（DWM cloaked）、图标提取（WM_GETICON / SHGetFileInfo）、
PrintWindow 窗口截图、AttachThreadInput 前台激活、ShellExecute 启动、
AppBar 预留顶栏空间、全屏前台检测。

## 开发

```bash
npm install          # 安装 @tauri-apps/cli
npm run icon         # 生成应用图标（首次）
npm run dev          # 开发运行
npm run build        # 构建 release（NSIS 安装包）
```

## 配置

`%APPDATA%\com.easydash.app\config.json`：

```json
{
  "pinned": [{ "name": "记事本", "path": "C:\\Windows\\System32\\notepad.exe", "args": "" }],
  "hotkey": "alt+f1",
  "fullscreenHide": true,
  "reserveTopbarSpace": true
}
```

修改后可在托盘菜单「重载配置」立即生效。

## 已知限制（v1）

- 仅支持主显示器；窗口缩略图为打开概览时的静态截图（非实时）；最小化窗口回退到最后一次可见时缓存的画面
- 音量图标为静态占位，不可调节
- UWP 应用（如「设置」）固定到 Dock 时记录 AppsFolder 启动器，图标取自包清单
- 原生任务栏仍然存在，如需完全 GNOME 化可在任务栏设置中开启自动隐藏
- Dock 中的未固定应用按**窗口打开顺序**排列（不随聚焦变化）；UWP 应用失焦挂起后（DWM cloaked=2）仍保留在 Dock 中，其他虚拟桌面的窗口也会显示

## 管理员权限说明

exe 内嵌了 `requireAdministrator` 清单：**每次启动会弹 UAC**。
这是为了能控制以管理员身份运行的程序（如 Geek Uninstaller、任务管理器）——
Windows 的 UIPI 机制禁止普通权限进程操作高权限窗口。
Dock 启动应用时经 `explorer.exe` 转发，被启动的程序仍是普通权限。
不想每次弹 UAC 可以创建一个任务计划程序（schtasks /rl highest /sc onlogon）来静默启动。

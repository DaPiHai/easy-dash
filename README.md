# easy-dash

轻量级 Windows 桌面美化工具，界面风格参考 GNOME：

- **顶栏**：屏幕顶部通栏，含「活动」按钮、时钟日期、任务托盘镜像、网络/音量/电池状态
- **通知中心**：点顶栏时钟展开，**左右两栏** —— 左栏大时钟 + 日期 + 通知列表（按应用分组、多条通知堆叠显示，真实应用图标），右栏中文月历（**接入中国法定节假日与调休「休/班」标记**）+ **媒体播放控件**（SMTC：封面/标题/歌手/进度/上一首·播放暂停·下一首）
- **控制中心**：点顶栏右侧状态区展开，含 Wi-Fi / 蓝牙 / 飞行模式 / 深色模式开关、**Wi-Fi 网络连接（扫描、密码、断开）与蓝牙设备连接/断开**、音量与亮度滑条、电源模式三档、设置/锁屏/睡眠/关机/重启/注销
- **任务托盘镜像**：Windows 系统托盘图标实时同步到顶栏（真实透明图标），支持**拖拽排序**；点击后的应用菜单/浮层（含 easy-dash 自身托盘菜单）**锚定在顶栏图标下方**，左键/右键行为与直接点任务栏托盘一致
- **Dock 栏**：底部居中胶囊应用栏，固定应用 + 运行指示点，悬停放大动效，右键固定/取消固定；右侧**九点格按钮 → 应用中心**
- **应用中心**：全屏 GNOME 风格应用网格（Dock 九点格或 `Alt+Space` 打开），汇集 Start Menu 经典应用与 Windows 打包应用，支持搜索、方向键导航、回车/点击启动、**拖拽排序**与**导入自定义应用**（.exe/.lnk/.url，可右键移除）；与活动概览互斥，顶栏/Dock 保持在上层
- **活动概览**：全屏概览所有打开的窗口（实时截图缩略图），点击切换，`Esc` 关闭
- **全局热键**：`Alt+F1` 打开/关闭活动概览；`Alt+Space` 打开/关闭应用中心
- **全屏自动隐藏**：前台应用全屏时，顶栏与 Dock 自动滑出隐藏
- **系统托盘**：左键切换概览，右键菜单（打开概览 / 重载配置 / 退出）

## 技术栈

Tauri 2（Rust + WebView2）+ 原生 HTML/CSS/JS（无前端框架、无打包器，`window.__TAURI__` 全局注入）。

Win32 能力：窗口枚举过滤（DWM cloaked）、图标提取（WM_GETICON / SHGetFileInfo / 托盘
NotifyIconSettings 快照 / 版本信息资源）、PrintWindow 窗口截图、AttachThreadInput 前台激活、
ShellExecute 启动、AppBar 预留顶栏空间、全屏前台检测、UI Automation 托盘枚举与弹窗迁移
（Win11 XAML 任务栏）、Core Audio 音量、DDC/CI 亮度、WLAN API（射频/SSID/扫描/连接）、
WinRT Radio 蓝牙开关与 Win32 蓝牙设备/音频服务连接、winsqlite3 只读通知库解析、
WinRT SMTC 媒体播放控制、WinRT PackageManager/AppListEntry 打包应用枚举（本地化名称与
Logo）、Shell 图标工厂（IShellItemImageFactory，任意尺寸透明图标）。

## 开发

```bash
npm install          # 安装 @tauri-apps/cli
npm run icon         # 生成应用图标（首次）
npm run dev          # 开发运行
npm run build        # 构建 release + NSIS 安装包
```

> 若 `npm run build` 卡在下载 NSIS 工具链（`Downloading .../nsis-3.13.zip` 或
> `nsis_tauri_utils.dll` 后报 `failed to bundle project: timeout: global`）：
> 这是 GitHub 资源在部分网络下超时。手动准备缓存即可（bundler 校验 SHA1 后会跳过下载）：
>
> 1. 下载 [nsis-3.13.zip](https://github.com/tauri-apps/binary-releases/releases/download/nsis-3.13/nsis-3.13.zip)，
>    解压出的 `nsis-3.13` 目录内容放到 `%LOCALAPPDATA%\tauri\NSIS\`（`NSIS.exe`、`makensis.exe` 等直接在 NSIS 下）；
> 2. 下载 [nsis_tauri_utils.dll](https://github.com/tauri-apps/nsis-tauri-utils/releases/download/nsis_tauri_utils-v0.5.3/nsis_tauri_utils.dll)，
>    放到 `%LOCALAPPDATA%\tauri\NSIS\Plugins\x86-unicode\additional\`。
>
> 国内网络可在 URL 前加 `https://ghproxy.net/` 前缀下载。

## 配置

`%APPDATA%\com.easydash.app\config.json`：

```json
{
  "pinned": [{ "name": "记事本", "path": "C:\\Windows\\System32\\notepad.exe", "args": "" }],
  "hotkey": "alt+f1",
  "fullscreenHide": true,
  "reserveTopbarSpace": true,
  "trayIcons": true,
  "notificationCenter": true,
  "controlCenter": true,
  "appCenterHotkey": "alt+space",
  "trayOrder": ["weixin#0", "qq#0"],
  "appOrder": ["lnk:c:\\windows\\system32\\notepad.exe"],
  "importedApps": [
    { "name": "计算器", "path": "C:\\Windows\\System32\\calc.exe", "args": "" }
  ]
}
```

修改后可在托盘菜单「重载配置」立即生效。

## 已知限制（v1）

- 仅支持主显示器；窗口缩略图为打开概览时的静态截图（非实时）；最小化窗口回退到最后一次可见时缓存的画面
- **任务托盘镜像 / 通知中心 / 控制中心依赖 Windows 11（XAML 任务栏 + UI Automation）**；不支持时托盘区自动隐藏（Win10 无此能力）
- 通知中心是 Windows 通知历史的**只读镜像**：不能在 easy-dash 内删除系统通知（「清除全部」仅本地隐藏），也不含勿扰开关；通知图标来自托盘图标快照/exe 图标，个别未登记应用回退字母占位
- 任务托盘图标取自系统 `NotifyIconSettings` 快照：**动态角标/红点不会同步**（QQ/TIM 等登录状态图标改用 exe 彩色图标，避免快照停留在离线灰状态）；UIA 提示文本为空的图标通过**屏幕图标视觉匹配**识别；纯黑/深色图标会自动垫浅色底避免在深色顶栏上看不见
- 托盘点击的弹窗锚定是 best-effort：仅迁移点击后新出现的弹出窗口（原生菜单/Electron/Qt 弹窗，含 easy-dash 自身 Tauri 托盘菜单），左键直接打开的主窗口、UWP 系统浮层保持原生行为；弹窗晚于 1.2s 出现或受 UIPI 限制时不迁移；**从 Windows 原生任务栏托盘图标右键的菜单仍由系统绘制、保持原位**
- **拖拽排序只改变 easy-dash 内的显示顺序**，不会修改 Windows 原生托盘/开始菜单顺序；托盘排序按稳定 key（exe 主名 + 同源 `#序号`）持久化，应用中心按应用 id 持久化，新增应用/图标会追加到已排序项之后
- **导入应用**只记录路径并加入应用中心，不复制文件、不写入 Windows 开始菜单；仅支持 `.exe/.lnk/.url`，不支持参数编辑（`args` 字段预留）；右键「移除导入应用」只从 easy-dash 移除，不卸载程序
- 拖拽排序在应用中心搜索状态下禁用（避免只重排搜索结果的歧义）
- 节假日数据固化在 `ui/js/cn-holidays.js`（2024–2026，来源 timor.tech，2026-10-25 抓取）；2027 及以后年份需补充数据后才显示标记
- 媒体控件依赖 Windows SMTC：未注册 SMTC 的播放器不显示；无封面时使用占位
- 亮度滑条依赖显示器 DDC/CI；外接屏或部分机型不支持时自动隐藏
- Wi-Fi 连接支持开放/WPA/WPA2/WPA3 个人网络；**企业级 802.1X 与隐藏网络需在系统设置中连接**；蓝牙「连接」通过启停设备音频服务实现（best-effort），**配对新设备仍走 Windows 设置**
- 蓝牙开关首次切换时系统可能弹出一次授权提示；任务栏设为自动隐藏时托盘图标可能取不到
- 音量图标为状态显示，调节使用控制中心滑条
- UWP 应用（如「设置」）固定到 Dock 时记录 AppsFolder 启动器，图标取自包清单
- 原生任务栏仍然存在，如需完全 GNOME 化可在任务栏设置中开启自动隐藏
- Dock 中的未固定应用按**窗口打开顺序**排列（不随聚焦变化）；UWP 应用失焦挂起后（DWM cloaked=2）仍保留在 Dock 中，其他虚拟桌面的窗口也会显示
- 应用中心列表 = Start Menu（用户 + 公共）快捷方式 + WinRT 打包应用；启动时后台枚举，超过 5 分钟或手动刷新时重扫；**控制面板项、分类/最近使用/拼音搜索暂不支持**（v1 仅名称子串过滤）
- 打包应用枚举失败（权限/系统异常）时**降级为仅经典应用**并在日志记录；个别应用无 AppListEntry 时不展示；图标统一 48px，提取失败回退字母占位
- `Alt+Space` 被 easy-dash 全局占用，**会覆盖系统「窗口菜单」快捷键**；若被其它程序占用导致注册失败，仅日志提示（Dock 入口仍可用，可在 config.json 改 `appCenterHotkey` 后重载配置）

## 管理员权限说明

exe 内嵌了 `requireAdministrator` 清单：**每次启动会弹 UAC**。
这是为了能控制以管理员身份运行的程序（如 Geek Uninstaller、任务管理器）——
Windows 的 UIPI 机制禁止普通权限进程操作高权限窗口。
Dock 启动应用时经 `explorer.exe` 转发，被启动的程序仍是普通权限。
不想每次弹 UAC 可以创建一个任务计划程序（schtasks /rl highest /sc onlogon）来静默启动。

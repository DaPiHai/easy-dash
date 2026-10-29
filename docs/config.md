# 配置文件参考

配置位于 `%APPDATA%\com.easydash.app\config.json`，由 easy-dash 自动读写。
修改后可在托盘右键菜单「重载配置」立即生效（任务栏、主题与材质设置也会同步应用）。

```json
{
  "pinned": [{ "name": "记事本", "path": "C:\\Windows\\System32\\notepad.exe", "args": "" }],
  "hotkey": "alt+f1",
  "fullscreenHide": true,
  "reserveTopbarSpace": true,
  "trayIcons": true,
  "notificationCenter": true,
  "controlCenter": true,
  "hideTaskbar": true,
  "topbarAllMonitors": true,
  "dockAllMonitors": false,
  "dockAutoHide": true,
  "theme": "dark",
  "material": "solid",
  "pointerGlow": true,
  "syncSystemTheme": true,
  "appCenterHotkey": "alt+space",
  "trayOrder": ["weixin#0", "qq#0"],
  "appOrder": ["lnk:c:\\windows\\system32\\notepad.exe"],
  "importedApps": [
    { "name": "计算器", "path": "C:\\Windows\\System32\\calc.exe", "args": "" }
  ]
}
```

## 外观

- `theme`：`light`（浅色）/ `dark`（深色），默认 `dark`。旧配置里的 `system` 或非法值会自动迁移为 `dark`。
  仅两态、不提供「跟随系统」，避免与系统主题互相干扰。
- `material`：界面材质，与明暗主题正交。`solid`（标准，默认）/ `liquid`（液态玻璃）/ `glow`（柔光玻璃），
  非法值自动归一为 `solid`。控制中心「外观」卡与管理面板「外观」页都能切换，即时生效。
  - `liquid` 会在面板打开前取样面板背后的桌面，做 30px 模糊 + 150% 饱和 + 自适应对比度色阶 + 蓝玻璃控件；
    顶栏 / Dock / 管理面板 / 活动概览 / 应用中心复用同一套光学（按厚度 18 / 24 / 30px 模糊）。
  - 顶栏 / Dock 的底片**常驻**，打开活动概览或 Dock 自动隐藏重显不会重截；管理面板 / 概览 / 应用中心每次打开重新取样。
  - 浅色模式不铺白底：白色阶最多 14%（常规 10%），并按最终合成亮度选择黑 / 白墨色。
  - 取样失败（远程桌面 / 受保护内容 / 非 Windows）时自动回落到半透明模拟玻璃。
- `pointerGlow`：鼠标流光（光标跟随柔光 + 边框绕行光带 + 点按光晕），默认 `true`；管理面板「外观」页可关闭。
- `syncSystemTheme`：在控制中心切亮 / 暗时是否同时修改 Windows 系统主题。

## 桌面与多屏

- `hideTaskbar`：启动时隐藏系统任务栏并接管工作区，退出时恢复；管理面板可随时切换。
- `topbarAllMonitors`：副屏是否也显示顶栏（主屏始终显示），默认 `true`。
- `dockAllMonitors`：副屏是否也显示 Dock（主屏始终显示），默认 `false`。
- `reserveTopbarSpace`：把顶栏注册为该显示器的 AppBar，最大化窗口自动避开顶栏。
- `fullscreenHide`：前台应用全屏时自动滑出隐藏顶栏与 Dock。

## Dock 与应用中心

- `dockAutoHide`：Dock 智能隐藏，默认 `true`。普通窗口压到 Dock 区域时自动滑下隐藏，鼠标移到 Dock 底边
  （屏幕 / 工作区底边 ±4px、水平两侧各 24px）时临时滑出，离开约 450ms 后再次隐藏。无窗口遮挡时常驻显示；
  全屏隐藏优先级更高。关闭「全屏自动隐藏」后，前台全屏应用不再触发智能隐藏。
  可在控制中心 / 管理面板关闭。
- `dockOrder`：Dock 图标顺序（按应用 key 持久化，如 `"deepseek harness.exe"`、`"aumid:<AUMID>"`）。
- `hotkey`：活动概览全局热键，默认 `alt+f1`。
- `appCenterHotkey`：应用中心全局热键，默认 `alt+space`。被其它程序占用导致注册失败时仅记录日志
  （Dock 九点格入口仍可用），可改此字段后「重载配置」。

## 托盘与通知

- `trayIcons`：是否在顶栏镜像系统托盘图标（Windows 11 下走托盘代理）。
- `notificationCenter` / `controlCenter`：是否启用通知中心 / 控制中心入口。
- `trayOrder`：托盘图标顺序，按稳定 key（exe 主名 / 注册表匹配 + 同源 `#序号`）持久化。

## 应用

- `pinned`：Dock 固定应用（`name` / `path` / `args`），UWP 应用记录 AppsFolder 启动器。
- `appOrder`：应用中心顺序（应用 id 持久化，新增应用追加在已排序项之后）。
- `importedApps`：应用中心「导入应用」记录的条目，仅记录路径，不复制文件、不写入开始菜单；
  仅支持 `.exe / .lnk / .url`，`args` 字段预留。

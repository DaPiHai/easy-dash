# 顶栏托盘替代方案调研：其他工具如何做到“不碰任务栏也能用托盘”

- 日期：2026-10-27
- 背景：easy-dash 当前把 Windows 通知区域图标用 **UIA 镜像**到顶栏，点击时再把任务栏临时显示出来、移动光标去点原生图标。由此产生坐标漂移、任务栏显形 60s、点击穿透、弹窗无法锚定等问题。
- 调研问题：其他同类工具（状态栏/任务栏替代/Dock）是否也需要“镜像并点击任务栏”？它们实际用的是什么方案？easy-dash 能否采用？
- 结论先行：**没有“零成本、纯公共 API”的方案**。同类工具并没有避开“从 Explorer 获取托盘图标”这件事，它们避开的是**显示/点击任务栏窗口**。目前最值得 easy-dash 采用的是 **YASB 的“托盘代理窗口 + 直接发送通知消息”** 方案（无需注入、无需显示任务栏），必要时用 **YASB 的 Explorer 注入 hook** 作为兜底。

## 0. 实施验证（2026-10-27）

已按本报告推荐方案落地 `src-tauri/src/tray_proxy.rs`，真机（Win11 26H2、1920×1080、应用以管理员运行）结果：

- 代理窗口成功注册类名 `Shell_TrayWnd` 并置顶；`ChangeWindowMessageFilterEx(WM_COPYDATA, MSGFLT_ALLOW)` 在 High 进程必需，否则 Medium 应用的注册消息被 UIPI 静默丢弃。
- 启动广播 `TaskbarCreated` 后捕获 9 个 `NIM_ADD`（`cbSize=956`、magic `0x34753423`、32 位布局），包括 easy-dash、微信、悟空、小米互联、蓝牙、必应壁纸等；tooltip/HICON/回调地址解析正确。
- 点击：`proxy btn=right key=weixin cursor=(1601,16) ok=true` 把光标移到顶栏图标中心；期间任务栏保持 `visible=true / alpha=0 / layered=true`，没有显形。`proxy btn=left key=easydash` 成功打开活动概览。
- 退出：`proxy-stop` 先销毁窗口并广播 `TaskbarCreated`，任务栏恢复到 `exstyle=0x88`、非 layered。

注意：代理会显示所有已注册图标（含原生折叠区），顶栏没有「»」；异常强杀时代理不会广播，应用图标会暂时从原生托盘消失，需重启 easy-dash/Explorer 恢复。

---

## 1. 关键认知：托盘图标本来就“属于 Explorer”

Windows 的托盘协议是：

```
应用调用 Shell_NotifyIcon(NIM_ADD/MODIFY/DELETE)
        ↓
shell32 找到系统托盘窗口（传统实现是 FindWindow("Shell_TrayWnd")）
        ↓
把 NOTIFYICONDATA（含 hWnd / uID / uCallbackMessage / hIcon / szTip / guid）
通过 WM_COPYDATA 发给托盘窗口
        ↓
Explorer 的托盘 UI 负责显示图标；点击时 Explorer 再把鼠标消息回发给应用窗口的 uCallbackMessage
```

因此，任何第三方顶栏要显示“别的应用”的托盘图标，只有四条路：

1. **成为/冒充托盘窗口**，直接接收 `Shell_NotifyIcon` 消息（YASB 的代理窗口/YASB 的 hook）；
2. **读 Explorer 的托盘数据结构**（ToolbarWindow32 + ReadProcessMemory，Cairo/RetroBar/ManagedShell）；
3. **替换整个 shell**（Cairo 的 shell 模式）；
4. **改造真实任务栏**（StartAllBack / ExplorerPatcher：把任务栏本身移到顶部/重绘）。

“完全不需要镜像”的公共 API **不存在**；所谓“不用镜像任务栏”，实际是指**不显示任务栏、不移动光标去点任务栏**，图标数据仍然来自上述渠道。

---

## 2. 方案对比

| # | 方案 | 代表工具 | 图标数据来源 | 点击方式 | 是否需注入/提权 | Win11 26H2 可行性 | 对 easy-dash 的评价 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | **托盘代理窗口** | YASB（legacy 模式） | 自己创建 `Shell_TrayWnd` 窗口，冒充托盘接收 `WM_COPYDATA` | `SendNotifyMessage` 直接发给应用 `uCallbackMessage` | **不需要**（普通进程即可） | 待 spike 验证（类注册已实测可行） | ★★★★★ 最契合：可删掉整套“点任务栏”逻辑 |
| 2 | **注入 Explorer 子类化** | YASB（默认模式） | DLL 注入 `explorer.exe`，子类化真实 `Shell_TrayWnd`，截获 `WM_COPYDATA` | 同上 | 需要 SetWindowsHookEx 注入（同用户、同位数即可，不强制管理员） | ✅ 已验证（YASB 默认） | ★★★★☆ 最可靠，但引入 DLL/管道/杀软风险 |
| 3 | **内存读取 ToolbarWindow32** | ManagedShell（Cairo/RetroBar 底层）、andrewiankidd/dock | `Shell_TrayWnd → TrayNotifyWnd → SysPager → ToolbarWindow32`，`TB_GETBUTTON` + `ReadProcessMemory` | 读到的 hWnd/uID 后发消息 | 不需要注入，但需 OpenProcess/ReadProcessMemory | ❌ 本机实测 Win11 26H2 无 `ToolbarWindow32`（`TrayNotifyWnd` 子窗口 0 个） | ✗ 仅 Win10/经典任务栏可用 |
| 4 | **Shell 替换（自建托盘窗口）** | Cairo（ManagedShell shell 模式） | 自建 `Shell_TrayWnd`/`TrayNotifyWnd` + `SysTrayObject`（`CGID_SHELLSERVICEOBJECT`） | 原生 | 需要 Explorer 不运行/注册为 shell | ✅（但等于换 shell） | ✗ 侵入性过大 |
| 5 | **改造真实任务栏** | StartAllBack、ExplorerPatcher | 真实任务栏本身（原生托盘） | 原生 | 需要 patch/replace Explorer 任务栏 | ✅ StartAllBack 支持 26H2 | ✗ 与 easy-dash 的独立顶栏目标冲突 |
| 6 | **UIA 镜像 + 指针转发** | easy-dash 现状 | UIA 枚举任务栏按钮 | 显示任务栏 + SetCursorPos/mouse_event | 需管理员（改任务栏样式） | ✅（但窗口/菜单难用） | ✗ 已证明问题多 |
| 7 | **不显示托盘，保留原生任务栏** | TaskbarX/TranslucentTB 一类 | — | 原生 | 不需要 | ✅ | 最省事但产品形态不同（顶栏无托盘） |

---

## 3. 各方案细节与代码证据

### 3.1 托盘代理窗口（YASB legacy，最推荐给 easy-dash）

YASB（5.7k★，Windows 10/11 状态栏）的 `SystrayMonitor` 做了三件事：

1. **创建一个窗口类名为 `Shell_TrayWnd` 的顶层窗口**，并设为 `WS_EX_TOOLWINDOW | WS_EX_TOPMOST`、`WS_POPUP`，尺寸 0×0：
   - `NativeWindowEx(..., "Shell_TrayWnd")`：`RegisterClassW` + `CreateWindowEx`；
   - `SetWindowPos(..., HWND_TOPMOST, ...)`。
2. **广播 `TaskbarCreated`**，让所有应用重新 `Shell_NotifyIcon(NIM_ADD)`；由于代理窗口是 topmost，`shell32` 的 `FindWindow("Shell_TrayWnd")` 会先找到它，于是所有 NIM 消息都进了代理进程。
3. **把消息原样转发给真正的 `Shell_TrayWnd`**（排除自己），所以系统自带任务栏的托盘图标仍然正常；代理只“旁路复制”一份数据。

YASB 源码位置：

- 代理窗口创建：`src/core/widgets/services/systray/utils.py` L92-132（`RegisterClassW` + `CreateWindowEx`）
- 启动/置顶/消息循环：`src/core/widgets/services/systray/systray_monitor.py` L81-105
- 广播 `TaskbarCreated`：`src/core/widgets/yasb/systray.py` L255-261
- 解析 `WM_COPYDATA`/转发：`systray_monitor.py` L144-200
- 点击：`systray_widget.py` L177-218

点击部分最关键：

```python
AllowSetForegroundWindow(app_pid)
SendNotifyMessage(hwnd, uCallbackMessage, wParam, lParam)   # WM_LBUTTONDOWN/UP / WM_RBUTTONDOWN/UP
# uVersion >= 3 时再补 NIN_SELECT / NIN_CONTEXTMENU
```

也就是说：**不需要显示任务栏、不需要移动光标、不需要点击屏幕上的托盘图标**。应用收到的消息与 Explorer 平时发给它的一致；右键菜单由应用自己 `TrackPopupMenu` 在**当前光标位置**弹出——用户光标本来就在顶栏图标上，菜单天然锚定在顶栏下方。

> 本机验证：在 Win11 26H2 上，向当前进程注册 `Shell_TrayWnd` 类名返回成功（atom=49552，err=0），说明“同进程伪造同名窗口类”可行；但 `Shell_NotifyIcon` 是否确实选择 topmost 的代理窗口，仍需 1 天 spike 实测（见 §6）。

**优点**
- 纯 Win32 消息，不需要 UIA、不需要坐标、不需要任务栏可见/透明切换；
- 实时（NIM_ADD/MODIFY/DELETE），不再 10s 轮询；
- 无 DLL 注入，杀软风险远低于 hook；
- 点击后菜单天然在顶栏图标下方，解决“锚定”问题；
- Explorer 重启后重新广播即可恢复。

**风险**
- 依赖 `Shell_NotifyIcon`“找 topmost `Shell_TrayWnd`”这一未公开行为；Windows 更新可能改变；
- 其他同样 `FindWindow("Shell_TrayWnd")` 的程序会找到代理窗口（可用窗口属性/短暂置顶缓解，但无法完全避免）；
- 代理崩溃/退出时必须重新广播 `TaskbarCreated`，否则应用以为图标已注册而原生托盘不显示；
- 同时运行多个代理（另一个顶栏工具）会争抢 topmost，需要检测并提示；
- 应用若缓存了旧托盘 HWND（极少数）可能绕过代理。

### 3.2 注入 Explorer + 子类化（YASB 默认，可靠但重）

YASB 默认 `use_hook: true`（`src/core/validation/widgets/yasb/systray.py` L34）：

- 用 `SetWindowsHookEx(WH_GETMESSAGE, ...)` 把 `YASBTrayHook.dll` 注入 `explorer.exe`；
- DLL 在 Explorer 内用 `SetWindowLongPtrW(Shell_TrayWnd, GWLP_WNDPROC, ...)` **子类化真实托盘窗口**，截获 `WM_COPYDATA`（`SHELLTRAYDATA`）；
- 通过命名管道把 `NOTIFYICONDATA` + 图标 RGBA 发给 YASB 进程；YASB 负责显示与点击（点击仍走 `SendNotifyMessage`）。

证据：
- `systray_hook.py` L92-120（注入）
- `trayhook.cpp` L331/L431（截获 + 子类化）
- YASB 也支持 `use_hook: false` 回退到 3.1 的代理窗口。

**优点**：捕获最可靠，不受 `FindWindow` 行为影响；不创建伪 `Shell_TrayWnd`，副作用小。
**缺点**：注入 explorer 有杀软误报/稳定性风险；需要额外 DLL、位数匹配、命名管道、Explorer 重启重注入；YASB 自身也遇到过“注入后 DLL 连不回来”的 issue（#1045）。

### 3.3 ToolbarWindow32 内存读取（ManagedShell / RetroBar / andrewiankidd/dock）

ManagedShell（Cairo 的底层库，RetroBar 也基于它）的 `ExplorerTrayService`：

- `FindWindowEx(Shell_TrayWnd → TrayNotifyWnd → SysPager → ToolbarWindow32)`；
- `TB_BUTTONCOUNT` / `TB_GETBUTTON` + `ReadProcessMemory` 读出 `TRAYDATA`（hWnd/uID/uCallbackMessage/hIcon/tip）；
- 用 `ITrayNotify::EnableAutoTray(false)` 临时展开隐藏图标，读完再恢复。

证据：
- `ManagedShell.WindowsTray/ExplorerTrayService.cs` L61-155
- `andrewiankidd/dock` README 明确写 “cross-process memory reads for the tray / Mirror the notification area”

**但在 Win11 26H2 上不可用**（本机实测）：

```
Shell_TrayWnd children:
  Windows.UI.Composition.DesktopWindowContentBridge
  Start
  TrayDummySearchControl
  TrayNotifyWnd      ← 子窗口数 = 0
  ReBarWindow32
  MSTaskSwWClass
  MSTaskListWClass
```

没有 `SysPager` / `ToolbarWindow32`。因此这条路只适用于 Win10/经典任务栏（或 ExplorerPatcher 的 Win10 任务栏）。

### 3.4 Shell 替换（Cairo / ManagedShell shell 模式）

ManagedShell 的 `ShellServiceObject` 仅在 `EnvironmentHelper.IsAppRunningAsShell == true` 时启动：

- 创建自己的 `Shell_TrayWnd`/`TrayNotifyWnd`；
- 通过 `SysTrayObject`（CLSID `35CEC8A3-2BE6-11D2-8773-92E220524153`）+ `CGID_SHELLSERVICEOBJECT` 注册 shell service object，从而成为系统托盘宿主。

这是“完全原生”的方案，但要求 Explorer 不运行/替换 shell。对 easy-dash 来说侵入性过大，不推荐。

### 3.5 改造真实任务栏（StartAllBack / ExplorerPatcher）

- **StartAllBack** 官方特性明确写有 “Move taskbar to top, left or right edges”，且任务栏是其“fully reimplemented taskbar”；系统托盘仍是原生托盘。
- **ExplorerPatcher** 支持 Windows 10 任务栏（可停靠 top/bottom/left/right）；Windows 11 任务栏的 top 仅在 22H2 之前支持，26H2 需切到 Win10 任务栏样式。

这条路能 100% 保留托盘原生行为，但等于**替换/改造系统任务栏**，与 easy-dash 的“独立 GNOME 风格顶栏”目标冲突；而且不开源/需 patch Explorer，不适合作为产品方案（可作为用户侧替代方案说明）。

### 3.6 现状：UIA 镜像 + 指针转发（easy-dash）

不再赘述，见评审报告：坐标依赖旧缓存、需要显示任务栏、60s hold、点击穿透、菜单无法锚定。根因是**用“屏幕坐标 + 真实鼠标”去点一个不可见/透明的任务栏**，而不是用托盘协议本身。

---

## 4. 对 easy-dash 的推荐架构

### 4.1 目标形态

```
┌──────────────────────────────────────────────────────────┐
│ easy-dash topbar                                          │
│  [活动] ... [托盘图标…] [网络/音量/电池] [时钟]            │
└──────────────────────────────────────────────────────────┘
        ▲ 图标数据（NIM_ADD/MODIFY/DELETE）
        │
┌───────┴──────────────────────────────────────────────────┐
│ tray proxy（easy-dash 进程内）                             │
│  · 创建 topmost、0×0、class="Shell_TrayWnd" 的窗口          │
│  · 广播 TaskbarCreated，接收所有 NIM 消息                   │
│  · 同步转发给真实 Shell_TrayWnd（原生托盘继续可用）          │
│  · 点击时 AllowSetForegroundWindow + SendNotifyMessage      │
│    （WM_LBUTTONUP / WM_RBUTTONUP / NIN_SELECT / NIN_CONTEXTMENU）│
└───────┬──────────────────────────────────────────────────┘
        │ 转发
┌───────▼──────────────────────────────────────────────────┐
│ Explorer 的真实任务栏（可继续隐藏/透明，仅保留消息泵）       │
└──────────────────────────────────────────────────────────┘
```

### 4.2 与现有代码的映射

| 现有实现 | 替换为 |
| --- | --- |
| `winapi/tray.rs::enumerate`（UIA 全树） | `tray_proxy` 的消息回调（内存中的 icon 表） |
| `tray_click` 的 `begin_hold` + `SetCursorPos` + `mouse_event` | `SendNotifyMessage(hwnd, uCallbackMessage, ...)` + `AllowSetForegroundWindow` |
| `taskbar.rs` 的 hold / alpha=255 / clickable_guard / 60s 轮询 | **删除该路径**（保留“隐藏任务栏/恢复工作区”能力，但托盘不再依赖它） |
| `ui/js/tray.js` 的 10s 轮询 + `refresh_tray` | 事件驱动（Tauri `tray-changed` 直接推送增量） |
| 弹窗检测/锚定 | 不再需要：菜单由应用在光标处弹出 |
| `tray_click` 的 order_key 兜底 | 保留（列表仍可能变化，但数据源实时） |

### 4.3 点击流程（关键）

```
用户点击顶栏图标（光标已在图标上）
  → 前端 invoke tray_click(key, button)
  → 后端查 proxy 表：{hwnd, uID, uCallbackMessage, uVersion}
  → AllowSetForegroundWindow(owner_pid)
  → SendNotifyMessage(hwnd, uCallbackMessage,
        wParam = (uVersion>3 ? MAKELPARAM(cursor.x,cursor.y) : uID),
        lParam = MAKELPARAM(WM_RBUTTONUP or WM_LBUTTONUP, 0))
  → 若 uVersion>=3 再补 NIN_CONTEXTMENU / NIN_SELECT
  → 应用在光标处弹菜单 → 天然锚定在顶栏下方
```

### 4.4 兜底策略

- 代理窗口启动失败 / 收不到任何 NIM 消息（Win 版本差异）→ 回退 **YASB 式 hook**（DLL 注入）；
- 代理/hook 都失败 → 保留现有 UIA 路径作为最后兜底（但不再默认）；
- 对少数不响应 `SendNotifyMessage` 的应用 → 记录到 `tray-click.log`，提供“使用原生托盘”降级提示；
- 系统图标（音量/网络/电池）不进入代理托盘列表，继续由 easy-dash 控制中心负责（避免 Shell flyout 不可见问题）。

---

## 5. 与当前修复计划的关系

- 原评审中的 **P0-1（锚定）/ P0-3（坐标漂移）/ P0-2（60s 显形）** 在新架构下**整体消失**，不需要再单独修“搬窗口/重枚举/降 hold”；
- **P0-4（残留任务栏）** 仍需修复：隐藏任务栏与工作区接管逻辑保留；
- 前一份修复计划的 **M0 止血** 仍然值得先做（文档如实、恢复自愈、失败 toast），因为新架构要到 v0.3 才能落地；
- 建议 feature flag：`trayBackend = "uia" | "proxy" | "hook"`，默认先 `uia`，代理验证通过后切默认，保留一个版本回退。

---

## 6. Spike 计划（1–2 天，先验证再改架构）

### Day 1：代理窗口可行性

用一个最小 Rust/C# 程序（不并入 easy-dash）验证：

1. 注册类名 `Shell_TrayWnd`，创建 0×0 topmost 窗口，启动消息循环；
2. 找到真实 `Shell_TrayWnd`（排除自己），广播 `TaskbarCreated`；
3. 统计 3 秒内收到的 `WM_COPYDATA` 数量、解析出的图标名/hWnd/uID，并**全部转发**给真实托盘；
4. 退出前销毁代理窗口，再次广播 `TaskbarCreated`；
5. 观察：原生托盘图标是否完整、是否重复、是否闪烁；Explorer 是否异常。

**Go 条件**：
- 3 个当前图标全部收到（3/3），且原生托盘在代理运行/退出后都正常；
- 代理退出后，应用能重新注册到真实托盘（无图标丢失）；
- 不出现 Explorer 崩溃、任务栏卡死。

### Day 2：点击与菜单锚定

1. 选一个第三方图标（微信/DeepSeek），在“光标位于顶栏图标”的前提下，`SendNotifyMessage` 发右键序列；
2. 验证菜单是否出现在顶栏图标下方（截图）；
3. 验证左键打开/聚焦应用；
4. 测试 `uVersion < 3` 的应用（只发 WM_RBUTTONUP）与 `uVersion >= 3` 的应用（补 NIN_CONTEXTMENU）。

**No-Go 时**：转向 hook spike（DLL 注入 + 管道），预计再 1–2 天。

### 验收清单

- [ ] 代理运行 10 分钟，托盘图标全部可见、可点；
- [ ] 右键菜单锚定在顶栏图标附近（无需任务栏显形）；
- [ ] 任务栏保持隐藏/透明，工作区不跳变；
- [ ] Explorer 重启后代理自动重建，图标恢复；
- [ ] 代理退出/崩溃后，原生托盘图标恢复（重新广播）；
- [ ] 不要求管理员权限（隐藏任务栏仍可要求，但托盘代理本身不需要）。

---

## 7. 风险与取舍

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| 伪 `Shell_TrayWnd` 依赖未公开行为 | 某些 Windows 版本收不到消息 | spike 先行；hook 兜底；版本探测 |
| 其他程序 `FindWindow("Shell_TrayWnd")` 找到代理 | 第三方工具行为异常 | 代理窗口加特殊属性/类名检测；文档说明；提供开关 |
| 代理崩溃导致应用以为图标已注册 | 原生托盘图标消失 | 退出/崩溃时广播 `TaskbarCreated`；watchdog；启动时自检 |
| 多代理冲突（YASB/其他 topbar） | 消息被抢、图标错乱 | 检测同名 topmost 窗口并提示/让位 |
| hook 注入被杀软拦截 | 功能不可用 | 优先代理；DLL 签名；提供“仅 UIA”模式 |
| 点击消息被应用忽略 | 个别图标点不动 | 记录并回退到 UIA 点击该图标；应用白名单 |
| `TaskbarCreated` 广播造成图标重建 | 短暂闪烁 | 低频、仅在启动/恢复时；避免每次刷新都广播 |

---

## 8. 参考

- YASB：<https://github.com/amnweb/yasb>
  - 代理窗口：`src/core/widgets/services/systray/systray_monitor.py`、`utils.py`
  - 点击：`src/core/widgets/services/systray/systray_widget.py`
  - Hook：`src/core/widgets/services/systray/systray_hook.py`、`hook/trayhook.cpp`
- ManagedShell（Cairo/RetroBar 底层）：<https://github.com/cairoshell/ManagedShell>
  - `ManagedShell.WindowsTray/ExplorerTrayService.cs`（ToolbarWindow32 + ReadProcessMemory）
  - `ManagedShell.WindowsTray/ShellServiceObject.cs`（shell 模式）
- RetroBar：<https://github.com/dremin/RetroBar>
- Cairo Shell：<https://github.com/cairoshell/cairoshell>
- StartAllBack（Move taskbar to top）：<https://www.startallback.com/>
- ExplorerPatcher（taskbar 位置）：<https://github.com/valinet/ExplorerPatcher/wiki/All-features>
- andrewiankidd/dock（跨进程读托盘）：<https://github.com/andrewiankidd/dock>
- 本机实测（2026-10-27，Win11 26H2）：`Shell_TrayWnd` 子窗口无 `ToolbarWindow32`；`TrayNotifyWnd` 子窗口 0 个；进程内注册 `Shell_TrayWnd` 类成功（atom=49552）。

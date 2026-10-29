# easy-dash 交接报告（v0.2.1，2026-10-26 → 2026-10-27 真机验证）

> **2026-10-27 多屏适配（v0.3.0）**：已完成每屏顶栏 / 按屏 AppBar / 按屏全屏隐藏 / 概览与应用中心跟随触发屏 / 托盘坐标物理化；控制中心新增「多屏」卡片（副屏顶栏、副屏 Dock），详见 `docs/releases/v0.3.0.md` 与 `docs/releases/v0.3.0-verification.md`。本文件其余章节是 v0.2.1 的历史记录。

> 用途：在新会话中继续完成 v0.2.1 的**真机运行时验证**与遗留收尾。
> 仓库：`D:\codes\easy-dash`（分支 `main`，HEAD = `7b93eee` v0.1.0）
> 状态：**全部改动未 commit**（41 个已跟踪文件修改 + 12 个新文件，含本轮 `taskbar.rs` / `tray.rs` 修复），工作区即最终实现。

---

## 0. 2026-10-27 真机验证结果（本会话新增，先读这一节）

**P0-2「隐藏任务栏后最大化窗口铺满全屏」原实现是真机失败的，本轮已定位根因、修复并在真机验证通过。**

- 根因：`ui/js/tray.js` 托盘镜像每 10 s 刷新 → `taskbar::bridged()/begin_hold()` → `show_taskbar()` 把 `SW_HIDE` 的任务栏 `SW_SHOW` 一次；
  任务栏一旦「显示」，Explorer 立即按仍注册的任务栏 AppBar 把底部 48 px 重新算回工作区
  （100 ms 采样：`(0,32)-(1920,1080)` 每 10 s 回落 `(0,32)-(1920,1032)` 约 0.5 s），最大化窗口被压回 1034 且**永久不会长回**。
  即 `SPI_SETWORKAREA` 静默接管本身没错，但任何一次可见性跳变都会冲掉它。
- 修复：改为「隐身模式」——任务栏窗口**保持 visible** + `WS_EX_LAYERED` alpha 0 + `WS_EX_TRANSPARENT`（鼠标穿透），
  没有任何显示/隐藏跳变 ⇒ Explorer 不再重算工作区；托盘点击时才用 `clickable_guard()` 临时关穿透 ~100 ms。
  `restore_all()` 与桥接串行并同时清除 layered/穿透（含崩溃残留自愈）；`SW_HIDE` 仅作兜底。
- 真机验证（交付构建）：100 ms 采样 20 s / 25 s → **0 次工作区跳变**；工作区稳定 `(0,32)-(1920,1080)`；
  新建 Notepad 最大化后 40 s（跨 4 次托盘刷新）`visibleFrame` 底边恒为 **1080**（铺满）。
  任务栏状态：`visible=True layered=True transparent=True alpha=0 exstyle=0x800A8`。
- 强杀恢复：`taskkill /F` → 任务栏保持隐身 + marker 存在；重启 → `recover_stale_lock()` 恢复并重新接管 ✅。
- 收尾已用提权清理把桌面还原为正常（`taskbar visible/非 layered/exstyle=0x88`、`workarea (0,0)-(1920,1032)`、marker 删除）。
- 交付物已重建：`src-tauri/target/release/easy-dash.exe` 与
  `src-tauri/target/release/bundle/nsis/easy-dash_0.2.1_x64-setup.exe`（22:5x 之后，含本次修复）。
- 详细证据与复现命令见 **`docs/releases/v0.2.1-verification.md`**。
- 仍未在真机用真实 UI 动作验证过的项（本会话顶栏 WebView 的程序化点击会穿透到下层窗口，属注入工具限制）：
  托盘左键面板 / 右键菜单的人工确认、正常退出 / 关开关 / 重载配置的恢复、面板（控制中心/通知中心）交互、主题切换与注册表写入、热键回归。
  → 请按 `docs/releases/v0.2.1-verification.md` §5 的 6 条清单人工点一遍（约 1 分钟）。

---

## 1. 当前完成度（一句话）

用户提出的两轮需求（v0.2.0 四大功能 + v0.2.1 九项实机反馈）**代码已全部实现并通过静态/单元/视觉预览验证**，
`0.2.1` 安装包已构建；唯一未完成的是**真实桌面上的端到端运行验证**（尤其是「隐藏任务栏后窗口铺满全屏」和「顶栏托盘图标点击」），
因为上一会话环境里非提升（非管理员）进程启动 Tauri GUI 会触发 WebView2 崩溃（见 §6），而用户自己安装的 0.2.0 能正常运行。

---

## 2. 交付物 / 版本

| 项 | 值 |
| --- | --- |
| 版本 | `0.2.1`（package.json / src-tauri/Cargo.toml / tauri.conf.json / Cargo.lock 均已同步） |
| 安装包 | `src-tauri/target/release/bundle/nsis/easy-dash_0.2.1_x64-setup.exe`（2.02 MiB，20:54 构建） |
| 便携 exe | `src-tauri/target/release/easy-dash.exe`（含 `requireAdministrator` 清单，启动会弹 UAC） |
| 设计规范 | `docs/design-system.md`（Aurora Glass，v0.2.1 组件规格已更新） |
| 发布说明 | `docs/releases/v0.2.1.md`（v0.2.0 说明也保留） |
| 变更记录 | `CHANGELOG.md`（含 `[0.2.1]`） |
| 验证截图 | `%TEMP%\easydash-qa\`（约 47 个文件，含 cc/nc/launcher/manager/theme-r2 截图与 mock harness） |
| 探针/日志 | `%TEMP%\easydash-verify\`（taskbar-state.ps1、probe-*.ps1、构建日志） |

> 未 commit、未打 tag。验证通过后建议 `git add -A && git commit`（注意 `.dsh-ops/` 是 DSH 内部目录，可保留或加入 .gitignore）。

---

## 3. 本轮（v0.2.1）九项反馈的实现位置

| # | 用户反馈 | 实现 | 关键文件 |
| --- | --- | --- | --- |
| 1 | 浅色主题托盘图标被半透明黑底盖住 | `.tray-dark` 浅色垫底限定 `:root[data-theme="dark"]`；`.tray-invert` 仅浅色；主题切换清 `dataset.trayTheme` 强制重算 | `ui/css/topbar.css`、`ui/js/tray.js` |
| 2 | 隐藏任务栏后窗口无法占满全屏 | 隐藏时保存原始工作区 → **静默** `SPI_SETWORKAREA` 到 `{0,顶栏物理高,屏宽,屏高}`；watchdog ≥1s 复查纠正；退出/关开关/重载/强杀重启恢复（marker 记录 workarea） | `src-tauri/src/winapi/taskbar.rs`、`commands.rs`、`lib.rs`、`monitor.rs` |
| 3 | 磁贴图标放大越界 | 图标泡 `overflow:hidden`、svg 18px + max 100%；hover/active 不再缩放磁贴 | `ui/css/panels.css` |
| 4 | 白 Dock 九点格白色不可见 | SVG `fill="currentColor"` + `.dock-btn { color: var(--text) }` | `ui/dock.html`、`ui/css/dock.css` |
| 5 | 去掉「跟随系统」 | 前后端只留 light/dark；`system`/非法值归一 `dark`；默认 `dark`；旧配置/localStorage 自动迁移；删除 monitor 系统主题轮询；控制中心外观改 2 段；管理面板文案/状态更新 | `config.rs`、`commands.rs`、`monitor.rs`、`ui/js/theme.js`、`ui/js/control-center.js`、`ui/js/manager.js`、五个 `ui/*.html` head 脚本 |
| 6 | 日历集成中国农历 | 新增 `ui/js/lunar.js` + `lunar-table.js`（1901–2100，.NET `ChineseLunisolarCalendar` 生成）；日期格第二行农历/节日、标题完整农历、头部干支生肖 | `ui/js/lunar.js`、`ui/js/lunar-table.js`、`ui/js/calendar.js`、`ui/css/panels.css` |
| 7 | 托盘图标无实际功能 | `TaskbarHold` 引用计数桥接：点击时任务栏 alpha=0 保持显示，托盘弹窗关闭后再隐藏；无弹窗→可见兜底→PostMessage 兜底；诊断日志 `%APPDATA%\com.easydash.app\tray-click.log` | `src-tauri/src/winapi/taskbar.rs`、`tray.rs`、`commands.rs` |
| 8 | 通知堆叠不明显 | 堆叠层内缩 8/16px、底部露出 4/9px、可见边框+表面阶梯、圆角 14、右上「N 条」徽标；展开隐藏层 | `ui/css/panels.css`、`ui/js/notifications.js` |
| 9 | 控制中心去连接模块 + 磁贴双区交互 | 删除主视图「连接」卡片；Wi-Fi/蓝牙= 图标按钮开关 + 文字区按钮进连接列表（chevron/aria/焦点环）；飞行模式整块切换；窗口高 620→560、面板 476、卡片间距回 8px、无滚动条 | `ui/js/control-center.js`、`ui/css/panels.css`、`ui/js/panels.js` |

---

## 4. 已验证（可信）

- **Rust**：`cargo test --lib` → **111 passed / 0 failed / 11 ignored**；`cargo check --all-targets` → **0 warning**。
  - 命令（需设置 TEMP 避开权限问题）：`cd src-tauri && TEMP=D:\codes\easy-dash\src-tauri\target\tmp TMP=... EASY_DASH_NO_ADMIN=1 cargo test --lib`
  - 新增工作区/标记/主题迁移单测；`smoke_real_taskbar` 冒烟测试扩展为断言工作区接管/还原（默认 `#[ignore]`，需**管理员终端**、不带 `EASY_DASH_NO_ADMIN` 构建执行）。
- **前端**：7 个 CSS + 18 个 JS 的 `node --check` 全过；CSS token 审计无缺失（`--pct` 由 JS 内联注入，有 fallback）；`git diff --check` 干净。
- **视觉预览**：用 headless Chromium + 真实 `ui/js`+`ui/css` + mock Tauri（`%TEMP%\easydash-qa\qa.html`、`server.js`、`mock-tauri.js`）核对：
  - 控制中心新磁贴交互/2 段外观/off 态、Wi-Fi 子视图、通知堆叠+农历、空态、展开态、电源菜单、管理面板亮暗、Dock/顶栏亮暗。
  - 4px 网格/圆角/对比度/无裁切/焦点环/键盘 Tab 顺序均有 DOM 实测记录。
- **农历正确性**：与 .NET `ChineseLunisolarCalendar` 对照 **3595 个日期 0 差异**（含闰月；2026-10-01=八月廿一，2025-07-25=闰六月初一，2026-02-17=正月初一）。
- **任务栏机制**：独立 Win32/Rust 探针在用户上下文成功隐藏/恢复任务栏（`IsWindowVisible` false→true）。
- **SPI_SETWORKAREA 探针**：广播 `SPIF_SENDCHANGE` 会被 Explorer 立刻按 AppBar 重算覆盖；**静默设置生效**（0,32,1920,1080 保持），故 Rust 采用静默设置 + watchdog 纠正。

## 5. 未验证 / 需要新会话完成（按优先级）

> **2026-10-27 更新**：P0-2 已完成（见 §0）；P0-3 后端链路有日志证据、P0-4 强杀恢复已验证；
> 其余仍建议按 `docs/releases/v0.2.1-verification.md` §5 用真实鼠标点一遍。

### P0 — 真机运行 0.2.1，验证核心修复
1. **安装 0.2.1**（覆盖 0.2.0）或运行 `src-tauri\target\release\easy-dash.exe`（UAC 允许）。
2. **主屏最大化窗口是否铺满到底**（修复 #2 核心）：
   - 打开记事本 → 最大化 → 看窗口底边是否到达屏幕 1080 像素处（任务栏已隐藏）。
   - 注意：**已存在的最大化窗口**需先还原再最大化一次才使用新工作区。
   - 若仍留条带：抓 `easy-dash.exe` stderr（release 无控制台，可用 DebugView/ProcMon 或加日志）里「设置工作区失败」；考虑改用 `ABM_REMOVE` 任务栏 AppBar（有恢复风险）或开启系统自动隐藏任务栏设置（StuckRects3）等替代方案。
   - 多屏：**副屏工作区仍保留任务栏条带**（SPI_SETWORKAREA 只影响 primary），属已知限制。
3. **顶栏托盘图标左/右键**是否有菜单/面板：
   - 每次点击后看 `%APPDATA%\com.easydash.app\tray-click.log` 最后几行：
     - `mode=invisible-mouse` 且 `popup=0x...` → 成功；
     - 三条尝试都 `popup=-` → Shell 浮窗未被 `relocate_popup` 的窗口类规则识别（可放宽类名规则/UIA 兜底）；
     - `err` 非 0 → 点击本身失败（权限/UIPI/坐标）。
   - 保持显示期间 Dock 可点（指针穿透只限点击+重定位阶段）。
   - 若弹窗出现后立即消失：检查 hold 是否覆盖到弹窗关闭（≤60s 轮询）。
4. **任务栏恢复路径**：正常退出、管理面板关闭「隐藏系统任务栏」、托盘菜单「重载配置」、`taskkill /F /IM easy-dash.exe` 后重启，四种情况任务栏与工作区都应恢复原状。
5. **主题**：控制中心只出现浅色/深色；切主题 5 个窗口同步；旧配置 `theme:"system"` 启动后为深色；开启「同时控制系统主题」时注册表 `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\AppsUseLightTheme` 被写入。
6. **托盘浅色图标**不再有黑底；白 Dock 九点格可见。
7. **控制中心新交互**：点 Wi-Fi 图标=开关、点文字区=进网络列表；蓝牙同理；飞行模式整块；面板高度 560、无滚动条、不裁切。
8. **通知中心**：堆叠是否足够明显（用户原始诉求），农历日期与节日优先级、干支标题、空态、展开。
9. **农历/主题/通知**之外，回归旧功能：Dock、活动概览、应用中心分页/滚轮/拖拽、管理面板各开关、热键。

### P1 — 代码收尾
- 评审 `git diff`（39+10 文件），确认无临时产物、无越 scope。
- 建议提交：`git add -A`（先决定 `.dsh-ops/` 是否忽略）→ commit → 打 tag `v0.2.1`。
- 如需发布：确认 `docs/releases/v0.2.1.md`、README 的 config 示例（theme 默认 dark）与已知限制一致。
- 可选改进（低优先）：
  - `commands.rs::set_theme` 在写系统主题**之后**再 emit `theme-changed`（当前先 emit，`systemDark` 可能滞后一次；管理面板下次 `get_theme_state` 会读注册表纠正）。
  - 通知堆叠可再增强（如主卡底部阴影 + 层间 2px 真实缝）。
  - 强调色实心控件白字 3.77:1 已作为设计例外记录（`docs/design-system.md` §6.1），若要 WCAG AA 需调深 dark `--accent`。
  - 控制中心磁贴 84px / 滑条行 32px 为 560 高度约束下的取舍，已记录。

### P2 — 环境/清理
- 上会话已清理：scheduled task `easydash-verify` 已删除；git worktree `D:\codes\easy-dash-v010` 已移除；WebView2 profile 已还原（`%LOCALAPPDATA%\com.easydash.app\EBWebView`）；用户配置未变（`%APPDATA%\com.easydash.app\config.json`，mtime 08:59，备份在 `%TEMP%\easydash-verify\config.backup.json`）。
- 临时 QA/探针文件都在 `%TEMP%`，可随时删除；仓库内无 png 残留。
- `ui/js/lunar-table.js` 由脚本生成（`%TEMP%\easydash-verify\gen-lunar-table.ps1`），如需重生成可复用。

---

## 6. 环境与已知坑（新会话必读）

1. **Tauri GUI 在非提升进程里起不来**（上会话实测）：`msedgewebview2.exe` 崩溃（WER: `msedge_elf.dll` 异常 `0x80000003`；`CreateCoreWebView2EnvironmentWithOptions` 返回 `0x8000FFFF`/`0x800700AA`），`npm run dev`、直接跑 debug exe、计划任务（Limited）均复现；WebView2 156 的 Edge 本身正常。**但用户安装的 0.2.0（requireAdministrator）能正常显示顶栏/Dock/面板**，因此推测与**管理员权限/上下文**有关。新会话若要跑 GUI：
   - 让用户在桌面双击安装包/安装后的 exe（UAC 同意）；
   - 或从 DSH 用 CUA `launch_app` 启动 `src-tauri\target\release\easy-dash.exe`（会弹 UAC，需用户点「是」）；
   - 不要用 `npm run dev` 作为唯一验证手段。
2. **DSH bash 无法启动 GUI 进程**（上会话 `notepad.exe` 无进程），GUI 用 `cua_driver_native` 工具（若提示 "session has ended" 先 `start_session`）。
3. **Rust 测试**需 `EASY_DASH_NO_ADMIN=1`（否则测试 exe 继承 requireAdministrator 清单，非提升会话起不来）；`TEMP/TMP` 指向 `src-tauri\target\tmp` 避免沙箱权限问题。
4. **Cargo 构建脚本**：`src-tauri/build.rs` 已加 `cargo:rerun-if-changed=app.manifest`；若切换 `EASY_DASH_NO_ADMIN`/清单后 manifest 不生效，`touch build.rs` 或 `cargo clean -p easy-dash` 再构建。构建后用
   `grep -a -o "requestedExecutionLevel[^<]*" src-tauri/target/release/easy-dash.exe` 验证是 `requireAdministrator`。
5. **PowerShell 探针**（`%TEMP%\easydash-verify\`）：`taskbar-state.ps1` 查任务栏可见/layered；`probe-workarea2.ps1` 可复现「广播被覆盖、静默生效」；`cleanup-taskbar.ps1` 清 layered 残留。注意探针脚本的「还原」要还原**清理后**的 exstyle（上会话踩过坑）。
6. **版本号改后**记得 `cargo metadata`/`cargo check` 更新 `Cargo.lock`（当前已是 0.2.1）。
7. **不要 git commit**（上会话约定）；由你/用户决定提交时机。

---

## 7. 关键接口/文件速查

- `src-tauri/src/winapi/taskbar.rs`（新，~1250 行）：`apply(hide)` / `restore_all()` / `watchdog()` / `begin_hold(alpha) -> TaskbarHold` / `with_taskbar_visible` / `state()` / `set_topbar_height(px)` / `update_topbar_height(app)` / marker `taskbar-hidden.lock`（含 workarea 行）/ 静默 `set_workarea(rect, notify)`。
- `src-tauri/src/commands.rs`：`get_theme_state`、`set_theme(mode)`（只接受 light/dark，sync 时写系统主题）、`get_taskbar_state`、`tray_click`（hold + 兜底 + 日志）。
- `src-tauri/src/config.rs`：`hide_taskbar`(默认 true)、`theme`(默认 "dark")、`sync_system_theme`(默认 true)、`normalize_theme`（system/非法→dark）。
- `src-tauri/src/monitor.rs`：每 tick `taskbar::watchdog()`；已删除系统主题轮询。
- `ui/js/theme.js`：`getThemeState / setThemeMode / subscribeTheme / resolvedTheme`；只支持 light/dark；旧 localStorage "system"→dark。
- `ui/js/panels.js`：`PANEL_HEIGHTS = { notifications: 620, control: 644 }`（v0.3.0 起控制中心含多屏卡片）。
- `ui/js/lunar.js`：`solarToLunar / formatLunarShort / formatLunarFull / ganzhiYear`（范围 1901-02-19 .. 2100）。
- 设计规范：`docs/design-system.md`；发布说明：`docs/releases/v0.2.1.md`；变更：`CHANGELOG.md`。

## 8. 建议的下一步操作顺序（新会话直接照做）

1. 读 `HANDOFF.md`（本文件）→ `git status` 确认工作区仍是 49 项（39 M + 10 ??）。
2. 让用户安装/启动 `easy-dash_0.2.1_x64-setup.exe`（或 release exe）。
3. 按 §5 P0 逐项验证；每项记录结果（截图/日志），特别是：
   - 主屏最大化铺满；
   - 托盘点击 + `tray-click.log`；
   - 退出/关开关/强杀后恢复；
   - 主题两态迁移；
   - 农历抽查 2026-10。
4. 若发现回归/未修复项：按 §5 P1 修复 → 重新构建 → 再验证。
5. 全绿后：更新报告 → 提交（可选）→ 打 tag（可选）。

# 顶栏任务托盘修复计划（v0.2.2 → v0.3）

- 制定日期：2026-10-27
- 输入：`docs/reviews/2026-10-27-topbar-tray-review.md`（评分 60/100，P0×4 / P1×6 / P2×5）
- 目标：把“顶栏托盘镜像”从 **best-effort 演示功能** 提升为 **可日常使用的系统托盘替代入口**；同时让文档与实现一致、桌面状态可自愈。

> **2026-10-27 架构补充（重要）**：`docs/research/2026-10-27-tray-architecture-alternatives.md` 调研了同类工具的实现。结论是 v0.3 应优先采用 **YASB 式托盘代理窗口（伪 `Shell_TrayWnd`）+ `SendNotifyMessage` 直接点击**，而不是继续修补“显示任务栏 + 移动光标点击”。
> 采用后，本计划的 **P0-1 锚定、P0-2 60s 显形、P0-3 坐标漂移会整体消失**；**M0 的文档/残局自愈/失败提示仍需先做**，作为过渡版本的止血；C 阶段（锚定决策）改为“代理 spike + hook 兜底”。

> **2026-10-27 实施状态（v0.3 已落地）**：`src-tauri/src/tray_proxy.rs` 已实现代理窗口、`WM_COPYDATA`/`SHELLTRAYDATA` 解析、`ChangeWindowMessageFilterEx`、`TaskbarCreated` 广播、HICON→PNG、真实托盘转发和 `SendNotifyMessage` 点击；`lib.rs` 启动代理并在失败时回退旧 UIA，退出先销毁代理并广播；`commands.rs` 的刷新/点击/排序已按代理分支路由；`ui/js/tray.js` 改为事件驱动 + 30s 兜底。真机验证：9 个图标捕获、右键光标锚定顶栏、左键 easy-dash 打开概览、右键期间任务栏 `alpha=0`。**P0-1/P0-2/P0-3 不再适用于主路径**；本计划其余 M0/M2/M3 项（残局自愈、失败提示、可见性文案、测试）仍可继续。

---

## 0. 修复目标与验收总口径

### 0.1 产品目标

1. **点击有一次明确结果**：左键/右键点击后 ≤1.5s 内，要么出现目标窗口/菜单，要么出现可读的失败提示（不再“点了没反应”）。
2. **不惊吓用户**：任何点击都不应让 Windows 任务栏无故显形；即使为显示系统菜单而临时显形，也必须在弹窗关闭后 **≤1s** 恢复隐身；用户 5s 无操作则提前恢复，硬上限 15s。
3. **点得准**：托盘图标增减、溢出箭头/输入法指示器出现或消失后，点击仍命中用户看到的那个图标（目标命中率 20/20）。
4. **桌面永远可恢复**：正常退出、强杀、崩溃、Explorer 重启后，任务栏与工作区都能恢复（有 marker 自愈，无 marker 也能识别残局）。
5. **文档可信**：README / CHANGELOG / 管理面板文案与真实行为一致；做不到的能力不写“已实现”。

### 0.2 必须持续成立的不变量（Invariants）

| ID | 不变量 | 校验方式 |
| --- | --- | --- |
| INV-1 | 隐藏期间任务栏窗口：`visible=true, alpha=0, WS_EX_TRANSPARENT`（或降级态有明确标记） | 定时采样 + 日志 |
| INV-2 | 任何“显示任务栏”的 hold 必须有上限（≤15s；用户 5s 无输入则提前恢复），且退出时恢复 INV-1 | 单测 + 真机采样 |
| INV-3 | 工作区在隐藏期间为 `(0,topbar_h)-(sw,sh)`；退出后为系统原值 | 探针脚本 |
| INV-4 | marker 存在 ⇒ 下次启动必恢复；进程消失且无 marker ⇒ 启动时检测残局 | 强杀/崩溃测试 |
| INV-5 | 点击坐标必须来自 ≤1.5s 内的枚举，且按 `order_key` 解析目标 | 日志字段 + 单测 |
| INV-6 | 任何失败路径都要写日志并在 UI 上给一次提示 | 代码审查 + 真机 |

### 0.3 问题 → 任务映射（保证评审报告中的每条问题都有归口）

| 评审问题 | 对应任务 |
| --- | --- |
| P0-1 弹窗锚定未实现但文档宣称 | A1（文档止血）+ C1/C2/C3/C4（方案决策与实现） |
| P0-2 弹窗误判/失败时任务栏显形 60s | A2（上限与恢复）+ B2（命中校验）+ B4（状态机） |
| P0-3 点击使用 ≤10s 旧坐标 | B1（实时重枚举）+ B2（失败重试）+ A5（日志可定位） |
| P0-4 残留隐身任务栏、无 marker 自愈 | A3（残局自愈）+ B4（状态机）+ R2–R4 验收 |
| P1-1 点击失败提示不可见 | A4（独立 toast） |
| P1-2 UIA/IPC 性能 | E1（CacheRequest）+ E2（轮询策略）+ E3（载荷精简） |
| P1-3 顶栏焦点环失效 | D1 |
| P1-4 深色垫底缩水/对比度/圆角/active | D3 + D4 |
| P1-5 可访问性缺口 | D2 |
| P1-6 降级不可逆 | B5 |
| P2 死代码/日志/排序边界 | A5 + E4 + E5 |

---

## 1. 工作分解（WBS）

> 估算按 1 名熟悉该仓库的工程师计；P0 项约 4–5 人日，整体约 12–15 人日。

### 阶段 A：止血热修（v0.2.2，2–3 天）

| ID | 任务 | 优先级 | 主要文件 | 做法 | 验收 |
| --- | --- | --- | --- | --- | --- |
| A1 | **文档如实化** | P0 | `README.md`、`CHANGELOG.md`、`docs/releases/*` | 把“锚定在顶栏图标下方”改为“原生托盘位置显示（best-effort）；任务栏会临时显形以显示系统菜单”；删除/改写 v0.1.0 与 v0.2.1 中不实描述，增加“已知限制” | 文档里不再出现“锚定”承诺；与新行为逐条对照通过 |
| A2 | **hold 硬上限 + 恢复保证** | P0 | `src-tauri/src/commands.rs`、`src-tauri/src/winapi/taskbar.rs` | `TRAY_POPUP_MAX_WAIT` 60s → **15s 硬上限**；弹窗关闭立即恢复；若弹窗存活但 `GetLastInputInfo` 显示用户已 5s 无输入，也提前恢复；`TaskbarHold::drop` 在 panic/early-return 下同样恢复；hold 期间 watchdog 保留状态检查，只让路工作区写入 | 任意点击后 ≤15s 回到 `alpha=0`；用户停止操作 5s 内恢复；日志记录 `released_reason` |
| A3 | **残局自愈** | P0 | `src-tauri/src/winapi/taskbar.rs` | 启动时无条件探测 `Shell_TrayWnd`：若 `alpha==0 && WS_EX_TRANSPARENT` 且无 marker，则执行恢复（带一次确认，避免误伤系统正常状态）；`restore_all()` 末尾读回校验并写日志 | 强杀/无 marker 场景重启后任务栏恢复；连续 20 次启动无误伤 |
| A4 | **失败可见提示** | P0 | `ui/js/tray.js`、`ui/css/topbar.css`、`ui/js/panels.js` | 不再复用 32px 窗口内的 `#tooltip`；改为顶栏下方独立 toast（复用 panels 的定位逻辑）；`tray_click` 失败/无弹窗 800ms 后提示“该图标没有可用菜单，请稍后重试” | 断桥接/UIA 失败时用户能看到提示；提示 2.5s 自动消失 |
| A5 | **日志 v2（最小集）** | P1 | `commands.rs` | 每次点击记录：`key/name/index/mode/fresh_ms/at/popup/rect/held_ms/reason/degraded`；轮转（保留最近 512KB）而不是 64KB 直接删 | 用日志可回答“点的是哪个图标、为什么失败” |

### 阶段 B：点击可靠性（v0.2.3，5–6 天）

| ID | 任务 | 优先级 | 做法 | 验收 |
| --- | --- | --- | --- | --- |
| B1 | **点击前实时重枚举** | P0 | `tray_click` 线程内：若距上次枚举 >1s，先 `tray::enumerate()`（复用桥接）→ 按 `order_key`（回退 `name`、再回退 `index`）解析目标 → 用新坐标点击；解析失败返回结构化错误给前端 | 制造布局变化（插拔 U 盘/开关输入法/增减托盘图标）后连续点击 20 次，目标命中 20/20；日志 `fresh_ms` 可证 |
| B2 | **点击命中校验** | P0 | 点击后 600ms 检查“新窗口/弹窗/前台窗口变化”；若三者皆无则视为失败，自动重试 1 次（仅重试一次，避免重复开关菜单），仍失败则 toast + 日志 | 右键 10/10 出现菜单或明确提示；不会出现“连点两次导致菜单开了又关” |
| B3 | **UIA Invoke 兜底（实验）** | P1 | 指针点击失败后，对目标任务元素尝试 `InvokePattern.Invoke()`；仅用于左键；评估 shell flyout 是否可展开 | 形成结论：哪些应用可兜底；不可用的写进文档 |
| B4 | **状态机重构** | P1 | 把 `WANT_HIDDEN/HIDDEN/VISIBLE_MODE/PERSISTENT_ALPHA/BRIDGE_DEPTH` 收敛为显式状态机（`HIDDEN / BRIDGING / REVEALED / DEGRADED / RESTORING`），所有转移集中在一个模块；每次转移校验 INV-1 | 并发点击、强杀、Explorer 重启下状态一致；新增单测覆盖转移矩阵 |
| B5 | **降级可恢复** | P1 | 降级态不再永久禁用可见兜底：允许手动/定时重试桥接（如每 5 分钟一次）；管理面板显示“桥接降级，点击重试”按钮 | 人为制造 3 次失败后可恢复；文档写清 |

### 阶段 C：弹窗/菜单体验（v0.3 决策点，1 天 spike + 3–5 天实现）

> 这是本功能的产品分水岭。先做 spike，再决定投入。

| ID | 任务 | 说明 |
| --- | --- | --- |
| C1 | **Spike：锚定可行性再评估** | 用当前 Windows 11 26H2 实测三类弹窗：① Tauri/#32768 菜单 ② Electron/Qt 菜单 ③ Shell XAML flyout（音量/网络/时钟）。分别测试 `SetWindowPos` + `SWP_FRAMECHANGED` + `RedrawWindow/InvalidateRect` 是否能让内容跟随重绘；记录 go/no-go |
| C2 | **方案 A（保底，推荐先做）** | 不搬移：原生位置显示 + 临时显形任务栏（受 A2 上限约束）+ 顶栏 toast 说明“菜单已在任务栏托盘处打开”。优点：稳定、低风险；缺点：体验不统一 |
| C3 | **方案 B（增强，spike 通过才做）** | 仅对“可搬移”的窗口类做迁移（Electron/Qt/#32768），Shell XAML 保持原生位置并回退方案 A；日志记录 `moved=true/false` |
| C4 | **方案 C（研究项，v0.3+）** | “菜单代理”：UIA 读取弹窗菜单项 → 在顶栏下方自绘同构菜单 → 点击时 UIA 调用原生菜单项。Go 条件：三类菜单均能稳定读取+调用；否则只做 Shell flyout 子集 |

**建议**：v0.2.3 先落地 C2（1 天内），C1 spike 并行；C3/C4 依据 spike 结果排期，**不要为了“锚定”承诺阻塞可靠性修复**。

### 阶段 D：体验与可访问性（与 B/C 并行，3–4 天）

| ID | 任务 | 优先级 | 做法 | 验收 |
| --- | --- | --- | --- | --- |
| D1 | 焦点环修复 | P1 | `ui/css/topbar.css` 的 `button:focus{outline:none}` 改为 `button:focus:not(:focus-visible){outline:none}`；全顶栏 Tab 验证 | Tab 到活动/时钟/齿轮/托盘按钮均可见焦点环（WCAG 2.4.7） |
| D2 | ARIA/键盘 | P1 | `#tray` 加 `role="toolbar" aria-label="系统托盘"`；tooltip `role=tooltip` + `aria-describedby`；错误提示 `role=status aria-live=polite`；托盘项支持 `Enter/Space`；管理面板 ↑↓ 支持键盘 | axe/人工检查通过；键盘可完成排序 |
| D3 | 视觉一致性 | P1 | `.tray-dark` 改 18px 内容盒（`box-sizing:content-box` 或调盒尺寸），确保图标内容恒 16px；字母占位色改用 token 并 ≥4.5:1；统一圆角；» 改 Lucide chevron；补 `:active` | 亮/暗两主题截图对比：图标尺寸一致、无 10px 缩水、对比度达标 |
| D4 | 溢出按钮策略 | P2 | 仅在后端存在隐藏图标时显示 `»`；点击后显示“隐藏图标面板”或明确提示 | 无隐藏图标时不出现无效 »；有隐藏图标时可用 |
| D5 | 空态/降级态 | P2 | 托盘不可用/为空时在顶栏显示占位提示（点击进管理面板），而不是静默空白 | Win10/断桥接时可见说明 |

### 阶段 E：性能与工程化（2–3 天）

| ID | 任务 | 优先级 | 做法 | 验收 |
| --- | --- | --- | --- | --- |
| E1 | UIA CacheRequest | P2 | `FindAllBuildCache` 一次性缓存 `BoundingRectangle/Name/ClassName/AutomationId/IsOffscreen`；减少跨进程属性读取 | 单次枚举耗时下降 ≥40%（在日志中记录 `enum_ms`） |
| E2 | 轮询策略 | P2 | 隐藏/无托盘时降频（30s）；面板打开或最近点击后 2s 内加频（2s）；可选注册 UIA 结构变化事件做事件驱动（spike） | 空转 CPU 降低；托盘变化感知延迟 ≤2s |
| E3 | IPC 载荷 | P2 | `tray-changed` 不再每次携带全量 base64：改为 `{revision, items:[{key,name,kind,iconHash}]}`，图标走 `get_tray_icon(key)` 按需取；或仅在变更时携带 | 事件体积下降 ≥80%；拖拽/主题切换仍正确 |
| E4 | 清理死代码 | P2 | 删除 `click_taskbar_postmessage` 或真正启用；统一 `mode` 字段；修正过时注释；`trayOrder` 清洗（去空、去重、截断） | `cargo check` 0 warning；搜索不到误导注释 |
| E5 | 测试补齐 | P2 | 为 `tray_click` 解析/重试、`relocate_popup` 策略、hold 生命周期、并发串行化、状态机转移写单测；Win32 层抽 trait 以便 mock | 新增 ≥15 个单测；关键路径覆盖率 ≥70% |

---

## 2. 里程碑与排期

| 里程碑 | 范围 | 预计 | 出口条件 |
| --- | --- | --- | --- |
| **M0 v0.2.2 止血** | A1–A5 | 2–3 天 | 文档如实；任何点击 ≤15s 恢复隐身（空闲 5s 提前恢复）；无 marker 也能自愈；失败有 toast；日志可定位 |
| **M1 v0.2.3 可靠性** | B1–B5 + C2 | 5–6 天 | 点击命中 20/20；右键失败率 <10%；状态机单测通过；降级可恢复 |
| **M2 v0.3 体验** | C1/C3/C4（按 spike）+ D1–D5 | 5–7 天 | 键盘可达；视觉一致；锚定方案有明确结论并落地/如实记录 |
| **M3 工程化** | E1–E5 | 2–3 天 | 性能指标达标；测试补齐；0 warning |

> 建议顺序：**M0 → B1/B2 → C1 spike → C2 → D → E**。不要先做锚定再修可靠性。

---

## 3. 真机验收矩阵（每轮发版必跑）

### 3.1 环境矩阵

- Windows 11 24H2 / 26H2（至少各 1 台；当前机器 26H2）
- DPI：100% / 125%（重点看坐标缩放）
- 任务栏：隐藏模式 / 关闭“隐藏系统任务栏” / 系统自动隐藏任务栏
- 多显示器：至少验证主屏 + 副屏（已知限制：副屏条带保留）

### 3.2 托盘场景

| # | 场景 | 预期 |
| --- | --- | --- |
| T1 | 0 个托盘图标 | 顶栏不显示 »；无报错 |
| T2 | 1 / 3 / 8 个图标 | 顺序、图标、hover 正常；» 仅在需要时出现 |
| T3 | 插拔 U 盘 / 开关输入法 / 打开关闭微信 | 10s 内顶栏顺序更新；点击不点错 |
| T4 | 右键 easy-dash 自身图标 | 出现「打开活动概览 / 重载配置 / 退出 easy-dash」；菜单关闭后 ≤1s、最长 15s 任务栏恢复 |
| T5 | 右键 Electron/Qt 应用（QQ、微信、DeepSeek） | 菜单出现；位置符合当前策略；点选后行为正确 |
| T6 | 左键 easy-dash 图标 | 活动概览出现/关闭 |
| T7 | 左键应用图标（微信/QQ） | 打开/聚焦应用窗口 |
| T8 | 连点 5 次同一图标 | 不出现重复开关/菜单闪断；日志串行 |
| T9 | 点击时任务栏从隐藏切到可见（管理面板开关） | 不崩溃、不残留 |
| T10 | 拖拽排序 2 个图标 / 单个图标 / 跨 » 位置 | 顺序持久化；刷新 10s 后不回跳 |
| T11 | 主题亮/暗切换 | 图标可见性修正正确；无 10px 缩水 |
| T12 | 管理面板「重置托盘顺序」 | 顺序恢复原生；顶栏 3s 内同步 |

### 3.3 桌面恢复场景

| # | 操作 | 预期 |
| --- | --- | --- |
| R1 | 正常退出（托盘菜单/管理面板/控制中心） | 任务栏可见、exstyle 0x88、工作区原值、marker 删除 |
| R2 | 保留弹窗时强杀进程（Task Manager） | 下次启动自动恢复并重新隐身 |
| R3 | 在 HIDDEN/BRIDGING/REVEALED 三种状态分别强杀 | 下次启动均恢复 |
| R4 | 隐藏期间重启 explorer.exe | watchdog 重新接管；不残留半透明任务栏 |
| R5 | 关闭“隐藏系统任务栏”再打开 | 任务栏正常显隐；工作区正确 |
| R6 | 注销/重启系统 | 登录后进入正确状态；无残留 |

### 3.4 自动化/脚本

- 新增 `scripts/qa/tray-checklist.ps1`：采样式验证任务栏 `visible/alpha/exstyle`、工作区、marker、`tray-click.log` 关键字段；输出 PASS/FAIL 表。
- `cargo test`（`EASY_DASH_NO_ADMIN=1`、`TEMP=src-tauri/target/tmp`）+ `node --check` 必须全绿。
- 每次发版把验收结果写入 `docs/releases/vX.Y.Z-verification.md`（沿用现有格式）。

---

## 4. 风险与应对

| 风险 | 概率 | 影响 | 应对 |
| --- | --- | --- | --- |
| Windows 更新改变任务栏 UIA/窗口类，桥接失效 | 中 | 高 | 保留“降级 + 重试 + 文档说明”；不要依赖单一窗口类；关键路径加 feature flag |
| 管理员权限/UIPI 导致 UIA 或输入不可用 | 中 | 高 | 已有 requireAdministrator；新增“桥接降级”状态与手动重试；避免在低权限构建上承诺托盘可用 |
| 临时显形任务栏影响全屏应用/最大化窗口 | 中 | 中 | hold 上限 + 仅弹窗存活时显形 + watchdog 保留工作区校正；全屏前台时直接拒绝显形并 toast |
| 重枚举带来点击延迟（+50–300ms） | 高 | 低 | 仅当缓存 >1s 才重枚举；并行预热；日志记录 `enum_ms` |
| 菜单代理/迁移引发新的渲染问题 | 中 | 中 | spike 先行；按窗口类灰度；保留方案 A 开关 |
| 回归影响“隐藏任务栏 / 最大化铺满” | 中 | 高 | 状态机 + INV 校验 + R1–R6 每轮必跑；先加测试再重构 |

---

## 5. 交付物

1. 代码：A–E 各任务对应 commit（建议按 ID 分 commit，便于 review/回滚）。
2. 文档：
   - `README.md` / `CHANGELOG.md` / `docs/releases/v0.2.2.md`（如实描述新行为与限制）
   - `docs/design-system.md` 补充托盘组件规范（尺寸/圆角/状态/对比度）
   - `docs/releases/v0.2.2-verification.md`（按 §3 矩阵填写结果）
3. 测试：
   - 新增单测（E5）
   - `scripts/qa/tray-checklist.ps1`
4. 发布：`easy-dash_0.2.2_x64-setup.exe` + 便携 exe；建议保留 `EASYDASH_TRAY_LEGACY=1` 回滚开关一个版本周期。

---

## 6. 明确不做（本期）

- 多显示器工作区完整接管（仅主屏，维持现状并写明）
- Windows 10 托盘镜像（继续降级隐藏）
- 托盘图标动态角标/红点实时同步（继续用静态快照/exe 图标策略）
- 完整“菜单代理”若 spike 不通过则不承诺（降级为方案 A）

---

## 7. 建议的第一个迭代（可以直接开工的 5 个 commit）

1. `docs: 修正托盘锚定/超时/限制描述（A1）`
2. `fix(taskbar): hold 上限 15s + 空闲提前恢复 + 恢复校验（A2）`
3. `fix(taskbar): 无 marker 残局自愈（A3）`
4. `fix(tray): 点击前按 1s TTL 重枚举并按 order_key 解析（B1）`
5. `feat(tray): 点击结果 toast + 日志字段 v2（A4/A5）`

> 每个 commit 都要附：`cargo test --lib`（相关模块）+ 真机 3 个用例（T4/T6/R1）。完成 5 个 commit 后再评估是否需要进入 B4 状态机重构。

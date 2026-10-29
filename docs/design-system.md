# easy-dash 设计规范（Aurora Glass）

> 适用于 v0.2 起的控制中心、通知中心、顶栏、Dock、应用中心与管理面板。
> 实现以 `ui/css/shared.css` 的 token 为唯一来源；组件样式只消费 token，不写死颜色。
> v0.4 起新增「界面材质」层（`ui/css/materials.css`）：标准 / 液态玻璃 / 柔光玻璃，见 §2.1。
> v0.5 把液态玻璃升级为「桌面取样 + 四层光学分层 + 自适应色阶」，见 §3.5 / §4.1。

## 1. 设计方向

- **Apple 控制中心 × Raycast**：
  - 来自 Apple：圆角分组卡片、磁贴式快捷开关、紧凑滑条组、强可读标签、克制的强调色。
  - 来自 Raycast：近黑/近白的中性表面阶梯、1px 发丝边、精确的字号与字重、轻量阴影。
- 参考文件（本机）：
  - `D:\codes\awesome-design-md\design-md\apple\DESIGN.md`
  - `D:\codes\awesome-design-md\design-md\raycast\DESIGN.md`
- 关键词：玻璃感分层、留白、发丝线、单一蓝色强调、克制动效。
- 限制：WebView2 透明窗口的 `backdrop-filter` **采不到窗口外的桌面**（桌面由 DWM 在 webview 之外合成），
  所以玻璃感不能只靠一条 `backdrop-filter`。
  - **v0.5 起**：控制中心 / 通知中心在面板展开前由 Rust 截取「面板将占据的屏幕矩形」（命令 `glass_backdrop`），
    前端把它当玻璃底图做 `blur(30px) saturate(150%)` + 轻微 `feDisplacementMap`，这是「液态玻璃」能真实透出桌面的唯一可靠路径（详见 §3.5、§4.1）。
  - 顶栏 / Dock / 管理面板仍用「半透明表面 + 顶部高光 + 柔和阴影」模拟（常驻表面，无法每帧截屏）。
  - 只有同窗口内确有下层页内内容的弹层（电源菜单 / 右键菜单 / tooltip）才用真实 `backdrop-filter`。
- 材质化（v0.4）：液态玻璃 / 柔光玻璃在「半透明表面（真实透出桌面）+ 层叠霞光 + 1px 镜面边 + 磨砂噪点 + 更大圆角」上做文章，
  不做原生 Acrylic/Mica——各 surface 是铺满屏幕宽的独立透明窗口，系统背景效果会把透明区域整片糊掉。
- 液态玻璃 2.0（v0.5）：真实桌面取样 + 四层光学分层（玻璃体 / 镜面层 / 边缘折射层 / 同色系投影）+ 自适应对比度色阶 + 蓝玻璃控件，
  只在 `liquid` 下生效；`solid` / `glow` 观感与实现不变（显式回归项）。

## 2. 主题机制

- `html[data-theme="light" | "dark"]` 控制 token；`color-scheme` 同步（原生滚动条/输入控件）。
- 主题模式（配置 `theme`）：只有 `light` / `dark` 两态，不再提供「跟随系统」（避免与 Windows 系统主题互相干扰）。
  旧配置里的 `system` 在加载时归一为 `dark`，前端 localStorage 旧值同样归一。
- `syncSystemTheme=true` 时，在控制中心切换浅/深会同时写入 Windows 主题（`AppsUseLightTheme` + `SystemUsesLightTheme` 并广播 `ImmersiveColorSet`）；关闭后 easy-dash 与系统主题各自独立。
- 首帧：`localStorage["easydash.theme"]` 缓存上次解析结果，HTML `<head>` 内联脚本在样式表之前写入 `data-theme`，避免闪烁。
- 未向 token 迁移完的旧样式通过别名兜底：`--panel-bg/--panel-border/--text/--text-dim/--accent/--danger/--ok` 映射到新 token。

### 2.1 界面材质（与明暗正交）

- 维度：`html[data-material="solid" | "liquid" | "glow"]`，与 `data-theme`（light / dark）**正交**，共 2×3 = 6 种组合。
  - `solid`（标准，默认）：不触发 `materials.css` 任何规则，观感与 v0.3 完全一致。
  - `liquid`（液态玻璃 2.0）：桌面取样 + 30px 模糊 / 150% 饱和 + 自适应对比度色阶 + 0.10 白偏光 +
    多重 inset 镜面边 + 同色系多层投影 + 蓝玻璃控件 + 轻微 `feDisplacementMap` 折射；圆角更大、打开时一次性扫光。
  - `glow`（柔光玻璃）：低对比、弥散外发光、暖调霞光、边缘几乎无硬线。
- 配置字段 `material`，白名单 `["solid","liquid","glow"]`，空串命令报错、非法值归一 `solid`（前端 localStorage 同规则）。
- 同步链：`set_material` → `config.json` → `theme-changed`（payload 携带 `material`）+ `config-changed` → `ui/js/theme.js`
  写 `data-material` 并通知订阅者；`get_theme_state` 兜底校准，「重载配置」同样生效。
- 首帧：`localStorage["easydash.material"]` 由 HTML 内联脚本在样式表之前落地，避免先闪一下 solid。
- 实现：材质只覆盖 `shared.css` 的 token + 5 个外壳（`.topbar` / `.dock` / `.panel` / `.mp-panel` / 全屏遮罩）的结构增强，
  组件样式不写材质分支；文字 token（`--text*` / 语义色 / 强调色）不覆盖，对比度只由表面 alpha 决定。
- 液态玻璃的例外：`.panel` 不再走 `--surface-1` 实底，而是由 `--lg-*` 四层结构与 `glass_backdrop` 取样结果接管（§3.5 / §4.1）；
  其他 liquid 外壳与 `solid` / `glow` 仍走 token。

## 3. 设计 Token

### 3.1 颜色

| Token | Dark | Light | 用途 |
| --- | --- | --- | --- |
| `--bg` | `#0b0c0f` | `#f2f3f6` | 应用画布 |
| `--surface-1` | `#17181c` | `#f7f7f9` | 面板外壳 |
| `--surface-2` | `#1e2025` | `#ffffff` | 卡片 / 磁贴 |
| `--surface-3` | `#26282e` | `#ffffff` | 悬浮层 / 弹出层 / 悬停抬升 |
| `--surface-inset` | `#0f1013` | `#ececf0` | 凹陷区域：输入框、滑轨底、缩略图底 |
| `--text` | `#f2f3f5` | `#1d1d1f` | 主文字 |
| `--text-dim` | `rgba(242,243,245,.72)` | `rgba(29,29,31,.66)` | 次要文字 |
| `--text-faint` | `rgba(242,243,245,.45)` | `rgba(29,29,31,.42)` | 分组标签/占位 |
| `--border` | `rgba(255,255,255,.08)` | `rgba(0,0,0,.10)` | 发丝边 |
| `--border-strong` | `rgba(255,255,255,.16)` | `rgba(0,0,0,.18)` | 输入框/强调边 |
| `--accent` | `#3584e4` | `#1c71d8` | 强调色（品牌蓝） |
| `--accent-hover` | `#4b93ef` | `#185fb8` | 强调 hover |
| `--accent-soft` | `rgba(53,132,228,.18)` | `rgba(28,113,216,.12)` | 选中底/焦点环 |
| `--on-accent` | `#ffffff` | `#ffffff` | 强调底上的文字 |
| `--danger` | `#f66152` | `#c01c28` | 错误/关机危险动作/休息日 |
| `--ok` | `#57e389` | `#1b8553` | 成功 |
| `--warn` | `#e5a50a` | `#9a6700` | 警告/调休「班」 |
| `--track` | `rgba(255,255,255,.14)` | `rgba(0,0,0,.12)` | 滑条/开关轨道 |
| `--scrim` | `rgba(9,10,13,.72)` | `rgba(238,239,243,.78)` | 全屏应用中心/概览遮罩 |
| `--hover` / `--hover-strong` | `rgba(255,255,255,.10)` / `.16` | `rgba(0,0,0,.055)` / `.09` | 交互状态叠加（深色用白、浅色用黑） |
| `--topbar-bg` | `rgba(24,25,29,.88)` | `rgba(248,248,250,.90)` | 顶栏/Dock 玻璃底 |
| `--topbar-border` | `rgba(255,255,255,.09)` | `rgba(0,0,0,.08)` | 顶栏分隔线 |
| `--glass-bg` / `--glass-border` | 同 `--topbar-bg` / `--topbar-border` 的别名 | 同左 | 玻璃层 |
| `--shadow-icon` | `rgba(0,0,0,.5)` | `rgba(20,22,28,.18)` | 仅图标 `drop-shadow` 颜色 |

### 3.2 阴影 / 圆角 / 间距 / 动效

| 类别 | 值 |
| --- | --- |
| `--shadow-panel` | Dark `0 10px 26px rgba(0,0,0,.55), 0 2px 6px rgba(0,0,0,.35)`；Light `0 12px 32px rgba(20,22,28,.16), 0 2px 6px rgba(20,22,28,.08)` |
| `--shadow-card` | Dark `0 6px 18px rgba(0,0,0,.38), 0 1px 3px rgba(0,0,0,.28)`；Light `0 4px 14px rgba(20,22,28,.10), 0 1px 3px rgba(20,22,28,.06)` |
| `--shadow-pop` | Dark `0 14px 38px rgba(0,0,0,.55), 0 2px 8px rgba(0,0,0,.30)`；Light `0 14px 38px rgba(20,22,28,.18), 0 2px 8px rgba(20,22,28,.08)` |
| `--radius-xs/sm/md/lg/xl/pill` | `4 / 6 / 10 / 14 / 18 / 999 px` |
| `--space-1..6` | `4 / 6 / 8 / 12 / 16 / 24 px` |
| `--dur-fast/base/slow` | `120 / 200 / 320 ms` |
| `--ease` | `cubic-bezier(.2,.8,.2,1)` |

### 3.3 排版

- 字体：`"Segoe UI Variable Text", "Segoe UI", "Microsoft YaHei UI", "PingFang SC", sans-serif`。
- 时钟：40px / 300 / `tabular-nums`；面板分组标签 11px / 600 / `--text-faint`；
  标题 13px / 600；正文 12.5px / 400；次要 11.5px；按钮 12.5px / 500。
- 数字（时间、百分比、电量、进度）一律 `font-variant-numeric: tabular-nums`。
- 单卡片不超过 3 种字号。

### 3.4 材质 token（`materials.css`，仅在 `[data-material]` 下覆盖）

| Token | solid（默认） | liquid（暗 / 亮） | glow（暗 / 亮） |
| --- | --- | --- | --- |
| `--surface-1`（外壳底） | 实色 `#17181c` / `#f7f7f9` | `rgba(12,13,18,.76)` / `rgba(252,253,255,.60)` | `rgba(38,40,52,.84)` / `rgba(255,253,250,.78)` |
| `--surface-2`（卡片） | 实色 | `rgba(255,255,255,.05)` / `.60` | `rgba(255,255,255,.055)` / `.66` |
| `--surface-3`（浮层） | 实色 | `rgba(255,255,255,.12)` / `.80` | `rgba(255,255,255,.08)` / `.88` |
| `--surface-inset` | 实色 | `rgba(6,7,11,.34)` / `rgba(232,236,246,.55)` | `rgba(20,21,28,.34)` / `rgba(246,244,240,.62)` |
| `--border` / `--border-strong` | `.08` / `.16` | `.16` / `.32`（亮 `.12` / `.22`） | `.09` / `.20`（亮 `.07` / `.14`） |
| `--topbar-bg` | `rgba(24,25,29,.88)` | `rgba(18,20,27,.68)` / `rgba(252,253,255,.55)` | `rgba(38,40,52,.72)` / `rgba(255,253,250,.72)` |
| `--radius-xl / lg / md` | 18 / 14 / 10 | 24 / 17 / 12 | 18 / 14 / 10 |
| `--glass-rim` | 无 | 顶白 `.55` + 左右冷暖彩色折射边 + 底黑 `.40` | 顶白 `.16` + 底黑 `.20`（几乎无硬线） |
| `--glass-glow` | 无 | 无（锐利镜面） | `0 0 72px rgba(130,160,255,.34)`（亮 `0 0 70px rgba(255,190,150,.45)`） |
| `--glass-sheen` | 无 | 左上白霞光 `.12`（亮 `.62`） | 顶部暖光 `.18` + 底部冷光，双层 |
| `--glass-topline` | 无（沿用原 1px 发丝高光） | 白 + 蓝强调色镜面线 | 白 + 暖色柔光线（低对比） |
| `--glass-panel-tint` | = `--surface-1` | `rgba(12,13,18,.80)` / `rgba(252,253,255,.80)` | `rgba(38,40,52,.88)` / `rgba(255,253,250,.90)` |
| `--glass-pop-tint` | = `--surface-3` | `rgba(34,37,48,.72)` / `rgba(255,255,255,.74)` | `rgba(46,48,62,.80)` / `rgba(255,253,250,.88)` |
| `--glass-noise-opacity` | 0 | .045 / .03 | .014 / .014 |
| `--glass-blur`（仅弹层） | 0 | 24px | 28px |

- 结构 token（`--space-*` / `--dur-*` / `--ease`）三材质一致；只有 `--radius-*` 在材质层被覆盖。
- 上表是 v0.4 的外壳 token 兜底；**v0.5 起 liquid 的 `.panel` 改由 §3.5 的 `--lg-*` 接管**，表中 liquid 行仍用于顶栏 / Dock / 管理面板 / 弹层等表面。
- **可读性下限（硬约束）**：非面板外壳沿用 v0.4 的 alpha 下限（暗色 liquid ≥ .76、glow ≥ .84；亮色 ≥ .55）；
  液态玻璃面板不再固定 alpha，而是按桌面取样亮度解出「正文 ≥ 4.7:1」所需的最小色阶（§3.5），文字 token 仍不覆盖。
- 磨砂噪点：`feTurbulence` + `feColorMatrix(saturate 0)` 的 SVG data-URI；外壳 `::after` 用 `mix-blend-mode: overlay`，
  液态玻璃面板改用 `--lg-noise`（rect 自带 `opacity=".035"`，normal 混合，见 §3.5）。

### 3.5 液态玻璃 2.0 光学参数（v0.5，仅 `liquid` 面板）

> 实现：`ui/css/materials.css` §7（四层结构）+ `ui/js/glass-math.js`（光学公式）+ `ui/js/glass.js`（取样与变量落地）
> + `src-tauri/src/winapi/glass.rs`（桌面取样）。`solid` / `glow` 不参与本节任何规则。

**四层结构（内容与文字永远不进滤镜）**

| 层 | 元素 / z-index | 内容 |
| --- | --- | --- |
| 玻璃体 | `.panel::before` / `-3` | 桌面取样图 + 对比度色阶 + 0.10 白偏光；`inset:-40px`、`filter: blur(30px) saturate(150%)`、`.lg-refract` 时追加 `url(#lg-refract)` |
| 镜面层 | `.panel::after` / `-2` | `--lg-noise` 噪点 + 顶部径向白光 + 1px 镜面线 + 多重 inset `--lg-rim` |
| 边缘折射层 | `.lg-edge`（JS 注入 span）/ `-1` | 内缘白光 + 冷调内阴影；`.lg-refract` 时接 `url(#lg-refract-edge)`，让边缘产生轻微液态弯曲 |
| 投影 | `.panel` 本体 | `--lg-shadow` 三段同色系柔和阴影；不画 inset（镜面边在 ::after 上） |

**token**

| Token | 暗色 | 亮色 | 说明 |
| --- | --- | --- | --- |
| `--lg-tint` | `rgba(255,255,255,.10)` | `rgba(255,255,255,.04)` | 固定白偏光上限；亮色只留一点玻璃抛光，不再做白底 |
| `--lg-grade` | `rgba(9,11,17,.72)` 兜底 | `rgba(250,251,255,.24)` 兜底 | 对比度色阶；有取样时由 JS 内联覆盖。亮色最多 14% 白，常规壁纸 ~10% |
| `--lg-backdrop` | `none` / `url(data:image/jpeg;base64,...)` | 同左 | 桌面取样底图 |
| `--lg-blur` / `--lg-sat` | `30px` / `1.5` | 同左 | 需求指定的模糊 / 饱和 |
| `--lg-accent-rgb` | `0 122 255` | 同左 | macOS 系统蓝 |
| `--lg-accent-alpha` | `.6` | `.6` | JS 按背景亮度在 `[.6,.9]` 内上调 |
| `--lg-rim` | 顶白 `.8`、底白 `.2`、左右白 `.2–.28`、底暗 `.35` 深度 | 顶白 `.9`、底白 `.18` + 暗 `.10`、左右白 `.7`、1px 暗环 `.06` | 多重 inset，无普通 border |
| `--lg-shadow` | 由背景平均色推出的 `hsl(h s% 11%)` 三段 | `hsl(h s% 24%)`；深色墨表面自动用 11% | 不纯黑 |
| `--lg-noise` | `feTurbulence`，rect `opacity=.035` | 同左 | 自带 alpha，normal 混合 |
| `--lg-specular` / `--lg-topline` | 顶部径向白光 / 1px 镜面线 | 同左（更强） | 玻璃受光 |
| `--lg-edge-wash` / `--lg-edge-rim` | 内缘白光 + 冷调阴影 | 同左 | `.lg-edge` |
| `--lg-hairline*` | 1px 白 `.12` + 暗 `.12` | 1px 暗 `.10` + 白 `.55` | 卡片 / 磁贴 / 分段的内侧发丝边 |
| `--lg-fallback-glow` | 主题色霞光 | 同左 | 没有取样时的兜底 |
| `--lg-shadow-pop` | 同色系三段（更小） | 同左 | 弹层 |
| `[data-ink="light"]` | — | 深色 token + 浅色字 | 亮色玻璃合成偏暗时，整块表面切到暗色 token（见下） |

**自适应色阶（`ui/js/glass-math.js`，纯函数 + `npm run check:glass` 回归）**

- 取样返回 `p05 / p95 / mean`：线性相对亮度分位与平均色（`src-tauri/src/winapi/glass.rs`）。
- 合成按 CSS 的 sRGB 逐通道 alpha 进行：`C1 = a·G + (1-a)·B`，`C = t·1 + (1-t)·C1`（`t` 为白偏光，`G` 为色阶色）。
- 暗色取 `B=p95`、`G≈rgb(9,11,17)`、`t=.10`；亮色取 `B=p05`、`G≈rgb(250,251,255)`、`t=.04`。
- **暗色**：求满足「正文 ≥ 4.7:1」的最小 `a ∈ [.10,.95]`（验收线 4.5，余量 0.2 给卡片内高光 / specular），
  再叠**材质密度**：`a_dark = max(对比度解, 0.10 + 0.78·p95^0.9)`，夹在 `[.10,.86]`。
  这是设计密度（背景越亮、深色玻璃越实，接近 macOS 暗色材质），对比度仍由前半项兜底。
- **亮色（v0.5.1 去白底）**：不再按 4.7 加白，`a_light = min(对比度解, MAX_LIGHT_GRADE=.14)`，常规壁纸只有 ~.10；
  不再用白底硬保深色字，而是**按最终合成亮度选墨色**：偏亮用深色字（浅色 token），偏暗用浅色字
  （`[data-ink="light"]` 把整块表面切到暗色 token：`--text`/`--surface-*`/`--lg-rim` 等全部走暗色分支）。
  中间亮度壁纸上黑/白墨色的最大对比度约 **4.0:1**（验收线 3.9），这是「非常透明」的物理折中。
- 物理结果：暗色桌面 → a≈.10；暗色 + 彩色壁纸 → a≈.54；暗色 + 纯白壁纸 → a≈.86；
  亮色白/浅壁纸 → a≈.10 + 深色字；亮色彩色/深壁纸 → a≤.14 + 浅色字（深色玻璃）；
  **亮色不再出现 .5–.86 的白色色阶**。
- 蓝玻璃 `--lg-accent-alpha ∈ [.60,.90]`，目标白字 ≥ 3.5:1（#007aff 对纯白上限 4.04；亮色纯白壁纸取 3.5 折中）。
- 投影色：`hsl(mean.h, clamp(mean.s×0.7, 18%, 40%), l)`；深色墨表面 `l=11%`、浅色墨表面 `l=24%`，灰阶背景回退色相 220°。

**桌面取样（`glass_backdrop`）**

- 前端在 `.panel` 完成布局、尚未拉高窗口时调用；参数是**相对顶栏客户区的 CSS 像素**矩形。
  从关闭态打开时窗口仍是 32px（面板在视口外，截到的是干净桌面）；面板互切时窗口已展开但旧面板已 `.hidden`，
  透明窗口不遮挡桌面，同样可截。
- Rust 用 `ClientToScreen` + `GetDpiForWindow` 换算物理矩形并夹到虚拟屏；`BitBlt` 截图，缩到宽 ≤480，JPEG q72 →
  `{ dataUrl, p05, p95, mean, width, height }`；近黑像素 >95% / 越界 / 失败返回 `null`。
- 只在面板打开时取一次；主题切换只重算色阶不重截图；材质切回 liquid 时复用本次打开缓存；面板开着时桌面变化不会实时跟随（见 §6.1）。
- 隐私：截图只在内存、不落盘、不打日志，只截面板区域；切到 solid / glow 即不再取样。

### 3.6 液态玻璃扩展到全部表面（顶栏 / Dock / 管理面板 / 概览 / 应用中心）

> 同一套光学语言与同一组 `--lg-*` token，按表面形态取用：条状/面板状表面是完整四层；
> 顶栏只有 32px，不注入边缘折射层；全屏概览/应用中心把玻璃体铺满，镜面层让位给 scrim，
> 卡片/搜索条承担低 alpha 玻璃 + 发丝边 + 同色系投影。
> 不再只服务控制中心/通知中心。实现：`ui/css/materials.css` §8 + `ui/js/glass.js` 的 surface 分支
> + `src-tauri/src/winapi/glass.rs` 的 `window_backdrop` / `surfaces.rs` 的显示前取样缓存。

**表面矩阵**

| 表面 | 玻璃元素 | 模糊 | 取样时机 | 裁切方式 |
| --- | --- | --- | --- | --- |
| 顶栏 | `.topbar` | **18px**（32px 条不能和 604px 面板共用 30px） | 窗口 `show` 前截整条客户区（启动 / 全屏后重显 / 配置重显）+ 页面就绪后补截 | `clip-path: inset(40px)`，不用 `overflow:hidden`（保留按钮焦点环） |
| Dock | `.dock` | **24px** | 首次 `show_dock` 前截窗口客户区；`show_dock` 返回后事件触发，隐藏条一拍用 `glass_backdrop` 按最终尺寸补截 | `clip-path: inset(40px round var(--radius-xl))`，图标放大不受影响 |
| 管理面板 | `.mp-panel` | **30px** | 每次 `show_at` 前截窗口客户区；`manager-open` 后隐藏面板一拍补截 | 面板自身 `overflow:hidden` + 圆角 |
| 活动概览 | `#backdrop` | **30px** | 每次打开前截全屏；`overview-data` 后隐藏玻璃体一拍补截 | 全屏 fixed 玻璃体，铺在 scrim 之下；卡片是低 alpha 玻璃 |
| 应用中心 | `#backdrop` | **30px** | 每次打开前截全屏；`app-center-open` 后隐藏玻璃体一拍补截 | 同上；搜索条是玻璃条，应用图标直接浮在 scrim 上 |

**取样管线**

- `src-tauri/src/winapi/glass.rs::window_backdrop(window)`：`GetClientRect` 取客户区物理尺寸 + `ClientToScreen((0,0))` 取原点，
  夹到虚拟屏后复用面板的 `capture_rect`（480 宽 JPEG q72、近黑 >95% 返回 null、统计 `p05/p95/mean`）。
- `surfaces.rs::capture_surface_backdrop` 在 `win.show()` **之前**截屏并按窗口 label 写入
  `AppState.surface_backdrops`；窗口已可见时跳过（避免截到自身形成「玻璃套玻璃」）。
- `surfaces.rs::publish_surface_backdrop` 在 `show()` 之后 `emit_to(label, "surface-backdrop", backdrop)`；
  顶栏启动时页面还没加载、事件会丢，前端用命令 `surface_backdrop(window)` 回读缓存兜底。
- **窗口级取样是快速路径，不作为唯一来源**：`window_backdrop` 在首帧/窗口重排时可能拿到旧位置或
  直接拿不到（真实桌面验证中 Dock 就出现过这种情况，退回 `.24` 兜底白玻璃）。因此：
  - 顶栏在 `refreshSurface` 完成后隐藏条 ~30ms，用面板已验证可用的 `glass_backdrop` 按条矩形补截；
  - Dock 在 `dock.js` 的 `show_dock` 返回后派发 `easydash:dock-shown`，`glass.js` 隐藏 Dock 一拍补截；
    事件错过时 900ms 兜底再补一次；
  - 管理面板 / 概览 / 应用中心在 `manager-open` / `overview-data` / `app-center-open` 后 30ms
    隐藏自身玻璃元素，用 `glass_backdrop` 补截，再显形。
  这样 `window_backdrop` 即使失败也不会长期停在兜底白玻璃上；补截只在 liquid 下发生。
- **WebView2 资源缓存**：Tauri 发布版会把 `ui/css/js` 缓存到 `EBWebView`；升级后即使 exe 是新的，
  Dock/顶栏可能仍加载旧 CSS（实测浅色模式停在 v0.4 的 `.55` 白玻璃）。`surfaces.rs` 给所有
  WebView2 窗口加 `additional_browser_args("--disable-http-cache")`；升级后若仍见旧界面，关闭应用并
  重命名/删除 `%LOCALAPPDATA%\\com.easydash.app\\EBWebView` 即可。
- 前端 `glass.js` 每个窗口只命中一个 surface（`.topbar` / `.dock` / `.mp-panel` / `#backdrop`），
  把 `dataUrl` 写成 `--lg-backdrop`，由 `glass-math.js` 按该表面自己的 `p05/p95/mean` 算色阶、蓝 alpha、投影。
  主题切换只重算；管理面板 / 概览 / 应用中心每次打开都会重新取样；Dock / 顶栏的下一次 show 会刷新。
- **全屏概览 / 应用中心**：取样图在 `#backdrop.lg-overlay::before`，scrim 在 `::after`（`--lg-overlay-scrim`，比 v0.4 略透），
  内容在其上；卡片 / 搜索条用 `--surface-2` 低 alpha + `--lg-hairline` + `--lg-shadow`，不是实心面板。
- **只在 liquid 取样**：显示前的自动取样检查 `config.material`，solid / glow 不后台截屏（隐私 / 性能）。
  启动时是 solid/glow、用户切到 liquid 时，顶栏 / Dock / 管理面板先 `visibility:hidden` 一拍，
  用 `glass_backdrop` 按自身矩形补一张（~30ms 空白，只发生在主动切材质时）；概览 / 应用中心下次打开时补。
- **顶栏/Dock 底片是常驻的**：`show_if_hidden` / `show_dock_window` 只在 `surface_backdrops`
  没有该 label 时截一次（启动 / 新显示器），之后从全屏、活动概览、Dock autohide 重显都**复用**，
  不再重截。否则重显瞬间屏幕上已经是概览/窗口缩略图，会把它们的颜色烘进常驻 chrome，
  表现为「打开活动后顶栏/Dock 颜色变了、还会跟着打开窗口的颜色变」。
  管理面板/概览/应用中心是临时浮层，仍用 `force=true` 每次打开取当前桌面。
- **顶栏启动补截**：Rust 启动取样发生在 AppBar 预留 / 最大化窗口重排之前，可能截到顶部窗口的一条；
  页面就绪后 `glass.js` 再隐藏顶栏条 ~30ms 用 `glass_backdrop` 按条矩形补一张，确保底片是预留后的桌面。
- **Dock / 管理面板 / 概览 / 应用中心补截**：`window_backdrop` 在 Dock 首帧、管理面板每次 `show_at`、
  概览/应用中心每次打开时同样可能拿到旧位置或失败；对应事件后隐藏玻璃元素 ~30ms 用 `glass_backdrop`
  按最终矩形补一张。这样真实桌面里 Dock 压在绿草地上时显示的是绿色透明玻璃，而不是 `.24` 兜底白。
- **浅色去白底（v0.5.1）**：所有 surface/面板的亮色白色阶上限 `.14`（常规 `.10`），不再用大面积白底；
  合成亮度偏暗时给元素加 `data-ink="light"`，`shared.css` 的暗色 token 块与 `materials.css` 的液态暗色 token 块
  直接命中该子树（`--text`/`--surface-*`/`--lg-rim`/`--lg-shadow` 等一起切），文字自动变浅色。
  亮色 + 暗壁纸 = 深色透明玻璃 + 浅色字；亮色 + 亮壁纸 = 浅色透明玻璃 + 深色字。中间亮度最差 ~4.0:1（验收 3.9）。
- **管理面板**：`.mp-panel` 去 1px border（`border:0`），玻璃体铺满窗口客户区；标题栏/侧栏保持半透明；
  `.mp-card` 去 border、改发丝边；选中开关使用 §4.2 同款蓝玻璃（`--lg-accent-alpha`，白色顶部内高光），不是纯蓝色块。
- **顶栏/Dock 的边界**：顶栏不加外投影（贴屏幕顶边），Dock/管理面板用 `--lg-shadow`；顶栏太薄不注入 `.lg-edge`，
  Dock/管理面板注入 `.lg-edge` 并在入场 320ms 后启用 `feDisplacementMap`。
- **静态取样**：顶栏/Dock 只在自己的 `show` 前截一张。桌面在它们可见期间变化（换壁纸、窗口移动到条后面）不会实时更新；
  下次 show / 重启后刷新。这是 WebView2 无法对窗口外做实时 `backdrop-filter` 的折中（§6.1）。


## 4. 组件规格

### 4.1 面板（控制中心 / 通知中心）

- 尺寸：控制中心 420×604（窗口高 688）；通知中心 880×536（窗口高 620）；`top: 40px`，窗口底部留 44px 阴影空间。
- **液态玻璃 2.0 外壳**（`liquid`）：
  - 去掉 1px 实心 border（`border:0`），`padding` 各 +1px 抵消边框，内容盒与 solid / glow 完全一致（不出现滚动条）。
  - 玻璃体 `.panel::before`：`inset:-40px`、`z-index:-3`，三层（0.10 白偏光 → 自适应色阶 → 桌面取样图），
    `filter: blur(30px) saturate(150%)`；没有取样时退回色阶 + 主题霞光（`[data-glass="fallback"]`）。
  - 镜面层 `.panel::after`：`z-index:-2`，噪点 + 顶部 1px 镜面线 + 多重 inset `--lg-rim`（顶白 `.8`、底白 `.2`、左右白 + 底暗深度），
    **没有任何普通 border**。
  - 边缘折射层 `.lg-edge`：`z-index:-1`，`feDisplacementMap` 只作用于它和玻璃体，文字清晰不扭曲。
  - 投影 `--lg-shadow`：三段，颜色由桌面平均色推出的同色系深色，不是纯黑；收敛在右侧 8px / 底部 44px 内。
  - **浅色去白底（v0.5.1）**：白色阶最多 `.14`、常规 `.10`；合成偏暗时整块面板加 `data-ink="light"`，
    走暗色 token + 浅色字，不再铺一层 `.5+` 的白底。浅色 + 暗壁纸看起来是「深色透明玻璃」，浅色 + 亮壁纸
    仍是「浅色透明玻璃」；面板、卡片、磁贴、通知卡、日历、媒体卡一起切换。
- **standard / glow**：保持 v0.4 规格（`--surface-1`、1px `--border`、`--radius-xl`、`--shadow-panel`、16px 内边距、顶部 1px 高光）。
- **全部 surface 同步**：顶栏 / Dock / 管理面板 / 活动概览 / 应用中心使用同一套四层与 `--lg-*` token，
  按厚度改模糊（18 / 24 / 30px）和裁切方式；详见 §3.6。
- 入场：液态玻璃先隐藏截屏（`.lg-wait`，`animation:none`），拿到取样后拉高窗口、移除 `.lg-wait`，
  `sheet-in` + `lg-sheen-sweep`（820ms）重新播放；`prefers-reduced-motion` 关闭扫光与位移。
- 内容超高时内部滚动，不改变窗口高度。

### 4.2 快捷开关磁贴（控制中心）

- 3 等宽，高 84px（双区交互需要两行热区），`--radius-lg`（14px），`--surface-inset` 底 + `--border`。
- 图标泡 28px（`--accent-soft` 底 + `--accent` 图标，`overflow:hidden` 且 svg 18px、max 100%，任何状态不越界）；名称 12.5px/600；状态 11px `--text-dim`。
- **液态玻璃开启态（v0.5）**：`rgb(0 122 255 / .6)` 半透明蓝（`--lg-accent-alpha` 会在亮色/亮底图上自动升到 ≤.9），
  叠 0→46% 白色竖向渐变 + `inset 0 1px 1px rgba(255,255,255,.55)` / 底暗 `.18` + 蓝调外阴影；
  图标泡 `rgba(255,255,255,.20)` + 顶部高光。**不是纯蓝色块**。关闭态是低 alpha 表面 + 发丝内侧高光。
- **双区交互**：Wi-Fi / 蓝牙磁贴 = 图标泡按钮（切换开关，`aria-pressed` + 「开启/关闭 X」`aria-label`）+ 文字区按钮（进入连接子视图，右侧 chevron 14px 暗示）；两按钮各自 `:focus-visible` 焦点环。飞行模式为整块单按钮切换。
- 禁用态 opacity .45。
- hover：边缘发丝线提亮（`--lg-hairline-hover`）/ 蓝玻璃顶部高光加强；不缩放、不上浮磁贴，避免子元素溢出/布局跳动。

### 4.3 分段控件（外观 / 电源模式）

- 高 34–36px，`--surface-inset` 底，`--radius-md`，内部等分按钮（明暗 2 段：浅色 / 深色；界面材质 3 段：标准 / 液态玻璃 / 柔光玻璃；电源 3 段）。
- 未选：透明底 + `--text-dim`；hover：`--surface-2`；选中：`--accent-soft` 底 + `--accent` 文字/图标 + 1px accent 边。
- `aria-pressed` / `role="group"` + `aria-label`。

### 4.4 滑条（音量 / 亮度）

- 行高 32px（受控制中心 476px 面板高度约束，4px 网格）；轨道 6px、`--track`、`--radius-pill`；填充 `--accent`；滑块 14px 圆形 + `--shadow-card`。
- 右侧百分比 11px tabular + `--text-dim`；静音按钮 28px，开启态 `--accent-soft`。
- **滚轮**：悬停滑条整行（含图标 / 百分比）即可调节，每档 5%；约 100px 滚动计一档，触控板连续滚动累计，`ctrlKey` 不参与；调节时 `preventDefault` 阻止面板内滚动，走相对 IPC（80ms 合批），不覆盖拖拽/轮询状态。
- 禁用/不可用（无 DDC/CI 亮度）：整行隐藏并保留 12px 间距。

### 4.5 列表行（连接 / 通知 / 管理面板）

- 控制中心主视图已移除「连接」卡片（Wi-Fi/蓝牙入口改由磁贴文字区进入），子视图行高 44px；管理面板行 padding 12px。左侧 18px 图标 + 主标题 12.5px + 副标题 11px `--text-dim`；右侧 chevron 14px `--text-faint`。
- hover：`--surface-inset`；分隔线 `--border` 的 50% 透明度；首末行不外扩圆角。

### 4.6 通知卡（通知中心）

- `--surface-inset` 底、`--radius-lg`、1px `--border`；应用图标 34px（真实图标，失败用字母色块）。
- 元信息行：app 名 11.5px/600 `--text-dim` + 条数徽标 + 相对时间 11px `--text-faint`。
- 标题 12.5px 单行截断；正文 11.5px `--text-dim` 最多 2 行。
- 多条通知堆叠：下方 2 层卡片左右内缩 8/16px、底部露出 4/9px（含 1px 层间隙）、`--surface-2`/`--surface-3` + 1px `--border`，右上角「N 条」徽标；展开后隐藏层。
- 展开列表淡入 160ms；未读点 6px `--accent`。
- **进入应用**：卡片右上角 22px 图标按钮（external-link 12px，默认透明度 .55，hover/focus 为 1；`--surface-2` 底 + `--border` + `--radius-sm`）；可映射应用的分组才显示，`.ntf-meta` 右侧预留 24px 避免压住时间。应用图标与展开后的单条通知也可点击进入（hover 抬升到 `--surface-3` / 左侧强调边框），条目必须具备 `role="button"` + `tabindex="0"` 与 Enter/Space 支持。

### 4.7 日历卡

- 头部：月份 12.5px/600 + 左右 26px 圆形导航按钮；星期行 10.5px `--text-faint`。
- 日期格 34px 高（数字 12.5px + 第二行农历/节日 9px）；今日：`--accent` 胶囊 + `--on-accent`；休息日：`--danger` 数字 + 节日短名；调休：`--warn`「班」角标；非本月 opacity .38。
- **农历**：无节日时第二行显示农历日名（初一显示月名，闰月带「闰」），颜色 `--text-dim`；头部月份标题旁显示干支生肖（如「2026年10月 · 丙午马年」，`--text-dim` 11px）；日期格 `title` 含公历 + 农历 + 节日。
- hover：`--surface-inset` 圆角 8px。

### 4.8 媒体卡

- 封面 64px、`--radius-md`；封面模糊副本铺底（页内 `filter: blur(18px)` + 40% 遮罩，不依赖桌面模糊）。
- 标题 13px/600 单行；歌手 11.5px `--text-dim`；进度 4px `--track`/`--accent`，时间 10.5px tabular。
- 控制：32px 圆形按钮（hover `--surface-inset`），播放键 40px `--accent` 实心。

### 4.9 管理面板

- 沿用 1060×760 窗口、168px 侧栏；`liquid` 下 `.mp-panel` 是整块玻璃（去 1px border，窗口客户区取样 + 30px 模糊），
  标题栏 `--surface-inset`、卡片 `--surface-inset` + `--lg-hairline`、投影 `--lg-shadow`；solid / glow 保持 v0.4 的
  `--surface-1` + 1px `--border` + `--shadow-panel`。
- **浅色透明**：白色阶 ≤`.14`，表面/卡片白色 alpha 下调（`--surface-2 .06`、`--surface-inset .12`）；
  暗壁纸时自动切 `data-ink="light"`（深色标题栏/卡片 + 浅色字），亮壁纸保持浅色玻璃 + 深色字。
- 开关：42×24 轨道 `--track`、选中 `--accent`、白色 18px 滑块；焦点环 `--accent-soft`。
  `liquid` 下选中轨道使用蓝玻璃（`rgb(0 122 255 / var(--lg-accent-alpha))` + 顶部白色内高光 + 底暗边），与 §4.2 磁贴同材质。
- 「通用」页新增「Dock 智能隐藏」开关，与「全屏自动隐藏」同组；关闭后 Dock 常驻，开启时按窗口遮挡自动滑下、底边悬停唤出。
- 行：名称 13px/500 + 描述 11.5px `--text-dim` + 可选状态小字 11px；危险按钮 hover `--danger`。
- 「多屏」页：顶部显示器列表复用列表行（24px 显示器图标 + 名称 + 主屏徽标 + 分辨率/缩放/坐标），下接两个 `mp-switch`（副屏顶栏 / 副屏 Dock）和「副屏底部工作区」「触发屏行为」状态行；单屏时两个开关置灰。

### 4.10 顶栏音量 OSD（滚轮调节反馈）

- 位置：顶栏内、控制中心入口左侧 8px；`top:4px`、高 24px。**不新增窗口、不改变顶栏窗口高度**（常态仅 32px，OSD 必须留在条内）。
- 外观：`--surface-3` 胶囊 + 1px `--border` + `--shadow-pop`；音量图标（静音时 muted）+ 百分比 11.5px/600 tabular（`min-width:26px` 右对齐）+ 36×4px 进度条（`--track` / `--accent`）。
- 交互：滚轮调节后显示约 1.2s，`--dur-fast` 淡入淡出（用 `.show` 类，不能用 `display:none` 的 `.hidden`）；静音或 0% 显示「静音」；`pointer-events:none`，不阻挡顶栏按钮。
- 无障碍：`role="status"` + `aria-live="polite"`，显示时 `aria-hidden="false"`。

### 4.11 多屏开关（控制中心「多屏」卡片）

- 两个并列开关：`副屏顶栏` / `副屏 Dock`；行高 28px，卡片与其它 `ctl-card` 同规格（16px 内边距、`--surface-2`、`--radius-lg`）。
- 开关轨道 34×20px、`--track` → `--accent`，滑块 14px；`role="switch"` + `aria-checked`；`focus-visible` 2px accent 外环。
- 单显示器时开关 `disabled`（0.4 透明度）并显示「仅 1 块显示器」；多屏时副标题显示「N 块显示器 · 主屏常开」。
- 交互即时生效：切换后窗口显隐/AppBar 预留/副屏 Dock 在 1 秒内完成，无需重启。

### 4.12 界面材质选择（控制中心 + 管理面板）

- **控制中心「外观」卡**：第一行明暗 2 段（浅色 / 深色），间隔 6px 后第二行材质 3 段（标准 / 液态玻璃 / 柔光玻璃）。
  两段与电源分段同规格（高 28px、`--radius-md`、`--surface-inset`），按钮内图标 14px + 文字 11.5px；
  `role="group"` + `aria-label`（明暗主题 / 界面材质），选中按钮 `aria-pressed="true"` + accent 内描边。
  卡片副标题显示「当前深色 · 液态玻璃」；切换后 200ms 内生效，不重建面板、不改窗口几何。
- **管理面板「外观」页**：新增一行「界面材质」+ 右侧 `.mp-seg` 三档（高 26px、`--mp-field-bg` 底、选中 accent-soft + 1px accent 内描边）；
  「当前外观」状态行改为「深色 · 液态玻璃 · Windows 浅色」。
- 两处入口共用 `theme.js` 的 `setMaterialStyle()`（单一写入路径 `set_material`），互相同步；失败回滚并提示。
- 键盘可达：Tab 进入分段控件，Enter/Space 触发；`focus-visible` 用 accent 环，不出现鼠标焦点环。
- 动效：液态玻璃面板打开时一次 820ms 扫光（`lg-sheen-sweep`，画在 `::after` 镜面层），柔光玻璃只做淡入；
  `prefers-reduced-motion: reduce` 下扫光整段不参与。
- 切换材质：切到 liquid 时若当前面板没有本次打开的有效取样，会临时隐藏面板补截一次（~30ms）再揭示；
  从 liquid 切走立即移除 `.lg-refract`，不重建面板。

## 5. 图标

- 来源：**Lucide**（ISC License）图标子集，内联为 `ui/js/icons.js`，不引入运行时依赖。
- 规格：24×24 viewBox、描边 1.8、`stroke-linecap/linejoin: round`、`currentColor`。
- 语义唯一：同一含义只用一个图标；按钮内 16px，列表行 18px，磁贴图标泡 18–20px，空态 48px。
- 兼容：`ICONS` 保留旧 key（`wifi/wifiOff/bluetooth/airplane/moon/volume/volumeMuted/sun`），顶栏聚合图标与面板共用同一来源。
- 托盘图标（位图）随主题处理：深色主题下近黑图标垫浅底（`.tray-dark`）；浅色主题下近白图标降低亮度（`.tray-invert`），主题切换后重新评估。

## 6. 交互与无障碍

- 所有图标按钮必须有 `title` / `aria-label`；开关使用 `aria-pressed`；分段控件 `role="group"`。
- 键盘可达：Tab 焦点顺序符合视觉顺序；`focus-visible` 显示 2–3px `--accent-soft` 外环；Esc 关闭面板/菜单。
- 状态覆盖：default / hover / active / selected / disabled / loading / error，全部有明确视觉。
- 动效尊重 `prefers-reduced-motion: reduce`。
- 对比度：正文 ≥ 4.5:1；≥12px 次要文字 ≥ 3:1；强调底上文字只使用 `--on-accent`。

### 6.1 已接受的例外（系统 UI 惯例 / 高度约束）

- **强调色实心控件上的白字**：`--on-accent`(#fff) on `--accent`（#3584e4）约 3.77:1。与 Windows / macOS 系统强调色按钮一致；
  若需正文级 WCAG AA，可调深 `--accent`（会改变品牌观感）或仅用于标题级文字。
- **分段选中态**：accent 文字 on accent-soft 约 3.49:1，但同时有 1px accent 边框与填充，作为「选中」状态可接受。
- **控制中心高度约束**：窗口 688 / 面板 604 下，磁贴取 84px、滑条行 32px、多屏开关行 28px、材质分段 28px + 6px 间距、卡片间距 8px；子视图行高 44px 保持宽松。更矮的显示器由 `.panel` 的 `max-height: calc(100vh - 84px)` 限制，`.ctl-main` 内部滚动。
- **材质可读性下限**：暗色按取样亮度解对比度（正文目标 4.7、验收 4.5，纯白壁纸约 `.86`、彩色壁纸约 `.54`）。
  **亮色 v0.5.1 去白底**：白色阶上限 `.14`（常规 `.10`），按合成亮度选黑/白墨色；中间亮度壁纸的最差墨色对比度约
  **4.0:1**（验收线 3.9，脚本 `npm run check:glass` 回归）。这是「不铺白底」的物理折中，已在 §6.1 记录；
  暗壁纸时表面会切 `data-ink="light"`（深色玻璃 + 浅色字），亮壁纸保持浅色玻璃 + 深色字。
  顶栏 / Dock / 管理面板 / 概览 / 应用中心使用同一套公式，只是取样矩形不同；
  solid / glow 仍沿用 v0.4 的固定底（暗色 liquid ≥ .76、glow ≥ .84）。阴影/描边不参与正文对比度。
- **玻璃质感非原生模糊**：各 surface 是铺满屏幕宽的独立透明窗口，原生 Acrylic/Mica 会把透明区域整片糊掉，故不使用。
  控制中心 / 通知中心在打开前用 Rust `BitBlt` 取样面板矩形（`glass_backdrop`），前端对取样图做 `blur(30px) saturate(150%)` + 轻微 `feDisplacementMap`；
  **顶栏 / Dock / 管理面板 / 活动概览 / 应用中心**由 Rust 在 `show` 前用 `window_backdrop` 截窗口客户区并缓存，
  前端通过 `surface_backdrop` 命令 / `surface-backdrop` 事件取图，按厚度做 18 / 24 / 30px 模糊；
  同窗口弹层（电源菜单 / 右键菜单 / tooltip）用真实 `backdrop-filter`（30px / 150%）。
- **取样是一次性的**：面板只在打开时截一张；顶栏 / Dock 只在自身 `show` 前截一张，管理面板 / 概览 / 应用中心每次打开重截。
  不逐帧刷新；可见期间桌面上移动/播放的内容不会实时模糊（下次 show / 重开刷新）。
  只在 liquid 取样；solid / glow 不截屏；启动 solid → 切 liquid 时顶栏/Dock/管理面板补截一次（概览/应用中心下次打开补）。
  截图只在内存、不落盘、不写日志；切到标准 / 柔光玻璃即不再取样。
- **亮色透明玻璃 + 自适应墨色**：亮色不再用 `.5+` 白底保深色字。背景亮时 `.10–.14` 白 + 深色字；
  背景暗时同色阶的深色玻璃 + 浅色字（`data-ink="light"` 切暗色 token）。中间亮度壁纸的黑/白墨色最大对比度约 4.0:1，
  低于 4.5 的正文级要求；这是用户明确要求「去除白色底色 / 非常透明」后的物理下限，验收线放宽到 3.9。
  若后续需要严格 AA，可把亮色白色阶上限调回 `.5`（会重新出现白底）或只在卡片/文本块局部加不透明底。
- **蓝玻璃白字**：`#007aff` 对纯白上限 4.04:1；液态玻璃按背景把 `.6 → ≤.9` 自适应，目标 ≥3.5:1（暗色通常 0.6 / 4.7:1）。
  沿用 §6.1 对强调色控件的例外，但不再使用「纯色块」。
- **打开延迟**：液态玻璃打开先截屏再拉高窗口，正常 +20–40ms（超时 220ms 自动回落）；固态 / 柔光玻璃不加延迟。
- **关闭态开关轨道 / 输入框边框**为 token 设计值（非文本 UI 对比约 1.3–1.6:1），沿用系统开关惯例。

## 7. 双主题视觉对齐清单（截图逐项核对）

| # | 检查项 |
| --- | --- |
| 1 | 4px 网格：所有 padding/gap/高度为 4 的倍数 |
| 2 | 圆角层级一致：标准 18 / 卡片·磁贴 14 / 按钮 6–10 / 胶囊 999（液态玻璃整体 +2~4：22 / 16；柔光玻璃 20 / 15） |
| 3 | 图标单一来源、视觉居中、描边一致，无混用图标家族 |
| 4 | 字号层级 ≤3 种/卡片，数字 tabular |
| 5 | 亮/暗两套都无纯白叠纯白、无低对比文字 |
| 6 | 无裁切/溢出/滚动条错位；小屏（1366×768）内容可滚动；控制中心 `scrollHeight === clientHeight` |
| 7 | 交互状态齐全且切换时无布局跳动 |
| 8 | 阴影三档且不被窗口边缘裁切；材质外发光（`--glass-glow`）不被圆角裁掉 |
| 9 | 动效 ≤240ms（分段滑块 340ms、液态玻璃扫光 820ms、Dock 弹跳 620ms 属一次性例外）、无阻塞、reduce-motion 有效 |
| 10 | 与参考设计气质一致：克制强调色、发丝边、留白均匀 |
| 11 | 材质间可辨：标准无霞光/镜面边，液态玻璃镜面边 + 扫光，柔光玻璃弥散外发光 + 暖调霞光 |
| 12 | 三种材质 × 亮暗共 6 组合下文字对比度与语义色一致（不因材质削弱状态色） |
| 13 | 动效手感：按压 0.96 即时缩小 + 弹回、分段选中块滑动不跳变、抽屉先出后缩窗口、无「位移中文字重排」 |
| 14 | Dock 智能隐藏：最大化/遮挡窗口压到 Dock 时隐藏；鼠标到 Dock 底边滑出、移开再隐藏；关闭开关后常驻且不遮挡点击 |
| 15 | 液态玻璃面板能真实看出被 30px 模糊的桌面（换壁纸重开面板后底图变化），启动截图不含面板自身 |
| 16 | 液态玻璃面板外轮廓无普通 1px border；顶部白镜面边、底部暗边、左右高光均可见；白壁纸下有 1px 暗环兜底 |
| 17 | 蓝玻璃磁贴：`rgb(0 122 255 / .6)` 起步、顶部白色内高光、底部暗边、白字可读；不是纯蓝块 |
| 18 | 投影 3 层且色相取自桌面平均色（暖壁纸暖影、冷壁纸冷影），不出现纯黑；边缘折射位移只在装饰层，文字不扭曲 |
| 19 | 截屏失败 / 超时回落 `[data-glass="fallback"]`，面板仍可读、可交互、无 `.lg-wait` 残留 |
| 20 | 主题切换只重算色阶不闪；材质切到 solid / glow 立即还原；切回 liquid 复用本次取样；`scrollHeight === clientHeight` |
| 21 | 顶栏 / Dock / 管理面板：无普通 1px border；顶栏 18px、Dock 24px、管理面板 30px 模糊；顶栏取样画面不含顶栏自身，Dock 取样不含 Dock 自身；Dock 圆角裁切不切掉图标放大 |
| 22 | 活动概览 / 应用中心：全屏取样在 scrim 之下，能看出被 30px 模糊的桌面；卡片 / 搜索条是低 alpha 玻璃 + 发丝边，不是实心面板；应用名直接浮在 scrim 上仍可读 |
| 23 | 管理面板：整块玻璃覆盖窗口客户区，标题栏/侧栏/卡片层级可辨；开关是蓝玻璃不是纯蓝色块；白/黑/彩色壁纸上正文可读 |
| 24 | surface 无取样（命令返回 null / 事件丢失）时 `data-glass="fallback"` 仍可读；切 solid/glow 后 `.lg-edge` 不参与 flex 布局；切回 liquid 恢复，无跳变 |
| 25 | 浅色玻璃白色阶 ≤`.18`（常规 `.10`），不再出现 `.5+` 白底；亮色 + 暗壁纸是深色透明玻璃 + 浅色字，亮色 + 亮壁纸是浅色透明玻璃 + 深色字 |
| 26 | `data-ink="light"` 时标题栏/卡片/正文/输入/开关一起切暗色 token，没有「深色玻璃 + 深色字」的混搭；中间亮度最差墨色对比度 ≥3.9，`npm run check:glass` 全绿 |

## 8. 截图 QA 流程

1. 以 1920×1080、100% 缩放启动 `npm run dev`（迭代期可设 `EASYDASH_DISABLE_TASKBAR_HIDE=1`）。
2. 截图矩阵：控制中心主视图（含「界面材质」分段）/ Wi-Fi 子视图 / 通知中心（有通知 + 空态）/ 活动概览 / 应用中心（第 1 页与多页分页器）/ 管理面板通用页 \
   × {亮色, 深色} × {标准, 液态玻璃, 柔光玻璃}（6 组合；材质验收至少覆盖控制中心、通知中心、顶栏+Dock、活动概览、应用中心、管理面板）。
3. 素材底图：至少在纯白、纯黑、彩色壁纸三种桌面上各抽一组，检查半透明表面上的正文对比度与镜面边是否可见。
   **液态玻璃额外必查**：换壁纸后重开控制中心/通知中心，确认玻璃底图跟着变（真实取样）；截屏里不能出现面板自身；
   亮色 + 白壁纸、亮色 + 黑壁纸、暗色 + 白壁纸、暗色 + 彩色壁纸四组是重点。
   **亮色去白底**：亮色 + 白壁纸必须是「很透的浅玻璃 + 深色字」，不能出现 `.5+` 白底；亮色 + 黑/彩色壁纸
   必须自动切 `data-ink="light"`（深色玻璃 + 浅色字），不能一边深色玻璃一边深色字；管理面板、概览、应用中心、顶栏、Dock 同查。
   **全部 surface 额外必查**：顶栏/Dock 的取样是不是它们自己（启动后立刻截图，条内不能出现玻璃套玻璃的糊影）；
   管理面板/概览/应用中心每次打开都重新取样（换壁纸后重开，底图变化）；概览/应用中心的卡片/搜索条不是实心块。
4. 逐项对照 §7 清单；问题记录为「组件 + 主题 + 材质 + 现象 + 期望」，回派对应 owner 修复后复检。
5. 同时检查顶栏、Dock、应用中心、活动概览在 6 组合下的一致性（tokens 是否漏用、图标是否可读、控制中心是否出现滚动条）。
6. 动效验收：录屏或逐帧截图检查「面板展开/收起 → 按压磁贴 → 切换材质 → 滑条拖动 → Dock 启动」五条链路；
   再开 `prefers-reduced-motion: reduce` 复检一次，确认没有位移/扫光/宽度拉伸。
7. 光学自检：`npm run check:glass`（两主题 × 背景亮度网格；暗色墨色 ≥4.5、亮色自适应墨色 ≥3.9、亮色白色阶 ≤.14、蓝玻璃白字 ≥3.5、阴影无纯黑）。
   Rust 侧 `cargo test --lib` 覆盖取样矩形/分位/缩放；`smoke_glass_backdrop`（ignored）在真实桌面上截一块并写 PNG 肉眼确认不是黑屏。
   surface 取样额外跑 `window_backdrop` 的窗口级路径（或复用同一 `capture_rect` 冒烟），确认顶栏 32px 条 / Dock 窗口 / 管理面板客户区都能出图。
8. 无 Tauri 权限 / WebView2 起不来时，用 `%TEMP%\easydash-qa` 的临时 harness（真实 `ui/css` + `ui/js` + 一份后端真实截屏）复现
   「打开前 32px → 截屏 → 拉高 → 揭示」流程，核对 `glass_backdrop` 参数、`data-glass`、`.lg-wait` 清理与 `scrollHeight === clientHeight`；
   surface 用真实 `ui/topbar.html` / `dock.html` / `overview.html` / `launcher.html` / `manager.html` + mock `__TAURI__`
   （`surface_backdrop` 返回一张底图、`surface-backdrop` / `theme-changed` 可手动 emit）核对四层、模糊半径、`clip-path`、卡片/搜索条变量；
   harness 只用于本地 QA，不提交仓库。

## 9. 动效规范（v0.4，iOS 手感）

> 实现：`ui/css/motion.css`（唯一动效来源）+ `ui/js/seg-thumb.js`（分段滑块）；
> 结构 token 在 `shared.css`，组件不再各写一套 keyframes。

### 9.1 曲线与时长

| Token | 值 | 用途 |
| --- | --- | --- |
| `--ease-spring` | `cubic-bezier(.34,1.56,.64,1)` | 按压回弹、分段滑块、开关滑块（有轻微过冲） |
| `--ease-spring-soft` | `cubic-bezier(.25,1.16,.36,1)` | 抽屉入场、toast、菜单弹出（几乎不过冲） |
| `--ease-out-fluid` | `cubic-bezier(.22,1,.36,1)` | 内容入场、退出、按压释放（快速起步、平缓收尾） |
| `--dur-press` / `--press-scale` | 110ms / 0.96 | 统一按压反馈 |
| `--dur-spring` | 340ms | 滑块位移 |
| `--dur-sheet` | 240ms（退出 150ms） | 面板 / 抽屉 |

### 9.2 各组件动效

| 组件 | 动效 | 时长 |
| --- | --- | --- |
| 磁贴 / 按钮 / 列表行 / 日历格 / 托盘项 | 按下 `scale 0.96`（用 `scale` 属性，不用 `transform`，避免与放大镜/拖拽冲突），松手弹回 | 110ms |
| 分段控件（明暗 / 材质 / 电源 / 管理面板材质） | 单一滑块在选中项之间弹性滑动；选中按钮背景透明，视觉由 `--accent-soft` 滑块 + 1px accent 边承担 | 340ms |
| 面板（控制中心 / 通知中心） | 入场 `translateY(-10px) scale(.972)` → 原位（弹性）；退出 `translateY(-8px) scale(.985)` + 淡出，**播完才隐藏并缩回 32px 窗口** | 入场 240ms / 退出 150ms |
| 面板内容 | 磁贴与卡片按 8/26/44/62/80ms 错峰上浮 8px | 300ms |
| 全屏表面（活动概览 / 应用中心） | 遮罩从桌面淡入（`opacity` 用 **transition**，不能用 animation：liquid 补截时 `.lg-wait` 会给 `#backdrop` 加 `animation:none !important`）；网格/面板 10px 上浮；窗口卡片 24ms、应用图标 10ms 错峰弹入；液态玻璃等底片补截结束再入场 | 遮罩 200ms / 容器 300–320ms / 单卡 200–260ms |
| 连接子视图（Wi-Fi / 蓝牙） | 从右侧 12px 滑入 | 260ms |
| 通知分组 | 打开面板时按 20–180ms 错峰上浮 + 轻微放大（仅 900ms 窗口内，新通知到达不重播） | 340ms |
| 展开的后续通知 | 逐条 4px 下落淡入 | 200ms |
| 开关 | 滑块弹性横移；按压时滑块拉宽 2–3px（iOS 拉伸感） | 340ms / 110ms |
| 滑条 | 悬停 1.12×、拖动 1.26× + accent 光环 | 120ms |
| 日历 | 今日胶囊 0.82→1.06→1 弹一下；日期格按压 0.92 | 420ms |
| Dock | 启动新进程时图标 620ms 弹跳（macOS 手感）；切换已有窗口不弹 | 620ms |
| Dock 智能隐藏 | 窗口遮挡时复用 `body.hidden` 同款下滑淡出；底边唤出反向滑回；隐藏即置 `pointer-events:none` + 后端指针穿透 | 320ms |
| 管理面板 | 切换页签内容 6px 上浮；toast `scale .92→1` 弹出 | 260ms |
| 菜单（右键 / 应用中心 / 电源） | `scale .96` + 2px 下落弹出 | 160ms |
| 顶栏音量 OSD | `scale .94→1` 弹一下 | 180ms |

### 9.3 约束

- 只动 `transform / scale / translate / opacity`；只有分段滑块与开关滑块会动宽度（≤3px，仅按压瞬间）。
- 全屏表面只在打开时播入场（关闭仍是瞬时隐藏：热键 / 托盘 / 概览与应用中心互斥切换等多个入口都在 Rust 侧，延后隐藏需要握手，不在当前范围）；卡片错峰 10–12 档后不再累加，`.surf-open` 播完即移除，不残留 `animation` / `fill`。
- 全屏表面的「关闭态」是常驻的：窗口隐藏后（Rust `hide()` → `surface-closed`）与页面加载时都回到 `.surf-closed`（遮罩 `opacity 0`），保证 `show()` 的首帧是透明而不是上一次的界面 —— 这是打开不闪屏的前提，不要在可见性/焦点事件里补播入场。
- 不使用无限循环动画；一次性动画最长 820ms（液态玻璃扫光），且只在入场播一次。
- `prefers-reduced-motion: reduce`：全局动画/过渡压到 0.01ms，`motion.css` 再显式关闭错峰入场与滑块拉伸。
- 动效不得改变布局（无 `width/height/margin` 过渡，开关滑块除外）；不得阻挡命中区域（滑块 `pointer-events: none`）。

## 10. 鼠标流光（v0.4）

> 实现：`ui/css/cursor-glow.css`（参数与光层样式）+ `ui/js/cursor-glow.js`（事件、命中、注入）。
> 参照 iOS 与国产安卓系统，拆成三层光效，不是无差别粒子拖尾。

### 10.1 三层光效与系统参照

| 层 | 效果 | 参照 |
| --- | --- | --- |
| 跟随柔光 `.glow-spot` | 光斑跟着指针走，带 60–110ms 甩尾；离开 160ms 淡出 | iOS 触感高光 / Liquid Glass 高光随触摸点 |
| 边框流光 `.glow-streak` | 液态玻璃下 1px 边框光沿元素边缘**持续绕行**（4.2s / 圈，离开即停）；指针方位决定起始角 | HyperOS / OriginOS / HarmonyOS 的「流光边框」 |
| 点按光晕 `.glow-burst` | 按下瞬间从落点扩散一圈柔光（8px → 190px，540ms 淡出）；面积 < 2500px² 的小按钮不播（避免铺满闪白，按压缩放已足够） | iOS 轻点高光 / 安卓 ripple 的柔光版 |

### 10.2 参数

| 项 | 标准 solid | 液态玻璃 liquid | 柔光玻璃 glow |
| --- | --- | --- | --- |
| 柔光半径（卡片 / 外壳） | 150px | 130px | 210px |
| 柔光半径（磁贴 / 图标 / 按钮） | 76–90px | 76–90px | 76–90px |
| 暗色强度（`--glow-strength`） | 0.12 | 0.26 | 0.32 |
| 亮色强度 | 0.12 | 0.24 | 0.30 |
| 跟随延迟（`--glow-follow`） | 60ms | 90ms（略「甩尾」） | 110ms |
| 光色 | 冷白 `rgba(255,255,255,.9)` | 冷白 | 暖白 `rgba(255,246,238,.95)` |
| 亮色光色 / 光晕色 | accent 45% + 白 | accent 55% + 白 | 暖橙 50% + 白 |
| 边框流光 | 无 | 有（流动，4.2s/圈） | 无 |
| 点按光晕 | 有（`--glow-burst` 白 .55） | 有 | 有（暖白 .6） |

### 10.3 命中面（就近命中，嵌套只亮最近一层）

顶栏 / Dock 条 / Dock 图标 / 面板外壳 / 管理面板外壳 / 磁贴 / 控制中心卡片 / 电源·锁屏按钮 /
通知卡 / 通知操作 / 应用中心搜索栏 / 应用图标 / 分页按钮 / 媒体卡 / 概览卡片 / 管理面板卡片与行。

### 10.4 实现与约束

- 光层是 JS 注入的 `.glow-spot` / `.glow-streak` / `.glow-burst` 三个独立 `span`，**不用伪元素**
  （外壳的 `::after` 已被材质噪点占用、`.panel::before` 是顶部高光）；三者必须是兄弟节点——
  父级 `opacity` 会把子元素整体压暗，边框弧与光晕会被削弱到看不见。
- 坐标/角度/半径走 `@property` 注册的 `--gx / --gy / --ga / --gaf / --bur`：
  `--gx/--gy` 用 transition 做甩尾跟随，`--ga` 是指针方位，`--gaf` 由 `glow-flow` 动画递增（边框光绕行），
  `--bur` 由 `glow-burst-out` 动画递增（光晕扩散）；不支持 `@property` 时退化为瞬时跟随（光斑仍工作）。
- 事件：document 上单个 passive `pointermove`（rAF 合并）+ `pointerdown`（点按光晕）；
  跟随光只响应 `pointerType === "mouse"`，点按光晕允许触屏；
  rect 缓存只在 host 切换 / resize / 捕获阶段 scroll / pointerdown / 500ms 心跳时刷新，避免逐帧强制布局。
- 交互安全：`pointer-events: none`、`aria-hidden`、不改布局（只为静态定位的 host 补 `position: relative`）、
  不抢焦点、不影响读屏；关闭开关后不再激活。
- 开关：管理面板「外观 → 鼠标流光」（`config.pointerGlow`，默认开），走 `update_preferences` → `config-changed` 全窗口同步。
- 降级：`prefers-reduced-motion: reduce` 下取消跟随延迟、边框流光与点按光晕（保留瞬时柔光）。

### 10.5 QA 要点

1. 指针依次掠过顶栏 → 面板 → 卡片 → 磁贴 → Dock，光点始终落在指针处（`--gx/--gy` 在 host 内）；
   离开内容区后淡出，不残留。
2. 液态玻璃下，磁贴 / 应用图标 / 搜索栏 / 通知卡 / Dock 出现沿边框**持续绕行**的 1px 流光弧
   （`--gaf` 递增，指针方位只决定起始角）；标准与柔光玻璃没有边框光。
3. 在卡片 / 磁贴 / 面板上按下 → 落点扩散一圈柔光，540ms 内消失；小按钮（电源菜单 / 链接按钮）不出现光晕。
4. 开关关闭后移动/点击均无光效；重新打开立即恢复。
5. `reduce-motion` 下无延迟跟随、无边框流光、无点按光晕。
6. 连续移动指针时无卡顿（rAF p95 < 16ms）、无 long task、无光层泄漏（每个 host 至多 1 组）。

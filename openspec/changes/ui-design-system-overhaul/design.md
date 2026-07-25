## Context

Termior 是 Rust + GPUI 单进程桌面应用，17 个业务 crate + 1 个 GPUI 应用 crate。UI 全部由 `crates/termior` 内的视图直接用 GPUI `div()` 组合而成，`workspace_view.rs` 约 5200 行，承载 header、tab、sidebar、分栏、状态栏与全部弹出层。

当前只有一层设计抽象是成立的：`termior-theme` 提供语义色板，`crates/termior/src/ui.rs` 提供 `palette()` 全局访问与 `muted` / `border` / `hover_wash` / `selected_wash` / `focus_ring` 等 helper，文件头注释明令禁止视图硬编码 rgba。除此之外没有任何共享抽象——`ui::button` 是全仓库唯一的共享控件原语。

约束条件：

- **gpui 版本被钉死**：`Cargo.toml` 的 `[patch.crates-io]` 把 gpui 指向 zed `3565c49`，并且 `[patch."https://github.com/zed-industries/zed"]` 把 `gpui_windows` 指向 `vendor/gpui_windows`（本地打过补丁，修 Windows 次级窗口关闭时的回调竞态）
- **NFR 门槛**：单二进制 ≤60MB（当前 release 29.8MiB）、常态 ≥60fps、空闲零重绘、冷启动 x86 Windows <800ms
- **spec 已固化的界面结构**：8 类 tab、sidebar 三面板、设置为独立窗口且六页签、附录 A 默认键位表、INV-1（tab 隐藏不销毁状态）
- **ADR 0002**：不内嵌 WebView，预览 tab 为占位面板

## Goals / Non-Goals

**Goals:**

- 建立可被所有视图消费的设计 token 层，使"间距/圆角/字号/层级"不再由各视图自行发明
- 用矢量图标替换 Unicode 字形，消除跨平台字形不一致与高 DPI 模糊
- 让主题选择的结果符合用户预期（选深色主题得到深色界面）
- 消除双层 chrome 与 header 的信息层级混乱
- 修复文件索引遇权限错误即全盘失效的 bug
- 全程保持 NFR-04 空闲零重绘与冷启动预算不回退

**Non-Goals:**

- 不引入命令面板（独立评估）
- 不改变 Composer 的停靠位置（需修订 spec FR-WS，独立产品决策）
- 不引入 i18n 运行时（仅集中文案为未来预留）
- 不重开内嵌 WebView（ADR 0002 保持）
- 不修 `settings_view.rs:71-74` 的 Windows 窗口销毁竞态（既有技术债，与本次改动正交）
- 不做像素级视觉回归 CI

## Decisions

### D1 — 不采用 gpui-component，自建轻量控件层

`gpui-component` 在其 `Cargo.toml` 中把 gpui 钉在 zed `1d217ee`；本仓库钉 `3565c49`，且 `gpui_windows` 已被 vendor 并打了补丁。采用它意味着：迁移 gpui rev、在新 rev 上重打 Windows 补丁并重新验证窗口关闭竞态，且此后每次跟随其升级都要重演一遍。

而实际缺口只有 `Icon` / `IconButton` / `Input` / `ListRow` / `Menu` / `Tooltip` / `EmptyState` 七类，量级可控。自建还能完全贴合 `termior-theme` 的 token，并天然满足 NFR-11（零增量依赖）。

**考虑过的替代方案**：(a) 迁 rev 后引入 gpui-component——被上述维护成本否决；(b) 按需 vendor 其单个组件源码——需要连带搬运其主题系统与内部工具函数，反而更脏；(c) 先做 spec 原计划的 M0 三组基线实验——rev 冲突已经足以定论，基线实验不会改变结论。

此决策关闭 `docs/termior-spec.md` §3.1 的开放问题 Q2，需落一份 ADR。

### D2 — 控件层落在 `termior-ui-kit`，用 feature gate 隔离 GPUI

spec §3.1 已把"token → 控件样式映射"的职责写给 `termior-ui-kit`，只是当前实现里它退化成了 `PaneLayout` / `SearchOverlay` 两个纯数据结构。把 GPUI 控件放进去是让代码回到 spec 意图，而不是再开第 19 个 crate。

`gpui` 依赖置于 `gpui` feature 之后，`PaneLayout` / `SearchOverlay` 的纯逻辑单测继续以 `--no-default-features` 快速运行。

**考虑过的替代方案**：(a) 新建 `termior-components` crate——让 spec 与代码进一步脱节；(b) 就地把 `crates/termior/src/ui.rs` 扩成模块目录——没有 crate 边界约束，视图可以继续就地手写 div，且任何控件微调都要重编译 5200 行的 `workspace_view.rs`。

### D3 — surface token 从数组改为语义命名字段

现状 `surface: [Color; 3]` 的三个下标被四种语义共用：`surface[2]` 同时是标题栏底色（`workspace_view.rs:4595`）、状态栏底色（`4746`）、Subtle 按钮底色（`ui.rs:152`）和分栏按钮组底色（`4670`）。数组下标无法自我解释，这正是它被滥用的直接原因。

改为语义字段（`chrome` / `panel` / `elevated` / `overlay`），并在浅色下拉开可感知的明暗差——当前浅色变体的层级落在 84%–97.5% 白之间，差距过小，是"各区域糊成一片"的直接原因。

这是 **BREAKING** 变更，影响已导出的 `Termior-custom-themes.json`。用 serde 字段别名 + 一次性迁移（读取时若命中旧的 `surface` 数组则按下标映射到新字段）保证向后兼容。

### D4 — 主题声明原生外观归属，`FollowSystem` 改为主题配对

现状 `styled_theme` 的浅色变体是从深色底色算出来的：`background = blend(底色, 白, 0.94)`（`termior-theme/src/lib.rs:337`），即 94% 白，八套主题只有 accent 有差别。用户选 "Tokyo Night" 得到近乎纯白的界面。

改为：每个主题声明自己是 light 还是 dark 原生；选择主题即切到它的原生外观；`FollowSystem` 变成"配一对主题"（浅色用 X、深色用 Y），沿用 Zed 的模型。`default` 与 `nord` 已有手写的浅色变体，可直接作为配对候选。

**考虑过的替代方案**：手工重写八套浅色变体——工作量大且很难做出真正可信的浅色版本（Tokyo Night 本身就不是为浅色设计的）。

### D5 — 图标走内嵌 SVG + GPUI `svg()`

GPUI 的 `svg()` 元素把 SVG 作为单色遮罩渲染，颜色跟随 `text_color`，能直接消费现有 palette。Zed 本身即此路径，踩坑成本低。选用 Lucide 子集（ISC 许可，与 Apache-2.0 兼容），约 30–40 个图标、20–40KB，对 60MB 二进制预算无影响。

需要新增 `AssetSource` 实现（`Application::new().with_assets(...)`）——当前 `assets/` 下只有应用图标（`.ico` / `.icns` / `.png`），代码里没有任何资源加载设施。

同时替换 `explorer_icon`（`workspace_view.rs:4837+`）的程序化 div 手绘图标，它硬编码了 `gpui::rgba(0xe5c07bff)`，违反 `ui.rs` 的禁令。

**考虑过的替代方案**：图标字体——与文字基线天然对齐、实现最简，但字形只能靠 codepoint 引用、可读性差，且加载自定义字体在跨平台上的坑不比 SVG 少。

### D6 — 自绘标题栏，平台能力已就绪

`vendor/gpui_windows` 已包含完整支持：`window.rs:451` 读取 `titlebar.appears_transparent`，`window.rs:381` / `982` 提供 `hit_test_window_control` 回调，`events.rs:890-910` 在 `WM_NCHITTEST` 中把 `WindowControlArea::{Drag, Min, Max, Close}` 映射到 `HTCAPTION`。无需再改 patch。

`app_identity.rs` 目前只设 `window_bounds` 与 `app_id`，需要补 `TitlebarOptions`。macOS 用原生红绿灯并设置 `traffic_light_position`；Windows / Linux 自绘三个窗口按钮。

### D7 — 焦点指示改为"压暗非活动方"

现状给聚焦 pane 画一圈 1px 纯 accent 实线，且单 pane 时也画——此时该框不传达任何信息。改为：单 pane 不绘制；多 pane 时非活动 pane 叠约 4–6% 的极淡遮罩。遮罩必须足够淡以不影响终端文本可读性，因此用叠加层而非整体降低不透明度。这是 iTerm2 / WezTerm 的既有做法。

### D8 — 文件索引改为容错

`termior-explorer/src/index.rs:87` 对每条 walk 结果使用 `?`，第一个不可读目录就中止整次索引；`workspace_view.rs:691-692` 收到 `Err` 后只设 `explorer_error`，`explorer` 保持 `None`，整棵树因此空白。

改为收集跳过项并走完全程，`IndexError::Walk` 不再用于单条目错误。跳过项数量在面板底部以一行低调提示呈现，可展开查看。这与 ripgrep / fd / VS Code 的行为一致。

### D9 — 动效只用于布局突变

hover / focus 保持即时切换（终端工具的用户在意跟手感）；侧边栏折叠、toast 进出、弹出层淡入使用短过渡。这些都是用户动作触发的有限时长动画，不影响 NFR-04 的空闲零重绘。

### D10 — 先做垂直切片验证方向

底座与主题模型两个阶段做完是看不见任何变化的，若视觉方向有误会发现得很晚。因此先做一个穿透所有层的垂直切片——自绘标题栏 + tab 合并 + 活动栏图标替换——它恰好需要用到 token、`AssetSource`、Lucide 子集、`Icon` / `IconButton` / `Tooltip`。同阶段固化截图脚本并跑 `termior-bench` 的冷启动与帧率检查（标题栏是全案唯一可能影响首帧的改动）。

## Risks / Trade-offs

**[自绘标题栏在 Windows 上的行为缺失]** → 全案唯一高风险项。需逐项验证：三个窗口按钮的状态与悬停、双击标题栏最大化/还原、四边与四角拖拽缩放、Snap Layouts 悬停最大化按钮弹出布局选择器、最大化时的窗口内边距补偿。切片阶段就把这些列成手工验收清单；若 Snap Layouts 无法工作，降级方案是先在 Windows 上保留原生标题栏（决策问答中的选项 C），不阻塞其余阶段。

**[`ResolvedPalette` 结构变更破坏已导出的自定义主题]** → serde 字段别名 + 读取时的下标映射迁移；`ThemeLibrary::import` 增加对旧格式的显式测试用例。用户基数尚小，现在改的代价远低于以后。

**[主题外观语义变更改变既有用户的界面]** → 已把 `theme_id` 设为 Tokyo Night 且外观为 Light 的用户，升级后会看到界面变深。这正是本次要修正的预期错位，但需要在设置的 Themes 页把"原生外观"与"配对"讲清楚，避免用户以为是 bug。

**[`workspace_view.rs` 约 5200 行，改动集中且易冲突]** → 按阶段拆 PR，每阶段只动其中一个区域（标题栏/header/焦点/状态栏）；不在本次做该文件的拆分重构，避免把两类风险叠加。

**[4px 栅格重排会改变现有布局密度]** → 这是有意的可见变化，不是回归。截图脚本产出的前后对比图入 PR 描述，由人工判断。

**[新增图标资源与 `AssetSource` 影响冷启动]** → SVG 资源在首次使用时解析，总量 20–40KB。切片阶段跑 `termior-bench` 冷启动检查确认无回退。

**[feature gate 增加 `termior-ui-kit` 的构建矩阵复杂度]** → CI 需同时验证 `--no-default-features`（纯逻辑）与默认（含 GPUI）两条路径。

## Migration Plan

1. **切片阶段**：token 骨架（仅切片所需）+ `AssetSource` + Lucide 子集 + `Icon` / `IconButton` / `Tooltip` + 自绘标题栏 + tab 合并 + 活动栏图标；固化截图脚本；跑 NFR 检查。**在此处停下，由人工确认视觉方向。**
2. **底座补齐**：完整尺寸 token、surface 语义重构（含主题 JSON 迁移）、`Input` / `ListRow` / `Menu` / `EmptyState`、文案模块
3. **主题模型**：原生外观归属、`FollowSystem` 配对、层级明暗差调整、设置 Themes 页文案
4. **主窗口其余**：header 收敛、焦点指示、状态栏上下文条、Composer 高度自适应
5. **设置窗口**：左侧导航、内容限宽、分组与说明、自动保存
6. **收尾**：Explorer 容错与跳过提示、空状态铺开、过渡动画、ADR 与 spec 修订

每阶段一个可回滚的 PR，描述中附前后截图。回滚粒度即 PR 粒度；阶段 3 的主题 JSON 迁移一旦发布，回滚需保留读取新格式的能力。

## Open Questions

- Lucide 子集的具体图标清单在切片阶段确定（切片只需活动栏三个 + 窗口控制三个 + tab 关闭）
- surface 语义字段的最终命名（`chrome` / `panel` / `elevated` / `overlay` 为初稿，实施时按实际用点核对）
- 资源内嵌用 `rust-embed` 还是 `include_dir`——按二进制体积与编译时间实测决定
- macOS 与 Linux 上自绘标题栏的验收清单需要各自补充（本次仅在 Windows 上有实测环境）

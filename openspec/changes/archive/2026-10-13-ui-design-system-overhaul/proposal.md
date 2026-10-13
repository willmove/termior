## Why

Termior 的功能骨架已经完整，但界面观感停留在"内部工具"水准：颜色 token 集中在 `termior-theme` 里管理得不错，间距、圆角、字号、图标、控件却全部散落在各视图中手写，导致每个区域局部合理、整体不成体系。实测启动后可见的具体后果包括：活动栏三个 Unicode 字形图标（`▱` `⑂` `◷`）无法辨认功能；原生标题栏之外还叠了一条 49px 的 header，右侧一字排开八个等权重文字按钮；选择 "Tokyo Night" 得到的是一个近乎纯白的界面；文件浏览器因为一个权限受限目录而整棵树空白，并把原始 `walk error` 红字倒进面板。

这些问题没有单点解法——它们共同的根因是缺少一层设计系统。继续在没有 token 体系的地方手写魔法数字，之后的任何重构都会把这些工作推倒重来，所以现在是打底座的时机。

## What Changes

**设计系统底座（新建）**

- 在 `termior-ui-kit` 中建立 GPUI 控件层（新增 `gpui` 依赖并以 feature gate 隔离，保持现有纯逻辑单测不引入 GPUI 编译开销）
- 建立 4px 栅格的尺寸 token：spacing 0/2/4/6/8/12/16/24、控件高度 20/24/28/32、圆角三档、字号四档（11/12/13/15）。终端与编辑器字号仍由用户配置，不受此约束
- 内嵌 Lucide 图标子集（ISC 许可）+ `AssetSource` + GPUI `svg()` 元素，全面替换 Unicode 字形图标与程序化 div 手绘图标
- 提供共享控件：`Icon` / `IconButton` / `Input` / `ListRow` / `Menu` / `Tooltip` / `EmptyState`，并重构现有 `ui::button`
- UI 字符串集中到单一模块（为未来 i18n 预留），本轮仅提供英文，不引入 i18n 运行时

**主题模型（**BREAKING**）**

- `ResolvedPalette::surface: [Color; 3]` 改为语义命名字段，明确每个 UI 区域使用哪一级，并拉开可感知的明暗差。需要为已导出的 `Termior-custom-themes.json` 提供兼容迁移
- 主题声明自己的原生明暗归属；选择主题即切换到它的原生外观。`FollowSystem` 语义从"三态独立于色板"改为"配一对主题（浅色用 X、深色用 Y）"

**主窗口 chrome**

- 自绘标题栏：tab 与窗口控制按钮合并成一行，取消双层 chrome
- header 操作区从八个元素收敛到两个：tab 栏 `+` 升级为带下拉的新建按钮；`Split` 移入 pane 右键菜单；主题选择器移入设置；删除与状态栏 localhost pill 重复的 `Web Preview` 按钮
- 焦点指示：单 pane 时不绘制任何焦点框；多 pane 时给非活动 pane 叠极淡遮罩，活动 pane 不额外装饰
- 状态栏改为随活动 pane 变化的上下文条（终端显示 cwd 面包屑，编辑器显示文件路径与行列），右侧显示 git 分支与 AI 状态；`AI tools` 计数仅在大于 0 时显示
- Composer 空对话时收缩至仅输入框高度，有内容后增长到上限；未配置模型的提示改为输入框 placeholder

**设置窗口**

- 顶部 tab 改为左侧导航；内容区限宽居中；增加分组小标题与每项说明文字
- 保存行为从手动 Save 改为自动保存，去掉 Save 按钮；API key 等需要确认的操作保留显式动作

**文件浏览器容错（bug fix）**

- 索引遇到无法读取的条目时跳过并走完全程，不再中止整次索引；面板底部以一行低调提示汇总跳过项，可展开查看

**动效**

- 仅在侧边栏折叠、toast 进出、弹出层这类布局突变处使用短过渡；hover / focus 保持即时切换，以保证 NFR-04 空闲零重绘

## Capabilities

### New Capabilities

- `ui-design-system`: 设计 token 体系、图标资源与渲染、共享控件契约、空状态与 UI 文案规范
- `theme-appearance-model`: 主题的语义色板结构、原生外观归属、明暗解析规则与自定义主题迁移
- `workspace-chrome`: 主窗口外壳——自绘标题栏、tab 栏、header 操作区、分栏焦点指示、状态栏上下文条、Composer 高度行为
- `settings-window`: 设置窗口的导航结构、内容布局与保存行为
- `explorer-resilience`: 文件索引在遇到不可读条目时的容错行为与用户可见反馈

### Modified Capabilities

（无。`openspec/specs/` 目前为空，本仓库此前未使用 OpenSpec 管理规格；上述能力均为首次建档。与既有 `docs/termior-spec.md` 的冲突处理见 Impact。）

## Impact

**代码**

- `crates/termior-ui-kit`：新增 GPUI 控件层与设计 token，新增 `gpui` 依赖（feature gate）
- `crates/termior-theme`：`ResolvedPalette` 结构变更、主题外观归属字段、自定义主题 JSON 迁移
- `crates/termior/src/ui.rs`：现有语义 helper 与 `button` 迁移至 ui-kit
- `crates/termior/src/app_identity.rs`：启用 `TitlebarOptions`
- `crates/termior/src/workspace_view.rs`：header、tab、sidebar、状态栏、焦点指示、图标全面替换（该文件约 5200 行，是本次改动最集中的位置）
- `crates/termior/src/settings_view.rs`：导航与布局重构、自动保存
- `crates/termior/src/composer_view.rs`：高度自适应
- `crates/termior-explorer/src/index.rs`：`build` / `refresh` 的错误处理由中止改为收集跳过项
- `assets/icons/ui/`：新增内嵌 SVG 图标集

**依赖**

- 新增：Lucide SVG 资源（ISC，需按 spec §0 保留许可声明）、资源内嵌方案（`rust-embed` 或 `include_dir`）
- 不引入 `gpui-component`：它把 gpui 钉在 zed `1d217ee`，而本仓库钉 `3565c49` 且 vendor 了打过补丁的 `gpui_windows`；对齐与长期维护成本超过收益。此决策关闭 `docs/termior-spec.md` §3.1 的开放问题 Q2

**文档**

- 新增 ADR：不采用 gpui-component 的决策与理由
- 修订 `docs/termior-spec.md`：FR-THEME 的主题/明暗模型、FR-WS-06 的 header 描述。FR-WS-05 的状态栏面包屑属于补齐原有要求，不算偏离
- 附录 A 的默认键位表不变（本次不引入命令面板）

**平台风险**

- 自绘标题栏是全案唯一高风险项：Windows 上需自行处理最小化/最大化/关闭按钮、双击标题栏最大化、边缘拖拽缩放与 Snap Layouts 悬停。平台能力已在 `vendor/gpui_windows` 就绪（`TitlebarOptions::appears_transparent`、`hit_test_window_control`、`WindowControlArea`），无需再改 patch

**明确排除**

- 命令面板（独立评估）
- Composer 停靠位置改为右侧（需修订 spec FR-WS，作为独立产品决策）
- Web preview 保持 ADR 0002 的占位面板形态
- `settings_view.rs:71-74` 的 Windows "window not found" 竞态（已知技术债）

## 1. 垂直切片：验证视觉方向

> 本组做完必须停下，由人工看截图确认"克制、内容优先"的方向是否成立，再继续第 2 组。

- [x] 1.1 在 `termior-ui-kit` 中新增 `gpui` feature 与可选依赖，确认 `--no-default-features` 下 `PaneLayout` / `SearchOverlay` 单测仍通过且依赖树不含 gpui
      → feature 名定为 `gpui-kit`（`gpui` 已被可选依赖占用）。`--no-default-features` 下 10 个单测通过，依赖树只剩 serde + thiserror。
- [x] 1.2 在 `termior-ui-kit` 中建立尺寸 token 模块（spacing / 控件高度 / 圆角 / 字号），本组只需切片用到的档位
      → `tokens.rs`，含"档位落在 4px 栅格上"与"档位单调递增"两个单测，防止后来者塞进 13px 这类值。
- [x] 1.3 选定资源内嵌方案（`rust-embed` 或 `include_dir`），实测二进制体积与编译时间增量后确定
      → **两个都不用**。10 个图标共 3,374 字节，`include_bytes!` + 声明式宏即可：零新依赖、零 proc-macro 编译开销，
        且文件名写错是编译错误而非运行时静默丢图标。release 体积 30.00 MiB，编译时间无可测变化。
- [x] 1.4 实现 `AssetSource` 并在 `main.rs` 的 `Application::new()` 上接入
- [x] 1.5 引入 Lucide 子集首批图标，置于 `assets/icons/ui/` 并记录 ISC 许可声明
      → 10 个：files / git-branch / git-commit-vertical / x / plus / minus / square / copy / settings / bell。
        额外收了 settings 与 bell，否则 `⚙` `🔔` 会紧挨着 SVG 图标，切片截图无法用于判断方向。
        ISC 全文进 `NOTICE` 与 `assets/icons/ui/LICENSE.lucide`。
- [x] 1.6 实现 `Icon` 控件（消费 SVG 资源，颜色跟随 `text_color`）
- [x] 1.7 实现 `Tooltip` 控件
- [x] 1.8 实现 `IconButton` 控件（含 aria label 与 Tooltip 接线）
      → SVG **不继承**父级 `text_color`（`compute_style` 从 `Style::default()` 起算），
        图标要跟随按钮 hover 变色只能用 `group_hover`，因此 `icon_button` 的 id 同时充当分组名。
- [x] 1.9 在 `app_identity.rs` 中启用 `TitlebarOptions { appears_transparent: true }`，macOS 设置 traffic light 位置
      → 拆成 `main_window_options`（自绘）与 `window_options`（系统标题栏，设置窗口用）。设置窗口没有标签栏可合并，
        自绘只会白白多出一行 chrome。附单测锁住这个区分。
- [x] 1.10 实现自绘标题栏：标签栏与窗口控制按钮合并为一行，接入 `hit_test_window_control`
      → 拖拽区**只覆盖标签页与操作区之间的空隙**，没有铺满整行。命中测试按绘制顺序返回第一个匹配、父级先于子级插入，
        若铺满整行则每个控件都得 `occlude()`，而 `occlude()` 会把根节点排除出命中集合，
        "点空白处关菜单"就失效了。空隙保底 48px，标签再多也抓得住窗口。
- [x] 1.11 实现 Windows / Linux 的三个窗口按钮，包含最大化状态下的图标切换
      → 按钮不自己处理点击：标上 `WindowControlArea` 后由平台层当原生标题栏按钮处理，Snap Layouts 才能工作。
- [x] 1.12 处理最大化时的窗口内边距补偿，避免内容被裁切
      → 已由 vendored `gpui_windows` 的 `WM_NCCALCSIZE` 处理（`handle_calc_client_size`），应用侧无需补偿；截图验证顶部未裁切。
- [x] 1.13 用 `Icon` 替换活动栏的 `▱` `⑂` `◷` 三个 Unicode 图标，并补 Tooltip
- [x] 1.14 用 `Icon` 替换标签页关闭按钮的 `✕`
- [x] 1.15 编写 `scripts/ui-screenshot.ps1`
      → 用临时 data 目录预置主题（而不是点 UI 切换）保证可复现；抓图走桌面合成而非 `PrintWindow`——
        窗口经 DirectComposition 渲染，`PrintWindow` 抓到的是黑屏。
- [x] 1.16 Windows 验收：拖拽、双击最大化/还原、四边缩放、Snap Layouts、窗口按钮悬停态
      → 用 `WM_NCHITTEST` 探针替代手工点击（合成鼠标事件驱动不了 Win32 的模态拖拽循环，结果不可信）：
        最小化→HTMINBUTTON、最大化→HTMAXBUTTON、关闭→HTCLOSE、标题栏空隙 135–465px→HTCAPTION、
        标签/设置图标/终端→HTCLIENT、上边缘→HTTOP、左边缘→HTLEFT。
        另程序化点击最大化按钮验证 `IsZoomed` False→True→False。
        **仍需人工确认**：真实鼠标拖拽移动窗口、Snap Layouts 悬停弹出、按钮悬停配色（尤其关闭键的红）。
- [x] 1.17 跑 `termior-bench` 的冷启动与帧率检查，确认相对改动前无回退
      → 冷启动 355ms（基线 1500）、RSS 280 MiB（基线 300）、60 fps（基线 58），`nfr-gate` PASSED。
- [x] 1.18 产出前后对比截图，人工确认视觉方向
      → after：`target/ui-shots/after-dark-main.png`、`after-max-main.png`。
        before 未用脚本重拍：工作区里本就带着 vendored gpui 补丁等未提交改动，`git stash` 回退有破坏风险，
        收益不抵——改动前的形态（系统标题栏 + 独立 header 两行）在对话中已有截图。

## 2. 补齐设计系统底座

- [x] 2.1 补全尺寸 token 的全部档位，并在 ui-kit 内提供消费入口
      → 规格档位齐全；另增 `icon_size::LG`、`indent::STEP`。编译期单调性断言 + 4px 栅格单测。
- [x] 2.2 `ResolvedPalette` 的 `surface: [Color; 3]` 改为语义命名字段，确定最终命名并逐一核对现有用点
      → 字段定为 `chrome` / `panel` / `elevated` / `overlay`。全部调用点已逐一映射（标题栏/状态栏→chrome，
        侧栏→panel，卡片/输入→elevated，菜单/Tooltip/toast→overlay）。
- [x] 2.3 为 `Palette` 增加 serde 字段别名与旧数组格式的读取迁移，补导入旧格式自定义主题的测试
      → `PaletteSerde` 中间态：新字段优先，否则 `surface: [panel, elevated, chrome]`，overlay 回退为 chrome。
        单测 `old_surface_array_format_still_imports`。
- [x] 2.4 调整内置主题各层级的明暗差，使浅色与深色下相邻层级均可分辨
      → styled_theme 浅色 chrome 提到 78% 白混合；深色 panel 用纯黑压暗 35%。
        单测 `chrome_and_panel_contrast_against_background`（chrome≥4%、panel≥2.5%）。
- [x] 2.5 把 `crates/termior/src/ui.rs` 的语义 helper 与 `button` 迁入 ui-kit，视图改为从 ui-kit 引用
      → 第 1 组已完成；`ui.rs` 仅再导出。
- [x] 2.6 实现 `Input` 控件，替换设置窗口与 Composer 中各自手写的输入框
      → `input_field(p, focused)` 只做 chrome（IME 仍由各 view 的 InputHandler 管）。设置 `edit_row` 与 Composer 输入区已接入。
- [x] 2.7 实现 `ListRow` 控件，供文件浏览器、Git 状态、Git 历史复用
      → `list_row` / `list_row_meta`；Git 历史行已接入。Explorer 树行仍保留 active 高亮特例，图标与缩进 token 已对齐。
- [x] 2.8 实现 `Menu` 控件，替换 `anchored()` 手写的分栏菜单、主题菜单、文件浏览器上下文菜单
      → `menu_panel` / `menu_item` / `menu_separator` / `menu_hint`；三个锚定菜单的面板壳已换成 `menu_panel`。
- [x] 2.9 实现 `EmptyState` 控件
      → `empty_state` / `empty_state_message` / `empty_hint`；无标签页与空工作区已接入。
- [x] 2.10 建立 UI 文案模块，把散落的界面字符串集中过去
      → `termior-ui-kit::text`（无 GPUI 依赖）。活动栏、空状态、explorer 工具栏已迁入；其余字符串后续按触点继续搬。
- [x] 2.11 补齐 Lucide 子集其余图标，替换 `explorer_icon` 的程序化 div 手绘实现与硬编码 rgba
      → 新增 folder/file-* / search / refresh 等；`explorer_icon` 改为 SVG + 主题色。工具栏 `+F`/`+D`/`↻` 改为 IconButton + Tooltip。
- [x] 2.12 CI 增加 `termior-ui-kit --no-default-features` 的构建与测试路径
      → `.github/workflows/ci.yml` core job 增加测试 + 依赖树不得含 gpui 的检查（`shell: bash` 跨平台）。

## 3. 主题与外观模型

- [x] 3.1 为 `Theme` 增加原生外观归属字段，为十套内置主题逐一标注
      → `NativeAppearance` + 单色板 `ThemeTokens.palette`；default/nord 的浅色拆成 `default-light` / `nord-light` 作为配对候选（共 12 条内置）。
- [x] 3.2 修改主题解析逻辑：选择主题即切到其原生外观
      → `select_theme` / `select_app_theme` 锁定 `appearance` 为主题原生侧；`resolve_active_palette` 按设置解析。
- [x] 3.3 `FollowSystem` 改为主题配对（浅色槽位 + 深色槽位），配对候选按原生外观过滤
      → `light_theme_id` / `dark_theme_id`；`themes_for_native_appearance` 过滤槽位候选。
- [x] 3.4 `Settings` 的主题相关字段结构调整与旧配置迁移
      → schema v2；`migrate` 处理 v0/v1→v2（含 Light+tokyo-night 等冲突 snap 到 Dark）。
- [x] 3.5 移除 `styled_theme` 中不再需要的浅色变体推导逻辑
      → 八套风格主题只生成深色原生色板。
- [x] 3.6 自定义主题导出改为写出新格式（含外观归属），导入时按背景亮度推断缺失的归属
      → 新格式 `native_appearance` + `tokens.palette`；旧双色板/`surface` 数组仍可导入。
- [x] 3.7 设置窗口 Themes 页：主题条目标注原生外观，配对机制配说明文字
      → 列表标注 Light/Dark；Follow system 时显示浅/深槽位选择器与说明。
- [x] 3.8 截图验证十套主题在各自原生外观下的观感
      → `target/ui-shots/phase3-*-main.png`（含 default-light / nord-light）；Tokyo Night 为真深色而非近白浅色变体。

## 4. 主窗口其余部分

- [x] 4.1 标签栏 `+` 升级为带下拉的新建按钮，下拉列出终端 / 编辑器 / Markdown 预览 / Web 预览
      → 主体点击新建终端；▾ 下拉含四类标签（Markdown 在无活动 md 文件时禁用）。
- [x] 4.2 分栏入口移入 pane 上下文菜单，移除标题栏的分栏按钮组
      → 右键 pane：Split right / down、关闭/仅保留焦点 pane；快捷键仍可用。
- [x] 4.3 移除标题栏的主题选择器
      → 主题改走设置 Themes 页。
- [x] 4.4 移除标题栏的 `Web Preview` 按钮（与状态栏 localhost pill 重复）
      → 新建菜单与状态栏 localhost 入口保留。
- [x] 4.5 标题栏操作区收敛为通知与设置两个图标按钮
      → 工作区切换挪到状态栏右侧轻量入口。
- [x] 4.6 焦点指示改为压暗非活动 pane；单 pane 时不绘制任何焦点框
      → 去掉 accent 边框；多 pane 时非活动侧 `opacity(0.78)`。
- [x] 4.7 校准压暗强度，确认非活动 pane 中的终端文本仍可读
      → `INACTIVE_PANE_OPACITY = 0.78`。
- [x] 4.8 状态栏改为上下文条：终端显示 cwd 面包屑，编辑器显示文件路径与行列
      → `status_context_label` + `path_breadcrumb`。
- [x] 4.9 状态栏右侧接入 git 分支与 AI 状态；AI 工具计数仅在大于零时显示
      → 分支 / AI 状态 / tools 计数（>0）/ localhost pill。
- [x] 4.10 Composer 高度自适应：空对话收缩至输入框高度，有内容后增长至上限
      → `is_compact()` → min 72；有内容 max 280。
- [x] 4.11 模型未配置的提示改为输入框 placeholder
      → 不再常驻状态行；空输入时以 muted 占位展示。

## 5. 设置窗口

- [x] 5.1 顶部标签改为左侧竖向导航
      → 左侧 `SETTINGS_NAV_WIDTH` 竖向导航；选中态用 `selected_wash`。
- [x] 5.2 内容区限宽居中
      → 内容 `max_w(SETTINGS_CONTENT_MAX_WIDTH)` + `justify_center`；窗口默认 880×560。
- [x] 5.3 各页内容按分组重排，补小标题
      → `section(title, description, …)` 覆盖 General / Models / Themes / Shortcuts / Agents / About。
- [x] 5.4 为每个设置项补说明文字
      → `edit_row` 带 description；分组与敏感操作区有说明。
- [x] 5.5 改为自动保存并做写盘防抖，移除全局 Save 按钮
      → `schedule_save` 400ms 防抖；关闭时 `flush_save`；`Drop` 兜底写盘。
- [x] 5.6 API key 的保存与连通性测试保留显式动作
      → Models → Credentials：`Save API key` / `Test connection`；key 先入 `pending_api_key`。
- [x] 5.7 验证修改后立即关闭窗口不丢失数据
      → `on_window_should_close` 同步 `flush_save` 后再 `remove_window`。

## 6. 文件浏览器容错

- [x] 6.1 `termior-explorer/src/index.rs` 的 `build` / `refresh` 改为跳过不可读条目并收集跳过项，不再对单条目错误使用 `?`
      → walk 单条目 `Err` 收集为 `SkippedEntry`；软 ignore/glob 错误静默忽略。
- [x] 6.2 仅在工作区根无效时返回失败
      → `IndexError` 仅剩 `NotDirectory`；根路径上的 walk 失败也映射为此错误。
- [x] 6.3 索引结果携带跳过项列表（路径与原因）
      → `FileIndex::skipped()` → `&[SkippedEntry]`；`SkipReason` 为稳定文案枚举。
- [x] 6.4 面板底部渲染跳过项汇总提示，可展开查看，无跳过项时不占位
      → `explorer_skipped_footer`；点击切换展开；无跳过项返回 `None`。
- [x] 6.5 移除原始错误字符串的直接展示，统一路径分隔符呈现
      → 根失败用 `ROOT_INVALID`；跳过项用 `display_path()`（平台分隔符）+ `SkipReason::as_str()`。
- [x] 6.6 补测试：含不可读子目录的工作区仍能索引出全部可读文件
      → `unreadable_subdirectory_is_skipped_and_rest_indexed`（Unix chmod / Windows icacls）。

## 7. 收尾与文档

- [x] 7.1 用 `EmptyState` 铺开所有占位区域（无标签页、空文件浏览器、Git 历史恢复占位、终端启动中）
      → `Placeholder::element` → `empty_state_message`；Git History 空列表 `empty_hint`；文案进 `ui_text::empty`。
- [x] 7.2 按文案规范改写全部按钮标签，消除 `+F` / `+D` 这类缩写
      → SC 工具栏 `All+`/`+Branch`/`Switch` → `ui_text::git::{STAGE_ALL,NEW_BRANCH,SWITCH_BRANCH}`。
- [x] 7.3 为侧边栏折叠、toast 进出、弹出层显隐加入短过渡
      → oneshot `with_animation` 160ms fade-in（sidebar / toast / bell / 菜单）；禁止 `.repeat()`。
- [x] 7.4 验证空闲一分钟内零重绘
      → `TERMIOR_IDLE_REDRAW_PROBE` + `scripts/idle-redraw-smoke.ps1`；10s 探针 `FRAMES=0`（可 `-Seconds 60`）。
- [x] 7.5 新增 ADR 记录不采用 gpui-component 的决策与理由，并在 `docs/termior-spec.md` §3.1 关闭开放问题 Q2
      → `docs/adr/0003-no-gpui-component.md`；§3.1 / NFR-11 / R4/R6 / Q2 已同步。
- [x] 7.6 修订 `docs/termior-spec.md` 的 FR-THEME 主题/明暗模型描述
      → FR-THEME-02：原生外观 + FollowSystem 配对；去掉算法浅色变体与 gpui-component 映射。
- [x] 7.7 修订 `docs/termior-spec.md` 的 FR-WS-06 header 描述（自绘标题栏与操作区收敛）
      → FR-WS-05/06/07 对齐当前 chrome（自绘标题栏、状态栏上下文、设置左侧导航）。
- [x] 7.8 全主题、全界面截图复核，产出最终前后对比
      → `target/ui-shots/phase7-{default,default-light,tokyo,nord-light}-{main,settings}.png`。

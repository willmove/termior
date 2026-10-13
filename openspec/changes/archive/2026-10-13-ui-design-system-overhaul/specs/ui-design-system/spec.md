## ADDED Requirements

### Requirement: 尺寸 token 集中定义

系统 SHALL 在 `termior-ui-kit` 中集中定义界面尺寸 token，覆盖间距、控件高度、圆角与字号四类，全部落在 4px 栅格上。视图层 MUST NOT 直接书写像素字面量来表达这四类尺寸。

终端与编辑器的字号、行高、字间距由用户设置驱动，MUST NOT 受此 token 体系约束。

#### Scenario: token 取值落在约定档位

- **WHEN** 读取尺寸 token 定义
- **THEN** 间距档位为 0/2/4/6/8/12/16/24，控件高度档位为 20/24/28/32，圆角为三档，字号为 11/12/13/15 四档

#### Scenario: 终端字号不受 token 约束

- **WHEN** 用户在设置中把终端字号改为 18
- **THEN** 终端以 18px 渲染，且界面其余部分的字号仍取自 token 体系

### Requirement: 语义化 surface 层级

`ResolvedPalette` SHALL 用语义命名字段表达表面层级，替代下标数组。每个界面区域 MUST 声明它使用哪一级，同一语义 MUST NOT 被复用于不相关的用途。

浅色与深色变体下，相邻层级之间 MUST 存在人眼可分辨的明暗差。

#### Scenario: 区域层级可辨认

- **WHEN** 在任一内置主题下截取主窗口
- **THEN** 标题栏、侧边栏、内容区、状态栏之间的背景色差异肉眼可分辨，无需依赖分隔线

#### Scenario: 旧格式自定义主题仍可导入

- **WHEN** 导入使用旧 `surface` 数组格式的自定义主题 JSON
- **THEN** 系统按下标映射到新的语义字段并成功加载，不报错

### Requirement: 矢量图标系统

界面图标 SHALL 由内嵌 SVG 资源经 GPUI `svg()` 元素渲染，颜色跟随当前主题 token。系统 MUST NOT 使用 Unicode 字形或程序化绘制的 div 作为功能图标。

#### Scenario: 图标随主题着色

- **WHEN** 用户从深色主题切换到浅色主题
- **THEN** 所有界面图标的颜色随之改变，无任何图标保持原色

#### Scenario: 高 DPI 下图标清晰

- **WHEN** 在 200% 缩放的显示器上查看活动栏
- **THEN** 图标边缘锐利无模糊，且三个面板图标的功能可被辨认

### Requirement: 共享控件层

`termior-ui-kit` SHALL 提供 `Icon`、`IconButton`、`Button`、`Input`、`ListRow`、`Menu`、`Tooltip`、`EmptyState` 控件，全部消费主题 token 与尺寸 token。业务视图 SHALL 优先使用这些控件而非就地组合 `div()`。

`gpui` 依赖 MUST 置于 feature 之后，使 `PaneLayout`、`SearchOverlay` 等纯逻辑单元可在不编译 GPUI 的情况下测试。

#### Scenario: 纯逻辑测试不依赖 GPUI

- **WHEN** 以 `--no-default-features` 编译并测试 `termior-ui-kit`
- **THEN** 编译成功，`PaneLayout` 与 `SearchOverlay` 的单测全部通过，依赖树中不含 gpui

#### Scenario: 图标按钮具备可访问名称

- **WHEN** 渲染一个只有图标没有文字的按钮
- **THEN** 该按钮带有 aria label，并在悬停时显示 Tooltip 说明其功能

### Requirement: 空状态与 UI 文案

所有占位与空白区域 SHALL 通过 `EmptyState` 控件呈现，包含图标、一句说明，以及可选的主操作与快捷键提示。

所有面向用户的界面字符串 SHALL 集中于单一模块。按钮标签 MUST NOT 使用无法自解释的缩写。

#### Scenario: 无标签页时的空状态

- **WHEN** 工作区中没有任何标签页
- **THEN** 内容区显示带图标的空状态，说明可创建终端，并给出对应快捷键

#### Scenario: 按钮标签可自解释

- **WHEN** 查看文件浏览器面板的操作按钮
- **THEN** 每个按钮的用途可从其图标与 Tooltip 判断，界面上不出现 `+F`、`+D` 这类缩写标签

### Requirement: 动效限定于布局突变

过渡动画 SHALL 仅用于侧边栏折叠、toast 进出、弹出层显隐这类布局突变。悬停与焦点状态 MUST 立即切换，不使用时间轴动画。

应用在无用户输入时 MUST NOT 产生重绘。

#### Scenario: 空闲状态零重绘

- **WHEN** 应用处于前台但一分钟内无任何输入且无后台任务
- **THEN** 该期间的帧渲染次数为零

#### Scenario: 悬停立即响应

- **WHEN** 鼠标移入一个按钮
- **THEN** 按钮背景在下一帧即呈现悬停态，无渐变过程

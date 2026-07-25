## ADDED Requirements

### Requirement: 单层标题栏

主窗口 SHALL 使用自绘标题栏，标签页与窗口控制按钮位于同一行。系统 MUST NOT 在原生标题栏之下再叠加一条独立的应用内 header 条。

#### Scenario: 标签页位于标题栏内

- **WHEN** 打开主窗口
- **THEN** 标签页直接呈现在窗口最顶部一行，其右侧为窗口控制按钮，其间无第二条水平分区

#### Scenario: 拖拽标题栏移动窗口

- **WHEN** 用户按住标题栏中不属于标签页或按钮的区域并拖动
- **THEN** 窗口随之移动

#### Scenario: 双击标题栏切换最大化

- **WHEN** 用户双击标题栏空白区域
- **THEN** 窗口在最大化与还原之间切换

#### Scenario: 窗口边缘可拖拽缩放

- **WHEN** 用户拖拽窗口的任一边或角
- **THEN** 窗口尺寸随之改变

#### Scenario: Windows 上支持 Snap Layouts

- **WHEN** 在 Windows 上把鼠标悬停在最大化按钮上
- **THEN** 系统弹出 Snap Layouts 布局选择器

### Requirement: 收敛的标题栏操作区

标题栏操作区 SHALL 只保留通知与设置两个常驻入口。新建操作 SHALL 由标签栏的新建按钮及其下拉承担；分栏操作 SHALL 通过 pane 上下文菜单与快捷键触发；主题选择 SHALL 位于设置窗口内。

系统 MUST NOT 在标题栏保留与状态栏功能重复的入口。

#### Scenario: 新建下拉包含全部标签类型

- **WHEN** 用户点击标签栏新建按钮的下拉箭头
- **THEN** 菜单列出终端、编辑器、Markdown 预览、Web 预览等可创建的标签类型

#### Scenario: 新建按钮主操作创建终端

- **WHEN** 用户点击新建按钮主体而非下拉箭头
- **THEN** 直接创建一个终端标签页

#### Scenario: 分栏入口移至上下文菜单

- **WHEN** 用户在一个 pane 上打开上下文菜单
- **THEN** 菜单包含向右分栏与向下分栏，且标题栏不再有分栏按钮

### Requirement: 分栏焦点指示

当标签页内只有一个 pane 时，系统 MUST NOT 绘制任何焦点指示。存在多个 pane 时，系统 SHALL 通过压暗非活动 pane 来表达焦点，活动 pane MUST NOT 被额外装饰。

压暗程度 MUST 不影响终端文本的可读性。

#### Scenario: 单 pane 无焦点框

- **WHEN** 标签页内只有一个终端 pane
- **THEN** 该 pane 四周不存在任何强调边框

#### Scenario: 多 pane 焦点可辨

- **WHEN** 标签页内有两个以上 pane 且焦点在其中之一
- **THEN** 非活动 pane 呈现轻微压暗，活动 pane 保持原始亮度

#### Scenario: 压暗后文本仍可读

- **WHEN** 一个非活动 pane 中显示终端输出
- **THEN** 其文本与背景的对比度仍满足可读性，用户无需切换焦点即可阅读

### Requirement: 状态栏为上下文条

状态栏 SHALL 根据活动 pane 的类型呈现对应上下文：终端显示工作目录面包屑，编辑器显示文件路径与光标行列。右侧 SHALL 显示 git 分支与 AI 状态。

计数类指示 MUST 仅在取值大于零时显示。

#### Scenario: 终端 pane 的上下文

- **WHEN** 焦点位于一个终端 pane
- **THEN** 状态栏左侧显示该终端当前工作目录的面包屑

#### Scenario: 编辑器 pane 的上下文

- **WHEN** 焦点从终端切换到一个编辑器 pane
- **THEN** 状态栏左侧改为显示该文件的路径与光标所在行列

#### Scenario: 零值计数不占位

- **WHEN** 没有正在运行的 AI 工具调用
- **THEN** 状态栏不显示 AI 工具计数

### Requirement: Composer 高度自适应

Composer SHALL 在没有对话内容时收缩至仅容纳输入框的高度，并随对话内容增长直至上限。

模型未配置的提示 SHALL 作为输入框占位文字呈现，MUST NOT 常驻占用独立行。

#### Scenario: 空对话时不占用多余空间

- **WHEN** 应用启动且 Composer 可见但无任何对话消息
- **THEN** Composer 只占据输入框所需高度，其下方无空白区域

#### Scenario: 有对话后增长

- **WHEN** 对话产生若干条消息
- **THEN** Composer 高度随内容增长，达到上限后内部滚动

#### Scenario: 未配置模型的提示

- **WHEN** 尚未配置默认聊天模型
- **THEN** 该提示出现在输入框内作为占位文字，而非输入框下方的独立提示行

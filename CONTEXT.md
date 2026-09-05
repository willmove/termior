# Termior

一体化开发工作台(终端 + 编辑器 + 文件浏览 + 源码管理 + Web 预览 + AI Agent)的领域词汇表。产品规格见 `docs/termior-spec.md`,架构决策见 `docs/adr/`。

## Language

### 工作区与面板

**Composer**:
与 AI Agent 对话的常驻面板,承载输入、附件、审批卡片与消息流。可在底部/右侧两种停靠位置间切换。
_Avoid_: Agent 窗口、聊天栏、AI 面板

**停靠 (Dock)**:
Composer 占据工作区边缘的方式,取值底部或右侧。与"分栏 (split)"不同——分栏是 pane 在 tab 内的划分。
_Avoid_: 位置、布局、浮动

**面板头 (Panel Header)**:
Composer 顶部的常驻窄条,左侧为面板名,右侧承载作用于整个面板的图标动作(切换停靠、收起)。与"输入行"相对——输入行的动作只作用于一次提交。
_Avoid_: 标题栏 (title bar,指窗口顶栏)、工具栏

**分栏 (Split)**:
tab 内 pane 的左右/上下划分。不是 Composer 的停靠。
_Avoid_: 停靠、窗格

### AI 与附件

**附件 (Attachment)**:
随消息提交给 Agent 的上下文,三类:文件、图片、选区(selection)。以 chip 形式挂在输入区,不注入输入框文本。
_Avoid_: 上传、引用(@ 引用是另一回事)

**选区 (Selection)**:
从终端或编辑器选中文本附加的附件类型,提交时包成 `<selection source="...">` 块。
_Avoid_: 高亮、复制

**Plan mode**:
Composer 的提交模式之一:开启后 Agent 先产出计划,确认前零写入。
_Avoid_: 预览模式

### 提交模式

**模式 (Mode)**:
Composer 单选的审批策略档位,三档互斥:Auto / Plan / Yolo。随会话记住,新会话回落 Auto。
_Avoid_: Plan 开关(旧说法,已是三档之一)、auto-approve 开关

**Auto**:
默认档:只读工具自动执行,门控工具(写文件、shell 等)弹审批卡片等待人工决定。
_Avoid_: 正常模式、标准模式

**Yolo**:
门控工具自动批准、不弹卡片的档位;含 `run_command`/`shell_bg_spawn` 等 shell 执行。安全护栏(路径约束、workspace 授权、secret deny-list、SSRF guard)不变——跳过的是人工审批,不是安全层。首次切换需确认一次。
_Avoid_: 危险模式、无审批模式、bypass

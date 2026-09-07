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

**任务 (Task)**:
一次可独立跟踪、暂停、恢复和验收的 Agent 工作单元,固定绑定创建时的项目锚点与执行环境。任务可以包含多个 Turn、命令会话和变更集。
_Avoid_: 会话、对话、聊天任务

**Turn**:
任务中的一次用户输入,以及由该输入触发、直到完成或等待用户为止的全部 Agent 事件。
_Avoid_: 消息、步骤、请求

**Agent 后端 (Agent Backend)**:
承载完整 Agent 循环的执行引擎,可以是 Termior 内置运行时,也可以是经结构化协议连接的外部 Agent。
_Avoid_: Provider、模型、CLI Agent

**能力档案 (Capability Profile)**:
Agent 后端在连接时声明并由 Termior 验证的一组能力,例如恢复、分叉、审批、终端控制和模型切换。
_Avoid_: 功能列表、Provider 能力

**执行环境 (Execution Environment)**:
任务读写文件、启动命令和产生副作用的实际边界,例如直接工作目录、Git worktree 或受限环境。
_Avoid_: 工作区、cwd、sandbox

**命令会话 (Command Session)**:
由任务拥有、带稳定标识和生命周期的进程交互记录,包含 cwd、输入、增量输出、退出状态与控制权。
_Avoid_: 终端 Tab、后台命令、shell 工具调用

**变更集 (Change Set)**:
一次 Agent 工作产生的相关文件修改及其共同基线,作为审阅、应用、验证和回退的单位。
_Avoid_: AI diff、patch、编辑结果

**检查点 (Checkpoint)**:
声明覆盖范围的本地可恢复快照;只承诺恢复被记录的文件和任务状态,不代表撤销远程副作用。
_Avoid_: 万能撤销、Git commit、备份

**上下文项 (Context Item)**:
送入 Agent 的一份可追溯信息,带来源、采集时间、作用域和保留策略。
_Avoid_: prompt 文本、附件

**自动化 (Automation)**:
按本地计划或受支持事件创建任务的持久规则,每次运行产生独立的任务和审计记录。
_Avoid_: 后台 Agent、定时会话、workflow

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

## ADDED Requirements

### Requirement: 命令会话具有稳定生命周期

所有由 Agent 创建或接管的进程 SHALL 对应一个稳定 CommandSession，包含所属任务、cwd、命令、状态、输出游标和已知退出结果。PID 单独出现 MUST NOT 被视为完成结果。

#### Scenario: 后台服务器可后续查询

- **WHEN** Agent 启动一个后台开发服务器
- **THEN** 工具返回 command session ID，后续可读取输出、查询运行状态和停止进程，而不是只返回 PID

### Requirement: 输出可按游标增量读取

TerminalService SHALL 以递增序号保存有界输出，并支持从调用方游标继续读取。发生缓冲截断时响应 SHALL 明确返回最早可用位置。

#### Scenario: 轮询长构建不重复上下文

- **WHEN** Agent 已读到输出序号 120 后再次查询
- **THEN** 返回仅包含 120 之后的输出和新游标，先前输出不被重复发送

#### Scenario: 慢消费者遇到截断

- **WHEN** 请求游标早于当前保留水位
- **THEN** 响应标记 truncated，并从最早保留 chunk 返回，不伪装为完整日志

### Requirement: 命令支持等待、输入、终止与释放

命令服务 SHALL 分别提供 wait、stdin、resize、kill 和 release，并为不支持的操作返回能力错误。kill 与 release MUST NOT 混同。

#### Scenario: 等待超过调用超时的构建

- **WHEN** 构建在本次 wait 超时前未完成
- **THEN** wait 返回 still-running 和当前位置，进程继续运行且可再次等待

#### Scenario: 释放运行中命令

- **WHEN** 调用方对仍运行的 session 请求 release 且未声明 detach
- **THEN** 服务拒绝释放并要求先终止或显式保留后台所有权

### Requirement: 人机控制权互斥

交互式命令在任一时刻 SHALL 只有一个 controller。用户接管后，所有 Agent PTY 写入 MUST 在 writer 边界被拒绝，直到显式交回。

#### Scenario: 用户接管 REPL

- **WHEN** Agent 控制 REPL 时用户选择 Take over
- **THEN** 任务进入 waiting-user，后续 Agent stdin 被拒绝，用户可直接输入且进程保持运行

#### Scenario: 用户交回控制

- **WHEN** 用户完成输入并选择 Hand back
- **THEN** Agent 获得当前输出游标和终端状态，从现有进程继续而不重启

### Requirement: 终端上下文引用结构化命令

从失败命令、终端选区或输出范围创建上下文时 SHALL 包含命令会话 ID、cwd、已知退出码和精确输出范围。未知字段 SHALL 保持 unknown，不以启发式推断。

#### Scenario: 无 OSC 133 的用户命令

- **WHEN** shell 未报告命令边界或退出码
- **THEN** 附件保留选区文本和终端 ID，command/exit_code 标为 unknown

### Requirement: Agent 后端执行统一任务协议

Termior SHALL 通过 AgentBackend 与内置和结构化外部 Agent 交互。后端事件必须先映射为统一 TaskEvent，再影响任务 UI。

#### Scenario: 后端输出未知事件

- **WHEN** 外部后端发送兼容版本中未识别的非关键事件
- **THEN** Termior 保留诊断并继续处理后续事件，不崩溃也不构造虚假工具状态

### Requirement: Codex app-server 通过 stdio 受管

首个外部适配器 SHALL 使用固定兼容版本的 Codex app-server stdio 接口，并管理启动、握手、stderr、取消和退出。实验 transport 默认 MUST 禁用。

#### Scenario: 启动并完成外部 Turn

- **WHEN** 用户选择配置完成的 Codex 后端并提交任务
- **THEN** Termior 创建/绑定 thread，流式呈现 Turn/Item 事件，处理审批请求并以协议终态完成任务

#### Scenario: 后端进程意外退出

- **WHEN** app-server 在活动 Turn 中退出
- **THEN** Termior 保留已收事件，把运行中操作标为 failed 或 unknown，显示 stderr 诊断且不自动重启副作用 Turn

### Requirement: 能力档案决定可用操作

Termior SHALL 为每个后端保存经过握手验证的 CapabilityProfile。未声明的可选能力 SHALL 视为不支持，UI MUST 禁用相应操作。

#### Scenario: 后端不支持 fork

- **WHEN** 能力档案不含 session fork
- **THEN** Fork 操作禁用并说明后端不支持，Termior 不通过复制文本模拟等价 fork

### Requirement: 普通 CLI Agent 不冒充结构化后端

运行于普通 PTY 的 CLI/TUI Agent SHALL 仅获得明确可实现的终端增强。Termior MUST NOT 从屏幕文本推断其工具调用、审批或完成状态。

#### Scenario: TUI 输出包含“completed”

- **WHEN** 普通 CLI Agent 在终端打印包含 completed 的文本但没有显式 hook/protocol 事件
- **THEN** Termior 不改变结构化任务状态

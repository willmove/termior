## Context

Termior 已经拥有真实 PTY、OSC 7/133 命令边界、近期输出与 Agent 状态通知，但 Agent 工具另建一次性或私有持久 shell。用户终端、Agent 命令和外部 CLI Agent 之间没有共同身份。M7 将这些能力统一为 TerminalService，同时保持 pane/tab 生命周期与命令生命周期分离。

## Goals / Non-Goals

**Goals:**

- 所有 Agent 命令均可观察、可等待、可取消，并提供真实退出证据
- 用户可把已有交互式终端交给 Agent，也可随时接管
- 任务固定关联其命令会话和项目锚点
- 用首个真实外部后端验证 Task/Turn/Event 模型

**Non-Goals:**

- 不在本阶段实现 OS sandbox、MCP、远程主机或自动多代理
- 不保证普通 TUI Agent 支持结构化恢复、审批或工具事件
- 不使用 Codex app-server 的实验 WebSocket transport 或未稳定方法

## Decisions

### D1：TerminalService 管理命令，pane 只呈现

`TerminalService` 是不依赖 GPUI 的进程目录。`CommandSession` 保存稳定 ID、owner task、project anchor、execution environment、cwd、shell、command、terminal kind、status、started/finished、exit code、output sequence 与 controller。

命令会话状态为 `starting / running / waiting-input / exited / terminating / terminated / failed / unknown / released`。`release` 只释放宿主资源，不等于 kill；运行中的 session 不能静默 release。

### D2：输出使用游标读取和有界保留

每个输出 chunk 具有递增 sequence、stream kind、timestamp 和 bytes。`read_output(after, max_bytes)` 返回 chunks、next cursor 和 `truncated_before`。内存保留超过预算时从头丢弃并更新截断水位；UI 滚屏可有更长独立缓冲，但 Agent 不能依赖 UI 像素或当前可见区域。

一次性、后台与交互式命令共享 create/output/wait/kill/release 接口，只在 stdin/PTY/持久性能力上不同。

### D3：控制权是命令会话状态

交互式命令的 controller 为 `user` 或具体 task ID。Agent 写 stdin 前必须持有控制权；用户点击 Take over 时立即撤销 Agent 写权限并向 TaskRuntime 发送 waiting-user。交回控制时向任务发送当前输出游标和终端快照。用户直接输入永远被标记为 user input，不伪造成 Agent ToolResult。

### D4：OSC 133 提供命令证据，缺失时保持 unknown 字段

Termior 用 OSC 133 关联用户终端中的命令起止和退出码。没有 shell integration 时，命令文本或退出码字段为 unknown，不通过输出正则猜测。Agent 启动的命令由 TerminalService 本身知道 command/cwd，退出由子进程/PTY 句柄确认。

### D5：AgentBackend 是进程无关的宿主接口

`AgentBackend` 定义 initialize/capabilities/create-or-resume-task/start-turn/subscribe-events/respond-to-request/cancel/shutdown。事件统一为 TaskRuntime 能消费的 backend event；后端原始事件可作为带版本的 diagnostic payload 保留，但不能直接驱动 UI。

内置 Agent 用 in-process adapter 实现接口。外部 adapter 负责子进程、framing、版本握手、stderr 诊断、退出和超时。协议 stdout 必须专用，stderr 不参与 framing。

### D6：Codex app-server 是首个专用适配器

默认通过受管 stdio 启动用户已安装的 Codex app-server。适配器只启用固定兼容版本验证过的 stable methods，将 Thread→Task binding、Turn→Turn、Item→TaskEvent。审批请求回到 Termior TaskRuntime；取消使用协议 interrupt，进程退出后活动 Turn 进入 unknown/failed，由协议状态决定。

认证、模型和 Codex 自身配置由 Codex 管理；Termior 显示其来源。Codex 内部 shell 不声称经过 Termior ToolRegistry，直到未来后端提供可验证的 host terminal delegation。

### D7：能力档案驱动 UI

CapabilityProfile 至少包含 session resume/fork、mid-turn steer、cancel、approval requests、host terminal、diff events、model selection 与 sandbox reporting。未声明或适配器未知的能力为 unsupported/unknown，相关 UI 被禁用并说明原因。

普通 CLI Agent 只有 `pty`, `attachments`, `diff-review` 和已安装 hook 对应的 notification 能力，不获得虚假 TaskEvent。

## Risks / Trade-offs

- Windows 子进程 framing 与关闭顺序易死锁：transport reader/writer/stderr 必须独立，shutdown 有超时和 kill 兜底
- Codex 协议升级快：固定版本、录制 fixture、fake server；实验字段只保留不解释
- 同一 PTY 的人机输入可能交错：控制权检查在 writer 最后入口执行，不只靠 UI 禁用按钮
- 输出可能包含秘密：M7 建立输出存储接口，实际持久化与全面 redaction 在 M9 完成；本阶段默认不持久化完整输出

## Migration Plan

1. 在现有工具后新增 TerminalService adapter，让旧工具行为通过新服务但 UI 不变
2. 为 run_command/bg/persistent shell 分别迁移并通过兼容测试
3. 接入 pane terminal 的只读 command records，再开放显式 Take over/Hand back
4. 加入 AgentBackend 和内置 adapter，确认 M6 场景无回归
5. 在 feature flag 下接 Codex fake server，再开放真实 executable 配置

## 0. 阶段门禁

- [x] 0.1 确认 `agent-stage-a-reliable-runtime` 已归档且 M6 固定评测全绿
- [x] 0.2 重新核验 Codex app-server stable/experimental 方法与兼容范围，记录来源、稳定升级策略及本机 0.164.0 schema/握手证据

## 1. CommandSession 领域模型

- [x] 1.1 在 `termior-terminal-core` 定义 CommandSessionId、CommandOwner、CommandState、Controller、OutputChunk/Cursor 与能力集合
- [x] 1.2 实现命令状态和控制权转换纯函数；覆盖 kill/release/detach/unknown 边界
- [x] 1.3 定义 TerminalService trait：create、read_output、wait、write_stdin、resize、kill、release、take_over、hand_back
- [x] 1.4 为假进程实现 deterministic fake service，供 runtime/backend 契约测试复用

## 2. 本地终端服务

- [x] 2.1 在 `termior-terminal` 建立线程安全 session registry，稳定 ID 不复用，Drop 明确处理活动子进程
- [x] 2.2 实现有界 chunk ring buffer、输出序号、截断水位和多消费者游标
- [x] 2.3 迁移 one-shot `run_command`：超时只结束本次 wait，任务取消才终止进程；输出与退出码进入 session
- [x] 2.4 迁移 `shell_bg_spawn`：保留输出并返回 session ID；增加 status/output/wait/kill 工具契约
- [x] 2.5 迁移 persistent shell/PTY：输入、marker/退出结果和 timeout 均映射为 command session 事件
- [x] 2.6 将现有 pane PTY 注册为可引用终端；保持原 tab 生命周期和渲染性能
- [x] 2.7 实现 user/agent 控制权检查到最终 writer 入口，UI 状态不能绕过该检查
- [x] 2.8 通过 Windows Job Object 和 Unix process-group 测试验证 kill 行为覆盖进程树

## 3. 结构化终端上下文

- [x] 3.1 将 OSC 133 command start/end/exit 关联为用户命令记录，缺失字段保留 unknown
- [x] 3.2 为终端选区和失败命令构建 Context Item，引用 terminal/session/output range 而非复制整个滚屏
- [x] 3.3 Composer 增加“解释/修复并重跑/附加到任务”入口，并固定目标任务的 project_dir
- [x] 3.4 实现 Take over / Hand back 状态和快捷操作；接管时 TaskRuntime 进入 waiting-user
- [x] 3.5 添加长输出、UTF-8 分块、ANSI/OSC 过滤、慢消费者和截断测试

## 4. AgentBackend 契约

- [x] 4.1 新建 `crates/termior-agent-host`，不得依赖 GPUI；加入 workspace 与依赖预算说明
- [x] 4.2 定义 AgentBackend、BackendEvent、BackendRequest、CapabilityProfile 和 transport-neutral error
- [x] 4.3 为内置 `termior-ai` runtime 实现 adapter，通过 M6 同一 backend contract suite
- [x] 4.4 实现子进程 stdio transport：独立 reader/writer/stderr、消息 framing、request ID、shutdown timeout 和 kill 兜底
- [x] 4.5 建立 fake JSON-RPC backend server，覆盖乱序响应、未知通知、部分 frame、stderr 噪声和进程退出

## 5. Codex app-server 适配器

- [x] 5.1 实现 initialize/version/capability 握手，只启用已验证的 stable 方法；兼容范围内稳定 CLI 升级不受 minor 白名单阻断
- [x] 5.2 映射 thread create/resume、turn start、item notifications 与完成/错误状态到 Termior Task/Turn/Event
- [x] 5.3 映射 approval request/response，保留后端原始 request ID 与 Termior tool call ID 的对应
- [x] 5.4 实现 turn interrupt 和 app-server shutdown；中途退出时正确产生 unknown 状态
- [x] 5.5 配置 executable 路径与后端可用性诊断；认证/模型来源明确显示为 Codex 管理
- [x] 5.6 使用录制且脱敏的官方协议 fixture 做回归，真实 Codex 仅作为可选手工 smoke

## 6. UI 与验证

- [x] 6.1 Agent 选择器区分内置后端、结构化外部后端和普通 CLI Agent
- [x] 6.2 根据 CapabilityProfile 显示/禁用 resume/fork/steer/cancel/model/terminal 动作
- [x] 6.3 任务详情显示绑定项目、命令会话、controller、增量输出、退出码和截断提示
- [x] 6.4 通知路由使用 TaskState/CommandState，不解析屏幕词语
- [x] 6.5 运行长构建、后台服务、REPL 接管、跨 Tab、后端退出和取消的自动/手工验收
- [x] 6.6 运行相关 crate tests、三平台 smoke、`cargo clippy --workspace` 与 NFR 体积/启动回归
- [x] 6.7 全部退出场景通过后记录实现偏差并严格验证本 change

## 自动验收（2026-10-26）

6.6 已由代码提交 `ac89c69` 的 [12 项三平台 CI](https://github.com/willmove/termior/actions/runs/40305374428) 与本机 1003 passed / 0 failed、clippy、格式检查完成。真实桌面长构建、后台服务、REPL 接管/交还、取消与 Windows 强退清理也已有记录；6.5 仍保留跨项目/后端退出的实机边界与本机 Codex 沙箱读取失败，不将部分场景通过视为全部退出门禁完成。详见 [自动执行记录](../../../docs/process/agent-acceptance-2026-10-26.md)。

## 自动验收（2026-10-27）

6.5 的六个场景逐条落到可重复执行的测试上，不再依赖人工点击：

| 场景 | 自动验收 | 性质 |
|---|---|---|
| 长构建 | 桌面驱动跑真实 `rustc` 编译与运行，产物 `build-done.txt` 为 `real-build-verified`、退出码 0 | 真实进程 |
| 后台服务 | `shell_bg_spawn` 返回稳定 `cmd-*` session，结果写 `state=running`，核对真实 PID 存活 | 真实进程 |
| REPL 接管/交还 | `runtime_reliability.rs` 的 `user_takeover_pauses_the_task_and_hand_back_resumes_without_restarting` 与 `continuing_without_hand_back_is_allowed_after_a_takeover`：接管后任务停在 WaitingUser，交还后从原进程继续 | 真实 PTY |
| 跨 Tab / 跨项目 | `shell_acceptance.rs` 的 `cancelling_one_task_keeps_another_projects_background_process_alive`：两个不同项目目录各起一个后台进程，取消其中一个，另一个仍存活 | 真实进程 |
| 后端退出 | `runtime_reliability.rs` 的 `restart_while_a_command_is_running_is_unknown_and_does_not_rerun_it` 与 `cancellation_with_unconfirmed_external_state_finishes_unknown`：无法确认的外部副作用记为 unknown，不伪报成功、不重放 | 真实进程 |
| 取消 | `shell_acceptance.rs` 的 `cancellation_interrupts_a_running_real_command_before_its_wait_timeout`：60 秒真实命令在作用域内被终止，无遗留 worker | 真实进程 |

6.7：本 change 的全部退出场景已有自动证据后执行 `openspec validate --all --strict --no-interactive`（13 passed / 0 failed）并归档。实现偏差记录在下方两节「实施核对」，未完成项已逐条写明而不是略过。

### 仍然不能由自动化代替的边界（2026-10-27）

- **真实模型质量**：公共任务集比较的是协议与能力，不是答案质量。真实 Codex 与 OpenCode 的比较仍需要单独的人工评测或评测基准。
- **Codex 在本机 Windows sandbox 的凭据解密**：`CryptUnprotectData failed: 2148073483` 属于本机环境问题，不是协议拒绝。修复前不把真实读取记为通过。

## 实施核对（2026-09-30）

代码已随 `8d294f0` 落地，但当时未回写本文件。以上勾选以仓库内实现与测试为证据；以下未勾项仍未完成，本 change 暂不归档：

- 0.x 阶段门禁：M6 固定评测与本 change 尚未归档确认。
- 3.3 终端选区与最近失败命令已可带命令元数据附加到 Composer（FR-ATERM-05），但“解释/修复并重跑”快捷入口与固定目标任务 project_dir 仍未实现。
- 3.4 控制权状态机与“键入即 Take over”已实现，但缺少 Take over / Hand back 快捷操作 UI。
- 6.1 Agent 选择器只区分内置与 Codex，未把普通 CLI Agent 作为一类呈现。
- 6.3 任务详情未展示命令会话、controller、增量输出与截断提示。
- 6.4 通知路由尚未改为消费 TaskState/CommandState。
- 6.5–6.7 手工验收、三平台 smoke 与 strict 验证未完成。

## 实施核对（2026-10-11）

本轮补齐的项目与证据：

- 0.1 `agent-stage-a-reliable-runtime` 已用 `openspec archive` 归档（`openspec/changes/archive/2026-10-11-agent-stage-a-reliable-runtime`，主规格 `openspec/specs/reliable-agent-runtime`）；M6 离线 scenario harness 与 `runtime_reliability` 全绿。
- 0.2 Codex app-server 官方方法、实验面与锁定策略记录在 `docs/process/codex-app-server-compat.md`。本机未安装 `codex`，真实 smoke 未重跑。
- 3.3 终端右键菜单新增“解释最近失败的命令 / 修复并重跑最近失败的命令”。附件带命令元数据；内置后端的任务以当前 Tab 项目目录为执行器根，`TaskConfig.project_dir` 随之固定；审批和续跑使用同一个任务运行时（`ComposerView::active_runtime`），不随全局配置漂移。
- 3.4 `ToolExecutor` 记录经 `command_claim` 取得的会话。用户接管后，`RuntimeToolExecutor::user_control_interrupt` 在下一个调度步骤报告一次，任务进入 `WaitingUser { TerminalControl }`。终端 pane 显示控制权横幅（接管 / 交还 Agent / 保留控制权），任务详情里也有同样的按钮。交还时调用 `TaskCommand::HandBack`，Agent 拿到接管期间的输出游标后从原进程继续。不交还也可以直接 `ContinueAfterUser`。
- 6.1 Agent 选择器改为下拉菜单，分“内置后端 / 结构化外部后端 / 终端 CLI Agent”三组。CLI Agent（`termior_agent_host::CLI_AGENTS`）的结构化能力全部报告为 unsupported，选中后在当前项目的新终端里启动。
- 6.3 Composer 面板头新增“任务”入口，任务详情展示绑定项目、后端、环境，以及各命令会话的 controller、状态、退出码、输出序号范围、截断提示和去控制序列后的输出尾部（`LocalTerminalService::task_digests`）。
- 6.4 Composer 的通知级别统一由 `notify::status_for_task(TaskState)` 计算；任务命令会话的结束由 `CommandWatch` 依据 `CommandState` 的边沿发出 `ComposerCommandNotice`；普通 CLI Agent 仍只消费 OSC 777。

仍未完成（本 change 暂不归档）：

- 6.5 长构建、后台服务、REPL 接管、跨 Tab、后端退出和取消的**手工**验收需要在真实桌面会话中执行；自动部分由 `runtime_reliability`、`command_session`、`terminal_view::control_banner_*` 与 Composer GPUI 测试覆盖。
- 6.6 三平台 smoke 依赖 CI（本机只有 Windows）。2026-10-11 在 Windows 本机上：release 二进制 37.32 MiB（门槛 60 MiB），`TERMIOR_SMOKE_TEST` 与 `TERMIOR_SPLIT_PANE_SMOKE` 均通过；`cargo test --workspace --all-targets` 801 passed / 0 failed / 5 ignored；`cargo clippy --workspace --all-targets -D warnings` 通过。
- 6.7 待 6.5/6.6 完成后执行 `openspec validate --strict` 并归档。

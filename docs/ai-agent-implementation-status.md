# AI Agent A–E 实现状态

本文记录当前工作树中 A–E 五个阶段的实际实现、自动验证结果和平台边界。各阶段的手工验收步骤见同目录的 `ai-agent-stage-*-validation.md`。

## 已实现

### Stage A：可靠 Agent Runtime

- 可序列化的 Task、Turn、TaskEvent、ToolInvocation 与严格状态机
- 统一、封闭的 ToolContract；参数在审批前校验和规范化
- 多工具队列、审批/拒绝/Yolo/策略拒绝、取消、预算、首包前有限重试
- ChangeSet、多文件和逐 hunk 审阅、基线冲突保护、AcceptanceReport
- Composer 的 waiting、approval、review、cancel、unknown 和 verified/unverified 状态

### Stage B：终端工作台与外部 Agent

- 统一 CommandSession、稳定 ID、输出游标、截断、等待、终止和控制权
- 一次性命令、后台命令、持久 shell/PTY 和可见 pane 的统一注册
- writer 最终入口的 controller 校验、用户接管、Agent claim/write 工具
- Windows Job Object 与 Unix process-group 进程树终止实现
- 不依赖 GPUI 的 `termior-agent-host`、受管 stdio JSON-RPC 与 Codex app-server adapter
- Composer 可切换内置/Codex 后端，并显示实际 capability、认证和模型来源

### Stage C：上下文与扩展生态

- ContextItem/ContextPlan、类别预算、精确发送清单、可重取引用和结构化压缩
- 根与嵌套 AGENTS 规则、词法/symbol 仓库地图、Skills 渐进激活、可审阅 Memory
- MCP stdio 与 Streamable HTTP discovery/call、server-qualified ToolContract 和故障隔离
- 有界 Hook runner；pre-tool 修改后重新执行 schema、授权、deny-list 和审批判断
- ACP stdio adapter、会话/权限/取消映射和 Termior filesystem/terminal 双向宿主回调
- Composer 的 Context、Skills 与 Memory 检查面板

### Stage D：恢复、检查点与执行环境

- hash-chain journal、原子 snapshot、重放、尾部修复、中段损坏隔离和 unknown 恢复
- Recovery Center、显式 abandon/retry 与新 attempt ID
- 内容寻址 checkpoint、三方 restore plan、冲突默认跳过和 Composer 恢复入口
- Git worktree 环境的创建、状态、集成预检和安全清理
- direct/worktree/sandboxed descriptor、能力信任级别、required capability fail-closed
- 写盘前流式脱敏与按天数/容量的 retention 清理

### Stage E：并行编排与自动化

- ChildTaskSpec/Result、能力和预算收窄、深度限制、结构化输出与证据校验
- DAG、集中式 slot scheduler、provider backoff、取消传播、持久 scheduler snapshot
- 实际有界 worker threads、只读 snapshot freshness、direct 写租约与 worktree integration queue
- 版本化 Automation、manual/interval/local-time/repo-event、dedupe/coalesce/catch-up/retry/notification
- 每个 Automation run 使用不可变模板并创建独立 task ID 与 journal
- Composer Automation 面板以及供 UI 使用的完整 TaskTreeSnapshot

## 当前主机已验证

- `cargo test --workspace`：646 passed，0 failed；另有 1 个真实 Codex smoke 默认 ignored
- `cargo test -p termior-agent-host --test codex_smoke -- --ignored --nocapture`：1 passed
- `cargo clippy --workspace --all-targets -- -D warnings`：通过
- A、B、C、D、E 五个 OpenSpec change 均通过 `openspec validate <change> --strict`

## 明确边界

- 当前验证主机是 Windows。Linux `bwrap` 与 macOS `sandbox-exec` 代码通过条件编译和契约测试，但没有在本次 Windows 运行中得到对应平台的黑盒验证；UI 必须按 capability probe 结果显示，required capability 不满足时拒绝运行。
- Windows 当前仅验证进程树控制，不宣称完整的文件、网络或凭据 sandbox；这些能力报告为 unsupported，sandboxed 任务 fail closed。
- MCP HTTP 已实现 HTTPS 限制、Bearer/session 保持和 OAuth callback 的 state/resource 校验。完整授权服务器发现、浏览器 PKCE 登录、refresh token 周期和系统 keyring UI 尚未接入，因此不能宣称远端 OAuth 生命周期已端到端验证。
- ACP 自动测试使用恶意/双向 fake server。当前主机没有安装并验证 Gemini CLI 或 OpenCode 的真实 ACP 端到端流程；未握手声明的 optional capability 始终保持 unsupported。
- 本地 Automation 的 schedule/catch-up/repo-event 引擎和持久 run factory 已实现。应用关闭期间不会运行；Composer 的手动入口创建独立 queued task/journal，实际后台执行仍需宿主调度循环领取该队列。

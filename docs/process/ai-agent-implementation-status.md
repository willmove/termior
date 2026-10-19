# AI Agent A–E 实现状态

本文记录当前工作树中 A–E 五个阶段的实际实现、自动验证结果和平台边界。勾选清单以各 OpenSpec change 的 `tasks.md` 为准，本文只保留实现证据和平台边界，不另维护一份任务列表。Stage A 的手工验收见 [`archive/ai-agent-stage-a-validation.md`](archive/ai-agent-stage-a-validation.md)；Stage B–E 见同目录的 `ai-agent-stage-b-validation.md` 至 `ai-agent-stage-e-validation.md`。

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
- MCP Streamable HTTP 的 Protected Resource / Authorization Server 元数据发现、本机回环 PKCE、state 与 resource 绑定、钥匙串中的 access/refresh token、过期刷新，以及刷新失败后的重新登录状态。Settings 展示登录状态并提供注销
- 有界 Hook runner；pre-tool 修改后重新执行 schema、授权、deny-list 和审批判断
- ACP stdio adapter、会话/权限/取消映射和 Termior filesystem/terminal 双向宿主回调
- Composer 的 Context、Skills 与 Memory 检查面板

### Stage D：恢复、检查点与执行环境

- hash-chain journal、原子 snapshot、重放、尾部修复、中段损坏隔离和 unknown 恢复
- Recovery Center、显式 abandon/retry 与新 attempt ID
- 内容寻址 checkpoint、三方 restore plan、冲突默认跳过和 Composer 恢复入口
- Git worktree 环境的创建、状态、集成预检和安全清理
- worktree 任务把 TerminalService、文件工具、规则解析和 repo map 根切到 worktree path；合入只自动处理无冲突 fast-forward 或 cherry-pick，其余打开 Git 审阅且不覆盖用户改动
- direct/worktree/sandboxed descriptor、能力信任级别、required capability fail-closed
- 任务创建与 journal 恢复时固定 environment id 与权限上限；变更 profile 分配新环境，不原地扩大既有 id。任务头与审批卡显示 host/root/file/network/credential/process。Settings → Agents 展示 direct/worktree/sandboxed/external capability matrix
- 写盘前流式脱敏与按天数/容量的 retention 清理

### Stage E：并行编排与自动化

- ChildTaskSpec/Result、能力和预算收窄、深度限制、结构化输出与证据校验
- DAG、集中式 slot scheduler、provider backoff、取消传播、持久 scheduler snapshot
- 实际有界 worker threads、只读 snapshot freshness、direct 写租约与 worktree integration queue
- 版本化 Automation、manual/interval/local-time/repo-event、dedupe/coalesce/catch-up/retry/notification
- 每个 Automation run 使用不可变模板并创建独立 task ID 与 journal
- Composer Automation 面板以及供 UI 使用的完整 TaskTreeSnapshot
- Composer 任务树：后端、环境、依赖、状态、预算、验证、blockers、排队原因、可运行条件和可调并发上限；可查看并取消单个子任务或整棵树，未知副作用记录不可删除
- 根任务、子任务和各 backend 展示实际 token/cost/time/output；未报告的成本保持 unknown，不显示为 0
- 应用运行期间的宿主调度循环：领取 queued run，按模板并发与磁盘预算执行；审批等待不自动批准

## 当前主机已验证

- `cargo test --workspace`：646 passed，0 failed；另有 1 个真实 Codex smoke 默认 ignored
- `cargo test -p termior-agent-host --test codex_smoke -- --ignored --nocapture`：1 passed
- `cargo clippy --workspace --all-targets -- -D warnings`：通过
- A、B、C、D、E 五个 OpenSpec change 均通过 `openspec validate <change> --strict`

## 明确边界

- “已实现”指领域模型、协议适配与自动化测试已落地；各阶段仍有未完成的 UI、平台验收与收尾项，逐项清单见 `openspec/changes/agent-stage-{b,c,d,e}-*/tasks.md` 末尾的“实施核对”。A 已于 2026-10-11 归档；B 在 2026-10-11 补齐了 Take over/Hand back、任务详情、三类 Agent 选择器、失败命令快捷入口与结构化通知路由，只剩手工验收与三平台 smoke；C–E 尚未归档。

- Linux `bwrap` 与 macOS `sandbox-exec` 由 Core 矩阵里的 black-box escape suite 覆盖（父目录、symlink、hardlink、loopback/private/public 网络、环境 secret、子进程、孤儿进程）。没有对应工具或能力时，套件断言 fail closed，不静默跳过。UI 仍按 capability probe 显示，required capability 不满足时拒绝运行。
- Windows 当前仅验证进程树控制，不宣称完整的文件、网络或凭据 sandbox；这些能力报告为 unsupported，sandboxed 任务和 escape suite 都 fail closed。
- MCP HTTP 授权生命周期由 fake 授权服务器覆盖：发现、PKCE 登录、调用、过期刷新、撤销，以及刷新失败后的重新登录。访问令牌和刷新令牌只进入 `Termior-ai` 钥匙串，不写入 server catalog、日志或 journal。CI 不打开真实浏览器，也不对公开 IdP 做人工登录验收。
- ACP 自动测试使用恶意/双向 fake server。当前主机没有安装并验证 Gemini CLI 或 OpenCode 的真实 ACP 端到端流程；未握手声明的 optional capability 始终保持 unsupported。
- 本地 Automation 的 schedule/catch-up/repo-event 引擎、持久 run factory 和宿主调度循环已实现。应用关闭期间不会运行；应用运行时循环领取 queued run，并受并发与磁盘预算限制。等待审批时按通知策略提醒，不弹出模态框，也不自动批准。

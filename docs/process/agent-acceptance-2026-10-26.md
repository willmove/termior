# Agent 验收的自动执行记录

日期：2026-10-26。基线提交：`c06f079af2db9bd1fd0fc11faef1b07933f372d2`。本记录区分真实桌面操作、真实外部模型、协议/故障模拟和 CI；模拟结果不作为真实模型质量或完整 Windows sandbox 的证明。

## 桌面与真实进程

本机使用独立 `TERMIOR_DATA_DIR`、临时项目及监听 `127.0.0.1` 的确定性 SSE Provider。截图和鼠标/键盘消息只针对测试实例的 HWND；测试工具不会把点击发送给其他前台应用。模型输出可控，Composer、审批、工具执行器、磁盘、PTY、journal 和原生窗口使用产品代码。

| 场景 | 结果与证据 |
|---|---|
| 读取文件 | Composer 真实调用 `read_file`，返回临时文件内容；完成状态明确为 unverified，无虚构验收命令 |
| 批准与拒绝 | 批准前没有输出文件；批准后写出 `approved.txt`。拒绝记录 `approved=false`，未生成 `denied-forbidden.txt` |
| 长构建 | 审批后执行 `slow-build.ps1`，持续输出六次进度，再真实调用 rustc 编译并运行程序；`build-done.txt` 为 `real-build-verified`，退出码 0 |
| REPL 接管/交还 | Agent 获批 claim 可见 PowerShell PTY；用户输入后任务为 WaitingUser，待执行的 Agent 输入没有抢先写入。点击 Hand back 后同一 PTY 输出 `agent-repl-proof`，原进程继续运行 |
| 后台服务 | `shell_bg_spawn` 返回稳定 `cmd-*` session，结果明确写 `state=running`；核对真实 PID 存活 |
| 取消与其他任务 | 最终桌面复验：Stop 后约 639ms 目标进程消失，禁止输出未生成，另一 task 的后台服务仍存活；两个不同项目的隔离另有真实进程回归 |
| Windows 崩溃清理 | 修复后再次强制结束真实测试应用，正在运行的后台服务随 owner 一并退出，无需用户手工清理 |
| 审批中崩溃恢复 | 在 WaitingApproval 强制结束测试实例并重启，Recovery Center 出现原记录；未执行未批准命令，也未生成禁止输出 |
| 检查点冲突 | `agent_recovery` 在真实临时文件上验证 expected-after 可恢复，用户后改文件为 SkipConflict，拒绝逃逸、符号链接和硬链接 |
| 两个项目 Tab | 用户此前已确认人工通过，#29 已关闭；本轮不重复要求人工操作 |

桌面复验产物位于本机 `target/agent-validation/`，该目录是调试产物，不属于发行包。可重复使用的驱动为 [Windows 窗口控制](../../scripts/windows-desktop-control.ps1)，模型与真实命令 fixture 位于 [agent-acceptance](../../scripts/fixtures/agent-acceptance/provider.py)。`X` / `Y` 采用该驱动所保存的窗口截图坐标。

已保存可随仓库复核的 [Stop 状态截图](evidence/agent-acceptance-2026-10-26/cancel-final-complete.png)、[接管状态截图](evidence/agent-acceptance-2026-10-26/repl-budget-fixed.png)、[恢复截图](evidence/agent-acceptance-2026-10-26/recovery-final-after.png)，以及 [取消断言](evidence/agent-acceptance-2026-10-26/desktop-stop-verified.json) 与 [强退清理断言](evidence/agent-acceptance-2026-10-26/desktop-crash-verified.json)。接管截图中 WaitingUser 没有错误的预算耗尽卡。

## 测试暴露的修复

| 问题 | 修复与回归 |
|---|---|
| Windows 嵌套引号命令只打印脚本却返回 0 | Agent 一次性命令使用实际探测到的默认 shell；包含空格项目和带引号 PowerShell 写入的真实进程测试先失败、后通过 |
| 取消一个项目终止其他任务的后台服务 | 新增按 task owner 终止进程，保留用户 pane 和其他 task；两个真实后台进程回归先失败、后通过 |
| 强退 Windows 应用留下后台服务 | 非 PTY 命令也保留现有 Windows Job Object，关闭最后一个句柄时终止进程树；真实 owner 进程被强制终止的回归先失败、后通过。文件/网络/凭据能力仍 unsupported |
| Stop 等待同步命令 30 秒超时 | 长命令在作用域内执行，取消监视每 20ms 检查 token，终止本任务进程并等待 worker 退出；真实 60 秒命令的取消测试先失败、后通过，没有后台遗留 worker |
| Codex MCP 交互请求不回应，Turn 挂住 | 不支持的服务端 request 回 JSON-RPC `-32601`；不支持的 notification 仍为诊断。真实子进程对端回归先失败、后通过，未增加隐式授权 |
| `task-<timestamp>` 被误识别为 `sk-` 密钥 | 形态匹配要求 token 边界，保留任务 ID、路径和恢复引用；真实 journal/snapshot 回归同时断言任务 ID 完整、真实密钥仍被脱敏 |
| 右侧 Composer 的 Stop 和停靠按钮被挤出面板 | 普通 chip 可横向滚动，关键按钮固定在独立区域。真实 GPUI 布局原为 420px 面板中 Stop 位于 x=557px，修复后位于边界内；工具栏高度也受回归约束 |
| 接管终端被错误提示为“预算耗尽” | 预算卡仅对 `BudgetExhausted` 显示；TerminalControl 保持原接管/交还流程 |

回归命令：

```powershell
cargo test -p termior-ai --test shell_acceptance --offline
cargo test -p termior-store --test agent_recovery --offline
cargo test -p termior-agent-host --lib unsupported_server_request --offline
cargo test -p termior-ui --lib --offline
cargo test --workspace --lib --tests --offline --no-fail-fast -- --test-threads=1
cargo clippy --workspace --all-targets --offline -- -D warnings
```

全工作区并发批次曾遇到一次 Windows 原子替换临时文件的 access denied；该用例单独复跑通过。未把这个未稳定复现的现象列作已修复。默认 ignored 包括真实 CLI、SSH/系统凭据库、网络下载等有外部前置条件的测试，不能把 ignored 计为通过。

最终全工作区以单测试线程完整复跑：54 个测试目标，**1003 passed / 0 failed / 8 ignored**；同时保留用例内部的真实并发、跨任务与进程故障场景。`cargo fmt --all -- --check`、全工作区 all-targets clippy（`-D warnings`）及变更空白检查通过。

## 真实外部模型

使用 [installed_backend_acceptance](../../crates/termior-agent-host/tests/installed_backend_acceptance.rs) 在新临时项目内生成未出现在提示词中的随机内容，要求外部 Agent 真实读取文件并返回证据；拒绝额外权限请求，核对原文件未变。

| 后端 | 结果 | 边界 |
|---|---|---|
| OpenCode 1.19.4 / ACP 1 | **通过真实模型 Turn**，返回文件中的随机证据并正常 end_turn | 使用已配置默认模型；Gemini CLI、批准写入和跨后端质量比较未由这个只读 smoke 覆盖 |
| Codex CLI 0.164.0 | 握手通过；不支持的交互请求已正确结束；**真实文件读取未通过** | Turn 返回本机 Windows sandbox 的 `CryptUnprotectData failed: 2148073483`。这不是版本拒绝，也没有把失败伪报成读取通过 |
| 内置 Agent | 本地 SSE fixture 通过真实 Composer/工具链 | 模型是确定性模拟，不能与外部真实模型比较完成率、费用或质量 |

复验命令会消耗所选 CLI 的模型额度：

```powershell
$env:TERMIOR_REAL_BACKEND_TESTS = '1'
$env:TERMIOR_OPENCODE_PATH = Join-Path (Split-Path (Get-Command opencode).Source) 'node_modules/opencode-ai/bin/opencode.exe'
cargo test -p termior-agent-host --test installed_backend_acceptance --offline -- --ignored --nocapture
```

## 三平台 CI

[c06f079 的 CI](https://github.com/willmove/termior/actions/runs/40288463484) 全部 12 个作业成功。实际读取作业日志，确认三个 Core 作业均运行 `black_box_escape_suite`、`subprocess_journal_exit_recovers_running_state` 与关键 journal event fault injection；三个 Desktop 作业均构建 release、检查体积并运行真实窗口 smoke 和进程级 NFR。三个 Context latency 作业也通过。

以下为下载的原始 NFR payload 的每次作业汇总值：

| 平台 | 首帧 ms | RSS MiB | FPS | 回显 p99 ms |
|---|---:|---:|---:|---:|
| Windows | 193.25 | 58.47 | 22 | 32.3 |
| macOS | 444.62 | 68.39 | 60 | 21.6 |
| Linux | 232.91 | 172.08 | 31 | 13.0 |

这证明现有共享 runner 的**回归基线门禁**通过，不证明 SDD 中所有绝对性能目标已达成，也不等同于长期 soak。三平台检查可由 CI 执行，不需要维护者逐个平台点击。

代码提交 `ac89c69f29b0f2cc40715c6b88baf88f2ae36e53` 的 [新 CI](https://github.com/willmove/termior/actions/runs/40305374428) **12 项全部通过**，[Audit](https://github.com/willmove/termior/actions/runs/40305374350) 也通过；不是只引用修复前的运行。作业状态保存在 [CI summary](evidence/agent-acceptance-2026-10-26/ci-ac89-summary.json)。

| 平台 | 首帧 ms | RSS MiB | FPS | 回显 p99 ms | 原始 payload |
|---|---:|---:|---:|---:|---|
| Windows | 157.20 | 58.64 | 31 | 34.1 | [进程](evidence/agent-acceptance-2026-10-26/ci-ac89-windows-run.json) / [上下文](evidence/agent-acceptance-2026-10-26/ci-ac89-windows-context.json) |
| macOS | 358.15 | 68.28 | 60 | 22.7 | [进程](evidence/agent-acceptance-2026-10-26/ci-ac89-macos-run.json) / [上下文](evidence/agent-acceptance-2026-10-26/ci-ac89-macos-context.json) |
| Linux | 155.49 | 171.73 | 62 | 12.2 | [进程](evidence/agent-acceptance-2026-10-26/ci-ac89-linux-run.json) / [上下文](evidence/agent-acceptance-2026-10-26/ci-ac89-linux-context.json) |

两次共享 runner 的负载与渲染环境可能不同，不把数值变化归因于本轮修复；判断仍使用未放宽的既有回归门禁。上述结果完成 B 6.6 和 C 8.3。D 8.3 的一般三平台回归已有证据，Agent IO 专项与完整阶段退出门禁仍保留。

## 协议与格式快照

| 协议 | 固定值与安全边界 |
|---|---|
| MCP | `2025-06-18`；版本不匹配拒绝，红队覆盖 list-changed、同名工具、断线重连、投毒、输出上限、路径与 token |
| ACP | 整数 `1`；缺失/null/字符串/其他数值拒绝，宿主 FS/terminal capability 按已配置 handler 声明 |
| Agent Skills | [规范快照 217be548](https://github.com/agentskills/agentskills/blob/217be548739f21d6008915c29aefe320ea1a90af/docs/specification.mdx)，SHA-256 `b9079c0c10b7930e8c6a20ff2bc10cda2a3343c55185120e3f1116a1a529b220` |

Skills 是明确的实现子集：读取 SKILL.md 元数据并按需激活正文/资源；本地 `allowed-tools` 使用逗号分隔，与当前任务工具取交集。上游该字段为空格分隔且标记 Experimental；本实现不声称完整 YAML/全格式兼容，不把这个字段解释成自动批准。Codex 握手保持 `experimentalApi=false`，未知请求不会开启实验权限。

## 可以解除与仍待完成的门禁

- B 的三平台回归 6.6，C 的协议快照、真实 OpenCode 只读端到端 smoke、上下文/压缩/规则/Skills/MCP/Hook 自动回归及三平台 NFR 门禁 8.3 已完成，清单同步到 tasks 与 #7/#13。
- D 的 journal fault、checkpoint 冲突、worktree 生命周期及三平台 escape suite 已有自动执行证据，可以完成 8.1；Windows 的 fail-closed 通过不等于 AppContainer/ACL/网络隔离已经实现。
- #28 的升级跟踪、补丁比较、节奏约定及现有 Audit 配置的回归已经通过；完整桌面许可检查独立保留在 [#64](https://github.com/willmove/termior/issues/64)，没有放宽许可允许列表。
- Codex Windows 沙箱凭据问题、Windows 完整 sandbox 实现、真实公共模型任务集/并行基线、磁盘耗尽和三平台长期资源门禁仍待处理。这些分别是运行环境、实现或自动化证据缺口，不再笼统交给用户做点击测试。
- B–E 的其他阶段依赖及退出条件仍需满足后再归档，不因一次 smoke 通过关闭整个阶段。

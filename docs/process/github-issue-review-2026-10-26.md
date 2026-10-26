# GitHub issue 核对与人工验收

核对日期：2026-10-26。范围：`willmove/termior` 最初核对时的全部 7 个开放 issue，含正文、评论、产品规格、阶段任务和当前实现。以下区分已完成实现、自动回归和仍需验收的事项；#29 已完成用户人工验收和本地归档，并以 completed 关闭，其余 issue 保持开放。

## 逐项结论

| Issue | 代码库现状与本次处理 | 剩余工作 |
|---|---|---|
| [#7 Stage B 手工验收与归档](https://github.com/willmove/termior/issues/7) | 结构化命令会话、接管/交还、通知和后端契约已有实现；回归覆盖命令输出、控制权、进程树和协议 fixture。本次修复 PID 文件创建早于写入完成导致的测试竞态 | 真实桌面长构建、后台服务、REPL、跨 Tab、取消/退出，三平台 smoke 与 NFR。完成后才能归档 B |
| [#13 Stage C 协议与真实后端](https://github.com/willmove/termior/issues/13) | 修复 ACP initialize 接受其他版本或缺失版本的问题；增加真实子进程协议回归。记录 MCP `2025-06-18`、ACP 整数 `1` 与 Skills 支持子集；OpenCode 1.19.4 真实握手和会话创建已通过；补齐用户文档，完成 8.4 | Skills 上游格式快照仍未锁定；真实 ACP 模型 Turn、内置/Codex/ACP 公共任务比较，三平台上下文/内存/体积门禁；依赖 B 归档 |
| [#14 三平台 sandbox 与 Windows 后端](https://github.com/willmove/termior/issues/14) | Windows 文件/网络/凭据能力保持 unsupported，要求隔离时 fail closed；已有 escape suite。issue 中 Linux/macOS “只有契约测试”的描述已过时，仓库已有实际黑盒套件与 CI 矩阵 | Windows AppContainer/restricted token、ACL、网络策略仍需实现及实机验证；Linux/macOS 需在对应平台保留黑盒证据。不是只点击验收即可完成的事项 |
| [#20 Stage D 安全与回归](https://github.com/willmove/termior/issues/20) | 已有 journal fault injection、checkpoint 冲突、worktree、escape suite。本次运行可用自动回归，补齐审批/worktree/sandbox/checkpoint/远程副作用用户说明，完成 8.4 | 三平台崩溃恢复与存储迁移、NFR、Windows sandbox、阶段门禁。D 未满足整体归档条件 |
| [#23 Stage E 通知与评测](https://github.com/willmove/termior/issues/23) | 修复 Started/Working 进度弹窗：按 agent ID 汇总铃铛状态，前后台均静默；完成 6.1。新增非 Git 与目标父路径不可用的 worktree 回归；补齐应用关闭、预算、权限、重试边界文档，完成 7.5 | 3.6 的真实磁盘不足等退出场景，单代理与并行的真实基线比较，三平台长期 smoke/迁移/资源残留。依赖 D 归档 |
| [#28 依赖与 GPUI 补丁](https://github.com/willmove/termior/issues/28) | 升级直接依赖 thiserror 2、dirs 6，清理旧 dirs 的锁文件依赖；clippy MSRV 与 1.98.1 对齐；记录升级节奏和上游补丁比较 | cargo deny 的现有许可失败需核实；GPUI 保留当前 revision 与补丁，升级须三平台与 Windows 关闭路径/NFR 验收。sysinfo 留到 NFR 专项升级 |
| [#29 文档整理与归档](https://github.com/willmove/termior/issues/29) | 原有提交已整理 docs 并归档三个 change；用户于 2026-10-26 确认“两个项目 Tab”人工验收通过，7.2 已勾选，`per-tab-project-folder` 已归档并生成主规格。Release 文档已明确 GitHub Release body 为发行说明归档 | **已关闭（completed）**，GitHub 四项清单均已勾选；本次归档与验收记录仍在本地工作区，尚未提交推送 |

## 人工测试顺序

先验证项目文件夹和 Stage B，再做 C、D、E；后一阶段的归档依赖前一阶段的真实验收。每项记录平台、应用 commit、后端版本、实际结果和日志位置，不能用 strict 校验代替运行证据。

| 场景 | 操作与通过条件 | 关联 issue |
|---|---|---|
| 两个项目 Tab | **已通过**：2026-10-26 用户明确确认该场景人工测试通过；对应 7.2 已完成。范围：两个终端 Tab 分别打开不同项目，会话存活，Explorer/Git/状态栏跟随，非 Git 空态及特殊路径 | #29 |
| 长构建与后台服务 | 从 Composer 启动长构建、增量读输出、Stop 后确认退出；后台服务保留输出，不能只因获得 PID 就报告完成 | #7 |
| REPL 接管与交还 | Agent 获批输入，用户接管后 Agent 写入被拒绝，交还并重新获批后可继续 | #7 |
| 跨 Tab 与后端退出 | 任务在项目 A 创建，切到 B 后仍在 A 执行；后端被终止时显示失败或 unknown，不伪报成功 | #7 |
| 真实外部后端 | 已认证 Codex 与 Gemini CLI/OpenCode 分别完成只读、批准写入、拒绝、取消；记录确切版本与能力。用同一任务集比较内置/Codex/ACP | #7、#13 |
| 上下文与扩展 | 嵌套 AGENTS.md、显式激活 Skill、长会话压缩、MCP 断开、Hook 改写参数；Inspector 来源正确，修改后的参数重新审批 | #13 |
| 崩溃恢复 | 分别在审批、Diff 审阅、长命令中退出应用；重启后等待或 unknown，不重放副作用。确认 Prepare retry 创建新 attempt | #20 |
| 检查点冲突 | 应用修改后手工改同一文件，再 Restore unchanged outputs；该文件应跳过冲突，未改文件可恢复 | #20 |
| Windows sandbox | 完成受限身份/文件/网络后端后运行逃逸套件；实现前要求这些能力应拒绝启动。Job Object 不作为完整隔离证据 | #14、#20 |
| 通知 | 前台隐藏 Composer 与后台窗口分别运行任务；Started/Working 只更新铃铛，完成/失败/等待按策略通知。重复 working 不刷屏 | #23 |
| 自动化停机与重试 | 关闭应用跨过计划时间再启动，核对 Skip/RunOnce/RunEach；unknown/非幂等失败不自动重试；去重不执行两次副作用 | #23 |
| 资源与边界 | 隔离测试环境中模拟磁盘不足、worktree 冲突、依赖变化和部分 child 失败；恢复后无重复写入、覆盖用户修改或遗留进程 | #23 |
| 三平台与性能 | Windows/macOS/Linux 分别做首次/升级启动、存储迁移、长期运行、进程清理，以及体积、首帧、RSS、上下文延迟门禁 | #7、#13、#20、#23、#28 |

详细步骤见现有 [Stage B](ai-agent-stage-b-validation.md)、[Stage C](ai-agent-stage-c-validation.md)、[Stage D](ai-agent-stage-d-validation.md)、[Stage E](ai-agent-stage-e-validation.md) 验证指南。用户使用边界见 [Agent 运行时说明](../user/agent-runtime.md)。

### 本机真实后端前置条件

按用户确认更新兼容策略：Codex 接受 `>=0.137.0, <1.0.0` 的稳定版，不再因本机稳定 minor 更新而拒绝启动。已生成本机 `codex-cli 0.164.0` schema 并通过真实 stdio 握手；版本不兼容前置问题已解除，实验协议仍关闭。版本范围与证据见 [Codex 兼容记录](codex-app-server-compat.md)。

OpenCode 已由用户重装，`opencode --version` 确认为 1.19.4。真实 `opencode acp --pure` 经 Termior `AcpBackend` 完成协议版本 1 握手及会话创建；无法启动前置问题已解除。本次未发起真实模型 Turn，跨后端公共任务集、审批/取消与桌面验收仍待完成。

## 本次自动验证

验证环境为 Windows，Rust 1.98.1；测试 fixture 使用 PowerShell 7。新增通知和 ACP 测试均在修复前失败，再验证修复后的结果。非 Git/目标路径失败测试不等同于真实磁盘耗尽。

| 命令 | 结果 |
|---|---|
| `cargo test -p termior-ai -p termior-agent-host -p termior-store -p termior-terminal -p termior-hooks -p termior-vcs -p termior-platform --lib --tests --offline --no-fail-fast` | 31 个测试目标，458 passed / 0 failed / 4 ignored；包含 recovery、journal fault、context、orchestration、worktree 和 Windows sandbox fail-closed |
| `cargo check --workspace --all-targets --offline` | 通过，含 UI、桌面入口、bench 与 vendored Windows GPUI |
| `cargo clippy --workspace --all-targets --offline -- -D warnings` | 通过 |
| `cargo fmt --all -- --check` 与 `git diff --check` | 通过 |
| `openspec validate --all --strict --no-interactive` | 13 passed / 0 failed；五个活动 change 未归档 |

首次批次的 4 个 ignored 分别是 Codex 握手、真实 GitHub installer 下载，以及依赖 Python paramiko/OpenSSH 的 SSH/SFTP roundtrip 和 SSH 端口转发 roundtrip；后续已单独执行 Codex 握手，见下表。编译还报告上游 `proc-macro-error2 2.0.1` 的未来 Rust 兼容提示，当前固定工具链通过。

首次扩展回归中，本地 HTTP MCP fixture 曾出现一次连接错误；后续两次完整七 crate 回归中的该目标均通过，尚无稳定复现，不把它列为已修复问题。进程树测试的 PID 空内容竞态已定位并修复，随后定向及全批次回归通过。

### Codex 与 OpenCode 更新后的验证

| 命令 | 结果 |
|---|---|
| `cargo test -p termior-agent-host --all-targets --offline` | 64 passed / 0 failed / 2 ignored；默认忽略的两个真实后端测试另行执行并通过 |
| `cargo test -p termior-agent-host --test codex_smoke --offline -- --ignored --nocapture` | 本机 Codex 0.164.0：真实稳定 stdio 握手通过（同时在 `--include-ignored` 定向批次中执行） |
| `cargo test -p termior-agent-host --test opencode_smoke --offline -- --ignored --nocapture` | 本机 OpenCode 1.19.4：真实 ACP 1 握手和会话创建通过，使用原生 `TERMIOR_OPENCODE_PATH` |
| `cargo clippy --workspace --all-targets --offline -- -D warnings` | 通过 |
| `cargo fmt --all -- --check`、`git diff --check`、OpenSpec strict | 通过，OpenSpec 13 passed |

新增版本测试在修复前证明旧白名单拒绝新版、误接收带旧 minor 前缀的预发布字符串；修复后接受本机及后续稳定 minor，拒绝过旧、预发布、格式错误与新 major。两个真实后端 smoke 均未发起模型 Turn。

## 未满足的门禁

`cargo-deny 0.20.2 --offline --target x86_64-pc-windows-msvc check` 在升级前后均为 advisories/bans/sources 通过、licenses 失败。阻塞为既有 GPUI 许可元数据、允许列表及 GPL 声明问题，见 [依赖维护记录](dependency-maintenance.md)。未放宽 deny 策略。离线 advisory 结果仅对应本机缓存数据库。

真实模型 Turn、macOS/Linux 桌面环境、长期性能与 Windows 完整 sandbox 证据尚不齐全，因此 Stage B–E 四个活动 change 继续保留，不标记整体验收完成。

## 项目文件夹人工验收与归档

2026-10-26，用户确认“人工测试：两个项目 Tab，这个场景已完成人工测试，已通过”。据此完成 `per-tab-project-folder` 的 7.2，执行 `openspec archive per-tab-project-folder --yes` 成功：

- 归档：[2026-10-26-per-tab-project-folder](../../openspec/changes/archive/2026-10-26-per-tab-project-folder/tasks.md)，保留用户验收来源。
- 主规格：[tab-project-folder](../../openspec/specs/tab-project-folder/spec.md)，新增 7 条 requirement。
- #29 其余三项已完成：docs 分层、三个既有 change 归档、Release 说明归档位置确认。本次实际读取 v0.2.4 Release 确认正文存在。

归档与记录变更尚未提交推送，不把本地路径描述为 GitHub 默认分支上的已有文件。

已更新 GitHub #29 的四项清单和完成依据，并关闭为 completed；回读确认状态为 CLOSED、未勾选项为 0。归档后的全量 OpenSpec strict 为 13 passed / 0 failed，`git diff --check` 通过。

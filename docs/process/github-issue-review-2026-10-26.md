# GitHub issue 核对与验收状态

核对日期：2026-10-26。范围：`willmove/termior` 最初核对时的全部 7 个开放 issue，含正文、评论、规格、阶段任务和实现。#29 已归档、提交并关闭；#28 已完成升级跟踪和现有 CI 验收并关闭，完整桌面许可缺口转入 #64。用户要求尽量代做验收后，已增加真实桌面操作、真实外部模型、故障模拟与三平台 CI 核对，见 [自动执行记录](agent-acceptance-2026-10-26.md)。

## 逐项结论

| Issue | 代码库现状与本次处理 | 剩余工作 |
|---|---|---|
| [#7 Stage B 手工验收与归档](https://github.com/willmove/termior/issues/7) | 真实桌面长构建、后台服务、REPL 接管/交还与审批/恢复已代做；修复引号命令不执行、跨任务取消、运行中取消等待及 Stop 被裁切。三平台 CI 基线全部通过 | Codex 真实读取受本机 Windows sandbox 凭据解密错误阻塞；新增修复仍须对应新 CI，跨项目/后端退出保留契约与实机证据边界，阶段不强行归档 |
| [#13 Stage C 协议与真实后端](https://github.com/willmove/termior/issues/13) | ACP 版本严格校验；已锁定 Skills 上游格式快照，完成 0.2；MCP 5.5/5.6 和场景回归 8.1 完成；OpenCode 1.19.4 真实模型读取通过，完成 7.5；用户文档 8.4 完成 | 内置/Codex/ACP 的真实公共任务比较仍不齐全；依赖 B 归档，新增提交的三平台/NFR 证据须保留 |
| [#14 三平台 sandbox 与 Windows 后端](https://github.com/willmove/termior/issues/14) | Windows 文件/网络/凭据能力保持 unsupported，要求隔离时 fail closed；已有 escape suite。issue 中 Linux/macOS “只有契约测试”的描述已过时，仓库已有实际黑盒套件与 CI 矩阵 | Windows AppContainer/restricted token、ACL、网络策略仍需实现及实机验证；Linux/macOS 需在对应平台保留黑盒证据。不是只点击验收即可完成的事项 |
| [#20 Stage D 安全与回归](https://github.com/willmove/termior/issues/20) | journal fault、checkpoint 冲突、worktree 与三平台 escape suite 8.1 已有执行证据；安全文档 8.4 完成；桌面审批中崩溃不重放已验证，并修复任务 ID 脱敏破坏恢复引用的问题 | Windows 完整 sandbox、阶段前置依赖和新提交的三平台/NFR 门禁；继续保留 change |
| [#23 Stage E 通知与评测](https://github.com/willmove/termior/issues/23) | 修复 Started/Working 进度弹窗：按 agent ID 汇总铃铛状态，前后台均静默；完成 6.1。新增非 Git 与目标父路径不可用的 worktree 回归；补齐应用关闭、预算、权限、重试边界文档，完成 7.5 | 3.6 的真实磁盘不足等退出场景，单代理与并行的真实基线比较，三平台长期 smoke/迁移/资源残留。依赖 D 归档 |
| [#28 依赖与 GPUI 补丁](https://github.com/willmove/termior/issues/28) | thiserror 2、dirs 6、MSRV、升级节奏和上游补丁比较完成；c06f079 的 12 项 CI 与既有范围的 Audit 全通过，四项待办已勾选并关闭 | **已关闭（completed）**。完整桌面许可失败独立保留为 [#64](https://github.com/willmove/termior/issues/64)，不放宽 deny 策略 |
| [#29 文档整理与归档](https://github.com/willmove/termior/issues/29) | docs 分层、三个既有 change、用户确认的两个项目 Tab、per-tab-project-folder 主规格与 Release 归档位置均完成 | **已关闭（completed）**，归档和记录已由 c06f079 提交并推送 main |

## 优先复用自动验收

阶段依赖仍按 B、C、D、E 执行，但以下场景已由桌面驱动、真实进程、确定性故障/时钟模拟及 CI 代做，不再整体交给用户点击。结果必须区分实际操作、模拟、未覆盖与失败；strict 校验不代替运行证据。

| 场景 | 操作与通过条件 | 关联 issue |
|---|---|---|
| 两个项目 Tab | **已通过**：2026-10-26 用户明确确认该场景人工测试通过；对应 7.2 已完成。范围：两个终端 Tab 分别打开不同项目，会话存活，Explorer/Git/状态栏跟随，非 Git 空态及特殊路径 | #29 |
| 长构建与后台服务 | **已代做**：Composer 审批后六次进度输出、rustc 编译与运行；后台结果报告 running，核对真实 PID；取消另有真实进程回归 | #7 |
| REPL 接管与交还 | **已代做**：同一 PowerShell PTY，接管时为 WaitingUser，不抢先写入；Hand back 后输出 Agent 证据并继续原进程 | #7 |
| 跨 Tab 与后端退出 | 任务在项目 A 创建，切到 B 后仍在 A 执行；后端被终止时显示失败或 unknown，不伪报成功 | #7 |
| 真实外部后端 | **部分通过**：OpenCode 真实只读 Turn 成功；Codex 握手成功、真实读取因 DPAPI 失败。批准写入/拒绝/取消和真实公共模型比较仍需补证，可由同一 opt-in 测试驱动 | #7、#13 |
| 上下文与扩展 | **自动场景通过**：嵌套规则、渐进加载、压缩、MCP 红队/重连、Hook 参数重校验；C 8.1 已完成。不宣称每个 Inspector UI 组合均实机覆盖 | #13 |
| 崩溃恢复 | **已代做部分**：真实窗口审批中强退/重启；此外自动 fault suite 覆盖审批、审阅、执行恢复和新 attempt，不重放副作用 | #20 |
| 检查点冲突 | **自动真实文件通过**：用户后改文件 SkipConflict，未改文件可恢复；不需要用户手工制造冲突 | #20 |
| Windows sandbox | 完成受限身份/文件/网络后端后运行逃逸套件；实现前要求这些能力应拒绝启动。Job Object 不作为完整隔离证据 | #14、#20 |
| 通知 | 前台隐藏 Composer 与后台窗口分别运行任务；Started/Working 只更新铃铛，完成/失败/等待按策略通知。重复 working 不刷屏 | #23 |
| 自动化停机与重试 | **时钟/重建模拟通过**：Skip/RunOnce/RunEach、模板不可变、去重及 unknown/非幂等失败不自动重试，见 orchestration_automation；长期运行另验 | #23 |
| 资源与边界 | 隔离测试环境中模拟磁盘不足、worktree 冲突、依赖变化和部分 child 失败；恢复后无重复写入、覆盖用户修改或遗留进程 | #23 |
| 三平台与性能 | **CI 基线通过**：三平台 Core/Desktop/Context latency、体积/首帧/RSS/FPS/回显回归；长期 soak、新提交检查和绝对产品目标不由该结果替代 | #7、#13、#20、#23、#28 |

详细步骤见现有 [Stage B](ai-agent-stage-b-validation.md)、[Stage C](ai-agent-stage-c-validation.md)、[Stage D](ai-agent-stage-d-validation.md)、[Stage E](ai-agent-stage-e-validation.md) 验证指南。用户使用边界见 [Agent 运行时说明](../user/agent-runtime.md)。

### 本机真实后端前置条件

按用户确认更新兼容策略：Codex 接受 `>=0.137.0, <1.0.0` 的稳定版，不再因本机稳定 minor 更新而拒绝启动。已生成本机 `codex-cli 0.164.0` schema 并通过真实 stdio 握手；版本不兼容前置问题已解除，实验协议仍关闭。版本范围与证据见 [Codex 兼容记录](codex-app-server-compat.md)。

OpenCode 已由用户重装，1.19.4 的真实 `opencode acp --pure` 经 `AcpBackend` 完成协议 1 握手、会话创建及模型 Turn，成功返回临时文件中未出现在提示词里的随机内容。Codex 0.164.0 的真实 Turn 暴露出本机 Windows sandbox `CryptUnprotectData failed: 2148073483`；未知交互请求不回应导致的挂起已修复，但不能将该读取记为通过。公共任务集与审批/取消的实际模型覆盖仍不齐。

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

真实 OpenCode Turn 与三平台 CI 已补齐；Codex Windows sandbox、真实公共模型任务集、长期资源/磁盘耗尽及完整 Windows sandbox 仍不齐，因此 B–E 四个 change 继续保留。各项可复用的自动证据与剩余实现见 [自动执行记录](agent-acceptance-2026-10-26.md)，不要求用户重复已经通过的测试。

## 项目文件夹人工验收与归档

2026-10-26，用户确认“人工测试：两个项目 Tab，这个场景已完成人工测试，已通过”。据此完成 `per-tab-project-folder` 的 7.2，执行 `openspec archive per-tab-project-folder --yes` 成功：

- 归档：[2026-10-26-per-tab-project-folder](../../openspec/changes/archive/2026-10-26-per-tab-project-folder/tasks.md)，保留用户验收来源。
- 主规格：[tab-project-folder](../../openspec/specs/tab-project-folder/spec.md)，新增 7 条 requirement。
- #29 其余三项已完成：docs 分层、三个既有 change 归档、Release 说明归档位置确认。本次实际读取 v0.2.4 Release 确认正文存在。

上述归档与记录已由提交 `c06f079af2db9bd1fd0fc11faef1b07933f372d2` 推送 main。

已更新 GitHub #29 的四项清单和完成依据，并关闭为 completed；回读确认状态为 CLOSED、未勾选项为 0。归档后的全量 OpenSpec strict 为 13 passed / 0 failed，`git diff --check` 通过。

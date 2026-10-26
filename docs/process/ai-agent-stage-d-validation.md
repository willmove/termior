# Stage D 验证指南：恢复、检查点与隔离

Stage D 在副作用前写入 hash-chained journal，并用原子 snapshot 恢复任务。正在执行且重启后无法确认的模型、工具和命令统一恢复为 unknown，不自动重放。

审批、worktree、sandbox 和文件恢复的使用边界见 [用户说明](../user/agent-runtime.md)。Windows 的完整 sandbox 后端仍需实现；现有 escape suite 证明的是其 fail-closed 行为。

## 自动化验证

```powershell
cargo test -p termior-store --test agent_recovery
cargo test -p termior-store --lib fault_tests
cargo test -p termior-ai --test journal_fault
cargo test -p termior-ai --test runtime_reliability
cargo test -p termior-vcs --test worktree_environment
cargo test -p termior-agent-host --test sandbox_contract
cargo test -p termior-agent-host --test escape_suite
```

测试覆盖 journal 尾部半记录修复、中段篡改隔离、未知事件保留、SHA-256 hash chain、每个关键事件在写入前/中/后的 fault injection（含一次真实进程退出）、secret 写盘脱敏、snapshot+replay、unknown 恢复、新 attempt ID、内容寻址 checkpoint、用户后改冲突、路径逃逸/符号链接/硬链接拒绝、worktree 创建与安全清理、sandbox required capability fail-closed、三平台 black-box escape suite 和 retention 清理顺序。

Escape suite 在 Core 矩阵的 Ubuntu、macOS、Windows 上随 `cargo test` 运行。Linux 需要 `bwrap`（CI 安装 `bubblewrap`，并关闭 `kernel.apparmor_restrict_unprivileged_userns`，否则 `--unshare-net` 在配置 loopback 时会因缺少 `CAP_NET_ADMIN` 失败）。macOS 使用 `sandbox-exec`，profile 带上 dyld 启动所需的 `file-map-executable` 与 mach 基线允许项。任一逃逸成功都会让该作业失败。当前 Windows 没有经黑盒验证的文件/网络/凭据隔离，套件断言 `prepare` fail closed，并且不会启动 payload。

## 桌面复验与已自动覆盖的场景

2026-10-27 补上祖先目录穿越授权后，Windows CI 上的逃逸套件重新通过；证据与边界见 [自动执行记录（2026-10-27）](agent-acceptance-2026-10-27.md)。


2026-10-26 已读取三平台 Core 的真实 escape/fault 日志，并运行本机真实文件 checkpoint/worktree、journal 恢复及桌面审批中强退/重启。8.1 完成。非 PTY Windows 后台命令现也绑定 Job Object；真实 owner 进程被强制终止的回归验证无后台残留。该进程清理能力不代表文件、网络或凭据隔离。详见 [自动执行记录](agent-acceptance-2026-10-26.md)。

1. 发起需要审批的文件修改，批准并进入 Diff 审阅。确认 `agent-tasks/<task-id>/events.jsonl` 与 `snapshot.json` 已存在。
2. 应用 Diff。输出应包含 `checkpoint=checkpoint-...`，对应 manifest 位于 `agent-checkpoints/manifests/`。在文件仍等于 expected-after 时执行 restore plan 应恢复原内容；先手工修改文件时必须变成 `SkipConflict`。
3. 让任务运行长命令并强制关闭应用。重启后检查恢复数据：活动操作必须为 unknown，不能自动再执行命令。点击 `Prepare retry` 时 attempt ID 必须递增且只建立新的待运行 attempt；点击 `Abandon` 后该任务应从恢复列表消失。
4. 在临时 Git 仓库创建 worktree 环境。dirty 或仍有活动进程时清理必须被拒绝；集成前查看 base、target、diff、dirty 和 conflict preview。
5. 重启后打开 Composer：有未完成任务时标题栏出现 `Recovery N`；点击后应看到持久化状态、恢复状态、最后可信 event、后端、可用动作和损坏 journal 的隔离诊断。
6. 应用过 AI 变更后，标题栏出现 `Checkpoints N`；展开后应列出 task/event 与可恢复文件，并明确说明 process/network/external-service 副作用不可撤销。点击 `Restore unchanged outputs`，仍等于 expected-after 的文件应恢复，用户随后修改过的文件应报告并跳过冲突。
7. 将鼠标移到环境 chip，核对 host、root、file/network/credential/process 能力和 verified/reported/unknown 信任级别。

## 平台隔离矩阵

`PlatformSandbox::probe` 是最终事实来源：

| 平台 | 文件/凭据/网络 | 进程树 | 行为 |
|---|---|---|---|
| Linux + `bwrap` | verified | verified | 系统库目录只读，工作区绑定到 `/workspace`，不挂载宿主根；清空环境；默认 unshare network/PID，沙箱退出后不留子进程 |
| macOS + `sandbox-exec` | verified | verified | profile 默认拒绝；允许 dyld/libsystem 启动（含根 vnode 的 literal 读取，不含子树），只允许工作区写入和系统目录读取，默认拒绝网络，清空环境，退出时结束进程组 |
| Windows | unsupported | verified | 当前没有经黑盒验证的文件/网络/凭据隔离；required 请求拒绝执行，escape suite 断言 fail closed |

Windows 不会把普通进程或 Job Object 描述成完整 sandbox。选择 sandboxed 且要求上述能力时会 fail closed。direct/worktree 也不会被显示为 sandboxed。

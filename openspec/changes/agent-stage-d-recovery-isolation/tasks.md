## 0. 阶段门禁与 prototype

- [ ] 0.1 确认 M8 已归档，上下文/扩展/三个后端公共评测通过
- [x] 0.2 分别为 Windows、macOS、Linux 做 sandbox capability prototype，记录可实现矩阵、依赖、权限和已知缺口
- [x] 0.3 根据 prototype 更新本 design；任何无法验证的能力保持 unsupported

## 1. 任务 Journal 与 Reducer

- [x] 1.1 在 `termior-store` 定义 task 目录、journal header/record、sequence、hash chain 和 schema migration
- [x] 1.2 实现 append writer：关键事件 flush、非关键 delta 批量、跨平台 crash-safe 关闭
- [x] 1.3 把 M6 TaskRuntime 状态变更全部归约为 TaskEvent；禁止绕过 reducer 直接改持久状态
- [x] 1.4 实现原子 TaskSnapshot 和从 snapshot sequence 继续重放
- [x] 1.5 实现尾部半记录修复、中段损坏隔离、未知 event 保留和诊断导出
- [x] 1.6 建立 fault-injection tests：每个关键事件写入前/中/后模拟退出并比较恢复状态

## 2. Unknown 恢复与会话入口

- [x] 2.1 启动扫描 task index/journal，running model/tool/command 统一恢复为 unknown
- [x] 2.2 定义 inspect/abandon/retry/resume commands；retry 必须创建新 attempt ID
- [x] 2.3 对可查询的 Codex/ACP session 先拉取远端/后端状态，再归约为已知或 unknown
- [x] 2.4 增加恢复中心 UI：按项目列出 waiting/unknown/failed，显示最后可信事件和可用动作
- [x] 2.5 测试等待审批、等待 change review、命令运行中三类重启场景

## 3. Checkpoint

- [x] 3.1 定义 Checkpoint manifest、content-addressed file blobs、before/expected-after hash 和覆盖范围
- [x] 3.2 在变更应用前创建检查点，并记录对应 task sequence/change set/tool call
- [x] 3.3 实现三方比较和逐文件 restore plan；冲突默认 skip，恢复操作自身写入新事件
- [x] 3.4 检查点 UI 明确显示文件可恢复项与不可撤销的进程/network/external events
- [x] 3.5 覆盖新增/删除/重命名、权限位、符号链接/硬链接拒绝和用户后续修改测试

## 4. Git Worktree 环境

- [x] 4.1 在 `termior-vcs` 定义 WorktreeEnvironment 与 create/init/status/integrate/remove 契约
- [x] 4.2 原子化“创建→初始化→绑定任务”；失败回滚或记录 orphaned
- [x] 4.3 将任务 TerminalService、文件工具、规则和 repo map 根切换到 worktree path
- [x] 4.4 实现合入前 base/target/dirty/diff/validation/conflict 检查与 preview
- [x] 4.5 只自动处理无冲突安全路径；其他情况打开现有 Git 审阅流程
- [x] 4.6 清理前拒绝 dirty/运行进程环境，覆盖 Windows 文件占用和进程树场景

## 5. Execution Environment 与安全描述

- [x] 5.1 定义 direct/worktree/sandboxed descriptor、required capabilities 和 reported/verified/unknown 信任级别
- [x] 5.2 任务创建/恢复时固定 environment ID；变更 profile 创建新环境，不能原地偷偷扩大权限
- [x] 5.3 UI 在任务头和审批卡持续显示 host/root/file/network/credential/process 能力
- [x] 5.4 外部后端映射自身声明，未由 Termior probe 的能力不得标 verified

## 6. 平台 Sandbox

- [x] 6.1 定义 `SandboxBackend` 纯接口和 fake backend contract suite
- [x] 6.2 实现 Windows backend：受限身份/文件 ACL 或经 prototype 选定机制、网络策略、Job Object 进程树和 cleanup
- [x] 6.3 实现 macOS backend：经 prototype 选定的原生机制、目录/network/credential/process 限制和 cleanup
- [x] 6.4 实现 Linux backend：经 prototype 选定的 namespace/seccomp/文件/network/process 组合和 capability probe
- [x] 6.5 所有 backend 在 prepare 后运行 capability probe；required 不满足则 fail closed
- [x] 6.6 建立三平台 black-box escape suite：父目录、symlink、hardlink、loopback/private/public network、env secret、child escape、orphan process

## 7. 脱敏、保留与清理

- [x] 7.1 建立 streaming secret redactor，覆盖 keyring 已知 secrets、常见 credential shape 和 deny-list 路径内容
- [x] 7.2 在 journal/blob/context/hook/MCP/backend stderr/crash log 写盘边界统一调用 redactor
- [x] 7.3 实现按天数和磁盘预算的 retention；先清 blob/可重取内容，再 snapshot，保留摘要与 AcceptanceReport
- [ ] 7.4 增加设置与磁盘使用 UI；清理任务必须可取消且不破坏 journal 引用完整性

## 8. 验证与收尾

- [x] 8.1 运行 journal fault injection、checkpoint 冲突、worktree 初始化/合入/清理和三平台 escape suite
- [x] 8.2 对 direct/worktree/sandboxed 与外部后端生成用户可见 capability matrix
- [ ] 8.3 运行相关 crate tests、三平台 smoke、clippy、存储迁移和 NFR IO/启动/体积回归
- [x] 8.4 更新安全文档，准确说明审批、worktree、sandbox、checkpoint 与远程副作用边界
- [ ] 8.5 全部退出场景通过后记录偏差并严格验证本 change

## 自动验收（2026-10-26）

8.1 的关键事件 fault、真实进程退出、checkpoint 冲突、worktree 生命周期及三平台 escape suite 均有执行证据，见 [自动执行记录](../../../docs/process/agent-acceptance-2026-10-26.md)。Windows 新增的后台命令 Job Object 只解决进程清理，不将文件/网络/凭据改报 supported；0.x、6.2 及其余退出门禁仍保留。

## Windows sandbox backend（2026-10-26）

0.2、0.3 与 6.2 已实现，Windows 不再只报告进程树。受限身份用 Windows AppContainer（`TermiorSandbox` profile）：文件系统按目录写 ACL 授权，进程以 `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` 启动且拿不到任何网络 capability，因此网络是被拒绝而不是未测量；进程树由 `KILL_ON_JOB_CLOSE` 的 Job Object 收口；环境变量从短白名单重建，父进程的凭据不进入沙箱。

证据：`crates/termior-terminal/tests/windows_appcontainer.rs` 直接调用真实 Windows API，证明受限进程能读授权目录、读不到同级目录、写不到授权目录之外、连不上回环监听、不继承父环境、管道输出与退出码正常、授权失败会报错。`crates/termior-agent-host/tests/escape_suite.rs` 的九项逃逸场景（父目录、符号链接、硬链接、回环/内网/公网、env secret、子进程、孤儿进程）在 Windows 上真实跑完并全部被挡住。

三个 Windows 特有的坑记录在代码注释里：环境块缺少 `LOCALAPPDATA` 时 `CreateProcessW` 以 203 失败；ACL 的 ACE 头是 4 字节而不是 8 字节，否则 `SetNamedSecurityInfoW` 报 0x538；`UpdateProcThreadAttribute` 的第二个参数是 flags 而不是下标，传 1 会被当成 flags 拒绝。

`C:\Windows` 由 TrustedInstaller 持有，普通用户无法追加 ACL，因此沙箱不显式授权系统目录；实测受限进程本来就能读取 `cmd.exe` 等系统二进制。

## 实施核对（2026-09-30，1.6 / 6.6 于 2026-10-18 补齐）

代码已随 `8d294f0` 落地，但当时未回写本文件。以上勾选以仓库内实现与测试为证据。1.6 与 6.6 已补齐；其余未勾项仍未完成，本 change 暂不归档：

- 0.x 阶段门禁与三平台 sandbox prototype 记录未完成。
- 1.6 已覆盖每个关键事件的写入前/中/后 fault injection（进程内钩子 + running 边界的真实进程退出），恢复状态与预期一致。
- 4.3 任务绑定 worktree 后，TerminalService、文件工具、规则解析与 repo map 根切到 worktree path；路径穿越、符号链接与硬链接不得读回主工作区。4.5 仅对无冲突的 fast-forward 或 cherry-pick 自动合入，其余情况打开现有 Git 审阅且不改写用户文件。
- 5.2 创建与 `recover_journal` 固定 environment id 和权限上限；`try_apply_grant` 拒绝原地扩大，`change_profile` / `rebind_environment` 分配新 id 并保留旧 grant。5.3 任务头、任务详情和审批卡用同一条 host/root/file/network/credential/process 能力行。8.2 的矩阵由 `capability_matrix` 生成；2026-10 起设置页不再展示整表，能力信息按当前环境在 Composer 的任务头、任务详情与审批卡展示。
- 6.2 Windows 仅进程树控制，文件/网络/凭据隔离报告 unsupported。6.6 逃逸套件已进入 Core 三平台：Linux `bwrap` 与 macOS `sandbox-exec` 执行黑盒探测；Windows 对未支持能力断言 fail closed，不静默跳过。
- 7.2 已在 journal、output blob、context、hook audit、MCP diagnostic、backend stderr 与 crash log 写盘前调用 StreamingRedactor，各路径有注入已知 secret 的落盘测试。7.4 无保留设置与磁盘使用 UI。
- 8.1、8.3、8.5 仍未完成。8.4 已于 2026-10-26 补充 `docs/user/agent-runtime.md`。8.2 能力矩阵由 `capability_matrix` 生成（原曾在 Settings → Agents 展示整表，2026-10 简化为仅在 Composer 按当前环境展示）。8.1 里的 journal fault injection 与三平台 escape suite 已由 Core `cargo test` 覆盖；checkpoint 冲突与 worktree 用例仍是既有测试，阶段门禁记录本身未归档。

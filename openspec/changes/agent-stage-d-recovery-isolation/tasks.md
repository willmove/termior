## 0. 阶段门禁与 prototype

- [ ] 0.1 确认 M8 已归档，上下文/扩展/三个后端公共评测通过
- [ ] 0.2 分别为 Windows、macOS、Linux 做 sandbox capability prototype，记录可实现矩阵、依赖、权限和已知缺口
- [ ] 0.3 根据 prototype 更新本 design；任何无法验证的能力保持 unsupported

## 1. 任务 Journal 与 Reducer

- [ ] 1.1 在 `termior-store` 定义 task 目录、journal header/record、sequence、hash chain 和 schema migration
- [ ] 1.2 实现 append writer：关键事件 flush、非关键 delta 批量、跨平台 crash-safe 关闭
- [ ] 1.3 把 M6 TaskRuntime 状态变更全部归约为 TaskEvent；禁止绕过 reducer 直接改持久状态
- [ ] 1.4 实现原子 TaskSnapshot 和从 snapshot sequence 继续重放
- [ ] 1.5 实现尾部半记录修复、中段损坏隔离、未知 event 保留和诊断导出
- [ ] 1.6 建立 fault-injection tests：每个关键事件写入前/中/后模拟退出并比较恢复状态

## 2. Unknown 恢复与会话入口

- [ ] 2.1 启动扫描 task index/journal，running model/tool/command 统一恢复为 unknown
- [ ] 2.2 定义 inspect/abandon/retry/resume commands；retry 必须创建新 attempt ID
- [ ] 2.3 对可查询的 Codex/ACP session 先拉取远端/后端状态，再归约为已知或 unknown
- [ ] 2.4 增加恢复中心 UI：按项目列出 waiting/unknown/failed，显示最后可信事件和可用动作
- [ ] 2.5 测试等待审批、等待 change review、命令运行中三类重启场景

## 3. Checkpoint

- [ ] 3.1 定义 Checkpoint manifest、content-addressed file blobs、before/expected-after hash 和覆盖范围
- [ ] 3.2 在变更应用前创建检查点，并记录对应 task sequence/change set/tool call
- [ ] 3.3 实现三方比较和逐文件 restore plan；冲突默认 skip，恢复操作自身写入新事件
- [ ] 3.4 检查点 UI 明确显示文件可恢复项与不可撤销的进程/network/external events
- [ ] 3.5 覆盖新增/删除/重命名、权限位、符号链接/硬链接拒绝和用户后续修改测试

## 4. Git Worktree 环境

- [ ] 4.1 在 `termior-vcs` 定义 WorktreeEnvironment 与 create/init/status/integrate/remove 契约
- [ ] 4.2 原子化“创建→初始化→绑定任务”；失败回滚或记录 orphaned
- [ ] 4.3 将任务 TerminalService、文件工具、规则和 repo map 根切换到 worktree path
- [ ] 4.4 实现合入前 base/target/dirty/diff/validation/conflict 检查与 preview
- [ ] 4.5 只自动处理无冲突安全路径；其他情况打开现有 Git 审阅流程
- [ ] 4.6 清理前拒绝 dirty/运行进程环境，覆盖 Windows 文件占用和进程树场景

## 5. Execution Environment 与安全描述

- [ ] 5.1 定义 direct/worktree/sandboxed descriptor、required capabilities 和 reported/verified/unknown 信任级别
- [ ] 5.2 任务创建/恢复时固定 environment ID；变更 profile 创建新环境，不能原地偷偷扩大权限
- [ ] 5.3 UI 在任务头和审批卡持续显示 host/root/file/network/credential/process 能力
- [ ] 5.4 外部后端映射自身声明，未由 Termior probe 的能力不得标 verified

## 6. 平台 Sandbox

- [ ] 6.1 定义 `SandboxBackend` 纯接口和 fake backend contract suite
- [ ] 6.2 实现 Windows backend：受限身份/文件 ACL 或经 prototype 选定机制、网络策略、Job Object 进程树和 cleanup
- [ ] 6.3 实现 macOS backend：经 prototype 选定的原生机制、目录/network/credential/process 限制和 cleanup
- [ ] 6.4 实现 Linux backend：经 prototype 选定的 namespace/seccomp/文件/network/process 组合和 capability probe
- [ ] 6.5 所有 backend 在 prepare 后运行 capability probe；required 不满足则 fail closed
- [ ] 6.6 建立三平台 black-box escape suite：父目录、symlink、hardlink、loopback/private/public network、env secret、child escape、orphan process

## 7. 脱敏、保留与清理

- [ ] 7.1 建立 streaming secret redactor，覆盖 keyring 已知 secrets、常见 credential shape 和 deny-list 路径内容
- [ ] 7.2 在 journal/blob/context/hook/MCP/backend stderr/crash log 写盘边界统一调用 redactor
- [ ] 7.3 实现按天数和磁盘预算的 retention；先清 blob/可重取内容，再 snapshot，保留摘要与 AcceptanceReport
- [ ] 7.4 增加设置与磁盘使用 UI；清理任务必须可取消且不破坏 journal 引用完整性

## 8. 验证与收尾

- [ ] 8.1 运行 journal fault injection、checkpoint 冲突、worktree 初始化/合入/清理和三平台 escape suite
- [ ] 8.2 对 direct/worktree/sandboxed 与外部后端生成用户可见 capability matrix
- [ ] 8.3 运行相关 crate tests、三平台 smoke、clippy、存储迁移和 NFR IO/启动/体积回归
- [ ] 8.4 更新安全文档，准确说明审批、worktree、sandbox、checkpoint 与远程副作用边界
- [ ] 8.5 全部退出场景通过后记录偏差并严格验证本 change

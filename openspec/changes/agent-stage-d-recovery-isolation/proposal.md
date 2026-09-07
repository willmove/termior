## Why

M8 之后 Agent 已能长时间运行并连接外部工具，但应用重启仍不能可靠恢复中间执行位置；当前文件工具和普通 shell 子进程的安全边界不同，worktree、审批和实际 sandbox 容易被用户混淆。对于真正长期和高自主任务，必须先解决“刚才到底执行成功了吗”“能恢复什么”“代码和机器实际被隔离到哪里”。

M9 建立追加式任务事件、显式 unknown 恢复、范围准确的检查点、独立 worktree 和平台能力驱动的执行隔离。

## What Changes

- 将任务事件写为带序号、schema version 和校验信息的追加日志，并周期性生成可重建快照
- 启动时把中断的 running 动作恢复为 unknown，禁止自动重放副作用
- 建立变更集/文件检查点，恢复前检测用户后续修改并预览影响范围
- 为写任务提供独立 Git worktree 生命周期与安全合入流程
- 将执行环境标准化为 direct/worktree/sandboxed，持续显示真实能力
- 为 sandboxed profile 建立三平台文件、网络、凭据和进程限制接口；缺失能力时 fail closed
- 对持久任务内容、输出和诊断统一做 secret redaction 与保留/清理

## Capabilities

### New Capabilities

- `agent-recovery-isolation`：持久事件恢复、检查点、worktree、执行环境、平台 sandbox 和安全透明度

### Modified Capabilities

- 无；本 change 细化 FR-ACHG-04–07 与 FR-ASBX。

## Impact

- `termior-store`：task index、append-only journal、snapshot、checkpoint 与 retention
- `termior-ai`：reducer/recovery、unknown resolution 与 retry policy
- `termior-security` / `termior-platform`：execution environment policy、sandbox backend、secret redaction
- `termior-vcs`：worktree create/status/merge/remove 和冲突检查
- `termior-agent-host`：外部后端权限/隔离声明与恢复映射
- UI：恢复中心、检查点预览、执行环境 badge 和能力诊断

## Dependencies

- Blocked by `agent-stage-c-context-ecosystem`。
- sandbox 技术选型必须逐平台做 prototype 和逃逸测试；规格允许平台能力不同，但禁止静默降级。

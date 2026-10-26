# Stage E 验证指南：并行编排与自动化

Stage E 提供普通 ChildTask、DAG、集中式 slot 调度、聚合预算、取消传播、snapshot freshness、写租约、串行 worktree integration queue，以及版本化本地自动化。

应用关闭、权限与预算、重试边界见 [用户说明](../user/agent-runtime.md)。通知路由将 Started/Working 更新保留在铃铛中，不向前台隐藏面板或后台窗口发进度弹窗。

## 自动化验证

```powershell
cargo test -p termior-ai --test orchestration_automation
cargo test -p termior-ai --test runtime_reliability
cargo test -p termior-platform notification
cargo test -p termior-vcs --test worktree_environment
```

编排测试会实际启动有界 worker thread，验证两个独立只读子任务并行执行，依赖节点仅在二者的结果通过 evidence/snapshot/acceptance 校验后运行。其余场景覆盖：

- 工具、预算和派生深度只能收窄；DAG 拒绝环
- global/provider/workspace/environment slot 与 provider backoff
- scheduler snapshot 持久化；重启时 running 任务报告 interrupted，不自动重放
- 父任务取消后未确认停止的 running child 为 unknown
- stale/unknown/无 evidence/未验证写结果不能供父任务消费
- ChildTaskResult 必须匹配 ChildTaskSpec 的封闭 JSON Schema；字段缺失、类型不符或多余字段均拒绝
- direct root 写租约和串行 worktree integration validation gate
- 手动、interval、本地时区 schedule 与 repo event；重复事件 dedupe 和事件风暴 coalesce
- DST spring-forward gap 跳过，fall-back overlap 只运行第一次
- 每次 run 使用不可变 TemplateVersion 并创建独立 task ID 与 journal
- 只有已知、幂等、transient 失败可自动重试；unknown effect 转 waiting-user
- 非 Git 项目申请写 worktree 会失败，原文件不变且不隐式初始化 Git；目标父路径不可用时保留已有文件

最后一项是可重复的创建失败用例，不等于真实磁盘耗尽验收；真实磁盘不足与清理恢复仍属于 3.6 未完成部分。

## 可自动复验的场景

2026-10-26 的时钟与持久化重建测试已覆盖停机 catch-up、去重、不可变版本和不安全重试；无需用户真实等待每一种计划时间。真实后台进程还验证取消不影响另一 task，并补齐 Windows owner 崩溃清理。真实公共模型评测、磁盘耗尽和三平台长期 soak 仍保留，短 smoke 不冒充这些门禁。详见 [自动执行记录](agent-acceptance-2026-10-26.md)。

1. 建立两个无依赖只读 child 和一个依赖二者的汇总 child。任务图中前两项应同时 Running，汇总项保持 Queued；任一前置失败时汇总应 Blocked。
2. 把 global 或 provider concurrency 调为 1。第二项应显示对应 queue reason，第一项结束后才能运行。
3. 运行两个写 child。它们应使用不同 worktree；integration queue 一次只处理一个，并在 validation 未通过时拒绝接受。
4. 创建 interval 或本地时区 automation，触发两次相同 dedupe key。每次触发都有不同 run/task ID，但第二次状态为 Duplicate，不执行副作用。
5. 在 automation 运行后修改模板。已运行 task 的 backend、权限、预算和模板版本不得变化。
6. 关闭 Termior 一段时间再启动。CatchUpPolicy 分别验证 Skip、RunOnce、RunEach maximum；应用关闭期间没有后台守护进程，只有重启后的本地 catch-up。
7. 点击 Composer 标题栏的 `Automations N`，核对 enabled、Trigger、next run、last run 和不可变 template version。点击 `Queue manual run` 后应生成独立 run ID、task ID 和 journal。再点击 `Task tree N`：DAG 列出 child 的 backend、environment、依赖、状态、预算、验证、blockers、queue reason 和可运行条件，并发上限可加减。取消单个子任务或 `Cancel all` 后记录仍在；带 unknown 副作用的记录没有删除按钮。未知 token/cost/time/output 显示为 unknown，不显示为 0。

评测报告中的未知 token/cost 保持 `null`，不会显示为 0。是否值得并行由完成率、验证数、墙钟、冲突和重复副作用共同判断，子任务数量本身不算质量提升。

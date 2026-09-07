## Context

已有 `AgentDefinition` 与 `run_subagent` 能用工具子集跑一次独立 Agent，但结果只是一段 answer/messages。M10 不把它直接扩大为任意递归调用，而是复用持久 TaskRuntime：子代理就是带父关系、输入/输出契约和更窄能力的任务。

## Goals / Non-Goals

**Goals:**

- 并行只发生在边界明确、预算允许、执行环境不冲突的任务之间
- 父任务能观察、取消、验证和合并所有子任务
- 自动化运行与交互式任务使用同一安全、恢复和审计路径
- 成本、并发和失败不会因递归派生失控

**Non-Goals:**

- 不实现自治 Agent 团队聊天、无限递归或动态购买模型额度
- 不允许两个写任务共享同一工作目录同时修改
- 不做云端调度、远程 worker 或应用关闭时保证运行
- 不自动发布、部署、合并 PR 或联系外部人员，除非未来任务获得明确授权和专门工具契约

## Decisions

### D1：子代理是 Task，不是隐藏函数调用

`ChildTaskSpec` 包含 parent ID、目标、输入 Context refs、expected output schema、Agent backend、tools、budget、environment requirement、dependencies 和 max depth。创建后获得普通 Task ID，事件进入自己的 journal，父任务只接收状态与最终 `ChildTaskResult` 引用。

默认最大深度 2；子任务不能自动提高工具、预算、网络或环境权限。父任务取消时默认传播到未脱离的子任务；已经产生 unknown 副作用的子任务仍保留供检查。

### D2：DAG Scheduler 负责依赖和资源

任务节点状态 `blocked/ready/running/waiting/terminal`，边只允许引用同一 orchestration root 内已有 task，创建时检查环。Scheduler 同时应用：全局并发、Provider 并发/速率、工作区读写锁、sandbox slot、模型费用和父任务预算。

公平策略按 ready 时间加权，交互式任务高于自动化，但不能永久饿死后台任务。配额不足时保持 queued 并显示具体资源，不让 Agent通过反复 spawn 绕过限制。

### D3：读任务共享快照，写任务独占环境

只读子任务共享父任务创建时的 immutable project snapshot/version；检测到文件版本变化时结果标 stale。写子任务默认创建独立 worktree；非 Git 项目使用独立复制环境或拒绝并行写，具体由 M9 ExecutionEnvironment 能力决定。

同一 direct root 的写租约互斥。文件 ownership 只能进一步收窄冲突检查，不能替代目录隔离作为默认路径。

### D4：结构化结果必须由父任务验证

ChildTaskResult 含 output schema data、evidence refs、change set、AcceptanceReport、cost 和 stale/unknown flags。父任务在使用前校验 schema、快照版本、变更基线和验证证据。只读结论可作为 Context Item，但标明来源；写结果必须经过 worktree 集成流程。

### D5：自动化只是 Task factory

Automation 保存 ID、name、enabled、trigger、task template、backend/profile、environment、budget、concurrency policy、retry policy、notification policy 和 last/next run。trigger 初版为 manual、local schedule 和明确支持的本地 repo event。每次触发先产生 dedupe key，再创建独立 task/run ID。

应用未运行时不承诺执行。重启后的 schedule 使用用户选择的 catch-up 策略：skip、run-once 或 run-each（默认 run-once 且有上限）。所有自动化默认不能弹阻塞审批；遇到需要审批时任务进入 waiting-user 并按通知策略提醒。

### D6：重试依据幂等性和已知结果

只允许对尚未产生副作用且被分类为 transient 的失败自动重试。任何 external/process/local-write 的 unknown、Hook/MCP 结果不确定或非幂等工具失败都进入 waiting-user。重试产生新 run/attempt ID，保留原失败。

### D7：预算自上而下保留

父任务在派生时为子任务预留 max tokens/cost/time/tool output；子任务实际用量回滚到父聚合。未用预算可归还，不能借用未授权额度。用户可查看各 backend/child 的累计与剩余；Provider 不报告价格时显示 token/unknown cost。

### D8：通知只在可行动变化时产生

并行任务的 working 事件聚合，不逐条发系统通知。只在全部完成、失败、等待审批/冲突、预算耗尽或自动化需要处理时通知。应用内任务树持续展示细粒度进度。

## Risks / Trade-offs

- 自动拆分质量依赖模型：先支持显式/计划内 ChildTaskSpec，再逐步允许模型建议拆分
- worktree 数量和依赖安装占磁盘：并发上限、共享缓存、清理策略和磁盘预算
- Provider rate limit 会造成队列抖动：集中 scheduler backoff，不让每个 Agent 自己重试
- 本地 schedule 在应用关闭时不运行：UI 明确 next eligible run 和 catch-up，不伪装后台云服务
- 多任务指标可能鼓励无意义并行：评测以完成率/总成本/冲突率为主，不以子代理数量为目标

## Migration Plan

1. 把旧 `run_subagent` 适配为创建单个前台 ChildTask，默认并发 1
2. 加 DAG/scheduler 与只读并行，跑评测确认结果一致
3. 接入 M9 worktree 开放写任务并行和合并队列
4. 增加 Automation manual trigger，再加入本地 schedule/repo event
5. 稳定 retry/dedupe/catch-up 后才允许无人值守启用

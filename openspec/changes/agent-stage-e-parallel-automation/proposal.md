## Why

M9 之后单个任务可以可靠执行、恢复和隔离，才具备安全并行与无人值守运行的基础。当前 `run_subagent` 只是一个未接入主流程的函数，没有父子任务、并发预算、依赖图、工作目录所有权或结果合并；也没有持久的本地计划任务。直接开放“多 Agent”会造成冲突写入、成本失控和重复副作用。

M10 将子代理改造为受控的子任务 DAG，并在同一任务系统上增加本地自动化。每个运行都有明确预算、执行环境、结果和审计记录。

## What Changes

- 主任务可创建窄目标、窄工具、窄预算和结构化输出的子任务
- 用依赖 DAG、全局/Provider/工作区并发上限和公平队列调度任务
- 只读子任务共享不可变项目快照，写子任务默认使用独立 worktree
- 父任务在合并前验证子任务输出、基线和冲突，不把文字汇报当作完成证据
- 新增本地自动化规则：手动、计划时间和受控本地事件触发，每次运行创建独立任务
- 定义 catch-up、重试、去重、通知、费用和并发策略；非幂等未知结果不自动重试
- 扩展固定评测为单代理/多任务/自动化三套本地质量报告

## Capabilities

### New Capabilities

- `agent-orchestration-automation`：父子任务、DAG 调度、并发/预算、worktree 合并、本地自动化和质量评测

### Modified Capabilities

- 无；本 change 细化 FR-AORCH-01–06，并扩展 FR-AORCH-07。

## Impact

- `termior-ai`：orchestrator、scheduler、task contracts、result validation 和 aggregate budgets
- `termior-store`：task relations、automation definitions/runs、dedupe keys 与 schedule state
- `termior-vcs`：并行 worktree 分配和合并队列
- `termior-platform`：本地唤醒/通知能力；不增加云服务
- UI：任务树/DAG、资源视图、自动化管理和运行历史

## Dependencies

- Blocked by `agent-stage-d-recovery-isolation`。
- 本阶段只允许本地执行；远程主机、托管队列与跨设备协作属于后续独立产品决策。

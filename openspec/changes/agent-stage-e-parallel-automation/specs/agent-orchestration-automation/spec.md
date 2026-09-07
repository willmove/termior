## ADDED Requirements

### Requirement: 子任务具有窄契约和独立身份

父任务派生子任务时 SHALL 指定目标、输入引用、输出 schema、工具子集、预算、执行环境、依赖和最大深度。子任务 SHALL 获得独立 Task ID 和 journal。

#### Scenario: 子任务请求额外工具

- **WHEN** 子任务模型尝试调用未在 ChildTaskSpec 中授权的工具
- **THEN** 调用被策略拒绝，不能借用父任务的完整工具集

#### Scenario: 超过派生深度

- **WHEN** depth 已达到配置上限的子任务尝试继续派生
- **THEN** spawn 被拒绝并返回结构化限制原因

### Requirement: DAG 无环且只运行 ready 节点

Orchestrator SHALL 拒绝产生环的依赖，并只在所有前置任务满足成功条件后把节点置为 ready。

#### Scenario: 前置任务失败

- **WHEN** 实现子任务依赖的研究子任务失败
- **THEN** 实现任务保持 blocked，并显示 blocker；系统不自动把失败当作空结果继续

### Requirement: 并发受集中资源预算控制

Scheduler SHALL 同时遵守全局、Provider、工作区、环境和父任务预算。Agent 不能通过创建更多子任务绕过限制。

#### Scenario: Provider 达到并发上限

- **WHEN** 同 Provider 已有允许数量的 running Turn
- **THEN** 新 ready task 保持 queued 并显示 provider-slot，不自行启动或忙等

### Requirement: 并行写入默认隔离

写子任务 SHALL 默认使用独立 worktree 或等价的已验证独立环境。两个任务 MUST NOT 同时持有同一 direct root 的写租约。

#### Scenario: 非 Git 项目没有复制环境

- **WHEN** 两个写任务请求并行且平台无法创建独立环境
- **THEN** Scheduler 串行执行或拒绝第二个任务，绝不在同一目录并发写

### Requirement: 子任务结果有证据和新鲜度

ChildTaskResult SHALL 包含 schema 数据、证据、快照版本、变更集、验收、用量和 unknown/stale 标志。父任务使用前 SHALL 校验。

#### Scenario: 只读结果已过期

- **WHEN** 子任务研究后相关文件版本发生变化
- **THEN** 结果标 stale；父任务必须刷新或明确接受旧证据，不能无提示使用

#### Scenario: 写子任务只报告“完成”

- **WHEN** 子任务返回文字成功但无变更集或 AcceptanceReport
- **THEN** 父任务不能把对应步骤标为 verified completed

### Requirement: 父任务取消传播且保留未知状态

取消父任务 SHALL 取消所有未脱离的 queued/running 子任务，但 MUST 保留无法确认终止的 unknown 调用供检查。

#### Scenario: 一个子任务终止未确认

- **WHEN** 父任务取消后某外部子任务未返回 interrupt 结果
- **THEN** 该子任务为 unknown，父任务不能宣称全部取消成功

### Requirement: 自动化每次运行创建独立任务

Automation SHALL 是持久的任务模板和触发规则；每次触发创建唯一 run ID、Task ID 和 dedupe key，不在旧对话上直接续写。

#### Scenario: 同一事件重复送达

- **WHEN** 相同 repo event 在去重窗口内送达两次
- **THEN** 只创建一个运行，第二次记录为 duplicate

### Requirement: 本地计划任务明确离线语义

应用关闭时本地自动化不承诺运行。重启后系统 SHALL 按 skip/run-once/run-each catch-up 策略处理遗漏时间点，并应用上限。

#### Scenario: 默认补跑多个遗漏周期

- **WHEN** 默认 run-once 自动化在关闭期间遗漏五次
- **THEN** 重启后最多创建一次 catch-up run，并把其余标为 coalesced

### Requirement: 自动重试受幂等性限制

只有无副作用或明确幂等、结果已知且失败分类 transient 的运行才可自动重试。unknown 或非幂等副作用 SHALL 等待用户。

#### Scenario: 部署结果未知

- **WHEN** 外部部署工具断线且无法查询结果
- **THEN** Automation 进入 waiting-user 并通知，不自动再次部署

### Requirement: 自动化权限与预算固定

Automation SHALL 固定 backend、Agent profile、execution environment、最大并发、时间和模型预算。模板更新只影响新运行。

#### Scenario: 运行期间修改模板

- **WHEN** 用户提高自动化预算
- **THEN** 已创建运行保持原预算，新触发运行使用新版本并记录 template version

### Requirement: 通知聚合为可行动状态

并行与自动化 SHALL 只为完成、失败、等待用户、冲突或预算耗尽等可行动变化发送系统通知。细粒度 working 事件只更新应用内视图。

#### Scenario: 十个子任务开始工作

- **WHEN** 十个子任务在短时间内从 queued 变 running
- **THEN** 系统不发送十条通知，任务树更新并可显示聚合 working 状态

### Requirement: 多任务评测报告整体效率

本地评测 SHALL 同时报告完成率、总验证结果、总用量、墙钟时间、冲突率、重复副作用、人工干预和恢复结果。

#### Scenario: 更多子代理但成本更高

- **WHEN** 并行方案与单代理完成率相同但成本和冲突显著更高
- **THEN** 报告如实呈现退化，不把子任务数量计为质量提升

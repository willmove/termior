## 0. 阶段门禁

- [ ] 0.1 确认 M9 已归档，journal/recovery/checkpoint/worktree/sandbox 能力矩阵均通过
- [ ] 0.2 记录单代理固定评测基线，作为并行和自动化比较组

## 1. 子任务领域模型

- [ ] 1.1 定义 ChildTaskSpec/Result、TaskRelation、OutputSchema、SnapshotVersion、Depth 和 aggregate budget
- [ ] 1.2 将现有 `run_subagent` 改为创建普通 ChildTask；先以 concurrency=1 保持行为可控
- [ ] 1.3 强制子任务工具/预算/网络/环境权限只能等于或窄于父任务
- [ ] 1.4 实现父子事件引用、状态聚合、取消传播与 unknown 保留
- [ ] 1.5 对 ChildTaskResult 做 schema/evidence/staleness/acceptance 校验

## 2. DAG 与 Scheduler

- [ ] 2.1 定义 TaskGraph、NodeState、DependencyCondition 和环检测纯函数
- [ ] 2.2 实现持久 ready queue 和公平排序，应用关闭/重启后从 journal 重建
- [ ] 2.3 实现 global/provider/workspace/environment slots 与父任务 token/time/cost/output 预留
- [ ] 2.4 集中处理 Provider rate limit/backoff，禁止各子任务叠加无界重试
- [ ] 2.5 增加 queued reason、预计可运行条件和用户调优并发上限 UI

## 3. 读写隔离与结果合并

- [ ] 3.1 创建 immutable project snapshot/version，所有只读子任务输入引用同一版本
- [ ] 3.2 文件变化时标记依赖结果 stale，并允许刷新为新 child attempt
- [ ] 3.3 写子任务默认向 M9 WorktreeEnvironment 申请独立目录；同 direct root 写租约互斥
- [ ] 3.4 实现 worktree integration queue，逐个做 base/diff/validation/conflict 检查
- [ ] 3.5 父任务只在 ChildTaskResult 校验后消费结论或合并变更
- [ ] 3.6 覆盖非 Git 项目、磁盘不足、worktree 冲突、依赖变化和部分子任务失败

## 4. 任务树与计划接入

- [ ] 4.1 把 PlanStepKind::Subagent 映射为 ChildTaskSpec，spawn 前保留用户拒绝入口
- [ ] 4.2 增加任务树/DAG UI，显示后端、环境、依赖、状态、预算、验证和 blockers
- [ ] 4.3 支持查看/取消单个子任务和聚合取消；不得从 UI 删除尚有 unknown 副作用的记录
- [ ] 4.4 限制默认派生深度为 2，提供循环/爆炸性 spawn 的红队测试

## 5. Automation 模型与触发

- [ ] 5.1 定义 Automation/TemplateVersion/Trigger/Run/Dedupe/Retry/CatchUp/NotificationPolicy 并持久化迁移
- [ ] 5.2 实现 manual trigger，确认每次运行产生独立 Task/Run/journal
- [ ] 5.3 实现本地 schedule 计算、时区/DST 处理和 skip/run-once/run-each 上限
- [ ] 5.4 实现受控本地 repo event trigger、稳定 dedupe key 和事件风暴合并
- [ ] 5.5 实现 retry classifier：只读/幂等 transient 可重试，unknown/非幂等进入 waiting-user
- [ ] 5.6 实现模板版本语义，编辑不会改变运行中 task 的后端/权限/预算
- [ ] 5.7 增加 Automation 管理、next run、last runs、missed/coalesced/duplicate 和磁盘预算 UI

## 6. 通知、预算与审计

- [ ] 6.1 聚合 working 事件，只为 completed/failed/waiting/conflict/budget 发送可行动通知
- [ ] 6.2 自动化等待审批时遵守通知策略，不弹出不可见的模态框或自动批准
- [ ] 6.3 展示 root task 与各 child/backend 的实际 token/cost/time/output，unknown cost 不显示为 0
- [ ] 6.4 审计记录 trigger、template version、dedupe、调度原因、重试决策、审批和合并

## 7. 评测与收尾

- [ ] 7.1 扩展 harness：并行只读研究、独立写 worktree、依赖失败、取消传播、stale result 和合并冲突
- [ ] 7.2 扩展 automation harness：重复事件、漏跑 catch-up、rate limit、unknown 外部副作用和重启恢复
- [ ] 7.3 与 M9 单代理基线比较完成率、验证、总用量、墙钟、冲突、重复副作用和人工干预
- [ ] 7.4 运行相关 crate tests、三平台长期 smoke、clippy、存储迁移、磁盘/内存/进程残留门禁
- [ ] 7.5 更新用户文档，明确本地应用关闭时不运行、权限/预算和非幂等重试边界
- [ ] 7.6 全部退出场景通过后记录偏差并严格验证本 change

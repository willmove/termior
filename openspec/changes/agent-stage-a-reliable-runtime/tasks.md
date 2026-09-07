## 0. 前置确认

- [ ] 0.1 阅读 `docs/termior-spec.md` FR-ARUN / FR-ACHG / FR-AORCH-07、`CONTEXT.md`、ADR 0004/0005 和本 change 全部文档
- [ ] 0.2 记录当前 `termior-ai` 单测、`agent_e2e`、`security_redteam` 与工作树基线；后续实现不得覆盖任务范围外的未提交改动

## 1. 任务与事件领域模型

- [ ] 1.1 在 `termior-ai` 定义稳定 ID newtypes、Task、Turn、TaskState、WaitingReason、TaskCommand 和带 schema version 的 TaskEvent；全部 serde round-trip
- [ ] 1.2 定义并测试任务状态转换表，非法转换返回领域错误且不修改旧状态
- [ ] 1.3 定义 ToolInvocation、ToolState、ToolDecision、ToolAttempt 和未知结果语义；测试每条合法/非法状态边
- [ ] 1.4 定义 AcceptanceCriteria / AcceptanceCheck / AcceptanceReport 及 verified、unverified、failed 的推导纯函数
- [ ] 1.5 给旧 Session 消息提供只读兼容映射；旧 JSON 加载不得伪造运行事件或验证状态

## 2. 严格工具契约

- [ ] 2.1 用 typed schema 构建 `ToolContract`，包含参数 schema、副作用、审批、幂等性、超时、输出上限和并行安全字段
- [ ] 2.2 为现有全部内置工具补封闭 JSON Schema；路径、cwd、command、content、查询与进程参数明确 required 和限制
- [ ] 2.3 执行前验证并规范化参数；无效参数在审批前失败，错误中包含工具名和 JSON path，但不回显 secret 值
- [ ] 2.4 重写 OpenAI / Anthropic / Google 工具序列化，从同一 `ToolContract` 生成并加入 fixture 测试
- [ ] 2.5 保留自定义 Agent 工具子集语义；未知或未授权工具不能通过 schema 注册表进入运行队列

## 3. TaskRuntime 状态机

- [ ] 3.1 新建不依赖 GPUI 的 `TaskRuntime`，以 command 输入和 event 输出驱动 Provider 与 ToolExecutor
- [ ] 3.2 将一次 assistant response 中的全部 ToolCall 入队，默认按返回顺序串行调度
- [ ] 3.3 实现自动审批、人工审批、Yolo 自动决议和策略硬拒绝四条路径；决议来源进入事件
- [ ] 3.4 重构拒绝语义为对应 call ID 的 ToolResult，不以普通 assistant 文本伪装工具结果
- [ ] 3.5 让累计 model steps 跨审批/审阅继续，达到限制时发出 budget-exhausted waiting reason
- [ ] 3.6 为 Provider 首包前瞬时错误实现最多两次有界重试；收到 delta、tool call 或无法证明无副作用后禁止自动重试
- [ ] 3.7 增加 CancellationToken；取消后清空未开始队列并传播给 Provider 和可取消工具

## 4. 变更集与验证闭环

- [ ] 4.1 将单文件 EditProposal 扩展为 ChangeSet / FileChange，记录基线摘要、候选文本、hunk 和来源 call ID
- [ ] 4.2 应用前重新校验基线；冲突返回结构化结果并保留用户磁盘内容
- [ ] 4.3 将 change review 决议作为 TaskCommand 输入；应用后生成对应 ToolResult 并继续原 Turn
- [ ] 4.4 支持同一 Turn 顺序产生多文件提案并合并到一个变更集；UI 能按文件与 hunk 决议
- [ ] 4.5 将约定的测试/构建命令作为 AcceptanceCheck 调度，记录命令、退出码、输出引用和耗时
- [ ] 4.6 完成判定只读取 AcceptanceReport；没有检查时显示 completed-unverified 及原因

## 5. Composer 接入

- [ ] 5.1 新建 task controller/entity 桥，GPUI 只订阅 TaskEvent 并发送 TaskCommand
- [ ] 5.2 迁移 Composer 的 busy、pending approval、pending edit、plan waiting 到 TaskState/WaitingReason
- [ ] 5.3 增加 Stop 动作和 cancelling/cancelled/unknown 可见状态；已取消审批卡不可再点击
- [ ] 5.4 审批卡展示调用 ID、精确规范化参数、副作用分类和决议来源；敏感字段使用脱敏值
- [ ] 5.5 变更审阅完成后保持任务为 running 并展示验证进度，不提前发送 finished 通知

## 6. 评测与回归

- [ ] 6.1 建立脚本化 Provider + fake executor 的离线 scenario harness 和 JSON report
- [ ] 6.2 覆盖混合三工具调用、审批拒绝、取消、超预算、Provider 瞬时失败、基线冲突和测试失败后修正
- [ ] 6.3 增加“副作用调用恰好一次”断言及 randomized event sequence 状态机测试
- [ ] 6.4 保留并扩展 `security_redteam`：Yolo、取消和参数校验都不能绕过 deny-list/workspace guard
- [ ] 6.5 运行 `cargo test -p termior-ai -p termior-diff -p termior-security`、相关 GUI smoke 与 `cargo clippy --workspace`
- [ ] 6.6 将 M6 指标基线记入本地测试产物说明，不添加遥测或网络上传
- [ ] 6.7 全部场景通过后回写实际实现偏差、勾选任务并用 `openspec validate agent-stage-a-reliable-runtime --strict` 验证

## ADDED Requirements

### Requirement: 任务固定绑定作用域

系统 SHALL 为每个 Agent 任务分配稳定 ID，并在创建时固定其项目目录、Agent 后端和执行环境。活动 Tab、pane 或 shell cwd 的后续变化 MUST NOT 改变这些绑定。

#### Scenario: 切换项目标签不迁移任务

- **WHEN** 在项目 A 创建的任务运行期间，用户切换到项目 B 的 Tab
- **THEN** 该任务的后续文件工具与命令仍以项目 A 的绑定为准，界面能显示任务属于项目 A

### Requirement: 完整保留工具调用队列

系统 SHALL 为一次模型响应中的每个工具调用创建独立队列项，并为其保存稳定 call ID 与生命周期。等待审批 MUST NOT 丢弃同一响应中的其他调用。

#### Scenario: 一个响应包含自动和审批调用

- **WHEN** 模型依次返回 `read_file`、`write_file` 和 `run_command`
- **THEN** 三个调用均进入队列，`read_file` 完成后 `write_file` 等待审阅，`run_command` 保留到前一调用达到终态后再处理

#### Scenario: 已成功调用不因恢复而重复

- **WHEN** 第一个工具成功后第二个工具等待审批，用户随后批准
- **THEN** Runtime 从第二个工具继续，MUST NOT 再次执行第一个工具

### Requirement: 工具状态转换可审计

每个 ToolInvocation SHALL 仅按规格允许的状态边转换，并记录状态时间、决议来源和结果。执行结果无法确认时 SHALL 进入 unknown，而非 succeeded 或 failed。

#### Scenario: 取消运行中的副作用操作

- **WHEN** 工具已提交给操作系统后用户取消，终止确认在超时内未返回
- **THEN** 调用状态为 unknown，任务展示“结果未知”，且恢复时不自动重试

### Requirement: 内置工具使用严格参数契约

每个内置工具 SHALL 使用包含 required、properties、类型约束和 `additionalProperties: false` 的 JSON Schema。相同契约 SHALL 用于 Provider 声明和执行前校验。

#### Scenario: 模型遗漏必填字段

- **WHEN** `write_file` 调用没有 `path` 或 `content`
- **THEN** 调用在进入审批前失败并返回结构化校验错误，磁盘和审批队列不发生变化

#### Scenario: 不同 Provider 获得等价契约

- **WHEN** 同一工具集合分别序列化给 OpenAI、Anthropic 和 Google Provider
- **THEN** 三种请求表达相同字段、必填项和封闭对象语义

### Requirement: 任务可以安全取消

运行中的任务 SHALL 接受取消命令。取消后系统 MUST NOT 启动尚未执行的模型回合或工具调用，并 SHALL 尝试终止支持取消的活动操作。

#### Scenario: 等待审批时取消

- **WHEN** 任务处于 waiting-approval 且用户取消
- **THEN** 待审批调用及其后调用均不执行，任务进入 cancelled，审批卡不再可批准

### Requirement: 预算跨恢复累计

步数、墙钟时间、模型用量与工具输出预算 SHALL 属于任务并跨审批、审阅和 resume 累计。达到硬限制时系统 SHALL 暂停并说明限制项。

#### Scenario: 审批不重置步数

- **WHEN** 任务在第 8 个模型回合等待审批并随后继续
- **THEN** 下一模型回合计为第 9 步，而非重新从第 1 步计数

### Requirement: 变更审阅属于原工具调用

文件写工具 SHALL 在候选变更产生后进入 waiting-change-review。只有接受项成功落盘后才生成成功 ToolResult，并继续原 Turn。

#### Scenario: 部分接受后继续验证

- **WHEN** 一个变更集有三个 hunk，用户接受两个并拒绝一个
- **THEN** 工具结果准确列出两项 applied 和一项 rejected，Agent 获得该结果并继续执行验收步骤

#### Scenario: 基线已被用户修改

- **WHEN** 审阅期间用户修改了目标文件，使磁盘摘要不同于变更集基线
- **THEN** 应用被暂停并标记 conflict，系统不覆盖用户内容

### Requirement: 完成状态需要验收报告

任务终态 SHALL 引用 AcceptanceReport。全部必需检查通过时为 completed-verified；没有运行全部必需检查时为 completed-unverified，并列出缺失项。

#### Scenario: 模型声称成功但测试失败

- **WHEN** 最终模型消息声称修复完成，而验收命令退出码非零
- **THEN** 任务不得标为 completed-verified，并向 Agent 回填失败证据以继续修正或最终标为 failed

### Requirement: 固定评测可离线复现

仓库 SHALL 提供不依赖真实模型网络的 Agent 评测 harness，以脚本化 Provider 与工具执行器运行固定任务并输出结构化指标。

#### Scenario: CI 运行可靠性基线

- **WHEN** CI 执行 M6 Agent 评测
- **THEN** 报告至少包含完成结果、验证结果、工具调用次数、重复副作用次数、人工决议次数和安全拒绝次数

## Context

当前 `AgentOutcome` 只返回最终消息、一个 `pending_approval` 和本次 `steps`。`resume` 执行获批工具后重新进入循环；同一 assistant message 中审批调用之后的调用没有独立存续状态。Composer 自己持有 `busy`、pending approval、pending edit 和 plan flags，导致任务生命周期与面板 Entity 耦合。`ToolDescriptor` 也只有名称、级别和描述，Provider 侧统一发送开放 object schema。

## Goals / Non-Goals

**Goals:**

- 让一个任务中的每个 ToolCall 恰好进入一次终态或明确的 unknown
- 让取消、审批、变更审阅和验证成为同一个运行时的输入事件
- 让 Provider 差异停留在序列化边界，内核只处理统一事件
- 在引入外部后端前形成可复用的确定性契约测试

**Non-Goals:**

- 本阶段不做重启后的完整事件恢复、OS sandbox、MCP 或并行子代理
- 不自动决定用户应运行哪些项目测试；验收命令来自任务约定、项目规则或 Agent 提议
- 不改变 Yolo 的定义；Yolo 仍只是在审批门自动批准，安全策略不变

## Decisions

### D1：Task Runtime 是唯一执行状态所有者

新增纯 Rust `TaskRuntime`。UI 只提交 `TaskCommand` 并订阅 `TaskEvent`，不直接拼装 resume 流程。

```text
Task
  ├─ immutable: id, project_dir, backend_id, environment_id, created_at
  ├─ mutable: title, goal, acceptance_criteria, state, budgets
  └─ Turn[]
       ├─ user_input
       ├─ RuntimeEvent[]
       └─ ToolInvocation[]
```

任务状态至少包含 `idle / running / waiting-approval / waiting-plan / waiting-change-review / waiting-user / cancelling / completed-verified / completed-unverified / failed / cancelled / unknown`。等待原因必须是带 ID 的结构化引用。

### D2：工具调用是持久队列项

每个 `ToolInvocation` 保存 call ID、工具名、原始和规范化参数、副作用分类、审批决议、开始/结束时间、尝试次数和结果。状态只能按以下方向转换：

```text
proposed → awaiting-approval → approved → running → succeeded | failed | unknown
         └──────────────────→ denied
proposed → approved                (自动工具或 Yolo 门决议)
proposed → denied                  (策略硬拒绝)
```

取消只把未开始调用置为 cancelled；已发给 OS/外部服务的动作在无法确认结果时置为 unknown。Runtime 每次从队首选择可运行项，同一 assistant response 的后续调用不会因前一个等待审批而丢失。默认保持模型给出的顺序；只有工具契约明确 `parallel_safe` 且本阶段显式启用时才可并行，M6 默认全部串行。

### D3：严格工具契约由一个注册表生成

`ToolContract` 包含名称、说明、JSON Schema、side-effect class、approval class、default timeout、max output bytes、idempotency 和 parallel safety。Provider 适配器从同一契约生成 OpenAI/Anthropic/Google 格式；执行前用相同 schema 校验。禁止 `additionalProperties: true` 作为内置工具的最终契约。

副作用分类为 `read / local-write / process / network / external`。审批分类与副作用分类分开，便于后续安全层决策。

### D4：变更集审阅暂停原工具调用

`write_file` 执行到提案阶段时不会返回“成功写入”。调用进入 `waiting-change-review` 并关联 `ChangeSet`。用户作出 hunk 决议后，运行时先检查磁盘基线，应用接受项，再生成真实 `ToolResult`：包含 applied/rejected/conflicted hunks 和文件摘要。然后恢复同一 Turn，让 Agent 执行验收。冲突时保持等待状态，禁止覆盖磁盘。

### D5：取消和预算由运行时强制

`CancellationToken` 贯穿 Provider stream、工具执行和 UI。取消后不再启动新工具；可取消的运行调用收到终止请求。步数按模型回合累计，不因审批 resume 清零。预算同时追踪墙钟时间、模型输入/输出 token（Provider 不报告时为 unknown）和工具输出字节。到达硬限制时进入 `waiting-user`，继续必须由显式命令提高或重置预算。

Provider 只对未收到任何响应、且确认没有触发工具的瞬时故障自动重试，最多两次并使用有界退避；其他错误交给运行时并保留诊断。

### D6：完成判定来自 AcceptanceReport

Agent 结束文字不会直接改变任务终态。运行时生成 `AcceptanceReport`，列出每条验收条件、对应命令/检查、退出码和证据引用。全部必需条件通过才是 `completed-verified`；没有可执行检查或用户选择跳过时是 `completed-unverified`。

### D7：M6 的存储兼容边界

M6 保留现有 SessionStore 用于消息历史，新增 task summary 的 versioned serde 结构，但不承诺从中间事件重启恢复。M9 会把相同 `TaskEvent` 写入追加日志，因此 M6 的事件必须稳定、可序列化并带 schema version，避免二次重构领域模型。

## Risks / Trade-offs

- 运行时重构会触及 Composer 主路径：先用 fake Provider 和 fake executor 钉住状态机，再替换 UI 调用点
- Provider usage 字段并非都可用：允许 unknown，禁止猜测为 0
- 多文件变更集比当前单文件 proposal 复杂：M6 先支持同 Turn 多个顺序提案，共享一个基线时点；跨 Turn 合并留后续
- 验收命令可能有副作用：仍按普通工具契约走审批和执行环境，不设“测试命令天然安全”的捷径

## Migration Plan

1. 新模型与适配层和旧 Agent 并存，用测试完成状态等价性
2. Composer 改订阅 TaskRuntime；迁移后删除 UI 自有的 pending/busy 状态
3. 现有 session JSON 读取后映射为无运行事件的历史任务摘要；写出保持 schema version
4. 默认关闭新的自动验证，只在任务有明确 acceptance command 时启用；完成 UI 稳定后再默认开启

## Implementation Notes

- `TaskRuntime` 已成为 Composer 的语义状态来源；Composer 仍保留一个短生命周期的 `busy` 字段，只表示 GPUI 后台命令正在传递，审批、审阅、预算、取消和完成语义全部读取 `TaskState` / `WaitingReason`。
- 同一 Turn 的多个文件提案在 runtime 中汇总为一个 `ChangeSet`。hunk ID 在该变更集内展平为全局序号，UI 仍按 `FileChange` 保留文件边界；应用前对所有文件做基线预检，任一冲突都会阻止整组落盘。
- 验收命令由任务显式配置，并作为普通 `run_command` 调用进入同一 schema、审批、安全和输出限制链路。失败结果返回模型，后续修正可以再次执行检查；只有 `AcceptanceReport` 能产生 `completed-verified`。
- 评测采用离线脚本 Provider 和 fake/real executor 组合，JSON 指标只写本地。自动化与手工复核步骤记录在 `docs/ai-agent-stage-a-validation.md`。

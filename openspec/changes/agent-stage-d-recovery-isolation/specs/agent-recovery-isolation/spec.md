## ADDED Requirements

### Requirement: 任务事件可重放且可检测损坏

系统 SHALL 将 TaskEvent 以单调序号和校验链追加写入任务日志，并用同一纯 reducer 构建在线与恢复状态。关键副作用边界必须在继续前持久化。

#### Scenario: 尾部写入中崩溃

- **WHEN** `events.jsonl` 最后一条记录只写入一部分
- **THEN** 恢复器忽略该半条尾记录、重放此前有效事件并报告修复，不丢弃整个任务

#### Scenario: 中间记录损坏

- **WHEN** 校验链在日志中间失败
- **THEN** 系统停止在最后可信 sequence，不执行恢复动作，并保留损坏文件供诊断

### Requirement: 未完成副作用恢复为 unknown

启动时没有终态证据的运行中工具、命令或外部请求 SHALL 恢复为 unknown。系统 MUST NOT 自动重放非只读操作。

#### Scenario: 写文件调用开始后崩溃

- **WHEN** journal 有 local-write started 但没有 succeeded/failed 事件
- **THEN** 调用显示 unknown，用户可检查磁盘或放弃；Retry 创建新 attempt 且需要显式决议

### Requirement: 检查点声明精确恢复范围

Checkpoint SHALL 列出覆盖的任务 event sequence、文件及内容 hash，并明确不覆盖的进程与远程副作用。恢复前 SHALL 展示预览。

#### Scenario: 恢复没有后续修改的文件

- **WHEN** 当前文件仍等于检查点记录的 expected-after 内容
- **THEN** 预览可恢复为 before 内容，用户确认后写入并记录新恢复事件

#### Scenario: 检查点后用户修改文件

- **WHEN** 当前文件既不等于 before 也不等于 expected-after
- **THEN** 文件标为 conflict 且默认不恢复，其他无冲突文件可单独决议

### Requirement: Worktree 绑定任务且安全清理

写任务 SHALL 可创建独立 Git worktree 并在成功准备后绑定。合入和删除前 SHALL 检查 dirty 状态、验证结果和冲突。

#### Scenario: 初始化失败

- **WHEN** worktree 创建成功但项目初始化命令失败
- **THEN** 任务不进入 running，环境标为 failed；可安全清理时回滚，否则显示 orphaned path 供用户处理

#### Scenario: 有未提交改动时删除

- **WHEN** worktree 包含未保存改动
- **THEN** 自动清理被拒绝，系统展示路径和 diff 摘要

### Requirement: Worktree 不标为 sandbox

系统 SHALL 明确区分 worktree code isolation 与 OS sandbox。Worktree 模式 MUST 显示其进程仍拥有当前用户系统权限。

#### Scenario: 用户查看 worktree 环境

- **WHEN** 任务在 worktree 中运行
- **THEN** 环境面板显示独立 repo path，同时把文件/网络隔离标为未提供

### Requirement: 要求隔离时禁止静默降级

任务声明 sandboxed 必需能力后，环境 probe 缺少任一必需项 SHALL 阻止执行。切换 direct/worktree 必须是用户显式创建的新环境选择。

#### Scenario: 当前平台不支持网络阻断

- **WHEN** profile 要求网络 blocked 而 backend probe 返回 unsupported
- **THEN** Start 被拒绝并显示缺失能力，不启动普通子进程

### Requirement: Sandbox 能力经过黑盒验证

每个发布的 sandbox profile SHALL 在对应平台通过文件、网络、凭据和进程逃逸测试，并记录验证时间与实现版本。

#### Scenario: 符号链接逃出工作区

- **WHEN** sandboxed 命令通过工作区内符号链接尝试读取授权根外文件
- **THEN** OS 执行边界拒绝访问，而非仅依赖 Agent 工具路径检查

### Requirement: 审批与隔离分别展示

任务 UI SHALL 同时显示审批模式和执行环境能力。批准动作 MUST NOT 改变 sandbox writable paths、network 或 credential mode。

#### Scenario: Yolo 运行于 sandbox

- **WHEN** 用户在 sandboxed 环境选择 Yolo
- **THEN** 人工审批可自动通过，但命令仍受相同文件/网络限制

### Requirement: 外部后端能力标明可信度

外部后端报告的 sandbox/approval 能力 SHALL 标明 reported、verified 或 unknown。未经 Termior probe 的能力 MUST NOT 标为 verified。

#### Scenario: 外部后端自管 shell

- **WHEN** 外部 Agent 不使用 Termior TerminalService 执行命令
- **THEN** UI 显示 backend-managed，Termior ToolRegistry 与 sandbox 列为不适用或 unknown

### Requirement: 持久内容先脱敏并受保留策略约束

任务日志、输出、上下文和诊断 SHALL 在写盘前执行 secret redaction。自动清理 SHALL 优先删除大 blob，保留任务摘要和验收报告。

#### Scenario: 命令输出含 API key

- **WHEN** 输出包含已知 credential 格式
- **THEN** UI 流可按安全策略短暂显示或遮挡，持久 blob 和日志中只保存脱敏标记

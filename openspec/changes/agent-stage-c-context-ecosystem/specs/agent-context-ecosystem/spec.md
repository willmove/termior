## ADDED Requirements

### Requirement: 上下文项可追溯

发送给 Agent 的每份上下文 SHALL 具有来源、采集时间、作用域、版本、敏感级别和保留策略。用户 SHALL 能查看每个 Turn 实际发送和未发送的上下文项及原因。

#### Scenario: 查看一次模型请求的构成

- **WHEN** 用户打开某 Turn 的 Context Inspector
- **THEN** 系统按类别列出 token 估算、来源路径/命令范围、是否内联/摘要/排除及排除原因

### Requirement: 上下文按预算装配

系统 SHALL 在模型硬上限内按固定优先级分配上下文预算，且 MUST 保留安全策略、任务目标、验收条件和未决状态。

#### Scenario: 大日志超过预算

- **WHEN** 终端产生远超预算的输出
- **THEN** 请求只包含相关有界片段、摘要和可回读游标，完整日志不会重复进入后续请求

### Requirement: 压缩不丢失任务不变量

自动压缩 SHALL 从 TaskRuntime 重建目标、验收、用户约束、未决审批、活动变更和未知副作用。连续压缩无法释放空间时 SHALL 暂停。

#### Scenario: 审批前发生压缩

- **WHEN** context 接近上限且任务正在等待一个工具审批
- **THEN** 压缩后的请求仍包含原 call ID、规范化参数、审批原因和之前已成功调用摘要

#### Scenario: 压缩抖动

- **WHEN** 单个不可裁剪输入使连续压缩后立即再次超限
- **THEN** 任务进入 waiting-user 并报告超限来源，不无限请求模型

### Requirement: 目录规则按目标文件生效

系统 SHALL 从工作区根到目标文件目录解析适用的 `AGENTS.md`，记录顺序和来源。项目规则 MUST NOT 扩大安全权限。

#### Scenario: 两个目录规则不同

- **WHEN** 同一任务修改 `frontend/` 与 `backend/` 文件且各有嵌套规则
- **THEN** 每个文件工具上下文只附加其路径适用的规则，共同上下文包含二者交集和根规则

### Requirement: 仓库地图有界且可失效

仓库地图 SHALL 依据文件版本更新符号和引用，并按任务生成有 token 上限的相关切片。解析失败 SHALL 降级而非阻塞任务。

#### Scenario: 用户修改已索引文件

- **WHEN** 文件内容 hash 变化
- **THEN** 旧符号条目标为 stale 并在下次检索前或后台重建，模型不收到标成当前的旧签名

### Requirement: 记忆由用户管理且有依据

永久记忆 SHALL 保存作用域、依据、更新时间和状态；用户可以接受、编辑、拒绝、删除或暂停。秘密内容 MUST NOT 写入记忆。

#### Scenario: Agent 建议保存偏好

- **WHEN** Agent 产生一个记忆候选
- **THEN** 用户能查看其来源并决议；未接受前该候选不进入后续会话上下文

### Requirement: Skills 渐进加载且不授予权限

系统 SHALL 兼容 Agent Skills 目录格式，启动时只索引元数据，激活时加载 SKILL.md，资源按需加载。技能声明的工具 MUST NOT 自动绕过审批或安全策略。

#### Scenario: 未激活的大型技能

- **WHEN** 工作区安装多个包含大量 references 的技能但任务未触发它们
- **THEN** 模型上下文仅包含所需技能元数据，不包含正文或引用文件

### Requirement: MCP 工具经过统一策略入口

MCP server 暴露的工具 SHALL 转换为带 server identity 的 ToolContract，并经过参数校验、审批、超时、输出限制、脱敏与审计。

#### Scenario: 两个 server 提供同名工具

- **WHEN** 两个 MCP server 都声明 `search`
- **THEN** Termior 使用 server-qualified ID 区分，审批卡显示 server 身份且结果回到正确连接

#### Scenario: server 描述要求绕过审批

- **WHEN** MCP 工具描述声称自己应自动执行
- **THEN** Termior 忽略该授权主张，仍按本地策略计算审批

### Requirement: MCP 凭据安全存储

远端 MCP 授权 SHALL 使用规范要求的安全流程，token MUST 存入系统钥匙串且不得进入 settings、任务事件或日志。

#### Scenario: 重新启动远端连接

- **WHEN** 应用重启后恢复 MCP server 配置
- **THEN** settings 只含 server metadata/key reference，token 从钥匙串读取并在日志中脱敏

### Requirement: Hooks 有界且修改后重新校验

Hook SHALL 在声明的生命周期点以有界时间/输出运行。任何 Hook 修改后的工具参数 SHALL 重新通过 schema、workspace、deny-list 与审批策略。

#### Scenario: pre-tool Hook 改变目标路径

- **WHEN** Hook 把写文件路径从已授权目录改到未授权目录
- **THEN** 调用被安全策略拒绝，不能沿用修改前的审批结果

### Requirement: ACP 后端按能力协商

ACP adapter SHALL 只启用初始化握手声明的能力，并通过统一 AgentBackend 事件工作。至少一个真实 ACP Agent SHALL 通过端到端 smoke。

#### Scenario: ACP 后端缺少 session load

- **WHEN** 后端没有声明 loadSession
- **THEN** Termior 禁用 Resume，仍允许创建新会话和提交 prompt

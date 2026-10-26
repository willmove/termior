## Context

当前 `ProviderRequest` 接受完整 messages，附件将文件和选区拼进用户文本；图片仅有占位提示。项目说明按 `Termior.md > CLAUDE.md > AGENTS.md` 选择根目录单文件，没有目录作用域或来源视图。M8 把“上下文是什么、为何出现、何时可丢弃”变成独立模型。

## Goals / Non-Goals

**Goals:**

- 在长任务中保留目标与决策，同时减少重复 token 和大输出污染
- 让用户能追踪任何注入模型的信息来源
- 用标准格式扩展工作方法和工具，所有扩展服从同一策略
- 以 ACP 第二后端验证后端契约的通用性

**Non-Goals:**

- 初版仓库地图不引入向量数据库或云 embedding
- Skills 不能修改 Termior UI；MCP server 不能自行授予权限
- Hooks 不是任意常驻插件或绕过 ToolRuntime 的执行通道
- 不承诺 ACP 的所有可选能力，按实际握手呈现

## Decisions

### D1：Context Item 保留来源，Context Plan 决定本次装配

`ContextItem` 包含 stable ID、kind、source URI、captured_at、scope、content version/hash、sensitivity、retention、estimated tokens 和可重取句柄。内容可以内联，也可以只保留引用。

每次模型调用先生成 `ContextPlan`：为 system/safety、task goal/acceptance、applicable rules、unresolved state、recent evidence、retrieved code、skills 和 optional history 分预算。用户显式附件优先级高，但仍受 secret deny-list 和模型硬上限。Context Inspector 展示 plan、实际发送项、被截断/摘要/排除的原因。

### D2：压缩保留一个结构化 Task Brief

达到软阈值时先删除可通过 cursor/file range 重取的旧 ToolResult 正文，只保留摘要和引用。仍超限时生成 `TaskBrief`：目标、验收、用户约束、已决策事项、已完成动作、活动调用/审批/变更、失败证据和下一步。压缩结果作为新 Context Item，保存其覆盖的 event range 与生成模型/版本。

硬不变量字段由代码从 TaskRuntime 重建，不能只依赖模型摘要。连续压缩仍超限时暂停并报告 context-thrashing，不无限循环。

### D3：规则按文件路径解析

规则层级为 Termior 安全策略、任务显式约束、用户全局指令、工作区根规则、从根到目标文件目录的嵌套 `AGENTS.md`。对多个项目文件的操作分别计算适用规则；共同 prompt 只放交集，文件特定规则附在相应工具上下文。规则来源和更新时间可见。

项目规则不能提升工具权限或取消安全护栏。规则之间语义冲突由模型/用户处理，运行时只确定作用域和顺序，不尝试自然语言合并。

### D4：仓库地图使用已有语法与索引能力

后台索引提取文件、符号、签名、定义/引用边和测试邻近关系，尊重 ignore 与授权根。任务检索结合路径、词法匹配、符号引用和近期变更，用固定 token 预算输出切片。无法解析的语言仍提供文件/grep 结果。

### D5：记忆需要人工可管理

模型只能提出 `MemoryCandidate`，包含内容、作用域、依据 Context/Event 引用、置信说明和过期提示。默认需用户接受后才持久化；设置可允许低风险偏好自动接受，但 secret scanner 和来源要求不可关闭。用户可编辑、删除、暂停注入，过期来源会标警告。

### D6：Skills 渐进加载

扫描用户级和项目 `.agents/skills/<name>/SKILL.md`，启动只加载 name/description/compatibility。触发后读取正文，引用的 references/scripts/assets 按需访问。`allowed-tools` 视为技能请求的上限提示，不自动授权。项目 skill 必须位于授权根并受 deny-list。

### D7：MCP 是外部工具来源

每个 MCP server 独立连接，支持 stdio 与 Streamable HTTP、能力发现、tools/resources/prompts 列表和变更通知。远端授权使用 OAuth/PKCE 等协议要求，token 在 keyring。MCP 工具被转换为 `ToolContract`，同时附 server identity、版本和信任状态；参数校验、审批、超时、输出限制、secret redaction 与审计仍由 Termior 入口执行。

同名工具以 server-qualified ID 区分。server 提示或工具描述是不受信任上下文，不能覆盖系统安全策略。连接失败隔离到单 server。

### D8：Hooks 是有界生命周期函数

支持 pre/post task、pre/post tool、post change apply、pre/post verification。Hook 输入输出为 versioned JSON，默认超时 5 秒，有 stdout 上限和无交互环境。pre hook 可 allow/deny/replace-safe-fields；任何参数修改均重新走 schema 与策略。Hook 失败策略按配置 fail-closed 或 warn，并显示来源。

### D9：ACP 验证第二个后端

ACP adapter 通过 stdio JSON-RPC 能力协商创建/加载会话、prompt/cancel、权限与宿主 terminal/filesystem 接口。至少选择 Gemini CLI 或 OpenCode 做真实 smoke，另用 fake ACP server 覆盖协议。ACP 与 Codex 专用字段只能留在 adapter 内；共同 UI 仅依赖 CapabilityProfile。

## Risks / Trade-offs

- 自动摘要可能遗漏：关键状态由代码重建，摘要只处理叙述性历史
- 嵌套规则增加 token：按目标文件解析并去重，不把整棵目录规则全量注入
- MCP 扩大攻击面：server-qualified identity、最小权限、输出限制和描述不可信是硬要求
- tree-sitter 语言支持不完整：降级到路径/grep，不阻塞任务
- 不同 ACP 后端实现子集不同：fake contract + 第二真实 backend，缺省能力关闭

## Migration Plan

1. ContextAssembler 先包住现有消息/附件输入，Inspector 以只读方式显示
2. 加预算和工具结果引用，验证请求语义后再启用自动 compaction
3. 加规则解析与 repo map，替换旧单 ProjectMemory 注入
4. Skills、Hooks、MCP 按独立 feature 依次启用
5. 最后加入 ACP adapter，必须通过与 Codex/内置相同的公共契约测试

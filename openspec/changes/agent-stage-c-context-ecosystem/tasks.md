## 0. 阶段门禁

- [ ] 0.1 确认 M7 已归档，内置与 Codex 后端通过相同 Task/Turn contract suite
- [ ] 0.2 固定实现时采用的 Agent Skills、MCP 与 ACP 协议版本，重新核验安全/实验标记

## 1. Context Item 与检查器

- [ ] 1.1 定义 ContextItem/Source/Scope/Sensitivity/Retention/ContentRef 与 ContextPlan，全部可序列化并可脱敏显示
- [ ] 1.2 实现文件 range、命令 output range、terminal selection、task event range 和 inline attachment 的可重取句柄
- [ ] 1.3 实现 token estimator 接口；Provider 精确 tokenizer 不可用时标记 estimated，不伪装精确值
- [ ] 1.4 建立 ContextAssembler 的类别预算和确定性优先级，覆盖硬上限和用户附件超限
- [ ] 1.5 在 Composer/任务详情增加 Context Inspector，展示实际发送、摘要、截断、排除和来源

## 2. 压缩与长会话

- [ ] 2.1 将旧 ToolResult 正文替换为摘要+内容引用，验证后续可按游标/文件范围重取
- [ ] 2.2 定义 TaskBrief schema，由代码注入不可丢字段，模型只生成叙述性摘要
- [ ] 2.3 实现软阈值 compaction、覆盖 event range、摘要版本和 context-thrashing 熔断
- [ ] 2.4 测试审批、变更审阅、unknown 副作用、失败证据和用户约束跨压缩保留

## 3. 规则、仓库地图与记忆

- [ ] 3.1 实现全局/根/嵌套 AGENTS 规则发现、路径作用域和来源排序，替换单文件 ProjectMemory 注入
- [ ] 3.2 对同 Turn 多文件分别计算规则；共同 prompt 去重，文件特定规则靠近工具上下文
- [ ] 3.3 基于现有 tree-sitter/文件索引建立增量 symbol/reference map，尊重 ignore、授权和文件 hash
- [ ] 3.4 实现路径+词法+引用关系的有界相关切片；无 parser 语言降级到文件/grep
- [ ] 3.5 定义 MemoryCandidate/MemoryEntry 和 review UI，含来源、编辑、删除、暂停与过期状态
- [ ] 3.6 在持久化前对记忆运行 deny-list/secret scanner；添加投毒和过期来源测试

## 4. Agent Skills

- [ ] 4.1 扫描用户级与项目 `.agents/skills`，校验 SKILL.md frontmatter、名称和授权路径
- [ ] 4.2 启动仅索引元数据，激活时加载正文，references/scripts/assets 按需读取并记录 Context Item
- [ ] 4.3 将 allowed-tools 视为技能工具上限提示并与 Agent/任务工具权限取交集，不能自动批准
- [ ] 4.4 增加技能状态、解析错误、来源和本 Turn 激活记录 UI

## 5. MCP client

- [ ] 5.1 选择许可兼容的 Rust MCP 实现或写最小协议层，记录依赖体积/启动/RSS 影响
- [ ] 5.2 实现 stdio server 进程管理、初始化、tools/resources/prompts discovery 和连接隔离
- [ ] 5.3 实现 Streamable HTTP、授权发现、PKCE/resource binding、token refresh 和 keyring 存储
- [ ] 5.4 将 MCP tools 映射为 server-qualified ToolContract，接入 schema/审批/超时/输出/脱敏/审计
- [ ] 5.5 处理 list-changed 通知、server 断开、同名工具和不可信描述；单 server 故障不拖垮任务
- [ ] 5.6 建立恶意 fake MCP server 红队：工具投毒、超大输出、路径逃逸、token 泄漏和断线重连

## 6. Hooks

- [ ] 6.1 定义 versioned HookEvent/HookDecision 和支持的生命周期点
- [ ] 6.2 实现无交互子进程 runner、默认 5 秒超时、输出上限、fail-closed/warn 策略与诊断
- [ ] 6.3 pre-tool 修改后重新执行 schema、授权、deny-list 和审批计算；旧决议作废
- [ ] 6.4 添加 Hook 管理与最近运行记录 UI，隐藏敏感字段

## 7. ACP 第二后端

- [ ] 7.1 在 `termior-agent-host` 实现 ACP initialize/capability negotiation 和 stdio transport adapter
- [ ] 7.2 映射 new/load session、prompt、cancel、permission request、tool event 到统一模型
- [ ] 7.3 实现 ACP client filesystem/terminal 回调时复用 Termior 授权与 TerminalService
- [ ] 7.4 以 fake ACP server 跑完整 contract suite，覆盖缺失 optional capabilities
- [ ] 7.5 选择 Gemini CLI 或 OpenCode 做真实端到端 smoke，并记录另一后端未覆盖的功能差异

## 8. 验证与收尾

- [ ] 8.1 运行长日志/长会话压缩、嵌套规则、repo map 失效、Skills 渐进加载、MCP 红队和 Hook 重校验场景
- [ ] 8.2 三个后端运行公共任务集，报告能力矩阵与不支持项
- [ ] 8.3 运行相关 crate tests、三平台 smoke、clippy 与 NFR 体积/内存/上下文延迟门禁
- [ ] 8.4 更新用户文档：扩展来源、权限含义、凭据位置、故障诊断与卸载
- [ ] 8.5 全部退出场景通过后记录偏差并严格验证本 change

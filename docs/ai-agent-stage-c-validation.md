# Stage C 验证指南：上下文与扩展生态

Stage C 提供有来源的上下文计划、可重取的大输出、结构化压缩、分层规则、Skills 渐进加载、可审阅记忆、MCP、Hooks 和 ACP。

## 自动化验证

```powershell
cargo test -p termior-ai --test context_ecosystem
cargo test -p termior-hooks
cargo test -p termior-agent-host --test ecosystem_contract
cargo test -p termior-ai --test security_redteam
```

这些测试覆盖：

- 类别 token 预算、required item 超限和 estimated token 标记
- 文件/终端输出引用的重取、SHA-256 完整性检查和大工具输出外置
- `TaskBrief` 压缩与连续压缩熔断，审批和 unknown 状态不丢失
- 根与嵌套 `AGENTS.md` 作用域、仓库 symbol/reference map、用户级/项目级 Skills 覆盖和按需资源读取
- 记忆的接受、编辑、暂停、删除、过期和 secret 拒绝
- MCP stdio 初始化、tools/resources/prompts discovery、server-qualified 工具、封闭 schema、实际调用、输出上限和单服务故障隔离
- Hook 真实子进程、超时/输出上限、修改参数后的 schema/路径/deny-list/审批重算
- ACP initialize/session/prompt、流式事件、权限事件，以及 prompt 尚在运行时处理宿主回调的双向协议

## 手工验证

1. 在项目根和子目录各放一份 `AGENTS.md`，要求 Agent 修改子目录文件。Context Inspector 应显示两份来源；修改根文件时不应注入子目录规则。
   发送消息后点击 Composer 工具栏的 `Context …t · … items`，逐条核对 category、token、Included/Summarized/Truncated/Excluded 和原因。
2. 运行产生大输出的工具。聊天历史只应保留摘要和 artifact 引用；引用仍可重取原输出，篡改 artifact 后读取必须报完整性错误。
3. 配置一个测试 Hook，让它把 `write_file` 的路径从 `a.txt` 改为 `b.txt`。审批卡必须显示并批准 `b.txt`；旧参数的审批不能沿用。
4. 连接两个都声明 `search` 的 MCP server。工具名应显示为 `mcp::<server>::search`，一个 server 断开不能移除另一个。
5. 启动 ACP fake/兼容 Agent。没有实际提供的 optional capability 必须显示 unsupported；文件或终端宿主回调只能经配置的 `AcpClientHandler` 执行。
6. 在用户级或项目 `.agents/skills/<name>/SKILL.md` 放置测试 Skill。Composer 标题栏应显示 `Skills N` 和来源/allowed-tools；在消息中显式写 `$<name>` 后，该 Skill 应显示 `active this turn`，正文才会进入 Context Inspector，工具上限只能与任务工具取交集。
7. 在 `agent-memory.json` 放入带证据的 candidate。`Memory N` 面板应支持 Accept/Reject；已接受条目支持 Pause/Resume/Delete，并在写盘前拒绝 secret 形态。

MCP 的远端端点只接受 HTTPS（localhost 例外），HTTP 重定向默认禁用；OAuth 回调同时校验 state 与 resource binding。凭据引用只保存 key reference，不能进入上下文或日志。

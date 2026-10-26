# Stage C 验证指南：上下文与扩展生态

Stage C 提供有来源的上下文计划、可重取的大输出、结构化压缩、分层规则、Skills 渐进加载、可审阅记忆、MCP、Hooks 和 ACP。

## 协议兼容范围

| 协议或格式 | 实现固定值 | 验证边界 |
|---|---|---|
| MCP | `2025-06-18`，见 `MCP_PROTOCOL_VERSION` | stdio 与 Streamable HTTP 均校验返回版本；fake server 和 HTTP fixture 覆盖初始化、discovery、调用 |
| ACP | 整数 `1`，见 `ACP_PROTOCOL_VERSION` | 初始化拒绝缺失、null、字符串及其他数值版本；fixture 覆盖 initialize/session/prompt 与双向回调 |
| Agent Skills | [上游格式快照 217be548](https://github.com/agentskills/agentskills/blob/217be548739f21d6008915c29aefe320ea1a90af/docs/specification.mdx) 的实现子集 | 元数据、正文按需加载与资源路径约束；本地 allowed-tools 为逗号分隔，上游为空格分隔且 Experimental。不声明全规范兼容，不将此字段解释为自动批准 |

协议版本不等于真实 Agent 产品版本。2026-10-26 已固定格式快照，并通过 OpenCode 1.19.4 的真实只读模型 Turn，完成 0.2 与 7.5；三个后端公共任务集 8.2 仍未完成。ACP 没声明的能力不当成已验证支持，实验方法不从版本号推导开启。快照校验值、能力差异与完整记录见 [自动验收](agent-acceptance-2026-10-26.md)。

使用说明见 [Agent 扩展、恢复与本地自动化](../user/agent-runtime.md)。

2026-10-26，本机重装后的 OpenCode 1.19.4 已通过真实 ACP 初始化、会话创建及模型读取文件，返回未包含在提示词中的随机内容并正常 end_turn。握手可用 `opencode_smoke` 复验；模型测试使用 `installed_backend_acceptance`，启用方法见 [自动验收](agent-acceptance-2026-10-26.md)。Gemini CLI、批准写入/拒绝/取消及跨后端质量比较尚未由只读 smoke 覆盖。

## 自动化验证

```powershell
cargo test -p termior-ai --test context_ecosystem
cargo test -p termior-ai --test repo_map
cargo test -p termior-editor outline
cargo test -p termior-hooks
cargo test -p termior-agent-host --test ecosystem_contract
cargo test -p termior-ai --test security_redteam
```

这些测试覆盖：

- 类别 token 预算、required item 超限和 estimated token 标记
- 文件/终端输出引用的重取、SHA-256 完整性检查和大工具输出外置
- `TaskBrief` 压缩与连续压缩熔断，审批和 unknown 状态不丢失
- 根与嵌套 `AGENTS.md` 作用域、用户级/项目级 Skills 覆盖和按需资源读取
- 仓库地图：tree-sitter 定义/引用与签名、相关性排序和依赖边、切片不超 token 预算；未改文件不重读、同长度 racy 改写、改名复用大纲、删除文件/目录、大文件只列路径、ignore/deny-list/硬链接/符号链接/二进制、未授权根、文件上限、后台服务与查询时过期排除
- 记忆的接受、编辑、暂停、删除、过期和 secret 拒绝
- MCP stdio 初始化、tools/resources/prompts discovery、server-qualified 工具、封闭 schema、实际调用、输出上限和单服务故障隔离
- Hook 真实子进程、超时/输出上限、修改参数后的 schema/路径/deny-list/审批重算
- ACP initialize/session/prompt、流式事件、权限事件，以及 prompt 尚在运行时处理宿主回调的双向协议

## 场景复验（自动回归已覆盖逻辑，桌面驱动可复验界面）

1. 在项目根和子目录各放一份 `AGENTS.md`，要求 Agent 修改子目录文件。Context Inspector 应显示两份来源；修改根文件时不应注入子目录规则。
   发送消息后点击 Composer 工具栏的 `Context …t · … items`，逐条核对 category、token、Included/Summarized/Truncated/Excluded 和原因。
2. 运行产生大输出的工具。聊天历史只应保留摘要和 artifact 引用；引用仍可重取原输出，篡改 artifact 后读取必须报完整性错误。
3. 配置一个测试 Hook，让它把 `write_file` 的路径从 `a.txt` 改为 `b.txt`。审批卡必须显示并批准 `b.txt`；旧参数的审批不能沿用。
4. 连接两个都声明 `search` 的 MCP server。工具名应显示为 `mcp::<server>::search`，一个 server 断开不能移除另一个。
5. 启动 ACP fake/兼容 Agent。没有实际提供的 optional capability 必须显示 unsupported；文件或终端宿主回调只能经配置的 `AcpClientHandler` 执行。
6. 在用户级或项目 `.agents/skills/<name>/SKILL.md` 放置测试 Skill。Composer 标题栏应显示 `Skills N` 和来源/allowed-tools；在消息中显式写 `$<name>` 后，该 Skill 应显示 `active this turn`，正文才会进入 Context Inspector，工具上限只能与任务工具取交集。
7. 在 `agent-memory.json` 放入带证据的 candidate。`Memory N` 面板应支持 Accept/Reject；已接受条目支持 Pause/Resume/Delete，并在写盘前拒绝 secret 形态。

上下文延迟（仓库地图）门禁：

```bash
cargo build -p termior-bench --release --bin nfr-context --bin nfr-gate
./target/release/nfr-context --rounds 3 > nfr-context-payload.json
./target/release/nfr-gate --platform linux --payload nfr-context-payload.json --baselines docs/baselines/nfr-baselines.json
```

8. 在较大的仓库里打开 Composer 并开始输入（触发后台索引），输入期间界面不应卡顿。索引完成后发送提到某个函数名的任务，Context Inspector 应出现 Evidence 类的 `repo-map` 条目及其 token 数（首轮索引未完成时该条目可能缺席）。切片内容（文件、签名、行号，以及改名/删除后的路径变化）由 `repo_map` 集成测试断言，Inspector 不显示正文。

MCP 的远端端点只接受 HTTPS（localhost 例外），HTTP 重定向默认禁用；OAuth 回调同时校验 state 与 resource binding。凭据引用只保存 key reference，不能进入上下文或日志。

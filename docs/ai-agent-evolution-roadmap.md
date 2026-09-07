# Termior AI Agent 演进路线

本文是 `docs/termior-spec.md` 中 M6–M10 的实施索引。详细需求、设计和任务分别放在五个 OpenSpec change 中,后续编码代理应按依赖顺序逐个实现、验证和归档,不要把五个阶段合并为一次大改。

## 产品方向

Termior 的目标是成为以终端现场为核心、可验证、可恢复、可扩展的 Agent 工作台。内置 Agent 保留 BYOK 和本地模型路径;结构化外部 Agent 用于复用成熟执行能力;普通 CLI/TUI Agent 继续获得 Termior 的终端、附件、通知与审阅增强。

架构依据见 [ADR 0005](./adr/0005-hybrid-agent-host-architecture.md)。调研参考包括 [Claude Code 的执行循环](https://code.claude.com/docs/en/how-claude-code-works)、[Codex app-server](https://learn.chatgpt.com/docs/app-server)、[ACP 架构](https://agentclientprotocol.com/get-started/architecture)、[Warp Full Terminal Use](https://docs.warp.dev/agents/capabilities/full-terminal-use/)、[Agent Skills 规范](https://agentskills.io/specification) 与 [MCP 架构](https://modelcontextprotocol.io/docs/2026-07-28/learn/architecture)。这些外部能力随版本变化,实现时须重新核验协议版本和稳定性标记。

## 依赖顺序

```text
M6 可靠单代理
  └─ M7 终端 Agent 工作台 + 首个外部后端
       └─ M8 上下文、Skills、MCP + 第二个协议后端
            └─ M9 持久恢复、检查点与执行隔离
                 └─ M10 受控并行与本地自动化
```

| 里程碑 | OpenSpec change | 核心退出条件 |
|---|---|---|
| M6 | [`agent-stage-a-reliable-runtime`](../openspec/changes/agent-stage-a-reliable-runtime/) | 单代理完整完成“读取→修改→审阅→验证→失败后修正”,工具调用不丢失、不重复执行 |
| M7 | [`agent-stage-b-terminal-workbench`](../openspec/changes/agent-stage-b-terminal-workbench/) | 长命令可流式观察、等待、输入、停止和接管;一个结构化外部后端端到端可用 |
| M8 | [`agent-stage-c-context-ecosystem`](../openspec/changes/agent-stage-c-context-ecosystem/) | 长会话能压缩并保留目标;Skills/MCP 可控可诊断;第二后端证明能力协商有效 |
| M9 | [`agent-stage-d-recovery-isolation`](../openspec/changes/agent-stage-d-recovery-isolation/) | 崩溃后能安全恢复;检查点范围准确;需要隔离的任务不会静默直接执行 |
| M10 | [`agent-stage-e-parallel-automation`](../openspec/changes/agent-stage-e-parallel-automation/) | 多任务在预算和并发限制内运行;写任务隔离;自动化每次运行可审计、可取消 |

## 跨阶段不变量

- 任务创建后固定绑定 `project_dir` 和执行环境;切换 Tab 或 shell `cd` 不改变任务目标。
- 每次工具调用都有稳定 ID 和 `proposed → approved/denied → running → succeeded/failed/unknown` 生命周期;未知结果不得自动重放。
- “Agent 已完成”必须附带验收证据或明确列出未验证项;模型文字不能替代工具结果。
- 审批只决定是否执行,隔离决定执行后实际能触达什么;两者在数据模型和界面中分别呈现。
- 文件恢复、进程取消和远程副作用是不同能力;检查点必须列明覆盖范围。
- 外部协议的可选能力缺省即视为不支持;Termior 不通过猜测或 TUI 文本解析补出虚假能力。
- 所有凭据仍进入系统钥匙串;任务日志、上下文、命令输出与诊断信息在持久化前执行脱敏。

## 编码代理执行约定

1. 只领取当前 change 中未完成且所有前置任务已完成的一项或一组紧邻任务。
2. 开始前读取 `AGENTS.md`、`CONTEXT.md`、`docs/termior-spec.md`、本路线、ADR 0005 和该 change 的 proposal/design/spec/tasks。
3. 先实现不依赖 GPUI 的领域模型和纯逻辑测试,再连接进程、存储和 UI。
4. 不得用字符串启发式绕过稳定 ID、状态机或协议能力协商。
5. 完成任务后更新对应 `tasks.md` 的复选框,记录实际测试命令;如偏离设计,先更新 design/spec 再改代码。
6. 一个阶段的退出场景全部通过后再归档 change 并进入下一阶段。

## 固定评测任务集

从 M6 起维护同一组可重复运行的真实任务,后续阶段只扩展场景:

- 修复一个能稳定复现的失败测试,修改后重跑并给出退出码。
- 完成包含两个以上文件的修改,在用户保留未提交改动时不覆盖其工作。
- 运行超过默认短命令时限的构建,期间读取增量输出并允许取消。
- 进入交互式程序,在人与 Agent 之间切换控制权后继续完成目标。
- 在审批、文件写入前后和命令结果未知三个位置模拟中断并恢复。
- 两个只读子任务并行执行,一个写任务在独立 worktree 中完成并合并。
- 定时自动化失败后保留诊断,修复条件后只重试一次且不重复副作用。

每次发布记录任务完成率、验证通过率、人工干预次数、恢复成功率、耗时、模型用量和越权拦截结果。所有指标默认只保存在本地。

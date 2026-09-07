## 0. 阶段门禁

- [ ] 0.1 确认 `agent-stage-a-reliable-runtime` 已归档且 M6 固定评测全绿
- [ ] 0.2 重新核验 Codex app-server 官方 stable/experimental 方法与兼容版本，记录链接和锁定策略

## 1. CommandSession 领域模型

- [ ] 1.1 在 `termior-terminal-core` 定义 CommandSessionId、CommandOwner、CommandState、Controller、OutputChunk/Cursor 与能力集合
- [ ] 1.2 实现命令状态和控制权转换纯函数；覆盖 kill/release/detach/unknown 边界
- [ ] 1.3 定义 TerminalService trait：create、read_output、wait、write_stdin、resize、kill、release、take_over、hand_back
- [ ] 1.4 为假进程实现 deterministic fake service，供 runtime/backend 契约测试复用

## 2. 本地终端服务

- [ ] 2.1 在 `termior-terminal` 建立线程安全 session registry，稳定 ID 不复用，Drop 明确处理活动子进程
- [ ] 2.2 实现有界 chunk ring buffer、输出序号、截断水位和多消费者游标
- [ ] 2.3 迁移 one-shot `run_command`：超时只结束本次 wait，任务取消才终止进程；输出与退出码进入 session
- [ ] 2.4 迁移 `shell_bg_spawn`：保留输出并返回 session ID；增加 status/output/wait/kill 工具契约
- [ ] 2.5 迁移 persistent shell/PTY：输入、marker/退出结果和 timeout 均映射为 command session 事件
- [ ] 2.6 将现有 pane PTY 注册为可引用终端；保持原 tab 生命周期和渲染性能
- [ ] 2.7 实现 user/agent 控制权检查到最终 writer 入口，UI 状态不能绕过该检查
- [ ] 2.8 通过 Windows Job Object 和 Unix process-group 测试验证 kill 行为覆盖进程树

## 3. 结构化终端上下文

- [ ] 3.1 将 OSC 133 command start/end/exit 关联为用户命令记录，缺失字段保留 unknown
- [ ] 3.2 为终端选区和失败命令构建 Context Item，引用 terminal/session/output range 而非复制整个滚屏
- [ ] 3.3 Composer 增加“解释/修复并重跑/附加到任务”入口，并固定目标任务的 project_dir
- [ ] 3.4 实现 Take over / Hand back 状态和快捷操作；接管时 TaskRuntime 进入 waiting-user
- [ ] 3.5 添加长输出、UTF-8 分块、ANSI/OSC 过滤、慢消费者和截断测试

## 4. AgentBackend 契约

- [ ] 4.1 新建 `crates/termior-agent-host`，不得依赖 GPUI；加入 workspace 与依赖预算说明
- [ ] 4.2 定义 AgentBackend、BackendEvent、BackendRequest、CapabilityProfile 和 transport-neutral error
- [ ] 4.3 为内置 `termior-ai` runtime 实现 adapter，通过 M6 同一 backend contract suite
- [ ] 4.4 实现子进程 stdio transport：独立 reader/writer/stderr、消息 framing、request ID、shutdown timeout 和 kill 兜底
- [ ] 4.5 建立 fake JSON-RPC backend server，覆盖乱序响应、未知通知、部分 frame、stderr 噪声和进程退出

## 5. Codex app-server 适配器

- [ ] 5.1 实现 initialize/version/capability 握手，只启用锁定版本已验证的 stable 方法
- [ ] 5.2 映射 thread create/resume、turn start、item notifications 与完成/错误状态到 Termior Task/Turn/Event
- [ ] 5.3 映射 approval request/response，保留后端原始 request ID 与 Termior tool call ID 的对应
- [ ] 5.4 实现 turn interrupt 和 app-server shutdown；中途退出时正确产生 unknown 状态
- [ ] 5.5 配置 executable 路径与后端可用性诊断；认证/模型来源明确显示为 Codex 管理
- [ ] 5.6 使用录制且脱敏的官方协议 fixture 做回归，真实 Codex 仅作为可选手工 smoke

## 6. UI 与验证

- [ ] 6.1 Agent 选择器区分内置后端、结构化外部后端和普通 CLI Agent
- [ ] 6.2 根据 CapabilityProfile 显示/禁用 resume/fork/steer/cancel/model/terminal 动作
- [ ] 6.3 任务详情显示绑定项目、命令会话、controller、增量输出、退出码和截断提示
- [ ] 6.4 通知路由使用 TaskState/CommandState，不解析屏幕词语
- [ ] 6.5 运行长构建、后台服务、REPL 接管、跨 Tab、后端退出和取消的自动/手工验收
- [ ] 6.6 运行相关 crate tests、三平台 smoke、`cargo clippy --workspace` 与 NFR 体积/启动回归
- [ ] 6.7 全部退出场景通过后记录实现偏差并严格验证本 change

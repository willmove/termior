## Context

现有会话每次消息变更重写一个 JSON 文件，适合聊天历史，不适合恢复工具队列、命令与审批。M6 已定义 versioned TaskEvent，M7 有 CommandSession，M8 有可重取 Context Item；M9 把这些状态持久化并建立真实执行边界。

## Goals / Non-Goals

**Goals:**

- 崩溃或退出后能重建任务，并诚实区分已知与 unknown
- 文件恢复不覆盖用户在检查点后独立完成的修改
- 写任务可在独立 worktree 工作和验证
- 审批、worktree 与 OS sandbox 在模型和 UI 中明确分层
- 任何“必须隔离”的配置都不会无声回退

**Non-Goals:**

- 不声称撤销数据库、部署、网络调用或其他远程副作用
- 不在本阶段提供远程 VM、容器云或跨设备同步
- 不要求三平台 sandbox 实现细节完全相同，只要求声明的能力通过统一测试
- 不把 Git commit 当作任务事件或检查点的替代

## Decisions

### D1：事件日志为事实来源，快照只是缓存

每个任务目录含 `events.jsonl`，记录 header(schema/task ID)、单调 sequence、event ID、timestamp、event kind/payload、前一记录 hash 和当前 hash。写入采用 append + flush 策略；关键边界（批准、副作用开始/结果、变更应用、完成）必须 flush 后才能推进。

TaskSnapshot 保存已归约状态和 last sequence，通过临时文件+rename 原子替换。启动时验证链并从最近有效快照重放；尾部半条记录截断并报告，本体中间损坏则停止恢复并保留诊断副本。

任务 reducer 是纯函数，线上恢复与测试使用同一实现。

### D2：崩溃时 running 变 unknown

恢复器把没有终态事件的模型请求、工具调用和命令映射为 unknown。只读且幂等的操作可由用户选择重取；本地写、process、network/external 默认只能检查或放弃，重试必须显式创建新 attempt ID。任何后端提供可验证 resume token 时，可先查询真实状态再决定。

### D3：检查点明确覆盖范围和基线

Checkpoint 记录任务 event sequence、涉及文件的路径、内容 hash、保存内容引用、文件类型/权限，以及关联 change set。创建和恢复均通过 deny-list/workspace policy。恢复前比较 current/checkpoint/expected-after 三方状态：未变可直接预览；用户后续修改产生冲突，默认跳过并要求逐文件决议。

检查点 UI 列出“任务状态”“文件”“不覆盖的命令/远端副作用”。恢复任务状态不得把已执行副作用改回未执行后自动重放。

### D4：Worktree 是代码并发环境，不是安全边界

`WorktreeEnvironment` 保存 repo、base commit、branch、path、owner task、dirty state 和 init result。创建后才将任务 project_dir 绑定到 worktree。初始化失败回滚新目录/分支或标记 orphaned；不得把任务留在一半绑定状态。

合入前要求任务停止写入，刷新 base/target、展示 diff/验证和冲突。用户决定 merge/cherry-pick/manual；删除 worktree 前必须确认无未保存变更。UI 始终把 worktree 标为 code isolation，而不是 sandboxed。

### D5：Execution Environment 公开能力

统一 descriptor 包含 type、host、root、writable paths、network mode、credential mode、process isolation、backend owner 和 verified-at。类型：

- direct：当前用户权限执行
- worktree：独立代码目录，系统权限仍为当前用户
- sandboxed：平台 sandbox backend 强制限制文件/网络/凭据/进程

任务声明 required capabilities。环境准备后以 probe 验证实际能力；缺少必需项时拒绝启动。任何 fallback 都需要用户显式选择另一个 profile，并产生新环境 ID。

### D6：平台 sandbox 通过统一契约而非相同实现

`SandboxBackend` 提供 probe/prepare/spawn/terminate/cleanup/describe。三平台实现可使用各自原生机制；至少能限制到授权工作目录、默认阻断网络或按 allowlist、最小环境变量、隔离/清理进程树。原生能力无法可靠实现的组合标为 unsupported。

Windows、macOS、Linux 分别运行 black-box escape suite：父目录/符号链接、私有地址/公网、环境 secret、子进程逃逸和残留。测试结果与版本写入 capability descriptor。

### D7：外部后端安全声明不被盲信

外部 Agent 的 approval/sandbox 信息映射为 `reported` 能力；只有 Termior 启动并 probe 的环境标为 `verified`。如果外部后端自行执行命令且无法接受 host environment，隔离来源显示 backend-managed/unknown。用户要求 Termior-verified sandbox 时，该后端不可选。

### D8：持久内容先脱敏、再落盘

task events、输出 chunks、Context Item、Hook/MCP 诊断和 crash log 统一经过 streaming redactor。原始秘密仅在具体工具的短生命周期内存中。任务 retention 支持按天/磁盘上限清理大内容，先删除可重取 blobs，保留任务摘要、事件元数据和 AcceptanceReport。

## Risks / Trade-offs

- hash chain 能检测损坏但不是防恶意本机用户的签名；不宣传为防篡改安全日志
- flush 关键事件增加 IO：批量非关键 delta，边界事件同步；用性能基准限制影响
- sandbox 在 Windows 尤其困难：先 prototype，能力不足就 fail closed，不用命令 denylist冒充
- worktree merge 可能复杂：自动路径只覆盖 clean fast-forward/无冲突 cherry-pick，其余交给可见的 Git 流程
- 输出脱敏可能误报或漏报：deny patterns + 高熵/credential shapes，用户可见但不能关闭硬编码敏感路径规则

## Migration Plan

1. 新 journal 旁路写入并与内存状态对比，不先开放恢复
2. reducer/replay 和故障注入通过后启用任务启动恢复
3. 加 checkpoint，再加 worktree；两者各自独立发布
4. 先上线 execution descriptor/direct/worktree 透明度，再按平台逐一开放 sandboxed profile
5. 只有 escape suite 通过的平台/profile 才显示为 available

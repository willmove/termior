## ADDED Requirements

### Requirement: 打开文件夹仅重定向活动 Tab

"打开文件夹"（标题栏按钮、状态栏工作区 pill、`Ctrl/Cmd+O`）SHALL 只改变活动 Tab 的项目文件夹（`project_dir`），MUST NOT 重建工作区或影响其他任何 Tab 的项目文件夹与运行时状态（终端会话、编辑器 buffer、分屏布局）。

#### Scenario: 重定向活动终端 Tab

- **WHEN** 活动 Tab 是一个终端，用户通过打开文件夹对话框选定了一个不同的有效目录
- **THEN** 该 Tab 的 `project_dir` 更新为所选目录，其余 Tab 的 `project_dir`、`cwd` 与运行状态全部不变

#### Scenario: 重定向非终端 Tab

- **WHEN** 活动 Tab 是编辑器/预览/Markdown 等非终端 Tab，用户选定了一个不同的有效目录
- **THEN** 该 Tab 的 `project_dir` 更新为所选目录，其展示内容不受影响

#### Scenario: 取消对话框

- **WHEN** 用户在文件夹选择对话框中取消
- **THEN** 所有 Tab 的 `project_dir`、`cwd` 与工作区状态无任何变化

#### Scenario: 选定目录与当前相同

- **WHEN** 用户选定的目录等于活动 Tab 当前的 `project_dir`
- **THEN** 系统不触发任何重扫、刷新或状态变更

#### Scenario: 空工作区中打开文件夹

- **WHEN** 工作区没有任何 Tab，用户打开文件夹
- **THEN** 所选目录成为工作区兜底根（`WorkspaceState.root`），后续新建 Tab 以此为继承来源

### Requirement: 运行中终端的 cd 同步

重定向的活动 Tab 若是运行中的终端，系统 SHALL 向该终端的 PTY 注入 `cd` 命令使 shell 切换到新目录，MUST NOT 杀死或重启 shell 会话。尚未 spawn 的终端 Tab SHALL 直接以新目录作为后续 spawn 的 cwd。

#### Scenario: 注入 cd 命令

- **WHEN** 运行中的终端 Tab 被重定向到新目录
- **THEN** 系统按其 shell 类型向 PTY 写入对应 `cd` 命令（cmd.exe 使用 `cd /d`，PowerShell 与 POSIX shell 使用 `cd`），前台会话保持存活

#### Scenario: 未运行终端直接落位

- **WHEN** 被重定向的终端 Tab 尚未 spawn（placeholder 状态）
- **THEN** 其 `cwd` 同步置为新目录，之后 spawn 直接落在该目录

### Requirement: project_dir 与 cwd 职责分离

每个 Tab SHALL 同时持有 `project_dir`（项目锚点）与 `cwd`（shell 当前目录）。`project_dir` MUST NOT 随 shell 的目录切换（OSC 7 报告）而改变；终端分屏与新建终端的 cwd 继承逻辑保持不变（继承 `cwd`）。

#### Scenario: shell cd 不拖动项目锚点

- **WHEN** 终端 Tab 的 shell 通过 `cd` 进入子目录并经 OSC 7 同步
- **THEN** 该 Tab 的 `cwd` 更新，但 `project_dir` 保持不变，Explorer 与 Git 面板不跟随进入子目录

#### Scenario: 分屏继承 shell 当前目录

- **WHEN** 用户在一个 `cwd` 已偏离 `project_dir` 的终端 Tab 内分屏
- **THEN** 新 pane 继承该 Tab 的 `cwd`（沿用既有授权校验）

### Requirement: 新建 Tab 的项目文件夹继承

新建 Tab 的 `project_dir` SHALL 继承自创建时的活动 Tab；无活动 Tab 时使用工作区兜底根。private terminal 的 `project_dir` SHALL 使用工作区兜底根。

#### Scenario: 普通新建继承活动 Tab

- **WHEN** 活动 Tab 的 `project_dir` 为 `D:\proj-a` 时新建一个终端 Tab
- **THEN** 新 Tab 的 `project_dir` 为 `D:\proj-a`

#### Scenario: private terminal 使用兜底根

- **WHEN** 用户新建 private terminal
- **THEN** 其 `project_dir` 为工作区兜底根而非活动 Tab 的项目文件夹

### Requirement: 侧栏与状态栏跟随活动 Tab

文件 Explorer 与 Git 面板 SHALL 以活动 Tab 的 `project_dir` 为根；切换活动 Tab 时若根发生变化，系统 SHALL 重扫 Explorer 并刷新 Git 状态。状态栏工作区指示 SHALL 显示活动 Tab 的项目文件夹名。重扫 MUST 可取消，过期扫描结果 MUST NOT 覆盖新根的内容。

#### Scenario: 切 Tab 触发跟随

- **WHEN** 用户从 `project_dir` 为 `D:\proj-a` 的 Tab 切换到 `project_dir` 为 `D:\proj-b` 的 Tab
- **THEN** Explorer 重扫至 `D:\proj-b`，Git 面板刷新为 `D:\proj-b` 的仓库状态，状态栏显示 `proj-b`

#### Scenario: 同根切 Tab 不重扫

- **WHEN** 用户切换到 `project_dir` 相同的另一个 Tab
- **THEN** 系统不触发 Explorer 重扫与 Git 刷新

#### Scenario: 非 git 目录的空态

- **WHEN** 活动 Tab 的 `project_dir` 不是 git 仓库
- **THEN** Git 面板显示无仓库空态，不报错

### Requirement: 多 root 工作区授权

通过打开文件夹对话框显式选定的目录 SHALL 被视为显式授权，立即加入 workspace 授权注册表，无需额外确认弹窗。PTY spawn、git 命令与 AI 工具 SHALL 继续共用同一注册表门控；位于所有已授权 root 之外的目标路径 MUST 继续被拒绝。

#### Scenario: 选定即授权

- **WHEN** 用户通过打开文件夹选定了一个此前未授权的目录
- **THEN** 该目录进入授权注册表，以其为 cwd 的 PTY spawn 不被 `resolve_cwd` 丢弃

#### Scenario: 未授权路径仍被拒绝

- **WHEN** 终端 spawn 或分屏继承的 cwd 落在全部已授权 root 之外
- **THEN** 该 cwd 被丢弃并回落到已授权根（沿用既有行为）

### Requirement: 项目文件夹的持久化与恢复

每个 Tab 的 `project_dir` SHALL 随工作区状态持久化并在重启后恢复。旧格式（无 `project_dir` 字段）的工作区文件 SHALL 兼容恢复：每个 Tab 的 `project_dir` 回填为文件中的全局 `root`。工作区恢复 MUST NOT 再以全局 `root` 与启动目录相等作为过滤条件。

#### Scenario: 各 Tab 独立恢复

- **WHEN** 上次会话中两个 Tab 的 `project_dir` 分别为 `D:\proj-a` 与 `D:\proj-b`，应用重启
- **THEN** 两个 Tab 各自恢复其 `project_dir`，Explorer 以活动 Tab 的 `project_dir` 为根

#### Scenario: 旧格式文件兼容

- **WHEN** 工作区文件由旧版本写出（只有全局 `root`，Tab 无 `project_dir`）
- **THEN** 所有 Tab 恢复后的 `project_dir` 等于该文件中的 `root`，行为与旧版等价

#### Scenario: 换目录启动不丢会话

- **WHEN** 应用以不同于上次工作区根目录的启动目录打开
- **THEN** 持久化的 Tab 集合仍被恢复，每个 Tab 保留自己的 `project_dir`

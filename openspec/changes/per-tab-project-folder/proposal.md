## Why

当前"打开文件夹"（标题栏文件夹按钮、状态栏工作区 pill、`Ctrl/Cmd+O` 三个入口）的语义是**切换整个工作区**：`open_workspace` 用新 root 推倒重建整个 `WorkspaceView`，所有 Tab 被迫同进退。用户期望的是"只给当前活动 Tab 换项目文件夹，其他 Tab 保留各自原文件夹"——每个 Tab 独立绑定项目，像浏览器每个标签页各有上下文。

## What Changes

- `TabState` 新增 `project_dir`（项目文件夹）作为一等持久化字段，与既有 `cwd`（shell 当前目录，OSC 7 同步）并存；`project_dir` 不再随 shell `cd` 漂移
- "打开文件夹"改为只重定向**活动 Tab**：设置其 `project_dir`、将该目录加入 workspace 授权注册表；活动 Tab 是运行中的终端时，向其 PTY 注入 `cd` 命令使 shell 同步过去（不重启会话）
- 侧栏文件 Explorer 与 Git 面板 SHALL 跟随活动 Tab 的 `project_dir`（切换 Tab 时重扫/刷新），状态栏工作区 pill SHALL 显示活动 Tab 的项目文件夹名
- 新建 Tab 的 `project_dir` 从活动 Tab 继承（与现有 `cwd` 继承同构）；非终端 Tab（编辑器/预览等）同样持有 `project_dir`，打开文件夹对它们只改项目关联
- `WorkspaceState.root` 降级为兜底默认值（无继承来源的新建 Tab、private terminal 使用）；**BREAKING**：工作区持久化恢复不再以 `root` 相等为过滤条件，`Termior-workspaces.json` 中每个 Tab 各自恢复其 `project_dir`
- workspace 授权注册表从"单一全局 root"变为"各 Tab `project_dir` 的并集"，通过文件对话框显式选择文件夹即视为一次显式授权（`WorkspaceAuthRegistry::authorize`）

## Capabilities

### New Capabilities

- `tab-project-folder`：Tab 级项目文件夹的绑定、继承与重定向语义；"打开文件夹"的 Tab 作用域行为；Explorer/Git/状态栏对活动 Tab 项目文件夹的跟随；以及多 root 授权与持久化恢复规则

### Modified Capabilities

（无。`openspec/specs/` 目前为空，无既有 capability 的需求被修改。）

## Impact

**代码**

- `crates/termior-ui/src/lib.rs`：`TabState` 增加 `project_dir` 字段；`new_tab` 继承逻辑；新增 `set_active_project_dir` 类方法
- `crates/termior/src/workspace_view.rs`：`open_workspace` 从"重建 View"改为"重定向活动 Tab"；终端 `cd` 注入（`TerminalBridge::writer().write_all`）；Tab 切换处触发 Explorer 重扫与 `refresh_vcs_data`；状态栏 pill 数据源
- `crates/termior-security/src/workspace.rs`：复用现有 `authorize`/`revoke`，注册表随打开文件夹动态累积 root
- `crates/termior-terminal/src/pty.rs`：`resolve_cwd` 授权检查逻辑不变，但授权集合变大，需确认多 root 场景行为

**行为**

- FR-SEC-04（workspace 授权注册表统一门控）语义扩展：授权粒度从"一个窗口一个 root"变为"每个显式打开的文件夹一个 root"，PTY spawn / AI 工具 / git 命令仍过同一注册表
- "打开文件夹"不再销毁并重建其他 Tab 的运行时状态（终端会话、编辑器 buffer 全部保留）
- 工作区持久化文件格式新增字段，旧格式通过 `serde(default)` 兼容恢复

**文档**

- 实现后应回写 `docs/termior-spec.md` 中"打开文件夹"与工作区模型的相关段落（6.14 授权模型、工作区/Tab 章节）

## 1. 数据模型（termior-ui）

- [x] 1.1 `crates/termior-ui/src/lib.rs`：`TabState` 新增 `project_dir: PathBuf` 字段，`#[serde(default)]` 兼容旧格式（缺省时回填语义见 1.3）
- [x] 1.2 `new_tab`：`project_dir` 继承活动 Tab（private terminal 用 `self.root`），`cwd` 继承逻辑保持不变
- [x] 1.3 新增 `set_active_project_dir(path)`：同时置 `project_dir` 与 `cwd`；新增恢复用回填逻辑——旧格式反序列化后 `project_dir` 为空时置为 `root`
- [x] 1.4 `termior-ui` 单测：继承规则（普通/private/无活动 Tab）、`set_active_project_dir`、旧格式 JSON 恢复回填

## 2. 打开文件夹改造（workspace_view）

- [x] 2.1 `open_workspace`（`crates/termior/src/workspace_view.rs:915`）：删除 `*workspace = Self::new(...)` 整区重建，改为——无活动 Tab 时更新 `model.root`；有活动 Tab 时 `set_active_project_dir` + 相同目录 no-op + 取消对话框无变化
- [x] 2.2 选定目录后调用 `WorkspaceAuthRegistry::authorize` 加入授权（`crates/termior-security/src/workspace.rs:41`）
- [x] 2.3 确认重定向路径不再触碰其他 Tab 的运行时（`AppTab` panes、终端 bridge、编辑器 entity 全部保留）

## 3. 运行中终端的 cd 注入

- [x] 3.1 按 shell 类型生成 cd 命令：cmd.exe → `cd /d "<path>"\r`；PowerShell/POSIX → `cd "<path>"\r`；无法判定时用裸 `cd "<path>"\r`
- [x] 3.2 经 `TerminalBridge::writer().write_all`（`crates/termior-terminal/src/bridge.rs:198`）注入运行中终端；未 spawn 的 placeholder 终端跳过注入（`cwd` 已置位）
- [x] 3.3 路径含引号/特殊字符的转义处理（core 单测覆盖引号/`$`/空格/中文路径；GUI 手工验证见 7.2）

## 4. 侧栏与状态栏跟随

- [x] 4.1 Tab 激活路径（`switch_to`/`switch_index`/关闭后激活等）统一检测活动 Tab `project_dir` 变化：变化才触发 `schedule_explorer_scan` + `refresh_vcs_data`，相同则跳过
- [x] 4.2 `refresh_vcs_data` 与 `schedule_explorer_scan` 的根改为活动 Tab 的 `project_dir`（替换 `self.model.root` / `explorer_requested_root` 的来源）
- [x] 4.3 状态栏工作区 pill（`workspace_view.rs:5159` 附近）显示活动 Tab 的 `project_dir` 名称；打开文件夹后立即刷新
- [x] 4.4 验证切 Tab 重扫与 `fix-file-explorer-indexing` 的可取消/世代作废扫描协作：快速来回切 Tab 不出现旧根结果覆盖新根

## 5. 授权与安全校验

- [x] 5.1 多 root 场景验证 `resolve_cwd`（`crates/termior-terminal/src/pty.rs:488`）：已授权的新项目文件夹 spawn cwd 不被丢弃；未授权路径仍回落
- [x] 5.2 确认 git 命令与 AI 工具经同一注册表门控的行为在多 root 下不变（FR-SEC-04）

## 6. 持久化与恢复

- [x] 6.1 删除恢复时的 `.filter(|state| state.root == root)`（`workspace_view.rs:357`），换目录启动也能恢复 Tab 集合
- [x] 6.2 旧格式兼容由单测钉住（无 `project_dir` 的 JSON 回填为 `root`；新旧字段均 `#[serde(default)]`，旧版读新版文件不报错）

## 7. 测试与文档

- [x] 7.1 `cargo test -p termior-ui` 全绿；`cargo clippy --workspace` 无新增告警
- [ ] 7.2 手工验收：两个终端 Tab 各自打开不同文件夹互不影响；运行中终端被 `cd` 注入后会话存活；切 Tab 时 Explorer/Git/状态栏跟随；非 git 目录空态
- [x] 7.3 回写 `docs/termior-spec.md`：6.14 授权模型（单 root → 多 root 并集）与工作区/Tab 章节的"打开文件夹"语义

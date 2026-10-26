## Context

现状：一个窗口 = 一个全局工作区。`WorkspaceState.root`（`crates/termior-ui/src/lib.rs:62`）是唯一文件夹锚点，"打开文件夹"通过 `*workspace = Self::new(root, ...)`（`crates/termior/src/workspace_view.rs:937`）推倒重建整个 View，Explorer、Git、授权注册表、全部 Tab 一起切换。

地基已经部分存在：`TabState.cwd` 是 per-tab 的（OSC 7 经 `sync_terminal_context` 同步，`set_active_cwd` 写入），`new_tab` 从活动 Tab 继承 cwd，PTY spawn 按 Tab 取 cwd（`create_terminal`，`workspace_view.rs:956`）。缺的是"项目文件夹"这个稳定锚点。授权侧 `WorkspaceAuthRegistry` 已有 `authorize`/`revoke` 动态 API（`crates/termior-security/src/workspace.rs:41-46`），`TerminalBridge::writer().write_all`（`crates/termior-terminal/src/bridge.rs:198`）可直接向 PTY 写字节。`fix-file-explorer-indexing` 刚落地的可取消/世代作废扫描为切 Tab 重扫提供了承载力。

## Goals / Non-Goals

**Goals:**

- 每个 Tab 拥有独立的、稳定的项目文件夹（`project_dir`），"打开文件夹"只作用于活动 Tab
- 运行中的终端在重定向后会话不中断（注入 `cd`），其他 Tab 的运行时状态（终端会话、编辑器 buffer）完全保留
- Explorer / Git / 状态栏 pill 跟随活动 Tab 的 `project_dir`
- 授权模型保持 FR-SEC-04 的统一门控，粒度扩展为多 root 并集
- 旧版 `Termior-workspaces.json` 无损恢复

**Non-Goals:**

- 多窗口 / 多 workspace 平铺（VS Code 式 multi-root）：每个 Tab 仍是单 root
- Explorer 每 Tab 记忆各自的手动浏览位置（初版切 Tab 即回到该 Tab 的 `project_dir`，可作为后续深化）
- 前台进程占用终端时的 `cd` 注入确认提示（与 VS Code 等行为一致：直接写入，后果由用户承担）
- 对 shell 历史、环境变量做项目级隔离

## Decisions

### D1：`project_dir` 与 `cwd` 双字段分离

`TabState` 新增持久化字段 `project_dir`，与 `cwd` 各司其职：

```
TabState
  ├── project_dir  ← 项目锚点：Explorer/Git/状态栏/授权 跟随它
  │                  只被"打开文件夹"与新建继承改变
  └── cwd          ← shell 当前目录：新 split/新终端 继承它
                     OSC 7 持续同步，随 shell cd 漂移
```

**为什么不复用 `cwd`**：若项目锚点 = `cwd`，用户在 shell 里 `cd src/` 就会把 Explorer/Git 拖进子目录，且"打开文件夹"建立的关联会被任何一次 `cd` 摧毁。两个概念的变化源不同（用户显式选择 vs shell 隐式移动），必须分离。

### D2：打开文件夹 = 重定向活动 Tab（用户已决策）

流程：

```
rfd 对话框选定 folder
  ├─ folder == 活动 Tab 的 project_dir 或不存在 → no-op
  ├─ AuthRegistry.authorize(folder)
  ├─ TabState.project_dir = folder；TabState.cwd = folder
  │    （cwd 同步置位，不依赖 OSC 7 回环，保证下一次 split/继承立即正确）
  ├─ 活动 Tab 是运行中终端 → writer.write_all(cd 命令)
  ├─ Explorer 重扫 + refresh_vcs_data + 状态栏 pill 更新
  └─ 无活动 Tab（空工作区）→ 退回旧语义：设为兜底 root
```

**替代方案**：(a) 维持整区重建——被否决，正是本 change 要消除的行为；(b) 总是在新 Tab 打开——被否决，用户明确要求重定向当前 Tab。

### D3：运行中终端注入 `cd`（用户已决策）

经 `TerminalBridge::writer()` 写入，不杀进程。Shell 语法分派：

| Shell | 命令 | 说明 |
|---|---|---|
| cmd.exe | `cd /d "<path>"\r` | `/d` 处理跨盘符 |
| PowerShell / bash / zsh / fish | `cd "<path>"\r` | 均支持带引号路径 |

Shell 类型按 spawn 时记录的 `shell_program`/`ShellDetection` 判定；无法判定时按平台默认（Windows → `cd /d` 对 cmd 安全、PowerShell 忽略多余开关不兼容——实测 PowerShell 的 `cd` 是 `Set-Location`，不支持 `/d`，故无法判定时用裸 `cd`，cmd 用户跨盘符属边缘场景，交给 OSC 7/手动纠正）。**替代方案（重启 shell）被否决**：丢失前台任务，体验断裂。

### D4：显式选择文件夹即授权

文件对话框是用户的显式授权行为，选定后 `authorize(folder)` 直接加入注册表，不再二次弹窗。注册表从 `with_roots([root])` 单 root 变为随打开动作累积的多 root；PTY spawn、git、AI 工具仍过同一注册表（FR-SEC-04 门控不变，粒度变化回写 spec 文档）。

### D5：`root` 降级为兜底 + 持久化不再按 root 过滤（BREAKING）

`WorkspaceState.root` 保留，仅服务：无继承来源的新建 Tab、private terminal、空工作区的打开文件夹。恢复逻辑删除 `.filter(|state| state.root == root)`（`workspace_view.rs:357`）——窗口恢复的是上次的 Tab 集合本身，每个 Tab 带自己的 `project_dir`。旧格式文件无 `project_dir` 字段，`#[serde(default)]` 回填为 `root`（语义等价于旧模型的全局 root）。`TabState` 无 `deny_unknown_fields`，新版写出的文件被旧版读取时自动忽略新字段，回滚安全。

### D6：切 Tab 跟随重扫

`switch_to`/`switch_index` 激活 Tab 后，若活动 Tab 的 `project_dir` 与当前 Explorer 根不同，触发 `schedule_explorer_scan(project_dir)` 与 `refresh_vcs_data`。复用 `fix-file-explorer-indexing` 的可取消扫描与世代作废，旧扫描结果不得覆盖新根。Git 面板对非 git 目录沿用现有"无仓库"空态。

## Risks / Trade-offs

- [终端有前台进程（vim、dev server）时注入 `cd` 会写入该进程 stdin] → 与 VS Code 等主流行为一致，直接注入；README/文档注明；后续可加前台进程检测再深化
- [OSC 7 不可用的 shell（未集成）注入 `cd` 后 `cwd` 不回同步] → D2 中重定向时已同步置位 `cwd`，不依赖回环
- [shell `cd` 漂移出所有授权 root 后，split 继承的 cwd 被 `resolve_cwd` 丢弃回落] → 维持现状语义（回落到授权 root），spec 化为明确场景；符合 FR-SEC-04 而非漏洞
- [切 Tab 重扫大体量目录的抖动] → 可取消扫描 + 浅层优先已在 `fix-file-explorer-indexing` 落地；首屏不阻塞
- [多 root 授权扩大文件可达面] → 每个 root 均来自用户显式对话框选择，授权语义等价于逐工作区授权的累计；`revoke` 保留可收回能力

## Migration Plan

1. 字段 additive 上线：`project_dir` 带 serde default，旧文件恢复时全部 Tab 的 `project_dir = root`（与旧行为等价）
2. 行为切换一次性生效，无灰度；回滚 = revert，新版写出的持久化文件对旧版可读（忽略未知字段）
3. 实现后回写 `docs/termior-spec.md` 6.14（授权粒度）与工作区/Tab 模型段落

## Open Questions

- 无法判定 shell 类型时的 `cd` 语法兜底策略（D3 给了初版取向，实现时按 Windows 实测微调）
- Explorer 每 Tab 记忆浏览位置是否随本 change 做（倾向不做，留后续深化）

## Known Limitations（后续深化）

- **AI ToolRegistry 的注册表是启动快照**：`WorkspaceAuthRegistry` 是值类型，`Clone` 即快照。PTY spawn 时取快照、git 用活引用，行为均正确；但 composer 的 `ToolRegistry` 在启动时烘焙，会话中新打开的文件夹要**重启后**才对 AI 工具可见（构造方已用全量 `project_dir` 播种，恢复的项目不受限）。深化方向：注册表改为共享可变的 `Arc<RwLock<…>>`（需处理 serde 派生与跨 crate 语义，超出本 change 范围）。行为已由 `clone_is_a_point_in_time_snapshot` 测试钉住。

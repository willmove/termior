## Why

保存的 SSH/SFTP 连接是单一平铺列表（侧栏与连接管理窗口均按存储顺序渲染），
连接数量增多后难以定位目标主机。用户需要把连接按环境/用途分组（如「生产」「测试」），
且分组本身应是持久化的一等实体：允许先建空分组备用的组织方式，删除组内最后一个
连接不应连带删除分组。

## What Changes

- `Termior-ssh.json` schema v1 → v2：顶层新增 `groups: Vec<String>`（按展示顺序持久化
  分组列表），连接对象新增可选 `group` 字段（引用分组名，空串/缺省 = 未分组）；
  v1 文件读取时自动迁移（新字段全部有 serde 默认值），保存写出 v2
- 分组独立于连接存在：空分组合法；删除组内最后一个连接不删除分组；删除分组时其
  成员归入未分组；重命名分组与现有分组重名即合并
- 侧栏（`ssh_session_list`）：未分组连接在最前，其后按 `groups` 顺序渲染分组表头
  （折叠 chevron + 名称 + 成员数，含空分组）+ 缩进的成员行；表头单击折叠/展开
  （运行时状态，默认全展开，不跨重启）；连接右键菜单新增「移动到分组…」（第二阶段
  面板列出未分组/全部分组/新建分组…）；分组表头右键提供「重命名分组…」「删除分组」
  （后者需确认）；侧栏底部新增「＋ 新建分组」
- 新建/重命名分组复用侧栏命令栏（`CommandMode::NewGroup` / `RenameGroup`，
  载荷放 `pending_group_profile` / `pending_group_rename`）；「移动到分组 → 新建分组…」
  同时创建分组并把该连接移入
- 连接管理窗口：表单新增「分组」字段（与 SFTP 远程路径同行，Tab 序扩为 0..=9），
  可输入已有分组名或新名称（保存时 `upsert_group` 就地建组），留空为未分组；
  左栏按分组渲染静态标签行（含空分组）
- 校验：分组名唯一、1–64 字节、无控制字符、首尾无空白；连接引用的分组必须存在
  （悬空引用在启动校验报错，不静默吞掉手动编辑错误）；分组变更不影响运行中的会话
  （分组仅是组织信息，凭证按目标绑定、连接按名称定位）

## Capabilities

### New Capabilities

- `ssh-connection-groups`：SSH/SFTP 连接分组的持久化模型（schema v2）、侧栏分组列表
  交互（折叠/移动/新建/重命名/删除）、管理窗口分组字段，以及校验与迁移规则

### Modified Capabilities

（无。`openspec/specs/` 目前为空，无既有 capability 的需求被修改。）

## Impact

**代码**

- `crates/termior-ssh/src/lib.rs`：`Profile.group`、`Profiles.groups`、
  `PROFILES_VERSION = 2`、load 迁移、校验、`upsert_group`、纯函数 `group_connections`
- `crates/termior/src/workspace_view.rs`：`ssh_collapsed_groups` / `ssh_group_menu` /
  分组命令栏状态与 `CommandMode` 新变体；命令执行与菜单关闭路径接线
- `crates/termior/src/workspace_view/sftp_browser.rs`：分组渲染、`SessionMenu` 两阶段
  菜单、分组表头菜单、`apply_profiles_mutation` 统一落盘路径及各变更操作
- `crates/termior/src/ssh_view.rs`：`values` 扩为 10 字段、分组列网格布局、左栏分组标签

**行为**

- FR-SSH-01 补充分组行为（侧栏列表组织、右键菜单项、管理窗口字段）
- 第 7 节 `Termior-ssh.json` 表项更新为 schema v2；全部写入仍走原子 JSON（FR-DATA）
- 旧版二进制读取 v2 文件得到明确的「Unsupported SSH profiles version」错误
  （与 settings 的前向不兼容行为一致）

**文档**

- `docs/termior-spec.md`：FR-SSH-01 与 §7 文件表已回写
- `docs/ssh.md`：新增「连接分组」章节与 schema v2 说明

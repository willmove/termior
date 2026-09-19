## 1. 数据模型与迁移（termior-ssh）

- [x] 1.1 `Profile` 新增 `group: String`（`serde(default, skip_serializing_if = "String::is_empty")`），`Default` 补充
- [x] 1.2 `Profiles` 新增 `groups: Vec<String>`（`serde(default)`）；`PROFILES_VERSION = 2`；`validate()` 校验版本、分组名唯一/合法、连接分组引用存在
- [x] 1.3 `load()` 接受 v1 并归一化为 v2；保存写出 v2
- [x] 1.4 `upsert_group` 与纯函数 `group_connections`（未分组在前、全部分组按存储顺序、含空分组）
- [x] 1.5 单测：v1 迁移、v2 往返、空分组保留、悬空引用拒绝、分区顺序、未分组序列化不含 group 键

## 2. 侧栏分组交互（workspace_view / sftp_browser）

- [x] 2.1 分组渲染：未分组行 → 分组表头（chevron + 名称 + 数量）+ 缩进成员行；`ssh-saved-{index}` 选择器保持 Vec 索引
- [x] 2.2 折叠状态 `ssh_collapsed_groups`（默认展开、reload 清理失效名）
- [x] 2.3 连接右键菜单「移动到分组…」两阶段面板；`move_session_to_group` 按名称定位并原子落盘
- [x] 2.4 分组表头右键菜单：重命名（命令栏预填）、删除（确认后成员归未分组）
- [x] 2.5 「＋ 新建分组」按钮 + `CommandMode::NewGroup`/`RenameGroup` 命令栏；`apply_profiles_mutation` 统一落盘并同步管理窗口
- [x] 2.6 菜单关闭路径（Esc / 左键 / close_chrome_menus）覆盖分组菜单与选择面板

## 3. 管理窗口（ssh_view）

- [x] 3.1 `values` 扩为 10 字段：索引 9 = 分组（标签「分组（可选，留空为未分组）」，与 SFTP 远程路径同行）；Tab 序 0..=9
- [x] 3.2 `profile()`/`select()`/`save()` 接入分组字段；保存前 `upsert_group`
- [x] 3.3 左栏按 `group_connections` 渲染分组标签行（含空分组）

## 4. 测试与文档

- [x] 4.1 侧栏测试：分组渲染顺序（含空分组）、折叠/展开、移动到分组持久化、分组重命名/删除、命令栏新建空分组
- [x] 4.2 管理窗口测试：分组字段往返 + upsert + 清空归未分组而分组保留
- [x] 4.3 `docs/termior-spec.md` FR-SSH-01 与 §7 文件表回写；`docs/ssh.md` 新增「连接分组」章节与 schema v2 说明

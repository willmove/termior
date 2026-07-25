## Context

侧栏文件浏览器当前流程：

1. `WorkspaceView::schedule_explorer_scan` 把 `explorer_loading = true`，在 background executor 上调用 `FileIndex::build`（`ignore::WalkBuilder` 全量递归）。
2. UI 在 `explorer_loading` 时于根标签旁显示 `· indexing…`；若尚无 `FileIndex`，文件行列表为空。
3. 扫描完成后才挂上 `WorkspaceWatcher`；后台每 500ms `try_changes()`，一旦非空就再次全量 `schedule_explorer_scan`。
4. Watcher 目前把 `EventKind::Access` 也当成变更发出。

复现场景（用户截图）：活动终端 cwd 为 `C:\Users\willmove`（FR-EXPL-02 树根跟随 cwd）。对该路径做全量递归会扫到 `AppData`、缓存等海量条目，首扫极慢；即便完成，主目录上持续的 Access/Modify 事件会不断作废未完成的扫描（`explorer_scan_generation`），使 UI 长期甚至永久停在 indexing。

另有一处完成路径缺陷：`Ok(index)` 但 `index.root() != explorer_requested_root` 时直接 `return`，此前虽已把 `explorer_loading = false`，却未 `cx.notify()`，界面可能继续绘制旧的 loading 文案。

约束：继续使用 `ignore` + `notify`；不破坏 FR-EXPL-02；与进行中的 `explorer-resilience`（跳过不可读条目）兼容。

## Goals / Non-Goals

**Goals:**

- 打开任意合法工作区根后，文件列表在短时间内出现可用内容（至少根级子项），不得无限卡在 `indexing…`
- Watch 驱动的重建有防抖，且忽略 Access；大体量目录下不会因事件风暴永久 loading
- 扫描世代/取消语义正确：过期结果丢弃时不影响当前 loading；完成路径一律刷新 UI
- 保留 `.gitignore` / `.ignore` 尊重与现有跳过项反馈

**Non-Goals:**

- 不做真正的增量 FS 索引（diff apply）；本轮仍可全量重建，但必须可控
- 不改变树根跟随 cwd 的产品规则
- 不引入新的第三方依赖（除非现有 API 明显不足且 design 后续修订）
- 不解决模糊查找/grep 在超大目录上的性能目标（10 万文件验收仍属既有 M2 范围）

## Decisions

### 1. 两阶段索引：浅层首屏 + 后台深化

- **选择**：`FileIndex` 增加“最大深度 / 仅根级子项”构建路径（或 `build_shallow` + 后续 `deepen`）。UI 先应用浅层结果并清除“无列表”状态；完整索引在后台继续，完成后替换/`merge` 条目供展开与 fuzzy 使用。
- **理由**：卡死的直接体验是“没有任何行”；浅层 `read_dir` 级扫描对主目录也通常秒级完成。
- **备选**：纯懒加载（展开时再扫子目录）——交互模型改动更大，且 fuzzy 需要另建索引；本轮以两阶段全量为主，懒加载留作后续。

### 2. Watch：过滤 Access + 防抖合并重扫

- **选择**：`WorkspaceWatcher` 仅转发 `Create` / `Modify` / `Remove`（及必要的 `Any` 中带路径的变更）；`workspace_view` 在收到变更后启动短防抖（建议 300–500ms），窗口内多次事件合并为一次 `schedule_explorer_scan`。正在扫描时只标记 `pending_rescan`，扫描结束后若仍 pending 再扫一次，而不是每次事件都抬高 generation 打断当前扫描。
- **理由**：当前“有事件就立刻全量重扫 + Access 噪声”是大体量目录永久 indexing 的主因。
- **备选**：卸载大体量根上的 recursive watch——会破坏 FR-EXPL-03 的变更感知，否决。

### 3. Loading 状态机显式化

- **选择**：区分 `IndexingPhase::{Idle, Shallow, Deep}`（或等价 bool：`has_visible_index` + `deep_indexing`）。根标签旁的 `indexing…` 仅表示“后台仍在深化”，不得挡住已有列表。过期 generation 的回调不得改写当前 phase；根路径比较使用规范化相等（Windows 下注意 canonical/前缀差异时以“同请求 generation”为准，避免误丢结果）。
- **理由**：修复“有结果但不 notify / 路径不等导致界面假死”。

### 4. 超时与失败可见

- **选择**：深扫可设软超时（例如 30–60s）：超时后保留已有浅层/部分结果，清除 deep loading，可选 toast/页脚提示“索引未完成”。硬失败（根不是目录）保持现有错误文案。
- **理由**：比无限转圈更符合可诊断性；具体时长实现时可调。

## Risks / Trade-offs

- [浅层与深扫条目集合不一致导致闪烁] → 深扫结果原子替换；展开态按 path 保留（现有 `TreeState`）
- [防抖延迟导致新建文件稍晚出现] → 300–500ms 可接受；手动刷新按钮仍立即扫描
- [pending_rescan 在极端写负载下仍频繁全量扫] → 后续可加最小间隔；本轮先消除 Access + 打断式重入
- [与 `explorer-resilience` 并行改 `index.rs`] → 合并时保留 skip 收集逻辑；浅层构建同样走 skip 路径

## Migration Plan

- 纯行为修复，无设置/数据迁移
- 回滚：恢复单次阻塞式 `FileIndex::build` + 原 watcher 行为即可

## Open Questions

- 深扫软超时的默认值是否需要设置项？（默认实现常量即可，除非产品要求可配）
- 模糊查找是否必须等待深扫完成才可用？建议：深扫完成前 fuzzy 仅基于已索引子集，并在 UI 上不额外打断文件树

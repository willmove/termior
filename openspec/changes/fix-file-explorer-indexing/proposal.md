## Why

打开工作区（尤其是用户主目录等大体量路径）时，侧栏文件浏览器会长时间甚至永久停在 `indexing…`，文件列表始终不出现。截图复现路径为 `C:\Users\willmove`：终端 cwd 作为树根后触发全量递归索引，UI 在索引完成前无法展示任何条目，用户无法浏览文件。

## What Changes

- 文件浏览器在索引进行中也 SHALL 尽快展示可用的目录内容（先浅层/根级可见列表），不再用整面板空白 + `indexing…` 作为唯一状态
- 全量索引改为可取消、可世代作废的后台任务；新扫描开始时旧任务结果不得错误地覆盖或卡住 loading 状态
- 文件系统 watch 触发的重建 SHALL 防抖，并忽略无关事件（尤其是 `Access`），避免大体量目录下因频繁重扫而永远停在 indexing
- 扫描完成路径一律刷新 UI（修复结果被丢弃却未 `notify`、界面仍显示 indexing 的缺陷）
- 大体量目录下索引失败或超时时，面板 SHALL 显示可读错误/部分结果，而不是无限 loading

## Capabilities

### New Capabilities

- `explorer-responsive-indexing`: 文件浏览器索引的响应式行为——首屏可见性、后台扫描生命周期、watch 防抖/过滤，以及 loading 状态不得卡死

### Modified Capabilities

（无。`openspec/specs/` 目前为空；`explorer-resilience` 仅存在于进行中的 `ui-design-system-overhaul` change，本 change 不修改其需求，只补齐索引响应性与卡死修复。）

## Impact

**代码**

- `crates/termior-explorer/src/index.rs`：支持浅层/可中断构建或分阶段构建 API
- `crates/termior-explorer/src/watcher.rs`：事件过滤（至少忽略 `Access`）
- `crates/termior/src/workspace_view.rs`：`schedule_explorer_scan`、loading 状态机、watch 轮询防抖、首屏渲染与错误展示

**行为**

- FR-EXPL-02（树根跟随活动 tab cwd）保持不变；修复的是 cwd 落在大体量目录时的可用性
- FR-EXPL-03（后台 `ignore` 索引 + `notify`）补齐为：watch 驱动的更新必须防抖且不得导致永久 indexing；增量更新可作为后续深化，本 change 以“不卡死 + 首屏可用”为验收底线

**文档**

- 不强制修订 `docs/termior-spec.md`；若实现引入明确的浅层优先策略，可在实现后于 tasks 中补一句 FR-EXPL-03 说明

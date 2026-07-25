## 1. Explorer 索引 API

- [x] 1.1 在 `termior-explorer` 增加浅层构建 API（根下直接子项，或可配置 max_depth），复用现有 skip / gitignore 行为
- [x] 1.2 为浅层构建补充单测（临时目录含多层子树时，浅层不含深层文件；不可读子项仍跳过）
- [x] 1.3 确认全量 `FileIndex::build` / `refresh` 与浅层 API 可先后调用且结果可原子替换

## 2. Watch 过滤与防抖

- [x] 2.1 修改 `WorkspaceWatcher`：不再转发 `EventKind::Access`（仅 Create/Modify/Remove 等实质性变更）
- [x] 2.2 在 `WorkspaceView` 对 watch 触发的重建做短防抖，并实现 `pending_rescan`：扫描中不打断，结束后至多重扫一次
- [x] 2.3 为事件过滤和/或防抖语义补充可测覆盖（单元或轻量集成）

## 3. Workspace 扫描状态机

- [x] 3.1 重构 `schedule_explorer_scan`：先异步/同步应用浅层结果并展示列表，再后台深扫；区分“无列表”与“后台深化中”
- [x] 3.2 修复完成回调：过期 generation 安全丢弃；成功/失败路径一律 `cx.notify()`；避免根路径比较导致结果静默丢弃却界面仍显示 indexing
- [x] 3.3 为深扫增加软超时：超时保留已有列表、结束 deep-indexing 指示，必要时给出未完成提示
- [x] 3.4 更新侧栏渲染：有可见条目时允许交互；`indexing…` 仅表示深化进行中

## 4. 验证

- [x] 4.1 手动验证：打开用户主目录（或其它大体量路径）为树根时，短时间内出现文件列表且不会永久卡在 indexing
- [x] 4.2 手动验证：在工作区内创建/删除文件后，防抖重建仍能更新列表且 loading 能结束
- [x] 4.3 运行相关 crate 测试（`termior-explorer`、必要时 `termior`）确保通过

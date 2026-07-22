# 02 — 多窗口工作区切换 + 跨窗口 tab 管理

**What to build:** 用户可以打开多个主窗口、在不同工作区之间切换，并把一个 tab 从一个窗口拖/移到另一个窗口；关闭重开后布局能恢复。FR-WS 的多窗口扩展。

**Blocked by:** None — can start immediately.

**Status:** ready-for-agent

- [ ] 可新建/关闭多个独立主窗口，每个窗口绑定一个工作区
- [ ] 工作区可在窗口间切换，活动工作区状态（终端 cwd、Explorer 根、Agent 上下文）随之切换
- [ ] tab 可在窗口之间移动（拖拽或菜单），移动后缓冲/状态不丢
- [ ] 多窗口布局持久化，重启后恢复各窗口及其 tab/pane
- [ ] spec 6.1 / FR-WS 相关验收通过（三平台）

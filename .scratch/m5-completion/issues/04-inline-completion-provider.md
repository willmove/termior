# 04 — 行内补全 Provider 停顿调度 + ghost text 网络接线

**What to build:** 在编辑器里输入停顿时自动请求已配置的行内补全 Provider，把返回的补全以 ghost text 渲染，Tab 接受。FR-EDIT 行内补全项。

**Blocked by:** None — can start immediately.

**Status:** ready-for-human

- [x] 编辑器停顿（debounce）触发补全请求，连续输入时取消在途请求
- [x] 复用 FR-PROV 的 Provider 与密钥配置发起补全请求
- [x] 返回结果以 ghost text 形式渲染在光标处
- [x] Tab 接受、Esc 取消，接受后正确插入到 Rope/缓冲
- [x] 请求失败/超时静默降级，不打扰输入

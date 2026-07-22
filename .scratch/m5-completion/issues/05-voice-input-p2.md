# 05 — 语音输入（P2）

**What to build:** 采集麦克风音频，调用已配置 Provider 的转写端点，把返回文本插入当前焦点（Composer 输入框或编辑器）。优先级最低。

**Blocked by:** None — can start immediately.

**Status:** ready-for-agent

- [ ] 跨平台麦克风采集（三平台）
- [ ] 复用已配置 Provider 的语音转写端点（首选，不额外引入 STT 依赖）
- [ ] 转写文本插入当前焦点控件
- [ ] 录音/转写状态可见，可中途取消
- [ ] 无可用 Provider 时优雅降级

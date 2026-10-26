# 依赖与 GPUI 补丁维护

## 升级节奏

每个 minor 发布窗口检查直接依赖、Rust 工具链和 GPUI revision。安全修复可提前处理。每次升级独立审查 Cargo.lock、许可与行为变化，运行相关 tests、clippy 和三平台桌面/NFR 门禁后再发布。

Rust 固定在 `rust-toolchain.toml` 的 1.98.1；升级时同时更新 `clippy.toml` 的 `msrv`、CI 与贡献说明，不使用浮动 stable。GPUI 的所有引用和 vendored crate 必须来自同一 revision，遵循 ADR 0003。

## 2026 年 10 月 26 日核对

直接依赖 `thiserror` 从 1 升到 2，`dirs` 从 5 升到 6；版本复用锁文件内已有的 2.0.18 与 6.0.0，移除旧 dirs 及其不再使用的 Windows 0.48 依赖。上游仍可能间接依赖 thiserror 1。`sysinfo` 0.31 的升级留在独立 NFR 窗口，需对照进程、RSS、启动测量结果；不把版本变化当作性能改善证据。

`thiserror` 的主要兼容变化见 [2.0.0 release](https://github.com/dtolnay/thiserror/releases/tag/2.0.0)，升级以本仓库实际编译和回归为准。

### GPUI 补丁

基线为 Zed [`3565c49dad884e8437b86a1b1beaa76bfac67fc3`](https://github.com/zed-industries/zed/commit/3565c49dad884e8437b86a1b1beaa76bfac67fc3)。本次检查的上游为 [`08cbb88a152c3751a43e853425a70e9f631a5918`](https://github.com/zed-industries/zed/commit/08cbb88a152c3751a43e853425a70e9f631a5918)。

| 本地补丁 | 上游证据与处理 |
|---|---|
| Drop 前清理全部平台 callback | 上游 `WindowsWindow::drop` 仅清理 visibility callback，不能据此删除本地清理逻辑 |
| `IsWindow` 检查避免重复销毁 HWND | 上游 Drop 仍直接调用 `DestroyWindow`；保留本地检查 |
| 缺少 Graphics Tools 时安静禁用 DXGI debug probe | 保留；升级窗口比较 `directx_devices.rs` 并在有/无 Graphics Tools 的 Windows 桌面测试 |

未确认有覆盖全部本地补丁的上游 PR；不填造关联编号。[当前窗口关闭相关 issue #55477](https://github.com/zed-industries/zed/issues/55477) 仅作相关排障线索，不代表本地问题已被修复。源码比较见 [上游 window.rs](https://github.com/zed-industries/zed/blob/08cbb88a152c3751a43e853425a70e9f631a5918/crates/gpui_windows/src/window.rs)。

升级 GPUI 前先重放设置窗口打开/关闭、主窗口失焦/恢复、重复关闭和进程退出；检查 `window not found`、无效 HWND、进程残留与渲染。再运行三平台 smoke 和 NFR 启动/体积/RSS 门禁。

### cargo deny 的现有阻塞

升级前，cargo-deny 0.20.2 的 Windows 依赖图检查已报告许可失败：`gpui_shared_string`、`gpui_util` 缺少许可字段/可识别声明，`libbz2-rs-sys` 的 `bzip2-1.0.6` 不在允许列表，`zlog`、`ztracing`、`ztracing_macro` 声明 GPL-3.0-or-later。不能仅靠加入 GPL 或忽略未许可 crate 来宣称 MIT 项目的许可门禁通过；需要维护者核实授权或调整依赖。

完整跨平台图还需要下载当前机器未缓存的 Linux/macOS crate。本次 Windows 检查不能替代三平台完整检查；升级后的实际结果见 [issue 核对记录](github-issue-review-2026-10-26.md)。

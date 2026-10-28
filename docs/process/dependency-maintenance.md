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

### wgpu-hal 补丁

基线为 crates.io `wgpu-hal 29.0.4`，通过根 `Cargo.toml` 的 `[patch.crates-io]` 使用 `vendor/wgpu-hal`，版本不变。`src/gles/egl.rs` 在 GPUI 提供 XCB display handle、但旧 Mesa 只有 `EGL_EXT_platform_x11` 而没有 `EGL_EXT_platform_xcb` 时，回退到默认 X11 EGL display，避免误用 surfaceless 后没有可呈现窗口格式。现代 XCB 路径保持原样。桌面 OpenGL 上下文优先请求 4.5、失败时保留上游 3.3 回退，使存储缓冲 shader 与间接调用校验可使用核心能力。

`termior-ui::gpu::prepare` 同时在无可用 DRM render node 或用户偏好软件渲染时选择 Mesa EGL、llvmpipe 及 GL 4.5/GLSL 450 覆盖，且不覆盖显式环境变量。升级 wgpu 时必须比较上游 EGL 平台和上下文选择；只有上游具备等价兼容后才可移除补丁。移除前需在旧 Mesa/X11 上完成真实窗口 smoke，并在硬件 Vulkan 环境确认原路径无回归。

### GPUI wgpu 补丁

`vendor/gpui_wgpu` 来自同一 Zed revision，通过 `[patch."https://github.com/zed-industries/zed"]` 替换。`WgpuContext::instance` 尊重标准 `WGPU_BACKEND`，使软件回退只初始化 OpenGL，不再探测当前不可用的 Vulkan 驱动；未设置时仍采用上游 Vulkan + OpenGL 默认值。`RenderingParameters::new` 对 CPU OpenGL 使用单采样路径渲染，规避旧 Mesa 只按 `GL_MAX_SAMPLES` 宣告 4x MSAA、实际分配 renderbuffer 却失败的问题；硬件后端保留 4x/2x/1x 能力选择。

升级 GPUI/wgpu 时同步比较这两处。只有上游支持后端选择并正确处理旧 Mesa 的 MSAA 能力后才可移除；移除前必须重放旧 Mesa 与硬件 Vulkan 的真实窗口 smoke。

### cargo deny 的现有阻塞

升级前，cargo-deny 0.20.2 的 Windows 依赖图检查已报告许可失败：`gpui_shared_string`、`gpui_util` 缺少许可字段/可识别声明，`libbz2-rs-sys` 的 `bzip2-1.0.6` 不在允许列表，`zlog`、`ztracing`、`ztracing_macro` 声明 GPL-3.0-or-later。不能仅靠加入 GPL 或忽略未许可 crate 来宣称 MIT 项目的许可门禁通过；需要维护者核实授权或调整依赖。

### 逐项核实（2026-10-27）

本机没有 `cargo-deny`，所以下面这些结论来自**直接读取依赖自身的 manifest 与 LICENSE 文件**，
不是门禁输出：

| crate | `Cargo.toml` 的 `license` | 仓库内 LICENSE 文件 |
|---|---|---|
| `gpui` | `Apache-2.0` | `LICENSE-APACHE` → `../../LICENSE-APACHE` |
| `sum_tree` | `Apache-2.0` | `LICENSE-APACHE` → `../../LICENSE-APACHE` |
| `gpui_shared_string` | **无字段** | `LICENSE-APACHE` → `../../LICENSE-APACHE` |
| `gpui_util` | **无字段** | `LICENSE-APACHE` → `../../LICENSE-APACHE` |
| `zlog` | `GPL-3.0-or-later` | `LICENSE-GPL` → `../../LICENSE-GPL` |
| `ztracing` | `GPL-3.0-or-later` | `LICENSE-APACHE` → `../../LICENSE-APACHE` |
| `ztracing_macro` | `GPL-3.0-or-later` | `LICENSE-APACHE` → `../../LICENSE-APACHE` |
| `libbz2-rs-sys 0.2.5` | `bzip2-1.0.6`（非 SPDX） | `LICENSE` |

三点结论：

1. `gpui_shared_string` 与 `gpui_util` **缺的是元数据，不是授权**。两者各自指向仓库根的
   `LICENSE-APACHE`，与 `gpui` 声明的许可一致；cargo-deny 读不到 SPDX 字段就判 unlicensed。
2. `ztracing` 与 `ztracing_macro` 的 `Cargo.toml` 说 GPL，指向的却是 Apache 许可文件。这个
   不一致需要上游确认，但它**不改变** cargo-deny 读到的声明。
3. `bzip2-1.0.6` 是宽松许可，但 SPDX 解析器不认这个字符串。

`deny.toml` 未被修改：允许列表没有放宽，`exceptions` 仍为空。Audit 工作流仍排除
`termior`/`termior-ui`/`termior-ui-kit`，所以 **Audit 通过不代表完整桌面产物通过许可门禁**。

剩下的三项（GPL 怎么办、`bzip2-1.0.6` 怎么办、两个 GPUI crate 的元数据缺口是向上游提 PR
还是本地标 exception）是**分发决策**，记录在
[#64](https://github.com/willmove/termior/issues/64)，保持开放与 `ready-for-human`。


完整跨平台图还需要下载当前机器未缓存的 Linux/macOS crate。本次 Windows 检查不能替代三平台完整检查；升级后的实际结果见 [issue 核对记录](github-issue-review-2026-10-26.md)。

### CI 验收与跟踪项

提交 `c06f079` 的 [CI](https://github.com/willmove/termior/actions/runs/40288463484) 三平台 Core/Desktop/Context latency、体积与 NFR 回归全部通过。[Audit](https://github.com/willmove/termior/actions/runs/40288463499) 也通过；其既有范围排除了 `termior`、`termior-ui`、`termior-ui-kit`，不能据此声称完整桌面图许可通过。完整图的既有失败保留在 [#64](https://github.com/willmove/termior/issues/64)，由维护者核实授权或替换，不变更 deny 策略以掩盖失败。

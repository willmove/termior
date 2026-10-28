# Vendored GPUI patches

## `gpui_windows` (zed rev `3565c49`)

Termior depends on GPUI from Zed. Closing a secondary OS window on Windows
(settings) races as follows:

1. App calls `window.remove_window()` (or title-bar close → `on_window_should_close`).
2. GPUI removes the window from `App.windows` and drops `WindowsWindow`.
3. `WindowsWindow::drop` **asynchronously** schedules `DestroyWindow`.
4. Before the HWND dies, Win32 still delivers `WM_ACTIVATE` / `WM_PAINT`.
5. Those callbacks call `handle.update(...).log_err()` and print
   `[ERROR gpui::window] window not found`.

Upstream Drop also calls `DestroyWindow` again when DefWindowProc already
destroyed the HWND, which logs invalid-handle HRESULTs.

This vendored crate clears platform callbacks in `Drop` and skips
`DestroyWindow` when `IsWindow` is false. It is wired through the workspace
`[patch."https://github.com/zed-industries/zed"]` entry so Cargo substitutes
it for the git workspace member.

Also: in debug builds, probing `DXGIGetDebugInterface1` fails with
`DXGI_ERROR_SDK_COMPONENT_MISSING` (0x887A002D) when Windows optional feature
"Graphics Tools" is not installed. Upstream logs that probe with `.log_err()`;
we use `.ok()` instead so startup is quiet (DXGI debug features stay disabled,
same as before).

Upgrade cadence, upstream comparison and removal criteria are tracked in
[`docs/process/dependency-maintenance.md`](../docs/process/dependency-maintenance.md).

## `wgpu-hal` 29.0.4

Termior patches the crates.io release through `[patch.crates-io]` without
changing its version. In `src/gles/egl.rs`, an XCB display handle falls back to
the default X11 EGL display when Mesa exposes `EGL_EXT_platform_x11` but not
`EGL_EXT_platform_xcb`. This keeps old Mesa from incorrectly selecting a
surfaceless display and allows llvmpipe to present to GPUI's XCB windows.

The existing XCB platform path remains unchanged. Desktop OpenGL context
creation tries 4.5 before the original 3.3 fallback so storage-buffer shaders
and indirect-call validation work on Mesa implementations that default to 3.3.

`termior-ui::gpu::prepare` selects Mesa EGL, llvmpipe and the corresponding GL
4.5/GLSL 450 overrides only when software rendering is preferred or no
accessible DRM render node exists; explicit user environment variables still
take precedence.

On wgpu upgrades, compare the EGL platform and context selection and retain
this patch until equivalent compatibility exists upstream. Verify both the old
Mesa X11 path and a hardware-backed Vulkan path before removal.

## `gpui_wgpu` (zed rev `3565c49`)

This crate is substituted for the git workspace member through
`[patch."https://github.com/zed-industries/zed"]`. `WgpuContext::instance`
respects the standard `WGPU_BACKEND` variable so Termior's software fallback
can initialize only OpenGL instead of probing unusable Vulkan drivers.
Hardware-backed startup retains the original Vulkan + OpenGL default.

`RenderingParameters::new` uses single-sample path rendering for CPU OpenGL
adapters. Some older Mesa versions advertise 4x MSAA through `GL_MAX_SAMPLES`
but reject the actual renderbuffer allocation. Other backends keep the
original 4x/2x/1x capability selection.

Compare both changes on GPUI/wgpu upgrades and replay the real-window smoke on
old Mesa and hardware Vulkan before removing the local crate.

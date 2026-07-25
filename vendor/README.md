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

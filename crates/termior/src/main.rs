//! Termior desktop application entry point.

// Windows: GUI subsystem — no console window; closing a terminal won't kill the app.
// Smoke/NFR scripts that redirect stdout still receive println! via inherited pipes.
#![windows_subsystem = "windows"]

mod ai_diff_view;
mod app_identity;
mod background_image;
mod composer_view;
mod editor_view;
mod git_views;
mod keystroke;
mod markdown_preview_view;
mod preview_view;
mod settings_view;
mod terminal_view;
mod ui;
mod workspace_view;

use gpui::{
    px, size, App, AppContext, Bounds, Entity, KeyBinding, Window, WindowAppearance, WindowBounds,
};
use gpui_platform::application;
use std::{ffi::OsString, io::Write as _, path::PathBuf};
use workspace_view::{OpenWorkspace, WorkspaceView};

fn main() {
    install_panic_log();
    let _ = env_logger::try_init();
    let smoke_test = std::env::var_os("TERMIOR_SMOKE_TEST").is_some();
    let markdown_preview_smoke_test =
        std::env::var_os("TERMIOR_MARKDOWN_PREVIEW_SMOKE_TEST").is_some();
    let settings_close_smoke_test = std::env::var_os("TERMIOR_SETTINGS_CLOSE_SMOKE_TEST").is_some();
    let open_settings = std::env::var_os("TERMIOR_OPEN_SETTINGS").is_some();
    let nfr_measure = std::env::var_os("TERMIOR_NFR_MEASURE").is_some();
    let idle_redraw_probe_secs = std::env::var("TERMIOR_IDLE_REDRAW_PROBE")
        .ok()
        .and_then(|value| value.parse::<u64>().ok());
    let headless = smoke_test
        || markdown_preview_smoke_test
        || settings_close_smoke_test
        || nfr_measure
        || idle_redraw_probe_secs.is_some();
    let root = resolve_workspace_root(headless);
    application()
        .with_assets(termior_ui_kit::IconAssets)
        .run(move |cx: &mut App| {
            let bounds = Bounds::centered(None, size(px(1180.0), px(760.0)), cx);
            cx.open_window(
                app_identity::main_window_options(WindowBounds::Windowed(bounds)),
                |window, cx| {
                    let system_is_dark = matches!(
                        window.appearance(),
                        WindowAppearance::Dark | WindowAppearance::VibrantDark
                    );
                    let workspace: Entity<WorkspaceView> =
                        cx.new(|cx| WorkspaceView::new(root.clone(), system_is_dark, cx));
                    workspace.update(cx, |workspace, cx| {
                        workspace.restore_or_create_runtime(window, cx);
                        workspace.start_background_services(cx);
                        if markdown_preview_smoke_test {
                            workspace.start_markdown_preview_smoke(window, cx);
                        }
                    });
                    if settings_close_smoke_test {
                        workspace.update(cx, |workspace, cx| {
                            workspace.start_settings_close_smoke(cx);
                        });
                    } else if smoke_test && !markdown_preview_smoke_test {
                        schedule_smoke_exit(window);
                    } else if open_settings {
                        workspace.update(cx, |workspace, cx| {
                            let _ = workspace.open_settings_for_ui_shot(cx);
                        });
                    }
                    if let Some(secs) = idle_redraw_probe_secs {
                        workspace.update(cx, |workspace, cx| {
                            workspace.start_idle_redraw_probe(secs, cx);
                        });
                    }
                    if nfr_measure {
                        schedule_nfr_measurement(window);
                    }
                    workspace
                },
            )
            .expect("open Termior window");
            // 跨平台：macOS 用 `cmd-o`，Windows/Linux 用 `ctrl-o`。两条显式绑定
            // 与 Zed 自身 keymap 风格一致；action 挂在 `workspace-root` 上，
            // 窗口焦点在 app 内任意位置都触发。
            cx.bind_keys([
                KeyBinding::new("cmd-o", OpenWorkspace, None),
                KeyBinding::new("ctrl-o", OpenWorkspace, None),
            ]);
            cx.activate(true);
        });
}

/// NFR 测量模式（`TERMIOR_NFR_MEASURE=1`，由 `termior-bench` 的 `nfr-run` 驱动）。
///
/// 协议（stdout 行，供 harness 采集）：
/// - 第一帧渲染时打印 `TERMIOR_NFR_FIRST_FRAME` → 冷启动终点（NFR-01）。
/// - 之后采样 `NFR_FPS_SAMPLE_SECS` 秒帧数，打印 `TERMIOR_NFR_FPS=NN` → 稳态帧率
///   （NFR-03），随后 `cx.quit()` 退出。RSS 由 harness 用 sysinfo 旁路采集（NFR-04）。
///
/// 这是冒烟测试（`schedule_smoke_exit`）的性能采样对偶：smoke 验「能启动」，
/// NFR 量「有多快/多重」。绝对值依赖硬件，门禁只看相对基线回归（见 docs/nfr-baselines.md）。
///
/// `on_next_frame` 只在注册的下一帧触发一次；要持续采样就用共享状态自重注册。
fn schedule_nfr_measurement(window: &mut Window) {
    let state = std::rc::Rc::new(std::cell::RefCell::new(NfrFrameState::start()));
    nfr_next_frame(window, state);
}

fn nfr_next_frame(window: &mut Window, state: std::rc::Rc<std::cell::RefCell<NfrFrameState>>) {
    window.on_next_frame(move |window, cx| {
        let action = state.borrow_mut().on_frame();
        match action {
            NfrAction::Continue => nfr_next_frame(window, state),
            NfrAction::Quit => cx.quit(),
        }
    });
}

const NFR_FPS_SAMPLE_SECS: f64 = 1.5;

struct NfrFrameState {
    first_frame_done: bool,
    sample_start: Option<std::time::Instant>,
    frame_count: u32,
}

enum NfrAction {
    Continue,
    Quit,
}

impl NfrFrameState {
    fn start() -> Self {
        Self {
            first_frame_done: false,
            sample_start: None,
            frame_count: 0,
        }
    }

    fn on_frame(&mut self) -> NfrAction {
        if !self.first_frame_done {
            self.first_frame_done = true;
            println!("TERMIOR_NFR_FIRST_FRAME");
            // 从下一帧开始计 FPS 采样窗口，避免把首帧的冷路径计入稳态。
            self.sample_start = Some(std::time::Instant::now());
            return NfrAction::Continue;
        }
        self.frame_count += 1;
        if let Some(start) = self.sample_start {
            let elapsed = start.elapsed().as_secs_f64();
            if elapsed >= NFR_FPS_SAMPLE_SECS {
                let fps = (self.frame_count as f64 / elapsed).round();
                println!("TERMIOR_NFR_FPS={fps:.0}");
                return NfrAction::Quit;
            }
        }
        NfrAction::Continue
    }
}

fn schedule_smoke_exit(window: &mut Window) {
    window.on_next_frame(|window, _| {
        window.refresh();
        window.on_next_frame(|window, _| {
            window.refresh();
            window.on_next_frame(|_, cx| {
                println!("TERMIOR_SMOKE_OK");
                cx.quit();
            });
        });
    });
}

fn resolve_workspace_root(smoke_test: bool) -> PathBuf {
    if let Some(path) = workspace_from_args(std::env::args_os().skip(1))
        .or_else(|| std::env::var_os("TERMIOR_WORKSPACE").map(PathBuf::from))
        .and_then(valid_workspace)
    {
        return path;
    }

    if let Ok(current) = std::env::current_dir() {
        let looks_like_project = current.join(".git").exists()
            || current.join("Cargo.toml").is_file()
            || current.join("package.json").is_file()
            || current.join("pyproject.toml").is_file();
        if looks_like_project || smoke_test {
            return current.canonicalize().unwrap_or(current);
        }
    }

    if let Some(root) = last_workspace_root() {
        return root;
    }

    if !smoke_test {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Open a Termior workspace")
            .pick_folder()
            .and_then(valid_workspace)
        {
            return path;
        }
    }

    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn workspace_from_args(args: impl IntoIterator<Item = OsString>) -> Option<PathBuf> {
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        if argument == "--workspace" || argument == "-w" {
            return args.next().map(PathBuf::from);
        }
        if let Some(value) = argument.to_string_lossy().strip_prefix("--workspace=") {
            return Some(PathBuf::from(value));
        }
        if !argument.to_string_lossy().starts_with('-') {
            return Some(PathBuf::from(argument));
        }
    }
    None
}

fn valid_workspace(path: PathBuf) -> Option<PathBuf> {
    path.is_dir().then(|| path.canonicalize().unwrap_or(path))
}

fn last_workspace_root() -> Option<PathBuf> {
    let path = termior_store::app_data_dir()
        .ok()?
        .join("Termior-workspaces.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let state: termior_ui::WorkspaceState = serde_json::from_str(&raw).ok()?;
    valid_workspace(state.root)
}

fn install_panic_log() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(directory) = termior_store::app_data_dir() {
            let _ = std::fs::create_dir_all(&directory);
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(directory.join("Termior-crash.log"))
            {
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let _ = writeln!(file, "[{timestamp}] {info}");
            }
        }
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_argument_forms_are_supported() {
        assert_eq!(
            workspace_from_args([OsString::from("--workspace"), OsString::from("demo")]),
            Some(PathBuf::from("demo"))
        );
        assert_eq!(
            workspace_from_args([OsString::from("--workspace=demo")]),
            Some(PathBuf::from("demo"))
        );
        assert_eq!(
            workspace_from_args([OsString::from("demo")]),
            Some(PathBuf::from("demo"))
        );
    }
}

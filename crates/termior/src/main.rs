//! Termior desktop application entry point.

// Windows: GUI subsystem — no console window; closing a terminal won't kill the app.
// Smoke/NFR scripts that redirect stdout still receive println! via inherited pipes.
#![windows_subsystem = "windows"]

// 视图层已实现于 `termior-ui` crate（spec §5.2）；入口只保留进程装配、
// smoke/NFR 探针与崩溃日志。
use termior_ui::t;

use gpui::{
    px, size, App, AppContext, Bounds, Entity, KeyBinding, Window, WindowAppearance, WindowBounds,
};
use gpui_platform::application;
use std::{ffi::OsString, io::Write as _, path::PathBuf};
use termior_ui::workspace_view::{self, OpenWorkspace, WorkspaceView};

fn main() {
    let process_start = std::time::Instant::now();
    if std::env::var_os("TERMIOR_SSH_ASKPASS").as_deref() == Some(std::ffi::OsStr::new("1")) {
        let prefer_software = workspace_view::load_settings().0.prefer_software_rendering;
        termior_ui::gpu::prepare(prefer_software);
        termior_ui::ssh_askpass::run();
    }
    install_panic_log();
    let _ = env_logger::try_init();
    // wgpu 会同时探测 Vulkan 和 GLES。无 DRM render node 的 Linux（虚拟机、
    // 未加入 video/render 组）上，GLES/EGL 探测会打出 Mesa DRI2/ZINK 错误，
    // 尽管 lavapipe Vulkan 仍能出窗。必须在创建 GPUI 实例之前设置。
    let (preset, _, _) = workspace_view::load_settings();
    termior_ui::gpu::prepare(preset.prefer_software_rendering);
    let smoke_test = std::env::var_os("TERMIOR_SMOKE_TEST").is_some();
    let nfr_measure = std::env::var_os("TERMIOR_NFR_MEASURE").is_some();
    // NFR-01 分阶段计时：从 main 入口起算（不含进程加载/动态链接，那部分由
    // harness 的「spawn → 首帧」总时长覆盖），用于定位冷启动耗时花在哪一段。
    let phase = move |name: &str| {
        if nfr_measure {
            termior_ui::nfr::emit(&format!(
                "TERMIOR_NFR_PHASE {name}={:.1}",
                process_start.elapsed().as_secs_f64() * 1000.0
            ));
        }
    };
    // 文件夹选择对话框可能在任何视图渲染前出现：先按持久化的语言设置
    //（缺省则系统语言）初始化 i18n，保证对话框文案跟随界面语言。
    // WorkspaceView::new 会用同一来源再初始化一次，幂等。
    termior_i18n::init(preset.language.as_deref());
    phase("settings");
    let markdown_preview_smoke_test =
        std::env::var_os("TERMIOR_MARKDOWN_PREVIEW_SMOKE_TEST").is_some();
    let settings_close_smoke_test = std::env::var_os("TERMIOR_SETTINGS_CLOSE_SMOKE_TEST").is_some();
    let split_pane_smoke_test = std::env::var_os("TERMIOR_SPLIT_PANE_SMOKE").is_some();
    let open_settings = std::env::var_os("TERMIOR_OPEN_SETTINGS").is_some();
    let nfr_echo_samples = std::env::var("TERMIOR_NFR_ECHO_SAMPLES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|samples| *samples > 0);
    let shell_picker_smoke = std::env::var_os("TERMIOR_SHELL_PICKER_SMOKE").is_some();
    let idle_redraw_probe_secs = std::env::var("TERMIOR_IDLE_REDRAW_PROBE")
        .ok()
        .and_then(|value| value.parse::<u64>().ok());
    let headless = smoke_test
        || markdown_preview_smoke_test
        || settings_close_smoke_test
        || split_pane_smoke_test
        || nfr_measure
        || idle_redraw_probe_secs.is_some();
    let root = resolve_workspace_root(headless);
    phase("workspace_root");
    application()
        .with_assets(termior_ui_kit::IconAssets)
        .run(move |cx: &mut App| {
            phase("app_init");
            // 必须在建窗前解析：GPUI 找不到 family 时会静默回退到比例字体，
            // 终端网格宽和字形 advance 就会对不上（见 monospace_font 模块注释）。
            let text_system = cx.text_system().clone();
            termior_ui::monospace_font::init_default(&text_system);
            phase("fonts");
            let bounds = Bounds::centered(None, size(px(1180.0), px(760.0)), cx);
            let open_result = cx.open_window(
                termior_ui::app_identity::main_window_options(WindowBounds::Windowed(bounds)),
                |window, cx| {
                    let system_is_dark = matches!(
                        window.appearance(),
                        WindowAppearance::Dark | WindowAppearance::VibrantDark
                    );
                    phase("window_created");
                    let workspace: Entity<WorkspaceView> =
                        cx.new(|cx| WorkspaceView::new(root.clone(), system_is_dark, cx));
                    phase("workspace_view");
                    workspace.update(cx, |workspace, cx| {
                        workspace.restore_or_create_runtime(window, cx);
                        workspace.start_background_services(cx);
                        if markdown_preview_smoke_test {
                            workspace.start_markdown_preview_smoke(window, cx);
                        }
                    });
                    phase("runtime_restored");
                    if settings_close_smoke_test {
                        workspace.update(cx, |workspace, cx| {
                            workspace.start_settings_close_smoke(cx);
                        });
                    } else if split_pane_smoke_test {
                        workspace.update(cx, |workspace, cx| {
                            workspace.start_split_pane_smoke(window, cx);
                        });
                    } else if smoke_test && !markdown_preview_smoke_test {
                        schedule_smoke_exit(window);
                    } else if open_settings {
                        workspace.update(cx, |workspace, cx| {
                            let _ = workspace.open_settings_for_ui_shot(cx);
                        });
                    }
                    if shell_picker_smoke {
                        workspace.update(cx, |workspace, cx| {
                            workspace.start_shell_picker_probe(window, cx);
                        });
                    }
                    if let Some(secs) = idle_redraw_probe_secs {
                        workspace.update(cx, |workspace, cx| {
                            workspace.start_idle_redraw_probe(secs, cx);
                        });
                    }
                    if std::env::var_os("TERMIOR_OPEN_SSH_MANAGER").is_some() {
                        let workspace = workspace.clone();
                        window.on_next_frame(move |window, cx| {
                            workspace
                                .update(cx, |workspace, cx| workspace.open_ssh_manager(window, cx));
                        });
                    }
                    if nfr_measure {
                        schedule_nfr_measurement(window, workspace.clone(), nfr_echo_samples);
                    }
                    workspace
                },
            );
            if let Err(error) = open_result {
                // 主窗口创建失败无法降级为可用应用：显式报错退出，不 panic。
                eprintln!("failed to open Termior window: {error}");
                std::process::exit(1);
            }
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
/// - `TERMIOR_NFR_PHASE name=ms`：冷启动各阶段相对 main 入口的时间戳（NFR-01 定位用）。
/// - `TERMIOR_NFR_FIRST_FRAME`：首帧**上屏之后**打印 → 冷启动终点（NFR-01）。
///   GPUI 的 next-frame 回调在同一 tick 的 draw 之前执行，所以在第二次回调里打印，
///   才能保证第一帧已经 draw + present（最多多计一个刷新间隔，偏保守）。
/// - `TERMIOR_NFR_FPS=NN` / `TERMIOR_NFR_FRAME_P99_MS=NN.N`：采样期间**每个 tick 都强制
///   整窗重绘**（`window.refresh()` 绕过视图缓存），统计真实 draw 的帧率与帧间隔 p99
///   （NFR-03）。只数 next-frame 回调而不重绘，量到的是显示器刷新节拍而非渲染开销——
///   空闲零重绘下根本没有帧被画出来。
/// - `TERMIOR_NFR_WINDOW_ACTIVE=0|1`：GPUI 对非前台窗口把帧间隔限到 ~33ms，
///   CI 上窗口拿不到前台时帧率上限约 30fps，据此判读。
/// - 可选 `TERMIOR_NFR_ECHO_P99_MS`：`TERMIOR_NFR_ECHO_SAMPLES=N` 时帧率采样后接着跑
///   键入回显探针（NFR-02），由 workspace 打印结果并退出；否则直接 `cx.quit()`。
///
/// RSS 由 harness 用 sysinfo 旁路采集（NFR-04）。绝对值依赖硬件，门禁只看相对基线
/// 回归（见 docs/nfr-baselines.md）。
///
/// `on_next_frame` 只在注册的下一帧触发一次；要持续采样就用共享状态自重注册。
fn schedule_nfr_measurement(
    window: &mut Window,
    workspace: Entity<WorkspaceView>,
    echo_samples: Option<usize>,
) {
    let state = std::rc::Rc::new(std::cell::RefCell::new(NfrFrameState::start()));
    nfr_next_frame(window, state, workspace, echo_samples);
}

fn nfr_next_frame(
    window: &mut Window,
    state: std::rc::Rc<std::cell::RefCell<NfrFrameState>>,
    workspace: Entity<WorkspaceView>,
    echo_samples: Option<usize>,
) {
    window.on_next_frame(move |window, cx| {
        let action = state
            .borrow_mut()
            .on_frame(std::time::Instant::now(), window.is_window_active());
        match action {
            NfrAction::Continue { redraw } => {
                if redraw {
                    window.refresh();
                }
                nfr_next_frame(window, state, workspace, echo_samples);
            }
            NfrAction::Done => match echo_samples {
                Some(samples) => workspace.update(cx, |workspace, cx| {
                    workspace.start_nfr_echo_probe(samples, cx);
                }),
                None => cx.quit(),
            },
        }
    });
}

const NFR_FPS_SAMPLE_SECS: f64 = 1.5;

enum NfrPhase {
    /// 等待第一次回调（其后本 tick 画出首帧）。
    AwaitFirstDraw,
    /// 首帧已 draw；下一次回调时它已上屏。
    AwaitFirstPresent,
    /// 强制重绘采样中。
    Sampling {
        start: std::time::Instant,
        last: std::time::Instant,
    },
}

struct NfrFrameState {
    phase: NfrPhase,
    intervals_ms: Vec<f64>,
    window_active: bool,
}

#[derive(Debug, PartialEq)]
enum NfrAction {
    Continue { redraw: bool },
    Done,
}

impl NfrFrameState {
    fn start() -> Self {
        Self {
            phase: NfrPhase::AwaitFirstDraw,
            intervals_ms: Vec::new(),
            window_active: true,
        }
    }

    fn on_frame(&mut self, now: std::time::Instant, window_active: bool) -> NfrAction {
        match self.phase {
            NfrPhase::AwaitFirstDraw => {
                self.phase = NfrPhase::AwaitFirstPresent;
                NfrAction::Continue { redraw: false }
            }
            NfrPhase::AwaitFirstPresent => {
                termior_ui::nfr::emit("TERMIOR_NFR_FIRST_FRAME");
                // 从这里开始计采样窗口，避免把首帧冷路径计入稳态。
                self.phase = NfrPhase::Sampling {
                    start: now,
                    last: now,
                };
                NfrAction::Continue { redraw: true }
            }
            NfrPhase::Sampling { start, last } => {
                self.intervals_ms
                    .push(now.duration_since(last).as_secs_f64() * 1000.0);
                self.window_active &= window_active;
                self.phase = NfrPhase::Sampling { start, last: now };
                let elapsed = now.duration_since(start).as_secs_f64();
                if elapsed < NFR_FPS_SAMPLE_SECS {
                    return NfrAction::Continue { redraw: true };
                }
                let fps = (self.intervals_ms.len() as f64 / elapsed).round();
                termior_ui::nfr::emit(&format!("TERMIOR_NFR_FPS={fps:.0}"));
                if let Some(p99) = termior_ui::nfr::percentile(&self.intervals_ms, 0.99) {
                    termior_ui::nfr::emit(&format!("TERMIOR_NFR_FRAME_P99_MS={p99:.1}"));
                }
                termior_ui::nfr::emit(&format!(
                    "TERMIOR_NFR_WINDOW_ACTIVE={}",
                    u8::from(self.window_active)
                ));
                NfrAction::Done
            }
        }
    }
}

fn schedule_smoke_exit(window: &mut Window) {
    window.on_next_frame(|window, _| {
        window.refresh();
        window.on_next_frame(|window, _| {
            window.refresh();
            window.on_next_frame(|_, cx| {
                println!("TERMIOR_SMOKE_OK");
                let _ = std::io::Write::flush(&mut std::io::stdout());
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
            return normal_path(current.canonicalize().unwrap_or(current));
        }
    }

    if let Some(root) = last_workspace_root() {
        return root;
    }

    if !smoke_test {
        if let Some(path) = rfd::FileDialog::new()
            .set_title(t!("chrome.open_workspace_dialog"))
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
    path.is_dir()
        .then(|| path.canonicalize().unwrap_or(path))
        .map(normal_path)
}

/// Windows 的 `canonicalize` 返回 `\\?\` 扩展长度（verbatim）路径。这种形态若作为
/// workspace root 存盘并传给 PTY 当 cwd，PowerShell 提示符会显示成
/// `Microsoft.PowerShell.Core\Filesystem::\\?\C:\...`。这里统一剥掉前缀，
/// 恢复普通 Win32 路径形态；非 Windows 平台原样返回。
fn normal_path(path: PathBuf) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path
    }
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

    #[test]
    fn nfr_first_frame_waits_for_present_and_sampling_forces_redraw() {
        use std::time::{Duration, Instant};
        let mut state = NfrFrameState::start();
        let t0 = Instant::now();
        // 第一次回调：首帧还没 draw，不打点也不强制重绘。
        assert_eq!(
            state.on_frame(t0, true),
            NfrAction::Continue { redraw: false }
        );
        // 第二次回调：首帧已上屏，开始强制重绘采样。
        assert_eq!(
            state.on_frame(t0 + Duration::from_millis(16), true),
            NfrAction::Continue { redraw: true }
        );
        let mut now = t0 + Duration::from_millis(16);
        let mut action = NfrAction::Continue { redraw: true };
        while action != NfrAction::Done {
            now += Duration::from_millis(20);
            action = state.on_frame(now, false);
        }
        assert!(!state.window_active);
        assert!(state
            .intervals_ms
            .iter()
            .all(|interval| (*interval - 20.0).abs() < 0.5));
    }

    #[test]
    fn normal_path_strips_windows_verbatim_prefix() {
        assert_eq!(
            normal_path(PathBuf::from(r"\\?\C:\Coding\ToolsProjects\termior")),
            PathBuf::from(r"C:\Coding\ToolsProjects\termior")
        );
        assert_eq!(
            normal_path(PathBuf::from(r"\\?\UNC\server\share\dir")),
            PathBuf::from(r"\\server\share\dir")
        );
        assert_eq!(
            normal_path(PathBuf::from("/home/user/project")),
            PathBuf::from("/home/user/project")
        );
    }
}

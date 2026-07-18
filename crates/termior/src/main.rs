//! Termior desktop application entry point.

mod ai_diff_view;
mod app_identity;
mod composer_view;
mod editor_view;
mod git_views;
mod keystroke;
mod preview_view;
mod settings_view;
mod terminal_view;
mod workspace_view;

use gpui::{px, size, App, AppContext, Bounds, Entity, Window, WindowBounds};
use gpui_platform::application;
use std::ffi::OsString;
use std::io::Write as _;
use std::path::PathBuf;
use workspace_view::WorkspaceView;

fn main() {
    install_panic_log();
    let _ = env_logger::try_init();
    let smoke_test = std::env::var_os("TERMIOR_SMOKE_TEST").is_some();
    let root = resolve_workspace_root(smoke_test);
    application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1180.0), px(760.0)), cx);
        cx.open_window(
            app_identity::window_options(WindowBounds::Windowed(bounds)),
            |window, cx| {
                let workspace: Entity<WorkspaceView> =
                    cx.new(|cx| WorkspaceView::new(root.clone(), cx));
                workspace.update(cx, |workspace, cx| {
                    workspace.restore_or_create_runtime(window, cx);
                    workspace.start_background_services(cx);
                });
                if smoke_test {
                    schedule_smoke_exit(window);
                }
                workspace
            },
        )
        .expect("open Termior window");
        cx.activate(true);
    });
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

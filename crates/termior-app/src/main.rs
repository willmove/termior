//! Termior desktop application entry point.

mod composer_view;
mod editor_view;
mod keystroke;
mod preview_view;
mod settings_view;
mod terminal_view;
mod workspace_view;

use gpui::{px, size, App, AppContext, Bounds, Entity, WindowBounds, WindowOptions};
use gpui_platform::application;
use workspace_view::WorkspaceView;

fn main() {
    let _ = env_logger::try_init();
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1180.0), px(760.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_window, cx| {
                let root =
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                let workspace: Entity<WorkspaceView> = cx.new(|cx| WorkspaceView::new(root, cx));
                workspace.update(cx, |workspace, cx| workspace.restore_or_create_terminal(cx));
                workspace
            },
        )
        .expect("open Termior window");
        cx.activate(true);
    });
}

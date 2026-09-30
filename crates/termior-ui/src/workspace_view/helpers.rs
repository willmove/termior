//! 界面辅助函数：菜单项、图标、颜色映射与布局小工具。

use super::explorer_actions::{
    ExplorerContextAction, ExplorerContextTarget, ExplorerContextTargetKind,
};
use super::*;

pub(super) fn single_pane(content: PaneContent) -> HashMap<PaneId, PaneContent> {
    HashMap::from([(PaneId(1), content)])
}

/// 判断窗口坐标 `pos` 是否落在 CSD 缩放环带上（边或 1.5 倍环带宽的角），
/// 在则返回应缩放的边（对齐 Zed `workspace::resize_edge`）。tiled 的边不缩放。
pub(super) fn resize_edge(
    pos: Point<Pixels>,
    band: Pixels,
    window_size: Size<Pixels>,
    tiling: Tiling,
) -> Option<ResizeEdge> {
    let inner = Bounds::new(Point::default(), window_size).inset(band * 1.5);
    if inner.contains(&pos) {
        return None;
    }

    let corner = size(band * 1.5, band * 1.5);
    let top_left = Bounds::new(Point::new(px(0.0), px(0.0)), corner);
    if !tiling.top && top_left.contains(&pos) {
        return Some(ResizeEdge::TopLeft);
    }
    let top_right = Bounds::new(
        Point::new(window_size.width - corner.width, px(0.0)),
        corner,
    );
    if !tiling.top && top_right.contains(&pos) {
        return Some(ResizeEdge::TopRight);
    }
    let bottom_left = Bounds::new(
        Point::new(px(0.0), window_size.height - corner.height),
        corner,
    );
    if !tiling.bottom && bottom_left.contains(&pos) {
        return Some(ResizeEdge::BottomLeft);
    }
    let bottom_right = Bounds::new(
        Point::new(
            window_size.width - corner.width,
            window_size.height - corner.height,
        ),
        corner,
    );
    if !tiling.bottom && bottom_right.contains(&pos) {
        return Some(ResizeEdge::BottomRight);
    }

    if !tiling.top && pos.y < band {
        Some(ResizeEdge::Top)
    } else if !tiling.bottom && pos.y > window_size.height - band {
        Some(ResizeEdge::Bottom)
    } else if !tiling.left && pos.x < band {
        Some(ResizeEdge::Left)
    } else if !tiling.right && pos.x > window_size.width - band {
        Some(ResizeEdge::Right)
    } else {
        None
    }
}

/// 给 CSD 窗口/内容元素加圆角：只圆「两条邻边都未 tiled」的角
/// （对齐 Zed `theme::ClientDecorationsExt::rounded_client_corners`）。
pub(super) fn rounded_client_corners<S: Styled>(element: S, tiling: &Tiling) -> S {
    let mut element = element;
    if !tiling.top && !tiling.left {
        element = element.rounded_tl(WINDOW_ROUNDING);
    }
    if !tiling.top && !tiling.right {
        element = element.rounded_tr(WINDOW_ROUNDING);
    }
    if !tiling.bottom && !tiling.left {
        element = element.rounded_bl(WINDOW_ROUNDING);
    }
    if !tiling.bottom && !tiling.right {
        element = element.rounded_br(WINDOW_ROUNDING);
    }
    element
}

pub(super) fn new_preview_view(
    url: String,
    cx: &mut Context<WorkspaceView>,
) -> Entity<PreviewView> {
    cx.new(|_| PreviewView::new(url))
}

pub(super) fn explorer_entry_visible(entry: &FileEntry, tree: &TreeState) -> bool {
    let root = tree.root();
    let mut parent = entry.path.parent();
    while let Some(path) = parent {
        if path == root {
            return true;
        }
        if !path.starts_with(root) || !tree.is_expanded(path) {
            return false;
        }
        parent = path.parent();
    }
    false
}

pub(super) fn explorer_content_width(entries: &[FileEntry]) -> f32 {
    entries
        .iter()
        .map(|entry| {
            let name = entry
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&entry.relative);
            entry.depth as f32 * 12.0
                + 42.0
                + name
                    .chars()
                    .map(|character| if character.is_ascii() { 7.0 } else { 12.0 })
                    .sum::<f32>()
        })
        .fold(1.0, f32::max)
}

/// 文件树图标：Lucide SVG，颜色取自主题（禁止硬编码 rgba）。
pub(super) fn explorer_icon(kind: IconKind, expanded: bool, p: &ResolvedPalette) -> gpui::Svg {
    let (glyph, tint) = match kind {
        IconKind::Folder if expanded => (Icon::FolderOpen, ui::color(p.accent)),
        IconKind::Folder => (Icon::Folder, ui::color(p.accent)),
        IconKind::Rust
        | IconKind::JavaScript
        | IconKind::TypeScript
        | IconKind::Python
        | IconKind::Go
        | IconKind::Java
        | IconKind::Html
        | IconKind::Css => (Icon::FileCode, ui::alpha(p.foreground, 0.78)),
        IconKind::Json => (Icon::Braces, ui::alpha(p.foreground, 0.78)),
        IconKind::Markdown | IconKind::File => (Icon::FileText, ui::alpha(p.foreground, 0.72)),
        IconKind::Image => (Icon::FileImage, ui::alpha(p.foreground, 0.78)),
        IconKind::Config => (Icon::FileCog, ui::alpha(p.foreground, 0.78)),
    };
    icon(glyph, icon_size::SM, tint)
}

pub(super) fn remote_icon_kind(name: &str, is_dir: bool) -> IconKind {
    if is_dir {
        return IconKind::Folder;
    }
    let lower = name.to_ascii_lowercase();
    let extension = lower.rsplit_once('.').map(|(_, extension)| extension);
    match extension {
        Some("rs") => IconKind::Rust,
        Some("js" | "jsx" | "mjs" | "cjs") => IconKind::JavaScript,
        Some("ts" | "tsx" | "mts" | "cts") => IconKind::TypeScript,
        Some("py" | "pyi") => IconKind::Python,
        Some("go") => IconKind::Go,
        Some("java") => IconKind::Java,
        Some("html" | "htm") => IconKind::Html,
        Some("css" | "scss" | "sass" | "less") => IconKind::Css,
        Some("json" | "jsonc") => IconKind::Json,
        Some("md" | "markdown") => IconKind::Markdown,
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg") => IconKind::Image,
        Some("toml" | "yaml" | "yml" | "ini") => IconKind::Config,
        _ => IconKind::File,
    }
}

pub(super) fn explorer_tool_button(
    id: &'static str,
    glyph: Icon,
    label: impl Into<SharedString>,
    p: &ResolvedPalette,
    cx: &mut Context<WorkspaceView>,
    listener: impl Fn(&mut WorkspaceView, &mut Context<WorkspaceView>) + 'static,
) -> impl IntoElement {
    ui::icon_button(id, glyph, label, p).on_mouse_down(
        MouseButton::Left,
        cx.listener(move |workspace, _event, window, cx| {
            window.focus(&workspace.focus_handle, cx);
            listener(workspace, cx);
        }),
    )
}

pub(super) fn explorer_context_actions(
    target: &ExplorerContextTarget,
) -> &'static [ExplorerContextAction] {
    if target.is_remote() {
        return match target.kind() {
            ExplorerContextTargetKind::File => &[
                ExplorerContextAction::Download,
                ExplorerContextAction::Rename,
                ExplorerContextAction::Move,
                ExplorerContextAction::Delete,
                ExplorerContextAction::Refresh,
            ],
            ExplorerContextTargetKind::Directory => &[
                ExplorerContextAction::Open,
                ExplorerContextAction::CreateFile,
                ExplorerContextAction::CreateDirectory,
                ExplorerContextAction::UploadFile,
                ExplorerContextAction::UploadDirectory,
                ExplorerContextAction::Download,
                ExplorerContextAction::Rename,
                ExplorerContextAction::Move,
                ExplorerContextAction::Delete,
                ExplorerContextAction::Refresh,
            ],
            ExplorerContextTargetKind::Workspace => &[
                ExplorerContextAction::CreateFile,
                ExplorerContextAction::CreateDirectory,
                ExplorerContextAction::UploadFile,
                ExplorerContextAction::UploadDirectory,
                ExplorerContextAction::Refresh,
            ],
        };
    }
    match target.kind() {
        ExplorerContextTargetKind::File => &[
            ExplorerContextAction::Open,
            ExplorerContextAction::Rename,
            ExplorerContextAction::Delete,
            ExplorerContextAction::Reveal,
            ExplorerContextAction::AttachToAi,
            ExplorerContextAction::Refresh,
        ],
        ExplorerContextTargetKind::Directory => &[
            ExplorerContextAction::CreateFile,
            ExplorerContextAction::CreateDirectory,
            ExplorerContextAction::Rename,
            ExplorerContextAction::Delete,
            ExplorerContextAction::Reveal,
            ExplorerContextAction::Refresh,
        ],
        ExplorerContextTargetKind::Workspace => &[
            ExplorerContextAction::CreateFile,
            ExplorerContextAction::CreateDirectory,
            ExplorerContextAction::FindFile,
            ExplorerContextAction::SearchContent,
            ExplorerContextAction::Reveal,
            ExplorerContextAction::Refresh,
        ],
    }
}

pub(super) fn explorer_context_action_label(action: ExplorerContextAction) -> SharedString {
    match action {
        ExplorerContextAction::Open => t!("ws.ctx_open"),
        ExplorerContextAction::CreateFile => t!("ws.ctx_new_file"),
        ExplorerContextAction::CreateDirectory => t!("ws.ctx_new_folder"),
        ExplorerContextAction::Rename => t!("ws.ctx_rename"),
        ExplorerContextAction::Move => t!("ws.ctx_move"),
        ExplorerContextAction::Delete => t!("ws.ctx_delete"),
        ExplorerContextAction::Reveal => t!("ws.ctx_reveal"),
        ExplorerContextAction::AttachToAi => t!("ws.ctx_attach"),
        ExplorerContextAction::FindFile => t!("ws.ctx_find_file"),
        ExplorerContextAction::SearchContent => t!("ws.ctx_search_files"),
        ExplorerContextAction::UploadFile => t!("ws.ctx_upload_file"),
        ExplorerContextAction::UploadDirectory => t!("ws.ctx_upload_folder"),
        ExplorerContextAction::Download => t!("ws.ctx_download"),
        ExplorerContextAction::Refresh => t!("ws.ctx_refresh"),
    }
}

pub(super) fn dir_is_non_empty(path: &Path) -> bool {
    std::fs::read_dir(path)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

pub(super) fn reveal_in_system_file_manager(path: &Path, select_file: bool) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("explorer");
        if select_file {
            command.arg(format!("/select,{}", path.display()));
        } else {
            command.arg(path);
        }
        command.spawn().map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        if select_file {
            command.arg("-R").arg(path);
        } else {
            command.arg(path);
        }
        command.spawn().map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let target = if select_file {
            path.parent().unwrap_or(path)
        } else {
            path
        };
        Command::new("xdg-open").arg(target).spawn().map(|_| ())
    }
}

pub(super) fn utf16_to_byte(text: &str, utf16_offset: usize) -> usize {
    let mut utf16_count = 0;
    for (byte, character) in text.char_indices() {
        if utf16_count >= utf16_offset {
            return byte;
        }
        utf16_count += character.len_utf16();
    }
    text.len()
}

pub(super) fn sidebar_button(
    label: impl Into<SharedString>,
    id: &'static str,
    p: &ResolvedPalette,
    cx: &mut Context<WorkspaceView>,
    listener: impl Fn(&mut WorkspaceView, &mut Context<WorkspaceView>) + 'static,
) -> impl IntoElement {
    ui::button(id, label, ButtonKind::Subtle, p).on_mouse_down(
        MouseButton::Left,
        cx.listener(move |workspace, _event, window, cx| {
            window.focus(&workspace.focus_handle, cx);
            listener(workspace, cx);
        }),
    )
}

pub(super) fn scroll_thumb_geometry(
    viewport_height: f32,
    max_offset: f32,
    scroll_offset_y: f32,
) -> (f32, f32) {
    let viewport_height = viewport_height.max(0.0);
    if viewport_height == 0.0 {
        return (0.0, 0.0);
    }
    let max_offset = max_offset.max(0.0);
    let thumb_height = (viewport_height * viewport_height / (viewport_height + max_offset))
        .max(24.0)
        .min(viewport_height);
    let thumb_top = if max_offset > 0.0 {
        (-scroll_offset_y / max_offset).clamp(0.0, 1.0) * (viewport_height - thumb_height)
    } else {
        0.0
    };
    (thumb_top, thumb_height)
}

pub(super) fn path_breadcrumb(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let parts: Vec<&str> = normalized
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return normalized;
    }
    // Windows 盘符保留在首段。
    if normalized.chars().nth(1) == Some(':') {
        let drive = parts[0];
        let rest = parts[1..].join(" › ");
        if rest.is_empty() {
            format!("{drive}/")
        } else {
            format!("{drive}/ › {rest}")
        }
    } else {
        parts.join(" › ")
    }
}

/// 新建终端 shell 选择器的菜单项：点击即以该 shell 创建终端（一次性选择，
/// 不写回设置；默认项 `spawn_override` 为 None，走设置默认）。
pub(super) fn shell_menu_item(
    option: crate::shell_select::ShellOption,
    id: impl Into<gpui::ElementId>,
    p: &ResolvedPalette,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement {
    let hover_bg = ui::hover_wash(p);
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded_sm()
        .text_xs()
        .cursor_pointer()
        .hover(move |style| style.bg(hover_bg))
        .child(option.label())
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _event, window, cx| {
                cx.stop_propagation();
                this.shell_menu = None;
                this.create_terminal_with_shell(false, option.spawn_override(), window, cx);
            }),
        )
}

pub(super) fn new_tab_menu_item(
    label: impl Into<SharedString>,
    id: &'static str,
    enabled: bool,
    action: NewTabAction,
    p: &ResolvedPalette,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement {
    let hover_bg = ui::hover_wash(p);
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded_sm()
        .text_xs()
        .opacity(if enabled { 1.0 } else { 0.45 })
        .child(label.into())
        .when(enabled, |item| {
            item.cursor_pointer()
                .hover(move |style| style.bg(hover_bg))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _event, window, cx| {
                        cx.stop_propagation();
                        this.handle_new_tab_action(action, window, cx);
                    }),
                )
        })
}

#[derive(Clone, Copy)]
pub(super) enum TerminalMenuAction {
    Copy,
    Paste,
    SelectAll,
    Find,
    /// 选区附加到 Composer（FR-EXPL-06 / FR-ATERM-05）。
    AskAi,
    /// 最近失败命令的输出附加到 Composer（FR-ATERM-05）。
    AskAiLastFailed,
}

/// 菜单项右侧的快捷键提示，须与 terminal_view 的按键处理及键位表保持一致
/// （查找走全局键位表 `InlineSearch`：Ctrl+F / ⌘F）。
pub(super) fn terminal_menu_shortcut(action: TerminalMenuAction) -> &'static str {
    match action {
        TerminalMenuAction::Copy => {
            if cfg!(target_os = "macos") {
                "⌘C"
            } else {
                "Ctrl+Shift+C"
            }
        }
        TerminalMenuAction::Paste => {
            if cfg!(target_os = "macos") {
                "⌘V"
            } else {
                "Ctrl+Shift+V"
            }
        }
        TerminalMenuAction::SelectAll => {
            if cfg!(target_os = "macos") {
                "⌘A"
            } else {
                "Ctrl+Shift+A"
            }
        }
        TerminalMenuAction::Find => {
            if cfg!(target_os = "macos") {
                "⌘F"
            } else {
                "Ctrl+F"
            }
        }
        TerminalMenuAction::AskAi => {
            if cfg!(target_os = "macos") {
                "⌘L"
            } else {
                "Ctrl+L"
            }
        }
        TerminalMenuAction::AskAiLastFailed => "",
    }
}

pub(super) fn terminal_menu_item(
    label: impl Into<SharedString>,
    id: &'static str,
    enabled: bool,
    action: TerminalMenuAction,
    terminal: Entity<TerminalView>,
    p: &ResolvedPalette,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement {
    let label = label.into();
    let shortcut = terminal_menu_shortcut(action);
    let hover_bg = ui::hover_wash(p);
    let focus_ring = ui::focus_ring(p);
    let activate =
        move |this: &mut WorkspaceView, window: &mut Window, cx: &mut Context<WorkspaceView>| {
            cx.stop_propagation();
            this.pane_context_menu = None;
            let attachment = match action {
                TerminalMenuAction::AskAi => terminal.read(cx).selection_attachment(),
                TerminalMenuAction::AskAiLastFailed => terminal.read(cx).last_failed_attachment(),
                _ => None,
            };
            if let Some(attachment) = attachment {
                this.attach_terminal_output(attachment, cx);
                return;
            }
            terminal.update(cx, |view, cx| {
                window.focus(&view.focus_handle(cx), cx);
                match action {
                    TerminalMenuAction::Copy => view.copy_selection(cx),
                    TerminalMenuAction::Paste => view.paste_clipboard(cx),
                    TerminalMenuAction::SelectAll => view.select_all(cx),
                    TerminalMenuAction::Find => view.open_search(cx),
                    TerminalMenuAction::AskAi | TerminalMenuAction::AskAiLastFailed => {}
                }
            });
            cx.notify();
        };
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded_sm()
        .text_xs()
        .opacity(if enabled { 1.0 } else { 0.45 })
        .focusable()
        .tab_stop(enabled)
        .role(Role::Button)
        .aria_label(label.clone())
        .focus_visible(move |style| style.border_1().border_color(focus_ring))
        .child(
            div()
                .flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap_2()
                .child(label)
                .child(div().text_color(ui::muted(p)).child(shortcut)),
        )
        .when(enabled, |item| {
            let click = activate.clone();
            item.cursor_pointer()
                .hover(move |style| style.bg(hover_bg))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| click(this, window, cx)),
                )
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "return" | "space") {
                        activate(this, window, cx);
                    }
                }))
        })
}

pub(super) fn split_menu_item(
    label: impl Into<SharedString>,
    id: &'static str,
    enabled: bool,
    action: SplitMenuAction,
    p: &ResolvedPalette,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement {
    let label = label.into();
    let focus_ring = ui::focus_ring(p);
    let hover_bg = ui::hover_wash(p);
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded_sm()
        .text_xs()
        .opacity(if enabled { 1.0 } else { 0.45 })
        .focusable()
        .tab_stop(enabled)
        .role(Role::Button)
        .aria_label(label.clone())
        .aria_description(if enabled {
            t!("ws.split_aria_active")
        } else {
            t!("ws.split_aria_unavailable")
        })
        .focus_visible(move |style| style.border_1().border_color(focus_ring))
        .child(label)
        .when(enabled, |item| {
            item.cursor_pointer()
                .hover(move |style| style.bg(hover_bg))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _event, window, cx| {
                        cx.stop_propagation();
                        this.handle_split_menu_action(action, window, cx);
                    }),
                )
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "return" | "space") {
                        cx.stop_propagation();
                        this.handle_split_menu_action(action, window, cx);
                    }
                }))
        })
}

pub(super) fn terminal_agent_status(state: AgentState) -> AgentStatus {
    match state {
        AgentState::Started => AgentStatus::Started,
        AgentState::Working => AgentStatus::Working,
        AgentState::Attention => AgentStatus::Attention,
        AgentState::Finished => AgentStatus::Finished,
        AgentState::Exited => AgentStatus::Exited,
    }
}

pub(super) fn status_label(status: AgentStatus) -> SharedString {
    match status {
        AgentStatus::Started => t!("ws.agent_started"),
        AgentStatus::Working => t!("ws.agent_working"),
        AgentStatus::Attention => t!("ws.agent_attention"),
        AgentStatus::Finished => t!("ws.agent_finished"),
        AgentStatus::Exited => t!("ws.agent_exited"),
        AgentStatus::Error => t!("ws.agent_error"),
    }
}

pub(super) fn agent_status_message(status: AgentStatus) -> SharedString {
    match status {
        AgentStatus::Started => t!("ws.agent_message_started"),
        AgentStatus::Working => t!("ws.agent_message_working"),
        AgentStatus::Attention => t!("ws.agent_message_attention"),
        AgentStatus::Finished => t!("ws.agent_message_finished"),
        AgentStatus::Exited => t!("ws.agent_message_exited"),
        AgentStatus::Error => t!("ws.agent_message_error"),
    }
}

/// 源码控制侧栏行首的变更分组标签（原 `{:?}` Debug 输出）。
pub(super) fn change_group_label(group: ChangeGroup) -> SharedString {
    match group {
        ChangeGroup::Staged => t!("ws.staged"),
        ChangeGroup::Unstaged => t!("ws.unstaged"),
        ChangeGroup::Untracked => t!("ws.untracked"),
    }
}

pub(super) fn status_color(palette: &ResolvedPalette, status: AgentStatus) -> termior_theme::Color {
    match status {
        AgentStatus::Started | AgentStatus::Working => palette.status[0],
        AgentStatus::Finished => palette.status[1],
        AgentStatus::Attention => palette.status[2],
        AgentStatus::Error => palette.status[3],
        AgentStatus::Exited => palette.foreground,
    }
}

pub(super) fn gpui_color(color: termior_theme::Color) -> gpui::Rgba {
    ui::color(color)
}

/// 带透明度的主题色（tab 非激活文字、分隔线等弱化元素用）。
pub(super) fn gpui_color_alpha(color: termior_theme::Color, alpha: f32) -> gpui::Rgba {
    ui::alpha(color, alpha)
}

pub(super) fn map_appearance(appearance: termior_store::settings::Appearance) -> Appearance {
    match appearance {
        termior_store::settings::Appearance::Light => Appearance::Light,
        termior_store::settings::Appearance::Dark => Appearance::Dark,
        termior_store::settings::Appearance::FollowSystem => Appearance::FollowSystem,
    }
}

/// Model layout for the active tab only when a matching runtime tab still exists.
/// Returns `None` instead of panicking if the two tab lists briefly diverge.
pub(super) fn paired_tab_layout<'a>(
    model: &'a WorkspaceState,
    runtime_ids: &[TabId],
) -> Option<&'a termior_ui_kit::PaneLayout> {
    let id = model.active?;
    if !runtime_ids.contains(&id) {
        return None;
    }
    model.tab(id).map(|tab| &tab.layout)
}

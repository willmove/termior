use gpui::{
    canvas, div, prelude::*, px, relative, App, Bounds, Context, EventEmitter, FocusHandle,
    Focusable, InputHandler, KeyDownEvent, MouseButton, Pixels, Point, SharedString,
    UTF16Selection, WeakEntity, Window,
};
use std::ops::Range;
use termior_vcs::{parse_diff_hunks, ChangeGroup, CommitInfo, GitDiffHunk};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitDiffAction {
    StageHunk { path: String, patch: String },
    UnstageHunk { path: String, patch: String },
    StageFile(String),
    UnstageFile(String),
    DiscardFile(String),
}

pub struct GitDiffView {
    path: String,
    group: Option<ChangeGroup>,
    patch: String,
    hunks: Vec<GitDiffHunk>,
    status: Option<Result<String, String>>,
    confirm_discard: bool,
}

impl GitDiffView {
    pub fn working(path: String, group: ChangeGroup, patch: String) -> Self {
        let hunks = parse_diff_hunks(&patch);
        Self {
            path,
            group: Some(group),
            patch,
            hunks,
            status: None,
            confirm_discard: false,
        }
    }

    pub fn commit_file(path: String, patch: String) -> Self {
        Self {
            path,
            group: None,
            hunks: parse_diff_hunks(&patch),
            patch,
            status: None,
            confirm_discard: false,
        }
    }

    pub fn update_after_action(
        &mut self,
        result: Result<String, String>,
        patch: Option<String>,
        group: Option<ChangeGroup>,
        cx: &mut Context<Self>,
    ) {
        if let Some(patch) = patch {
            self.hunks = parse_diff_hunks(&patch);
            self.patch = patch;
        }
        if group.is_some() {
            self.group = group;
        }
        self.status = Some(result);
        self.confirm_discard = false;
        cx.notify();
    }

    fn file_action(&mut self, cx: &mut Context<Self>) {
        match self.group {
            Some(ChangeGroup::Staged) => cx.emit(GitDiffAction::UnstageFile(self.path.clone())),
            Some(ChangeGroup::Unstaged | ChangeGroup::Untracked) => {
                cx.emit(GitDiffAction::StageFile(self.path.clone()))
            }
            None => {}
        }
    }

    fn discard(&mut self, cx: &mut Context<Self>) {
        if self.confirm_discard {
            cx.emit(GitDiffAction::DiscardFile(self.path.clone()));
        } else {
            self.confirm_discard = true;
            cx.notify();
        }
    }
}

impl EventEmitter<GitDiffAction> for GitDiffView {}

impl gpui::Render for GitDiffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hunk_controls = self.hunks.iter().enumerate().map(|(index, hunk)| {
            let patch = hunk.patch.clone();
            let path = self.path.clone();
            let group = self.group;
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(gpui::rgba(0x344052ff))
                .child(SharedString::from(format!(
                    "Hunk {} · {}",
                    index + 1,
                    hunk.header
                )))
                .children(group.map(|group| {
                    div()
                        .id(SharedString::from(format!("git-hunk-{index}")))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .bg(gpui::rgba(0x293241ff))
                        .child(if group == ChangeGroup::Staged {
                            "Unstage hunk"
                        } else {
                            "Stage hunk"
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |_this, _, _, cx| {
                                if group == ChangeGroup::Staged {
                                    cx.emit(GitDiffAction::UnstageHunk {
                                        path: path.clone(),
                                        patch: patch.clone(),
                                    });
                                } else {
                                    cx.emit(GitDiffAction::StageHunk {
                                        path: path.clone(),
                                        patch: patch.clone(),
                                    });
                                }
                            }),
                        )
                }))
        });
        let patch_lines = self.patch.lines().map(|line| {
            let color = if line.starts_with('+') && !line.starts_with("+++") {
                gpui::rgba(0x8bd49cff)
            } else if line.starts_with('-') && !line.starts_with("---") {
                gpui::rgba(0xe06c75ff)
            } else if line.starts_with("@@") {
                gpui::rgba(0x75a7ffff)
            } else {
                gpui::rgba(0xc6d0e0ff)
            };
            div()
                .px_3()
                .text_color(color)
                .child(SharedString::from(line.to_owned()))
        });
        let file_label = match self.group {
            Some(ChangeGroup::Staged) => "Unstage file",
            Some(ChangeGroup::Unstaged | ChangeGroup::Untracked) => "Stage file",
            None => "Commit snapshot",
        };
        let status = self.status.as_ref().map(|result| match result {
            Ok(message) => div()
                .px_3()
                .py_2()
                .bg(gpui::rgba(0x244c36ff))
                .child(SharedString::from(message.clone())),
            Err(error) => div()
                .px_3()
                .py_2()
                .bg(gpui::rgba(0x5b2929ff))
                .child(SharedString::from(error.clone())),
        });

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(gpui::rgba(0x151a22ff))
            .text_color(gpui::rgba(0xd7deebff))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(gpui::rgba(0x344052ff))
                    .child(SharedString::from(self.path.clone()))
                    .children(self.group.map(|group| {
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                div()
                                    .id("git-file-action")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .bg(gpui::rgba(0x3a6ea5ff))
                                    .child(file_label)
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| this.file_action(cx)),
                                    ),
                            )
                            .children((group != ChangeGroup::Staged).then(|| {
                                div()
                                    .id("git-discard-file")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .bg(if self.confirm_discard {
                                        gpui::rgba(0xc94f4fff)
                                    } else {
                                        gpui::rgba(0x6b3f3fff)
                                    })
                                    .child(if self.confirm_discard {
                                        "Confirm discard"
                                    } else {
                                        "Discard…"
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| this.discard(cx)),
                                    )
                            }))
                    })),
            )
            .children(status)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .p_3()
                    .children(hunk_controls),
            )
            .child(
                div()
                    .id("git-diff-patch")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_x_scroll()
                    .overflow_y_scroll()
                    .p_3()
                    .font_family("monospace")
                    .text_sm()
                    .children(patch_lines),
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitHistoryAction {
    SelectCommit(String),
    OpenFile { commit: String, path: String },
    OpenRemote(String),
}

pub struct GitHistoryView {
    commits: Vec<CommitInfo>,
    selected: Option<String>,
    files: Vec<String>,
    query: String,
    marked_text: String,
    focus_handle: FocusHandle,
}

impl GitHistoryView {
    pub fn new(commits: Vec<CommitInfo>, files: Vec<String>, cx: &mut Context<Self>) -> Self {
        Self {
            selected: commits.first().map(|commit| commit.id.clone()),
            commits,
            files,
            query: String::new(),
            marked_text: String::new(),
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn set_commit_files(&mut self, commit: String, files: Vec<String>, cx: &mut Context<Self>) {
        self.selected = Some(commit);
        self.files = files;
        cx.notify();
    }

    fn select_commit(&mut self, commit: String, cx: &mut Context<Self>) {
        self.selected = Some(commit.clone());
        self.files.clear();
        cx.emit(GitHistoryAction::SelectCommit(commit));
        cx.notify();
    }

    fn handle_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "backspace" => {
                self.query.pop();
                cx.notify();
            }
            "escape" => {
                self.query.clear();
                cx.notify();
            }
            _ => {}
        }
    }
}

impl Focusable for GitHistoryView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<GitHistoryAction> for GitHistoryView {}

impl gpui::Render for GitHistoryView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focus = self.focus_handle.clone();
        let input_focus = focus.clone();
        let handler = GitHistoryInputHandler {
            view: cx.entity().downgrade(),
        };
        let query = self.query.to_ascii_lowercase();
        let commits = self
            .commits
            .iter()
            .filter(|commit| {
                query.is_empty()
                    || commit.summary.to_ascii_lowercase().contains(&query)
                    || commit.id.contains(&query)
                    || commit.author.to_ascii_lowercase().contains(&query)
            })
            .map(|commit| {
                let id = commit.id.clone();
                let selected = self.selected.as_deref() == Some(commit.id.as_str());
                let lane = commit.lane.as_ref().map_or(0, |lane| lane.lane);
                let graph = format!("{}●", "│ ".repeat(lane));
                let decorations = if commit.decorations.is_empty() {
                    String::new()
                } else {
                    format!("  [{}]", commit.decorations.join(", "))
                };
                div()
                    .id(SharedString::from(format!("history-{}", commit.id)))
                    .flex()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .cursor_pointer()
                    .when(selected, |row| row.bg(gpui::rgba(0x365880aa)))
                    .child(
                        div()
                            .font_family("monospace")
                            .text_color(gpui::rgba(0x75a7ffff))
                            .child(SharedString::from(graph)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(SharedString::from(format!(
                                "{}{}",
                                commit.summary, decorations
                            )))
                            .child(div().text_xs().text_color(gpui::rgba(0x9aa6b7ff)).child(
                                SharedString::from(format!(
                                    "{} · {}",
                                    &commit.id[..7.min(commit.id.len())],
                                    commit.author
                                )),
                            )),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| this.select_commit(id.clone(), cx)),
                    )
            });
        let selected = self.selected.clone();
        let files = self.files.iter().map(|path| {
            let path_for_open = path.clone();
            let commit = selected.clone().unwrap_or_default();
            div()
                .id(SharedString::from(format!("commit-file-{path}")))
                .px_3()
                .py_2()
                .cursor_pointer()
                .hover(|style| style.bg(gpui::rgba(0x36588066)))
                .child(SharedString::from(path.clone()))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |_this, _, _, cx| {
                        cx.emit(GitHistoryAction::OpenFile {
                            commit: commit.clone(),
                            path: path_for_open.clone(),
                        })
                    }),
                )
        });
        let remote_commit = self.selected.clone();

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .track_focus(&focus)
            .on_key_down(cx.listener(Self::handle_key))
            .bg(gpui::rgba(0x151a22ff))
            .text_color(gpui::rgba(0xd7deebff))
            .child(
                canvas(
                    |bounds, _, _| bounds,
                    move |_, _, window, cx| window.handle_input(&input_focus, handler, cx),
                )
                .absolute()
                .size_full(),
            )
            .child(
                div()
                    .mx_3()
                    .my_2()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(gpui::rgba(0x4f8fefff))
                    .child(SharedString::from(format!(
                        "Search history: {}▏",
                        self.query
                    ))),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(
                        div()
                            .id("git-history-list")
                            .w(relative(0.62))
                            .h_full()
                            .overflow_y_scroll()
                            .border_r_1()
                            .border_color(gpui::rgba(0x344052ff))
                            .children(commits),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .px_3()
                                    .py_2()
                                    .child("Changed files")
                                    .children(remote_commit.map(|commit| {
                                        div()
                                            .id("open-remote-commit")
                                            .px_2()
                                            .py_1()
                                            .rounded_md()
                                            .cursor_pointer()
                                            .bg(gpui::rgba(0x293241ff))
                                            .child("Open remote")
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |_this, _, _, cx| {
                                                    cx.emit(GitHistoryAction::OpenRemote(
                                                        commit.clone(),
                                                    ))
                                                }),
                                            )
                                    })),
                            )
                            .child(
                                div()
                                    .id("git-history-files")
                                    .flex_1()
                                    .overflow_y_scroll()
                                    .children(files),
                            ),
                    ),
            )
    }
}

#[derive(Clone)]
struct GitHistoryInputHandler {
    view: WeakEntity<GitHistoryView>,
}

impl InputHandler for GitHistoryInputHandler {
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        let view = self.view.upgrade()?;
        let end = view.read(cx).query.encode_utf16().count();
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&mut self, _: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        let view = self.view.upgrade()?;
        let len = view.read(cx).marked_text.encode_utf16().count();
        (len > 0).then_some(0..len)
    }

    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<String> {
        None
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut App,
    ) {
        if let Some(view) = self.view.upgrade() {
            view.update(cx, |view, cx| {
                view.query.push_str(text);
                view.marked_text.clear();
                cx.notify();
            });
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut App,
    ) {
        if let Some(view) = self.view.upgrade() {
            view.update(cx, |view, cx| {
                view.marked_text = text.into();
                cx.notify();
            });
        }
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut App) {
        if let Some(view) = self.view.upgrade() {
            view.update(cx, |view, cx| {
                view.marked_text.clear();
                cx.notify();
            });
        }
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<Bounds<Pixels>> {
        None
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<usize> {
        None
    }
}

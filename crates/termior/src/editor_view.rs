//! GPUI viewport for the rope/tree-sitter editor core, including IME preedit.

use gpui::{
    canvas, div, prelude::*, px, uniform_list, AnyElement, App, Bounds, Context, FocusHandle,
    Focusable, InputHandler, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, ScrollStrategy, SharedString, Task, UTF16Selection,
    UniformListScrollHandle, WeakEntity, Window,
};
use std::{
    cell::RefCell,
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};
use termior_ai::{InlineCompleter, InlineCompletionContext, InlineCompletionResult};
use termior_editor::{
    builtin_editor_themes, CompletionController, EditorBuffer, EditorTheme, HighlightKind, Motion,
    SyntaxDocument, SyntaxLanguage, VimCommand, VimEngine, VimMode,
};
use termior_ui_kit::SearchOverlay;

/// Editor-side debounce: how long after the last keystroke before a completion request fires
/// (FR-EDIT-05「停顿触发」). Continuous typing keeps resetting this timer, cancelling in-flight
/// requests along the way so no stale completion ever renders.
const COMPLETION_DEBOUNCE_MS: u64 = 300;
/// 单次补全请求的超时上限（FR-EDIT-05「请求失败/超时静默降级」）。Provider 卡住时这里主动
/// 中止，避免拖住后续停顿触发的请求（聊天路径的 300s 超时对补全体验太长）。
const COMPLETION_REQUEST_TIMEOUT_MS: u64 = 8_000;

/// 编辑器右侧滚动条的布局与拖拽状态。由 render 里一个测量 canvas 在 prepaint 写入（容器高度 /
/// 顶端坐标），供拖拽与滚动手柄定位使用；跨帧持久（GPUI 的 canvas 是一次性 FnOnce，不能只靠它
/// 逐帧重绘，所以只用来测量）。
#[derive(Default)]
struct ScrollbarLayout {
    /// 滚动条轨道顶端在窗口坐标系的 Y（用于把指针 Y 映射为本地 Y）。
    track_top: f32,
    /// 轨道/容器高度（即视口高度）。
    viewport_height: f32,
    dragging: bool,
}

pub struct EditorView {
    buffer: EditorBuffer,
    syntax: SyntaxDocument,
    focus_handle: FocusHandle,
    marked_text: String,
    title: String,
    theme: EditorTheme,
    search: SearchOverlay,
    search_matches: Vec<termior_editor::SearchMatch>,
    vim_enabled: bool,
    vim: VimEngine,
    visual_anchor: Option<usize>,
    scroll_handle: UniformListScrollHandle,
    scrollbar_layout: Rc<RefCell<ScrollbarLayout>>,
    completion: CompletionController,
    /// 已配置好的补全 Provider（来自 Settings → Models 的 completion profile）。
    /// 为 `None` 时（autocomplete 关闭或未配置补全模型）补全不触发。
    completer: Option<Arc<InlineCompleter>>,
    /// 进行中的 debounce + 请求 task；drop 即取消，避免连续输入堆积请求。
    pending_completion: Option<Task<()>>,
}

impl EditorView {
    pub fn untitled(cx: &mut Context<Self>) -> Self {
        let text = "// Termior editor\n// Open a file from the Explorer or start typing.\n";
        Self {
            buffer: EditorBuffer::new(text),
            syntax: SyntaxDocument::new(SyntaxLanguage::Rust, text),
            focus_handle: cx.focus_handle(),
            marked_text: String::new(),
            title: "Untitled".into(),
            theme: default_editor_theme(),
            search: SearchOverlay::default(),
            search_matches: Vec::new(),
            vim_enabled: false,
            vim: VimEngine::default(),
            visual_anchor: None,
            scroll_handle: UniformListScrollHandle::default(),
            scrollbar_layout: Rc::new(RefCell::new(ScrollbarLayout::default())),
            completion: CompletionController::new(false),
            completer: None,
            pending_completion: None,
        }
    }

    pub fn open(path: &Path, cx: &mut Context<Self>) -> Self {
        let buffer = match EditorBuffer::open(path) {
            Ok(buffer) => buffer,
            Err(error) => {
                log::warn!("failed to open {}: {error}", path.display());
                return Self::untitled(cx);
            }
        };
        let language = SyntaxLanguage::detect(path);
        let syntax = SyntaxDocument::new(language, &buffer.text());
        Self {
            buffer,
            syntax,
            focus_handle: cx.focus_handle(),
            marked_text: String::new(),
            title: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Editor")
                .to_owned(),
            theme: default_editor_theme(),
            search: SearchOverlay::default(),
            search_matches: Vec::new(),
            vim_enabled: false,
            vim: VimEngine::default(),
            visual_anchor: None,
            scroll_handle: UniformListScrollHandle::default(),
            scrollbar_layout: Rc::new(RefCell::new(ScrollbarLayout::default())),
            completion: CompletionController::new(false),
            completer: None,
            pending_completion: None,
        }
    }

    pub fn set_preferences(
        &mut self,
        theme_id: &str,
        vim_enabled: bool,
        completer: Option<Arc<InlineCompleter>>,
        completion_enabled: bool,
    ) {
        if let Some(theme) = builtin_editor_themes()
            .into_iter()
            .find(|theme| theme.id == theme_id)
        {
            self.theme = theme;
        }
        self.vim_enabled = vim_enabled;
        if !vim_enabled {
            self.vim = VimEngine::default();
            self.visual_anchor = None;
        }
        self.completer = completer;
        self.completion.set_enabled(completion_enabled);
        if !completion_enabled {
            self.pending_completion = None;
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn path(&self) -> Option<&Path> {
        self.buffer.path()
    }

    /// 光标行列（0-based），供状态栏上下文条显示。
    pub fn cursor_line_col(&self) -> (usize, usize) {
        self.buffer
            .line_col_for_char(self.buffer.cursor().char_index)
            .unwrap_or((0, 0))
    }

    pub fn text(&self) -> String {
        self.buffer.text()
    }

    pub fn revision(&self) -> u64 {
        self.buffer.revision()
    }

    pub fn is_dirty(&self) -> bool {
        self.buffer.is_dirty()
    }

    /// Retarget an already-open editor after an Explorer rename without
    /// discarding its buffer, selection, undo history, or syntax state.
    pub fn set_path_after_rename(&mut self, path: PathBuf) {
        self.title = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Editor")
            .to_owned();
        self.buffer.set_path(path);
    }

    pub fn selected_text(&self) -> Option<String> {
        self.buffer.selected_text()
    }

    /// 当前显示中的 ghost text（无则为 `None`），供状态栏等外部观察。
    #[allow(dead_code)]
    pub fn ghost_text(&self) -> Option<&str> {
        self.completion.ghost_text()
    }

    pub fn open_search(&mut self, cx: &mut Context<Self>) {
        self.search.open();
        self.update_search();
        cx.notify();
    }

    pub fn undo(&mut self, cx: &mut Context<Self>) {
        self.buffer.undo();
        self.reparse();
        self.scroll_cursor_into_view();
        cx.notify();
    }

    pub fn redo(&mut self, cx: &mut Context<Self>) {
        self.buffer.redo();
        self.reparse();
        self.scroll_cursor_into_view();
        cx.notify();
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let primary = if cfg!(target_os = "macos") {
            modifiers.platform
        } else {
            modifiers.control
        };
        if primary {
            if key == "s" {
                if let Err(error) = self.buffer.save() {
                    log::warn!("save failed: {error}");
                }
            }
            return;
        }
        if self.search.visible {
            match key {
                "escape" => self.search.close(),
                "enter" | "return" => {
                    self.search.next(modifiers.shift);
                    self.select_current_search();
                }
                "backspace" => {
                    self.search.query.pop();
                    self.update_search();
                }
                "c" if modifiers.alt => {
                    self.search.options.case_sensitive = !self.search.options.case_sensitive;
                    self.update_search();
                }
                _ => return,
            }
            cx.notify();
            return;
        }
        if self.vim_enabled && self.vim.mode() != VimMode::Insert {
            let vim_key = if modifiers.control {
                format!("ctrl-{key}")
            } else {
                key.to_owned()
            };
            if let Some(command) = self.vim.feed(&vim_key) {
                self.execute_vim(command);
            }
            self.scroll_cursor_into_view();
            cx.notify();
            return;
        }
        if self.vim_enabled && key == "escape" {
            if let Some(command) = self.vim.feed("esc") {
                self.execute_vim(command);
            }
            self.scroll_cursor_into_view();
            cx.notify();
            return;
        }
        // 行内补全：Esc 取消当前 ghost text（FR-EDIT-05）。
        if !self.search.visible && key == "escape" && self.completion.ghost_text().is_some() {
            self.dismiss_completion();
            cx.notify();
            return;
        }
        let cursor = self.buffer.cursor().char_index;
        let mut mutated = true;
        match key {
            "left" => {
                let _ = self.buffer.set_cursor(cursor.saturating_sub(1));
                mutated = false;
            }
            "right" => {
                let _ = self
                    .buffer
                    .set_cursor((cursor + 1).min(self.buffer.len_chars()));
                mutated = false;
            }
            "up" | "down" => {
                if let Ok((line, column)) = self.buffer.line_col_for_char(cursor) {
                    let target = if key == "up" {
                        line.saturating_sub(1)
                    } else {
                        (line + 1).min(self.buffer.len_lines().saturating_sub(1))
                    };
                    let target = self.buffer.char_for_line_col(target, column);
                    let _ = self.buffer.set_cursor(target);
                }
                mutated = false;
            }
            "backspace" if cursor > 0 => {
                self.completion.cancel();
                let _ = self.buffer.delete(cursor - 1..cursor);
                self.reparse();
            }
            "delete" if cursor < self.buffer.len_chars() => {
                self.completion.cancel();
                let _ = self.buffer.delete(cursor..cursor + 1);
                self.reparse();
            }
            "enter" | "return" => {
                self.completion.cancel();
                let _ = self.buffer.insert("\n");
                self.reparse();
            }
            "tab" => {
                // 行内补全：Tab 接受当前 ghost text（FR-EDIT-05）；无 ghost text 时回退到缩进。
                if let Some(accepted) = self
                    .completion
                    .accept(self.buffer.revision())
                    .filter(|text| !text.is_empty())
                {
                    self.apply_accepted_completion(&accepted);
                } else {
                    let _ = self.buffer.insert("    ");
                    self.reparse();
                }
            }
            _ => return,
        }
        if mutated {
            // 光标移动不触发补全；文本变更后 debounce 重新调度。
            self.schedule_completion(cx);
        } else {
            self.completion.cancel();
            self.pending_completion = None;
        }
        self.scroll_cursor_into_view();
        cx.notify();
    }

    fn reparse(&mut self) {
        let edits = self.buffer.take_pending_edits();
        if !edits.is_empty() {
            self.syntax.reparse_incremental(&self.buffer.text(), &edits);
        }
        self.update_search();
    }

    /// Debounce + 发起一次行内补全请求（FR-EDIT-05）。连续输入时旧的 task 被 drop 取消，
    /// 在途请求即便稍后返回也会因 revision 不匹配被 [`CompletionController::receive`] 拒绝。
    fn schedule_completion(&mut self, cx: &mut Context<Self>) {
        // 取消上一轮：ghost text 立即消失，pending 请求即便到达也被拒绝。
        self.completion.cancel();
        let Some(completer) = self.completer.clone() else {
            self.pending_completion = None;
            return;
        };
        // Vim 非 Insert 模式、选区激活、IME 预编辑中均不补全。
        if self.vim_enabled && self.vim.mode() != VimMode::Insert {
            self.pending_completion = None;
            return;
        }
        if self.buffer.selection().is_some() || !self.marked_text.is_empty() {
            self.pending_completion = None;
            return;
        }
        let cursor = self.buffer.cursor().char_index;
        let context = InlineCompletionContext {
            prefix: self.buffer.text().chars().take(cursor).collect(),
            suffix: self.buffer.text().chars().skip(cursor).collect(),
            language: Some(self.syntax.language().id().to_owned()),
            path: self
                .buffer
                .path()
                .and_then(|path| path.to_str())
                .map(str::to_owned),
        };
        let revision = self.buffer.revision();
        if !self.completion.request(revision) {
            self.pending_completion = None;
            return;
        }
        self.pending_completion = Some(cx.spawn(async move |editor, cx| {
            // Debounce：停顿 COMPLETION_DEBOUNCE_MS 后才真正发起请求。
            cx.background_executor()
                .timer(std::time::Duration::from_millis(COMPLETION_DEBOUNCE_MS))
                .await;
            // 超时静默降级：把请求与一个超时 future 赛跑，先就绪者胜出（FR-EDIT-05）。
            let request = completer.complete(&context);
            futures::pin_mut!(request);
            let timeout = cx
                .background_executor()
                .timer(std::time::Duration::from_millis(
                    COMPLETION_REQUEST_TIMEOUT_MS,
                ));
            let result = match futures::future::select(request, timeout).await {
                futures::future::Either::Left((outcome, _)) => outcome,
                // 超时：请求仍可能继续在后台线程跑，但其结果会被 revision 不匹配静默拒绝。
                futures::future::Either::Right(_) => InlineCompletionResult::Empty,
            };
            let _ = editor.update(cx, |editor, cx| {
                editor.apply_completion_result(revision, result);
                cx.notify();
            });
        }));
    }

    /// 把一次补全结果投递给 [`CompletionController`]；过期/失败/空都静默降级。
    fn apply_completion_result(&mut self, revision: u64, result: InlineCompletionResult) {
        match result {
            InlineCompletionResult::Completed(text) => {
                self.completion.receive(revision, text);
            }
            // 空与错误都不打扰输入：前者保留 Idle，后者归一为 Idle（不显示错误）。
            InlineCompletionResult::Empty | InlineCompletionResult::Error(_) => {
                self.completion.cancel();
            }
        }
    }

    /// 把接受的 ghost text 插入到光标处（FR-EDIT-05「接受后正确插入到 Rope/缓冲」）。
    fn apply_accepted_completion(&mut self, text: &str) {
        let _ = self.buffer.insert(text);
        self.reparse();
    }

    fn dismiss_completion(&mut self) {
        self.completion.dismiss();
        self.pending_completion = None;
    }

    fn scroll_cursor_into_view(&self) {
        if let Ok((line, _)) = self
            .buffer
            .line_col_for_char(self.buffer.cursor().char_index)
        {
            self.scroll_handle
                .scroll_to_item(line, ScrollStrategy::Nearest);
        }
    }

    /// 把滚动条轨道上的指针 Y 映射为滚动偏移并写入 scroll handle（拖拽/点击跳转共用）。
    fn set_scroll_from_pointer_y(&self, pointer_y: f32) {
        let (viewport_height, track_top) = {
            let l = self.scrollbar_layout.borrow();
            (l.viewport_height, l.track_top)
        };
        let base = self.scroll_handle.0.borrow().base_handle.clone();
        let max_y: f32 = base.max_offset().y.into();
        if max_y <= 0.0 || viewport_height <= 0.0 {
            return;
        }
        let content_h = viewport_height + max_y;
        let thumb_h = (viewport_height * viewport_height / content_h).clamp(24.0, viewport_height);
        let travel = (viewport_height - thumb_h).max(1.0);
        let local_y = (pointer_y - track_top - thumb_h / 2.0).clamp(0.0, travel);
        let offset_y = -(local_y / travel) * max_y;
        base.set_offset(Point::new(px(0.0), px(offset_y)));
    }

    fn update_search(&mut self) {
        self.search_matches = self
            .buffer
            .search(&self.search.query, self.search.options.case_sensitive);
        self.search.set_results(self.search_matches.len());
    }

    fn select_current_search(&mut self) {
        if let Some(found) = self.search_matches.get(self.search.current) {
            let range = found.char_range.clone();
            let line = found.line;
            let _ = self.buffer.set_selection(range.start, range.end);
            self.scroll_handle
                .scroll_to_item(line, ScrollStrategy::Center);
        }
    }

    fn render_visible_lines(&mut self, range: Range<usize>) -> Vec<AnyElement> {
        let requested_lines = range.len();
        let viewport = self.buffer.text_for_lines(range.clone());
        let global_end = viewport.start_byte + viewport.text.len();
        let spans = self
            .syntax
            .highlight_spans(viewport.start_byte..global_end)
            .into_iter()
            .map(|mut span| {
                span.byte_range = span.byte_range.start.saturating_sub(viewport.start_byte)
                    ..span.byte_range.end.saturating_sub(viewport.start_byte);
                span
            })
            .collect::<Vec<_>>();
        let cursor_byte = self
            .buffer
            .byte_for_char(self.buffer.cursor().char_index)
            .ok()
            .filter(|byte| *byte >= viewport.start_byte && *byte <= global_end)
            .map(|byte| byte - viewport.start_byte)
            .unwrap_or(usize::MAX);
        let marked_text = self.marked_text.clone();
        let theme = self.theme.clone();
        // Ghost text 只在光标行渲染，且仅当光标落在当前视口内时才有意义。
        let cursor_line = self
            .buffer
            .line_col_for_char(self.buffer.cursor().char_index)
            .map(|(line, _)| line)
            .ok();
        let ghost_text = self
            .completion
            .ghost_text()
            .filter(|_| cursor_byte != usize::MAX)
            .map(str::to_owned);
        let selected_lines = self.buffer.selection().map(|selection| {
            let selection = selection.range();
            let start = self
                .buffer
                .line_col_for_char(selection.start)
                .map(|value| value.0)
                .unwrap_or(0);
            let end = self
                .buffer
                .line_col_for_char(selection.end)
                .map(|value| value.0)
                .unwrap_or(start);
            start..=end
        });

        line_ranges(&viewport.text)
            .into_iter()
            .take(requested_lines)
            .enumerate()
            .map(|(offset, line_range)| {
                let line_index = viewport.start_line + offset;
                let line_ghost = if cursor_line == Some(line_index) {
                    ghost_text.as_deref()
                } else {
                    None
                };
                let segments = highlighted_segments(
                    &viewport.text,
                    line_range,
                    &spans,
                    cursor_byte,
                    &marked_text,
                    line_ghost,
                    &theme,
                );
                let is_match = self
                    .search_matches
                    .iter()
                    .any(|found| found.line == line_index);
                let is_selected = selected_lines
                    .as_ref()
                    .is_some_and(|lines| lines.contains(&line_index));
                div()
                    .flex()
                    .flex_row()
                    .w_full()
                    .px_3()
                    .bg(if is_selected {
                        parse_hex(&theme.selection)
                    } else if is_match {
                        parse_hex_alpha(&theme.selection, 0x88)
                    } else {
                        gpui::rgba(0x00000000)
                    })
                    .child(
                        div()
                            .w(px(52.0))
                            .pr_3()
                            .text_right()
                            .text_color(parse_hex_alpha(&theme.foreground, 0x66))
                            .child(SharedString::from(format!("{}", line_index + 1))),
                    )
                    .child(div().flex().flex_1().children(segments))
                    .into_any_element()
            })
            .collect()
    }

    fn execute_vim(&mut self, command: VimCommand) {
        let cursor = self.buffer.cursor().char_index;
        match command {
            VimCommand::Move { motion, count } => {
                let target = self.motion_target(motion, count);
                if matches!(
                    self.vim.mode(),
                    VimMode::Visual | VimMode::VisualLine | VimMode::VisualBlock
                ) {
                    let anchor = *self.visual_anchor.get_or_insert(cursor);
                    let _ = self.buffer.set_selection(anchor, target);
                } else {
                    let _ = self.buffer.set_cursor(target);
                }
            }
            VimCommand::DeleteMotion { motion, count } => {
                let target = self.motion_target(motion, count);
                let range = cursor.min(target)
                    ..cursor
                        .max(target)
                        .max(cursor + usize::from(target == cursor));
                let text: String = self
                    .buffer
                    .text()
                    .chars()
                    .skip(range.start)
                    .take(range.len())
                    .collect();
                self.vim.set_register('"', text);
                let _ = self.buffer.delete(range);
                self.reparse();
            }
            VimCommand::ChangeMotion { motion, count } => {
                let target = self.motion_target(motion, count);
                let range = cursor.min(target)
                    ..cursor
                        .max(target)
                        .max(cursor + usize::from(target == cursor));
                let text: String = self
                    .buffer
                    .text()
                    .chars()
                    .skip(range.start)
                    .take(range.len())
                    .collect();
                self.vim.set_register('"', text);
                let _ = self.buffer.delete(range);
                let _ = self.vim.feed("i");
                self.reparse();
            }
            VimCommand::YankMotion { motion, count } => {
                let target = self.motion_target(motion, count);
                let range = cursor.min(target)..cursor.max(target);
                let text: String = self
                    .buffer
                    .text()
                    .chars()
                    .skip(range.start)
                    .take(range.len())
                    .collect();
                self.vim.set_register('"', text);
            }
            VimCommand::PasteAfter { count } => {
                if let Some(text) = self.vim.register('"').map(str::to_owned) {
                    for _ in 0..count {
                        let _ = self.buffer.insert(&text);
                    }
                    self.reparse();
                }
            }
            VimCommand::Undo => {
                self.buffer.undo();
                self.reparse();
            }
            VimCommand::Redo => {
                self.buffer.redo();
                self.reparse();
            }
            VimCommand::EnterMode(mode) => {
                if matches!(
                    mode,
                    VimMode::Visual | VimMode::VisualLine | VimMode::VisualBlock
                ) {
                    self.visual_anchor = Some(cursor);
                } else if mode == VimMode::Normal {
                    self.visual_anchor = None;
                    self.buffer.clear_selection();
                }
            }
            VimCommand::SetMark(name) => self.vim.set_mark_position(name, cursor),
            VimCommand::JumpMark(name) => {
                if let Some(position) = self.vim.mark_position(name) {
                    let _ = self
                        .buffer
                        .set_cursor(position.min(self.buffer.len_chars()));
                }
            }
            VimCommand::Execute(command) => match command.trim() {
                "w" | "write" | "wq" | "x" => {
                    let _ = self.buffer.save();
                }
                _ => {}
            },
            VimCommand::Cancel => {
                self.visual_anchor = None;
                self.buffer.clear_selection();
            }
        }
    }

    fn motion_target(&self, motion: Motion, count: usize) -> usize {
        let cursor = self.buffer.cursor().char_index;
        let text = self.buffer.text();
        match motion {
            Motion::Left => cursor.saturating_sub(count),
            Motion::Right => (cursor + count).min(self.buffer.len_chars()),
            Motion::Up | Motion::Down => {
                let (line, column) = self.buffer.line_col_for_char(cursor).unwrap_or((0, 0));
                let line = if motion == Motion::Up {
                    line.saturating_sub(count)
                } else {
                    (line + count).min(self.buffer.len_lines().saturating_sub(1))
                };
                self.buffer.char_for_line_col(line, column)
            }
            Motion::LineStart => self
                .buffer
                .line_col_for_char(cursor)
                .map(|(line, _)| self.buffer.char_for_line_col(line, 0))
                .unwrap_or(cursor),
            Motion::LineEnd => self
                .buffer
                .line_col_for_char(cursor)
                .map(|(line, _)| self.buffer.char_for_line_col(line, usize::MAX))
                .unwrap_or(cursor),
            Motion::DocumentStart => 0,
            Motion::DocumentEnd => self.buffer.len_chars(),
            Motion::WordForward | Motion::WordEnd => {
                let mut position = cursor;
                for _ in 0..count {
                    let tail: Vec<char> = text.chars().skip(position).collect();
                    let mut seen_word = false;
                    for (offset, character) in tail.iter().enumerate().skip(1) {
                        if character.is_alphanumeric() || *character == '_' {
                            seen_word = true;
                        } else if seen_word {
                            position += offset;
                            break;
                        }
                    }
                }
                position.min(self.buffer.len_chars())
            }
            Motion::WordBackward => {
                let prefix: Vec<char> = text.chars().take(cursor).collect();
                prefix
                    .iter()
                    .rposition(|character| !character.is_alphanumeric() && *character != '_')
                    .map(|position| position + 1)
                    .unwrap_or(0)
            }
        }
    }
}

impl Focusable for EditorView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focus = self.focus_handle.clone();
        let input_focus = focus.clone();
        let handler = EditorInputHandler {
            view: cx.entity().downgrade(),
        };
        let line_count = self.buffer.len_lines();
        let lines = uniform_list(
            "editor-lines",
            line_count,
            cx.processor(|this, range: Range<usize>, _window, _cx| {
                this.render_visible_lines(range)
            }),
        )
        .track_scroll(&self.scroll_handle)
        .size_full();

        let p = crate::ui::palette(cx);
        let search_overlay = self.search.visible.then(|| {
            div()
                .absolute()
                .top(px(8.0))
                .right(px(12.0))
                .flex()
                .gap_2()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::color(p.accent))
                .bg(crate::ui::color(p.overlay))
                .text_color(crate::ui::color(p.foreground))
                .shadow_md()
                .child(SharedString::from(format!("Find: {}|", self.search.query)))
                .child(SharedString::from(format!(
                    "{}/{} · {}",
                    if self.search.total == 0 {
                        0
                    } else {
                        self.search.current + 1
                    },
                    self.search.total,
                    if self.search.options.case_sensitive {
                        "Aa"
                    } else {
                        "aa"
                    }
                )))
        });
        let vim_status = self.vim_enabled.then(|| {
            let command = if self.vim.mode() == VimMode::CommandLine {
                format!(":{}", self.vim.command_line())
            } else {
                format!("-- {:?} --", self.vim.mode())
            };
            div()
                .absolute()
                .bottom(px(4.0))
                .right(px(8.0))
                .px_2()
                .py_1()
                .rounded_md()
                .bg(crate::ui::alpha(p.overlay, 0.9))
                .border_1()
                .border_color(crate::ui::border(&p))
                .text_xs()
                .text_color(crate::ui::color(p.foreground))
                .child(SharedString::from(command))
        });

        // 右侧竖向滚动条：轨道拖拽 + 滚动手柄。容器高度由内嵌 canvas 在 prepaint 测量写入
        // scrollbar_layout（GPUI 的 canvas 是一次性 FnOnce，只作测量、不作逐帧重绘）。
        let scrollbar = {
            let layout = self.scrollbar_layout.clone();
            let scroll = self.scroll_handle.clone();
            let sb_layout = self.scrollbar_layout.clone();
            let thumb_color = crate::ui::alpha(p.foreground, 0.5);
            let track_color = crate::ui::alpha(p.foreground, 0.12);
            let (thumb_h, thumb_top) = {
                let l = layout.borrow();
                let vh = l.viewport_height;
                let base = scroll.0.borrow().base_handle.clone();
                let max_y: f32 = base.max_offset().y.into();
                let off_y: f32 = base.offset().y.into();
                if max_y > 0.0 && vh > 0.0 {
                    let content_h = vh + max_y;
                    let th = (vh * vh / content_h).clamp(24.0, vh);
                    let travel = (vh - th).max(1.0);
                    (th, ((-off_y / max_y).clamp(0.0, 1.0)) * travel)
                } else {
                    (0.0, 0.0)
                }
            };
            div()
                .absolute()
                .right_0()
                .top_0()
                .bottom_0()
                .w(px(8.0))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                        {
                            let mut l = this.scrollbar_layout.borrow_mut();
                            l.dragging = true;
                        }
                        this.set_scroll_from_pointer_y(event.position.y.into());
                        cx.notify();
                    }),
                )
                .on_mouse_move(
                    cx.listener(move |this, event: &MouseMoveEvent, _window, cx| {
                        let dragging = this.scrollbar_layout.borrow().dragging;
                        if dragging {
                            this.set_scroll_from_pointer_y(event.position.y.into());
                            cx.notify();
                        }
                    }),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |this, _event: &MouseUpEvent, _window, _cx| {
                        this.scrollbar_layout.borrow_mut().dragging = false;
                    }),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(1.0))
                        .right(px(1.0))
                        .top_0()
                        .bottom_0()
                        .rounded_full()
                        .bg(track_color),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(1.5))
                        .right(px(1.5))
                        .top(px(thumb_top))
                        .h(px(thumb_h))
                        .rounded_full()
                        .bg(thumb_color),
                )
                .child(
                    canvas(
                        move |bounds, _, _| bounds,
                        move |_bounds, _prepaint, _window, _cx| {
                            let mut l = sb_layout.borrow_mut();
                            l.track_top = _bounds.origin.y.into();
                            l.viewport_height = _bounds.size.height.into();
                        },
                    )
                    .absolute()
                    .size_full(),
                )
        };

        div()
            .id("editor-view")
            .relative()
            .track_focus(&focus)
            .on_key_down(cx.listener(Self::handle_key_down))
            .on_scroll_wheel(cx.listener(|_this, _event, _window, cx| cx.notify()))
            .size_full()
            .bg(parse_hex(&self.theme.background))
            .text_color(parse_hex(&self.theme.foreground))
            .font_family(crate::monospace_font::default_family())
            .text_sm()
            .child(
                canvas(
                    |bounds, _, _| bounds,
                    move |_, _, window, cx| {
                        window.handle_input(&input_focus, handler, cx);
                    },
                )
                .absolute()
                .size_full(),
            )
            .child(lines)
            .child(scrollbar)
            .children(search_overlay)
            .children(vim_status)
    }
}

#[derive(Clone)]
struct EditorInputHandler {
    view: WeakEntity<EditorView>,
}

impl InputHandler for EditorInputHandler {
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        let view = self.view.upgrade()?;
        let view = view.read(cx);
        let text = view.buffer.text();
        let selection = view
            .buffer
            .selection()
            .map(|selection| selection.range())
            .unwrap_or_else(|| {
                let cursor = view.buffer.cursor().char_index;
                cursor..cursor
            });
        Some(UTF16Selection {
            range: char_range_to_utf16(&text, selection),
            reversed: false,
        })
    }

    fn marked_text_range(&mut self, _window: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        let view = self.view.upgrade()?;
        let view = view.read(cx);
        (!view.marked_text.is_empty()).then(|| 0..view.marked_text.encode_utf16().count())
    }

    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<String> {
        let view = self.view.upgrade()?;
        let text = view.read(cx).buffer.text();
        let chars = utf16_range_to_chars(&text, range.clone());
        *actual_range = Some(range);
        Some(text.chars().skip(chars.start).take(chars.len()).collect())
    }

    fn replace_text_in_range(
        &mut self,
        replacement_range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut App,
    ) {
        let Some(view) = self.view.upgrade() else {
            return;
        };
        view.update(cx, |view, cx| {
            if view.search.visible {
                view.search.query.push_str(text);
                view.update_search();
                cx.notify();
                return;
            }
            if view.vim_enabled && view.vim.mode() != VimMode::Insert {
                return;
            }
            let range = replacement_range
                .map(|range| utf16_range_to_chars(&view.buffer.text(), range))
                .or_else(|| view.buffer.selection().map(|selection| selection.range()))
                .unwrap_or_else(|| {
                    let cursor = view.buffer.cursor().char_index;
                    cursor..cursor
                });
            let _ = view.buffer.replace(range, text);
            view.marked_text.clear();
            view.reparse();
            view.scroll_cursor_into_view();
            view.schedule_completion(cx);
            cx.notify();
        });
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _replacement_range: Option<Range<usize>>,
        new_text: &str,
        _new_marked_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(view) = self.view.upgrade() {
            view.update(cx, |view, cx| {
                // IME 预编辑开始：预编辑串内联显示，ghost text 让位（FR-EDIT-05 / NFR-07）。
                view.dismiss_completion();
                view.marked_text = new_text.to_owned();
                cx.notify();
            });
        }
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut App) {
        if let Some(view) = self.view.upgrade() {
            view.update(cx, |view, cx| {
                view.marked_text.clear();
                cx.notify();
            });
        }
    }

    fn bounds_for_range(
        &mut self,
        _range: Range<usize>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        None
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<usize> {
        None
    }
}

fn char_range_to_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    let start: String = text.chars().take(range.start).collect();
    let end: String = text.chars().take(range.end).collect();
    start.encode_utf16().count()..end.encode_utf16().count()
}

fn utf16_range_to_chars(text: &str, range: Range<usize>) -> Range<usize> {
    let mut utf16 = 0;
    let mut start = text.chars().count();
    for (index, character) in text.chars().enumerate() {
        if utf16 >= range.start && start == text.chars().count() {
            start = index;
        }
        if utf16 >= range.end {
            return start..index;
        }
        utf16 += character.len_utf16();
    }
    if start == text.chars().count() {
        start = text.chars().count();
    }
    start..text.chars().count()
}

fn default_editor_theme() -> EditorTheme {
    builtin_editor_themes()
        .into_iter()
        .find(|theme| theme.id == "nord")
        .expect("bundled nord editor theme")
}

fn line_ranges(source: &str) -> Vec<Range<usize>> {
    if source.is_empty() {
        return std::iter::once(0..0).collect();
    }
    let mut ranges = Vec::new();
    let mut start = 0;
    for line in source.split_inclusive('\n') {
        let end = start + line.trim_end_matches(['\r', '\n']).len();
        ranges.push(start..end);
        start += line.len();
    }
    if source.ends_with('\n') {
        ranges.push(source.len()..source.len());
    }
    ranges
}

fn highlighted_segments(
    source: &str,
    line: Range<usize>,
    spans: &[termior_editor::HighlightSpan],
    cursor: usize,
    marked_text: &str,
    ghost_text: Option<&str>,
    theme: &EditorTheme,
) -> Vec<AnyElement> {
    let ghost_color = parse_hex_alpha(&theme.foreground, 0x55);
    let ghost = ghost_text.filter(|ghost| !ghost.is_empty());
    // 光标标记块：ASCII `|` 竖线字符 + IME 预编辑串，其后内联渲染 ghost text
    // （FR-EDIT-05）。不用 `▏`（字体回退会渲染成宽空白），也不拆成独立
    // flex 光标元素（真实字体下文本段有被压成逐字换行的先例）。
    let push_cursor_block = |elements: &mut Vec<AnyElement>| {
        elements.push(
            div()
                .text_color(parse_hex(&theme.cursor))
                .child(SharedString::from(format!("|{marked_text}")))
                .into_any_element(),
        );
        if let Some(ghost) = ghost {
            elements.push(
                div()
                    .text_color(ghost_color)
                    .child(SharedString::from(ghost.to_owned()))
                    .into_any_element(),
            );
        }
    };
    let mut boundaries = vec![line.start, line.end];
    for span in spans {
        if span.byte_range.start < line.end && span.byte_range.end > line.start {
            boundaries.push(span.byte_range.start.max(line.start));
            boundaries.push(span.byte_range.end.min(line.end));
        }
    }
    if cursor >= line.start && cursor <= line.end {
        boundaries.push(cursor);
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut elements = Vec::new();
    for window in boundaries.windows(2) {
        let start = window[0];
        let end = window[1];
        if cursor == start {
            push_cursor_block(&mut elements);
        }
        if start < end {
            let kind = spans
                .iter()
                .find(|span| span.byte_range.start <= start && span.byte_range.end >= end)
                .map(|span| span.kind);
            elements.push(
                div()
                    .text_color(color_for_highlight(theme, kind))
                    .child(SharedString::from(source[start..end].to_owned()))
                    .into_any_element(),
            );
        }
    }
    if line.start == line.end || cursor == line.end {
        push_cursor_block(&mut elements);
    }
    elements
}

fn color_for_highlight(theme: &EditorTheme, kind: Option<HighlightKind>) -> gpui::Rgba {
    parse_hex(match kind {
        Some(HighlightKind::Comment) => &theme.comment,
        Some(HighlightKind::String) => &theme.string,
        Some(HighlightKind::Number) => &theme.number,
        Some(HighlightKind::Keyword) => &theme.keyword,
        Some(HighlightKind::Function) => &theme.function,
        Some(HighlightKind::Type) => &theme.type_name,
        _ => &theme.foreground,
    })
}

fn parse_hex(value: &str) -> gpui::Rgba {
    let value = value.trim_start_matches('#');
    u32::from_str_radix(value, 16)
        .ok()
        .map(|rgb| gpui::rgba((rgb << 8) | 0xff))
        .unwrap_or_else(|| gpui::rgba(0xffffffff))
}

/// 编辑器主题色 + 透明度(行号弱化、搜索命中底色跟随编辑器主题而非应用主题)。
fn parse_hex_alpha(value: &str, alpha: u32) -> gpui::Rgba {
    let value = value.trim_start_matches('#');
    u32::from_str_radix(value, 16)
        .ok()
        .map(|rgb| gpui::rgba((rgb << 8) | (alpha & 0xff)))
        .unwrap_or_else(|| gpui::rgba(0xffffff88))
}

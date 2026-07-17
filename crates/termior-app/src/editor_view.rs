//! GPUI viewport for the rope/tree-sitter editor core, including IME preedit.

use gpui::{
    canvas, div, prelude::*, px, AnyElement, App, Bounds, Context, FocusHandle, Focusable,
    InputHandler, KeyDownEvent, Pixels, Point, SharedString, StatefulInteractiveElement,
    UTF16Selection, WeakEntity, Window,
};
use std::ops::Range;
use std::path::Path;
use termior_editor::{
    builtin_editor_themes, EditorBuffer, EditorTheme, HighlightKind, Motion, SyntaxDocument,
    SyntaxLanguage, VimCommand, VimEngine, VimMode,
};
use termior_ui_kit::SearchOverlay;

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
        }
    }

    pub fn set_preferences(&mut self, theme_id: &str, vim_enabled: bool) {
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
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn path(&self) -> Option<&Path> {
        self.buffer.path()
    }

    pub fn selected_text(&self) -> Option<String> {
        self.buffer.selected_text()
    }

    pub fn open_search(&mut self, cx: &mut Context<Self>) {
        self.search.open();
        self.update_search();
        cx.notify();
    }

    pub fn undo(&mut self, cx: &mut Context<Self>) {
        self.buffer.undo();
        self.reparse();
        cx.notify();
    }

    pub fn redo(&mut self, cx: &mut Context<Self>) {
        self.buffer.redo();
        self.reparse();
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
            cx.notify();
            return;
        }
        if self.vim_enabled && key == "escape" {
            if let Some(command) = self.vim.feed("esc") {
                self.execute_vim(command);
            }
            cx.notify();
            return;
        }
        let cursor = self.buffer.cursor().char_index;
        match key {
            "left" => {
                let _ = self.buffer.set_cursor(cursor.saturating_sub(1));
            }
            "right" => {
                let _ = self
                    .buffer
                    .set_cursor((cursor + 1).min(self.buffer.len_chars()));
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
            }
            "backspace" if cursor > 0 => {
                let _ = self.buffer.delete(cursor - 1..cursor);
                self.reparse();
            }
            "delete" if cursor < self.buffer.len_chars() => {
                let _ = self.buffer.delete(cursor..cursor + 1);
                self.reparse();
            }
            "enter" | "return" => {
                let _ = self.buffer.insert("\n");
                self.reparse();
            }
            "tab" => {
                let _ = self.buffer.insert("    ");
                self.reparse();
            }
            _ => return,
        }
        cx.notify();
    }

    fn reparse(&mut self) {
        self.syntax.reparse(&self.buffer.text());
        self.update_search();
    }

    fn update_search(&mut self) {
        self.search_matches = self
            .buffer
            .search(&self.search.query, self.search.options.case_sensitive);
        self.search.set_results(self.search_matches.len());
    }

    fn select_current_search(&mut self) {
        if let Some(found) = self.search_matches.get(self.search.current) {
            let _ = self
                .buffer
                .set_selection(found.char_range.start, found.char_range.end);
        }
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
        let source = self.buffer.text();
        let spans = self.syntax.highlight_spans(0..source.len());
        let cursor_byte = char_to_byte(&source, self.buffer.cursor().char_index);
        let marked_text = self.marked_text.clone();
        let theme = self.theme.clone();
        let selected_lines = self.buffer.selection().map(|selection| {
            let range = selection.range();
            let start = self
                .buffer
                .line_col_for_char(range.start)
                .map(|value| value.0)
                .unwrap_or(0);
            let end = self
                .buffer
                .line_col_for_char(range.end)
                .map(|value| value.0)
                .unwrap_or(start);
            start..=end
        });
        let lines = line_ranges(&source)
            .into_iter()
            .take(500)
            .enumerate()
            .map(|(index, range)| {
                let segments = highlighted_segments(
                    &source,
                    range.clone(),
                    &spans,
                    cursor_byte,
                    &marked_text,
                    &theme,
                );
                let is_match = self.search_matches.iter().any(|found| found.line == index);
                let is_selected = selected_lines
                    .as_ref()
                    .is_some_and(|lines| lines.contains(&index));
                div()
                    .flex()
                    .flex_row()
                    .w_full()
                    .bg(if is_selected {
                        parse_hex(&theme.selection)
                    } else if is_match {
                        gpui::rgba(0x5f4b2488)
                    } else {
                        gpui::rgba(0x00000000)
                    })
                    .child(
                        div()
                            .w(px(52.0))
                            .pr_3()
                            .text_right()
                            .text_color(gpui::rgba(0x738091ff))
                            .child(SharedString::from(format!("{}", index + 1))),
                    )
                    .child(div().flex().flex_1().children(segments))
            })
            .collect::<Vec<_>>();

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
                .border_color(gpui::rgba(0x4f8fefff))
                .bg(gpui::rgba(0x202733ff))
                .child(SharedString::from(format!("Find: {}▏", self.search.query)))
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
                .bg(gpui::rgba(0x293241dd))
                .text_xs()
                .child(SharedString::from(command))
        });

        div()
            .id("editor-view")
            .relative()
            .track_focus(&focus)
            .on_key_down(cx.listener(Self::handle_key_down))
            .size_full()
            .overflow_y_scroll()
            .bg(parse_hex(&self.theme.background))
            .text_color(parse_hex(&self.theme.foreground))
            .font_family("monospace")
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
            .child(div().flex().flex_col().p_3().children(lines))
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
    theme: &EditorTheme,
) -> Vec<AnyElement> {
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
            elements.push(
                div()
                    .text_color(parse_hex(&theme.cursor))
                    .child(SharedString::from(format!("▏{marked_text}")))
                    .into_any_element(),
            );
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
        elements.push(
            div()
                .text_color(parse_hex(&theme.cursor))
                .child(SharedString::from(format!("▏{marked_text}")))
                .into_any_element(),
        );
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

fn char_to_byte(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

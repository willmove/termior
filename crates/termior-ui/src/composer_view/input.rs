//! 附件、补全建议、输入行、键盘与 IME。

use super::helpers::{char_to_byte, snippet_preview};
use super::*;

impl ComposerView {
    pub fn attach_selection(&mut self, source: AttachmentSource, label: String, text: String) {
        self.draft.attach_selection(source, label, text);
    }

    pub fn attach_terminal_output(
        &mut self,
        label: String,
        text: String,
        command: Option<termior_ai::CommandProvenance>,
    ) {
        self.draft.attach_terminal_output(label, text, command);
    }

    pub fn attach_file(&mut self, path: PathBuf) {
        self.draft.attach_file(path);
    }

    pub fn set_workspace_paths(&mut self, paths: Vec<String>) {
        self.workspace_paths = paths;
        self.update_suggestions();
    }

    /// 单一附件入口：一个无过滤对话框，按扩展名自动判型（图片 / 文本文件）。
    pub(super) fn pick_attachment(&mut self, cx: &mut Context<Self>) {
        let mut dialog =
            rfd::AsyncFileDialog::new().set_title(t!("composer.attach_dialog_title").to_string());
        if self.workspace_root.is_dir() {
            dialog = dialog.set_directory(&self.workspace_root);
        }
        // Never run a blocking file dialog inside a GPUI event handler: on
        // Windows its modal message loop re-enters the foreground executor
        // while the `App` RefCell is borrowed. Await the async dialog instead.
        cx.spawn(async move |view, cx| {
            let Some(handle) = dialog.pick_file().await else {
                return;
            };
            let path = handle.path().to_path_buf();
            let _ = view.update(cx, |view, cx| {
                match image_mime(&path) {
                    Some(mime) => match std::fs::read(&path) {
                        Ok(bytes) => {
                            let name = path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("image")
                                .to_owned();
                            view.draft.attach_image(name, mime, &bytes);
                            view.set_status(t!("composer.status.image_attached"));
                        }
                        Err(error) => view.set_status(tf!(
                            "composer.status.image_failed",
                            "error" => error
                        )),
                    },
                    None => {
                        view.draft.attach_file(path);
                        view.set_status(t!("composer.status.file_attached"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn update_suggestions(&mut self) {
        self.update_path_suggestions();
        self.update_slash_suggestions();
        self.update_snippet_suggestions();
        self.warm_repo_map();
    }

    pub(super) fn update_snippet_suggestions(&mut self) {
        self.snippet_suggestions = termior_ai::snippet_query(&self.draft.input, self.cursor)
            .map(|query| {
                self.snippets
                    .suggest(query)
                    .into_iter()
                    .take(8)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        self.selected_snippet_suggestion = self
            .selected_snippet_suggestion
            .min(self.snippet_suggestions.len().saturating_sub(1));
    }

    /// 把光标前的 `#query` 词元替换为 `#handle `；正文在提交时才展开，输入框保持可读。
    pub(super) fn accept_snippet_suggestion(&mut self, handle: String, cx: &mut Context<Self>) {
        let query_chars = termior_ai::snippet_query(&self.draft.input, self.cursor)
            .map_or(0, |query| query.chars().count() + 1);
        let start_char = self.cursor.saturating_sub(query_chars);
        let start_byte = char_to_byte(&self.draft.input, start_char);
        let end_byte = char_to_byte(&self.draft.input, self.cursor);
        let replacement = format!("#{handle} ");
        self.draft
            .input
            .replace_range(start_byte..end_byte, &replacement);
        self.cursor = start_char + replacement.chars().count();
        self.snippet_suggestions.clear();
        self.selected_snippet_suggestion = 0;
        cx.notify();
    }

    pub(super) fn persist_snippets(&mut self) {
        let Some(dir) = &self.data_dir else {
            return;
        };
        if let Err(error) = DataFiles::new(dir.clone())
            .snippets::<SnippetStore>()
            .save(&self.snippets)
        {
            self.set_status(tf!("composer.snippet.save_failed", "error" => error));
        }
    }

    pub(super) fn update_slash_suggestions(&mut self) {
        self.slash_suggestions = slash::slash_query(&self.draft.input, self.cursor)
            .map(slash::match_commands)
            .unwrap_or_default();
        self.selected_slash_suggestion = self
            .selected_slash_suggestion
            .min(self.slash_suggestions.len().saturating_sub(1));
    }

    /// 执行命令面板选中的命令：先清空 `/xxx` 输入，再分派动作。
    pub(super) fn run_slash_command(
        &mut self,
        command: &'static SlashCommand,
        cx: &mut Context<Self>,
    ) {
        self.slash_suggestions.clear();
        self.selected_slash_suggestion = 0;
        if command.takes_args {
            // 需要参数：补全命令名，等用户输入参数后按 Enter 提交。
            self.draft.input = format!("/{} ", command.name);
            self.cursor = self.draft.input.chars().count();
            self.set_status(t!(command.description_key));
            cx.notify();
            return;
        }
        self.invoke_slash_command(command, "", cx);
    }

    pub(super) fn invoke_slash_command(
        &mut self,
        command: &'static SlashCommand,
        args: &str,
        cx: &mut Context<Self>,
    ) {
        self.draft.input.clear();
        self.cursor = 0;
        self.slash_suggestions.clear();
        self.selected_slash_suggestion = 0;
        match command.action {
            SlashAction::App(action) => cx.emit(ComposerAppAction(action)),
            SlashAction::Composer(command) => match command {
                ComposerCommand::NewSession => self.new_session(cx),
                ComposerCommand::ModeAuto => self.set_mode(Mode::Auto, cx),
                ComposerCommand::ModePlan => self.set_mode(Mode::Plan, cx),
                ComposerCommand::ModeYolo => self.set_mode(Mode::Yolo, cx),
                ComposerCommand::CycleAgent => self.cycle_custom_agent(cx),
                ComposerCommand::Stop => self.stop_active_task(cx),
                ComposerCommand::AttachFile => self.pick_attachment(cx),
                ComposerCommand::ToggleDock => cx.emit(ComposerDockToggle),
                ComposerCommand::ContextInspector => {
                    self.toggle_center(ComposerCenter::ContextInspector)
                }
                ComposerCommand::Skills => self.toggle_center(ComposerCenter::Skills),
                ComposerCommand::Memory => self.toggle_center(ComposerCenter::Memory),
                ComposerCommand::Recovery => self.toggle_center(ComposerCenter::Recovery),
                ComposerCommand::Checkpoints => self.toggle_center(ComposerCenter::Checkpoints),
                ComposerCommand::Automations => self.toggle_center(ComposerCenter::Automations),
                ComposerCommand::SaveSnippet => match self.snippets.define(args) {
                    Ok(handle) => {
                        self.set_status(tf!("composer.snippet.saved", "handle" => handle));
                        self.persist_snippets();
                    }
                    Err(error) => {
                        // 保留输入，便于用户修正参数后重新提交。
                        self.draft.input = format!("/snippet {args}");
                        self.cursor = self.draft.input.chars().count();
                        self.set_status(match error {
                            SnippetError::MissingBody => t!("composer.snippet.usage"),
                            SnippetError::InvalidHandle(handle) => {
                                tf!("composer.snippet.invalid_handle", "handle" => handle)
                            }
                        });
                    }
                },
                ComposerCommand::Snippets => self.toggle_center(ComposerCenter::Snippets),
                ComposerCommand::Todos => self.toggle_center(ComposerCenter::Todos),
            },
        }
        cx.notify();
    }

    pub(super) fn update_path_suggestions(&mut self) {
        let Some(query) = self.active_path_query() else {
            self.path_suggestions.clear();
            self.selected_path_suggestion = 0;
            return;
        };
        self.path_suggestions =
            fuzzy_match(&query, self.workspace_paths.iter().map(String::as_str))
                .into_iter()
                .take(8)
                .map(|hit| hit.path)
                .collect();
        self.selected_path_suggestion = self
            .selected_path_suggestion
            .min(self.path_suggestions.len().saturating_sub(1));
    }

    pub(super) fn active_path_query(&self) -> Option<String> {
        let prefix = self
            .draft
            .input
            .chars()
            .take(self.cursor)
            .collect::<String>();
        let token = prefix
            .rsplit(|character: char| character.is_whitespace())
            .next()?;
        token.strip_prefix('@').map(str::to_owned)
    }

    pub(super) fn accept_path_suggestion(&mut self, path: String, cx: &mut Context<Self>) {
        let prefix = self
            .draft
            .input
            .chars()
            .take(self.cursor)
            .collect::<String>();
        let token_chars = prefix
            .rsplit(|character: char| character.is_whitespace())
            .next()
            .map(|token| token.chars().count())
            .unwrap_or(0);
        let start_char = self.cursor.saturating_sub(token_chars);
        let start_byte = char_to_byte(&self.draft.input, start_char);
        let end_byte = char_to_byte(&self.draft.input, self.cursor);
        self.draft.input.replace_range(start_byte..end_byte, "");
        self.cursor = start_char;
        self.draft.attach_file(self.workspace_root.join(path));
        self.path_suggestions.clear();
        self.selected_path_suggestion = 0;
        cx.notify();
    }

    /// 在光标处插入文本，光标按字符数推进到插入内容之后。键盘输入、IME
    /// 提交与剪贴板粘贴共用；同时清空预编辑串并刷新补全建议。
    pub(super) fn insert_text_at_cursor(&mut self, text: &str, cx: &mut Context<Self>) {
        let byte = char_to_byte(&self.draft.input, self.cursor);
        self.draft.input.insert_str(byte, text);
        self.cursor += text.chars().count();
        self.marked_text.clear();
        self.update_suggestions();
        cx.notify();
    }

    /// 把剪贴板读取推迟到当前 entity 更新结束之后。Windows 上打开剪贴板会往
    /// wndproc 回派 sent message，若此时 entity 的 RefCell 仍被借着就会重入崩溃。
    pub(super) fn schedule_attach_clipboard(&mut self, cx: &mut Context<Self>) {
        let entity = cx.entity().downgrade();
        cx.defer(move |cx| {
            let Some(item) = cx.read_from_clipboard() else {
                return;
            };
            let _ = entity.update(cx, |this, cx| {
                this.apply_clipboard_item(item, cx);
            });
        });
    }

    pub(super) fn apply_clipboard_item(&mut self, item: ClipboardItem, cx: &mut Context<Self>) {
        let mut attached = false;
        for entry in item.into_entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    let format = image.format();
                    self.draft.attach_image(
                        format!("pasted-image.{}", format.extension()),
                        format.mime_type(),
                        image.bytes(),
                    );
                    attached = true;
                }
                ClipboardEntry::ExternalPaths(paths) => {
                    for path in paths.0 {
                        if path.is_file() {
                            self.draft.attach_file(path);
                            attached = true;
                        }
                    }
                }
                // 剪贴板是纯文本时插入输入框光标处；图片/外部路径才作为附件。
                ClipboardEntry::String(text) => self.insert_text_at_cursor(&text.text, cx),
            }
        }
        if attached {
            self.set_status(t!("composer.status.clipboard_attached"));
            cx.notify();
        }
    }

    pub fn update_terminal_context(
        &mut self,
        cwd: String,
        recent_output: String,
        command_reference: Option<termior_terminal::TerminalCommandRecord>,
    ) {
        let captured_at_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        *self
            .terminal_context
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = TerminalContext {
            cwd,
            recent_output,
            captured_at_unix_ms,
            command_reference,
        };
    }

    pub fn context_plan(&self) -> Option<&ContextPlan> {
        self.last_context_plan.as_ref()
    }

    pub(super) fn send(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.orchestrator.busy {
            self.stop_active_task(cx);
        } else {
            self.submit(cx);
        }
    }

    pub(super) fn stop_active_task(&mut self, cx: &mut Context<Self>) {
        match self.orchestrator.stop_action() {
            StopAction::CancelToken(cancellation) => {
                cancellation.cancel();
                self.set_status(t!("composer.status.cancelling"));
                cx.emit(AgentStatus::Attention);
                cx.notify();
            }
            StopAction::DispatchCancel => {
                self.dispatch_task_command(
                    TaskCommand::Cancel,
                    t!("composer.status.cancelling"),
                    cx,
                );
            }
            StopAction::Idle => {}
        }
    }

    pub(super) fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = event.keystroke.modifiers;
        let primary = if cfg!(target_os = "macos") {
            modifiers.platform
        } else {
            modifiers.control
        };
        if primary && event.keystroke.key.eq_ignore_ascii_case("c") {
            self.copy_text_selection(cx);
            return;
        }
        if primary && event.keystroke.key.eq_ignore_ascii_case("v") {
            self.schedule_attach_clipboard(cx);
            return;
        }
        if !self.slash_suggestions.is_empty() {
            match event.keystroke.key.as_str() {
                "up" => {
                    self.selected_slash_suggestion =
                        self.selected_slash_suggestion.saturating_sub(1);
                    cx.notify();
                    return;
                }
                "down" => {
                    self.selected_slash_suggestion = (self.selected_slash_suggestion + 1)
                        .min(self.slash_suggestions.len().saturating_sub(1));
                    cx.notify();
                    return;
                }
                "enter" | "return" | "tab" if !event.keystroke.modifiers.shift => {
                    if let Some(command) = self
                        .slash_suggestions
                        .get(self.selected_slash_suggestion)
                        .copied()
                    {
                        self.run_slash_command(command, cx);
                    }
                    return;
                }
                "escape" => {
                    self.slash_suggestions.clear();
                    cx.notify();
                    return;
                }
                _ => {}
            }
        }
        if !self.snippet_suggestions.is_empty() {
            match event.keystroke.key.as_str() {
                "up" => {
                    self.selected_snippet_suggestion =
                        self.selected_snippet_suggestion.saturating_sub(1);
                    cx.notify();
                    return;
                }
                "down" => {
                    self.selected_snippet_suggestion = (self.selected_snippet_suggestion + 1)
                        .min(self.snippet_suggestions.len().saturating_sub(1));
                    cx.notify();
                    return;
                }
                "enter" | "return" | "tab" if !event.keystroke.modifiers.shift => {
                    if let Some(snippet) = self
                        .snippet_suggestions
                        .get(self.selected_snippet_suggestion)
                    {
                        let handle = snippet.handle.clone();
                        self.accept_snippet_suggestion(handle, cx);
                    }
                    return;
                }
                "escape" => {
                    self.snippet_suggestions.clear();
                    cx.notify();
                    return;
                }
                _ => {}
            }
        }
        if !self.path_suggestions.is_empty() {
            match event.keystroke.key.as_str() {
                "up" => {
                    self.selected_path_suggestion = self.selected_path_suggestion.saturating_sub(1);
                    cx.notify();
                    return;
                }
                "down" => {
                    self.selected_path_suggestion = (self.selected_path_suggestion + 1)
                        .min(self.path_suggestions.len().saturating_sub(1));
                    cx.notify();
                    return;
                }
                "enter" | "return" => {
                    if let Some(path) = self
                        .path_suggestions
                        .get(self.selected_path_suggestion)
                        .cloned()
                    {
                        self.accept_path_suggestion(path, cx);
                    }
                    return;
                }
                "escape" => {
                    self.path_suggestions.clear();
                    cx.notify();
                    return;
                }
                _ => {}
            }
        }
        match event.keystroke.key.as_str() {
            "escape" if self.mode_menu_open => self.mode_menu_open = false,
            "escape" if self.agent_menu_open => self.agent_menu_open = false,
            "escape" if self.model_menu_open => self.model_menu_open = false,
            "escape" if self.text_selection.is_some() => self.text_selection = None,
            "escape" if self.open_center.is_some() => self.open_center = None,
            "enter" | "return" if !event.keystroke.modifiers.shift => self.submit(cx),
            "backspace" if self.cursor > 0 => {
                let start = char_to_byte(&self.draft.input, self.cursor - 1);
                let end = char_to_byte(&self.draft.input, self.cursor);
                self.draft.input.replace_range(start..end, "");
                self.cursor -= 1;
            }
            "left" => self.cursor = self.cursor.saturating_sub(1),
            "right" => self.cursor = (self.cursor + 1).min(self.draft.input.chars().count()),
            _ => return,
        }
        self.update_suggestions();
        cx.notify();
    }

    /// 输入草稿 + 光标。光标用 ASCII `|` 字符而不是 `▏`（U+258F）：后者在
    /// Windows 上经字体回退渲染成一个很宽的空白。`|` 在任何 UI 字体里都是
    /// 窄竖线，且随文本流布局，无独立元素参与 flex 分配（曾因文本拆段 +
    /// flex 子项被压成逐字换行的竖排回归）。
    pub(super) fn display_input(&self) -> String {
        let mut input = self.draft.input.clone();
        let byte = char_to_byte(&input, self.cursor);
        input.insert_str(
            byte,
            if self.marked_text.is_empty() {
                "|"
            } else {
                "|…"
            },
        );
        input
    }

    pub(super) fn render_attachment_chips(
        &mut self,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let chips = self
            .draft
            .attachments
            .iter()
            .enumerate()
            .map(|(index, attachment)| {
                div()
                    .id(SharedString::from(format!("composer-attachment-{index}")))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(crate::ui::color(p.elevated))
                    .border_1()
                    .border_color(crate::ui::border(p))
                    .text_xs()
                    .cursor_pointer()
                    .child(tf!(
                        "composer.attachment_chip",
                        "label" => attachment.chip_label()
                    ))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.draft.remove_attachment(index);
                            cx.notify();
                        }),
                    )
            });
        chips.map(IntoElement::into_any_element).collect()
    }

    pub(super) fn render_path_suggestions(
        &mut self,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let path_suggestions = (!self.path_suggestions.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .mx_3()
                .mt_1()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::color(p.accent))
                .bg(crate::ui::color(p.elevated))
                .shadow_md()
                .children(
                    self.path_suggestions
                        .iter()
                        .enumerate()
                        .map(|(index, path)| {
                            let selected = index == self.selected_path_suggestion;
                            let selected_path = path.clone();
                            div()
                                .id(SharedString::from(format!("composer-path-{index}")))
                                .px_3()
                                .py_1()
                                .text_xs()
                                .cursor_pointer()
                                .when(selected, |row| row.bg(crate::ui::selected_wash(p)))
                                .child(SharedString::from(format!("@{path}")))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.accept_path_suggestion(selected_path.clone(), cx)
                                    }),
                                )
                        }),
                )
        });
        path_suggestions.map(IntoElement::into_any_element)
    }

    pub(super) fn render_slash_suggestions(
        &mut self,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let slash_suggestions = (!self.slash_suggestions.is_empty()).then(|| {
            div()
                .id("composer-slash-palette")
                .flex()
                .flex_col()
                .mx_3()
                .mt_1()
                .max_h(px(240.0))
                .overflow_y_scroll()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::color(p.accent))
                .bg(crate::ui::color(p.elevated))
                .shadow_md()
                .children(self.slash_suggestions.iter().copied().enumerate().map(
                    |(index, command)| {
                        let selected = index == self.selected_slash_suggestion;
                        div()
                            .id(SharedString::from(format!("composer-slash-{index}")))
                            .flex()
                            .flex_row()
                            .gap_3()
                            .px_3()
                            .py_1()
                            .text_xs()
                            .cursor_pointer()
                            .when(selected, |row| row.bg(crate::ui::selected_wash(p)))
                            .child(
                                div()
                                    .min_w(px(120.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(SharedString::from(format!("/{}", command.name))),
                            )
                            .child(
                                div()
                                    .text_color(crate::ui::muted(p))
                                    .child(t!(command.description_key)),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.run_slash_command(command, cx)
                                }),
                            )
                    },
                ))
        });
        slash_suggestions.map(IntoElement::into_any_element)
    }

    pub(super) fn render_snippet_suggestions(
        &mut self,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let snippet_suggestions =
            (!self.snippet_suggestions.is_empty()).then(|| {
                div()
                    .flex()
                    .flex_col()
                    .mx_3()
                    .mt_1()
                    .rounded_md()
                    .border_1()
                    .border_color(crate::ui::color(p.accent))
                    .bg(crate::ui::color(p.elevated))
                    .shadow_md()
                    .children(self.snippet_suggestions.iter().enumerate().map(
                        |(index, snippet)| {
                            let selected = index == self.selected_snippet_suggestion;
                            let handle = snippet.handle.clone();
                            div()
                                .id(SharedString::from(format!("composer-snippet-{index}")))
                                .flex()
                                .flex_row()
                                .gap_3()
                                .px_3()
                                .py_1()
                                .text_xs()
                                .cursor_pointer()
                                .when(selected, |row| row.bg(crate::ui::selected_wash(p)))
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(SharedString::from(format!("#{}", snippet.handle))),
                                )
                                .child(
                                    div()
                                        .text_color(crate::ui::muted(p))
                                        .child(SharedString::from(snippet_preview(&snippet.body))),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.accept_snippet_suggestion(handle.clone(), cx)
                                    }),
                                )
                        },
                    ))
            });
        snippet_suggestions.map(IntoElement::into_any_element)
    }

    pub(super) fn render_prompt_row(
        &self,
        p: &ResolvedPalette,
        window: &mut Window,
        placeholder_active: bool,
        input_display: String,
    ) -> AnyElement {
        // 第 1 行：输入文本独占整行宽度，右侧停靠窄面板下长提示词仍完整可见。
        // 光标是文本内的 `|` 字符；文本保持单一文本子节点，不做 flex 拆段
        // （拆段曾在真实字体下被压成逐字换行的竖排）。
        termior_ui_kit::input_field(p, self.focus_handle.is_focused(window))
            .flex()
            .items_center()
            .mx_3()
            .mt_2()
            .min_w(px(0.0))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_sm()
                    .when(placeholder_active, |text| {
                        text.text_color(crate::ui::muted(p))
                    })
                    .debug_selector(|| "composer-input-text".into())
                    .relative()
                    .child(SharedString::from(input_display))
                    // IME 候选窗锚点探针：记录输入行文本 bounds 与样式。
                    .child(crate::ime_anchor::anchor_probe(&self.ime_anchor)),
            )
            .into_any_element()
    }

    pub(super) fn render_toolbar(
        &mut self,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let external_backend =
            self.orchestrator.backend_choice == AgentBackendChoice::CodexAppServer;
        let mode_label = if external_backend {
            t!("composer.backend_approval")
        } else {
            SharedString::from(self.mode.label())
        };
        let agent_name = self.active_agent_name();
        let agent_tooltip = self
            .orchestrator
            .backend_capability_summary
            .clone()
            .map(SharedString::from)
            .unwrap_or_else(|| t!("composer.agent_tooltip"));
        let model_label = self.model_chip_label();
        let context_label = self.context_plan().map(|plan| {
            tf!(
                "composer.context_summary",
                "tokens" => plan.total_tokens,
                "items" => plan.entries.len()
            )
        });
        // Scroll controls in a narrow dock; keep Send/Stop outside this group.
        let controls = div()
            .id("composer-toolbar-controls")
            .flex()
            .flex_row()
            .flex_1()
            .items_center()
            .gap_1()
            .h(px(32.0))
            .overflow_x_scroll()
            .min_w(px(0.0))
            .child(
                crate::ui::icon_button(
                    "composer-attach",
                    Icon::Paperclip,
                    t!("composer.attach_hint"),
                    p,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.pick_attachment(cx)),
                ),
            )
            .child({
                let mode_palette = p.clone();
                div()
                    .id("composer-mode")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(crate::ui::border(p))
                    .bg(if self.mode_menu_open {
                        crate::ui::selected_wash(p)
                    } else {
                        crate::ui::color(p.elevated)
                    })
                    .text_color(if self.mode == Mode::Yolo {
                        crate::ui::color(p.accent)
                    } else {
                        crate::ui::color(p.foreground)
                    })
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .gap(px(3.0))
                    .flex_none()
                    .text_xs()
                    .tooltip(move |_window, cx| {
                        Tooltip::view(
                            if external_backend {
                                t!("composer.mode.backend_hint")
                            } else {
                                t!("composer.mode.hint")
                            },
                            &mode_palette,
                            cx,
                        )
                    })
                    .child(mode_label)
                    .when(!external_backend, |chip| {
                        chip.child(crate::ui::icon(
                            Icon::ChevronDown,
                            icon_size::XS,
                            crate::ui::muted(p),
                        ))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if external_backend {
                                this.set_status(t!("composer.status.codex_owns_approvals_hint"));
                            } else {
                                this.mode_menu_open = !this.mode_menu_open;
                                this.model_menu_open = false;
                            }
                            cx.notify();
                        }),
                    )
            })
            .child({
                let agent_palette = p.clone();
                let agent_tooltip = agent_tooltip.clone();
                div()
                    .id("composer-agent")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(crate::ui::border(p))
                    .bg(crate::ui::color(p.elevated))
                    .cursor_pointer()
                    .flex_none()
                    .max_w(px(160.0))
                    .overflow_hidden()
                    .text_xs()
                    .tooltip(move |_window, cx| {
                        Tooltip::view(agent_tooltip.clone(), &agent_palette, cx)
                    })
                    .flex()
                    .items_center()
                    .gap(px(3.0))
                    .when(self.agent_menu_open, |chip| {
                        chip.bg(crate::ui::selected_wash(p))
                    })
                    .child(agent_name)
                    .child(crate::ui::icon(
                        Icon::ChevronDown,
                        icon_size::XS,
                        crate::ui::muted(p),
                    ))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_agent_menu(cx);
                        }),
                    )
            })
            // 会话级模型选择；外部后端自带模型来源，不显示。
            .when(!external_backend, |toolbar| {
                let model_palette = p.clone();
                toolbar.child(
                    div()
                        .id("composer-model")
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(crate::ui::border(p))
                        .bg(if self.model_menu_open {
                            crate::ui::selected_wash(p)
                        } else {
                            crate::ui::color(p.elevated)
                        })
                        .cursor_pointer()
                        .flex_none()
                        .max_w(px(180.0))
                        .overflow_hidden()
                        .text_xs()
                        .flex()
                        .items_center()
                        .gap(px(3.0))
                        .tooltip(move |_window, cx| {
                            Tooltip::view(t!("composer.model_menu.hint"), &model_palette, cx)
                        })
                        .child(model_label)
                        .child(crate::ui::icon(
                            Icon::ChevronDown,
                            icon_size::XS,
                            crate::ui::muted(p),
                        ))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_model_menu(cx);
                            }),
                        ),
                )
            })
            .when_some(context_label, |toolbar, label| {
                let inspector_palette = p.clone();
                toolbar.child(
                    div()
                        .id("composer-context-inspector")
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(crate::ui::border(p))
                        .text_xs()
                        .text_color(crate::ui::muted(p))
                        .cursor_pointer()
                        .tooltip(move |_window, cx| {
                            Tooltip::view(
                                t!("composer.context_inspector_hint"),
                                &inspector_palette,
                                cx,
                            )
                        })
                        .child(label)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_center(ComposerCenter::ContextInspector);
                                cx.notify();
                            }),
                        ),
                )
            });
        div()
            .debug_selector(|| "composer-toolbar".into())
            .flex()
            .flex_none()
            .flex_row()
            .items_center()
            .gap_1()
            .mx_3()
            .mt_1()
            .mb_2()
            .min_w(px(0.0))
            .child(controls)
            .child(
                crate::ui::icon_button(
                    "composer-send",
                    if self.orchestrator.busy {
                        Icon::Stop
                    } else {
                        Icon::Send
                    },
                    if self.orchestrator.busy {
                        t!("composer.stop_task")
                    } else {
                        t!("composer.send_hint")
                    },
                    p,
                )
                .debug_selector(|| "composer-send".into())
                .on_mouse_down(MouseButton::Left, cx.listener(Self::send)),
            )
            .into_any_element()
    }
}

#[derive(Clone)]
pub(super) struct ComposerInputHandler {
    pub(super) view: WeakEntity<ComposerView>,
}

impl InputHandler for ComposerInputHandler {
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        let view = self.view.upgrade()?;
        let view = view.read(cx);
        let pos = view
            .draft
            .input
            .chars()
            .take(view.cursor)
            .collect::<String>()
            .encode_utf16()
            .count();
        Some(UTF16Selection {
            range: pos..pos,
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
                view.insert_text_at_cursor(text, cx);
            });
        }
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(view) = self.view.upgrade() {
            view.update(cx, |view, cx| {
                view.marked_text = text.into();
                cx.notify();
            });
            // 预编辑串变化时重新上报锚点，候选窗贴住输入行光标。
            window.invalidate_character_coordinates();
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
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        // IME 候选窗锚点：输入行文本 bounds + 光标前缀宽度（探针每帧刷新）。
        let view = self.view.upgrade()?;
        let view = view.read(cx);
        let byte = char_to_byte(&view.draft.input, view.cursor);
        view.ime_anchor
            .caret_bounds(&view.draft.input, byte, window)
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

/// 附件单入口的判型映射：认得的扩展名返回 MIME（图片附件），`None` 走文本文件附件。
/// svg 归图片。判型与 MIME 用同一张表，避免两份清单漂移。
fn image_mime(path: &Path) -> Option<&'static str> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    Some(match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        _ => return None,
    })
}

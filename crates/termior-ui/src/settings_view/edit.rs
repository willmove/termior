//! 文本字段编辑、键盘、鼠标划选与 IME。

use super::*;

impl SettingsView {
    pub(super) fn begin_edit(
        &mut self,
        field: EditField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_edit();
        self.schedule_save_if_dirty(cx);
        self.edit_field = Some(field);
        self.draft = self.value_for(field);
        if field == EditField::ApiKey {
            self.draft.clear();
        }
        self.cursor = self.draft.chars().count();
        self.anchor = None;
        self.drag_anchor = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// 当前选区（字符区间，start <= end）；无选区时返回 None。
    pub(super) fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    /// 删除选区并把光标折叠到选区起点；返回是否确有选区被删除。
    pub(super) fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection() else {
            return false;
        };
        let start = char_to_byte(&self.draft, range.start);
        let end = char_to_byte(&self.draft, range.end);
        self.draft.replace_range(start..end, "");
        self.cursor = range.start;
        self.anchor = None;
        true
    }

    /// 在光标处插入文本（替换当前选区）。单行字段丢弃换行符。
    pub(super) fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.edit_field.is_none() {
            return;
        }
        let text = text.replace(['\r', '\n'], "");
        if text.is_empty() {
            return;
        }
        self.delete_selection();
        self.anchor = None;
        let byte = char_to_byte(&self.draft, self.cursor);
        self.draft.insert_str(byte, &text);
        self.cursor += text.chars().count();
        self.marked_text.clear();
        cx.notify();
    }

    /// 读取剪贴板并把文本粘贴进当前编辑字段。Windows 上打开剪贴板会向
    /// wndproc 回派 sent message，若在 entity 更新中直接读会重入崩溃，
    /// 因此推迟到更新结束之后（与 Composer 的 schedule_attach_clipboard 相同）。
    pub(super) fn paste_from_clipboard(&mut self, cx: &mut Context<Self>) {
        let entity = cx.entity().downgrade();
        cx.defer(move |cx| {
            let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
                return;
            };
            let _ = entity.update(cx, |this, cx| this.insert_text(&text, cx));
        });
    }

    /// 把当前选区复制到剪贴板；`cut` 为真时同时删除选区。
    pub(super) fn copy_selection(&mut self, cut: bool, cx: &mut Context<Self>) {
        let Some(range) = self.selection() else {
            return;
        };
        let text: String = self
            .draft
            .chars()
            .skip(range.start)
            .take(range.end - range.start)
            .collect();
        if text.is_empty() {
            return;
        }
        if cut && self.delete_selection() {
            cx.notify();
        }
        cx.defer(move |cx| cx.write_to_clipboard(ClipboardItem::new_string(text)));
    }

    /// 鼠标按下编辑字段：进入编辑并把光标/选区落到点击处。
    /// 单击定位光标，双击选词，三击全选；随后按住拖拽从对应端划选。
    pub(super) fn begin_mouse_edit(
        &mut self,
        field: EditField,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_edit(field, window, cx);
        let index = self.char_index_at(event.position, window);
        match event.click_count {
            2 => {
                let range = self.word_range_at(index);
                self.anchor = Some(range.start);
                self.cursor = range.end;
                self.drag_anchor = Some(range.start);
            }
            3.. => {
                self.anchor = Some(0);
                self.cursor = self.draft.chars().count();
                self.drag_anchor = Some(0);
            }
            _ => {
                self.cursor = index;
                self.anchor = None;
                self.drag_anchor = Some(index);
            }
        }
        cx.notify();
    }

    /// 拖拽划选：固定端停在按下处，游标端跟随指针。
    pub(super) fn drag_edit_to(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = self.char_index_at(position, window);
        if self.cursor != index || self.anchor.is_none() {
            self.cursor = index;
            self.anchor = Some(self.drag_anchor.unwrap_or(index));
            cx.notify();
        }
    }

    /// 把窗口坐标换算成编辑草稿的字符下标。API key 按掩码测量，与实际渲染一致。
    pub(super) fn char_index_at(&self, position: Point<Pixels>, window: &mut Window) -> usize {
        let Some(field) = self.edit_field else {
            return 0;
        };
        let Some(bounds) = self.edit_bounds else {
            return self.draft.chars().count();
        };
        let text: String = if field == EditField::ApiKey {
            "•".repeat(self.draft.chars().count())
        } else {
            self.draft.clone()
        };
        if text.is_empty() {
            return 0;
        }
        let run = TextRun {
            len: text.len(),
            font: self.edit_font.clone(),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window.text_system().layout_line(
            &text,
            px(termior_ui_kit::tokens::font_size::BODY),
            &[run],
            None,
        );
        let local = position - bounds.origin;
        // 单行文本：只用 x 命中最接近的字符边界。
        let byte = line.closest_index_for_x(local.x).min(text.len());
        text[..byte].chars().count()
    }

    /// 双击选词的词边界：字母数字以及 URL/型号里常见的符号算词内字符。
    pub(super) fn word_range_at(&self, index: usize) -> Range<usize> {
        let chars: Vec<char> = self.draft.chars().collect();
        let is_word =
            |c: char| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/' | '+' | '=');
        let mut start = index.min(chars.len());
        while start > 0 && is_word(chars[start - 1]) {
            start -= 1;
        }
        let mut end = index.min(chars.len());
        while end < chars.len() && is_word(chars[end]) {
            end += 1;
        }
        start..end
    }

    pub(super) fn value_for(&self, field: EditField) -> String {
        match field {
            EditField::FontFamily => self.settings.terminal.font_family.clone(),
            EditField::FontSize => self.settings.terminal.font_size.to_string(),
            EditField::LineHeight => self.settings.terminal.line_height.to_string(),
            EditField::LetterSpacing => self.settings.terminal.letter_spacing.to_string(),
            EditField::Scrollback => self.settings.terminal.scrollback_lines.to_string(),
            EditField::ShellPath => match &self.settings.terminal.shell_detection {
                ShellDetection::Manual { path } => path.clone(),
                ShellDetection::Auto => String::new(),
            },
            EditField::Instructions => self.settings.custom_instructions.clone(),
            // 「添加模型」输入框总是从空白开始。
            EditField::AddModel => String::new(),
            EditField::BaseUrl => self
                .profile()
                .map(|p| p.base_url.clone())
                .unwrap_or_default(),
            EditField::ApiKey => {
                if self.pending_api_key.is_some() {
                    t!("settings.api_key.unsaved").to_string()
                } else if self.credential_present {
                    t!("settings.api_key.stored").to_string()
                } else {
                    t!("settings.api_key.click_to_add").to_string()
                }
            }
            EditField::BackgroundOpacity => self.settings.background.opacity.to_string(),
            EditField::BackgroundBlur => self.settings.background.blur.to_string(),
            EditField::AgentName => self.agent().map(|a| a.name.clone()).unwrap_or_default(),
            EditField::AgentPrompt => self
                .agent()
                .map(|a| a.system_prompt.clone())
                .unwrap_or_default(),
            EditField::AgentIcon => self.agent().map(|a| a.icon.clone()).unwrap_or_default(),
            EditField::AgentColor => self.agent().map(|a| a.color.clone()).unwrap_or_default(),
            EditField::RetentionDays => self.settings.agent_retention.max_age_days.to_string(),
            EditField::RetentionCapacity => {
                bytes_to_mib(self.settings.agent_retention.max_disk_bytes).to_string()
            }
        }
    }

    pub(super) fn commit_edit(&mut self) {
        let Some(field) = self.edit_field.take() else {
            return;
        };
        let value = self.draft.trim().to_owned();
        let result: Result<(), String> = match field {
            EditField::FontFamily => {
                if !value.is_empty() {
                    self.settings.terminal.font_family = value;
                }
                Ok(())
            }
            EditField::FontSize => value
                .parse::<u8>()
                .map(|value| self.settings.terminal.font_size = value)
                .map_err(|error| error.to_string()),
            EditField::LineHeight => value
                .parse::<f32>()
                .map(|value| self.settings.terminal.line_height = value)
                .map_err(|error| error.to_string()),
            EditField::LetterSpacing => value
                .parse::<f32>()
                .map(|value| self.settings.terminal.letter_spacing = value)
                .map_err(|error| error.to_string()),
            EditField::Scrollback => value
                .parse::<u32>()
                .map(|value| self.settings.terminal.scrollback_lines = value)
                .map_err(|error| error.to_string()),
            EditField::ShellPath => {
                // 空路径视同 auto（spawn 侧对空 Manual 已按 None 处理）。
                self.settings.terminal.shell_detection = ShellDetection::Manual { path: value };
                Ok(())
            }
            EditField::Instructions => {
                self.settings.custom_instructions = self.draft.clone();
                Ok(())
            }
            EditField::AddModel => {
                if !value.is_empty() {
                    let added = self
                        .profile_mut()
                        .is_some_and(|profile| profile.add_model(&value));
                    self.status = if added {
                        tf!("settings.status.model_added", "model" => value)
                    } else {
                        tf!("settings.status.model_exists", "model" => value)
                    }
                    .to_string();
                }
                Ok(())
            }
            EditField::BaseUrl => {
                if let Some(profile) = self.profile_mut() {
                    profile.base_url = value;
                }
                Ok(())
            }
            EditField::ApiKey => {
                // 敏感凭据不随字段失焦写入钥匙串，等显式 Save API key。
                if value.is_empty() {
                    Ok(())
                } else {
                    self.pending_api_key = Some(value);
                    self.status = t!("settings.status.api_key_ready").to_string();
                    Ok(())
                }
            }
            EditField::BackgroundOpacity => value
                .parse::<f32>()
                .map(|value| self.settings.background.opacity = value)
                .map_err(|error| error.to_string()),
            EditField::BackgroundBlur => value
                .parse::<f32>()
                .map(|value| self.settings.background.blur = value)
                .map_err(|error| error.to_string()),
            EditField::AgentName => {
                if let Some(agent) = self.agent_mut() {
                    agent.name = value;
                }
                Ok(())
            }
            EditField::AgentPrompt => {
                if let Some(agent) = self.agent_mut() {
                    agent.system_prompt = value;
                }
                Ok(())
            }
            EditField::AgentIcon => {
                if let Some(agent) = self.agent_mut() {
                    agent.icon = value;
                }
                Ok(())
            }
            EditField::AgentColor => {
                if let Some(agent) = self.agent_mut() {
                    agent.color = value;
                }
                Ok(())
            }
            EditField::RetentionDays => value
                .parse::<u64>()
                .ok()
                .filter(|days| {
                    (RetentionPolicy::MIN_AGE_DAYS..=RetentionPolicy::MAX_AGE_DAYS).contains(days)
                })
                .map(|days| self.settings.agent_retention.max_age_days = days)
                .ok_or_else(|| "retention days out of range".to_owned()),
            EditField::RetentionCapacity => value
                .parse::<u64>()
                .ok()
                .and_then(|mib| mib.checked_mul(1024 * 1024))
                .filter(|bytes| {
                    (RetentionPolicy::MIN_DISK_BYTES..=RetentionPolicy::MAX_DISK_BYTES)
                        .contains(bytes)
                })
                .map(|bytes| self.settings.agent_retention.max_disk_bytes = bytes)
                .ok_or_else(|| "retention capacity out of range".to_owned()),
        };
        match result {
            Err(error) => {
                self.status = tf!("settings.status.invalid_value", "error" => error).to_string()
            }
            Ok(()) if field != EditField::ApiKey => {
                self.settings_dirty = true;
            }
            Ok(()) => {}
        }
        self.draft.clear();
        self.cursor = 0;
        self.anchor = None;
        self.drag_anchor = None;
        self.marked_text.clear();
    }

    pub(super) fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(action) = self.capture_shortcut {
            let key = event.keystroke.key.as_str();
            if key == "escape" {
                self.capture_shortcut = None;
                self.status = t!("settings.status.shortcut_cancelled").to_string();
                cx.stop_propagation();
                cx.notify();
                return;
            }
            if matches!(key, "control" | "shift" | "alt" | "meta" | "command") {
                return;
            }
            let modifiers = event.keystroke.modifiers;
            let has_required_modifier = if cfg!(target_os = "macos") {
                modifiers.platform || modifiers.control
            } else {
                modifiers.control
            };
            if !has_required_modifier {
                self.status = t!("settings.status.shortcut_needs_modifier").to_string();
                cx.stop_propagation();
                cx.notify();
                return;
            }
            if key.eq_ignore_ascii_case("s")
                && !modifiers.shift
                && !modifiers.alt
                && (modifiers.control || modifiers.platform)
            {
                self.status = t!("settings.status.shortcut_reserved").to_string();
                cx.stop_propagation();
                cx.notify();
                return;
            }
            let binding = UserKeyBinding {
                primary: if cfg!(target_os = "macos") {
                    modifiers.platform
                } else {
                    true
                },
                shift: modifiers.shift,
                alt: modifiers.alt,
                key: event.keystroke.key.clone(),
            };
            self.status = match self.settings.keymap.rebind(action, binding) {
                Ok(()) => {
                    self.capture_shortcut = None;
                    self.schedule_save(cx);
                    tf!(
                        "settings.status.shortcut_updated",
                        "action" => format!("{action:?}")
                    )
                    .to_string()
                }
                Err(error) => error.to_string(),
            };
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.edit_field.is_none() {
            return;
        }
        let modifiers = event.keystroke.modifiers;
        let primary = if cfg!(target_os = "macos") {
            modifiers.platform
        } else {
            modifiers.control
        };
        if primary && !modifiers.shift && !modifiers.alt {
            match event.keystroke.key.as_str() {
                "v" => self.paste_from_clipboard(cx),
                "a" => {
                    self.anchor = Some(0);
                    self.cursor = self.draft.chars().count();
                    cx.notify();
                }
                "c" => self.copy_selection(false, cx),
                "x" => self.copy_selection(true, cx),
                _ => {}
            }
            cx.stop_propagation();
            return;
        }
        let shift = modifiers.shift;
        if event.keystroke.key == "tab" {
            let fields: &[EditField] = match self.page {
                SettingsPage::Models => {
                    &[EditField::AddModel, EditField::BaseUrl, EditField::ApiKey]
                }
                SettingsPage::Agents if self.agent_appearance_open => &[
                    EditField::RetentionDays,
                    EditField::RetentionCapacity,
                    EditField::AgentName,
                    EditField::AgentPrompt,
                    EditField::AgentIcon,
                    EditField::AgentColor,
                ],
                SettingsPage::Agents => &[
                    EditField::RetentionDays,
                    EditField::RetentionCapacity,
                    EditField::AgentName,
                    EditField::AgentPrompt,
                ],
                _ => &[],
            };
            if let Some(index) = fields
                .iter()
                .position(|field| Some(*field) == self.edit_field)
            {
                let next = if shift {
                    (index + fields.len() - 1) % fields.len()
                } else {
                    (index + 1) % fields.len()
                };
                self.begin_edit(fields[next], window, cx);
                cx.stop_propagation();
                return;
            }
        }
        let extend = |this: &mut Self, target: usize| {
            this.anchor.get_or_insert(this.cursor);
            this.cursor = target;
        };
        match event.keystroke.key.as_str() {
            "enter" | "return" => {
                self.commit_edit();
                self.schedule_save_if_dirty(cx);
            }
            "escape" => {
                self.edit_field = None;
                self.draft.clear();
                self.anchor = None;
            }
            "backspace" if self.selection().is_some() || self.cursor > 0 => {
                if !self.delete_selection() {
                    let start = char_to_byte(&self.draft, self.cursor - 1);
                    let end = char_to_byte(&self.draft, self.cursor);
                    self.draft.replace_range(start..end, "");
                    self.cursor -= 1;
                }
                self.anchor = None;
            }
            "delete" if self.selection().is_some() || self.cursor < self.draft.chars().count() => {
                if !self.delete_selection() {
                    let start = char_to_byte(&self.draft, self.cursor);
                    let end = char_to_byte(&self.draft, self.cursor + 1);
                    self.draft.replace_range(start..end, "");
                }
                self.anchor = None;
            }
            "left" if shift => extend(self, self.cursor.saturating_sub(1)),
            "right" if shift => extend(self, (self.cursor + 1).min(self.draft.chars().count())),
            "home" if shift => extend(self, 0),
            "end" if shift => extend(self, self.draft.chars().count()),
            "left" => {
                self.cursor = self
                    .selection()
                    .map_or_else(|| self.cursor.saturating_sub(1), |range| range.start);
                self.anchor = None;
            }
            "right" => {
                self.cursor = self.selection().map_or_else(
                    || (self.cursor + 1).min(self.draft.chars().count()),
                    |range| range.end,
                );
                self.anchor = None;
            }
            "home" => {
                self.cursor = 0;
                self.anchor = None;
            }
            "end" => {
                self.cursor = self.draft.chars().count();
                self.anchor = None;
            }
            _ => return,
        }
        cx.notify();
    }

    /// 编辑中的输入框内容 + 光标。光标用 ASCII `|` 字符而不是 `▏`（U+258F）：
    /// 后者在 Windows 上经字体回退渲染成一个很宽的空白，还让鼠标命中测试
    /// 产生偏差。`|` 在任何 UI 字体里都是窄竖线，随文本流布局。
    pub(super) fn display_edit(&self, field: EditField) -> String {
        if self.edit_field == Some(field) {
            let mut value = if field == EditField::ApiKey {
                "•".repeat(self.draft.chars().count())
            } else {
                self.draft.clone()
            };
            let byte = char_to_byte(&value, self.cursor);
            value.insert_str(
                byte,
                if self.marked_text.is_empty() {
                    "|"
                } else {
                    "|…"
                },
            );
            value
        } else {
            self.value_for(field)
        }
    }

    /// 编辑中的输入框内容：无选区时整段文本加光标；有选区时高亮选中部分。
    /// 编辑期间在文本区铺一层透明 canvas，paint 时记录内容 bounds，供鼠标命中测试。
    pub(super) fn edit_display(&self, field: EditField, cx: &Context<Self>) -> AnyElement {
        if self.edit_field != Some(field) {
            return div()
                .child(SharedString::from(self.value_for(field)))
                .into_any_element();
        }
        let bounds_probe = {
            let view = cx.entity().downgrade();
            canvas(
                |_, _, _| (),
                move |bounds, _, _, cx| {
                    let _ = view.update(cx, |view, _| view.edit_bounds = Some(bounds));
                },
            )
            // 显式钉在定位祖先原点：无 inset 的 absolute 会排在静态位置（文本之后）。
            .absolute()
            .top_0()
            .left_0()
            .size_full()
        };
        let chars: Vec<char> = if field == EditField::ApiKey {
            "•".repeat(self.draft.chars().count()).chars().collect()
        } else {
            self.draft.chars().collect()
        };
        let p = &self.palette;
        let run = |text: String, highlighted: bool| -> Option<AnyElement> {
            if text.is_empty() {
                return None;
            }
            let mut element = div();
            if highlighted {
                element = element
                    .bg(crate::ui::selected_wash(p))
                    .rounded_sm()
                    .text_color(crate::ui::color(p.foreground));
            }
            Some(element.child(SharedString::from(text)).into_any_element())
        };
        if let Some(range) = self.selection() {
            let prefix = chars[..range.start].iter().collect();
            let selected = chars[range.start..range.end].iter().collect();
            let suffix = chars[range.end..].iter().collect();
            div()
                .flex()
                .flex_row()
                .items_baseline()
                .relative()
                .children(run(prefix, false))
                .children(run(selected, true))
                .children(run(suffix, false))
                .child(bounds_probe)
                .child(crate::ime_anchor::anchor_probe(&self.ime_anchor))
                .into_any_element()
        } else {
            // 无选区：单一文本子节点（含 `|` 光标字符）。不做 flex 拆段加
            // 独立光标元素——拆段曾在真实字体下被压成逐字换行的竖排。
            div()
                .relative()
                .child(SharedString::from(self.display_edit(field)))
                .child(bounds_probe)
                .child(crate::ime_anchor::anchor_probe(&self.ime_anchor))
                .into_any_element()
        }
    }

    pub(super) fn edit_row(
        &self,
        label: impl Into<SharedString>,
        description: impl Into<SharedString>,
        field: EditField,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = label.into();
        let description = description.into();
        let p = self.palette.clone();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_sm().child(label))
            .child(
                div()
                    .text_xs()
                    .text_color(crate::ui::muted(&p))
                    .child(description),
            )
            .child(
                termior_ui_kit::input_field(&p, self.edit_field == Some(field))
                    .id(SharedString::from(format!("edit-{field:?}")))
                    .child(self.edit_display(field, cx))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.begin_mouse_edit(field, event, window, cx)
                        }),
                    )
                    .on_mouse_move(
                        cx.listener(move |this, event: &MouseMoveEvent, window, cx| {
                            if this.edit_field == Some(field)
                                && event.pressed_button == Some(MouseButton::Left)
                            {
                                this.drag_edit_to(event.position, window, cx);
                            }
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _event: &MouseUpEvent, _window, cx| {
                            if this.drag_anchor.take().is_some() {
                                cx.notify();
                            }
                        }),
                    ),
            )
            .into_any_element()
    }
}

#[derive(Clone)]
pub(super) struct SettingsInputHandler {
    pub(super) view: WeakEntity<SettingsView>,
}

impl InputHandler for SettingsInputHandler {
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        let view = self.view.upgrade()?;
        let view = view.read(cx);
        let utf16 = |char_index: usize| {
            view.draft
                .chars()
                .take(char_index)
                .collect::<String>()
                .encode_utf16()
                .count()
        };
        match view.selection() {
            Some(range) => Some(UTF16Selection {
                range: utf16(range.start)..utf16(range.end),
                reversed: view.anchor.is_some_and(|anchor| anchor > view.cursor),
            }),
            None => {
                let position = utf16(view.cursor);
                Some(UTF16Selection {
                    range: position..position,
                    reversed: false,
                })
            }
        }
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
                if view.edit_field.is_some() {
                    view.insert_text(text, cx);
                }
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
            // 预编辑串变化时重新上报锚点，候选窗贴住编辑字段光标。
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
        // IME 候选窗锚点：编辑字段文本 bounds + 光标前缀宽度（探针每帧刷新）。
        // API key 按掩码测量，与渲染一致。
        let view = self.view.upgrade()?;
        let view = view.read(cx);
        let text = if view.edit_field == Some(EditField::ApiKey) {
            "•".repeat(view.draft.chars().count())
        } else {
            view.draft.clone()
        };
        let byte = char_to_byte(&text, view.cursor);
        view.ime_anchor.caret_bounds(&text, byte, window)
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

//! 会话消息流：文本选取、流式草稿，以及消息与工具块的渲染。

use super::*;

impl ComposerView {
    /// 在消息区按下左键：命中文字则开始选取（双击选词、三击选整段、Shift 扩展），
    /// 否则清除选区。不阻止冒泡，面板的聚焦与收起菜单逻辑照常执行。
    pub(super) fn begin_text_selection(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(point) = self.selectable_texts.hit(event.position) else {
            self.selecting_text = false;
            if self.text_selection.take().is_some() {
                cx.notify();
            }
            return;
        };
        // 复制快捷键要落到面板上，而不是仍持有焦点的终端（那里 Ctrl+C 是中断）。
        window.focus(&self.focus_handle, cx);
        self.selecting_text = false;
        self.text_selection = match event.click_count {
            2 => self.selectable_texts.word_at(point),
            count if count >= 3 => self.selectable_texts.segment_at(point),
            _ => {
                self.selecting_text = true;
                match self.text_selection {
                    Some(selection) if event.modifiers.shift => Some(TextSelection {
                        anchor: selection.anchor,
                        head: point,
                    }),
                    _ => Some(TextSelection::caret(point)),
                }
            }
        };
        cx.notify();
    }

    /// 拖选期间跟踪整个窗口的鼠标：拖出消息区仍能延伸选区，任意位置松开结束。
    pub(super) fn text_drag_tracker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selecting = self.selecting_text;
        let view = cx.entity().downgrade();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                if !selecting {
                    return;
                }
                let moved = view.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase != DispatchPhase::Bubble
                        || event.pressed_button != Some(MouseButton::Left)
                    {
                        return;
                    }
                    let _ = moved.update(cx, |this, cx| {
                        this.extend_text_selection(event.position, cx)
                    });
                });
                let released = view.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = released.update(cx, |this, _| this.selecting_text = false);
                    }
                });
            },
        )
        .absolute()
        .size_0()
    }

    pub(super) fn extend_text_selection(
        &mut self,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(selection) = self.text_selection.as_mut() else {
            return;
        };
        if let Some(head) = self.selectable_texts.nearest(position) {
            if selection.head != head {
                selection.head = head;
                cx.notify();
            }
        }
    }

    /// 复制选中的消息文本；没有选区时不做任何事。
    pub(super) fn copy_text_selection(&mut self, cx: &mut Context<Self>) {
        let Some(selection) = self
            .text_selection
            .filter(|selection| !selection.is_empty())
        else {
            return;
        };
        let text = self.selectable_texts.selected_text(selection);
        if text.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.set_status(t!("composer.status.selection_copied"));
        cx.notify();
    }

    /// 追加流式文本增量：把 TextDelta 实时拼到当前 assistant 消息草稿上。
    /// 若 history 末尾不是 assistant，先 push 一条空 assistant 作为流式占位。
    /// 最终消息会在 apply_agent_result 时被 AgentOutcome.messages 整体覆盖。
    pub(super) fn append_text_delta(&mut self, delta: String) {
        let draft_open = self.streaming_draft
            && matches!(self.history.last(), Some(m) if m.role == Role::Assistant);
        if !draft_open {
            self.history.push(Message::assistant(String::new()));
        }
        self.streaming_draft = true;
        if let Some(last) = self.history.last_mut() {
            last.content.push_str(&delta);
        }
    }

    /// 运行时追加到历史的定稿消息（实时推送）：assistant 消息替换流式占位，工具结果直接追加；
    /// 用户/系统消息在提交时已显示或不属于会话视图，忽略。命令处理返回后
    /// apply_task_result 仍以运行时历史整体覆盖，这里只负责运行期间的即时显示。
    pub(super) fn apply_live_message(&mut self, message: Message) {
        match message.role {
            Role::Assistant => {
                let draft_open = self.streaming_draft
                    && matches!(self.history.last(), Some(m) if m.role == Role::Assistant);
                match self.history.last_mut() {
                    Some(last) if draft_open => *last = message,
                    _ => self.history.push(message),
                }
            }
            Role::Tool => self.history.push(message),
            Role::User | Role::System => return,
        }
        self.streaming_draft = false;
    }

    pub(super) fn render_conversation(
        &mut self,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        // 工具结果按 call_id 并入发起它的 assistant 消息显示；找不到发起方的结果单独成块。
        let issued_calls: HashSet<&str> = self
            .history
            .iter()
            .flat_map(|message| message.tool_calls.iter().map(|call| call.id.as_str()))
            .collect();
        let tool_results: HashMap<&str, &termior_ai::ToolResult> = self
            .history
            .iter()
            .filter_map(|message| message.tool_result.as_ref())
            .map(|result| (result.call_id.as_str(), result))
            .collect();
        // 选区坐标以消息在会话中的序号分组：历史只追加，序号在流式更新间保持稳定。
        let conversation = self
            .history
            .iter()
            .filter(|message| match message.role {
                Role::User | Role::Assistant => true,
                Role::Tool => message
                    .tool_result
                    .as_ref()
                    .is_some_and(|result| !issued_calls.contains(result.call_id.as_str())),
                Role::System => false,
            })
            .enumerate()
            .collect::<Vec<_>>();
        let skipped = conversation
            .len()
            .saturating_sub(COMPOSER_RENDERED_MESSAGES);
        let texts = self.selectable_texts.clone();
        texts.clear();
        let selection = self.text_selection;
        let selection_background: Hsla = crate::ui::alpha(p.accent, 0.35).into();
        let tool_style = tool_blocks::ToolBlockStyle {
            palette: p,
            texts: &texts,
            selection,
            selection_background,
        };
        let messages = conversation
            .iter()
            .enumerate()
            .skip(skipped)
            .map(|(position, &(index, message))| {
                let is_user = message.role == Role::User;
                // 同一轮里连续的 Agent 输出（文本与工具调用）只在开头标一次名字。
                let show_label = is_user
                    || position == skipped
                    || conversation[position - 1].1.role == Role::User;
                let label = if is_user {
                    t!("composer.message_you")
                } else {
                    t!("composer.message_agent")
                };
                let label_color = if is_user {
                    crate::ui::muted(p)
                } else {
                    crate::ui::color(p.accent)
                };
                // Agent 回复按 Markdown 渲染（复用预览的 pulldown-cmark + GPUI
                // 管线，见 SDD 依赖决策）；用户消息保持原样，避免输入里的
                // *、# 等字符被误解析成格式。
                let body = if is_user {
                    // 整条消息作为一段多行文本，复制结果与原文一致；
                    // 空消息用不断行空格占位，避免塌陷成零高度。
                    let content = message.content.replace("\r\n", "\n");
                    let content = if content.is_empty() {
                        "\u{00A0}".to_owned()
                    } else {
                        content
                    };
                    Some(div().text_xs().cursor_text().children(texts.text(
                        SegmentKey {
                            group: index,
                            seq: 0,
                        },
                        content.into(),
                        None,
                        Vec::new(),
                        selection,
                        selection_background,
                    )))
                } else if message.role == Role::Assistant
                    && (message.tool_calls.is_empty() || !message.content.trim().is_empty())
                {
                    // 流式期间每个增量都会重渲染并重新解析；消息量已被
                    // COMPOSER_RENDERED_MESSAGES 截断，解析成本可接受。
                    let document = MarkdownDocument::parse(&message.content);
                    let blocks = crate::markdown_render::render_selectable_blocks(
                        &document,
                        crate::markdown_render::Density::CHAT,
                        &format!("composer-md-{index}"),
                        p,
                        crate::markdown_render::Selectable {
                            texts: &texts,
                            group: index,
                            selection,
                            background: selection_background,
                        },
                    );
                    Some(
                        div()
                            .flex()
                            .flex_col()
                            .min_w(px(0.0))
                            .cursor_text()
                            .text_size(px(crate::markdown_render::Density::CHAT.base))
                            .line_height(relative(1.5))
                            // 首个增量到达前内容为空，用占位空行维持行高。
                            .when(blocks.is_empty(), |body| {
                                body.child(SharedString::from("\u{00A0}"))
                            })
                            .children(blocks),
                    )
                } else {
                    None
                };
                let tool_blocks = message
                    .tool_calls
                    .iter()
                    .enumerate()
                    .map(|(ordinal, call)| {
                        let result = tool_results.get(call.id.as_str()).copied();
                        self.tool_block(index, ordinal, call, result, &tool_style, cx)
                    })
                    .chain(message.tool_result.as_ref().map(|result| {
                        let call = termior_ai::ToolCall {
                            id: result.call_id.clone(),
                            name: t!("composer.tool.result_label").to_string(),
                            arguments: String::new(),
                        };
                        self.tool_block(index, 0, &call, Some(result), &tool_style, cx)
                    }))
                    .collect::<Vec<_>>();
                div()
                    .flex()
                    .flex_col()
                    .min_w(px(0.0))
                    .gap(px(2.0))
                    .when(show_label, |item| {
                        item.child(div().text_xs().text_color(label_color).child(label))
                    })
                    .children(body)
                    .children(tool_blocks)
            })
            .collect::<Vec<_>>();
        messages
            .into_iter()
            .map(IntoElement::into_any_element)
            .collect()
    }
}

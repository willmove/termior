//! Composer 面板骨架：标题栏、对话区装配与底部工具栏。

use super::input::ComposerInputHandler;
use super::*;

impl Focusable for ComposerView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::Render for ComposerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = crate::ui::palette(cx);
        let focus = self.focus_handle.clone();
        let input_focus = focus.clone();
        let handler = ComposerInputHandler {
            view: cx.entity().downgrade(),
        };
        let external_backend =
            self.orchestrator.backend_choice == AgentBackendChoice::CodexAppServer;
        let environment_id = self
            .orchestrator
            .active_task
            .as_ref()
            .map(|runtime| runtime.task().environment_id.clone())
            .unwrap_or_else(|| {
                if external_backend {
                    "codex-workspace-write".into()
                } else {
                    "direct-workspace".into()
                }
            });
        let environment_tooltip = if external_backend {
            tf!(
                "composer.environment_external",
                "host" => std::env::consts::OS,
                "root" => self.workspace_root.display()
            )
        } else {
            tf!(
                "composer.environment_builtin",
                "host" => std::env::consts::OS,
                "root" => self.workspace_root.display()
            )
        };
        let compact = self.is_compact();
        let placeholder = self.model_setup_placeholder();
        let show_status_row = placeholder.is_none() && !self.status.is_empty();
        let placeholder_active = self.draft.input.is_empty() && placeholder.is_some();
        // 占位态显示引导文案 + 光标；光标是文本内的 `|` 字符，随文本流布局。
        let input_display = if placeholder_active {
            format!("{}|", placeholder.unwrap_or_default())
        } else {
            self.display_input()
        };
        // 贴底时跟随新内容下滚；用户向上翻阅（离开底部）后不再强制滚动。
        // offset/max 读取的是上一帧布局的值——判定略滞后一帧，对本场景足够。
        let scroll_offset = self.scroll_handle.offset().y;
        let scroll_max = self.scroll_handle.max_offset().y;
        if scroll_max + scroll_offset <= px(8.0) {
            self.scroll_handle.scroll_to_bottom();
        }
        let messages = self.render_conversation(&p, cx);
        // 菜单与任务详情需要 &mut cx 注册监听，须在惰性的 chips 迭代器借用 cx 之前构建。
        let agent_menu = self.agent_menu_open.then(|| {
            self.agent_menu(
                &p,
                if show_status_row {
                    MODE_MENU_BOTTOM + STATUS_ROW_HEIGHT
                } else {
                    MODE_MENU_BOTTOM
                },
                cx,
            )
        });
        let model_menu = self.model_menu_open.then(|| {
            self.model_menu(
                &p,
                if show_status_row {
                    MODE_MENU_BOTTOM + STATUS_ROW_HEIGHT
                } else {
                    MODE_MENU_BOTTOM
                },
                cx,
            )
        });
        let text_drag_tracker = self.text_drag_tracker(cx);
        let task_details = self.task_details_center(&p, cx);
        let chips = self.render_attachment_chips(&p, cx);
        let path_suggestions = self.render_path_suggestions(&p, cx);
        let slash_suggestions = self.render_slash_suggestions(&p, cx);
        let snippet_suggestions = self.render_snippet_suggestions(&p, cx);
        let todo_center = self.render_todo_center(&p, cx);
        let snippet_center = self.render_snippet_center(&p, cx);
        let approval = self.render_approval_card(&p, cx);
        let edit_review = self.render_edit_review(&p, cx);
        let context_inspector = self.render_context_inspector(&p, cx);
        let recovery_center = self.render_recovery_center(&p, cx);
        let automation_center = self.render_automation_center(&p, cx);
        let checkpoint_center = self.render_checkpoint_center(&p, cx);
        let skill_center = self.render_skill_center(&p, cx);
        let memory_center = self.render_memory_center(&p, cx);
        let budget_resume = self.render_budget_resume(&p, cx);
        let plan_review = self.render_plan_review(&p, cx);
        // 模式下拉菜单：浮在工具栏上方（面板底部附近），最后挂载以便盖住下层内容。
        // 锚定面板底缘而非光标位置——菜单向下展开会被 overflow_hidden 裁掉。
        // 偏移 = 工具栏行高（+ 状态行高度），行高变化时需同步这两个值。
        let mode_menu = {
            let selected_mode = self.mode;
            menu_panel(&p)
                .absolute()
                .left(px(MODE_MENU_LEFT))
                .bottom(px(if show_status_row {
                    MODE_MENU_BOTTOM + STATUS_ROW_HEIGHT
                } else {
                    MODE_MENU_BOTTOM
                }))
                .min_w(px(230.0))
                .child(div().flex().flex_col().gap(px(2.0)).children(
                    [Mode::Auto, Mode::Plan, Mode::Yolo].map(|mode| {
                        let selected = mode == selected_mode;
                        let hover = crate::ui::hover_wash(&p);
                        // 档位名 Auto/Plan/Yolo 是专有名词保留原文；一句话说明走 i18n。
                        let description = match mode {
                            Mode::Auto => t!("composer.mode.auto_desc"),
                            Mode::Plan => t!("composer.mode.plan_desc"),
                            Mode::Yolo => t!("composer.mode.yolo_desc"),
                        };
                        div()
                            .id(SharedString::from(format!(
                                "composer-mode-{}",
                                mode.label().to_lowercase()
                            )))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .flex()
                            .flex_col()
                            .gap(px(1.0))
                            .when(selected, |item| item.bg(crate::ui::selected_wash(&p)))
                            .cursor_pointer()
                            .hover(move |style| style.bg(hover))
                            .child(mode.label())
                            .child(div().text_color(crate::ui::muted(&p)).child(description))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.set_mode(mode, cx);
                                }),
                            )
                    }),
                ))
        };
        // 面板互斥展开，至多一个为 Some。
        let center_panel = task_details
            .or(todo_center)
            .or(snippet_center)
            .or(memory_center)
            .or(skill_center)
            .or(checkpoint_center)
            .or(automation_center)
            .or(recovery_center)
            .or(context_inspector);
        let prompt_row = self.render_prompt_row(&p, window, placeholder_active, input_display);
        let toolbar = self.render_toolbar(&p, cx);
        div()
            .flex()
            .flex_col()
            .relative()
            .overflow_hidden()
            .when(
                self.fill_workspace || self.dock == ComposerDock::Right,
                |root| root.w_full().h_full(),
            )
            .when(
                !self.fill_workspace && self.dock == ComposerDock::Bottom,
                |root| {
                    root.w_full()
                        .when(compact, |root| root.min_h(px(COMPOSER_COMPACT_HEIGHT)))
                        .when(!compact, |root| root.h(px(self.panel_height)))
                },
            )
            // 分隔线由工作区在面板外沿绘制的拖拽手柄承担。
            .bg(crate::ui::color(p.elevated))
            .text_color(crate::ui::color(p.foreground))
            .track_focus(&focus)
            .on_key_down(cx.listener(Self::handle_key_down))
            // 面板空白处点击收起模式下拉；chip 与菜单项各自 stop_propagation。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.mode_menu_open || this.agent_menu_open || this.model_menu_open {
                        this.mode_menu_open = false;
                        this.agent_menu_open = false;
                        this.model_menu_open = false;
                        cx.notify();
                    }
                }),
            )
            // 面板头：面板级动作的常驻入口（切换停靠 / 收起），取代埋在输入行里的文字按钮。
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .h(px(COMPOSER_HEADER_HEIGHT))
                    .px_2()
                    .flex_none()
                    .border_b_1()
                    .border_color(crate::ui::border(&p))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .px_1()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(t!("composer.header")),
                            )
                            .when(self.last_task.is_some(), |header| {
                                header.child(
                                    div()
                                        .id("composer-task-details-badge")
                                        .px_2()
                                        .py(px(2.0))
                                        .rounded_md()
                                        .bg(crate::ui::alpha(p.status[0], 0.12))
                                        .text_xs()
                                        .cursor_pointer()
                                        .child(t!("composer.task.badge"))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.toggle_center(ComposerCenter::TaskDetails);
                                                if this.center_open(ComposerCenter::TaskDetails) {
                                                    this.ensure_command_watch(cx);
                                                }
                                                cx.notify();
                                            }),
                                        ),
                                )
                            })
                            .when(!self.recovery_tasks.is_empty(), |header| {
                                header.child(
                                    div()
                                        .id("composer-recovery-center")
                                        .px_2()
                                        .py(px(2.0))
                                        .rounded_md()
                                        .bg(crate::ui::alpha(p.status[2], 0.14))
                                        .text_xs()
                                        .cursor_pointer()
                                        .child(tf!(
                                            "composer.recovery.badge",
                                            "count" => self.recovery_tasks.len()
                                        ))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.toggle_center(ComposerCenter::Recovery);
                                                cx.notify();
                                            }),
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .id("composer-automation-center")
                                    .px_2()
                                    .py(px(2.0))
                                    .rounded_md()
                                    .bg(crate::ui::alpha(p.accent, 0.10))
                                    .text_xs()
                                    .cursor_pointer()
                                    .child(tf!(
                                        "composer.automation.badge",
                                        "count" => self.automations.automations.len()
                                    ))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.toggle_center(ComposerCenter::Automations);
                                            cx.notify();
                                        }),
                                    ),
                            )
                            .when(!self.checkpoints.is_empty(), |header| {
                                header.child(
                                    div()
                                        .id("composer-checkpoint-center")
                                        .px_2()
                                        .py(px(2.0))
                                        .rounded_md()
                                        .bg(crate::ui::alpha(p.status[1], 0.12))
                                        .text_xs()
                                        .cursor_pointer()
                                        .child(tf!(
                                            "composer.checkpoint.badge",
                                            "count" => self.checkpoints.len()
                                        ))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.toggle_center(ComposerCenter::Checkpoints);
                                                cx.notify();
                                            }),
                                        ),
                                )
                            })
                            .when(
                                !self.skills.is_empty() || self.skill_scan_error.is_some(),
                                |header| {
                                    header.child(
                                        div()
                                            .id("composer-skill-center")
                                            .px_2()
                                            .py(px(2.0))
                                            .rounded_md()
                                            .border_1()
                                            .border_color(crate::ui::border(&p))
                                            .text_xs()
                                            .cursor_pointer()
                                            .child(tf!(
                                                "composer.skills.badge",
                                                "count" => self.skills.len()
                                            ))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.toggle_center(ComposerCenter::Skills);
                                                    cx.notify();
                                                }),
                                            ),
                                    )
                                },
                            )
                            .when(
                                !self.memory.candidates.is_empty()
                                    || !self.memory.entries.is_empty(),
                                |header| {
                                    header.child(
                                        div()
                                            .id("composer-memory-center")
                                            .px_2()
                                            .py(px(2.0))
                                            .rounded_md()
                                            .border_1()
                                            .border_color(crate::ui::border(&p))
                                            .text_xs()
                                            .cursor_pointer()
                                            .child(tf!(
                                                "composer.memory.badge",
                                                "count" => self.memory.candidates.len()
                                                    + self.memory.entries.len()
                                            ))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.toggle_center(ComposerCenter::Memory);
                                                    cx.notify();
                                                }),
                                            ),
                                    )
                                },
                            )
                            .child({
                                let environment_palette = p.clone();
                                div()
                                    .id("composer-environment")
                                    .px_2()
                                    .py(px(2.0))
                                    .rounded_md()
                                    .border_1()
                                    .border_color(crate::ui::border(&p))
                                    .text_xs()
                                    .child(SharedString::from(environment_id))
                                    .tooltip(move |_window, cx| {
                                        Tooltip::view(
                                            environment_tooltip.clone(),
                                            &environment_palette,
                                            cx,
                                        )
                                    })
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .child(
                                crate::ui::icon_button(
                                    "composer-dock",
                                    self.dock.toggle_icon(),
                                    t!(self.dock.toggle_label_key()),
                                    &p,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|_, _, _, cx| {
                                        cx.emit(ComposerDockToggle);
                                    }),
                                ),
                            )
                            .child(
                                crate::ui::icon_button(
                                    "composer-collapse",
                                    Icon::ChevronDown,
                                    t!("composer.hide_panel"),
                                    &p,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|_, _, _, cx| {
                                        cx.emit(ComposerCollapse);
                                    }),
                                ),
                            ),
                    ),
            )
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
            .when(!compact, |root| {
                // 消息与审批/评审卡片同属对话流，共享滚动区：面板调矮后
                // Approve/Reject 等操作滚动可达，而不是被固定高度裁掉。
                root.child(
                    div()
                        .id("composer-messages")
                        .debug_selector(|| "composer-messages".into())
                        .relative()
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::begin_text_selection))
                        .child(text_drag_tracker)
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h(px(0.0))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll_handle)
                        .px_3()
                        .pt_2()
                        .pb_1()
                        .gap_2()
                        .children(messages)
                        .children(approval)
                        .children(edit_review)
                        .children(budget_resume)
                        .children(plan_review),
                )
                // 检查面板独立于对话流：限高并自带滚动，展开后最近的回复仍然可见。
                .when_some(center_panel, |root, panel| {
                    root.child(
                        div()
                            .id("composer-center-dock")
                            .debug_selector(|| "composer-center-dock".into())
                            .flex()
                            .flex_row()
                            .flex_none()
                            .max_h(relative(CENTER_DOCK_MAX_FRACTION))
                            .mx_3()
                            .mb_1()
                            .gap_1()
                            .child(
                                div()
                                    .id("composer-center-scroll")
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .overflow_y_scroll()
                                    .child(panel),
                            )
                            .child(
                                crate::ui::icon_button(
                                    "composer-center-close",
                                    Icon::Close,
                                    t!("composer.close_center"),
                                    &p,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.open_center = None;
                                        cx.notify();
                                    }),
                                ),
                            ),
                    )
                })
            })
            .child(div().flex().flex_row().px_3().gap_1().children(chips))
            .children(slash_suggestions)
            .children(snippet_suggestions)
            .children(path_suggestions)
            .child(prompt_row)
            .child(toolbar)
            .when(show_status_row, |root| {
                root.child(
                    div()
                        .px_3()
                        .pb_1()
                        .text_xs()
                        .text_color(crate::ui::muted(&p))
                        .child(SharedString::from(self.status.clone())),
                )
            })
            // 模式下拉最后挂载：绘制顺序在输入区之后，能盖住下层内容。
            .when(self.mode_menu_open && !external_backend, |root| {
                root.child(mode_menu)
            })
            .when_some(agent_menu, |root, menu| root.child(menu))
            .when_some(model_menu, |root, menu| root.child(menu))
    }
}

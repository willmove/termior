//! 通用页：语言、终端与编辑器。

use super::*;

impl SettingsView {
    /// 语言选择（`None` = 跟随系统）。i18n 是进程级全局状态：这里立即切换并
    /// notify 重渲染设置窗口；主窗口经防抖保存的 `on_save → apply_settings`
    /// 路径同步（与主题实时预览一致的最大 400ms 延迟）。
    pub(super) fn select_language(&mut self, language: Option<&str>, cx: &mut Context<Self>) {
        self.settings.language = language.map(str::to_owned);
        termior_i18n::init(self.settings.language.as_deref());
        self.select_menu = None;
        self.schedule_save(cx);
        cx.notify();
    }

    /// shell 下拉选择：写回 shell_detection / wsl_distribution（映射见
    /// `shell_select::apply_option`），主窗口经防抖保存路径同步。
    pub(super) fn select_shell(
        &mut self,
        option: crate::shell_select::ShellOption,
        cx: &mut Context<Self>,
    ) {
        crate::shell_select::apply_option(&option, &mut self.settings);
        self.select_menu = None;
        self.schedule_save(cx);
        cx.notify();
    }

    /// Terminal 区：默认 shell 下拉（选中「手动路径」时追加路径输入行）、
    /// 字体等编辑行，以及「新建终端时选择 Shell」开关。
    pub(super) fn terminal_section(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::shell_select::{self, ShellOption};

        let shell_menu_open = self.select_menu == Some(SelectMenu::Shell);
        let selection = shell_select::selected_option(&self.settings, &self.discovered_shells);
        let shell_menu = shell_menu_open.then(|| {
            let mut items: Vec<gpui::Stateful<gpui::Div>> = vec![Self::select_option(
                t!("settings.terminal.shell.auto"),
                "shell-option-default",
                selection == ShellOption::Default,
                &self.palette,
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.select_shell(ShellOption::Default, cx);
                }),
            )];
            for (index, shell) in self.discovered_shells.iter().enumerate() {
                let option = ShellOption::from(shell);
                let selected = option == selection;
                items.push(
                    Self::select_option(
                        option.label(),
                        SharedString::from(format!("shell-option-{index}")),
                        selected,
                        &self.palette,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.select_shell(option.clone(), cx);
                        }),
                    ),
                );
            }
            items.push(
                Self::select_option(
                    t!("settings.terminal.shell.manual"),
                    "shell-option-manual",
                    selection == ShellOption::Manual,
                    &self.palette,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.select_shell(ShellOption::Manual, cx);
                    }),
                ),
            );
            div()
                .w(px(360.0))
                .p_1()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(&self.palette))
                .bg(crate::ui::color(self.palette.overlay))
                .shadow_md()
                .children(items)
        });
        let shell_prompt = self.settings.terminal.shell_prompt;

        let mut rows: Vec<AnyElement> = vec![div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                Self::select_button(
                    t!("settings.terminal.shell"),
                    shell_select::selection_value(&selection, &self.settings),
                    "shell-select",
                    shell_menu_open,
                    &self.palette,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_select_menu(SelectMenu::Shell, cx);
                    }),
                ),
            )
            .when_some(shell_menu, |select, menu| select.child(menu))
            .into_any_element()];
        if selection == ShellOption::Manual {
            rows.push(self.edit_row(
                t!("settings.terminal.shell.manual_path"),
                t!("settings.terminal.shell.manual_path_description"),
                EditField::ShellPath,
                cx,
            ));
        }
        rows.extend([
            self.edit_row(
                t!("settings.terminal.font_family"),
                t!("settings.terminal.font_family_description"),
                EditField::FontFamily,
                cx,
            ),
            self.edit_row(
                t!("settings.terminal.font_size"),
                t!("settings.terminal.font_size_description"),
                EditField::FontSize,
                cx,
            ),
            self.edit_row(
                t!("settings.terminal.line_height"),
                t!("settings.terminal.line_height_description"),
                EditField::LineHeight,
                cx,
            ),
            self.edit_row(
                t!("settings.terminal.letter_spacing"),
                t!("settings.terminal.letter_spacing_description"),
                EditField::LetterSpacing,
                cx,
            ),
            self.edit_row(
                t!("settings.terminal.scrollback"),
                t!("settings.terminal.scrollback_description"),
                EditField::Scrollback,
                cx,
            ),
        ]);
        // 「新建终端时选择 Shell」：开启后新建终端入口先弹选择器（一次性的
        // 本次选择，不改默认）。沿用编辑区开关按钮的形态。
        rows.push(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .child(t!("settings.terminal.shell_prompt_label")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(crate::ui::muted(&self.palette))
                        .child(t!("settings.terminal.shell_prompt_description")),
                )
                .child(
                    Self::button(
                        tf!(
                            "settings.terminal.shell_prompt",
                            "state" => on_off(shell_prompt)
                        ),
                        "shell-prompt-toggle",
                        &self.palette,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.settings.terminal.shell_prompt =
                                !this.settings.terminal.shell_prompt;
                            this.schedule_save(cx);
                            cx.notify();
                        }),
                    ),
                )
                .into_any_element(),
        );
        self.section(
            t!("settings.terminal.title"),
            t!("settings.terminal.description"),
            rows,
        )
    }

    pub(super) fn general_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let autocomplete = self.settings.autocomplete_enabled;
        let vim = self.settings.vim_mode;
        let dotfiles = self.settings.show_dotfiles;
        // 语言下拉：当前值显示母语名（不翻译），`None` 显示「跟随系统」。
        let language_menu_open = self.select_menu == Some(SelectMenu::Language);
        let current_language = self
            .settings
            .language
            .as_deref()
            .and_then(termior_i18n::canonicalize);
        let language_label = match current_language {
            Some(id) => SharedString::from(
                termior_i18n::available_locales()
                    .iter()
                    .find(|locale| locale.id == id)
                    .map(|locale| locale.native_name)
                    .unwrap_or(id),
            ),
            None => t!("settings.language.follow_system"),
        };
        let language_menu = language_menu_open.then(|| {
            div()
                .w(px(360.0))
                .p_1()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(&self.palette))
                .bg(crate::ui::color(self.palette.overlay))
                .shadow_md()
                .child(
                    Self::select_option(
                        t!("settings.language.follow_system"),
                        "language-option-follow-system",
                        current_language.is_none(),
                        &self.palette,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.select_language(None, cx);
                        }),
                    ),
                )
                .children(termior_i18n::available_locales().iter().map(|locale| {
                    let id = locale.id;
                    Self::select_option(
                        locale.native_name,
                        SharedString::from(format!("language-option-{id}")),
                        current_language == Some(id),
                        &self.palette,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.select_language(Some(id), cx);
                        }),
                    )
                }))
        });
        div()
            .flex()
            .flex_col()
            .gap_6()
            .child(
                self.section(
                    t!("settings.language"),
                    t!("settings.language_description"),
                    [div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            Self::select_button(
                                t!("settings.language"),
                                language_label,
                                "language-select",
                                language_menu_open,
                                &self.palette,
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.toggle_select_menu(SelectMenu::Language, cx);
                                }),
                            ),
                        )
                        .when_some(language_menu, |select, menu| select.child(menu))
                        .into_any_element()],
                ),
            )
            .child(
                self.section(
                    t!("settings.ssh.section_title"),
                    t!("settings.ssh.description"),
                    [Self::button(
                        t!("settings.ssh.manage_button"),
                        "ssh-settings",
                        &self.palette,
                    )
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(OpenSshManager)))
                    .into_any_element()],
                ),
            )
            .child(self.terminal_section(cx))
            .child(
                self.section(
                    t!("settings.editor.title"),
                    t!("settings.editor.description"),
                    [
                        self.edit_row(
                            t!("settings.editor.custom_instructions"),
                            t!("settings.editor.custom_instructions_description"),
                            EditField::Instructions,
                            cx,
                        ),
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                Self::button(
                                    tf!("settings.editor.autocomplete", "state" => on_off(autocomplete)),
                                    "autocomplete",
                                    &self.palette,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.settings.autocomplete_enabled =
                                            !this.settings.autocomplete_enabled;
                                        this.schedule_save(cx);
                                        cx.notify();
                                    }),
                                ),
                            )
                            .child(
                                Self::button(
                                    tf!("settings.editor.vim", "state" => on_off(vim)),
                                    "vim",
                                    &self.palette,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.settings.vim_mode = !this.settings.vim_mode;
                                        this.schedule_save(cx);
                                        cx.notify();
                                    }),
                                ),
                            )
                            .child(
                                Self::button(
                                    tf!("settings.editor.dotfiles", "state" => on_off(dotfiles)),
                                    "dotfiles",
                                    &self.palette,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.settings.show_dotfiles = !this.settings.show_dotfiles;
                                        this.schedule_save(cx);
                                        cx.notify();
                                    }),
                                ),
                            )
                            .into_any_element(),
                    ],
                ),
            )
            .into_any_element()
    }
}

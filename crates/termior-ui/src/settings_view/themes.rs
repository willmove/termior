//! 主题页：外观、配色与背景。

use super::*;

impl SettingsView {
    pub(super) fn preview_theme_preferences(&self, cx: &mut Context<Self>) {
        if let Some(callback) = &self.on_theme_preview {
            callback(&self.settings, cx);
            return;
        }

        let themes = self.themes.all();
        let appearance = match self.settings.appearance {
            Appearance::Light => termior_theme::Appearance::Light,
            Appearance::Dark => termior_theme::Appearance::Dark,
            Appearance::FollowSystem => termior_theme::Appearance::FollowSystem,
        };
        crate::ui::set_palette(
            cx,
            resolve_active_palette(
                &themes,
                appearance,
                &self.settings.theme_id,
                &self.settings.light_theme_id,
                &self.settings.dark_theme_id,
                true,
            ),
        );
    }

    pub(super) fn select_app_theme(&mut self, theme_id: String, cx: &mut Context<Self>) {
        let Some(theme) = self
            .themes
            .all()
            .into_iter()
            .find(|theme| theme.id == theme_id)
        else {
            return;
        };
        self.settings.theme_id = theme.id.clone();
        self.settings.appearance = match theme.native_appearance {
            NativeAppearance::Light => Appearance::Light,
            NativeAppearance::Dark => Appearance::Dark,
        };
        match theme.native_appearance {
            NativeAppearance::Light => self.settings.light_theme_id = theme.id,
            NativeAppearance::Dark => self.settings.dark_theme_id = theme.id,
        }
        self.select_menu = None;
        self.preview_theme_preferences(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn select_pair_theme(
        &mut self,
        slot: NativeAppearance,
        theme_id: String,
        cx: &mut Context<Self>,
    ) {
        match slot {
            NativeAppearance::Light => self.settings.light_theme_id = theme_id,
            NativeAppearance::Dark => self.settings.dark_theme_id = theme_id,
        }
        self.settings.appearance = Appearance::FollowSystem;
        self.select_menu = None;
        self.preview_theme_preferences(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn import_theme(&mut self, cx: &mut Context<Self>) {
        // Blocking dialogs inside GPUI handlers deadlock/panic on Windows
        // (re-entrant dispatch while the `App` RefCell is borrowed), so the
        // async dialog is awaited before touching the view again.
        let task = cx.spawn(async move |view, cx| {
            let Some(handle) = rfd::AsyncFileDialog::new()
                .add_filter(&*t!("settings.themes.import_filter"), &["json"])
                .pick_file()
                .await
            else {
                return;
            };
            let path = handle.path().to_path_buf();
            let _ = view.update(cx, |view, cx| {
                view.status = match std::fs::read_to_string(path)
                    .map_err(|error| error.to_string())
                    .and_then(|json| view.themes.import(&json).map_err(|error| error.to_string()))
                {
                    Ok(theme) => {
                        view.settings.theme_id = theme.id.clone();
                        view.settings.appearance = match theme.native_appearance {
                            NativeAppearance::Light => Appearance::Light,
                            NativeAppearance::Dark => Appearance::Dark,
                        };
                        match theme.native_appearance {
                            NativeAppearance::Light => {
                                view.settings.light_theme_id = theme.id;
                            }
                            NativeAppearance::Dark => {
                                view.settings.dark_theme_id = theme.id;
                            }
                        }
                        view.preview_theme_preferences(cx);
                        view.schedule_save(cx);
                        t!("settings.status.theme_imported").to_string()
                    }
                    Err(error) => {
                        tf!("settings.status.theme_import_failed", "error" => error).to_string()
                    }
                };
                cx.notify();
            });
        });
        self.track_task(task);
    }

    pub(super) fn export_theme(&mut self, cx: &mut Context<Self>) {
        let Some(theme) = self
            .themes
            .all()
            .into_iter()
            .find(|theme| theme.id == self.settings.theme_id)
        else {
            self.status = t!("settings.status.theme_not_found").to_string();
            cx.notify();
            return;
        };
        let task = cx.spawn(async move |view, cx| {
            let Some(handle) = rfd::AsyncFileDialog::new()
                .set_file_name(format!("{}.json", theme.id))
                .save_file()
                .await
            else {
                return;
            };
            let path = handle.path().to_path_buf();
            let status = match ThemeLibrary::export(&theme)
                .map_err(|error| error.to_string())
                .and_then(|json| atomic_write(&path, &json).map_err(|error| error.to_string()))
            {
                Ok(()) => {
                    tf!("settings.status.theme_exported", "path" => path.display()).to_string()
                }
                Err(error) => {
                    tf!("settings.status.theme_export_failed", "error" => error).to_string()
                }
            };
            let _ = view.update(cx, |view, cx| {
                view.status = status;
                cx.notify();
            });
        });
        self.track_task(task);
    }

    pub(super) fn select_background(&mut self, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |view, cx| {
            let Some(handle) = rfd::AsyncFileDialog::new()
                .add_filter(
                    &*t!("settings.themes.image_filter"),
                    &["png", "jpg", "jpeg", "webp", "gif"],
                )
                .pick_file()
                .await
            else {
                return;
            };
            let path = handle.path().to_path_buf();
            let _ = view.update(cx, |view, cx| {
                view.settings.background.image_path = Some(path.to_string_lossy().into_owned());
                view.schedule_save(cx);
                cx.notify();
            });
        });
        self.track_task(task);
    }

    pub(super) fn clear_background(&mut self, cx: &mut Context<Self>) {
        self.settings.background.image_path = None;
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn select_editor_theme(&mut self, theme_id: String, cx: &mut Context<Self>) {
        self.settings.editor_theme_id = theme_id;
        self.select_menu = None;
        self.preview_theme_preferences(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn select_appearance(&mut self, appearance: Appearance, cx: &mut Context<Self>) {
        self.settings.appearance = appearance;
        match appearance {
            Appearance::Light => {
                self.settings.theme_id = self.settings.light_theme_id.clone();
            }
            Appearance::Dark => {
                self.settings.theme_id = self.settings.dark_theme_id.clone();
            }
            Appearance::FollowSystem => {}
        }
        self.select_menu = None;
        self.preview_theme_preferences(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn pair_theme_menu(
        &self,
        slot: NativeAppearance,
        selected_id: &str,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let themes = self.themes.all();
        let candidates = themes_for_native_appearance(&themes, slot);
        div()
            .w(px(360.0))
            .p_1()
            .rounded_md()
            .border_1()
            .border_color(crate::ui::border(&self.palette))
            .bg(crate::ui::color(self.palette.overlay))
            .shadow_md()
            .children(candidates.into_iter().map(|theme| {
                let theme_id = theme.id.clone();
                let selected = theme.id == selected_id;
                Self::select_option(
                    theme.name.clone(),
                    SharedString::from(format!("pair-theme-option-{}-{}", slot.as_str(), theme.id)),
                    selected,
                    &self.palette,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.select_pair_theme(slot, theme_id.clone(), cx);
                    }),
                )
            }))
    }

    pub(super) fn themes_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let background = self
            .settings
            .background
            .image_path
            .as_deref()
            .map(SharedString::from)
            .unwrap_or_else(|| t!("settings.themes.background_none"));
        let app_themes = self.themes.all();
        let follow_system = self.settings.appearance == Appearance::FollowSystem;
        let app_theme_name = app_themes
            .iter()
            .find(|theme| theme.id == self.settings.theme_id)
            .map(|theme| format!("{} · {}", theme.name, theme.native_appearance.as_str()))
            .unwrap_or_else(|| self.settings.theme_id.clone());
        let app_menu_open = self.select_menu == Some(SelectMenu::ApplicationTheme);
        let app_theme_menu = app_menu_open.then(|| {
            div()
                .w(px(360.0))
                .p_1()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(&self.palette))
                .bg(crate::ui::color(self.palette.overlay))
                .shadow_md()
                .children(app_themes.iter().map(|theme| {
                    let theme_id = theme.id.clone();
                    let selected = theme.id == self.settings.theme_id;
                    let label = format!("{} · {}", theme.name, theme.native_appearance.as_str());
                    Self::select_option(
                        label,
                        SharedString::from(format!("app-theme-option-{}", theme.id)),
                        selected,
                        &self.palette,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.select_app_theme(theme_id.clone(), cx);
                        }),
                    )
                }))
        });

        let light_name = app_themes
            .iter()
            .find(|theme| theme.id == self.settings.light_theme_id)
            .map(|theme| theme.name.clone())
            .unwrap_or_else(|| self.settings.light_theme_id.clone());
        let dark_name = app_themes
            .iter()
            .find(|theme| theme.id == self.settings.dark_theme_id)
            .map(|theme| theme.name.clone())
            .unwrap_or_else(|| self.settings.dark_theme_id.clone());
        let light_menu_open = self.select_menu == Some(SelectMenu::LightTheme);
        let dark_menu_open = self.select_menu == Some(SelectMenu::DarkTheme);
        let light_theme_menu = light_menu_open.then(|| {
            self.pair_theme_menu(NativeAppearance::Light, &self.settings.light_theme_id, cx)
        });
        let dark_theme_menu = dark_menu_open.then(|| {
            self.pair_theme_menu(NativeAppearance::Dark, &self.settings.dark_theme_id, cx)
        });

        let editor_themes = termior_editor::builtin_editor_themes();
        let editor_theme_name = editor_themes
            .iter()
            .find(|theme| theme.id == self.settings.editor_theme_id)
            .map(|theme| theme.name.clone())
            .unwrap_or_else(|| self.settings.editor_theme_id.clone());
        let editor_menu_open = self.select_menu == Some(SelectMenu::EditorTheme);
        let editor_theme_menu = editor_menu_open.then(|| {
            div()
                .w(px(360.0))
                .p_1()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(&self.palette))
                .bg(crate::ui::color(self.palette.overlay))
                .shadow_md()
                .children(editor_themes.into_iter().map(|theme| {
                    let theme_id = theme.id.clone();
                    let selected = theme.id == self.settings.editor_theme_id;
                    Self::select_option(
                        theme.name,
                        SharedString::from(format!("editor-theme-option-{}", theme.id)),
                        selected,
                        &self.palette,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.select_editor_theme(theme_id.clone(), cx);
                        }),
                    )
                }))
        });

        let appearance_menu_open = self.select_menu == Some(SelectMenu::Appearance);
        let appearance_label = match self.settings.appearance {
            Appearance::Light => t!("settings.appearance.light"),
            Appearance::Dark => t!("settings.appearance.dark"),
            Appearance::FollowSystem => t!("settings.appearance.follow_system"),
        };
        let appearance_menu = appearance_menu_open.then(|| {
            div()
                .w(px(360.0))
                .p_1()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(&self.palette))
                .bg(crate::ui::color(self.palette.overlay))
                .shadow_md()
                .children(
                    [
                        (Appearance::Light, t!("settings.appearance.light")),
                        (Appearance::Dark, t!("settings.appearance.dark")),
                        (
                            Appearance::FollowSystem,
                            t!("settings.appearance.follow_system"),
                        ),
                    ]
                    .into_iter()
                    .map(|(appearance, label)| {
                        Self::select_option(
                            label,
                            SharedString::from(format!("appearance-option-{appearance:?}")),
                            appearance == self.settings.appearance,
                            &self.palette,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.select_appearance(appearance, cx);
                            }),
                        )
                    }),
                )
        });

        let mut appearance_items: Vec<AnyElement> = vec![
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    Self::select_button(
                        t!("settings.themes.application_theme"),
                        app_theme_name,
                        "app-theme-select",
                        app_menu_open,
                        &self.palette,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_select_menu(SelectMenu::ApplicationTheme, cx);
                        }),
                    ),
                )
                .when_some(app_theme_menu, |select, menu| select.child(menu))
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    Self::select_button(
                        t!("settings.themes.appearance"),
                        appearance_label,
                        "appearance-select",
                        appearance_menu_open,
                        &self.palette,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_select_menu(SelectMenu::Appearance, cx);
                        }),
                    ),
                )
                .when_some(appearance_menu, |select, menu| select.child(menu))
                .into_any_element(),
        ];
        if follow_system {
            appearance_items.push(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(crate::ui::muted(&self.palette))
                            .child(t!("settings.themes.pair_hint")),
                    )
                    .child(
                        Self::select_button(
                            t!("settings.themes.light_theme"),
                            light_name,
                            "light-theme-select",
                            light_menu_open,
                            &self.palette,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_select_menu(SelectMenu::LightTheme, cx);
                            }),
                        ),
                    )
                    .when_some(light_theme_menu, |select, menu| select.child(menu))
                    .child(
                        Self::select_button(
                            t!("settings.themes.dark_theme"),
                            dark_name,
                            "dark-theme-select",
                            dark_menu_open,
                            &self.palette,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_select_menu(SelectMenu::DarkTheme, cx);
                            }),
                        ),
                    )
                    .when_some(dark_theme_menu, |select, menu| select.child(menu))
                    .into_any_element(),
            );
        }
        appearance_items.push(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    Self::select_button(
                        t!("settings.themes.editor_theme"),
                        editor_theme_name,
                        "editor-theme-select",
                        editor_menu_open,
                        &self.palette,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_select_menu(SelectMenu::EditorTheme, cx);
                        }),
                    ),
                )
                .when_some(editor_theme_menu, |select, menu| select.child(menu))
                .into_any_element(),
        );
        appearance_items.push(
            div()
                .flex()
                .gap_2()
                .child(
                    Self::button(t!("settings.themes.import"), "import-theme", &self.palette)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.import_theme(cx)),
                        ),
                )
                .child(
                    Self::button(t!("settings.themes.export"), "export-theme", &self.palette)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.export_theme(cx)),
                        ),
                )
                .into_any_element(),
        );

        div()
            .flex()
            .flex_col()
            .gap_6()
            .child(self.section(
                t!("settings.themes.appearance"),
                t!("settings.themes.appearance_description"),
                appearance_items,
            ))
            .child(
                self.section(
                    t!("settings.themes.background_title"),
                    t!("settings.themes.background_description"),
                    [
                        div()
                            .text_sm()
                            .child(tf!("settings.themes.background_image", "value" => background))
                            .into_any_element(),
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Self::button(
                                    t!("settings.themes.choose_image"),
                                    "background-image",
                                    &self.palette,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.select_background(cx)),
                                ),
                            )
                            .child(
                                Self::button(
                                    t!("settings.themes.clear_image"),
                                    "background-clear",
                                    &self.palette,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.clear_background(cx)),
                                ),
                            )
                            .into_any_element(),
                        self.edit_row(
                            t!("settings.themes.background_opacity"),
                            t!("settings.themes.background_opacity_description"),
                            EditField::BackgroundOpacity,
                            cx,
                        ),
                        self.edit_row(
                            t!("settings.themes.background_blur"),
                            t!("settings.themes.background_blur_description"),
                            EditField::BackgroundBlur,
                            cx,
                        ),
                    ],
                ),
            )
            .into_any_element()
    }
}

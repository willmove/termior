//! 模型页：服务提供方、凭据与模型列表。

use super::*;

impl SettingsView {
    pub(super) fn profile(&self) -> Option<&termior_store::ModelProviderSettings> {
        self.settings.models.profiles.get(self.profile_index)
    }

    pub(super) fn profile_mut(&mut self) -> Option<&mut termior_store::ModelProviderSettings> {
        self.settings.models.profiles.get_mut(self.profile_index)
    }

    pub(super) fn profile_key(&self) -> Option<String> {
        self.profile()
            .map(|profile| format!("provider:{}", profile.id))
    }

    pub(super) fn refresh_credential_state(&mut self) {
        self.credential_present = self
            .profile_key()
            .and_then(|key| KeyringSecretStore::new().get(&key).ok().flatten())
            .is_some();
    }

    pub(super) fn save_api_key(&mut self, cx: &mut Context<Self>) {
        self.commit_edit();
        let Some(value) = self.pending_api_key.take() else {
            self.status = if self.credential_present {
                t!("settings.status.api_key_already_stored").to_string()
            } else {
                t!("settings.status.enter_api_key").to_string()
            };
            cx.notify();
            return;
        };
        let key = self.profile_key().unwrap_or_default();
        match KeyringSecretStore::new().set(&key, &value) {
            Ok(()) => {
                self.credential_present = true;
                self.status = t!("settings.status.api_key_saved").to_string();
            }
            Err(error) => {
                self.pending_api_key = Some(value);
                self.status =
                    tf!("settings.status.api_key_save_failed", "error" => error).to_string();
            }
        }
        cx.notify();
    }

    pub(super) fn select_profile(&mut self, index: usize, cx: &mut Context<Self>) {
        self.commit_edit();
        self.schedule_save_if_dirty(cx);
        if index >= self.settings.models.profiles.len() {
            return;
        }
        if let (Some(key), Some(draft)) = (self.profile_key(), self.pending_api_key.take()) {
            self.credential_drafts.insert(key, draft);
        }
        self.profile_index = index;
        self.pending_api_key = self
            .profile_key()
            .and_then(|key| self.credential_drafts.remove(&key));
        self.select_menu = None;
        self.refresh_credential_state();
        self.status.clear();
        cx.notify();
    }

    pub(super) fn toggle_provider(&mut self, cx: &mut Context<Self>) {
        if let Some(profile) = self.profile_mut() {
            profile.enabled = !profile.enabled;
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// 设为默认聊天模型（Agent 面板使用）。Composer 只会选用已启用的 profile，
    /// 因此设默认时自动启用，避免「设了默认却仍不可用」的静默失效。
    pub(super) fn make_active_chat(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.profile().map(|p| p.id.clone()) else {
            return;
        };
        if let Some(profile) = self.profile_mut() {
            profile.enabled = true;
        }
        self.settings.models.active_chat_profile = Some(id);
        self.schedule_save(cx);
        cx.notify();
    }

    /// 设为 Agent 默认模型：成为该服务提供方的默认模型，并让 Agent 使用该服务提供方。
    /// 与「用于聊天」同理，设默认时自动启用。
    pub(super) fn set_agent_default_model(&mut self, model: String, cx: &mut Context<Self>) {
        self.commit_edit();
        let Some(profile) = self.profile_mut() else {
            return;
        };
        profile.models = profile.model_choices();
        profile.model = model.clone();
        profile.enabled = true;
        let (id, name) = (profile.id.clone(), profile.display_name.clone());
        self.settings.models.active_chat_profile = Some(id);
        self.status = tf!(
            "settings.status.agent_default_set",
            "provider" => name,
            "model" => model
        )
        .to_string();
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn remove_model(&mut self, model: &str, cx: &mut Context<Self>) {
        self.commit_edit();
        if self
            .profile_mut()
            .is_some_and(|profile| profile.remove_model(model))
        {
            self.schedule_save(cx);
        }
        cx.notify();
    }

    pub(super) fn merge_recommended_models(&mut self, cx: &mut Context<Self>) {
        self.commit_edit();
        if let Some(profile) = self.profile_mut() {
            profile.merge_recommended_models();
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// 当前 profile 的模型列表：标注默认，提供设为 Agent 默认 / 移除操作。
    pub(super) fn model_list(&self, active_chat: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.clone();
        let Some(profile) = self.profile() else {
            return div().into_any_element();
        };
        let choices = profile.model_choices();
        let removable = choices.len() > 1;
        let has_recommended = !termior_store::recommended_models(&profile.provider).is_empty();
        let tag = |label: SharedString, color| {
            div()
                .px_2()
                .py(px(1.0))
                .rounded_md()
                .flex_none()
                .text_xs()
                .bg(crate::ui::alpha(color, 0.14))
                .child(label)
        };
        let small_button = |id: SharedString, label: SharedString| {
            crate::ui::button(id, label, crate::ui::ButtonKind::Subtle, &p)
                .flex_none()
                .px_2()
                .py_1()
                .text_xs()
        };
        let rows = choices.into_iter().enumerate().map(|(index, model)| {
            let is_default = model == profile.model;
            let agent_default = active_chat && is_default;
            let default_target = model.clone();
            let remove_target = model.clone();
            div()
                .id(SharedString::from(format!("model-row-{index}")))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(crate::ui::alpha(p.foreground, 0.04))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .text_sm()
                        .child(SharedString::from(model)),
                )
                .when(agent_default, |row| {
                    row.child(tag(t!("settings.models.tag_agent_default"), p.accent))
                })
                .when(is_default && !agent_default, |row| {
                    row.child(tag(
                        t!("settings.models.tag_provider_default"),
                        p.foreground,
                    ))
                })
                .when(!agent_default, |row| {
                    row.child(
                        small_button(
                            SharedString::from(format!("model-set-default-{index}")),
                            t!("settings.models.set_agent_default"),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.set_agent_default_model(default_target.clone(), cx);
                            }),
                        ),
                    )
                })
                .when(removable, |row| {
                    row.child(
                        small_button(
                            SharedString::from(format!("model-remove-{index}")),
                            t!("settings.models.remove_model"),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.remove_model(&remove_target, cx);
                            }),
                        ),
                    )
                })
        });
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_sm().child(t!("settings.models.list_label")))
            .child(
                div()
                    .text_xs()
                    .text_color(crate::ui::muted(&p))
                    .child(t!("settings.models.list_description")),
            )
            .children(rows)
            .when(has_recommended, |list| {
                list.child(
                    div().flex().pt_1().child(
                        Self::button(
                            t!("settings.models.restore_recommended"),
                            "merge-recommended-models",
                            &p,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.merge_recommended_models(cx);
                            }),
                        ),
                    ),
                )
            })
            .into_any_element()
    }

    /// 设为默认补全模型（内联补全使用）。与聊天默认同理，设默认时自动启用。
    pub(super) fn make_active_completion(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.profile().map(|p| p.id.clone()) else {
            return;
        };
        if let Some(profile) = self.profile_mut() {
            profile.enabled = true;
        }
        self.settings.models.active_completion_profile = Some(id);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn ping_provider(&mut self, cx: &mut Context<Self>) {
        self.commit_edit();
        self.schedule_save_if_dirty(cx);
        if self.pending_api_key.is_some() {
            self.status = t!("settings.status.save_api_key_first").to_string();
            cx.notify();
            return;
        }
        if self.connection_check.is_some() {
            return;
        }
        let Some(profile) = self.profile().cloned() else {
            return;
        };
        let api_key = self
            .profile_key()
            .and_then(|key| KeyringSecretStore::new().get(&key).ok().flatten());
        let profile_id = profile.id.clone();
        self.connection_check = Some(profile_id.clone());
        self.status = t!("settings.status.checking_provider").to_string();
        let task = cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    ProviderConfig::from_settings(&profile, api_key)
                        .and_then(HttpProvider::new)
                        .and_then(|provider| provider.ping())
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.connection_check = None;
                if view.profile().map(|p| &p.id) != Some(&profile_id) {
                    cx.notify();
                    return;
                }
                view.status = match result {
                    Ok(duration) => {
                        tf!("settings.status.reachable_ms", "ms" => duration.as_millis())
                            .to_string()
                    }
                    Err(error) => {
                        tf!("settings.status.provider_check_failed", "error" => error).to_string()
                    }
                };
                cx.notify();
            });
        });
        self.track_task(task);
        cx.notify();
    }

    pub(super) fn models_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(profile) = self.profile() else {
            return div()
                .child(t!("empty.no_provider_profiles"))
                .into_any_element();
        };
        let active_chat = self.settings.models.active_chat_profile.as_deref() == Some(&profile.id);
        let active_completion =
            self.settings.models.active_completion_profile.as_deref() == Some(&profile.id);
        // 下拉菜单里直接标注每个 profile 的状态，不必逐个翻看就能找到
        // Agent 面板当前用的是哪一个。
        let profile_menu_open = self.select_menu == Some(SelectMenu::ProviderProfile);
        let profile_menu =
            profile_menu_open.then(|| {
                div()
                    .w(px(360.0))
                    .p_1()
                    .rounded_md()
                    .border_1()
                    .border_color(crate::ui::border(&self.palette))
                    .bg(crate::ui::color(self.palette.overlay))
                    .shadow_md()
                    .children(self.settings.models.profiles.iter().enumerate().map(
                        |(index, entry)| {
                            let mut tags = Vec::new();
                            if self.settings.models.active_chat_profile.as_deref()
                                == Some(entry.id.as_str())
                            {
                                tags.push(t!("settings.models.tag_chat").to_string());
                            }
                            if self.settings.models.active_completion_profile.as_deref()
                                == Some(entry.id.as_str())
                            {
                                tags.push(t!("settings.models.tag_completion").to_string());
                            }
                            if !entry.enabled {
                                tags.push(t!("settings.models.tag_disabled").to_string());
                            }
                            let label = if tags.is_empty() {
                                format!("{} · {}", entry.display_name, entry.model)
                            } else {
                                format!(
                                    "{} · {} · {}",
                                    entry.display_name,
                                    entry.model,
                                    tags.join(" · ")
                                )
                            };
                            Self::select_option(
                                SharedString::from(label),
                                SharedString::from(format!("provider-option-{}", entry.id)),
                                index == self.profile_index,
                                &self.palette,
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.select_profile(index, cx);
                                }),
                            )
                        },
                    ))
            });
        let chat_profile_label = self
            .settings
            .models
            .active_chat_profile
            .as_deref()
            .and_then(|id| self.settings.models.profiles.iter().find(|p| p.id == id))
            .map(|p| {
                // Composer 只选启用的 profile；标注禁用态，提示为何 Agent 面板没生效。
                if p.enabled {
                    format!("{} · {}", p.display_name, p.model)
                } else {
                    format!(
                        "{} · {} ({})",
                        p.display_name,
                        p.model,
                        t!("settings.models.tag_disabled")
                    )
                }
            })
            .unwrap_or_else(|| t!("settings.models.not_set_choose").to_string());
        let completion_profile_label = self
            .settings
            .models
            .active_completion_profile
            .as_deref()
            .and_then(|id| self.settings.models.profiles.iter().find(|p| p.id == id))
            .map(|p| format!("{} · {}", p.display_name, p.model))
            .unwrap_or_else(|| t!("settings.models.not_set").to_string());
        div()
            .flex()
            .flex_col()
            .gap_6()
            .child(
                self.section(
                    t!("settings.models.step1_title"),
                    t!("settings.models.step1_description"),
                    [div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            Self::select_button(
                                t!("settings.models.provider_label"),
                                tf!(
                                    "settings.models.provider_value",
                                    "name" => profile.display_name,
                                    "index" => self.profile_index + 1,
                                    "total" => self.settings.models.profiles.len()
                                ),
                                "provider-select",
                                profile_menu_open,
                                &self.palette,
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.toggle_select_menu(SelectMenu::ProviderProfile, cx);
                                }),
                            ),
                        )
                        .when_some(profile_menu, |select, menu| select.child(menu))
                        .into_any_element()],
                ),
            )
            .child(self.section(
                t!("settings.models.step2_title"),
                t!("settings.models.step2_description"),
                [
                    self.model_list(active_chat, cx),
                    self.edit_row(
                        t!("settings.models.add_label"),
                        t!("settings.models.add_description"),
                        EditField::AddModel,
                        cx,
                    ),
                    self.edit_row(
                        t!("settings.models.base_url_label"),
                        t!("settings.models.base_url_description"),
                        EditField::BaseUrl,
                        cx,
                    ),
                ],
            ))
            .child(
                self.section(
                    t!("settings.models.step3_title"),
                    if profile.local {
                        t!("settings.models.step3_description_local")
                    } else {
                        t!("settings.models.step3_description_remote")
                    },
                    [
                        self.edit_row(
                            t!("settings.models.api_key_label"),
                            t!("settings.models.api_key_description"),
                            EditField::ApiKey,
                            cx,
                        ),
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                Self::button(
                                    t!("settings.models.save_api_key"),
                                    "save-api-key",
                                    &self.palette,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.save_api_key(cx)),
                                ),
                            )
                            .child(
                                Self::button(
                                    if self.connection_check.is_some() {
                                        t!("settings.models.testing")
                                    } else {
                                        t!("settings.models.test_connection")
                                    },
                                    "ping-provider",
                                    &self.palette,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.ping_provider(cx)),
                                ),
                            )
                            .into_any_element(),
                    ],
                ),
            )
            .child(
                self.section(
                    t!("settings.models.step4_title"),
                    t!("settings.models.step4_description"),
                    [
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_sm().child(tf!(
                                "settings.models.chat_line",
                                "value" => chat_profile_label
                            )))
                            .child(div().text_sm().child(tf!(
                                "settings.models.completion_line",
                                "value" => completion_profile_label
                            )))
                            .into_any_element(),
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                Self::button(
                                    tf!(
                                        "settings.models.enabled",
                                        "state" => on_off(profile.enabled)
                                    ),
                                    "provider-enabled",
                                    &self.palette,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.toggle_provider(cx)),
                                ),
                            )
                            .child(
                                Self::button(
                                    if active_chat {
                                        t!("settings.models.use_for_chat_active")
                                    } else {
                                        t!("settings.models.use_for_chat")
                                    },
                                    "active-chat",
                                    &self.palette,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.make_active_chat(cx)),
                                ),
                            )
                            .child(
                                Self::button(
                                    if active_completion {
                                        t!("settings.models.use_for_completion_active")
                                    } else {
                                        t!("settings.models.use_for_completion")
                                    },
                                    "active-completion",
                                    &self.palette,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.make_active_completion(cx)),
                                ),
                            )
                            .into_any_element(),
                    ],
                ),
            )
            .into_any_element()
    }
}

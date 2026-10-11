//! 会话级模型选择：在 Agent 面板为当前会话选定任一已启用服务商的模型，
//! 不改动设置里的 Agent 默认模型；选择随会话持久化。

use super::*;
use termior_theme::ResolvedPalette;

impl ComposerView {
    pub(super) fn toggle_model_menu(&mut self, cx: &mut Context<Self>) {
        self.model_menu_open = !self.model_menu_open;
        if self.model_menu_open {
            // 钥匙串探测只在打开菜单时做一次，避免每帧访问系统凭据库。
            let secrets = KeyringSecretStore::new();
            self.model_menu_ready = self
                .model_settings
                .profiles
                .iter()
                .map(|profile| {
                    profile.enabled
                        && (profile.local
                            || secrets
                                .get(&format!("provider:{}", profile.id))
                                .ok()
                                .flatten()
                                .is_some())
                })
                .collect();
            self.mode_menu_open = false;
            self.agent_menu_open = false;
        }
        cx.notify();
    }

    /// 为当前会话选定模型；`None` 回到跟随 Agent 默认模型。
    pub(super) fn choose_model(&mut self, choice: Option<SessionModel>, cx: &mut Context<Self>) {
        self.model_menu_open = false;
        self.model_choice = choice;
        self.persist_session();
        self.rebuild_runtime(cx);
    }

    /// 模型 chip 文案：当前会话实际使用的模型 id。
    pub(super) fn model_chip_label(&self) -> SharedString {
        self.effective_profile()
            .map(|profile| SharedString::from(profile.model))
            .unwrap_or_else(|| t!("composer.model_menu.none"))
    }

    pub(super) fn model_menu(
        &self,
        p: &ResolvedPalette,
        bottom: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let default_label = self
            .model_settings
            .active_chat()
            .map(|profile| format!("{} · {}", profile.display_name, profile.model))
            .unwrap_or_else(|| t!("composer.model_menu.default_unset").to_string());
        let follow_default = termior_ui_kit::menu_item(p, self.model_choice.is_none(), true)
            .id("composer-model-default")
            .flex()
            .flex_col()
            .gap(px(1.0))
            .text_xs()
            .child(t!("composer.model_menu.follow_default"))
            .child(
                div()
                    .text_color(crate::ui::muted(p))
                    .child(SharedString::from(default_label)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.choose_model(None, cx);
                }),
            )
            .into_any_element();
        let mut rows = vec![follow_default];
        let enabled = self
            .model_settings
            .profiles
            .iter()
            .enumerate()
            .filter(|(_, profile)| profile.enabled)
            .collect::<Vec<_>>();
        if enabled.is_empty() {
            rows.push(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(crate::ui::muted(p))
                    .child(t!("composer.model_menu.empty"))
                    .into_any_element(),
            );
        }
        for (profile_index, profile) in enabled {
            let ready = self
                .model_menu_ready
                .get(profile_index)
                .copied()
                .unwrap_or(false);
            rows.push(
                div()
                    .px_2()
                    .pt_1()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(crate::ui::muted(p))
                    .child(if ready {
                        SharedString::from(profile.display_name.clone())
                    } else {
                        tf!(
                            "composer.model_menu.needs_key",
                            "provider" => profile.display_name
                        )
                    })
                    .into_any_element(),
            );
            for (model_index, model) in profile.model_choices().into_iter().enumerate() {
                let choice = SessionModel {
                    profile_id: profile.id.clone(),
                    model: model.clone(),
                };
                let selected = self.model_choice.as_ref() == Some(&choice);
                rows.push(
                    termior_ui_kit::menu_item(p, selected, ready)
                        .id(SharedString::from(format!(
                            "composer-model-{profile_index}-{model_index}"
                        )))
                        .text_xs()
                        .child(SharedString::from(model))
                        .when(ready, |item| {
                            item.on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.choose_model(Some(choice.clone()), cx);
                                }),
                            )
                        })
                        .into_any_element(),
                );
            }
        }
        menu_panel(p)
            .id("composer-model-menu")
            .absolute()
            .left(px(MODE_MENU_LEFT))
            .bottom(px(bottom))
            .min_w(px(260.0))
            .max_h(px(360.0))
            .overflow_y_scroll()
            .child(div().flex().flex_col().gap(px(2.0)).children(rows))
    }
}

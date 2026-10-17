//! 关于页：版本、迁移状态与更新控件。

use super::*;

impl SettingsView {
    pub(super) fn about_page(&self, cx: &mut Context<Self>) -> AnyElement {
        self.section(
            t!("settings.page.about"),
            t!("settings.about.description"),
            [
                div()
                    .text_sm()
                    .child(tf!(
                        "settings.about.version",
                        "version" => env!("CARGO_PKG_VERSION")
                    ))
                    .into_any_element(),
                self.update_controls(cx),
                div()
                    .text_sm()
                    .text_color(crate::ui::muted(&self.palette))
                    .child(t!("settings.about.principles"))
                    .into_any_element(),
                div()
                    .text_xs()
                    .text_color(crate::ui::muted(&self.palette))
                    .child(tf!(
                        "settings.about.migration_status",
                        "value" => self
                            .migration_error
                            .as_deref()
                            .map(SharedString::from)
                            .unwrap_or_else(|| t!("settings.about.migration_ok"))
                    ))
                    .into_any_element(),
            ],
        )
    }

    pub(super) fn update_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let updater = crate::updater::entity(cx);
        let state = updater.read(cx);
        let ready = state.ready.is_some();
        let busy = state.busy;
        let opened = state.installer_opened;
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                Self::button(
                    tf!(
                        "settings.about.automatic_updates",
                        "state" => on_off(self.settings.automatic_updates)
                    ),
                    "automatic-updates",
                    &self.palette,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.settings.automatic_updates = !this.settings.automatic_updates;
                        crate::updater::set_enabled(this.settings.automatic_updates, cx);
                        this.schedule_save(cx);
                        cx.notify();
                    }),
                ),
            )
            .child(div().text_sm().child(state.status.clone()))
            .when_some(state.progress.as_ref(), |view, progress| {
                let mut detail = tf!(
                    "updater.progress",
                    "downloaded" => crate::updater::format_bytes(progress.downloaded),
                    "total" => crate::updater::format_bytes(progress.total),
                    "percent" => (progress.fraction() * 100.0).floor() as u32
                )
                .to_string();
                if let (Some(speed), Some(eta)) = (progress.speed(), progress.eta()) {
                    detail.push_str(" · ");
                    detail.push_str(&tf!(
                        "updater.progress_eta",
                        "speed" => crate::updater::format_bytes(speed as u64),
                        "eta" => crate::updater::format_eta(eta)
                    ));
                }
                view.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .h(px(6.0))
                                .w_full()
                                .rounded_full()
                                .bg(crate::ui::border(&self.palette))
                                .child(
                                    div()
                                        .h_full()
                                        .w(gpui::relative(progress.fraction()))
                                        .rounded_full()
                                        .bg(crate::ui::color(self.palette.accent)),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(crate::ui::muted(&self.palette))
                                .child(detail),
                        ),
                )
            })
            .child(
                div()
                    .text_xs()
                    .text_color(crate::ui::muted(&self.palette))
                    .child(t!("settings.about.updates_description")),
            )
            .when(!busy && !opened, |view| {
                view.child(
                    Self::button(
                        if ready {
                            t!("settings.about.install_update")
                        } else {
                            t!("settings.about.check_updates")
                        },
                        "check-update",
                        &self.palette,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _, _, cx| {
                            crate::updater::entity(cx).update(cx, |updater, cx| {
                                if ready {
                                    updater.install(cx);
                                } else {
                                    updater.check(cx);
                                }
                            });
                        }),
                    ),
                )
            })
            .child(
                Self::button(
                    t!("settings.about.release_notes"),
                    "release-downloads",
                    &self.palette,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.pending_tasks.push(cx.spawn(async move |this, cx| {
                            let result = cx
                                .background_executor()
                                .spawn(async {
                                    termior_platform::open_external(
                                        termior_platform::update::RELEASES_URL,
                                    )
                                })
                                .await;
                            if let Err(error) = result {
                                let _ = this.update(cx, |this, cx| {
                                    this.status = error.to_string();
                                    cx.notify();
                                });
                            }
                        }));
                    }),
                ),
            )
            .into_any_element()
    }
}

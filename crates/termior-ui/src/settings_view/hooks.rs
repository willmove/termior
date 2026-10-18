//! Lifecycle hook management: configured commands and recent redacted runs.

use super::*;
use termior_hooks::{hook_list_entries, recent_run_entries, RECENT_RUN_LIMIT};

impl SettingsView {
    pub(super) fn reload_lifecycle_hooks(&mut self) {
        let Some(dir) = self.data_dir.clone() else {
            self.lifecycle_hooks = termior_hooks::HookCatalog::default();
            self.hook_runs.clear();
            self.hook_load_error = None;
            return;
        };
        match termior_hooks::HookCatalog::load(&termior_hooks::lifecycle_hooks_path(&dir)) {
            Ok(catalog) => {
                self.lifecycle_hooks = catalog;
                self.hook_load_error = None;
            }
            Err(error) => self.hook_load_error = Some(error),
        }
        self.hook_runs =
            termior_hooks::load_run_log(&termior_hooks::hook_runs_path(&dir)).unwrap_or_default();
    }

    pub(super) fn toggle_lifecycle_hook(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(index) = self
            .lifecycle_hooks
            .hooks
            .iter()
            .position(|hook| hook.id == id)
        else {
            return;
        };
        let previous = self.lifecycle_hooks.hooks[index].enabled;
        self.lifecycle_hooks.hooks[index].enabled = !previous;
        let Some(dir) = self.data_dir.clone() else {
            self.lifecycle_hooks.hooks[index].enabled = previous;
            self.status = t!("settings.hooks.storage_unavailable").to_string();
            cx.notify();
            return;
        };
        if let Err(error) = self
            .lifecycle_hooks
            .save(&termior_hooks::lifecycle_hooks_path(&dir))
        {
            self.lifecycle_hooks.hooks[index].enabled = previous;
            self.status = tf!("settings.hooks.save_failed", "error" => error).to_string();
        } else {
            self.status.clear();
        }
        cx.notify();
    }

    pub(super) fn hooks_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = self.palette.clone();
        let load_error = self.hook_load_error.clone();
        let hooks = hook_list_entries(&self.lifecycle_hooks.hooks, &[]);
        let runs = recent_run_entries(&self.hook_runs, RECENT_RUN_LIMIT, &[]);
        let hook_rows = hooks.iter().enumerate().map(|(index, hook)| {
            let summary = tf!(
                "settings.hooks.row",
                "id" => hook.id.clone(),
                "source" => hook.source.clone(),
                "point" => hook.point.clone(),
                "command" => hook.command.clone(),
                "timeout" => hook.timeout_ms
            );
            let state = if hook.enabled {
                t!("settings.hooks.enabled")
            } else {
                t!("settings.hooks.disabled")
            };
            let action = if hook.enabled {
                t!("settings.hooks.disable")
            } else {
                t!("settings.hooks.enable")
            };
            let hook_id = hook.id.clone();
            let painted = summary.to_string();
            div()
                .id(SharedString::from(format!("lifecycle-hook-{index}")))
                .debug_selector({
                    let painted = painted.clone();
                    move || painted.clone()
                })
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .bg(crate::ui::alpha(palette.foreground, 0.04))
                .aria_label(summary.clone())
                .child(div().text_sm().child(summary))
                .child(
                    div()
                        .text_xs()
                        .text_color(crate::ui::muted(&palette))
                        .child(state),
                )
                .child(
                    Self::button(
                        action,
                        SharedString::from(format!("lifecycle-hook-toggle-{index}")),
                        &palette,
                    )
                    .debug_selector(move || format!("lifecycle-hook-toggle-{index}"))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_lifecycle_hook(&hook_id, cx);
                        }),
                    ),
                )
                .into_any_element()
        });
        let run_rows = runs.iter().enumerate().map(|(index, run)| {
            let modified = if run.modified_arguments {
                t!("settings.hooks.modified_yes").to_string()
            } else {
                t!("settings.hooks.modified_no").to_string()
            };
            let summary = tf!(
                "settings.hooks.run",
                "point" => run.point.clone(),
                "duration" => run.duration_ms,
                "result" => run.result.clone(),
                "modified" => modified
            );
            let painted = summary.to_string();
            div()
                .id(SharedString::from(format!("lifecycle-hook-run-{index}")))
                .debug_selector({
                    let painted = painted.clone();
                    move || painted.clone()
                })
                .text_sm()
                .aria_label(summary.clone())
                .child(summary)
                .into_any_element()
        });
        div()
            .flex()
            .flex_col()
            .gap_6()
            .when_some(load_error, |page, error| {
                page.child(
                    div()
                        .text_xs()
                        .text_color(crate::ui::color(palette.status[3]))
                        .child(tf!("settings.hooks.load_failed", "error" => error)),
                )
            })
            .child(
                self.section(
                    t!("settings.hooks.title"),
                    t!("settings.hooks.description"),
                    [div()
                        .id("lifecycle-hook-list")
                        .flex()
                        .flex_col()
                        .gap_2()
                        .when(hooks.is_empty(), |list| {
                            list.child(
                                div()
                                    .id("lifecycle-hooks-empty")
                                    .debug_selector(|| "lifecycle-hooks-empty".into())
                                    .text_sm()
                                    .text_color(crate::ui::muted(&palette))
                                    .child(t!("settings.hooks.empty")),
                            )
                        })
                        .children(hook_rows)
                        .into_any_element()],
                ),
            )
            .child(
                self.section(
                    t!("settings.hooks.runs_title"),
                    t!("settings.hooks.runs_description"),
                    [div()
                        .id("lifecycle-hook-runs")
                        .flex()
                        .flex_col()
                        .gap_2()
                        .when(runs.is_empty(), |list| {
                            list.child(
                                div()
                                    .id("lifecycle-hook-runs-empty")
                                    .debug_selector(|| "lifecycle-hook-runs-empty".into())
                                    .text_sm()
                                    .text_color(crate::ui::muted(&palette))
                                    .child(t!("settings.hooks.runs_empty")),
                            )
                        })
                        .children(run_rows)
                        .into_any_element()],
                ),
            )
            .into_any_element()
    }
}

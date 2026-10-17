//! 快捷键页。

use super::*;

impl SettingsView {
    pub(super) fn capture_shortcut(
        &mut self,
        action: KeyAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_edit();
        self.capture_shortcut = Some(action);
        self.status = tf!(
            "settings.status.shortcut_capture",
            "action" => format!("{action:?}")
        )
        .to_string();
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    pub(super) fn shortcuts_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = default_keymap().into_iter().map(|entry| {
            let action = entry.action;
            let current = self.settings.keymap.bindings.get(&entry.action);
            let label = current
                .map(|binding| {
                    let mut parts = Vec::new();
                    if binding.primary {
                        parts.push(if cfg!(target_os = "macos") {
                            "Cmd"
                        } else {
                            "Ctrl"
                        });
                    }
                    if binding.shift {
                        parts.push("Shift");
                    }
                    if binding.alt {
                        parts.push("Alt");
                    }
                    parts.push(&binding.key);
                    parts.join("+")
                })
                .unwrap_or_else(|| entry.binding.display(Platform::current()));
            div()
                .id(SharedString::from(format!("shortcut-{action:?}")))
                .flex()
                .justify_between()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(self.capture_shortcut == Some(action), |row| {
                    row.bg(crate::ui::selected_wash(&self.palette))
                })
                .child(SharedString::from(format!("{:?}", entry.action)))
                .child(SharedString::from(label))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        this.capture_shortcut(action, window, cx)
                    }),
                )
        });
        self.section(
            t!("settings.shortcuts.title"),
            t!("settings.shortcuts.description"),
            [div().flex().flex_col().children(rows).into_any_element()],
        )
    }
}

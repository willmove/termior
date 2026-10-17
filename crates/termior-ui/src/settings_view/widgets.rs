//! 设置页共用的区块、按钮与下拉。

use super::*;

impl SettingsView {
    pub(super) fn toggle_select_menu(&mut self, menu: SelectMenu, cx: &mut Context<Self>) {
        self.select_menu = (self.select_menu != Some(menu)).then_some(menu);
        cx.notify();
    }

    pub(super) fn section(
        &self,
        title: impl Into<SharedString>,
        description: impl Into<SharedString>,
        children: impl IntoIterator<Item = AnyElement>,
    ) -> AnyElement {
        let p = &self.palette;
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_lg().child(title.into()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(crate::ui::muted(p))
                            .child(description.into()),
                    ),
            )
            .children(children)
            .into_any_element()
    }

    pub(super) fn button(
        label: impl Into<SharedString>,
        id: impl Into<gpui::ElementId>,
        p: &termior_theme::ResolvedPalette,
    ) -> gpui::Stateful<gpui::Div> {
        crate::ui::button(id, label, crate::ui::ButtonKind::Subtle, p)
            .px_3()
            .py_2()
            .text_sm()
    }

    pub(super) fn select_button(
        label: impl Into<SharedString>,
        value: impl Into<SharedString>,
        id: impl Into<gpui::ElementId>,
        open: bool,
        p: &termior_theme::ResolvedPalette,
    ) -> gpui::Stateful<gpui::Div> {
        let label = label.into();
        div()
            .id(id)
            .w(px(360.0))
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(if open {
                crate::ui::color(p.accent)
            } else {
                crate::ui::border(p)
            })
            .bg(crate::ui::color(p.elevated))
            .text_sm()
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_color(crate::ui::muted(p)).child(label))
                    .child(value.into()),
            )
            .child(if open { "▴" } else { "▾" })
            .hover({
                let wash = crate::ui::hover_wash(p);
                move |style| style.bg(wash)
            })
    }

    pub(super) fn select_option(
        label: impl Into<SharedString>,
        id: impl Into<gpui::ElementId>,
        selected: bool,
        p: &termior_theme::ResolvedPalette,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .w_full()
            .px_3()
            .py_2()
            .rounded_sm()
            .text_sm()
            .cursor_pointer()
            .flex()
            .items_center()
            .gap_2()
            .when(selected, |option| option.bg(crate::ui::selected_wash(p)))
            .when(!selected, |option| {
                let wash = crate::ui::hover_wash(p);
                option.hover(move |style| style.bg(wash))
            })
            .child(if selected { "✓" } else { " " })
            .child(label.into())
    }
}

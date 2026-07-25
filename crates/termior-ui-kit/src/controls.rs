//! 共享控件。全部消费 [`crate::theme`] 的颜色与 [`crate::tokens`] 的尺寸。

use crate::{
    icon::{icon, Icon},
    theme::{active_wash, alpha, color, hover_shade, hover_wash, on_color},
    tokens::{font_size, height, icon_size, radius, space},
};
use gpui::{div, prelude::*, px, AnyView, App, Context, Div, SharedString, Stateful, Window};
use termior_theme::ResolvedPalette;

/// 统一按钮语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// 强调操作（发送、应用）：accent 实底。
    Primary,
    /// 常规操作：浅底 + 边框。
    Subtle,
    /// 无底色，悬浮才显示背景（工具栏图标类）。
    Ghost,
    /// 成功/确认语义（status[1]）。
    Success,
    /// 危险操作（status[3]）。
    Danger,
}

/// 主题化按钮基础件：统一圆角、内边距、悬浮态与文字对比度。
/// 调用方追加 `.on_mouse_down(...)`、尺寸覆盖（如 `.text_sm()`）即可。
pub fn button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    p: &ResolvedPalette,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .px(px(space::MD))
        .py(px(space::XS))
        .rounded(px(radius::MD))
        .text_size(px(font_size::BODY))
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(space::XS))
        .whitespace_nowrap()
        .child(label.into());
    match kind {
        ButtonKind::Primary => {
            let bg = p.accent;
            base.bg(color(bg))
                .text_color(on_color(bg))
                .hover(move |style| style.bg(color(hover_shade(bg))))
        }
        ButtonKind::Success => {
            let bg = p.status[1];
            base.bg(color(bg))
                .text_color(on_color(bg))
                .hover(move |style| style.bg(color(hover_shade(bg))))
        }
        ButtonKind::Danger => {
            let bg = p.status[3];
            base.bg(color(bg))
                .text_color(on_color(bg))
                .hover(move |style| style.bg(color(hover_shade(bg))))
        }
        ButtonKind::Subtle => {
            let hover_bg = hover_wash(p);
            base.bg(color(p.elevated))
                .border_1()
                .border_color(crate::theme::border(p))
                .text_color(color(p.foreground))
                .hover(move |style| style.bg(hover_bg))
        }
        ButtonKind::Ghost => {
            let hover_bg = hover_wash(p);
            base.text_color(alpha(p.foreground, 0.85))
                .hover(move |style| style.bg(hover_bg))
        }
    }
}

/// 只有图标、没有文字的按钮。
///
/// 因为没有可见标签，`label` 是必填的：它同时作为无障碍名称与悬停提示，
/// 所以一个图标按钮不可能既没有文字也没有说明。
///
/// `id` 同时充当 hover 分组名 —— SVG 元素不继承父级的 `text_color`，图标要跟着
/// 按钮一起变色只能靠 `group_hover`。
pub fn icon_button(
    id: &'static str,
    glyph: Icon,
    label: impl Into<SharedString>,
    p: &ResolvedPalette,
) -> Stateful<Div> {
    let label = label.into();
    let tooltip_label = label.clone();
    let hover_bg = hover_wash(p);
    let active_bg = active_wash(p);
    let resting = alpha(p.foreground, 0.72);
    let engaged = color(p.foreground);
    let palette = p.clone();
    div()
        .id(id)
        .group(id)
        .aria_label(label)
        .tooltip(move |_window, cx| Tooltip::view(tooltip_label.clone(), &palette, cx))
        .size(px(height::REGULAR))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(radius::MD))
        .cursor_pointer()
        .hover(move |style| style.bg(hover_bg))
        .active(move |style| style.bg(active_bg))
        .child(
            icon(glyph, icon_size::SM, resting)
                .group_hover(id, move |style| style.text_color(engaged)),
        )
}

/// 悬停提示。只承载一句话；需要更多内容说明该用别的呈现方式。
pub struct Tooltip {
    label: SharedString,
    palette: ResolvedPalette,
}

impl Tooltip {
    /// 供 `.tooltip(move |_, cx| Tooltip::view(..))` 使用。
    pub fn view(
        label: impl Into<SharedString>,
        palette: &ResolvedPalette,
        cx: &mut App,
    ) -> AnyView {
        let label = label.into();
        let palette = palette.clone();
        cx.new(|_| Self { label, palette }).into()
    }
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let p = &self.palette;
        div()
            .px(px(space::SM))
            .py(px(space::HAIR))
            .rounded(px(radius::SM))
            .bg(color(p.overlay))
            .border_1()
            .border_color(crate::theme::border(p))
            .text_size(px(font_size::BODY))
            .text_color(color(p.foreground))
            .whitespace_nowrap()
            .child(self.label.clone())
    }
}

//! 空状态。

use crate::{
    icon::{icon, Icon},
    theme::{color, muted},
    tokens::{font_size, icon_size, space},
};
use gpui::{div, prelude::*, px, Div, IntoElement, SharedString};
use termior_theme::ResolvedPalette;

/// 居中空状态：可选图标、一句说明、可选次要说明，以及可选操作区（由调用方 `.child` 追加）。
pub fn empty_state(p: &ResolvedPalette) -> Div {
    div()
        .flex()
        .flex_col()
        .size_full()
        .items_center()
        .justify_center()
        .gap(px(space::LG))
        .text_color(color(p.foreground))
}

/// 带图标与主文案的空状态主体。操作按钮由调用方继续 `.child(...)`。
pub fn empty_state_message(
    glyph: Icon,
    title: impl Into<SharedString>,
    detail: Option<SharedString>,
    p: &ResolvedPalette,
) -> Div {
    empty_state(p)
        .child(icon(glyph, icon_size::LG, muted(p)))
        .child(div().text_size(px(font_size::EMPHASIS)).child(title.into()))
        .when_some(detail, |root, text| {
            root.child(
                div()
                    .text_size(px(font_size::BODY))
                    .text_color(muted(p))
                    .child(text),
            )
        })
}

/// 侧栏内的低调空提示（左对齐、不占满屏）。
pub fn empty_hint(label: impl Into<SharedString>, p: &ResolvedPalette) -> impl IntoElement {
    div()
        .p(px(space::MD))
        .text_size(px(font_size::BODY))
        .text_color(muted(p))
        .child(label.into())
}

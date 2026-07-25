//! 弹出菜单 chrome。
//!
//! 定位仍由调用方用 `anchored()` 负责；这里只统一面板与菜单项的外观，
//! 避免三个菜单各写一套 `p_1 / rounded_md / shadow_md`。

use crate::{
    theme::{border, color, hover_wash, selected_wash},
    tokens::{font_size, radius, space},
};
use gpui::{div, prelude::*, px, Div, SharedString};
use termior_theme::ResolvedPalette;

/// 菜单面板外壳。调用方自行 `.children(...)` 填入菜单项。
pub fn menu_panel(p: &ResolvedPalette) -> Div {
    div()
        .occlude()
        .p(px(space::XS))
        .rounded(px(radius::MD))
        .border_1()
        .border_color(border(p))
        .bg(color(p.overlay))
        .shadow_md()
}

/// 一条菜单项。
///
/// `enabled = false` 时降低不透明度并去掉指针与 hover；仍可渲染，方便给出禁用原因。
/// 需要键盘焦点时由调用方追加 `.id(...).focusable()`。
pub fn menu_item(p: &ResolvedPalette, selected: bool, enabled: bool) -> Div {
    let hover = hover_wash(p);
    div()
        .px(px(space::MD))
        .py(px(space::XS))
        .rounded(px(radius::SM))
        .text_size(px(font_size::BODY))
        .opacity(if enabled { 1.0 } else { 0.45 })
        .when(selected, |item| item.bg(selected_wash(p)))
        .when(enabled, move |item| {
            item.cursor_pointer().hover(move |style| style.bg(hover))
        })
}

/// 菜单内部分隔线。
pub fn menu_separator(p: &ResolvedPalette) -> Div {
    div().h(px(1.0)).my(px(space::XS)).bg(border(p))
}

/// 禁用态提示行（不可点击）。
pub fn menu_hint(label: impl Into<SharedString>, p: &ResolvedPalette) -> Div {
    div()
        .px(px(space::MD))
        .py(px(space::XS))
        .mb(px(space::XS))
        .text_size(px(font_size::BODY))
        .opacity(0.65)
        .text_color(crate::theme::muted(p))
        .child(label.into())
}

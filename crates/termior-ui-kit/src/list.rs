//! 列表行。

use crate::{
    theme::{hover_wash, selected_wash},
    tokens::{font_size, indent, radius, space},
};
use gpui::{div, prelude::*, px, Div, SharedString};
use termior_theme::ResolvedPalette;

/// 一行可交互列表项的外壳。
///
/// - `selected`：整行铺 `selected_wash`
/// - 未选中时提供 hover 蒙层
/// - `depth`：左侧缩进层数（文件树用），步进见 [`indent::STEP`]
pub fn list_row(p: &ResolvedPalette, selected: bool, depth: usize) -> Div {
    let hover = hover_wash(p);
    div()
        .w_full()
        .ml(px(depth as f32 * indent::STEP))
        .px(px(space::XS))
        .py(px(space::HAIR))
        .rounded(px(radius::SM))
        .text_size(px(font_size::BODY))
        .cursor_pointer()
        .when(selected, |row| row.bg(selected_wash(p)))
        .when(!selected, move |row| {
            row.hover(move |style| style.bg(hover))
        })
}

/// 行内主文案 + 次要文案的纵向堆叠（Git 历史那类两行信息）。
pub fn list_row_meta(
    primary: impl Into<SharedString>,
    secondary: impl Into<SharedString>,
    p: &ResolvedPalette,
) -> Div {
    div()
        .flex()
        .flex_col()
        .min_w(px(0.0))
        .child(primary.into())
        .child(
            div()
                .text_size(px(font_size::MICRO))
                .text_color(crate::theme::muted(p))
                .child(secondary.into()),
        )
}

//! 输入框 chrome。
//!
//! GPUI 没有现成的文本框控件——IME 与键盘编辑由各 view 自己的 `InputHandler`
//! 承担。本模块只提供统一的视觉外壳：底色、边框、聚焦态、内边距与字号。
//! 把草稿字符串（可含 `|` 光标字符）作为 child 传入即可。

use crate::{
    theme::{border, color},
    tokens::{font_size, radius, space},
};
use gpui::{div, prelude::*, px, Div};
use termior_theme::ResolvedPalette;

/// 单行输入框外壳。
///
/// `focused` 为真时边框切到 accent；否则用通用分隔线色。
pub fn input_field(p: &ResolvedPalette, focused: bool) -> Div {
    div()
        .px(px(space::LG))
        .py(px(space::MD))
        .rounded(px(radius::MD))
        .border_1()
        .border_color(if focused { color(p.accent) } else { border(p) })
        .bg(color(p.elevated))
        .text_size(px(font_size::BODY))
        .text_color(color(p.foreground))
        .cursor_text()
}

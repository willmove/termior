//! IME 候选窗锚点，composer / 编辑器 / 设置 / git 历史过滤等自定义文本输入共用。
//!
//! 这些输入都把光标画成文本流里的内联 `|` 字符，没有独立元素可直接测量。锚点由
//! 文本容器内的探针 canvas 在 paint 时记录（文本 bounds + 当时的环境文本样式），
//! `InputHandler::bounds_for_range` 再用 `layout_line` 量出光标前缀宽度得到锚点。
//! Windows 上 GPUI 在 `WM_IME_STARTCOMPOSITION` 时读取该锚点定位候选窗；返回
//! `None` 会让各 IME 退回默认位置（小狼毫在窗口左上角、微软拼音在右下角）。

use gpui::{
    canvas, point, px, size, AnyElement, App, Bounds, Font, IntoElement, Pixels, Styled, TextRun,
    Window,
};
use std::{cell::RefCell, rc::Rc};

#[derive(Clone)]
struct AnchorState {
    text_bounds: Bounds<Pixels>,
    font: Font,
    font_size: Pixels,
    line_height: Pixels,
}

/// 探针 paint 与 `InputHandler::bounds_for_range` 之间共享的锚点缓存。
#[derive(Clone, Default)]
pub(crate) struct ImeAnchor(Rc<RefCell<Option<AnchorState>>>);

impl ImeAnchor {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 探针 canvas 的 paint 闭包调用：记录文本 bounds 与环境样式。探针挂在文本
    /// 节点旁，paint 时的 `window.text_style()` 就是该文本的实际渲染样式。
    fn record(&self, text_bounds: Bounds<Pixels>, window: &Window) {
        let style = window.text_style();
        let rem_size = window.rem_size();
        let font_size = style.font_size.to_pixels(rem_size);
        *self.0.borrow_mut() = Some(AnchorState {
            text_bounds,
            font: Font {
                family: style.font_family.clone(),
                weight: style.font_weight,
                style: style.font_style,
                features: style.font_features.clone(),
                fallbacks: style.font_fallbacks.clone(),
            },
            font_size,
            line_height: style.line_height.to_pixels(style.font_size, rem_size),
        });
    }

    /// 光标锚点：文本 bounds 原点 + `text[..caret_byte]` 的自然宽度。
    /// 单行输入精确；会折行的长文本按未折行宽度取首行近似（横向截到 bounds 内）。
    pub(crate) fn caret_bounds(
        &self,
        text: &str,
        caret_byte: usize,
        window: &Window,
    ) -> Option<Bounds<Pixels>> {
        let state = self.0.borrow().clone()?;
        let run = TextRun {
            len: text.len(),
            font: state.font,
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window
            .text_system()
            .layout_line(text, state.font_size, &[run], None);
        let x = line
            .x_for_index(caret_byte.min(text.len()))
            .min(state.text_bounds.size.width);
        Some(Bounds {
            origin: state.text_bounds.origin + point(x, px(0.0)),
            size: size(px(2.0), state.line_height),
        })
    }
}

/// 探针元素：放在（已 `relative()` 定位的）文本容器里，paint 时记录文本 bounds
/// 与环境样式。每帧 render 重新创建，闭包拿 `ImeAnchor` 的克隆。
///
/// 必须显式 `top_0().left_0()`：无 inset 的 absolute 元素会被排在静态位置
/// （上一个兄弟节点之后），而不是钉在定位祖先的原点上。
pub(crate) fn anchor_probe(anchor: &ImeAnchor) -> AnyElement {
    let anchor = anchor.clone();
    canvas(
        move |_, _, _| (),
        move |bounds, _, window: &mut Window, _: &mut App| {
            anchor.record(bounds, window);
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

//! 手写滚动容器的共享竖向滚动条。
//!
//! 视图自己持有 `gpui::ScrollHandle` 并在滚动容器上 `track_scroll`；
//! 这里只负责画轨道与滑块。轨道和滑块都在 canvas 的 paint 闭包里绘制，
//! 因此能跟随每帧的真实滚动状态，内容放得下（max_offset 为 0）时整条隐藏，
//! 无需重建元素树。拖拽/点击换页由调用方挂鼠标事件，配合
//! [`scroll_to_pointer`] 使用。
#![forbid(unsafe_code)]

use gpui::{canvas, div, prelude::*, px, ElementId, Pixels, ScrollHandle, Stateful};
use termior_theme::ResolvedPalette;

use crate::theme::alpha;
use crate::tokens::space;

/// 滑块最小可点击高度。
const MIN_THUMB: f32 = 24.0;

fn thumb_height(viewport: f32, max_offset: f32) -> f32 {
    (viewport * viewport / (viewport + max_offset)).clamp(MIN_THUMB, viewport)
}

/// 滚动条列。宽度恒定占据布局（避免出现/消失时内容回流）；
/// 是否可见由 paint 闭包按当帧 `max_offset` 决定。
/// 拖拽所需的鼠标事件由调用方附加（见 `scroll_to_pointer`）。
pub fn scrollbar(
    id: impl Into<ElementId>,
    scroll: &ScrollHandle,
    p: &ResolvedPalette,
) -> Stateful<gpui::Div> {
    let scroll = scroll.clone();
    let track = alpha(p.foreground, 0.08);
    let thumb = alpha(p.foreground, 0.5);
    div()
        .id(id)
        .w(px(space::LG))
        .h_full()
        .flex_shrink_0()
        .cursor_pointer()
        .child(
            canvas(
                |bounds, _, _| bounds,
                move |bounds, _, window, _| {
                    let h = f32::from(bounds.size.height);
                    let max = f32::from(scroll.max_offset().y);
                    if h <= 0.0 || max <= 0.0 {
                        return; // 内容放得下：轨道与滑块都不要。
                    }
                    let th = thumb_height(h, max);
                    let top = (-f32::from(scroll.offset().y) / max).clamp(0., 1.) * (h - th);
                    window.paint_quad(gpui::fill(bounds, track));
                    window.paint_quad(gpui::fill(
                        gpui::Bounds::new(
                            bounds.origin + gpui::point(px(space::HAIR), px(top)),
                            gpui::size(px(space::MD), px(th)),
                        ),
                        thumb,
                    ));
                },
            )
            .size_full(),
        )
}

/// 把视口坐标的指针 y 换算成滚动偏移：点击跳转和按住拖拽共用。
pub fn scroll_to_pointer(scroll: &ScrollHandle, y: Pixels) {
    let bounds = scroll.bounds();
    let height = f32::from(bounds.size.height).max(1.0);
    let max = f32::from(scroll.max_offset().y);
    let thumb = thumb_height(height, max);
    let ratio =
        ((f32::from(y - bounds.top()) - thumb / 2.0) / (height - thumb).max(1.0)).clamp(0.0, 1.0);
    scroll.set_offset(gpui::point(px(0.), px(-max * ratio)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumb_height_clamps_to_viewport_and_minimum() {
        assert_eq!(thumb_height(100., 0.), 100.);
        assert_eq!(thumb_height(100., 100.), 50.);
        assert_eq!(thumb_height(100., 1_000_000.), 24.);
    }
}

//! 自绘标题栏的构件。
//!
//! 应用把标签页画进标题栏，所以窗口的最小化/最大化/关闭也得自己画。点击行为并
//! 不由我们处理：给按钮标上 [`WindowControlArea`]，平台层会在命中测试里把它们
//! 报告成原生的标题栏按钮，于是 Windows 的 Snap Layouts、双击最大化、系统菜单
//! 全部照常工作。
//!
//! 关键约束：标题栏里任何可交互元素都必须 [`occlude`]，否则会被父级的拖拽区
//! 吞掉 —— 命中测试按绘制顺序返回第一个匹配，父级先于子级插入。
//!
//! [`occlude`]: gpui::InteractiveElement::occlude

use crate::{
    icon::{icon, Icon},
    theme::{active_wash, alpha, color, hover_wash},
    tokens::{height, icon_size},
};
use gpui::{div, prelude::*, px, Div, Stateful, WindowControlArea};
use termior_theme::ResolvedPalette;

/// 窗口控制按钮宽度。取自 Windows 原生标题栏按钮（46px）就近的栅格档位。
const CONTROL_WIDTH: f32 = 44.0;

/// 关闭按钮的悬浮红。Windows 与 Linux 桌面环境的通用约定，不走主题色 —— 用户
/// 对这个位置上的红色有肌肉记忆。
const CLOSE_HOVER: u32 = 0xe81123ff;

/// macOS 由系统绘制红绿灯，不需要我们这组按钮。
pub const fn draws_own_window_controls() -> bool {
    !cfg!(target_os = "macos")
}

/// macOS 红绿灯所占的左侧宽度，标题栏内容需要为它让位。
pub const MACOS_TRAFFIC_LIGHT_INSET: f32 = 72.0;

/// 最小化 / 最大化或还原 / 关闭三个按钮。
pub fn window_controls(p: &ResolvedPalette, is_maximized: bool) -> Div {
    let restore_or_maximize = if is_maximized {
        Icon::WindowRestore
    } else {
        Icon::WindowMaximize
    };
    div()
        .flex()
        .flex_none()
        .h(px(height::TITLE_BAR))
        .child(control(
            "window-minimize",
            Icon::WindowMinimize,
            "Minimize",
            WindowControlArea::Min,
            false,
            p,
        ))
        .child(control(
            "window-maximize",
            restore_or_maximize,
            if is_maximized { "Restore" } else { "Maximize" },
            WindowControlArea::Max,
            false,
            p,
        ))
        .child(control(
            "window-close",
            Icon::Close,
            "Close",
            WindowControlArea::Close,
            true,
            p,
        ))
}

fn control(
    id: &'static str,
    glyph: Icon,
    label: &'static str,
    area: WindowControlArea,
    destructive: bool,
    p: &ResolvedPalette,
) -> Stateful<Div> {
    let resting = alpha(p.foreground, 0.72);
    let engaged = color(p.foreground);
    let hover_bg = if destructive {
        gpui::rgba(CLOSE_HOVER)
    } else {
        hover_wash(p)
    };
    let hover_fg = if destructive {
        gpui::rgba(0xffffffff)
    } else {
        engaged
    };
    let active_bg = if destructive {
        alpha(termior_theme::Color::rgb(0xe8, 0x11, 0x23), 0.8)
    } else {
        active_wash(p)
    };
    div()
        .id(id)
        .group(id)
        .aria_label(label)
        .occlude()
        .window_control_area(area)
        .w(px(CONTROL_WIDTH))
        .h(px(height::TITLE_BAR))
        .flex()
        .items_center()
        .justify_center()
        .hover(move |style| style.bg(hover_bg))
        .active(move |style| style.bg(active_bg))
        .child(
            icon(glyph, icon_size::XS, resting)
                .group_hover(id, move |style| style.text_color(hover_fg)),
        )
}

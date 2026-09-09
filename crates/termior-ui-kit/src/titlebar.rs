//! 自绘标题栏的构件。
//!
//! 应用把标签页画进标题栏，所以窗口的最小化/最大化/关闭也得自己画。点击行为的
//! 分派是平台相关的：给按钮标上 [`WindowControlArea`] 后，Windows 后端会在
//! `WM_NCHITTEST` 命中测试里把它们报告成原生的标题栏按钮，于是 Snap Layouts、
//! 双击最大化、系统菜单全部照常工作；gpui 的 X11/Wayland 后端不实现窗口控制
//! 命中测试，Linux/BSD 上的点击必须由按钮自己处理（见
//! [`handles_own_window_control_clicks`]）。
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

/// Linux/BSD 上窗口控制按钮必须自己响应点击：gpui（zed rev 3565c49）的
/// X11/Wayland 后端把 `on_hit_test_window_control` 实现为空操作，核心也不会
/// 在点击 `window_control_area` 区域时执行动作，点击若不自理会完全无效。
/// Windows 上则相反——命中测试（`WM_NCHITTEST`）已让系统处理点击，再挂
/// `on_click` 会与 `DefWindowProcW(WM_CLOSE)` 双重关闭，踩中
/// `WindowsWindow::drop` 二次 `DestroyWindow` 的竞态（见 workspace_view
/// 设置窗口的注释），因此那条路径绝不手动处理。
pub const fn handles_own_window_control_clicks() -> bool {
    cfg!(any(target_os = "linux", target_os = "freebsd"))
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
        .when(handles_own_window_control_clicks(), |button| {
            button.on_click(move |_, window, cx| {
                cx.stop_propagation();
                match area {
                    WindowControlArea::Close => window.remove_window(),
                    WindowControlArea::Max => window.zoom_window(),
                    WindowControlArea::Min => window.minimize_window(),
                    WindowControlArea::Drag => {}
                }
            })
        })
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

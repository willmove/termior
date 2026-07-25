//! 主题 token → GPUI 样式的映射层。
//!
//! 所有视图（含独立的设置窗口）通过 [`palette`] 读取当前色板，主题切换时
//! [`set_palette`] 更新全局并刷新全部窗口，保证浅色/深色主题下每个界面都能正确
//! 着色 —— 禁止在视图里硬编码 rgba 颜色。

use gpui::{App, Global, Rgba};
use termior_theme::{Color, ResolvedPalette};

/// 当前生效的主题色板（GPUI 全局）。
pub struct ActiveTheme(pub ResolvedPalette);

impl Global for ActiveTheme {}

/// 读取当前色板；主题尚未初始化时退回默认深色，避免 panic。
pub fn palette(cx: &App) -> ResolvedPalette {
    cx.try_global::<ActiveTheme>()
        .map(|theme| theme.0.clone())
        .unwrap_or_else(|| termior_theme::default_theme().palette().clone())
}

/// 更新全局色板并重绘所有窗口（设置窗口与主窗口同步换肤）。
pub fn set_palette(cx: &mut App, palette: ResolvedPalette) {
    cx.set_global(ActiveTheme(palette));
    cx.refresh_windows();
}

/// 主题色 → GPUI 不透明色。
pub fn color(value: Color) -> Rgba {
    gpui::rgba(((value.r as u32) << 24) | ((value.g as u32) << 16) | ((value.b as u32) << 8) | 0xff)
}

/// 主题色 → 带透明度的 GPUI 色（弱化文字、分隔线、悬浮蒙层等）。
pub fn alpha(value: Color, opacity: f32) -> Rgba {
    let a = (opacity.clamp(0.0, 1.0) * 255.0).round() as u32;
    gpui::rgba(((value.r as u32) << 24) | ((value.g as u32) << 16) | ((value.b as u32) << 8) | a)
}

/// 次要文字色（说明、路径、时间戳）。
pub fn muted(p: &ResolvedPalette) -> Rgba {
    alpha(p.foreground, 0.62)
}

/// 通用边框/分隔线色：以前景色低透明度实现，浅深主题均成立。
pub fn border(p: &ResolvedPalette) -> Rgba {
    alpha(p.foreground, 0.14)
}

/// 鼠标悬浮蒙层。
pub fn hover_wash(p: &ResolvedPalette) -> Rgba {
    alpha(p.foreground, 0.08)
}

/// 按下态蒙层。比悬浮更重一档，让点击有即时反馈。
pub fn active_wash(p: &ResolvedPalette) -> Rgba {
    alpha(p.foreground, 0.14)
}

/// 选中/激活行背景（基于 accent）。
pub fn selected_wash(p: &ResolvedPalette) -> Rgba {
    alpha(p.accent, 0.20)
}

/// 键盘焦点描边色。
pub fn focus_ring(p: &ResolvedPalette) -> Rgba {
    alpha(p.accent, 0.9)
}

fn blend(a: Color, b: Color, amount: f32) -> Color {
    let mix = |x: u8, y: u8| {
        (x as f32 * (1.0 - amount) + y as f32 * amount)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color::rgb(mix(a.r, b.r), mix(a.g, b.g), mix(a.b, b.b))
}

fn luminance(value: Color) -> f32 {
    (0.2126 * value.r as f32 + 0.7152 * value.g as f32 + 0.0722 * value.b as f32) / 255.0
}

/// 实色背景上的可读文字色（黑或白，按亮度选择）。
pub fn on_color(value: Color) -> Rgba {
    if luminance(value) > 0.6 {
        gpui::rgba(0x1a1a1aff)
    } else {
        gpui::rgba(0xffffffff)
    }
}

/// 实色按钮悬浮时的提亮/压暗色。
pub fn hover_shade(value: Color) -> Color {
    if luminance(value) > 0.6 {
        blend(value, Color::rgb(0, 0, 0), 0.12)
    } else {
        blend(value, Color::rgb(255, 255, 255), 0.12)
    }
}

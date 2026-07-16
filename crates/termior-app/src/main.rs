//! `termior-app` — GPUI 入口（FR-WS 骨架 / FR-THEME 渲染层接入）。
//!
//! 最小可运行窗口：打开一个 GPUI 窗口，用 `termior_theme` 中央主题引擎解析调色板并驱动
//! 窗口背景/前景/状态色；点击切换 default ↔ nord，验证纯逻辑主题 crate 能驱动真实 GPU 渲染。
//!
//! 这是 M0 骨架的起点：后续把 tab/分栏/sidebar/statusbar/header/composer 视图逐步接入。
//! 对齐 Spec §6.1 / §6.7：Theme 结构 + 全局 Entity 是唯一主题源，切换时广播重绘。

use gpui::{
    div, prelude::*, px, size, App, Bounds, Context, Entity, FontWeight, MouseButton,
    MouseDownEvent, SharedString, Window, WindowBounds, WindowOptions,
};
use gpui_platform::application;
use termior_theme::{builtin_themes, Appearance, ResolvedPalette, Theme};

/// 工作区根状态：持有当前主题与解析后的调色板（FR-THEME-01：全局 Entity 是唯一主题源）。
struct Workspace {
    themes: Vec<Theme>,
    current: usize,
    palette: ResolvedPalette,
}

impl Workspace {
    fn new() -> Self {
        let themes = builtin_themes();
        let current = 0;
        let palette = themes[current].resolve(Appearance::Dark, true);
        Self {
            themes,
            current,
            palette,
        }
    }

    fn current_theme(&self) -> &Theme {
        &self.themes[self.current]
    }

    fn cycle_theme(&mut self, _ev: &MouseDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.current = (self.current + 1) % self.themes.len();
        self.palette = self.current_theme().resolve(Appearance::Dark, true);
        cx.notify();
    }
}

/// 把 termior `Color` 转 gpui `Rgba`（打包为 0xRRGGBBAA）。
fn gpui_color(c: termior_theme::Color) -> gpui::Rgba {
    gpui::rgba(((c.r as u32) << 24) | ((c.g as u32) << 16) | ((c.b as u32) << 8) | 0xff)
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = &self.palette;
        let theme_name: SharedString = self.current_theme().name.clone().into();
        let theme_id: SharedString = self.current_theme().id.clone().into();

        // 状态色 swatches：info/success/warning/danger
        let status = p.status;
        let diff = p.diff;

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(gpui_color(p.background))
            .text_color(gpui_color(p.foreground))
            // header
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .h(px(44.0))
                    .px_4()
                    .bg(gpui_color(p.surface[2]))
                    .border_b_1()
                    .border_color(gpui_color(p.surface[1]))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(SharedString::from("Termior")),
                    )
                    .child(div().text_xs().child(SharedString::from(format!(
                        "theme: {} ({})",
                        theme_name, theme_id
                    )))),
            )
            // 主区域：主题信息 + 状态色板
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .p_6()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(SharedString::from("中央主题引擎 · FR-THEME-01/02")),
                    )
                    .child(div().text_sm().child(SharedString::from(format!(
                        "background {}  surface[0] {}  accent {}",
                        p.background.to_hex(),
                        p.surface[0].to_hex(),
                        p.accent.to_hex()
                    ))))
                    // 状态色 swatches
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(color_swatch("info", gpui_color(status[0])))
                            .child(color_swatch("success", gpui_color(status[1])))
                            .child(color_swatch("warning", gpui_color(status[2])))
                            .child(color_swatch("danger", gpui_color(status[3]))),
                    )
                    // diff 色 swatches
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(color_swatch("+added", gpui_color(diff[0])))
                            .child(color_swatch("-removed", gpui_color(diff[1])))
                            .child(color_swatch(" context", gpui_color(diff[2]))),
                    )
                    // 终端 16 色条
                    .child(terminal_palette_bar(p)),
            )
            // 底部切换按钮（点击 cycle 主题，广播重绘）
            .child(
                div()
                    .flex()
                    .flex_row()
                    .h(px(48.0))
                    .px_4()
                    .items_center()
                    .border_t_1()
                    .border_color(gpui_color(p.surface[1]))
                    .bg(gpui_color(p.surface[0]))
                    .child(
                        div()
                            .id("cycle-theme")
                            .px_4()
                            .py_2()
                            .rounded_md()
                            .bg(gpui_color(p.accent))
                            .text_color(gpui_color(p.background))
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Cycle theme")
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, cx.listener(Self::cycle_theme)),
                    )
                    .child(div().ml_4().text_xs().italic().child(SharedString::from(
                        "点击切换 default ↔ nord，验证主题热切换广播重绘",
                    ))),
            )
    }
}

fn color_swatch(label: &str, color: gpui::Rgba) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .items_center()
        .child(
            div()
                .w(px(64.0))
                .h(px(32.0))
                .rounded_md()
                .bg(color)
                .border_1()
                .border_color(gpui::rgba(0x00000050)),
        )
        .child(div().text_xs().child(SharedString::from(label.to_string())))
}

fn terminal_palette_bar(p: &ResolvedPalette) -> impl IntoElement {
    let row = div().flex().flex_row().gap(px(1.0));
    let mut row = row;
    for c in p.terminal.all() {
        row = row.child(div().w(px(20.0)).h(px(20.0)).bg(gpui_color(c)));
    }
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_xs().child("terminal 16 colors"))
        .child(row)
}

fn run() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(720.), px(520.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_entity, cx| {
                let workspace: Entity<Workspace> = cx.new(|_cx| Workspace::new());
                workspace
            },
        )
        .unwrap();
        cx.activate(true);
    });
}

fn main() {
    run();
}

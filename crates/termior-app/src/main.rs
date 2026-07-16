//! `termior-app` — GPUI 入口（FR-WS 骨架 / FR-THEME / FR-TERM 终端 tab）。
//!
//! 打开一个 GPUI 窗口，挂载主题引擎驱动的调色板与第一个真终端 tab（PTY + alacritty
//! 网格 + canvas 渲染）。M1 阶段：单终端填满主区域，键盘输入转发 PTY。
//! 对齐 Spec §6.1 / §6.2 / §6.7。

mod keystroke;
mod terminal_view;

use gpui::{
    div, prelude::*, px, size, App, Bounds, Context, Entity, Focusable, FontWeight, MouseButton,
    MouseDownEvent, SharedString, Window, WindowBounds, WindowOptions,
};
use gpui_platform::application;
use termior_theme::{builtin_themes, Appearance, ResolvedPalette, Theme};

use crate::terminal_view::TerminalView;

/// 工作区根状态：主题 + 终端视图。
struct Workspace {
    themes: Vec<Theme>,
    current: usize,
    palette: ResolvedPalette,
    terminal: Option<Entity<TerminalView>>,
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
            terminal: None,
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

    /// 创建第一个终端 tab（若尚无）。
    /// PTY spawn 在 cx.new() 外完成（避免阻塞 GPUI 事件借用周期导致 RefCell 重入）。
    fn spawn_terminal(
        &mut self,
        _ev: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.terminal.is_some() {
            return;
        }
        // 先同步 spawn PTY（在 GPUI borrow 周期外），拿到 bridge 后再建 view。
        let bridge = match termior_terminal::TerminalBridge::spawn(&Default::default()) {
            Ok(b) => b,
            Err(e) => {
                log::error!("PTY spawn failed: {e}");
                return;
            }
        };
        let palette = self.palette.clone();
        let terminal = cx.new(|cx| TerminalView::from_bridge(bridge, palette, cx));
        let handle = terminal.read(cx).focus_handle(cx);
        self.terminal = Some(terminal);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// 联调用：无 Window 句柄时自动 spawn 终端（不设焦点）。
    fn auto_spawn_terminal(&mut self, cx: &mut Context<Self>) {
        if self.terminal.is_some() {
            return;
        }
        // 联调开关：TERMior_SHELL=pwsh|powershell|cmd 覆盖默认探测。
        let mut config = termior_terminal::PtySessionConfig::default();
        if let Ok(shell) = std::env::var("TERMior_SHELL") {
            match shell.as_str() {
                "pwsh" => config.shell = Some(termior_terminal_core::ShellKind::Pwsh),
                "powershell" => config.shell = Some(termior_terminal_core::ShellKind::PowerShell),
                "cmd" => config.shell = Some(termior_terminal_core::ShellKind::Cmd),
                other => log::warn!("unknown TERMior_SHELL={other}, using default"),
            }
        }
        let bridge = match termior_terminal::TerminalBridge::spawn(&config) {
            Ok(b) => b,
            Err(e) => {
                log::error!("PTY spawn failed: {e}");
                return;
            }
        };
        let palette = self.palette.clone();
        let terminal = cx.new(|cx| TerminalView::from_bridge(bridge, palette, cx));
        self.terminal = Some(terminal);
        cx.notify();
    }
}

/// 把 termior `Color` 转 gpui `Rgba`（打包为 0xRRGGBBAA，不透明）。
fn gpui_color(c: termior_theme::Color) -> gpui::Rgba {
    gpui::rgba(((c.r as u32) << 24) | ((c.g as u32) << 16) | ((c.b as u32) << 8) | 0xff)
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = &self.palette;
        let theme_name: SharedString = self.current_theme().name.clone().into();

        let main_area = if let Some(terminal) = self.terminal.clone() {
            // 终端视图填满主区域
            div().size_full().child(terminal)
        } else {
            // 未创建终端：显示提示
            div()
                .flex()
                .flex_col()
                .size_full()
                .items_center()
                .justify_center()
                .gap_4()
                .child(
                    div()
                        .text_xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(SharedString::from("Termior · M1 终端 MVP")),
                )
                .child(
                    div()
                        .id("new-terminal")
                        .px_4()
                        .py_2()
                        .rounded_md()
                        .bg(gpui_color(p.accent))
                        .text_color(gpui_color(p.background))
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("打开终端 (New terminal)")
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::spawn_terminal)),
                )
                .child(div().text_xs().italic().child(SharedString::from(format!(
                    "theme: {theme_name} · 点击按钮 spawn 第一个 PTY 终端 tab"
                ))))
        };

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
                    .h(px(36.0))
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
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .text_xs()
                                    .child(SharedString::from(format!("theme: {theme_name}"))),
                            )
                            .child(
                                div()
                                    .id("header-cycle-theme")
                                    .text_xs()
                                    .cursor_pointer()
                                    .child("⏾")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(Self::cycle_theme),
                                    ),
                            ),
                    ),
            )
            // 主区域：终端 或 提示
            .child(main_area)
    }
}

fn run() {
    // 初始化日志（OSC 事件 / PTY 错误输出到 stderr）。
    let _ = env_logger::try_init();

    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.), px(600.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_entity, cx| {
                let workspace: Entity<Workspace> = cx.new(|_cx| Workspace::new());
                // 联调开关：设置 TERMior_AUTO_SPAWN=1 时，窗口打开后自动 spawn 终端，
                // 便于无 GUI 交互的环境验证 PTY→Term→渲染 全链路。默认关闭（保持点击 spawn）。
                if std::env::var("TERMior_AUTO_SPAWN").ok().as_deref() == Some("1") {
                    let ws = workspace.downgrade();
                    cx.spawn(async move |cx| {
                        // 给窗口一点时间完成首帧。
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(300))
                            .await;
                        let _ = ws.update(cx, |view, cx| {
                            view.auto_spawn_terminal(cx);
                        });
                    })
                    .detach();
                }
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

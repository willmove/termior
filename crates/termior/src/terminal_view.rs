//! `TerminalView` — GPUI 终端视图（FR-TERM-08 渲染 / FR-TERM-01 键盘）。
//!
//! 持有 alacritty `Term` 网格与 vte `Processor`，消费 PTY reader 线程过滤后的字节流。
//! 渲染用 canvas 逐 cell 绘制（背景 paint_quad + 前景文本 run），键盘经 keystroke 映射写回 PTY。
//! OSC 7/133/777 经 `OscParser` 旁路嗅探，并同步 cwd、shell integration 与代理状态。

use alacritty_terminal::{
    event::{Event as AlacrittyEvent, WindowSize as TerminalWindowSize},
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point as TerminalPoint, Side},
    selection::{Selection, SelectionType},
    term::{cell::Flags, ClipboardType, Config as TermConfig, RenderableContent, Term, TermMode},
    vte::ansi::{
        Color as VteColor, CursorShape, NamedColor, Processor as VteProcessor, Rgb, StdSyncHandler,
    },
};
use futures::StreamExt;
use gpui::{
    canvas, div, fill, point, prelude::FluentBuilder, px, App, Bounds, ClipboardItem, Context,
    EventEmitter, FocusHandle, Focusable, Font, FontFeatures, FontStyle, FontWeight, Hsla,
    InputHandler, InteractiveElement, IntoElement, KeyDownEvent, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render,
    ScrollWheelEvent, SharedString, StrikethroughStyle, Styled, Subscription, Task, TextAlign,
    TextRun, UTF16Selection, UnderlineStyle, WeakEntity, Window,
};
use termior_store::{TerminalSettings, UserKeymap};
use termior_terminal::{PtySessionConfig, TerminalBridge, TerminalEventProxy};
use termior_terminal_core::{
    find_hyperlinks,
    osc::{AgentState, OscEvent},
    shell_integration::{cd_command, ShellKind},
    TerminalSearch,
};
use termior_theme::{Color as ThemeColor, ResolvedPalette, TerminalPalette};
use termior_ui_kit::{tokens, SearchOverlay};

use crate::keystroke::{encode_paste, keystroke_to_pty_bytes};

const MAX_PTY_BATCH_BYTES: usize = 256 * 1024;

/// 终端网格四周与窗格边缘之间保留的间隙（像素）。避免文字紧贴窗格边框。
/// 取 `space::MD`——终端字号由用户设置驱动，但内边距仍落在 4px 栅格上。
const TERMINAL_PANE_PADDING: f32 = tokens::space::MD;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalViewEvent {
    TitleChanged(Option<String>),
    Bell,
    Exited(Option<i32>),
}

/// GPUI 终端视图。
pub struct TerminalView {
    bridge: TerminalBridge,
    /// spawn 时解析出的实际 shell 类型（cd 注入按此分派语法）。
    shell_kind: ShellKind,
    /// 是否为 WSL 会话（影响 cd 注入的路径形式）。
    is_wsl: bool,
    term: Term<TerminalEventProxy>,
    vte_processor: VteProcessor<StdSyncHandler>,
    palette: ResolvedPalette,
    focus_handle: FocusHandle,
    /// PTY 字节消费任务（持有以避免被取消）。
    _consumer: Task<()>,
    focus_in_subscription: Option<Subscription>,
    focus_out_subscription: Option<Subscription>,
    cols: usize,
    rows: usize,
    latest_cwd: Option<String>,
    /// shell/程序经 OSC 0/2 设置的原始标题（已清洗控制字符）。
    shell_title: Option<String>,
    /// 最近一次已 emit 的展示标题，用于去重（OSC 7 每个 prompt 都会上报）。
    emitted_title: Option<String>,
    localhost_urls: Vec<String>,
    agent_state: Option<AgentState>,
    marked_text: String,
    search_overlay: SearchOverlay,
    search: TerminalSearch,
    snapshot_text: String,
    buffer_text: String,
    text_style: TerminalTextStyle,
    cell_width: f32,
    line_height_px: f32,
    viewport_origin: Point<Pixels>,
    scroll_px: f32,
    last_mouse_cell: Option<(i32, usize, Option<MouseButton>)>,
    keymap: UserKeymap,
}

impl TerminalView {
    /// 创建终端视图：内部 spawn PTY、启动字节消费循环。
    /// 生产路径用 `from_bridge`（在 GPUI borrow 周期外 spawn，避免 RefCell 重入）；
    /// 本构造函数保留给测试/未来单测场景。
    #[allow(dead_code)]
    pub fn new(
        palette: ResolvedPalette,
        settings: TerminalSettings,
        keymap: UserKeymap,
        cx: &mut Context<Self>,
    ) -> Self {
        let bridge = TerminalBridge::spawn(&PtySessionConfig::default()).expect("PTY spawn failed");
        Self::from_bridge(bridge, palette, settings, keymap, cx)
    }

    /// 用已 spawn 的 bridge 创建视图。
    /// bridge 在 GPUI borrow 周期外创建，避免阻塞事件循环导致 RefCell 重入。
    pub fn from_bridge(
        mut bridge: TerminalBridge,
        palette: ResolvedPalette,
        settings: TerminalSettings,
        keymap: UserKeymap,
        cx: &mut Context<Self>,
    ) -> Self {
        let cols = PtySessionConfig::default().cols as usize;
        let rows = PtySessionConfig::default().rows as usize;
        let output_rx = bridge.take_output().expect("output channel");
        let (event_proxy, mut event_rx) = TerminalEventProxy::new(bridge.writer());
        log::info!("PTY attached: cols={cols} rows={rows}");

        let term_config = TermConfig {
            scrolling_history: settings.scrollback_lines as usize,
            ..Default::default()
        };
        let term = Term::new(
            term_config,
            &TermSize {
                columns: cols,
                screen_lines: rows,
            },
            event_proxy,
        );

        let focus_handle = cx.focus_handle();
        let frame_executor = cx.background_executor().clone();

        // 消费循环：PTY 字节 → vte 解析更新 Term + OscParser 旁路 → cx.notify 重绘。
        // Term 非 Send，只能在主线程访问，故字节通过 channel 送回主线程处理。
        let consumer = cx.spawn(async move |this, cx| {
            let mut rx = output_rx;
            let mut first_byte_seen = false;
            while let Some(mut data) = rx.next().await {
                while data.bytes.len() < MAX_PTY_BATCH_BYTES {
                    let Ok(next) = rx.try_recv() else {
                        break;
                    };
                    data.bytes.extend(next.bytes);
                    data.events.extend(next.events);
                    data.localhost_urls.extend(next.localhost_urls);
                }
                let backlog_likely = data.bytes.len() >= MAX_PTY_BATCH_BYTES;
                let bytes = data.bytes;
                let events = data.events;
                let localhost_urls = data.localhost_urls;
                let _ = this.update(cx, |view, cx| {
                    if !first_byte_seen {
                        first_byte_seen = true;
                        log::info!(
                            "first PTY bytes received ({} bytes), feeding Term grid",
                            bytes.len()
                        );
                    }
                    view.vte_processor.advance(&mut view.term, &bytes);
                    while let Ok(event) = event_rx.try_recv() {
                        view.handle_terminal_event(event, cx);
                    }
                    for ev in events {
                        log::info!("OSC event: {ev:?}");
                        match ev {
                            OscEvent::Cwd { path, .. } => {
                                view.latest_cwd = Some(path);
                                view.update_display_title(cx);
                            }
                            OscEvent::AgentEvent(state) if view.agent_state != Some(state) => {
                                view.agent_state = Some(state);
                                cx.emit(state);
                            }
                            OscEvent::AgentEvent(_) => {}
                            _ => {}
                        }
                    }
                    for url in localhost_urls {
                        if !view.localhost_urls.contains(&url) {
                            view.localhost_urls.push(url);
                        }
                    }
                    cx.notify();
                });
                if backlog_likely {
                    // Force a scheduler yield between large batches so continuous output cannot
                    // monopolize the GPUI executor.
                    frame_executor
                        .timer(std::time::Duration::from_millis(1))
                        .await;
                }
            }
            log::info!("PTY output stream ended");
            let _ = this.update(cx, |_view, cx| {
                cx.emit(TerminalViewEvent::Exited(None));
                cx.notify();
            });
        });

        let text_style = TerminalTextStyle::from_settings(&settings);
        let line_height_px = text_style.font_size * text_style.line_height;
        let shell_kind = bridge.shell_kind();
        let is_wsl = bridge.is_wsl();
        Self {
            bridge,
            shell_kind,
            is_wsl,
            term,
            vte_processor: VteProcessor::<StdSyncHandler>::default(),
            palette,
            focus_handle,
            _consumer: consumer,
            focus_in_subscription: None,
            focus_out_subscription: None,
            cols,
            rows,
            latest_cwd: None,
            shell_title: None,
            emitted_title: None,
            localhost_urls: Vec::new(),
            agent_state: None,
            marked_text: String::new(),
            search_overlay: SearchOverlay::default(),
            search: TerminalSearch::default(),
            snapshot_text: String::new(),
            buffer_text: String::new(),
            cell_width: text_style.font_size * 0.6 + text_style.letter_spacing,
            line_height_px,
            viewport_origin: Point::default(),
            scroll_px: 0.0,
            last_mouse_cell: None,
            text_style,
            keymap,
        }
    }

    pub fn latest_cwd(&self) -> Option<&str> {
        self.latest_cwd.as_deref()
    }

    /// 应用主题切换后同步终端色板(背景/前景/16 色即时生效)。
    pub fn set_palette(&mut self, palette: ResolvedPalette, cx: &mut Context<Self>) {
        if self.palette != palette {
            self.palette = palette;
            cx.notify();
        }
    }

    /// 重新计算标签页展示标题并在变化时 emit（OSC 7 每个 prompt 都上报，必须去重）。
    fn update_display_title(&mut self, cx: &mut Context<Self>) {
        let display = display_title(self.shell_title.as_deref(), self.latest_cwd.as_deref());
        if display != self.emitted_title {
            self.emitted_title = display.clone();
            cx.emit(TerminalViewEvent::TitleChanged(display));
            cx.notify();
        }
    }

    pub fn localhost_urls(&self) -> &[String] {
        &self.localhost_urls
    }

    pub fn recent_text(&self) -> String {
        termior_ai::context::tail_lines(&self.buffer_text, 300)
    }

    pub fn write_input(&self, bytes: &[u8]) -> std::io::Result<()> {
        self.bridge.writer().write_all(bytes)
    }

    /// 生成把本会话 shell 切到 `dir` 的注入命令（含回车），按 spawn 时解析的
    /// shell 类型与 WSL 状态选择语法（cmd 用 `cd /d`，WSL 转 `/mnt/<drive>` 路径）。
    pub fn cd_command_to(&self, dir: &std::path::Path) -> String {
        cd_command(self.shell_kind, self.is_wsl, dir)
    }

    fn handle_terminal_event(&mut self, event: AlacrittyEvent, cx: &mut Context<Self>) {
        match event {
            AlacrittyEvent::MouseCursorDirty
            | AlacrittyEvent::CursorBlinkingChange
            | AlacrittyEvent::Wakeup => cx.notify(),
            AlacrittyEvent::Title(title) => {
                self.shell_title = clean_terminal_title(title);
                self.update_display_title(cx);
            }
            AlacrittyEvent::ResetTitle => {
                self.shell_title = None;
                self.update_display_title(cx);
            }
            AlacrittyEvent::ClipboardStore(kind, text) => {
                let item = ClipboardItem::new_string(text);
                // Defer the clipboard access out of the current entity update: on
                // Windows, opening the clipboard can dispatch sent messages back into
                // our wndproc while this entity's RefCell is still borrowed.
                cx.defer(move |cx| match kind {
                    // GPUI's app context exposes the system clipboard everywhere.
                    // Primary selection is not portable (notably on Windows), so use
                    // the system clipboard as its fallback here.
                    ClipboardType::Clipboard | ClipboardType::Selection => {
                        cx.write_to_clipboard(item)
                    }
                });
            }
            AlacrittyEvent::ClipboardLoad(kind, formatter) => {
                // Same re-entrancy hazard as ClipboardStore above: read the clipboard
                // and reply to the PTY after the current update cycle completes.
                let writer = self.bridge.writer();
                cx.defer(move |cx| {
                    let text = match kind {
                        ClipboardType::Clipboard | ClipboardType::Selection => {
                            cx.read_from_clipboard()
                        }
                    }
                    .and_then(|item| item.text())
                    .unwrap_or_default();
                    let reply = formatter(&text);
                    if let Err(error) = writer.write_all(reply.as_bytes()) {
                        log::warn!("terminal clipboard reply failed: {error}");
                    }
                });
            }
            AlacrittyEvent::ColorRequest(index, formatter) => {
                let color = self.term.colors()[index]
                    .unwrap_or_else(|| terminal_rgb_for_index(index, &self.palette));
                let reply = formatter(color);
                if let Err(error) = self.write_input(reply.as_bytes()) {
                    log::warn!("terminal color reply failed: {error}");
                }
            }
            AlacrittyEvent::TextAreaSizeRequest(formatter) => {
                let size = TerminalWindowSize {
                    num_lines: self.rows.min(u16::MAX as usize) as u16,
                    num_cols: self.cols.min(u16::MAX as usize) as u16,
                    cell_width: self.cell_width.round().clamp(1.0, u16::MAX as f32) as u16,
                    cell_height: self.line_height_px.round().clamp(1.0, u16::MAX as f32) as u16,
                };
                let reply = formatter(size);
                if let Err(error) = self.write_input(reply.as_bytes()) {
                    log::warn!("terminal size reply failed: {error}");
                }
            }
            AlacrittyEvent::Bell => {
                cx.emit(TerminalViewEvent::Bell);
                cx.notify();
            }
            AlacrittyEvent::Exit => {
                cx.emit(TerminalViewEvent::Exited(None));
                cx.notify();
            }
            AlacrittyEvent::ChildExit(status) => {
                log::info!("terminal child exited: {status:?}");
                cx.emit(TerminalViewEvent::Exited(status.code()));
                cx.notify();
            }
            AlacrittyEvent::PtyWrite(text) => {
                // `TerminalEventProxy` handles this synchronously; keep this branch defensive in
                // case an alternate proxy forwards it in the future.
                if let Err(error) = self.write_input(text.as_bytes()) {
                    log::warn!("terminal protocol reply failed: {error}");
                }
            }
        }
    }

    /// Resize the terminal grid and PTY to the actual GPUI canvas dimensions.
    pub fn resize(
        &mut self,
        cols: usize,
        rows: usize,
        cell_width: f32,
        line_height_px: f32,
        viewport_origin: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if cols == 0 || rows == 0 {
            return;
        }
        let dimensions_changed = cols != self.cols || rows != self.rows;
        let metrics_changed = (self.cell_width - cell_width).abs() > f32::EPSILON
            || (self.line_height_px - line_height_px).abs() > f32::EPSILON;
        let origin_changed = self.viewport_origin != viewport_origin;
        if dimensions_changed {
            let pty_rows = rows.min(u16::MAX as usize) as u16;
            let pty_cols = cols.min(u16::MAX as usize) as u16;
            self.term.resize(TermSize {
                columns: cols,
                screen_lines: rows,
            });
            if let Err(error) = self.bridge.resize(pty_rows, pty_cols) {
                log::warn!("PTY resize failed: {error}");
            }
            self.cols = cols;
            self.rows = rows;
        }
        if metrics_changed {
            self.cell_width = cell_width;
            self.line_height_px = line_height_px;
        }
        if origin_changed {
            self.viewport_origin = viewport_origin;
        }
        if dimensions_changed || metrics_changed || origin_changed {
            cx.notify();
        }
    }

    pub fn set_settings(
        &mut self,
        settings: &TerminalSettings,
        keymap: &UserKeymap,
        cx: &mut Context<Self>,
    ) {
        self.text_style = TerminalTextStyle::from_settings(settings);
        self.keymap = keymap.clone();
        let config = TermConfig {
            scrolling_history: settings.scrollback_lines as usize,
            ..Default::default()
        };
        self.term.set_options(config);
        cx.notify();
    }

    pub fn open_search(&mut self, cx: &mut Context<Self>) {
        self.search_overlay.open();
        self.update_search();
        cx.notify();
    }

    /// 处理键盘输入：编码成 PTY 字节写回。
    fn handle_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = ev.keystroke.modifiers;
        let key = ev.keystroke.key.as_str();
        if self.keymap.bindings.values().any(|binding| {
            let primary_matches = if binding.primary {
                if cfg!(target_os = "macos") {
                    modifiers.platform && !modifiers.control
                } else {
                    modifiers.control
                }
            } else {
                modifiers.control && !modifiers.platform
            };
            primary_matches
                && modifiers.shift == binding.shift
                && modifiers.alt == binding.alt
                && key.eq_ignore_ascii_case(&binding.key)
        }) {
            return;
        }
        if self.search_overlay.visible {
            match key {
                "escape" => self.search_overlay.close(),
                "enter" | "return" => {
                    self.search.next(modifiers.shift);
                    self.search_overlay.current = self.search.current;
                }
                "backspace" => {
                    self.search_overlay.query.pop();
                    self.update_search();
                }
                "c" if modifiers.alt => {
                    self.search_overlay.options.case_sensitive =
                        !self.search_overlay.options.case_sensitive;
                    self.update_search();
                }
                _ => return,
            }
            cx.notify();
            return;
        }
        if is_copy_shortcut(&ev.keystroke) {
            if let Some(text) = self.term.selection_to_string() {
                // Key listeners run inside this entity's update; defer the clipboard
                // access so Windows clipboard message pumping cannot re-enter while
                // the RefCell is borrowed (same hazard as in handle_terminal_event).
                cx.defer(move |cx| cx.write_to_clipboard(ClipboardItem::new_string(text)));
            }
            return;
        }
        if is_paste_shortcut(&ev.keystroke) {
            let mode = *self.term.mode();
            let writer = self.bridge.writer();
            // Same deferral as the copy path above.
            cx.defer(move |cx| {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    let bytes = encode_paste(&text, mode);
                    if let Err(error) = writer.write_all(&bytes) {
                        log::warn!("PTY paste error: {error}");
                    }
                }
            });
            return;
        }
        let bytes = keystroke_to_pty_bytes(&ev.keystroke, *self.term.mode());
        if ev.keystroke.key_char.is_some()
            && !modifiers.control
            && !modifiers.alt
            && !modifiers.platform
        {
            // Committed text, including IME, is delivered through InputHandler.
            return;
        }
        if !bytes.is_empty() {
            if let Err(e) = self.bridge.writer().write_all(&bytes) {
                log::warn!("PTY write error: {e}");
            }
        }
    }

    fn update_search(&mut self) {
        self.search.query = self.search_overlay.query.clone();
        self.search.case_sensitive = self.search_overlay.options.case_sensitive;
        self.search.update(&self.snapshot_text);
        self.search_overlay.set_results(self.search.hits.len());
        self.search_overlay.current = self.search.current;
    }

    fn handle_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pixels = event
            .delta
            .pixel_delta(px(self.line_height_px.max(1.0)))
            .y
            .as_f32();
        if self.scroll_px != 0.0 && self.scroll_px.signum() != pixels.signum() {
            self.scroll_px = 0.0;
        }
        self.scroll_px += pixels;
        let lines = (self.scroll_px / self.line_height_px.max(1.0)).trunc() as i32;
        if lines == 0 {
            return;
        }
        self.scroll_px -= lines as f32 * self.line_height_px.max(1.0);

        let (point, _) = self.terminal_point_for_position(event.position);
        let mode = *self.term.mode();
        if mode.intersects(TermMode::MOUSE_MODE) && !event.modifiers.shift {
            let button = if lines > 0 { 64 } else { 65 };
            if let Some(report) = mouse_report(point, button, true, event.modifiers, mode) {
                for _ in 0..lines.unsigned_abs() {
                    let _ = self.write_input(&report);
                }
            }
        } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL)
            && !event.modifiers.shift
        {
            let bytes = alternate_scroll(lines);
            let _ = self.write_input(&bytes);
        } else {
            self.term.scroll_display(Scroll::Delta(lines));
            cx.notify();
        }
    }

    fn handle_focus_in(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.term.mode().contains(TermMode::FOCUS_IN_OUT) {
            let _ = self.write_input(b"\x1b[I");
        }
        cx.notify();
    }

    fn handle_focus_out(&mut self, cx: &mut Context<Self>) {
        if self.term.mode().contains(TermMode::FOCUS_IN_OUT) {
            let _ = self.write_input(b"\x1b[O");
        }
        cx.notify();
    }

    fn handle_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let (point, side) = self.terminal_point_for_position(event.position);
        let mode = *self.term.mode();
        if mode.intersects(TermMode::MOUSE_MODE) && !event.modifiers.shift {
            if let Some(button) = mouse_button_code(event.button, false) {
                if let Some(report) = mouse_report(point, button, true, event.modifiers, mode) {
                    let _ = self.write_input(&report);
                }
            }
        } else if event.button == MouseButton::Left {
            let selection_type = match event.click_count {
                2 => SelectionType::Semantic,
                3.. => SelectionType::Lines,
                _ => SelectionType::Simple,
            };
            self.term.selection = Some(Selection::new(selection_type, point, side));
            cx.notify();
        }
    }

    fn handle_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (point, side) = self.terminal_point_for_position(event.position);
        let mode = *self.term.mode();
        if mode.intersects(TermMode::MOUSE_MODE) && !event.modifiers.shift {
            let current = (point.line.0, point.column.0, event.pressed_button);
            if self.last_mouse_cell == Some(current) {
                return;
            }
            self.last_mouse_cell = Some(current);
            let should_report = mode.contains(TermMode::MOUSE_MOTION)
                || (mode.contains(TermMode::MOUSE_DRAG) && event.pressed_button.is_some());
            if should_report {
                if let Some(button) = mouse_motion_button_code(event.pressed_button) {
                    if let Some(report) = mouse_report(point, button, true, event.modifiers, mode) {
                        let _ = self.write_input(&report);
                    }
                }
            }
        } else if event.pressed_button == Some(MouseButton::Left) {
            if let Some(selection) = self.term.selection.as_mut() {
                selection.update(point, side);
                cx.notify();
            }
        }
    }

    fn handle_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.last_mouse_cell = None;
        let (point, _) = self.terminal_point_for_position(event.position);
        let mode = *self.term.mode();
        if mode.intersects(TermMode::MOUSE_MODE) && !event.modifiers.shift {
            if let Some(button) = mouse_button_code(event.button, false) {
                if let Some(report) = mouse_report(point, button, false, event.modifiers, mode) {
                    let _ = self.write_input(&report);
                }
            }
        } else if event.button == MouseButton::Left {
            cx.notify();
        }
    }

    fn terminal_point_for_position(&self, position: Point<Pixels>) -> (TerminalPoint, Side) {
        let relative_x = (position.x - self.viewport_origin.x).as_f32();
        let relative_y = (position.y - self.viewport_origin.y).as_f32();
        let raw_column = (relative_x.max(0.0) / self.cell_width.max(1.0)).floor() as usize;
        let column = raw_column.min(self.cols.saturating_sub(1));
        let cell_x = relative_x.max(0.0) % self.cell_width.max(1.0);
        let side = if cell_x >= self.cell_width.max(1.0) / 2.0 {
            Side::Right
        } else {
            Side::Left
        };
        let viewport_line = (relative_y.max(0.0) / self.line_height_px.max(1.0)).floor() as i32;
        let viewport_line = viewport_line.min(self.rows.saturating_sub(1) as i32);
        let display_offset = self.term.grid().display_offset().min(i32::MAX as usize) as i32;
        (
            TerminalPoint::new(Line(viewport_line - display_offset), Column(column)),
            side,
        )
    }
}

impl EventEmitter<AgentState> for TerminalView {}
impl EventEmitter<TerminalViewEvent> for TerminalView {}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bg = theme_color_to_hsla(self.palette.background);
        let fg = theme_color_to_hsla(self.palette.foreground);
        let focus = self.focus_handle.clone();
        let input_focus = focus.clone();
        if self.focus_in_subscription.is_none() {
            self.focus_in_subscription =
                Some(cx.on_focus_in(&focus, window, Self::handle_focus_in));
        }
        if self.focus_out_subscription.is_none() {
            self.focus_out_subscription = Some(cx.on_focus_out(
                &focus,
                window,
                |view, _event, _window, cx| view.handle_focus_out(cx),
            ));
        }
        let input_handler = TerminalInputHandler {
            view: cx.entity().downgrade(),
        };

        // 提前把 renderable content 收集成 owned 数据，避免 'static paint 闭包借用 &self.term。
        let content = self.term.renderable_content();
        let snapshot: RenderSnapshot = collect_snapshot(content);
        self.snapshot_text = snapshot_to_text(&snapshot);
        self.buffer_text = terminal_buffer_tail_to_text(&self.term, 300);
        self.update_search();
        let search = self.search.clone();
        let links = find_hyperlinks(&self.snapshot_text)
            .into_iter()
            .filter_map(|range| self.snapshot_text.get(range).map(str::to_owned))
            .collect::<Vec<_>>();
        let marked_text = self.marked_text.clone();
        let marked_col = snapshot.cursor.col;
        let marked_row = snapshot.cursor.row.max(0) as usize;
        let palette = self.palette.clone();
        let cols = self.cols;
        let rows = self.rows;
        let text_style = self.text_style.clone();
        let paint_style = text_style.clone();
        let view = cx.entity().downgrade();
        let marked_cell_width = self.cell_width;
        let marked_line_height = self.line_height_px;
        let current_cell_width = self.cell_width;
        let current_line_height = self.line_height_px;
        let current_origin = self.viewport_origin;

        div()
            .id("terminal-view")
            .track_focus(&focus)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::handle_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::handle_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::handle_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::handle_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::handle_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::handle_mouse_up))
            .on_mouse_move(cx.listener(Self::handle_mouse_move))
            .on_key_down(cx.listener(Self::handle_key_down))
            .on_scroll_wheel(cx.listener(Self::handle_scroll))
            .size_full()
            .bg(bg)
            .text_color(fg)
            .child(canvas(
                move |bounds, window, _cx| {
                    let line_height = px(text_style.font_size * text_style.line_height);
                    let cell_width = cell_advance_width(window, &text_style);
                    // 网格原点内移固定间隙，且可用宽高扣除两侧间隙，文字不再紧贴窗格边缘。
                    let origin = point(
                        bounds.origin.x + px(TERMINAL_PANE_PADDING),
                        bounds.origin.y + px(TERMINAL_PANE_PADDING),
                    );
                    let avail_width =
                        (bounds.size.width.as_f32() - 2.0 * TERMINAL_PANE_PADDING).max(0.0);
                    let avail_height =
                        (bounds.size.height.as_f32() - 2.0 * TERMINAL_PANE_PADDING).max(0.0);
                    let measured_cols = (avail_width / cell_width.max(1.0)).floor() as usize;
                    let measured_rows =
                        (avail_height / line_height.as_f32().max(1.0)).floor() as usize;
                    if measured_cols > 0
                        && measured_rows > 0
                        && (measured_cols != cols
                            || measured_rows != rows
                            || (current_cell_width - cell_width).abs() > f32::EPSILON
                            || (current_line_height - line_height.as_f32()).abs() > f32::EPSILON
                            || current_origin != origin)
                    {
                        let view = view.clone();
                        window.on_next_frame(move |_, cx| {
                            if let Some(view) = view.upgrade() {
                                view.update(cx, |view, cx| {
                                    view.resize(
                                        measured_cols,
                                        measured_rows,
                                        cell_width,
                                        line_height.as_f32(),
                                        origin,
                                        cx,
                                    );
                                });
                            }
                        });
                    }
                    LayoutInfo {
                        origin,
                        line_height,
                        cell_width,
                        cols: measured_cols.max(1),
                        rows: measured_rows.max(1),
                    }
                },
                move |_bounds, layout: LayoutInfo, window, cx| {
                    // Register the IME input handler only for the focused pane. With split
                    // panes, unconditional per-frame registrations from every pane fight
                    // each other, and on Windows each registration can trigger re-entrant
                    // IME/TSF (COM) message traffic. Zed's terminal element gates the same
                    // way (gpui's handle_input also checks focus internally on this rev).
                    if input_focus.is_focused(window) {
                        window.handle_input(&input_focus, input_handler, cx);
                    }
                    paint_terminal(
                        &layout,
                        &snapshot,
                        &palette,
                        &search,
                        &paint_style,
                        window,
                        cx,
                    );
                },
            ))
            .when(!marked_text.is_empty(), |element| {
                element.child(
                    div()
                        .absolute()
                        .left(px(TERMINAL_PANE_PADDING + marked_col as f32 * marked_cell_width + 2.0))
                        .top(px(TERMINAL_PANE_PADDING + marked_row as f32 * marked_line_height + 1.0))
                        .px_1()
                        .bg(crate::ui::alpha(self.palette.accent, 0.35))
                        .child(SharedString::from(marked_text)),
                )
            })
            .when(self.search_overlay.visible, |element| {
                element.child(
                    div()
                        .absolute()
                        .top(px(8.0))
                        .right(px(10.0))
                        .flex()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .border_1()
                        .border_color(crate::ui::color(self.palette.accent))
                        .bg(crate::ui::color(self.palette.overlay))
                        .text_color(crate::ui::color(self.palette.foreground))
                        .shadow_md()
                        .child(SharedString::from(format!(
                            "Find: {}▏",
                            self.search_overlay.query
                        )))
                        .child(SharedString::from(format!(
                            "{}/{} · {}",
                            if self.search_overlay.total == 0 {
                                0
                            } else {
                                self.search_overlay.current + 1
                            },
                            self.search_overlay.total,
                            if self.search_overlay.options.case_sensitive {
                                "Aa"
                            } else {
                                "aa"
                            }
                        ))),
                )
            })
            .when(!links.is_empty(), |element| {
                element.child(
                    div()
                        .absolute()
                        .bottom(px(6.0))
                        .left(px(6.0))
                        .flex()
                        .gap_1()
                        .children(links.into_iter().take(3).enumerate().map(|(index, url)| {
                            let target = url.clone();
                            div()
                                .id(SharedString::from(format!("terminal-link-{index}")))
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(crate::ui::alpha(self.palette.overlay, 0.92))
                                .border_1()
                                .border_color(crate::ui::border(&self.palette))
                                .text_color(crate::ui::color(self.palette.accent))
                                .cursor_pointer()
                                .text_xs()
                                .child(SharedString::from(url))
                                .on_mouse_down(gpui::MouseButton::Left, move |_, _, _| {
                                    let _ = termior_platform::open_external(&target);
                                })
                        })),
                )
            })
    }
}

#[derive(Clone)]
struct TerminalInputHandler {
    view: WeakEntity<TerminalView>,
}

impl InputHandler for TerminalInputHandler {
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(
        &mut self,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<std::ops::Range<usize>> {
        let view = self.view.upgrade()?;
        let length = view.read(cx).marked_text.encode_utf16().count();
        (length > 0).then_some(0..length)
    }

    fn text_for_range(
        &mut self,
        _range: std::ops::Range<usize>,
        _actual_range: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<String> {
        None
    }

    fn replace_text_in_range(
        &mut self,
        _replacement_range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(view) = self.view.upgrade() {
            view.update(cx, |view, cx| {
                if view.search_overlay.visible {
                    view.search_overlay.query.push_str(text);
                    view.update_search();
                    cx.notify();
                    return;
                }
                view.marked_text.clear();
                if let Err(error) = view.bridge.writer().write_all(text.as_bytes()) {
                    log::warn!("PTY IME write error: {error}");
                }
                cx.notify();
            });
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _replacement_range: Option<std::ops::Range<usize>>,
        new_text: &str,
        _new_marked_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(view) = self.view.upgrade() {
            view.update(cx, |view, cx| {
                view.marked_text = new_text.to_owned();
                cx.notify();
            });
        }
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut App) {
        if let Some(view) = self.view.upgrade() {
            view.update(cx, |view, cx| {
                view.marked_text.clear();
                cx.notify();
            });
        }
    }

    fn bounds_for_range(
        &mut self,
        _range: std::ops::Range<usize>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        None
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<usize> {
        None
    }
}

/// 渲染快照：owned 的 cells + 光标信息，可安全 move 进 'static paint 闭包。
struct RenderSnapshot {
    /// (row:i32, col:usize, cell) —— 每个可见 cell。
    cells: Vec<(i32, usize, CellSnapshot)>,
    cursor: CursorSnapshot,
    colors: Vec<Option<Rgb>>,
}

/// Cell 的 owned 快照；保留组合字符和样式标志，避免把 VTE 网格降级为纯字符矩阵。
struct CellSnapshot {
    text: String,
    fg: VteColor,
    bg: VteColor,
    underline_color: Option<VteColor>,
    flags: Flags,
    selected: bool,
}

/// 光标快照。
struct CursorSnapshot {
    shape: CursorShape,
    col: usize,
    row: i32,
}

/// 从 RenderableContent 收集 owned 快照（消耗 content，在 render 主线程完成）。
fn collect_snapshot(content: RenderableContent<'_>) -> RenderSnapshot {
    let RenderableContent {
        display_iter,
        selection,
        cursor,
        display_offset,
        colors,
        ..
    } = content;
    let dynamic_colors = (0..alacritty_terminal::term::color::COUNT)
        .map(|index| colors[index])
        .collect();
    let cursor_point = cursor.point;
    let cursor_shape = cursor.shape;
    let cells = display_iter
        .map(|indexed| {
            let cell = indexed.cell;
            let mut text = String::from(cell.c);
            if let Some(zerowidth) = cell.zerowidth() {
                text.extend(zerowidth);
            }
            let selected = selection.as_ref().is_some_and(|selection| {
                selection.contains_cell(&indexed, cursor_point, cursor_shape)
            });
            (
                indexed.point.line.0 + display_offset.min(i32::MAX as usize) as i32,
                indexed.point.column.0,
                CellSnapshot {
                    text,
                    fg: cell.fg,
                    bg: cell.bg,
                    underline_color: cell.underline_color(),
                    flags: cell.flags,
                    selected,
                },
            )
        })
        .collect();
    let cursor = CursorSnapshot {
        shape: if display_offset == 0 {
            cursor_shape
        } else {
            CursorShape::Hidden
        },
        col: cursor_point.column.0,
        row: cursor_point.line.0,
    };
    RenderSnapshot {
        cells,
        cursor,
        colors: dynamic_colors,
    }
}

fn snapshot_to_text(snapshot: &RenderSnapshot) -> String {
    let mut rows = std::collections::BTreeMap::<i32, Vec<String>>::new();
    for (row, column, cell) in &snapshot.cells {
        let line = rows.entry(*row).or_default();
        if line.len() <= *column {
            line.resize(*column + 1, " ".to_owned());
        }
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            line[*column].clear();
        } else {
            line[*column].clone_from(&cell.text);
        }
    }
    rows.into_values()
        .map(|line| line.concat().trim_end().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

fn terminal_buffer_tail_to_text<T>(term: &Term<T>, max_lines: usize) -> String {
    if max_lines == 0 {
        return String::new();
    }
    let bottom = term.bottommost_line().0;
    let requested_top = bottom.saturating_sub(max_lines.saturating_sub(1) as i32);
    let top = requested_top.max(term.topmost_line().0);
    let mut rows = Vec::with_capacity((bottom - top + 1).max(0) as usize);
    for line_index in top..=bottom {
        let line = &term.grid()[Line(line_index)];
        let mut text = String::new();
        for column in 0..term.columns() {
            let cell = &line[Column(column)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            text.push(cell.c);
            if let Some(zerowidth) = cell.zerowidth() {
                text.extend(zerowidth);
            }
        }
        rows.push(text.trim_end().to_owned());
    }
    rows.join("\n")
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// 等宽字体的单字 advance 宽度（cell 宽）。
#[derive(Debug, Clone)]
struct TerminalTextStyle {
    font_family: SharedString,
    font_size: f32,
    line_height: f32,
    letter_spacing: f32,
}

impl TerminalTextStyle {
    fn from_settings(settings: &TerminalSettings) -> Self {
        Self {
            font_family: settings.font_family.clone().into(),
            font_size: settings.font_size.max(6) as f32,
            line_height: settings.line_height.max(0.8),
            letter_spacing: settings.letter_spacing,
        }
    }
}

fn cell_advance_width(window: &Window, style: &TerminalTextStyle) -> f32 {
    let run = TextRun {
        len: 1,
        font: Font {
            family: style.font_family.clone(),
            weight: FontWeight::NORMAL,
            style: FontStyle::Normal,
            features: FontFeatures::default(),
            fallbacks: None,
        },
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line =
        window
            .text_system()
            .shape_line(SharedString::from("M"), px(style.font_size), &[run], None);
    (line.width().as_f32() + style.letter_spacing).max(1.0)
}

/// 布局计算结果，prepaint → paint 之间传递。
struct LayoutInfo {
    origin: Point<Pixels>,
    line_height: Pixels,
    cell_width: f32,
    cols: usize,
    rows: usize,
}

/// 绘制终端：逐 cell 背景 + 按颜色分段的前景文本 run + 光标。
fn paint_terminal(
    layout: &LayoutInfo,
    snapshot: &RenderSnapshot,
    palette: &ResolvedPalette,
    search: &TerminalSearch,
    text_style: &TerminalTextStyle,
    window: &mut Window,
    cx: &mut App,
) {
    let default_bg = theme_color_to_hsla(palette.background);

    let lh = layout.line_height;
    let cw = layout.cell_width;
    let origin = layout.origin;
    // 1) 整体刷背景
    let total_bounds = Bounds {
        origin,
        size: gpui::size(px(layout.cols as f32 * cw), lh * (layout.rows as f32)),
    };
    window.paint_quad(fill(total_bounds, default_bg));

    // 2) 逐 cell：背景单独绘制；文本按连续同样式 run 合并。宽字符占两格，但它的
    // spacer 不进入文本 run，组合字符则跟随主字符一起交给 shaping。
    let mut cur_row: i32 = if let Some(first) = snapshot.cells.first() {
        first.0
    } else {
        paint_cursor(&snapshot.cursor, palette, origin, lh, cw, window);
        return;
    };
    let mut run_text = String::new();
    let mut run_style: Option<CellTextStyle> = None;
    let mut run_start_col: usize = 0;
    let mut run_next_col: usize = 0;

    for (row, col, cell) in &snapshot.cells {
        // 换行：flush 上一段
        if *row != cur_row {
            if let Some(style) = run_style.as_ref() {
                flush_run(
                    &mut run_text,
                    style,
                    run_start_col,
                    cur_row,
                    origin,
                    lh,
                    cw,
                    text_style,
                    window,
                    cx,
                );
            }
            cur_row = *row;
            run_start_col = *col;
            run_next_col = *col;
            run_style = None;
        }

        let mut cell_bg = resolve_color(cell.bg, palette, &snapshot.colors);
        let mut cell_fg = resolve_color(cell.fg, palette, &snapshot.colors);
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut cell_fg, &mut cell_bg);
        }
        if cell.selected {
            std::mem::swap(&mut cell_fg, &mut cell_bg);
        }
        if cell.flags.contains(Flags::DIM) {
            cell_fg = cell_fg.opacity(0.7);
        }

        // 非默认背景：画 cell 背景块
        if cell_bg != default_bg {
            let cb = Bounds {
                origin: point(origin.x + px(*col as f32 * cw), origin.y + lh * *row as f32),
                size: gpui::size(px(cw), lh),
            };
            window.paint_quad(fill(cb, cell_bg));
        }

        let search_line = (*row).max(0) as usize;
        if let Some((hit_index, _)) = search
            .hits
            .iter()
            .enumerate()
            .find(|(_, hit)| hit.line == search_line && hit.char_range.contains(col))
        {
            let bounds = Bounds {
                origin: point(origin.x + px(*col as f32 * cw), origin.y + lh * *row as f32),
                size: gpui::size(px(cw), lh),
            };
            let color = if hit_index == search.current {
                gpui::hsla(0.10, 0.85, 0.55, 0.75)
            } else {
                gpui::hsla(0.12, 0.70, 0.45, 0.45)
            };
            window.paint_quad(fill(bounds, color));
        }

        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }

        let hidden = cell.flags.contains(Flags::HIDDEN);
        let underline = cell
            .flags
            .intersects(Flags::ALL_UNDERLINES)
            .then(|| UnderlineStyle {
                thickness: px(1.0),
                color: Some(
                    cell.underline_color
                        .map(|color| resolve_color(color, palette, &snapshot.colors))
                        .unwrap_or(cell_fg),
                ),
                wavy: cell.flags.contains(Flags::UNDERCURL),
            });
        let strikethrough = cell
            .flags
            .contains(Flags::STRIKEOUT)
            .then(|| StrikethroughStyle {
                thickness: px(1.0),
                color: Some(cell_fg),
            });
        let style = CellTextStyle {
            fg: cell_fg,
            weight: if cell.flags.contains(Flags::BOLD) {
                FontWeight::BOLD
            } else {
                FontWeight::NORMAL
            },
            font_style: if cell.flags.contains(Flags::ITALIC) {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
            underline,
            strikethrough,
        };
        let cell_width = if cell.flags.contains(Flags::WIDE_CHAR) {
            2
        } else {
            1
        };
        let append = !hidden && run_style.as_ref() == Some(&style) && *col == run_next_col;
        if !append {
            if let Some(previous_style) = run_style.as_ref() {
                flush_run(
                    &mut run_text,
                    previous_style,
                    run_start_col,
                    cur_row,
                    origin,
                    lh,
                    cw,
                    text_style,
                    window,
                    cx,
                );
            }
            run_start_col = *col;
            run_style = (!hidden).then_some(style);
        }
        if !hidden {
            run_text.push_str(&cell.text);
        }
        run_next_col = col.saturating_add(cell_width);
    }
    if let Some(style) = run_style.as_ref() {
        flush_run(
            &mut run_text,
            style,
            run_start_col,
            cur_row,
            origin,
            lh,
            cw,
            text_style,
            window,
            cx,
        );
    }

    // 3) 光标块
    paint_cursor(&snapshot.cursor, palette, origin, lh, cw, window);
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CellTextStyle {
    fg: Hsla,
    weight: FontWeight,
    font_style: FontStyle,
    underline: Option<UnderlineStyle>,
    strikethrough: Option<StrikethroughStyle>,
}

/// flush 一段同色文本：shape 成 ShapedLine 后 paint。
#[allow(clippy::too_many_arguments)]
fn flush_run(
    text: &mut String,
    style: &CellTextStyle,
    start_col: usize,
    row: i32,
    origin: Point<Pixels>,
    lh: Pixels,
    cw: f32,
    text_style: &TerminalTextStyle,
    window: &mut Window,
    cx: &mut App,
) {
    if text.is_empty() {
        return;
    }
    if text.chars().all(char::is_whitespace)
        && style.underline.is_none()
        && style.strikethrough.is_none()
    {
        text.clear();
        return;
    }
    let len = text.len();
    let shared = SharedString::from(std::mem::take(text));
    let run = TextRun {
        len,
        font: Font {
            family: text_style.font_family.clone(),
            weight: style.weight,
            style: style.font_style,
            features: FontFeatures::default(),
            fallbacks: None,
        },
        color: style.fg,
        background_color: None,
        underline: style.underline,
        strikethrough: style.strikethrough,
    };
    let line = window
        .text_system()
        .shape_line(shared, px(text_style.font_size), &[run], None);
    let line_x = origin.x + px(start_col as f32 * cw);
    let baseline_y = origin.y + lh * row as f32;
    let _ = line.paint(
        point(line_x, baseline_y),
        lh,
        TextAlign::Left,
        None,
        window,
        cx,
    );
}

/// 绘制光标。
fn paint_cursor(
    cursor: &CursorSnapshot,
    palette: &ResolvedPalette,
    origin: Point<Pixels>,
    lh: Pixels,
    cw: f32,
    window: &mut Window,
) {
    let col = cursor.col as f32;
    let row = cursor.row as f32;
    let cell_origin = point(origin.x + px(col * cw), origin.y + lh * row);
    let cell_bounds = Bounds {
        origin: cell_origin,
        size: gpui::size(px(cw), lh),
    };
    let color = theme_color_to_hsla(palette.foreground);
    match cursor.shape {
        CursorShape::Block => {
            // Keep the glyph legible without a second text pass by using a translucent block.
            window.paint_quad(fill(cell_bounds, color.opacity(0.55)));
        }
        CursorShape::HollowBlock => {
            let thickness = px(1.0);
            window.paint_quad(fill(
                Bounds {
                    origin: cell_origin,
                    size: gpui::size(px(cw), thickness),
                },
                color,
            ));
            window.paint_quad(fill(
                Bounds {
                    origin: point(cell_origin.x, cell_origin.y + lh - thickness),
                    size: gpui::size(px(cw), thickness),
                },
                color,
            ));
            window.paint_quad(fill(
                Bounds {
                    origin: cell_origin,
                    size: gpui::size(thickness, lh),
                },
                color,
            ));
            window.paint_quad(fill(
                Bounds {
                    origin: point(cell_origin.x + px(cw) - thickness, cell_origin.y),
                    size: gpui::size(thickness, lh),
                },
                color,
            ));
        }
        CursorShape::Underline => {
            let b = Bounds {
                origin: point(cell_origin.x, cell_origin.y + lh - px(2.0)),
                size: gpui::size(px(cw), px(2.0)),
            };
            window.paint_quad(fill(b, color));
        }
        CursorShape::Beam => {
            let b = Bounds {
                origin: cell_origin,
                size: gpui::size(px(2.0), lh),
            };
            window.paint_quad(fill(b, color));
        }
        CursorShape::Hidden => {}
    }
}

fn resolve_color(c: VteColor, palette: &ResolvedPalette, colors: &[Option<Rgb>]) -> Hsla {
    match c {
        VteColor::Spec(Rgb { r, g, b }) => theme_color_to_hsla(ThemeColor { r, g, b }),
        VteColor::Named(named) => {
            if let Some(Some(Rgb { r, g, b })) = colors.get(named as usize) {
                theme_color_to_hsla(ThemeColor {
                    r: *r,
                    g: *g,
                    b: *b,
                })
            } else if matches!(named, NamedColor::Foreground) {
                theme_color_to_hsla(palette.foreground)
            } else if matches!(named, NamedColor::Background) {
                theme_color_to_hsla(palette.background)
            } else {
                named_to_theme(named, &palette.terminal)
                    .map(theme_color_to_hsla)
                    .unwrap_or_else(|| theme_color_to_hsla(palette.foreground))
            }
        }
        VteColor::Indexed(idx) => {
            if let Some(Some(Rgb { r, g, b })) = colors.get(idx as usize) {
                theme_color_to_hsla(ThemeColor {
                    r: *r,
                    g: *g,
                    b: *b,
                })
            } else {
                indexed_to_theme(idx, &palette.terminal)
                    .map(theme_color_to_hsla)
                    .unwrap_or_else(|| theme_color_to_hsla(palette.foreground))
            }
        }
    }
}

/// 命名色（0-15）映射到主题 16 色调色板。
fn named_to_theme(named: NamedColor, pal: &TerminalPalette) -> Option<ThemeColor> {
    use NamedColor::*;
    Some(match named {
        Black => pal.black,
        Red => pal.red,
        Green => pal.green,
        Yellow => pal.yellow,
        Blue => pal.blue,
        Magenta => pal.magenta,
        Cyan => pal.cyan,
        White => pal.white,
        BrightBlack => pal.bright_black,
        BrightRed => pal.bright_red,
        BrightGreen => pal.bright_green,
        BrightYellow => pal.bright_yellow,
        BrightBlue => pal.bright_blue,
        BrightMagenta => pal.bright_magenta,
        BrightCyan => pal.bright_cyan,
        BrightWhite => pal.bright_white,
        _ => return None,
    })
}

/// 256 色索引：16-231 立方体、232-255 灰度。
fn indexed_to_theme(idx: u8, pal: &TerminalPalette) -> Option<ThemeColor> {
    if idx < 16 {
        return Some(pal.all()[idx as usize]);
    }
    if idx >= 232 {
        let g = 8 + (idx - 232) * 10;
        return Some(ThemeColor { r: g, g, b: g });
    }
    let i = (idx - 16) as u32;
    let levels = [0u8, 95, 135, 175, 215, 255];
    let r = levels[(i / 36) as usize % 6];
    let g = levels[((i / 6) % 6) as usize];
    let b = levels[(i % 6) as usize];
    Some(ThemeColor { r, g, b })
}

fn terminal_rgb_for_index(index: usize, palette: &ResolvedPalette) -> Rgb {
    let ansi = palette.terminal.all();
    let color = match index {
        0..=15 => ansi[index],
        16..=255 => indexed_to_theme(index as u8, &palette.terminal).unwrap_or(palette.foreground),
        256 => palette.foreground,
        257 => palette.background,
        258 => palette.foreground,
        259..=266 => dim_color(ansi[index - 259]),
        267 => palette.foreground,
        268 => dim_color(palette.foreground),
        _ => palette.foreground,
    };
    Rgb {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

fn is_copy_shortcut(keystroke: &gpui::Keystroke) -> bool {
    let key = keystroke.key.to_ascii_lowercase();
    let modifiers = keystroke.modifiers;
    if cfg!(target_os = "macos") {
        modifiers.platform && !modifiers.control && !modifiers.alt && key == "c"
    } else {
        (modifiers.control && modifiers.shift && !modifiers.alt && key == "c")
            || (modifiers.control && !modifiers.shift && key == "insert")
    }
}

fn is_paste_shortcut(keystroke: &gpui::Keystroke) -> bool {
    let key = keystroke.key.to_ascii_lowercase();
    let modifiers = keystroke.modifiers;
    if cfg!(target_os = "macos") {
        modifiers.platform && !modifiers.control && !modifiers.alt && key == "v"
    } else {
        (modifiers.control && modifiers.shift && !modifiers.alt && key == "v")
            || (!modifiers.control && modifiers.shift && key == "insert")
    }
}

fn mouse_button_code(button: MouseButton, motion: bool) -> Option<u8> {
    match (button, motion) {
        (MouseButton::Left, false) => Some(0),
        (MouseButton::Middle, false) => Some(1),
        (MouseButton::Right, false) => Some(2),
        (MouseButton::Left, true) => Some(32),
        (MouseButton::Middle, true) => Some(33),
        (MouseButton::Right, true) => Some(34),
        (MouseButton::Navigate(_), _) => None,
    }
}

fn mouse_motion_button_code(button: Option<MouseButton>) -> Option<u8> {
    match button {
        Some(button) => mouse_button_code(button, true),
        None => Some(35),
    }
}

fn mouse_report(
    point: TerminalPoint,
    button: u8,
    pressed: bool,
    modifiers: Modifiers,
    mode: TermMode,
) -> Option<Vec<u8>> {
    if point.line.0 < 0 {
        return None;
    }
    let mut modifier_bits = 0;
    if modifiers.shift {
        modifier_bits |= 4;
    }
    if modifiers.alt {
        modifier_bits |= 8;
    }
    if modifiers.control {
        modifier_bits |= 16;
    }
    let button = button.saturating_add(modifier_bits);
    if mode.contains(TermMode::SGR_MOUSE) {
        let terminator = if pressed { 'M' } else { 'm' };
        return Some(
            format!(
                "\x1b[<{};{};{}{}",
                button,
                point.column.0 + 1,
                point.line.0 + 1,
                terminator
            )
            .into_bytes(),
        );
    }

    let utf8 = mode.contains(TermMode::UTF8_MOUSE);
    let max_coordinate = if utf8 { 2015 } else { 223 };
    if point.line.0 >= max_coordinate || point.column.0 >= max_coordinate as usize {
        return None;
    }
    let reported_button = if pressed { button } else { 3 + modifier_bits };
    let mut report = vec![b'\x1b', b'[', b'M', 32 + reported_button];
    append_mouse_coordinate(&mut report, point.column.0, utf8);
    append_mouse_coordinate(&mut report, point.line.0 as usize, utf8);
    Some(report)
}

fn append_mouse_coordinate(report: &mut Vec<u8>, coordinate: usize, utf8: bool) {
    let encoded = 33 + coordinate;
    if utf8 && coordinate >= 95 {
        report.push((0xc0 + encoded / 64) as u8);
        report.push((0x80 + encoded % 64) as u8);
    } else {
        report.push(encoded as u8);
    }
}

fn alternate_scroll(lines: i32) -> Vec<u8> {
    let final_character = if lines > 0 { b'A' } else { b'B' };
    let mut bytes = Vec::with_capacity(lines.unsigned_abs() as usize * 3);
    for _ in 0..lines.unsigned_abs() {
        bytes.extend_from_slice(&[b'\x1b', b'O', final_character]);
    }
    bytes
}

fn clean_terminal_title(title: String) -> Option<String> {
    let title: String = title
        .chars()
        .filter(|character| !character.is_control())
        .take(120)
        .collect();
    let title = title.trim();
    (!title.is_empty()).then(|| title.to_owned())
}

/// 计算标签页展示标题：
/// - shell/程序设置的标题若不像路径（如 vim/ssh 设置的程序名），原样采用；
/// - 路径式标题（如 PowerShell 启动时设置的全路径 `C:\...\pwsh.exe`）退化为
///   当前目录名（OSC 7 上报的 cwd）；
/// - 还没有 cwd 时退化为路径标题的程序名（如 `pwsh`）。
fn display_title(shell_title: Option<&str>, cwd: Option<&str>) -> Option<String> {
    if let Some(title) = shell_title {
        if !looks_like_path(title) {
            return Some(title.to_owned());
        }
    }
    if let Some(dir) = cwd.and_then(dir_name) {
        return Some(dir);
    }
    shell_title.and_then(program_name_from_path)
}

/// 标题是否形如文件系统路径（`C:\...`、`/...`、`~/...`、`\\server\...`）。
fn looks_like_path(title: &str) -> bool {
    let title = title.trim_start();
    if title.starts_with('/') || title.starts_with('~') || title.starts_with("\\\\") {
        return true;
    }
    let bytes = title.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// 路径的最后一个非空分量（目录名 / 文件名）。
fn last_path_component(path: &str) -> Option<&str> {
    path.trim()
        .rsplit(['/', '\\'])
        .find(|component| !component.is_empty())
}

/// cwd → 目录名（`C:\Users\willmove` → `willmove`；根目录退回自身，如 `C:` / `/`）。
fn dir_name(cwd: &str) -> Option<String> {
    let cwd = cwd.trim();
    if cwd.is_empty() {
        return None;
    }
    Some(
        last_path_component(cwd)
            .unwrap_or(cwd)
            .trim_end_matches(['/', '\\'])
            .to_owned(),
    )
    .filter(|name| !name.is_empty())
    .or_else(|| Some(cwd.to_owned()))
}

/// 路径标题 → 程序名（去目录与扩展名：`C:\Tools\pwsh.exe` → `pwsh`）。
fn program_name_from_path(path: &str) -> Option<String> {
    let name = last_path_component(path)?;
    let stem = match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
    };
    Some(stem.to_owned())
}

fn dim_color(color: ThemeColor) -> ThemeColor {
    ThemeColor {
        r: color.r / 2,
        g: color.g / 2,
        b: color.b / 2,
    }
}

/// termior Color → GPUI Hsla（打包 RGB 为 u32，不透明）。
fn theme_color_to_hsla(c: ThemeColor) -> Hsla {
    let hex = ((c.r as u32) << 16) | ((c.g as u32) << 8) | (c.b as u32);
    gpui::rgb(hex).into()
}

/// 给 Term 构造/resize 用的尺寸适配。
struct TermSize {
    columns: usize,
    screen_lines: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;

    fn parsed_term(input: &str, columns: usize, screen_lines: usize) -> Term<VoidListener> {
        let size = TermSize {
            columns,
            screen_lines,
        };
        let mut term = Term::new(TermConfig::default(), &size, VoidListener);
        let mut processor = VteProcessor::<StdSyncHandler>::default();
        processor.advance(&mut term, input.as_bytes());
        term
    }

    #[test]
    fn sgr_mouse_reports_are_one_based_and_preserve_release() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        let point = TerminalPoint::new(Line(2), Column(4));
        assert_eq!(
            mouse_report(point, 0, true, Modifiers::none(), mode).unwrap(),
            b"\x1b[<0;5;3M"
        );
        assert_eq!(
            mouse_report(point, 0, false, Modifiers::none(), mode).unwrap(),
            b"\x1b[<0;5;3m"
        );
    }

    #[test]
    fn alternate_scroll_uses_application_cursor_sequences() {
        assert_eq!(alternate_scroll(2), b"\x1bOA\x1bOA");
        assert_eq!(alternate_scroll(-1), b"\x1bOB");
    }

    #[test]
    fn display_title_prefers_program_titles_over_paths() {
        // 程序设置的标题（不含路径形态）原样采用。
        assert_eq!(
            display_title(Some("vim - main.rs"), Some("C:\\Users\\willmove")),
            Some("vim - main.rs".to_owned())
        );
        // PowerShell 启动时把标题设为全路径：优先展示当前目录名。
        assert_eq!(
            display_title(
                Some("C:\\Program Files\\PowerShell\\7\\pwsh.exe"),
                Some("C:\\Users\\willmove")
            ),
            Some("willmove".to_owned())
        );
        // 还没有 cwd 时退化为程序名（去目录与扩展名）。
        assert_eq!(
            display_title(Some("C:\\Program Files\\PowerShell\\7\\pwsh.exe"), None),
            Some("pwsh".to_owned())
        );
        // Unix 风格同样处理。
        assert_eq!(
            display_title(Some("/usr/bin/bash"), Some("/home/u/proj")),
            Some("proj".to_owned())
        );
        assert_eq!(
            display_title(Some("/usr/bin/fish"), None),
            Some("fish".to_owned())
        );
        // 无标题时用目录名；都没有则 None（上层回退 "Terminal"）。
        assert_eq!(
            display_title(None, Some("~/work/termior")),
            Some("termior".to_owned())
        );
        assert_eq!(display_title(None, None), None);
    }

    #[test]
    fn dir_name_handles_roots_and_separators() {
        assert_eq!(dir_name("C:\\Users\\willmove"), Some("willmove".to_owned()));
        assert_eq!(dir_name("C:/Users/willmove/"), Some("willmove".to_owned()));
        assert_eq!(dir_name("C:\\"), Some("C:".to_owned()));
        assert_eq!(dir_name("/"), Some("/".to_owned()));
        assert_eq!(dir_name("   "), None);
    }

    #[test]
    fn render_snapshot_preserves_wide_combining_and_style_cells() {
        let term = parsed_term(
            "\x1b[1m中\x1b[22me\u{301}\x1b[3mI\x1b[23;4mU\x1b[24;7mR",
            12,
            2,
        );
        let snapshot = collect_snapshot(term.renderable_content());

        let wide = &snapshot
            .cells
            .iter()
            .find(|(_, column, _)| *column == 0)
            .expect("wide cell")
            .2;
        assert!(wide.flags.contains(Flags::WIDE_CHAR));
        assert!(wide.flags.contains(Flags::BOLD));

        let spacer = &snapshot
            .cells
            .iter()
            .find(|(_, column, _)| *column == 1)
            .expect("wide spacer")
            .2;
        assert!(spacer.flags.contains(Flags::WIDE_CHAR_SPACER));

        let combining = &snapshot
            .cells
            .iter()
            .find(|(_, column, _)| *column == 2)
            .expect("combining cell")
            .2;
        assert_eq!(combining.text, "e\u{301}");
        assert!(snapshot
            .cells
            .iter()
            .any(|(_, _, cell)| cell.flags.contains(Flags::ITALIC)));
        assert!(snapshot
            .cells
            .iter()
            .any(|(_, _, cell)| cell.flags.contains(Flags::UNDERLINE)));
        assert!(snapshot
            .cells
            .iter()
            .any(|(_, _, cell)| cell.flags.contains(Flags::INVERSE)));
        let text = snapshot_to_text(&snapshot);
        assert!(text.starts_with("中e\u{301}IUR"), "snapshot text: {text:?}");
    }

    #[test]
    fn scrolled_history_is_mapped_back_into_viewport_rows() {
        let mut term = parsed_term("one\r\ntwo\r\nthree", 8, 2);
        term.scroll_display(Scroll::Delta(1));
        let snapshot = collect_snapshot(term.renderable_content());

        assert_eq!(snapshot.cursor.shape, CursorShape::Hidden);
        assert!(snapshot
            .cells
            .iter()
            .all(|(row, _, _)| (0..2).contains(row)));
        assert!(snapshot_to_text(&snapshot).starts_with("one\ntwo"));
    }

    #[test]
    fn buffer_tail_includes_scrollback_without_wide_spacers() {
        let term = parsed_term("一\r\ntwo\r\nthree\r\nfour", 8, 2);
        let tail = terminal_buffer_tail_to_text(&term, 3);

        assert_eq!(tail, "two\nthree\nfour");
        assert!(!tail.contains(' '));
    }
}

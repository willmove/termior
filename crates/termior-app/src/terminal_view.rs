//! `TerminalView` — GPUI 终端视图（FR-TERM-08 渲染 / FR-TERM-01 键盘）。
//!
//! 持有 alacritty `Term` 网格、vte `Processor`、`OscParser`，消费 PTY reader 线程的字节流。
//! 渲染用 canvas 逐 cell 绘制（背景 paint_quad + 前景文本 run），键盘经 keystroke 映射写回 PTY。
//! OSC 7/133/777 经 OscParser 旁路嗅探，M1 先 log。

use alacritty_terminal::{
    event::VoidListener,
    grid::Dimensions,
    term::{Config as TermConfig, RenderableContent, Term},
    vte::ansi::{
        Color as VteColor, CursorShape, NamedColor, Processor as VteProcessor, Rgb, StdSyncHandler,
    },
};
use futures::StreamExt;
use gpui::{
    canvas, div, fill, point, px, App, Bounds, Context, FocusHandle, Focusable, Font, FontFeatures,
    FontStyle, FontWeight, Hsla, InteractiveElement, IntoElement, KeyDownEvent, ParentElement,
    Pixels, Point, Render, SharedString, Styled, Task, TextAlign, TextRun, Window,
};
use termior_terminal::{PtySessionConfig, TerminalBridge};
use termior_terminal_core::OscParser;
use termior_theme::{Color as ThemeColor, ResolvedPalette, TerminalPalette};

use crate::keystroke::keystroke_to_pty_bytes;

/// 默认等宽字体族与字号。
const FONT_FAMILY: &str = "monospace";
const FONT_SIZE: f32 = 14.0;

/// GPUI 终端视图。
pub struct TerminalView {
    bridge: TerminalBridge,
    term: Term<VoidListener>,
    vte_processor: VteProcessor<StdSyncHandler>,
    osc_parser: OscParser,
    palette: ResolvedPalette,
    focus_handle: FocusHandle,
    /// PTY 字节消费任务（持有以避免被取消）。
    _consumer: Task<()>,
    cols: usize,
    rows: usize,
}

impl TerminalView {
    /// 创建终端视图：spawn PTY、启动字节消费循环。
    pub fn new(palette: ResolvedPalette, cx: &mut Context<Self>) -> Self {
        let config = PtySessionConfig::default();
        let mut bridge = TerminalBridge::spawn(&config).expect("PTY spawn failed");
        let output_rx = bridge.take_output().expect("output channel");

        let cols = config.cols as usize;
        let rows = config.rows as usize;
        let term = Term::new(
            TermConfig::default(),
            &TermSize {
                columns: cols,
                screen_lines: rows,
            },
            VoidListener,
        );

        let focus_handle = cx.focus_handle();

        // 消费循环：PTY 字节 → vte 解析更新 Term + OscParser 旁路 → cx.notify 重绘。
        // Term 非 Send，只能在主线程访问，故字节通过 channel 送回主线程处理。
        let consumer = cx.spawn(async move |this, cx| {
            let mut rx = output_rx;
            while let Some(data) = rx.next().await {
                let bytes = data.bytes;
                let _ = this.update(cx, |view, cx| {
                    view.vte_processor.advance(&mut view.term, &bytes);
                    for ev in view.osc_parser.feed(&bytes) {
                        log::debug!("OSC event: {ev:?}");
                    }
                    cx.notify();
                });
            }
            log::debug!("PTY output stream ended");
        });

        Self {
            bridge,
            term,
            vte_processor: VteProcessor::<StdSyncHandler>::default(),
            osc_parser: OscParser::new(),
            palette,
            focus_handle,
            _consumer: consumer,
            cols,
            rows,
        }
    }

    /// resize 终端网格与 PTY（窗口尺寸变化时调用；M1 预留，后续接布局事件）。
    #[allow(dead_code)]
    pub fn resize(&mut self, cols: usize, rows: usize, cx: &mut Context<Self>) {
        if cols == 0 || rows == 0 || (cols == self.cols && rows == self.rows) {
            return;
        }
        self.term.resize(TermSize {
            columns: cols,
            screen_lines: rows,
        });
        let _ = self.bridge.resize(rows as u16, cols as u16);
        self.cols = cols;
        self.rows = rows;
        cx.notify();
    }

    /// 处理键盘输入：编码成 PTY 字节写回。
    fn handle_key_down(
        &mut self,
        ev: &KeyDownEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        let bytes = keystroke_to_pty_bytes(&ev.keystroke);
        if !bytes.is_empty() {
            if let Err(e) = self.bridge.writer().write_all(&bytes) {
                log::warn!("PTY write error: {e}");
            }
        }
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bg = theme_color_to_hsla(self.palette.background);
        let fg = theme_color_to_hsla(self.palette.foreground);
        let focus = self.focus_handle.clone();

        // 提前把 renderable content 收集成 owned 数据，避免 'static paint 闭包借用 &self.term。
        let content = self.term.renderable_content();
        let snapshot: RenderSnapshot = collect_snapshot(content);
        let palette = self.palette.clone();
        let cols = self.cols;
        let rows = self.rows;

        div()
            .id("terminal-view")
            .track_focus(&focus)
            .on_key_down(cx.listener(Self::handle_key_down))
            .size_full()
            .bg(bg)
            .text_color(fg)
            .child(canvas(
                move |bounds, window, _cx| {
                    let line_height = window.line_height();
                    let cell_width = cell_advance_width(window);
                    LayoutInfo {
                        origin: bounds.origin,
                        line_height,
                        cell_width,
                        cols,
                        rows,
                    }
                },
                move |_bounds, layout: LayoutInfo, window, cx| {
                    paint_terminal(&layout, &snapshot, &palette, window, cx);
                },
            ))
    }
}

/// 渲染快照：owned 的 cells + 光标信息，可安全 move 进 'static paint 闭包。
struct RenderSnapshot {
    /// (row:i32, col:usize, cell) —— 每个可见 cell。
    cells: Vec<(i32, usize, CellSnapshot)>,
    cursor: CursorSnapshot,
}

/// cell 的 owned 快照（Cell 含 Arc，但 c/fg/bg/flags 足够渲染）。
struct CellSnapshot {
    c: char,
    fg: VteColor,
    bg: VteColor,
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
        cursor,
        ..
    } = content;
    let cells = display_iter
        .map(|indexed| {
            let cell = &indexed.cell;
            (
                indexed.point.line.0,
                indexed.point.column.0,
                CellSnapshot {
                    c: cell.c,
                    fg: cell.fg,
                    bg: cell.bg,
                },
            )
        })
        .collect();
    let cursor = CursorSnapshot {
        shape: cursor.shape,
        col: cursor.point.column.0,
        row: cursor.point.line.0,
    };
    RenderSnapshot { cells, cursor }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// 等宽字体的单字 advance 宽度（cell 宽）。
fn cell_advance_width(window: &Window) -> f32 {
    let run = TextRun {
        len: 1,
        font: Font {
            family: FONT_FAMILY.into(),
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
            .shape_line(SharedString::from("M"), px(FONT_SIZE), &[run], None);
    line.width().as_f32()
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
    window: &mut Window,
    cx: &mut App,
) {
    let default_fg = theme_color_to_hsla(palette.foreground);
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

    // 2) 逐 cell：背景非默认的画 quad，前景按「连续相同 fg+bg」分段 shape+paint。
    let mut cur_row: i32 = if let Some(first) = snapshot.cells.first() {
        first.0
    } else {
        paint_cursor(&snapshot.cursor, palette, origin, lh, cw, window);
        return;
    };
    let mut run_chars: Vec<char> = Vec::new();
    let mut run_fg: Hsla = default_fg;
    let mut run_bg: Hsla = default_bg;
    let mut run_start_col: usize = 0;

    for (row, col, cell) in &snapshot.cells {
        // 换行：flush 上一段
        if *row != cur_row {
            flush_run(
                &mut run_chars,
                run_fg,
                run_start_col,
                cur_row,
                origin,
                lh,
                cw,
                window,
                cx,
            );
            cur_row = *row;
            run_start_col = *col;
            run_fg = default_fg;
            run_bg = default_bg;
        }

        let cell_bg = resolve_color_simple(cell.bg, palette);
        let cell_fg = resolve_color_simple(cell.fg, palette);

        // 非默认背景：画 cell 背景块
        if cell_bg != default_bg {
            let cb = Bounds {
                origin: point(origin.x + px(*col as f32 * cw), origin.y + lh * *row as f32),
                size: gpui::size(px(cw), lh),
            };
            window.paint_quad(fill(cb, cell_bg));
        }

        // 分段：fg/bg 变化时 flush
        if cell_fg == run_fg && cell_bg == run_bg {
            run_chars.push(cell.c);
        } else {
            flush_run(
                &mut run_chars,
                run_fg,
                run_start_col,
                cur_row,
                origin,
                lh,
                cw,
                window,
                cx,
            );
            run_fg = cell_fg;
            run_bg = cell_bg;
            run_start_col = *col;
            run_chars.push(cell.c);
        }
    }
    flush_run(
        &mut run_chars,
        run_fg,
        run_start_col,
        cur_row,
        origin,
        lh,
        cw,
        window,
        cx,
    );

    // 3) 光标块
    paint_cursor(&snapshot.cursor, palette, origin, lh, cw, window);
}

/// flush 一段同色文本：shape 成 ShapedLine 后 paint。
#[allow(clippy::too_many_arguments)]
fn flush_run(
    chars: &mut Vec<char>,
    fg: Hsla,
    start_col: usize,
    row: i32,
    origin: Point<Pixels>,
    lh: Pixels,
    cw: f32,
    window: &mut Window,
    cx: &mut App,
) {
    if chars.is_empty() {
        return;
    }
    let text: String = chars.iter().collect();
    chars.clear();
    if !text.chars().any(|c| !c.is_whitespace()) {
        return;
    }
    let len = text.len();
    let shared = SharedString::from(text);
    let run = TextRun {
        len,
        font: Font {
            family: FONT_FAMILY.into(),
            weight: FontWeight::NORMAL,
            style: FontStyle::Normal,
            features: FontFeatures::default(),
            fallbacks: None,
        },
        color: fg,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(shared, px(FONT_SIZE), &[run], None);
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
        CursorShape::Block | CursorShape::HollowBlock => {
            window.paint_quad(fill(cell_bounds, color));
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

/// 简化颜色解析（不查 term.colors 表，仅用主题调色板；M1 足够）。
fn resolve_color_simple(c: VteColor, palette: &ResolvedPalette) -> Hsla {
    match c {
        VteColor::Spec(Rgb { r, g, b }) => theme_color_to_hsla(ThemeColor { r, g, b }),
        VteColor::Named(named) => {
            if matches!(named, NamedColor::Foreground) {
                theme_color_to_hsla(palette.foreground)
            } else if matches!(named, NamedColor::Background) {
                theme_color_to_hsla(palette.background)
            } else {
                named_to_theme(named, &palette.terminal)
                    .map(theme_color_to_hsla)
                    .unwrap_or_else(|| theme_color_to_hsla(palette.foreground))
            }
        }
        VteColor::Indexed(idx) => indexed_to_theme(idx, &palette.terminal)
            .map(theme_color_to_hsla)
            .unwrap_or_else(|| theme_color_to_hsla(palette.foreground)),
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
fn indexed_to_theme(idx: u8, _pal: &TerminalPalette) -> Option<ThemeColor> {
    if idx < 16 {
        return None;
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

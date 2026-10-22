//! 终端与编辑器共用的查找条（FR-TERM-04：搜索 overlay 与编辑器复用）。
//!
//! 结构 = 标签 + 可编辑查询串（光标 `|` / 选区高亮 / IME 预编辑）+ 匹配计数 +
//! Aa / 上一处 / 下一处 / 关闭四个控件。编辑语义（光标、选区、插入删除）在
//! [`SearchOverlay`]；本模块负责视图接线：渲染、鼠标命中与划选、按键分派、
//! IME 选区上报与候选窗锚点。
//!
//! 鼠标监听里一律 `cx.stop_propagation()`：否则点击会冒泡到终端根元素的
//! `handle_mouse_down`，把查询区点击误当成终端划选。

use gpui::{
    div, prelude::FluentBuilder, px, AnyElement, App, Bounds, ClipboardItem, Context, FocusHandle,
    InteractiveElement, IntoElement, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Point, SharedString, StatefulInteractiveElement, Styled,
    UTF16Selection, Window,
};
use termior_theme::ResolvedPalette;
use termior_ui_kit::{theme::active_wash, tokens, Icon, SearchOverlay, Tooltip};

use crate::ime_anchor::ImeAnchor;
use crate::ui;

/// 查找条交互事件：由各 view 的 `cx.listener` 闭包转发给 [`handle_search_bar_event`]。
#[derive(Debug, Clone, Copy)]
pub(crate) enum SearchBarEvent {
    /// 查询区按下：单击定位光标 / 双击选词 / 三击全选，随后拖拽划选。
    InputDown {
        position: Point<Pixels>,
        click_count: usize,
    },
    /// 查询区按住左键拖拽。
    InputDrag { position: Point<Pixels> },
    /// 左键抬起：结束拖选。
    InputUp,
    /// Aa：切换大小写敏感（需重跑搜索）。
    ToggleCase,
    /// 上一处匹配。
    Prev,
    /// 下一处匹配。
    Next,
    /// 关闭查找条。
    Close,
}

/// 事件处理结果：view 据此重跑搜索 / 跳转匹配 / 关闭。
pub(crate) enum SearchBarOutcome {
    /// 光标或选区变化，仅需重绘。
    Redraw,
    /// 查询选项变化，需重跑搜索。
    QueryChanged,
    Prev,
    Next,
    Close,
}

/// 处理查找条鼠标/按钮事件。`focus` 是承载查找条的 pane 焦点句柄——
/// 点击查询区要把键盘焦点拿回来（根元素的聚焦逻辑被 stop_propagation 跳过了）。
pub(crate) fn handle_search_bar_event(
    event: SearchBarEvent,
    overlay: &mut SearchOverlay,
    anchor: &ImeAnchor,
    focus: &FocusHandle,
    window: &mut Window,
    cx: &mut App,
) -> SearchBarOutcome {
    match event {
        SearchBarEvent::InputDown {
            position,
            click_count,
        } => {
            window.focus(focus, cx);
            let index = anchor.char_index_at(&overlay.query, position, window);
            overlay.click_at(index, click_count);
            SearchBarOutcome::Redraw
        }
        SearchBarEvent::InputDrag { position } => {
            let index = anchor.char_index_at(&overlay.query, position, window);
            overlay.drag_to(index);
            SearchBarOutcome::Redraw
        }
        SearchBarEvent::InputUp => {
            overlay.end_drag();
            SearchBarOutcome::Redraw
        }
        SearchBarEvent::ToggleCase => {
            overlay.options.case_sensitive = !overlay.options.case_sensitive;
            SearchBarOutcome::QueryChanged
        }
        SearchBarEvent::Prev => SearchBarOutcome::Prev,
        SearchBarEvent::Next => SearchBarOutcome::Next,
        SearchBarEvent::Close => SearchBarOutcome::Close,
    }
}

/// 查找条打开时的按键分派结果。
pub(crate) enum SearchKeyOutcome {
    /// 不是查找条编辑键。调用方自行决定放行（全局快捷键）还是吞掉——
    /// 查找条打开期间绝不能让按键落进 PTY / 编辑器缓冲。
    Ignored,
    /// 查询串被修改，需重跑搜索。
    QueryChanged,
    /// 仅光标/选区变化（含复制）。
    SelectionChanged,
    /// Esc：关闭查找条。
    Close,
    /// Enter / Shift+Enter：跳转下一处 / 上一处。
    Submit { backwards: bool },
    /// Ctrl+V：Windows 上读剪贴板会向 wndproc 回派消息，不能在实体更新中
    /// 直接读，由调用方 `cx.defer` 延迟粘贴。
    Paste,
}

/// 查找条打开时的编辑按键：复制/剪切/粘贴/全选、光标移动（Shift 扩选）、
/// Backspace/Delete、Enter、Esc、Alt+C 大小写切换。
pub(crate) fn search_edit_key(
    cx: &mut App,
    overlay: &mut SearchOverlay,
    key: &str,
    modifiers: Modifiers,
) -> SearchKeyOutcome {
    use SearchKeyOutcome::*;
    let primary = if cfg!(target_os = "macos") {
        modifiers.platform
    } else {
        modifiers.control
    };
    if primary && !modifiers.shift && !modifiers.alt {
        match key {
            "a" => {
                overlay.select_all();
                return SelectionChanged;
            }
            "c" => {
                if let Some(text) = overlay.selected_text() {
                    cx.defer(move |cx| cx.write_to_clipboard(ClipboardItem::new_string(text)));
                    return SelectionChanged;
                }
                return Ignored;
            }
            "x" => {
                if let Some(text) = overlay.selected_text() {
                    overlay.delete_selection();
                    cx.defer(move |cx| cx.write_to_clipboard(ClipboardItem::new_string(text)));
                    return QueryChanged;
                }
                return Ignored;
            }
            "v" => return Paste,
            _ => {}
        }
    }
    match key {
        "escape" => Close,
        "enter" | "return" => Submit {
            backwards: modifiers.shift,
        },
        "backspace" => {
            overlay.backspace();
            QueryChanged
        }
        "delete" => {
            overlay.delete_forward();
            QueryChanged
        }
        "left" if modifiers.shift => {
            overlay.move_cursor(overlay.cursor.saturating_sub(1), true);
            SelectionChanged
        }
        "right" if modifiers.shift => {
            let end = overlay.char_len();
            overlay.move_cursor((overlay.cursor + 1).min(end), true);
            SelectionChanged
        }
        "home" => {
            overlay.move_cursor(0, modifiers.shift);
            SelectionChanged
        }
        "end" => {
            let end = overlay.char_len();
            overlay.move_cursor(end, modifiers.shift);
            SelectionChanged
        }
        "left" => {
            let target = overlay
                .selection()
                .map_or_else(|| overlay.cursor.saturating_sub(1), |range| range.start);
            overlay.move_cursor(target, false);
            SelectionChanged
        }
        "right" => {
            let target = overlay.selection().map_or_else(
                || (overlay.cursor + 1).min(overlay.char_len()),
                |range| range.end,
            );
            overlay.move_cursor(target, false);
            SelectionChanged
        }
        "c" if modifiers.alt => {
            overlay.options.case_sensitive = !overlay.options.case_sensitive;
            QueryChanged
        }
        _ => Ignored,
    }
}

/// 查找条选区/光标的 UTF-16 区间（`InputHandler::selected_text_range` 用）：
/// 上报真实选区后，IME 组词提交才会替换选中文字而不是追加。
pub(crate) fn search_utf16_selection(overlay: &SearchOverlay) -> UTF16Selection {
    let utf16 = |char_index: usize| {
        overlay
            .query
            .chars()
            .take(char_index)
            .collect::<String>()
            .encode_utf16()
            .count()
    };
    match overlay.selection() {
        Some(range) => UTF16Selection {
            range: utf16(range.start)..utf16(range.end),
            reversed: overlay.anchor.is_some_and(|anchor| anchor > overlay.cursor),
        },
        None => {
            let position = utf16(overlay.cursor);
            UTF16Selection {
                range: position..position,
                reversed: false,
            }
        }
    }
}

/// IME 候选窗锚点：查询串光标处（`InputHandler::bounds_for_range` 用）。
pub(crate) fn search_caret_bounds(
    overlay: &SearchOverlay,
    anchor: &ImeAnchor,
    window: &mut Window,
) -> Option<Bounds<Pixels>> {
    anchor.caret_bounds(&overlay.query, overlay.cursor_byte(), window)
}

/// 鼠标事件监听器：GPUI 各 `on_mouse_*` 的处理器形状。
type MouseListener<E> = Box<dyn Fn(&E, &mut Window, &mut App) + 'static>;

/// 把鼠标事件转成查找条事件的监听器：命中后再截断冒泡，交给 view 处理。
fn forward_mouse<E, V, F>(
    cx: &Context<V>,
    on_event: &F,
    to_event: fn(&E) -> Option<SearchBarEvent>,
) -> MouseListener<E>
where
    E: 'static,
    V: 'static,
    F: Fn(&mut V, SearchBarEvent, &mut Window, &mut Context<V>) + Clone + 'static,
{
    let on_event = on_event.clone();
    Box::new(cx.listener(move |view, event, window, cx| {
        if let Some(event) = to_event(event) {
            cx.stop_propagation();
            on_event(view, event, window, cx);
        }
    }))
}

/// 图标按钮监听器：截断冒泡后把按钮事件交给 view。
fn forward_click<V, F>(
    cx: &Context<V>,
    on_event: &F,
    event: SearchBarEvent,
) -> MouseListener<MouseDownEvent>
where
    V: 'static,
    F: Fn(&mut V, SearchBarEvent, &mut Window, &mut Context<V>) + Clone + 'static,
{
    let on_event = on_event.clone();
    Box::new(cx.listener(move |view, _, window, cx| {
        cx.stop_propagation();
        on_event(view, event, window, cx);
    }))
}

/// 渲染查找条。`marked_text` 是 IME 预编辑串（显示在光标处，accent 底色）。
pub(crate) fn render_search_bar<V: 'static>(
    cx: &mut Context<V>,
    marked_text: &str,
    overlay: &SearchOverlay,
    palette: &ResolvedPalette,
    anchor: &ImeAnchor,
    on_event: impl Fn(&mut V, SearchBarEvent, &mut Window, &mut Context<V>) + Clone + 'static,
) -> AnyElement {
    let p = palette;

    // 查询区内容：有选区时前缀/选中/后缀三段（选中段高亮）；有预编辑时把
    // `|` + 预编辑串贴在光标处；其余情况单段文本内联 `|` 假光标——不做 flex
    // 拆段加独立光标元素（拆段曾在真实字体下被压成逐字换行的竖排）。
    let chars: Vec<char> = overlay.query.chars().collect();
    let cursor = overlay.cursor.min(chars.len());
    let plain = |text: String| -> Option<AnyElement> {
        (!text.is_empty()).then(|| {
            div()
                .whitespace_nowrap()
                .child(SharedString::from(text))
                .into_any_element()
        })
    };
    let highlighted = |text: String| -> Option<AnyElement> {
        (!text.is_empty()).then(|| {
            div()
                .whitespace_nowrap()
                .rounded_sm()
                .bg(ui::selected_wash(p))
                .child(SharedString::from(text))
                .into_any_element()
        })
    };
    let query_content: AnyElement = if let Some(range) = overlay.selection() {
        div()
            .flex()
            .flex_row()
            .items_baseline()
            .children(plain(chars[..range.start].iter().collect()))
            .children(highlighted(chars[range.start..range.end].iter().collect()))
            .children(plain(chars[range.end..].iter().collect()))
            .into_any_element()
    } else if !marked_text.is_empty() {
        div()
            .flex()
            .flex_row()
            .items_baseline()
            .children(plain(chars[..cursor].iter().collect()))
            .child(
                div()
                    .whitespace_nowrap()
                    .rounded_sm()
                    .bg(ui::alpha(p.accent, 0.35))
                    .child(SharedString::from(format!("|{marked_text}"))),
            )
            .children(plain(chars[cursor..].iter().collect()))
            .into_any_element()
    } else {
        let mut text: String = chars[..cursor].iter().collect();
        text.push('|');
        text.extend(chars[cursor..].iter());
        div()
            .whitespace_nowrap()
            .child(SharedString::from(text))
            .into_any_element()
    };

    let input = div()
        .id("find-bar-input")
        .debug_selector(|| "find-bar-input".to_owned())
        .flex()
        .flex_row()
        .items_center()
        .px_2()
        .py_1()
        .rounded(px(tokens::radius::SM))
        .border_1()
        .border_color(ui::border(p))
        .bg(ui::color(p.elevated))
        .min_w(px(160.0))
        .max_w(px(420.0))
        .overflow_hidden()
        .cursor_text()
        .child(
            // 探针挂在无内边距的 relative 容器里：bounds 即文本原点，
            // 鼠标命中测试与 IME 候选窗定位都以它为基准。
            div()
                .relative()
                .child(query_content)
                .child(crate::ime_anchor::anchor_probe(anchor)),
        )
        .on_mouse_down(
            MouseButton::Left,
            forward_mouse(cx, &on_event, |event: &MouseDownEvent| {
                Some(SearchBarEvent::InputDown {
                    position: event.position,
                    click_count: event.click_count,
                })
            }),
        )
        .on_mouse_up(
            MouseButton::Left,
            forward_mouse(cx, &on_event, |_: &MouseUpEvent| {
                Some(SearchBarEvent::InputUp)
            }),
        )
        .on_mouse_move(forward_mouse(cx, &on_event, |event: &MouseMoveEvent| {
            (event.pressed_button == Some(MouseButton::Left)).then_some(SearchBarEvent::InputDrag {
                position: event.position,
            })
        }));

    let counter = tf!(
        "find.counter",
        "current" => if overlay.total == 0 { 0 } else { overlay.current + 1 },
        "total" => overlay.total
    );

    let case_chip = {
        let case_label = t!("find.case_sensitive");
        let tooltip_label = case_label.clone();
        let chip_palette = p.clone();
        div()
            .id("find-bar-case")
            .debug_selector(|| "find-bar-case".to_owned())
            .px_2()
            .py_1()
            .rounded(px(tokens::radius::SM))
            .cursor_pointer()
            .whitespace_nowrap()
            .aria_label(case_label)
            .tooltip(move |_window, cx| Tooltip::view(tooltip_label.clone(), &chip_palette, cx))
            .when(overlay.options.case_sensitive, |chip| {
                chip.bg(active_wash(p)).text_color(ui::color(p.accent))
            })
            .when(!overlay.options.case_sensitive, |chip| {
                chip.text_color(ui::muted(p))
            })
            .child(SharedString::from("Aa"))
            .on_mouse_down(
                MouseButton::Left,
                forward_click(cx, &on_event, SearchBarEvent::ToggleCase),
            )
    };

    div()
        .absolute()
        .top(px(8.0))
        .right(px(10.0))
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(ui::color(p.accent))
        .bg(ui::color(p.overlay))
        .text_color(ui::color(p.foreground))
        .shadow_md()
        // 点击条身（标签/计数等非交互区）也不能触发终端划选。
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
        .child(t!("find.label"))
        .child(input)
        .child(counter)
        .child(case_chip)
        .child(
            ui::icon_button("find-bar-prev", Icon::ChevronUp, t!("find.previous"), p)
                .debug_selector(|| "find-bar-prev".to_owned())
                .on_mouse_down(
                    MouseButton::Left,
                    forward_click(cx, &on_event, SearchBarEvent::Prev),
                ),
        )
        .child(
            ui::icon_button("find-bar-next", Icon::ChevronDown, t!("find.next"), p)
                .debug_selector(|| "find-bar-next".to_owned())
                .on_mouse_down(
                    MouseButton::Left,
                    forward_click(cx, &on_event, SearchBarEvent::Next),
                ),
        )
        .child(
            ui::icon_button("find-bar-close", Icon::Close, t!("find.close"), p)
                .debug_selector(|| "find-bar-close".to_owned())
                .on_mouse_down(
                    MouseButton::Left,
                    forward_click(cx, &on_event, SearchBarEvent::Close),
                ),
        )
        .into_any_element()
}

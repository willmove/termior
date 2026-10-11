//! 只读富文本的鼠标选取（Agent 会话消息）。
//!
//! GPUI 的 `StyledText` 只负责显示，本模块补上选取：渲染期每段文本旁挂一个零尺寸
//! canvas，在 paint 阶段登记该段的 `TextLayout`（此时布局与位置都已确定）；鼠标
//! 事件据此把屏幕坐标命中到「段 + 字节偏移」。选区以高亮背景绘制，复制时按段拼接。

use gpui::{
    canvas, prelude::*, AnyElement, HighlightStyle, Hsla, Pixels, Point, SharedString, StyledText,
    TextLayout,
};
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

/// 一段可选文本的位置：`group` 区分消息，`seq` 为消息内的渲染顺序。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SegmentKey {
    pub group: usize,
    pub seq: usize,
}

/// 选区端点：某段文本内的字节偏移。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TextPoint {
    pub key: SegmentKey,
    pub offset: usize,
}

/// 锚点（按下处）与活动端（拖动处）；两者可逆序。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TextSelection {
    pub anchor: TextPoint,
    pub head: TextPoint,
}

impl TextSelection {
    pub(crate) fn caret(point: TextPoint) -> Self {
        Self {
            anchor: point,
            head: point,
        }
    }

    pub(crate) fn ordered(&self) -> (TextPoint, TextPoint) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// 选区落在 `key` 段（文本为 `text`）内的字节区间，边界对齐到字符。
    pub(crate) fn range_in(&self, key: SegmentKey, text: &str) -> Option<Range<usize>> {
        let (start, end) = self.ordered();
        if self.is_empty() || key < start.key || key > end.key {
            return None;
        }
        let from = if key == start.key {
            floor_char_boundary(text, start.offset)
        } else {
            0
        };
        let to = if key == end.key {
            floor_char_boundary(text, end.offset)
        } else {
            text.len()
        };
        (from < to).then_some(from..to)
    }
}

struct Segment {
    key: SegmentKey,
    text: SharedString,
    layout: TextLayout,
}

/// 本帧已绘制的可选文本段。每次渲染前 [`clear`](Self::clear)，paint 期重新登记。
#[derive(Clone, Default)]
pub(crate) struct SelectableTexts(Rc<RefCell<Vec<Segment>>>);

impl SelectableTexts {
    pub(crate) fn clear(&self) {
        self.0.borrow_mut().clear();
    }

    /// 渲染一段可选文本：在原有样式上叠加选区背景，并登记布局供命中测试。
    /// 返回文本元素与登记用的零尺寸 canvas，调用方把两者都作为子元素挂上。
    pub(crate) fn text(
        &self,
        key: SegmentKey,
        text: SharedString,
        highlights: Vec<(Range<usize>, HighlightStyle)>,
        selection: Option<TextSelection>,
        selection_background: Hsla,
    ) -> [AnyElement; 2] {
        let selected = selection.and_then(|selection| selection.range_in(key, &text));
        let highlights = overlay_selection(highlights, selected, selection_background);
        let styled = StyledText::new(text.clone()).with_highlights(highlights);
        let layout = styled.layout().clone();
        let registry = self.0.clone();
        let register = canvas(
            |_, _, _| {},
            move |_, _, _, _| {
                registry.borrow_mut().push(Segment {
                    key,
                    text: text.clone(),
                    layout: layout.clone(),
                })
            },
        )
        .absolute()
        .size_0();
        [styled.into_any_element(), register.into_any_element()]
    }

    /// 精确命中：坐标落在某段文本的范围内。
    pub(crate) fn hit(&self, position: Point<Pixels>) -> Option<TextPoint> {
        let segments = self.0.borrow();
        segments
            .iter()
            .find(|segment| segment.layout.bounds().contains(&position))
            .map(|segment| TextPoint {
                key: segment.key,
                offset: index_for(segment, position),
            })
    }

    /// 拖动时的最近点：按文档顺序找第一个底边不在坐标之上的段；在其上方取段首，
    /// 越过全部文本取末段段尾。
    pub(crate) fn nearest(&self, position: Point<Pixels>) -> Option<TextPoint> {
        if let Some(point) = self.hit(position) {
            return Some(point);
        }
        let segments = self.0.borrow();
        let mut ordered = segments.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|segment| segment.key);
        for segment in &ordered {
            let bounds = segment.layout.bounds();
            if position.y <= bounds.bottom() {
                let offset = if position.y < bounds.top() {
                    0
                } else {
                    index_for(segment, position)
                };
                return Some(TextPoint {
                    key: segment.key,
                    offset,
                });
            }
        }
        ordered.last().map(|segment| TextPoint {
            key: segment.key,
            offset: segment.text.len(),
        })
    }

    /// 双击选词：扩展到点所在的连续单词字符（字母数字、下划线及非 ASCII 字符）。
    pub(crate) fn word_at(&self, point: TextPoint) -> Option<TextSelection> {
        let segments = self.0.borrow();
        let segment = segments.iter().find(|segment| segment.key == point.key)?;
        let range = word_range(&segment.text, point.offset);
        Some(TextSelection {
            anchor: TextPoint {
                key: point.key,
                offset: range.start,
            },
            head: TextPoint {
                key: point.key,
                offset: range.end,
            },
        })
    }

    /// 三击选段：整段文本。
    pub(crate) fn segment_at(&self, point: TextPoint) -> Option<TextSelection> {
        let segments = self.0.borrow();
        let segment = segments.iter().find(|segment| segment.key == point.key)?;
        Some(TextSelection {
            anchor: TextPoint {
                key: point.key,
                offset: 0,
            },
            head: TextPoint {
                key: point.key,
                offset: segment.text.len(),
            },
        })
    }

    #[cfg(test)]
    pub(crate) fn segment_bounds(&self) -> Vec<(SegmentKey, gpui::Bounds<Pixels>)> {
        let mut bounds = self
            .0
            .borrow()
            .iter()
            .map(|segment| (segment.key, segment.layout.bounds()))
            .collect::<Vec<_>>();
        bounds.sort_by_key(|(key, _)| *key);
        bounds
    }

    /// 选中的纯文本：同一消息内的段以换行分隔，消息之间空一行。
    pub(crate) fn selected_text(&self, selection: TextSelection) -> String {
        let segments = self.0.borrow();
        let mut ordered = segments.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|segment| segment.key);
        let mut out = String::new();
        let mut previous: Option<SegmentKey> = None;
        for segment in ordered {
            let Some(range) = selection.range_in(segment.key, &segment.text) else {
                continue;
            };
            if let Some(previous) = previous {
                out.push_str(if previous.group == segment.key.group {
                    "\n"
                } else {
                    "\n\n"
                });
            }
            out.push_str(&segment.text[range]);
            previous = Some(segment.key);
        }
        out
    }
}

fn index_for(segment: &Segment, position: Point<Pixels>) -> usize {
    match segment.layout.index_for_position(position) {
        Ok(index) | Err(index) => floor_char_boundary(&segment.text, index),
    }
}

fn floor_char_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn word_range(text: &str, offset: usize) -> Range<usize> {
    let offset = floor_char_boundary(text, offset);
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || !c.is_ascii();
    let start = text[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map_or(offset, |(index, _)| index);
    let end = text[offset..]
        .char_indices()
        .find(|(_, c)| !is_word(*c))
        .map_or(text.len(), |(index, _)| offset + index);
    start..end
}

/// 把选区背景叠加到已有高亮上。`StyledText` 要求高亮区间有序且不重叠，
/// 因此按所有区间端点切分，逐小段合并样式。
fn overlay_selection(
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    selected: Option<Range<usize>>,
    background: Hsla,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let Some(selected) = selected else {
        return highlights;
    };
    let mut cuts = vec![selected.start, selected.end];
    for (range, _) in &highlights {
        cuts.push(range.start);
        cuts.push(range.end);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut merged = Vec::new();
    for window in cuts.windows(2) {
        let piece = window[0]..window[1];
        let base = highlights
            .iter()
            .find(|(range, _)| range.start <= piece.start && piece.end <= range.end)
            .map(|(_, style)| *style);
        let in_selection = selected.start <= piece.start && piece.end <= selected.end;
        match (base, in_selection) {
            (None, false) => {}
            (Some(style), false) => merged.push((piece, style)),
            (base, true) => {
                let mut style = base.unwrap_or_default();
                style.background_color = Some(background);
                merged.push((piece, style));
            }
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::FontWeight;

    fn point(group: usize, seq: usize, offset: usize) -> TextPoint {
        TextPoint {
            key: SegmentKey { group, seq },
            offset,
        }
    }

    #[test]
    fn selection_ranges_span_segments_in_document_order() {
        let selection = TextSelection {
            anchor: point(1, 0, 3),
            head: point(0, 1, 2),
        };
        let key = |group, seq| SegmentKey { group, seq };
        assert_eq!(selection.range_in(key(0, 0), "hello"), None);
        assert_eq!(selection.range_in(key(0, 1), "hello"), Some(2..5));
        assert_eq!(selection.range_in(key(0, 2), "hello"), Some(0..5));
        assert_eq!(selection.range_in(key(1, 0), "hello"), Some(0..3));
        assert_eq!(selection.range_in(key(1, 1), "hello"), None);
    }

    #[test]
    fn offsets_snap_to_char_boundaries_and_clamp() {
        let selection = TextSelection {
            anchor: point(0, 0, 1),
            head: point(0, 0, 99),
        };
        // "你" 占 3 字节：偏移 1 回退到 0，越界偏移截到文本末尾。
        assert_eq!(
            selection.range_in(SegmentKey { group: 0, seq: 0 }, "你好"),
            Some(0..6)
        );
        assert!(TextSelection::caret(point(0, 0, 1))
            .range_in(SegmentKey { group: 0, seq: 0 }, "abc")
            .is_none());
    }

    #[test]
    fn selection_background_overlays_without_overlapping_ranges() {
        let bold = HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            ..Default::default()
        };
        let background = gpui::red();
        let merged = overlay_selection(vec![(2..6, bold)], Some(4..8), background);
        let ranges = merged
            .iter()
            .map(|(range, _)| range.clone())
            .collect::<Vec<_>>();
        assert_eq!(ranges, vec![2..4, 4..6, 6..8]);
        assert_eq!(merged[0].1.background_color, None);
        assert_eq!(merged[1].1.font_weight, Some(FontWeight::BOLD));
        assert_eq!(merged[1].1.background_color, Some(background));
        assert_eq!(merged[2].1.font_weight, None);
        assert_eq!(merged[2].1.background_color, Some(background));
    }

    #[test]
    fn word_range_expands_over_word_characters() {
        assert_eq!(word_range("fn main_loop() {}", 5), 3..12);
        assert_eq!(word_range("选取 文本", 3), 0..6);
        assert_eq!(word_range("a  b", 2), 2..2);
    }
}

//! 查找条状态：查询串、匹配导航与输入框的光标/选区。
//!
//! 光标与选区都以**字符下标**计（不是字节），与设置页编辑字段一致。
//! 编辑语义（插入替换选区、Backspace/Delete、Shift 扩选、双击选词、拖拽划选）
//! 全部收敛在这里，视图只做事件接线。

use serde::{Deserialize, Serialize};
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SearchOptions {
    pub case_sensitive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchOverlay {
    pub visible: bool,
    pub query: String,
    pub options: SearchOptions,
    pub current: usize,
    pub total: usize,
    /// 输入框光标位置（字符下标）。
    pub cursor: usize,
    /// 选区锚点（字符下标）；`None` 表示无选区。
    pub anchor: Option<usize>,
    /// 拖选起点：按下处固定，拖拽期间不变。
    pub drag_anchor: Option<usize>,
}

impl SearchOverlay {
    /// 打开查找条。已有查询串时全选，便于直接输入覆盖。
    pub fn open(&mut self) {
        self.visible = true;
        self.drag_anchor = None;
        if self.query.is_empty() {
            self.cursor = 0;
            self.anchor = None;
        } else {
            self.select_all();
        }
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.drag_anchor = None;
    }

    pub fn set_results(&mut self, total: usize) {
        self.total = total;
        self.current = self.current.min(total.saturating_sub(1));
    }

    pub fn next(&mut self, backwards: bool) {
        if self.total == 0 {
            self.current = 0;
        } else if backwards {
            self.current = (self.current + self.total - 1) % self.total;
        } else {
            self.current = (self.current + 1) % self.total;
        }
    }

    pub fn char_len(&self) -> usize {
        self.query.chars().count()
    }

    /// 光标处的字节偏移（`InputHandler` 与文本测量用）。
    pub fn cursor_byte(&self) -> usize {
        char_to_byte(&self.query, self.cursor)
    }

    /// 当前选区（归一化字符区间，start <= end）；无选区时返回 `None`。
    pub fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    /// 选区文本（复制/剪切用）；无选区时返回 `None`。
    pub fn selected_text(&self) -> Option<String> {
        let range = self.selection()?;
        Some(
            self.query
                .chars()
                .skip(range.start)
                .take(range.end - range.start)
                .collect(),
        )
    }

    /// 删除选区并把光标折叠到选区起点；返回是否确有选区被删除。
    pub fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection() else {
            return false;
        };
        let start = char_to_byte(&self.query, range.start);
        let end = char_to_byte(&self.query, range.end);
        self.query.replace_range(start..end, "");
        self.cursor = range.start;
        self.anchor = None;
        true
    }

    /// 在光标处插入文本（替换当前选区）。单行输入丢弃换行符。
    pub fn insert_text(&mut self, text: &str) {
        let text: String = text.chars().filter(|c| !matches!(c, '\r' | '\n')).collect();
        if text.is_empty() {
            return;
        }
        self.delete_selection();
        self.anchor = None;
        self.drag_anchor = None;
        let byte = char_to_byte(&self.query, self.cursor);
        self.query.insert_str(byte, &text);
        self.cursor += text.chars().count();
    }

    /// Backspace：优先删除选区，否则删光标前一个字符。
    pub fn backspace(&mut self) {
        if !self.delete_selection() {
            if self.cursor == 0 {
                return;
            }
            let start = char_to_byte(&self.query, self.cursor - 1);
            let end = char_to_byte(&self.query, self.cursor);
            self.query.replace_range(start..end, "");
            self.cursor -= 1;
        }
        self.anchor = None;
        self.drag_anchor = None;
    }

    /// Delete：优先删除选区，否则删光标处字符。
    pub fn delete_forward(&mut self) {
        if !self.delete_selection() {
            if self.cursor >= self.char_len() {
                return;
            }
            let start = char_to_byte(&self.query, self.cursor);
            let end = char_to_byte(&self.query, self.cursor + 1);
            self.query.replace_range(start..end, "");
        }
        self.anchor = None;
        self.drag_anchor = None;
    }

    /// 移动光标；`extend` 为真时从锚点扩展选区（Shift+方向键）。
    pub fn move_cursor(&mut self, target: usize, extend: bool) {
        let target = target.min(self.char_len());
        if extend {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
            self.drag_anchor = None;
        }
        self.cursor = target;
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.char_len();
        self.drag_anchor = Some(0);
    }

    /// 鼠标按下：单击定位光标，双击选词，三击全选；随后拖拽从按下处划选。
    pub fn click_at(&mut self, index: usize, click_count: usize) {
        let index = index.min(self.char_len());
        match click_count {
            2 => {
                let range = self.word_range_at(index);
                self.anchor = Some(range.start);
                self.cursor = range.end;
                self.drag_anchor = Some(range.start);
            }
            3.. => self.select_all(),
            _ => {
                self.cursor = index;
                self.anchor = None;
                self.drag_anchor = Some(index);
            }
        }
    }

    /// 拖拽划选：固定端停在按下处，游标端跟随指针。
    pub fn drag_to(&mut self, index: usize) {
        let index = index.min(self.char_len());
        if self.cursor != index || self.anchor.is_none() {
            self.cursor = index;
            self.anchor = Some(self.drag_anchor.unwrap_or(index));
        }
    }

    /// 左键抬起：结束拖选。
    pub fn end_drag(&mut self) {
        self.drag_anchor = None;
    }

    /// 双击选词的词边界：字母数字以及 URL/型号里常见的符号算词内字符。
    pub fn word_range_at(&self, index: usize) -> Range<usize> {
        let chars: Vec<char> = self.query.chars().collect();
        let is_word =
            |c: char| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/' | '+' | '=');
        let mut start = index.min(chars.len());
        while start > 0 && is_word(chars[start - 1]) {
            start -= 1;
        }
        let mut end = index.min(chars.len());
        while end < chars.len() && is_word(chars[end]) {
            end += 1;
        }
        start..end
    }
}

fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(byte, _)| byte)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_navigation_wraps() {
        let mut overlay = SearchOverlay::default();
        overlay.set_results(3);
        overlay.next(true);
        assert_eq!(overlay.current, 2);
        overlay.next(false);
        assert_eq!(overlay.current, 0);
    }

    #[test]
    fn open_selects_existing_query() {
        let mut overlay = SearchOverlay::default();
        overlay.open();
        assert_eq!(overlay.cursor, 0);
        assert_eq!(overlay.selection(), None);
        overlay.insert_text("find me");
        overlay.close();
        overlay.open();
        assert_eq!(overlay.selection(), Some(0..7));
    }

    #[test]
    fn insert_replaces_selection() {
        let mut overlay = SearchOverlay::default();
        overlay.insert_text("hello world");
        overlay.click_at(6, 2); // 双击选中 "world"
        assert_eq!(overlay.selected_text().as_deref(), Some("world"));
        overlay.insert_text("there");
        assert_eq!(overlay.query, "hello there");
        assert_eq!(overlay.cursor, 11);
        assert_eq!(overlay.selection(), None);
    }

    #[test]
    fn insert_ignores_newlines_and_keeps_cursor_mid_query() {
        let mut overlay = SearchOverlay::default();
        overlay.insert_text("ab");
        overlay.move_cursor(1, false);
        overlay.insert_text("x\ny\rz");
        assert_eq!(overlay.query, "axyzb");
        assert_eq!(overlay.cursor, 4);
    }

    #[test]
    fn backspace_and_delete_respect_selection() {
        let mut overlay = SearchOverlay::default();
        overlay.insert_text("abc");
        overlay.move_cursor(1, false);
        overlay.move_cursor(0, true); // 选中 "a"
        overlay.backspace();
        assert_eq!(overlay.query, "bc");
        overlay.delete_forward(); // 删 "b"
        assert_eq!(overlay.query, "c");
        overlay.move_cursor(0, false);
        overlay.backspace(); // 光标在 0，无事发生
        assert_eq!(overlay.query, "c");
        overlay.delete_forward();
        overlay.delete_forward(); // 删空后再删无事发生
        assert_eq!(overlay.query, "");
    }

    #[test]
    fn shift_move_extends_selection_and_plain_move_collapses() {
        let mut overlay = SearchOverlay::default();
        overlay.insert_text("abcd");
        overlay.move_cursor(2, false);
        overlay.move_cursor(4, true);
        assert_eq!(overlay.selection(), Some(2..4));
        overlay.move_cursor(3, true);
        assert_eq!(overlay.selection(), Some(2..3));
        overlay.move_cursor(1, true);
        assert_eq!(overlay.selection(), Some(1..2));
        overlay.move_cursor(0, false);
        assert_eq!(overlay.selection(), None);
    }

    #[test]
    fn left_right_collapse_selection_at_edges() {
        let mut overlay = SearchOverlay::default();
        overlay.insert_text("abcd");
        overlay.select_all();
        overlay.move_cursor(overlay.selection().unwrap().start, false);
        assert_eq!(overlay.cursor, 0);
        overlay.select_all();
        overlay.move_cursor(overlay.selection().unwrap().end, false);
        assert_eq!(overlay.cursor, 4);
    }

    #[test]
    fn click_and_drag_select_from_press_point() {
        let mut overlay = SearchOverlay::default();
        overlay.insert_text("drag me");
        overlay.click_at(1, 1);
        overlay.drag_to(5);
        assert_eq!(overlay.selection(), Some(1..5));
        // 反向拖拽：固定端仍是按下处。
        overlay.drag_to(0);
        assert_eq!(overlay.selection(), Some(0..1));
        overlay.end_drag();
        assert_eq!(overlay.drag_anchor, None);
    }

    #[test]
    fn triple_click_selects_all() {
        let mut overlay = SearchOverlay::default();
        overlay.insert_text("three clicks");
        overlay.click_at(5, 3);
        assert_eq!(overlay.selection(), Some(0..12));
    }

    #[test]
    fn word_range_covers_urls_and_models() {
        let mut overlay = SearchOverlay::default();
        overlay.insert_text("see https://a.io/x+1_v2 now");
        assert_eq!(overlay.word_range_at(6), 4..23);
        assert_eq!(overlay.word_range_at(25), 24..27);
    }

    #[test]
    fn cursor_byte_tracks_multibyte_text() {
        let mut overlay = SearchOverlay::default();
        overlay.insert_text("中文ab");
        assert_eq!(overlay.cursor, 4);
        assert_eq!(overlay.cursor_byte(), 8);
        overlay.move_cursor(1, false);
        assert_eq!(overlay.cursor_byte(), 3);
    }
}

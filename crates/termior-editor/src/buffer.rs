//! Rope-backed buffer, selection, search and undo history (FR-EDIT-01/03).

use ropey::Rope;
use std::ops::Range;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    /// Character offset, never a byte offset.
    pub char_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn range(self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    pub char_range: Range<usize>,
    pub line: usize,
}

/// A byte-based point used to keep tree-sitter's existing syntax tree in sync with rope edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextPoint {
    pub row: usize,
    pub column: usize,
}

/// The coordinates of one mutation before and after it is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferEdit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start_position: TextPoint,
    pub old_end_position: TextPoint,
    pub new_end_position: TextPoint,
}

/// A small, owned slice of the rope suitable for rendering a virtualized viewport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewportText {
    pub start_line: usize,
    pub start_char: usize,
    pub start_byte: usize,
    pub text: String,
}

#[derive(Debug, Clone)]
struct EditRecord {
    range_before: Range<usize>,
    removed: String,
    inserted: String,
    cursor_before: Cursor,
    cursor_after: Cursor,
    selection_before: Option<Selection>,
    selection_after: Option<Selection>,
}

#[derive(Debug)]
pub struct EditorBuffer {
    path: Option<PathBuf>,
    rope: Rope,
    cursor: Cursor,
    selection: Option<Selection>,
    undo: Vec<EditRecord>,
    redo: Vec<EditRecord>,
    pending_edits: Vec<BufferEdit>,
    revision: u64,
    saved_revision: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("character range {start}..{end} is outside buffer length {len}")]
    OutOfBounds {
        start: usize,
        end: usize,
        len: usize,
    },
    #[error("buffer has no file path")]
    NoPath,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl EditorBuffer {
    pub fn new(text: &str) -> Self {
        Self {
            path: None,
            rope: Rope::from_str(text),
            cursor: Cursor::default(),
            selection: None,
            undo: Vec::new(),
            redo: Vec::new(),
            pending_edits: Vec::new(),
            revision: 0,
            saved_revision: 0,
        }
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, EditError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)?;
        let mut buffer = Self::new(&text);
        buffer.path = Some(path.to_path_buf());
        Ok(buffer)
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn set_path(&mut self, path: impl Into<PathBuf>) {
        self.path = Some(path.into());
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    /// Copy only the requested line window instead of materializing the complete document.
    pub fn text_for_lines(&self, range: Range<usize>) -> ViewportText {
        let line_count = self.len_lines();
        let start_line = range.start.min(line_count.saturating_sub(1));
        let end_line = range.end.max(start_line).min(line_count);
        let start_char = self.rope.line_to_char(start_line);
        let end_char = if end_line < line_count {
            self.rope.line_to_char(end_line)
        } else {
            self.len_chars()
        };
        ViewportText {
            start_line,
            start_char,
            start_byte: self.rope.char_to_byte(start_char),
            text: self.rope.slice(start_char..end_char).to_string(),
        }
    }

    pub fn byte_for_char(&self, char_index: usize) -> Result<usize, EditError> {
        self.check_range(char_index..char_index)?;
        Ok(self.rope.char_to_byte(char_index))
    }

    /// Drain the ordered edits that have not yet been applied to the syntax tree.
    pub fn take_pending_edits(&mut self) -> Vec<BufferEdit> {
        std::mem::take(&mut self.pending_edits)
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    pub fn cursor(&self) -> Cursor {
        self.cursor
    }

    pub fn set_cursor(&mut self, char_index: usize) -> Result<(), EditError> {
        self.check_range(char_index..char_index)?;
        self.cursor.char_index = char_index;
        self.selection = None;
        Ok(())
    }

    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }

    pub fn set_selection(&mut self, anchor: usize, head: usize) -> Result<(), EditError> {
        self.check_range(anchor..head)?;
        self.selection = Some(Selection { anchor, head });
        self.cursor.char_index = head;
        Ok(())
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    pub fn selected_text(&self) -> Option<String> {
        let range = self.selection?.range();
        Some(self.rope.slice(range).to_string())
    }

    pub fn insert(&mut self, text: &str) -> Result<(), EditError> {
        let range = self
            .selection
            .map(Selection::range)
            .unwrap_or(self.cursor.char_index..self.cursor.char_index);
        self.replace(range, text)
    }

    pub fn delete(&mut self, range: Range<usize>) -> Result<(), EditError> {
        self.replace(range, "")
    }

    pub fn replace(&mut self, range: Range<usize>, text: &str) -> Result<(), EditError> {
        self.check_range(range.clone())?;
        let syntax_edit = self.describe_edit(range.clone(), text);
        let before_cursor = self.cursor;
        let before_selection = self.selection;
        let removed = self.rope.slice(range.clone()).to_string();
        self.rope.remove(range.clone());
        self.rope.insert(range.start, text);
        let inserted_chars = text.chars().count();
        self.cursor.char_index = range.start + inserted_chars;
        self.selection = None;
        self.undo.push(EditRecord {
            range_before: range,
            removed,
            inserted: text.to_owned(),
            cursor_before: before_cursor,
            cursor_after: self.cursor,
            selection_before: before_selection,
            selection_after: self.selection,
        });
        self.redo.clear();
        self.pending_edits.push(syntax_edit);
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        let Some(edit) = self.undo.pop() else {
            return false;
        };
        let inserted_len = edit.inserted.chars().count();
        let current_range = edit.range_before.start..edit.range_before.start + inserted_len;
        let syntax_edit = self.describe_edit(current_range.clone(), &edit.removed);
        self.rope.remove(current_range);
        self.rope.insert(edit.range_before.start, &edit.removed);
        self.cursor = edit.cursor_before;
        self.selection = edit.selection_before;
        self.redo.push(edit);
        self.pending_edits.push(syntax_edit);
        self.revision = self.revision.saturating_add(1);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(edit) = self.redo.pop() else {
            return false;
        };
        let removed_len = edit.removed.chars().count();
        let current_range = edit.range_before.start..edit.range_before.start + removed_len;
        let syntax_edit = self.describe_edit(current_range.clone(), &edit.inserted);
        self.rope.remove(current_range);
        self.rope.insert(edit.range_before.start, &edit.inserted);
        self.cursor = edit.cursor_after;
        self.selection = edit.selection_after;
        self.undo.push(edit);
        self.pending_edits.push(syntax_edit);
        self.revision = self.revision.saturating_add(1);
        true
    }

    pub fn save(&mut self) -> Result<(), EditError> {
        let path = self.path.clone().ok_or(EditError::NoPath)?;
        self.save_as(path)
    }

    pub fn save_as(&mut self, path: impl AsRef<Path>) -> Result<(), EditError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.text())?;
        self.path = Some(path.to_path_buf());
        self.saved_revision = self.revision;
        Ok(())
    }

    pub fn search(&self, query: &str, case_sensitive: bool) -> Vec<SearchMatch> {
        if query.is_empty() {
            return Vec::new();
        }
        let haystack = self.text();
        let (search_haystack, needle) = if case_sensitive {
            (haystack.clone(), query.to_owned())
        } else {
            (haystack.to_lowercase(), query.to_lowercase())
        };
        let mut matches = Vec::new();
        let mut byte_start = 0;
        while let Some(offset) = search_haystack[byte_start..].find(&needle) {
            let start_byte = byte_start + offset;
            let end_byte = start_byte + needle.len();
            // Lowercasing can change byte length for a few Unicode characters. Validate boundaries
            // against the original; CJK and ASCII (our primary cases) retain offsets.
            if haystack.is_char_boundary(start_byte) && haystack.is_char_boundary(end_byte) {
                let start = haystack[..start_byte].chars().count();
                let end = haystack[..end_byte].chars().count();
                matches.push(SearchMatch {
                    char_range: start..end,
                    line: self.rope.char_to_line(start),
                });
            }
            byte_start = end_byte.max(start_byte + 1);
            if byte_start >= search_haystack.len() {
                break;
            }
        }
        matches
    }

    pub fn line_col_for_char(&self, char_index: usize) -> Result<(usize, usize), EditError> {
        self.check_range(char_index..char_index)?;
        let line = self.rope.char_to_line(char_index);
        Ok((line, char_index - self.rope.line_to_char(line)))
    }

    pub fn char_for_line_col(&self, line: usize, column: usize) -> usize {
        let line = line.min(self.rope.len_lines().saturating_sub(1));
        let start = self.rope.line_to_char(line);
        let line_len = self.rope.line(line).len_chars();
        start + column.min(line_len.saturating_sub(usize::from(line + 1 < self.rope.len_lines())))
    }

    fn describe_edit(&self, range: Range<usize>, inserted: &str) -> BufferEdit {
        let start_byte = self.rope.char_to_byte(range.start);
        let old_end_byte = self.rope.char_to_byte(range.end);
        let start_position = self.point_for_char(range.start);
        let old_end_position = self.point_for_char(range.end);
        let mut new_end_position = start_position;
        let mut line_start = 0;
        for (index, byte) in inserted.bytes().enumerate() {
            if byte == b'\n' {
                new_end_position.row += 1;
                line_start = index + 1;
            }
        }
        if new_end_position.row == start_position.row {
            new_end_position.column += inserted.len();
        } else {
            new_end_position.column = inserted.len() - line_start;
        }
        BufferEdit {
            start_byte,
            old_end_byte,
            new_end_byte: start_byte + inserted.len(),
            start_position,
            old_end_position,
            new_end_position,
        }
    }

    fn point_for_char(&self, char_index: usize) -> TextPoint {
        let row = self.rope.char_to_line(char_index);
        let line_start = self.rope.line_to_char(row);
        TextPoint {
            row,
            column: self.rope.char_to_byte(char_index) - self.rope.char_to_byte(line_start),
        }
    }

    fn check_range(&self, range: Range<usize>) -> Result<(), EditError> {
        let start = range.start.min(range.end);
        let end = range.start.max(range.end);
        if end > self.len_chars() {
            return Err(EditError::OutOfBounds {
                start,
                end,
                len: self.len_chars(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_edit_uses_character_offsets() {
        let mut b = EditorBuffer::new("a中文b");
        b.set_cursor(1).unwrap();
        b.insert("好").unwrap();
        assert_eq!(b.text(), "a好中文b");
        assert_eq!(b.cursor().char_index, 2);
    }

    #[test]
    fn selection_replace_undo_redo_preserves_state() {
        let mut b = EditorBuffer::new("one two three");
        b.set_selection(4, 7).unwrap();
        b.insert("TWO").unwrap();
        assert_eq!(b.text(), "one TWO three");
        assert!(b.undo());
        assert_eq!(b.text(), "one two three");
        assert_eq!(b.selected_text().as_deref(), Some("two"));
        assert!(b.redo());
        assert_eq!(b.text(), "one TWO three");
    }

    #[test]
    fn search_case_and_cjk() {
        let b = EditorBuffer::new("Hello hello\n中文 中文\n");
        assert_eq!(b.search("hello", true).len(), 1);
        assert_eq!(b.search("hello", false).len(), 2);
        assert_eq!(b.search("中文", true).len(), 2);
        assert_eq!(b.search("中文", true)[0].line, 1);
    }

    #[test]
    fn save_roundtrip_and_dirty_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.txt");
        let mut b = EditorBuffer::new("a");
        b.set_cursor(1).unwrap();
        b.insert("b").unwrap();
        assert!(b.is_dirty());
        b.save_as(&path).unwrap();
        assert!(!b.is_dirty());
        assert_eq!(EditorBuffer::open(&path).unwrap().text(), "ab");
    }

    #[test]
    fn line_column_conversion() {
        let b = EditorBuffer::new("abc\n中文\n");
        assert_eq!(b.line_col_for_char(5).unwrap(), (1, 1));
        assert_eq!(b.char_for_line_col(1, 1), 5);
    }

    #[test]
    fn viewport_text_reads_only_requested_lines_from_large_document() {
        let source = (0..100_000)
            .map(|line| format!("line {line:06}\n"))
            .collect::<String>();
        let b = EditorBuffer::new(&source);
        let viewport = b.text_for_lines(50_000..50_003);
        assert_eq!(viewport.start_line, 50_000);
        assert_eq!(viewport.text, "line 050000\nline 050001\nline 050002\n");
        assert!(viewport.text.len() < 100);
        assert_eq!(viewport.start_byte, "line 000000\n".len() * 50_000);
    }

    #[test]
    fn records_ordered_byte_and_point_edits_for_incremental_parsing() {
        let mut b = EditorBuffer::new("fn main() {\n    println!(\"hi\");\n}\n");
        b.set_cursor(12).unwrap();
        b.insert("// 中文\n").unwrap();
        b.insert("let x = 1;\n").unwrap();
        let edits = b.take_pending_edits();
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].start_position, TextPoint { row: 1, column: 0 });
        assert_eq!(edits[0].new_end_position, TextPoint { row: 2, column: 0 });
        assert_eq!(edits[1].start_position, TextPoint { row: 2, column: 0 });
    }
}

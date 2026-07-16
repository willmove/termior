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
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        let Some(edit) = self.undo.pop() else {
            return false;
        };
        let inserted_len = edit.inserted.chars().count();
        self.rope
            .remove(edit.range_before.start..edit.range_before.start + inserted_len);
        self.rope.insert(edit.range_before.start, &edit.removed);
        self.cursor = edit.cursor_before;
        self.selection = edit.selection_before;
        self.redo.push(edit);
        self.revision = self.revision.saturating_add(1);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(edit) = self.redo.pop() else {
            return false;
        };
        let removed_len = edit.removed.chars().count();
        self.rope
            .remove(edit.range_before.start..edit.range_before.start + removed_len);
        self.rope.insert(edit.range_before.start, &edit.inserted);
        self.cursor = edit.cursor_after;
        self.selection = edit.selection_after;
        self.undo.push(edit);
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
}

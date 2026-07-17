//! Native editor core for Termior (FR-EDIT).
//!
//! The crate deliberately contains no GPUI types. Buffers and parse trees survive view switches,
//! while the application crate only renders the visible viewport.

#![forbid(unsafe_code)]

pub mod buffer;
pub mod completion;
pub mod syntax;
pub mod theme;
pub mod vim;

pub use buffer::{
    BufferEdit, Cursor, EditError, EditorBuffer, SearchMatch, Selection, TextPoint, ViewportText,
};
pub use completion::{CompletionController, CompletionState};
pub use syntax::{HighlightKind, HighlightSpan, SyntaxDocument, SyntaxLanguage};
pub use theme::{builtin_editor_themes, EditorTheme};
pub use vim::{Motion, VimCommand, VimEngine, VimMode};

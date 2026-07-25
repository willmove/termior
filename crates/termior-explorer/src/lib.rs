//! Filesystem-backed explorer, index and content search (FR-EXPL).

#![forbid(unsafe_code)]

mod index;
mod search;
mod tree;
mod watcher;

pub use index::{FileEntry, FileIndex, IconKind, IndexError, SkipReason, SkippedEntry};
pub use search::{ContentMatch, ContentSearch, SearchError};
pub use tree::{TreeError, TreeState};
pub use watcher::{FsChange, WorkspaceWatcher};

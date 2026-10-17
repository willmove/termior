//! Filesystem-backed explorer, index and content search (FR-EXPL).

#![forbid(unsafe_code)]
#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used))]

mod directory;
mod index;
mod search;
mod tree;
mod watcher;

pub use directory::{list_directory, DirectoryEntry};

pub use index::{
    FileEntry, FileIndex, IconKind, IndexError, SkipReason, SkippedEntry, SHALLOW_MAX_DEPTH,
};
pub use search::{ContentMatch, ContentSearch, SearchError};
pub use tree::{TreeError, TreeState};
pub use watcher::{
    debounced_rescan_action, rescan_worthy_kind, DebouncedRescanAction, FsChange, WorkspaceWatcher,
};

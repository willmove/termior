//! Lightweight UI contracts used by business crates. This internal implementation intentionally
//! avoids a direct gpui-component dependency until its NFR-11 budget is measured.

#![forbid(unsafe_code)]

mod layout;
mod search;

pub use layout::{LayoutError, LayoutNode, PaneExtent, PaneId, PaneLayout, SplitDirection};
pub use search::{SearchOptions, SearchOverlay};

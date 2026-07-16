use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PaneId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    Right,
    Down,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayoutNode {
    Pane {
        id: PaneId,
    },
    Split {
        direction: SplitDirection,
        ratio: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneLayout {
    pub root: LayoutNode,
    pub focused: PaneId,
    next_id: u64,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum LayoutError {
    #[error("pane not found: {0:?}")]
    PaneNotFound(PaneId),
    #[error("split ratio must be between 0.1 and 0.9")]
    InvalidRatio,
    #[error("cannot close the final pane")]
    FinalPane,
}

impl Default for PaneLayout {
    fn default() -> Self {
        Self::new()
    }
}

impl PaneLayout {
    pub fn new() -> Self {
        let pane = PaneId(1);
        Self {
            root: LayoutNode::Pane { id: pane },
            focused: pane,
            next_id: 2,
        }
    }

    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.root.collect(&mut out);
        out
    }

    pub fn split_focused(&mut self, direction: SplitDirection) -> PaneId {
        let new = PaneId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.root.split(self.focused, direction, new);
        self.focused = new;
        new
    }

    pub fn focus(&mut self, pane: PaneId) -> Result<(), LayoutError> {
        if self.root.contains(pane) {
            self.focused = pane;
            Ok(())
        } else {
            Err(LayoutError::PaneNotFound(pane))
        }
    }

    pub fn focus_relative(&mut self, delta: isize) {
        let panes = self.panes();
        let current = panes
            .iter()
            .position(|pane| *pane == self.focused)
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(panes.len() as isize) as usize;
        self.focused = panes[next];
    }

    pub fn close_focused(&mut self) -> Result<PaneId, LayoutError> {
        if self.panes().len() == 1 {
            return Err(LayoutError::FinalPane);
        }
        self.root = self
            .root
            .clone()
            .remove(self.focused)
            .expect("more than one pane leaves a root");
        self.focused = self.panes()[0];
        Ok(self.focused)
    }

    pub fn resize_split(&mut self, path: &[usize], ratio: f32) -> Result<(), LayoutError> {
        if !(0.1..=0.9).contains(&ratio) {
            return Err(LayoutError::InvalidRatio);
        }
        let mut node = &mut self.root;
        for branch in path {
            node = match (node, branch) {
                (LayoutNode::Split { first, .. }, 0) => first,
                (LayoutNode::Split { second, .. }, 1) => second,
                _ => return Err(LayoutError::PaneNotFound(self.focused)),
            };
        }
        match node {
            LayoutNode::Split { ratio: value, .. } => {
                *value = ratio;
                Ok(())
            }
            LayoutNode::Pane { .. } => Err(LayoutError::PaneNotFound(self.focused)),
        }
    }
}

impl LayoutNode {
    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Self::Pane { id } => out.push(*id),
            Self::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
        }
    }

    fn contains(&self, pane: PaneId) -> bool {
        match self {
            Self::Pane { id } => *id == pane,
            Self::Split { first, second, .. } => first.contains(pane) || second.contains(pane),
        }
    }

    fn split(&mut self, target: PaneId, direction: SplitDirection, new: PaneId) -> bool {
        match self {
            Self::Pane { id } if *id == target => {
                *self = Self::Split {
                    direction,
                    ratio: 0.5,
                    first: Box::new(Self::Pane { id: target }),
                    second: Box::new(Self::Pane { id: new }),
                };
                true
            }
            Self::Pane { .. } => false,
            Self::Split { first, second, .. } => {
                first.split(target, direction, new) || second.split(target, direction, new)
            }
        }
    }

    fn remove(self, target: PaneId) -> Option<Self> {
        match self {
            Self::Pane { id } if id == target => None,
            Self::Pane { .. } => Some(self),
            Self::Split {
                direction,
                ratio,
                first,
                second,
            } => match (first.remove(target), second.remove(target)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    direction,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
                (None, None) => None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_focus_close_and_persist() {
        let mut layout = PaneLayout::new();
        let first = layout.focused;
        let second = layout.split_focused(SplitDirection::Right);
        let third = layout.split_focused(SplitDirection::Down);
        assert_eq!(layout.panes(), vec![first, second, third]);
        layout.focus_relative(-1);
        assert_eq!(layout.focused, second);
        layout.close_focused().unwrap();
        assert_eq!(layout.panes(), vec![first, third]);
        let json = serde_json::to_string(&layout).unwrap();
        let restored: PaneLayout = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, layout);
    }

    #[test]
    fn final_pane_cannot_close() {
        assert_eq!(
            PaneLayout::new().close_focused(),
            Err(LayoutError::FinalPane)
        );
    }

    #[test]
    fn ratio_is_bounded() {
        let mut layout = PaneLayout::new();
        layout.split_focused(SplitDirection::Right);
        assert_eq!(
            layout.resize_split(&[], 0.95),
            Err(LayoutError::InvalidRatio)
        );
        layout.resize_split(&[], 0.6).unwrap();
    }
}

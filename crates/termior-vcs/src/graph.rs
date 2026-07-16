//! Pure commit graph lane allocation (FR-VCS-04 / NFR-10).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitNode {
    pub id: String,
    pub parents: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneCommit {
    pub id: String,
    pub lane: usize,
    /// Lanes that connect this row to its parents after the row is consumed.
    pub parent_lanes: Vec<usize>,
}

/// Allocate stable left-to-right lanes for commits ordered newest-to-oldest.
pub fn allocate_lanes(commits: &[CommitNode]) -> Vec<LaneCommit> {
    let mut active: Vec<String> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());
    for commit in commits {
        let lane = match active.iter().position(|id| id == &commit.id) {
            Some(index) => index,
            None => {
                active.push(commit.id.clone());
                active.len() - 1
            }
        };
        active.remove(lane);
        for (offset, parent) in commit.parents.iter().enumerate() {
            if let Some(existing) = active.iter().position(|id| id == parent) {
                // Keep the existing lane and only report it.
                if offset == 0 && existing > lane {
                    let parent = active.remove(existing);
                    active.insert(lane.min(active.len()), parent);
                }
            } else {
                active.insert((lane + offset).min(active.len()), parent.clone());
            }
        }
        let parent_lanes = commit
            .parents
            .iter()
            .filter_map(|parent| active.iter().position(|id| id == parent))
            .collect();
        rows.push(LaneCommit {
            id: commit.id.clone(),
            lane,
            parent_lanes,
        });
        active.retain(|id| !id.is_empty());
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_history_stays_in_lane_zero() {
        let nodes = vec![
            CommitNode {
                id: "c".into(),
                parents: vec!["b".into()],
            },
            CommitNode {
                id: "b".into(),
                parents: vec!["a".into()],
            },
            CommitNode {
                id: "a".into(),
                parents: vec![],
            },
        ];
        assert_eq!(
            allocate_lanes(&nodes)
                .iter()
                .map(|row| row.lane)
                .collect::<Vec<_>>(),
            vec![0, 0, 0]
        );
    }

    #[test]
    fn merge_allocates_two_parent_lanes() {
        let nodes = vec![
            CommitNode {
                id: "m".into(),
                parents: vec!["a".into(), "b".into()],
            },
            CommitNode {
                id: "a".into(),
                parents: vec!["base".into()],
            },
            CommitNode {
                id: "b".into(),
                parents: vec!["base".into()],
            },
            CommitNode {
                id: "base".into(),
                parents: vec![],
            },
        ];
        let rows = allocate_lanes(&nodes);
        assert_eq!(rows[0].parent_lanes.len(), 2);
        assert_ne!(rows[0].parent_lanes[0], rows[0].parent_lanes[1]);
        assert_eq!(rows[1].lane, 0);
    }
}

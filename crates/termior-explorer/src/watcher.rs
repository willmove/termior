use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsChange {
    pub paths: Vec<PathBuf>,
    pub kind: String,
}

pub struct WorkspaceWatcher {
    _watcher: RecommendedWatcher,
    receiver: Receiver<FsChange>,
}

impl std::fmt::Debug for WorkspaceWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceWatcher").finish_non_exhaustive()
    }
}

impl WorkspaceWatcher {
    pub fn watch(root: impl AsRef<Path>) -> notify::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                if let Ok(event) = result {
                    let Some(kind) = rescan_worthy_kind(&event.kind) else {
                        return;
                    };
                    let _ = sender.send(FsChange {
                        paths: event.paths,
                        kind: kind.to_owned(),
                    });
                }
            })?;
        watcher.watch(root.as_ref(), RecursiveMode::Recursive)?;
        Ok(Self {
            _watcher: watcher,
            receiver,
        })
    }

    pub fn try_changes(&self) -> Vec<FsChange> {
        self.receiver.try_iter().collect()
    }
}

/// Labels for event kinds that should trigger an explorer rebuild.
///
/// Pure [`EventKind::Access`] is ignored — access storms on large trees (e.g. home
/// directories) otherwise keep the UI stuck on indexing.
pub fn rescan_worthy_kind(kind: &EventKind) -> Option<&'static str> {
    match kind {
        EventKind::Create(_) => Some("create"),
        EventKind::Modify(_) => Some("modify"),
        EventKind::Remove(_) => Some("remove"),
        EventKind::Any => Some("any"),
        EventKind::Other => Some("other"),
        EventKind::Access(_) => None,
    }
}

/// Outcome of a debounced watch tick for explorer rebuilds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebouncedRescanAction {
    /// Debounce window has not elapsed.
    Wait,
    /// A scan is already running — queue at most one follow-up.
    QueuePending,
    /// Schedule a rebuild now.
    ScheduleNow,
}

/// Decide how to act when the watch poller wakes.
pub fn debounced_rescan_action(
    deadline_reached: bool,
    scan_in_progress: bool,
) -> DebouncedRescanAction {
    if !deadline_reached {
        return DebouncedRescanAction::Wait;
    }
    if scan_in_progress {
        DebouncedRescanAction::QueuePending
    } else {
        DebouncedRescanAction::ScheduleNow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind};

    #[test]
    fn access_events_are_not_rescan_worthy() {
        assert_eq!(
            rescan_worthy_kind(&EventKind::Access(AccessKind::Any)),
            None
        );
        assert_eq!(
            rescan_worthy_kind(&EventKind::Access(AccessKind::Read)),
            None
        );
    }

    #[test]
    fn create_modify_remove_are_rescan_worthy() {
        assert_eq!(
            rescan_worthy_kind(&EventKind::Create(CreateKind::Any)),
            Some("create")
        );
        assert_eq!(
            rescan_worthy_kind(&EventKind::Modify(ModifyKind::Any)),
            Some("modify")
        );
        assert_eq!(
            rescan_worthy_kind(&EventKind::Remove(RemoveKind::Any)),
            Some("remove")
        );
        assert_eq!(rescan_worthy_kind(&EventKind::Any), Some("any"));
    }

    #[test]
    fn debounced_rescan_queues_while_busy() {
        assert_eq!(
            debounced_rescan_action(false, false),
            DebouncedRescanAction::Wait
        );
        assert_eq!(
            debounced_rescan_action(true, true),
            DebouncedRescanAction::QueuePending
        );
        assert_eq!(
            debounced_rescan_action(true, false),
            DebouncedRescanAction::ScheduleNow
        );
    }
}

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
                    let kind = match event.kind {
                        EventKind::Create(_) => "create",
                        EventKind::Modify(_) => "modify",
                        EventKind::Remove(_) => "remove",
                        EventKind::Access(_) => "access",
                        EventKind::Other => "other",
                        EventKind::Any => "any",
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

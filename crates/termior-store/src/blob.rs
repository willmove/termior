//! Redacted output blobs. Checkpoint file snapshots stay byte-exact; this store is the
//! persistence boundary for agent output chunks that retention may later delete.

use crate::StreamingRedactor;
use std::path::PathBuf;

pub struct OutputBlobStore {
    root: PathBuf,
    redactor: StreamingRedactor,
}

impl OutputBlobStore {
    pub fn new(root: impl Into<PathBuf>, known_secrets: impl IntoIterator<Item = String>) -> Self {
        Self {
            root: root.into(),
            redactor: StreamingRedactor::new(known_secrets),
        }
    }

    /// Write `contents` under `blobs/` after streaming redaction.
    pub fn put(&self, name: &str, contents: &str) -> Result<PathBuf, crate::AtomicWriteError> {
        let path = self.root.join("blobs").join(safe_blob_name(name));
        self.redactor.write_atomic(&path, contents)?;
        Ok(path)
    }
}

fn safe_blob_name(name: &str) -> String {
    let safe = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .take(128)
        .collect::<String>();
    let safe = if safe.is_empty() {
        "blob".to_owned()
    } else {
        safe
    };
    format!("{safe}.txt")
}

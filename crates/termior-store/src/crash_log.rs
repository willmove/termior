//! Append-only crash log. Panic payloads are redacted before they touch disk.

use crate::StreamingRedactor;
use std::path::{Path, PathBuf};

pub const CRASH_LOG_FILE: &str = "Termior-crash.log";

/// Append one crash record under `directory`. Known secrets and credential shapes are removed first.
pub fn append_crash_log(
    directory: &Path,
    message: &str,
    known_secrets: impl IntoIterator<Item = String>,
) -> Result<PathBuf, std::io::Error> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let path = directory.join(CRASH_LOG_FILE);
    StreamingRedactor::new(known_secrets)
        .append_record(&path, &format!("[{timestamp}] {message}"))?;
    Ok(path)
}

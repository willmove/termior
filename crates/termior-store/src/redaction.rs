//! Streaming secret redaction used at persistence boundaries.

use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionReport {
    pub text: String,
    pub replacements: usize,
}

#[derive(Clone)]
pub struct StreamingRedactor {
    known: Vec<String>,
}
impl StreamingRedactor {
    pub fn new(known_secrets: impl IntoIterator<Item = String>) -> Self {
        Self {
            known: known_secrets
                .into_iter()
                .filter(|secret| !secret.is_empty())
                .collect(),
        }
    }
    pub fn redact(&self, input: &str) -> RedactionReport {
        let mut text = input.to_owned();
        let mut replacements = 0;
        for secret in &self.known {
            while let Some(index) = text.find(secret) {
                text.replace_range(index..index + secret.len(), "[REDACTED]");
                replacements += 1;
            }
        }
        for prefix in ["sk-", "ghp_", "xai-"] {
            let mut offset = 0;
            while let Some(relative) = text[offset..].find(prefix) {
                let start = offset + relative;
                // API-key prefixes start a token. Matching inside identifiers such as
                // `task-<timestamp>` corrupts task IDs and journal recovery references.
                if text[..start]
                    .chars()
                    .next_back()
                    .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                {
                    offset = start + prefix.len();
                    continue;
                }
                let end = text[start..]
                    .find(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | ',' | ';'))
                    .map(|length| start + length)
                    .unwrap_or(text.len());
                if end - start > prefix.len() + 8 {
                    text.replace_range(start..end, "[REDACTED]");
                    replacements += 1;
                    offset = start + 10;
                } else {
                    offset = end;
                }
            }
        }
        RedactionReport { text, replacements }
    }

    /// Atomically persist `text` after redaction. The returned report is what landed on disk.
    pub fn write_atomic(
        &self,
        path: &Path,
        text: &str,
    ) -> Result<RedactionReport, crate::AtomicWriteError> {
        let report = self.redact(text);
        crate::atomic_write(path, &report.text)?;
        Ok(report)
    }

    /// Append one redacted record. Parent directories are created when missing.
    pub fn append_record(
        &self,
        path: &Path,
        text: &str,
    ) -> Result<RedactionReport, std::io::Error> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let report = self.redact(text);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(file, "{}", report.text)?;
        file.sync_data()?;
        Ok(report)
    }
}

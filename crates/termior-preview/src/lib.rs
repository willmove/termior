//! Web preview discovery and tab lifecycle (FR-PREV). The platform-specific wry view is mounted by
//! the app only while a preview tab exists.

#![forbid(unsafe_code)]

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use url::Url;

#[derive(Debug)]
pub struct LocalhostDetector {
    regex: Regex,
    tail: String,
    seen: HashSet<String>,
}

impl Default for LocalhostDetector {
    fn default() -> Self {
        Self {
            regex: Regex::new(
                r#"https?://(?:localhost|127\.0\.0\.1|\[::1\])(?::[0-9]{1,5})?(?:/[^\s<>\"']*)?"#,
            )
            .expect("static preview regex"),
            tail: String::new(),
            seen: HashSet::new(),
        }
    }
}

impl LocalhostDetector {
    /// Feed the raw PTY output stream. Keeping a small tail handles URLs split across reads without
    /// consulting the terminal grid, so redrawing TUIs do not retrigger detections.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Url> {
        self.tail.push_str(&String::from_utf8_lossy(bytes));
        if self.tail.len() > 8192 {
            let split = self.tail.len() - 8192;
            let boundary = (split..=self.tail.len())
                .find(|index| self.tail.is_char_boundary(*index))
                .unwrap_or(self.tail.len());
            self.tail.drain(..boundary);
        }
        let mut found = Vec::new();
        for matched in self.regex.find_iter(&self.tail) {
            let candidate = matched
                .as_str()
                .trim_end_matches(['.', ',', ';', ':', ')', ']', '}']);
            if self.seen.insert(candidate.to_owned()) {
                if let Ok(url) = Url::parse(candidate) {
                    found.push(url);
                }
            }
        }
        found
    }

    pub fn clear_seen(&mut self) {
        self.seen.clear();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewBackend {
    Pending,
    Embedded,
    ExternalBrowser,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewTab {
    pub url: String,
    pub backend: PreviewBackend,
    pub last_error: Option<String>,
    /// Incremented only on explicit navigation; tab switches leave it untouched (INV-1).
    pub navigation_generation: u64,
}

impl PreviewTab {
    pub fn new(url: impl Into<String>) -> Result<Self, url::ParseError> {
        let url = url.into();
        Url::parse(&url)?;
        Ok(Self {
            url,
            backend: PreviewBackend::Pending,
            last_error: None,
            navigation_generation: 0,
        })
    }

    pub fn navigate(&mut self, url: impl Into<String>) -> Result<(), url::ParseError> {
        let url = url.into();
        Url::parse(&url)?;
        self.url = url;
        self.navigation_generation = self.navigation_generation.saturating_add(1);
        Ok(())
    }

    pub fn embedded_ready(&mut self) {
        self.backend = PreviewBackend::Embedded;
        self.last_error = None;
    }

    pub fn fallback(&mut self, error: impl Into<String>) {
        self.backend = PreviewBackend::ExternalBrowser;
        self.last_error = Some(error.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_local_urls_across_chunks_once() {
        let mut detector = LocalhostDetector::default();
        assert!(detector.feed(b"ready at http://local").is_empty());
        let urls = detector.feed(b"host:5173/app\n");
        assert_eq!(urls[0].as_str(), "http://localhost:5173/app");
        assert!(detector.feed(b"http://localhost:5173/app\n").is_empty());
    }

    #[test]
    fn ignores_public_and_malformed_urls() {
        let mut detector = LocalhostDetector::default();
        assert!(detector
            .feed(b"https://example.com http://localhost:99999")
            .is_empty());
    }

    #[test]
    fn fallback_keeps_url_and_state() {
        let mut tab = PreviewTab::new("http://localhost:3000").unwrap();
        tab.fallback("webview unavailable");
        assert_eq!(tab.backend, PreviewBackend::ExternalBrowser);
        assert_eq!(tab.url, "http://localhost:3000");
    }
}

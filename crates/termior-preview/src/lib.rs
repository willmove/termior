//! Preview-domain logic: Web preview discovery/lifecycle and native Markdown parsing.
//! Per ADR 0002 Termior no longer embeds a WebView — previews open in the system browser —
//! so this crate only owns URL detection, validation, the (now browser-only) tab state, and
//! Markdown parsing. GPUI rendering lives in the desktop application crate.

#![forbid(unsafe_code)]

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use url::Url;

mod markdown;

pub use markdown::{is_markdown_path, MarkdownDocument, MarkdownNode};

#[derive(Debug, thiserror::Error)]
pub enum PreviewUrlError {
    #[error("preview URL is empty")]
    Empty,
    #[error("invalid preview URL: {0}")]
    Invalid(#[from] url::ParseError),
    #[error("preview URL must use http or https, got {0}")]
    UnsupportedScheme(String),
    #[error("preview URL must include a host")]
    MissingHost,
}

/// Normalizes user-entered preview addresses, enforcing an HTTP/HTTPS-only boundary so the
/// system-browser opener never receives a non-web scheme.
pub fn normalize_preview_url(input: &str) -> Result<String, PreviewUrlError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(PreviewUrlError::Empty);
    }
    let candidate = if input.contains("://") {
        input.to_owned()
    } else {
        format!("http://{input}")
    };
    let url = Url::parse(&candidate)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(PreviewUrlError::UnsupportedScheme(url.scheme().to_owned()));
    }
    if url.host_str().is_none() {
        return Err(PreviewUrlError::MissingHost);
    }
    Ok(url.into())
}

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

    #[test]
    fn preview_url_input_accepts_localhost_without_a_scheme() {
        assert_eq!(
            normalize_preview_url("localhost:5173/app").unwrap(),
            "http://localhost:5173/app"
        );
        assert_eq!(
            normalize_preview_url("  https://127.0.0.1:3000  ").unwrap(),
            "https://127.0.0.1:3000/"
        );
    }

    #[test]
    fn preview_url_input_rejects_non_web_schemes() {
        assert!(matches!(
            normalize_preview_url("file:///etc/passwd"),
            Err(PreviewUrlError::UnsupportedScheme(scheme)) if scheme == "file"
        ));
        assert!(matches!(
            normalize_preview_url("   "),
            Err(PreviewUrlError::Empty)
        ));
    }
}

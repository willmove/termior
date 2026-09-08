//! Streaming secret redaction used at persistence boundaries.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionReport {
    pub text: String,
    pub replacements: usize,
}
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
}

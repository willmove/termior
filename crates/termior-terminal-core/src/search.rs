//! Terminal snapshot search and hyperlink detection (FR-TERM-04/08).

use regex::Regex;
use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub line: usize,
    pub char_range: Range<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalSearch {
    pub query: String,
    pub case_sensitive: bool,
    pub current: usize,
    pub hits: Vec<SearchHit>,
}

impl TerminalSearch {
    pub fn update(&mut self, snapshot: &str) {
        self.hits.clear();
        if self.query.is_empty() {
            self.current = 0;
            return;
        }
        let needle = if self.case_sensitive {
            self.query.clone()
        } else {
            self.query.to_lowercase()
        };
        for (line_index, line) in snapshot.lines().enumerate() {
            let haystack = if self.case_sensitive {
                line.to_owned()
            } else {
                line.to_lowercase()
            };
            let mut offset = 0;
            while let Some(found) = haystack[offset..].find(&needle) {
                let start_byte = offset + found;
                let end_byte = start_byte + needle.len();
                if line.is_char_boundary(start_byte) && line.is_char_boundary(end_byte) {
                    self.hits.push(SearchHit {
                        line: line_index,
                        char_range: line[..start_byte].chars().count()
                            ..line[..end_byte].chars().count(),
                    });
                }
                offset = end_byte.max(offset + 1);
                if offset >= haystack.len() {
                    break;
                }
            }
        }
        self.current = self.current.min(self.hits.len().saturating_sub(1));
    }

    pub fn next(&mut self, backwards: bool) -> Option<&SearchHit> {
        if self.hits.is_empty() {
            return None;
        }
        if backwards {
            self.current = (self.current + self.hits.len() - 1) % self.hits.len();
        } else {
            self.current = (self.current + 1) % self.hits.len();
        }
        self.hits.get(self.current)
    }
}

pub fn find_hyperlinks(text: &str) -> Vec<Range<usize>> {
    let regex = Regex::new(r#"https?://[^\s<>\"'`]+"#).expect("static hyperlink regex");
    regex
        .find_iter(text)
        .map(|matched| {
            let trimmed = matched
                .as_str()
                .trim_end_matches(['.', ',', ';', ':', ')', ']', '}']);
            matched.start()..matched.start() + trimmed.len()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_case_navigation_and_cjk() {
        let mut search = TerminalSearch {
            query: "hello".into(),
            ..Default::default()
        };
        search.update("Hello hello\n中文 hello");
        assert_eq!(search.hits.len(), 3);
        search.case_sensitive = true;
        search.update("Hello hello");
        assert_eq!(search.hits.len(), 1);
        search.query = "中文".into();
        search.update("a中文b");
        assert_eq!(search.hits[0].char_range, 1..3);
    }

    #[test]
    fn hyperlink_trims_sentence_punctuation() {
        let text = "open https://example.com/a). now";
        let links = find_hyperlinks(text);
        assert_eq!(&text[links[0].clone()], "https://example.com/a");
    }
}

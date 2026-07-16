use grep_regex::RegexMatcherBuilder;
use grep_searcher::{sinks, SearcherBuilder};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentMatch {
    pub path: PathBuf,
    pub line_number: u64,
    pub line: String,
    pub match_byte_ranges: Vec<std::ops::Range<usize>>,
}

#[derive(Debug, Clone)]
pub struct ContentSearch {
    case_sensitive: bool,
    smart_case: bool,
}

impl Default for ContentSearch {
    fn default() -> Self {
        Self {
            case_sensitive: false,
            smart_case: true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("invalid search pattern: {0}")]
    Pattern(String),
    #[error("search I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl ContentSearch {
    pub fn case_sensitive(mut self, enabled: bool) -> Self {
        self.case_sensitive = enabled;
        self
    }

    pub fn search_paths<I, P, F>(
        &self,
        query: &str,
        paths: I,
        mut on_match: F,
    ) -> Result<usize, SearchError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
        F: FnMut(ContentMatch) -> bool,
    {
        let case_insensitive =
            !self.case_sensitive && (!self.smart_case || !query.chars().any(char::is_uppercase));
        let matcher = RegexMatcherBuilder::new()
            .case_insensitive(case_insensitive)
            .build(query)
            .map_err(|error| SearchError::Pattern(error.to_string()))?;
        let mut count = 0usize;
        let mut keep_going = true;
        for path in paths {
            if !keep_going {
                break;
            }
            let path = path.as_ref().to_path_buf();
            let mut searcher = SearcherBuilder::new()
                .line_number(true)
                .binary_detection(grep_searcher::BinaryDetection::quit(b'\x00'))
                .build();
            let matcher_ref = &matcher;
            searcher.search_path(
                &matcher,
                &path,
                sinks::UTF8(|line_number, line: &str| {
                    use grep_matcher::Matcher;
                    let mut ranges = Vec::new();
                    let _ = matcher_ref.find_iter(line.as_bytes(), |matched| {
                        ranges.push(matched.start()..matched.end());
                        true
                    });
                    count += 1;
                    keep_going = on_match(ContentMatch {
                        path: path.clone(),
                        line_number,
                        line: line.trim_end_matches(['\r', '\n']).to_owned(),
                        match_byte_ranges: ranges,
                    });
                    Ok(keep_going)
                }),
            )?;
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn streams_groupable_matches_with_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        fs::write(&a, "zero\nneedle one\n").unwrap();
        fs::write(&b, "needle two\n").unwrap();
        let mut out = Vec::new();
        let count = ContentSearch::default()
            .search_paths("needle", [&a, &b], |hit| {
                out.push(hit);
                true
            })
            .unwrap();
        assert_eq!(count, 2);
        assert_eq!(out[0].line_number, 2);
        assert_eq!(out[1].line_number, 1);
        assert_eq!(out[0].match_byte_ranges[0], 0..6);
    }

    #[test]
    fn callback_can_cancel_stream() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "x\nx\nx\n").unwrap();
        let count = ContentSearch::default()
            .search_paths("x", [&path], |_| false)
            .unwrap();
        assert_eq!(count, 1);
    }
}

//! Incremental symbol/reference repo map (Stage C 3.3, FR-ACTX-05).
//!
//! Candidate files come from the explorer [`FileIndex`], so `.gitignore`, `.ignore`, global
//! excludes and hidden files are honoured exactly as in the file tree. Each candidate is then
//! filtered by the secret deny-list, root confinement after canonicalization, symlink and
//! hardlink rejection, and a size cap. The workspace root itself must be authorized.
//!
//! Outlines are keyed by the SHA-256 digest of the file content. A sweep trusts size + mtime only
//! outside a racy-timestamp window, re-hashes everything else, and parses only digests it has not
//! seen. A rename or copy therefore reuses the existing outline instead of parsing again.
//!
//! [`RepoMapService`] runs sweeps on a worker thread and applies results in small batches under a
//! write lock. A caller on the UI thread waits for at most one batch, never for a sweep.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use termior_editor::{OutlineExtractor, SyntaxLanguage};
use termior_explorer::FileIndex;
use termior_security::deny_list::{check_path, Direction};
use termior_security::workspace::WorkspaceAuthRegistry;

use crate::context_engine::{ContextError, HeuristicTokenEstimator, TokenEstimator};

pub use termior_editor::{OutlineSymbol, SymbolKind};

pub const DEFAULT_MAX_FILE_BYTES: u64 = 1024 * 1024;
pub const DEFAULT_MAX_FILES: usize = 20_000;
/// Filesystems with coarse timestamps (FAT 2s, HFS+ 1s, Linux jiffies) can store the same mtime
/// for two writes. Metadata is only trusted once the file was hashed this long after its mtime.
const RACY_WINDOW: Duration = Duration::from_secs(2);
const APPLY_BATCH: usize = 64;
const MAX_QUERY_TERMS: usize = 24;
const MAX_SYMBOLS_PER_ENTRY: usize = 12;
const MAX_SLICE_CANDIDATES: usize = 200;
const SEED_FILES: usize = 8;
/// A referenced name defined in more files than this is too generic to be a dependency edge.
const MAX_DEFINITION_SITES: usize = 3;
const LEXICAL_REFERENCE_CAP: usize = termior_editor::outline::MAX_REFERENCES_PER_FILE;

/// Shorter query words (`fix`, `add`, `new`) would match a word segment in most files.
const MIN_PARTIAL_TERM: usize = 4;
const SYMBOL_EXACT: u32 = 12;
const SYMBOL_PARTIAL: u32 = 4;
const PATH_EXACT: u32 = 6;
const PATH_PARTIAL: u32 = 2;
const REFERENCE_EXACT: u32 = 2;
const TERM_COVERAGE: u32 = 3;
const DEPENDENCY_EDGE: u32 = 2;
const RECENT_CHANGE: u32 = 2;

#[derive(Debug, Clone)]
pub struct RepoMapConfig {
    pub max_file_bytes: u64,
    pub max_files: usize,
    /// When set, the root must be authorized here. Files are always confined to the root.
    pub authorization: Option<WorkspaceAuthRegistry>,
}

impl Default for RepoMapConfig {
    fn default() -> Self {
        Self {
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_files: DEFAULT_MAX_FILES,
            authorization: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SymbolExtraction {
    TreeSitter,
    /// No definition table for the language: keyword heuristics plus identifier tokens.
    Lexical,
    /// Over [`RepoMapConfig::max_file_bytes`]; listed by path only and never read.
    LargeFile,
    /// Not UTF-8; listed by path only.
    Binary,
}

/// Content-addressed outline shared by every path whose bytes hash to the same digest.
#[derive(Debug, PartialEq, Eq)]
pub struct FileSymbols {
    pub language: &'static str,
    pub extraction: SymbolExtraction,
    pub has_error: bool,
    pub symbols: Vec<OutlineSymbol>,
    symbol_keys: Vec<String>,
    /// Deduplicated word segments of the symbol names, for partial matches.
    segment_keys: Vec<String>,
    /// Sorted, deduplicated hashes of normalized reference names. Names are not retained, so
    /// lexical tokens from config files never sit in memory as strings.
    reference_keys: Vec<u64>,
}

impl FileSymbols {
    fn path_only(language: &'static str, extraction: SymbolExtraction) -> Self {
        Self {
            language,
            extraction,
            has_error: false,
            symbols: Vec::new(),
            symbol_keys: Vec::new(),
            segment_keys: Vec::new(),
            reference_keys: Vec::new(),
        }
    }

    fn new(
        language: &'static str,
        extraction: SymbolExtraction,
        has_error: bool,
        symbols: Vec<OutlineSymbol>,
        references: impl IntoIterator<Item = String>,
    ) -> Self {
        let symbol_keys = symbols
            .iter()
            .map(|symbol| normalize(&symbol.name))
            .collect();
        let segment_keys = symbols
            .iter()
            .flat_map(|symbol| word_segments(&symbol.name))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let mut reference_keys = references
            .into_iter()
            .map(|name| key_hash(&normalize(&name)))
            .collect::<Vec<_>>();
        reference_keys.sort_unstable();
        reference_keys.dedup();
        Self {
            language,
            extraction,
            has_error,
            symbols,
            symbol_keys,
            segment_keys,
            reference_keys,
        }
    }

    pub fn reference_count(&self) -> usize {
        self.reference_keys.len()
    }
}

#[derive(Debug, Clone)]
pub struct RepoMapEntry {
    /// Canonical absolute path inside the map root.
    pub path: PathBuf,
    /// Root-relative path with `/` separators.
    pub relative: String,
    /// SHA-256 of the content; empty for [`SymbolExtraction::LargeFile`], which is never read.
    pub digest: String,
    pub bytes: u64,
    /// Map generation in which this path last changed content.
    pub generation: u64,
    pub outline: Arc<FileSymbols>,
    modified: Option<SystemTime>,
    hashed_at: SystemTime,
    path_keys: Vec<String>,
    path_segments: Vec<String>,
}

impl RepoMapEntry {
    pub fn symbols(&self) -> &[OutlineSymbol] {
        &self.outline.symbols
    }

    pub fn extraction(&self) -> SymbolExtraction {
        self.outline.extraction
    }

    pub fn defines(&self, name: &str) -> bool {
        self.symbols().iter().any(|symbol| symbol.name == name)
    }

    pub fn references(&self, name: &str) -> bool {
        self.outline
            .reference_keys
            .binary_search(&key_hash(&normalize(name)))
            .is_ok()
    }

    /// The file on disk still matches what was indexed. Stat-only unless the stored mtime is
    /// inside the racy window, in which case the bytes are hashed again.
    pub fn is_current(&self) -> bool {
        let Ok(metadata) = std::fs::symlink_metadata(&self.path) else {
            return false;
        };
        if !metadata.is_file() || metadata.len() != self.bytes {
            return false;
        }
        let modified = metadata.modified().ok();
        if modified != self.modified {
            return false;
        }
        if self.digest.is_empty() || !is_racy(modified, self.hashed_at) {
            return true;
        }
        std::fs::read(&self.path).is_ok_and(|bytes| digest_hex(&bytes) == self.digest)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshStats {
    pub listed: usize,
    /// New content digests parsed in this refresh.
    pub parsed: usize,
    /// Changed or renamed paths whose digest already had an outline.
    pub reused: usize,
    /// Trusted by size and mtime without reading.
    pub unchanged: usize,
    /// Re-read because metadata changed or was racy, but the digest was the same.
    pub rehashed: usize,
    pub removed: usize,
    pub large: usize,
    pub binary: usize,
    pub denied: usize,
    pub unauthorized: usize,
    pub truncated: bool,
}

/// A token-bounded, relevance-ranked view of the map for one task query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoSlice {
    pub generation: u64,
    pub budget: u64,
    pub tokens: u64,
    pub terms: Vec<String>,
    pub entries: Vec<RepoSliceEntry>,
    /// Files with a non-zero relevance score.
    pub candidates: usize,
    /// Relevant files whose disk content no longer matches the index; never sent as current.
    pub stale_excluded: Vec<String>,
    /// Relevant files left out because the budget ran out.
    pub budget_excluded: usize,
    /// The background index had not finished its latest sweep when this slice was taken.
    pub indexing: bool,
}

impl RepoSlice {
    pub fn render(&self) -> String {
        self.entries
            .iter()
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSliceEntry {
    pub path: PathBuf,
    pub relative: String,
    pub score: u32,
    pub language: &'static str,
    pub extraction: SymbolExtraction,
    pub symbols: Vec<OutlineSymbol>,
    pub omitted_symbols: usize,
    /// Pulled in because a top-ranked file references a symbol it defines.
    pub dependency: bool,
    pub recently_changed: bool,
    pub text: String,
}

pub struct RepoMap {
    root: PathBuf,
    root_label: String,
    config: RepoMapConfig,
    ids: HashMap<String, u32>,
    slots: Vec<Option<Arc<RepoMapEntry>>>,
    free: Vec<u32>,
    symbol_index: HashMap<String, Vec<u32>>,
    definition_index: HashMap<u64, Vec<u32>>,
    reference_index: HashMap<u64, Vec<u32>>,
    path_index: HashMap<String, Vec<u32>>,
    symbol_segment_index: HashMap<String, Vec<u32>>,
    path_segment_index: HashMap<String, Vec<u32>>,
    by_digest: HashMap<String, Arc<FileSymbols>>,
    generation: u64,
    swept_generation: Option<u64>,
    truncated: bool,
}

impl std::fmt::Debug for RepoMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepoMap")
            .field("root", &self.root)
            .field("files", &self.ids.len())
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl RepoMap {
    /// An empty map confined to `root`. Nothing is read until [`Self::refresh`].
    pub fn new(root: &Path, mut config: RepoMapConfig) -> Result<Self, ContextError> {
        let canonical = std::fs::canonicalize(root)?;
        if !canonical.is_dir() {
            return Err(ContextError::OutsideWorkspace(canonical));
        }
        if let Some(registry) = config.authorization.as_mut() {
            // Windows canonicalization adds a `\\?\` prefix; carry a grant for the user-facing
            // path over to that twin, as the tool executor does.
            if registry.is_authorized(&root.to_string_lossy()) {
                registry.authorize(&canonical.to_string_lossy());
            }
            if !registry.is_authorized(&canonical.to_string_lossy()) {
                return Err(ContextError::OutsideWorkspace(canonical));
            }
        }
        Ok(Self {
            root_label: canonical.to_string_lossy().replace('\\', "/"),
            root: canonical,
            config,
            ids: HashMap::new(),
            slots: Vec::new(),
            free: Vec::new(),
            symbol_index: HashMap::new(),
            definition_index: HashMap::new(),
            reference_index: HashMap::new(),
            path_index: HashMap::new(),
            symbol_segment_index: HashMap::new(),
            path_segment_index: HashMap::new(),
            by_digest: HashMap::new(),
            generation: 0,
            swept_generation: None,
            truncated: false,
        })
    }

    /// [`Self::new`] followed by a full sweep.
    pub fn build(root: &Path, config: RepoMapConfig) -> Result<Self, ContextError> {
        let mut map = Self::new(root, config)?;
        map.refresh()?;
        Ok(map)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The last sweep stopped at [`RepoMapConfig::max_files`].
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn entry(&self, relative: &str) -> Option<&RepoMapEntry> {
        let id = *self.ids.get(relative)?;
        self.slot(id)
    }

    pub fn entries(&self) -> impl Iterator<Item = &RepoMapEntry> {
        self.slots.iter().filter_map(|slot| slot.as_deref())
    }

    /// Full sweep of the file index. Unchanged files are not read; unchanged digests are not
    /// parsed; files that disappeared, became ignored, or became unauthorized are dropped.
    pub fn refresh(&mut self) -> Result<RefreshStats, ContextError> {
        let state = self.scan_state();
        let mut extractor = OutlineExtractor::new();
        let cancel = AtomicBool::new(false);
        let stats = sweep(&state, &mut extractor, &cancel, |changes| {
            self.apply(changes)
        })?;
        self.finish_sweep(stats.truncated);
        Ok(stats)
    }

    /// Re-check specific paths, such as watcher events or files an agent just wrote. Falls back
    /// to a full sweep when a path is new, because only the walk knows its ignore status.
    pub fn refresh_paths(&mut self, paths: &[PathBuf]) -> Result<RefreshStats, ContextError> {
        let state = self.scan_state();
        let mut extractor = OutlineExtractor::new();
        match scan_paths(&state, &mut extractor, paths) {
            Some((changes, stats)) => {
                self.apply(changes);
                self.collect_unused_outlines();
                Ok(stats)
            }
            None => self.refresh(),
        }
    }

    /// Rank files against `query` and render as many as fit in `token_budget`.
    pub fn slice(&self, query: &str, token_budget: u64) -> RepoSlice {
        let terms = query_terms(query);
        let mut slice = RepoSlice {
            generation: self.generation,
            budget: token_budget,
            terms: terms.clone(),
            ..RepoSlice::default()
        };
        if terms.is_empty() || token_budget == 0 {
            return slice;
        }
        let mut scores = self.score_terms(&terms);
        self.add_dependency_edges(&mut scores);
        if let Some(baseline) = self.swept_generation {
            for (id, score) in scores.iter_mut() {
                if self
                    .slot(*id)
                    .is_some_and(|entry| entry.generation > baseline)
                {
                    score.points += RECENT_CHANGE;
                    score.recent = true;
                }
            }
        }
        let mut ranked = scores
            .into_iter()
            .filter_map(|(id, score)| Some((self.slot(id)?, score)))
            .filter(|(_, score)| score.points > 0)
            .collect::<Vec<_>>();
        ranked.sort_by(|(a, sa), (b, sb)| {
            sb.points
                .cmp(&sa.points)
                .then_with(|| a.relative.cmp(&b.relative))
        });
        slice.candidates = ranked.len();
        let estimator = HeuristicTokenEstimator;
        let mut used = 0u64;
        for (index, (entry, score)) in ranked.iter().enumerate() {
            if index >= MAX_SLICE_CANDIDATES || used >= token_budget {
                slice.budget_excluded += ranked.len() - index;
                break;
            }
            if !entry.is_current() {
                slice.stale_excluded.push(entry.relative.clone());
                continue;
            }
            let header = entry_header(entry);
            let mut cost = estimator.estimate(&format!("{header}\n")).tokens;
            if used + cost > token_budget {
                slice.budget_excluded += 1;
                continue;
            }
            let mut lines = vec![header];
            let mut symbols = Vec::new();
            let ordered = ordered_symbols(entry, &terms);
            for symbol in ordered.iter().take(MAX_SYMBOLS_PER_ENTRY) {
                let line = symbol_line(symbol);
                let line_cost = estimator.estimate(&format!("{line}\n")).tokens;
                if used + cost + line_cost > token_budget {
                    break;
                }
                cost += line_cost;
                lines.push(line);
                symbols.push((*symbol).clone());
            }
            let omitted = ordered.len() - symbols.len();
            if omitted > 0 {
                let line = format!("  … {omitted} more symbols");
                let line_cost = estimator.estimate(&format!("{line}\n")).tokens;
                if used + cost + line_cost <= token_budget {
                    cost += line_cost;
                    lines.push(line);
                }
            }
            used += cost;
            slice.entries.push(RepoSliceEntry {
                path: entry.path.clone(),
                relative: entry.relative.clone(),
                score: score.points,
                language: entry.outline.language,
                extraction: entry.outline.extraction,
                symbols,
                omitted_symbols: omitted,
                dependency: score.dependency,
                recently_changed: score.recent,
                text: lines.join("\n"),
            });
        }
        slice.tokens = used;
        slice
    }

    fn slot(&self, id: u32) -> Option<&RepoMapEntry> {
        self.slots.get(id as usize)?.as_deref()
    }

    fn score_terms(&self, terms: &[String]) -> HashMap<u32, Score> {
        let mut scores = HashMap::<u32, Score>::new();
        for (index, term) in terms.iter().enumerate() {
            let mut hits = HashMap::<u32, [u32; 3]>::new();
            let mut hit = |ids: &[u32], slot: usize, points: u32| {
                for id in ids {
                    let cell = &mut hits.entry(*id).or_default()[slot];
                    *cell = (*cell).max(points);
                }
            };
            if let Some(ids) = self.symbol_index.get(term) {
                hit(ids, 0, SYMBOL_EXACT);
            }
            if let Some(ids) = self.path_index.get(term) {
                hit(ids, 1, PATH_EXACT);
            }
            if term.chars().count() >= MIN_PARTIAL_TERM {
                if let Some(ids) = self.symbol_segment_index.get(term) {
                    hit(ids, 0, SYMBOL_PARTIAL);
                }
                if let Some(ids) = self.path_segment_index.get(term) {
                    hit(ids, 1, PATH_PARTIAL);
                }
            }
            if let Some(ids) = self.reference_index.get(&key_hash(term)) {
                hit(ids, 2, REFERENCE_EXACT);
            }
            for (id, [symbol, path, reference]) in hits {
                let score = scores.entry(id).or_default();
                score.points += symbol + path + reference;
                score.terms |= 1 << index;
            }
        }
        for score in scores.values_mut() {
            score.points += TERM_COVERAGE * score.terms.count_ones().saturating_sub(1);
        }
        scores
    }

    /// Files defining specific names that the top-ranked files reference are their dependencies.
    fn add_dependency_edges(&self, scores: &mut HashMap<u32, Score>) {
        let mut seeds = scores
            .iter()
            .map(|(id, score)| (*id, score.points))
            .collect::<Vec<_>>();
        seeds.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        seeds.truncate(SEED_FILES);
        let mut edges = HashMap::<u32, u32>::new();
        for (seed, _) in &seeds {
            let Some(entry) = self.slot(*seed) else {
                continue;
            };
            for reference in &entry.outline.reference_keys {
                let Some(ids) = self.definition_index.get(reference) else {
                    continue;
                };
                if ids.len() > MAX_DEFINITION_SITES {
                    continue;
                }
                for id in ids.iter().filter(|id| *id != seed) {
                    *edges.entry(*id).or_default() += 1;
                }
            }
        }
        for (id, count) in edges {
            let score = scores.entry(id).or_default();
            score.points += DEPENDENCY_EDGE * count.min(3);
            score.dependency = true;
        }
    }

    fn scan_state(&self) -> ScanState {
        ScanState {
            root: self.root.clone(),
            root_label: self.root_label.clone(),
            config: self.config.clone(),
            known: self
                .entries()
                .map(|entry| {
                    (
                        entry.relative.clone(),
                        Known {
                            digest: entry.digest.clone(),
                            bytes: entry.bytes,
                            modified: entry.modified,
                            hashed_at: entry.hashed_at,
                            extraction: entry.outline.extraction,
                        },
                    )
                })
                .collect(),
            outlines: self.by_digest.clone(),
        }
    }

    fn apply(&mut self, changes: Vec<Change>) {
        let next = self.generation + 1;
        let mut content_changed = false;
        for change in changes {
            match change {
                Change::Upsert(mut entry) => {
                    content_changed = true;
                    self.remove_entry(&entry.relative);
                    entry.generation = next;
                    self.insert_entry(*entry);
                }
                Change::Touch {
                    relative,
                    modified,
                    hashed_at,
                } => {
                    let Some(id) = self.ids.get(&relative).copied() else {
                        continue;
                    };
                    if let Some(Some(slot)) = self.slots.get_mut(id as usize) {
                        let mut updated = RepoMapEntry::clone(slot);
                        updated.modified = modified;
                        updated.hashed_at = hashed_at;
                        *slot = Arc::new(updated);
                    }
                }
                Change::Remove(relative) => {
                    content_changed |= self.remove_entry(&relative);
                }
            }
        }
        if content_changed {
            self.generation = next;
        }
    }

    fn finish_sweep(&mut self, truncated: bool) {
        self.truncated = truncated;
        self.collect_unused_outlines();
        if self.swept_generation.is_none() {
            self.swept_generation = Some(self.generation);
        }
    }

    fn collect_unused_outlines(&mut self) {
        self.by_digest
            .retain(|_, outline| Arc::strong_count(outline) > 1);
    }

    fn insert_entry(&mut self, entry: RepoMapEntry) {
        let id = match self.free.pop() {
            Some(id) => id,
            None => {
                self.slots.push(None);
                (self.slots.len() - 1) as u32
            }
        };
        let keys = EntryKeys::of(&entry);
        for key in keys.symbols {
            self.symbol_index.entry(key).or_default().push(id);
        }
        for key in keys.definitions {
            self.definition_index.entry(key).or_default().push(id);
        }
        for key in keys.references {
            self.reference_index.entry(key).or_default().push(id);
        }
        for key in keys.paths {
            self.path_index.entry(key).or_default().push(id);
        }
        for key in keys.symbol_segments {
            self.symbol_segment_index.entry(key).or_default().push(id);
        }
        for key in keys.path_segments {
            self.path_segment_index.entry(key).or_default().push(id);
        }
        if !entry.digest.is_empty() {
            self.by_digest
                .insert(entry.digest.clone(), Arc::clone(&entry.outline));
        }
        self.ids.insert(entry.relative.clone(), id);
        self.slots[id as usize] = Some(Arc::new(entry));
    }

    fn remove_entry(&mut self, relative: &str) -> bool {
        let Some(id) = self.ids.remove(relative) else {
            return false;
        };
        let Some(entry) = self.slots.get_mut(id as usize).and_then(Option::take) else {
            return false;
        };
        let keys = EntryKeys::of(&entry);
        for key in keys.symbols {
            remove_posting(&mut self.symbol_index, key, id);
        }
        for key in keys.definitions {
            remove_posting(&mut self.definition_index, key, id);
        }
        for key in keys.references {
            remove_posting(&mut self.reference_index, key, id);
        }
        for key in keys.paths {
            remove_posting(&mut self.path_index, key, id);
        }
        for key in keys.symbol_segments {
            remove_posting(&mut self.symbol_segment_index, key, id);
        }
        for key in keys.path_segments {
            remove_posting(&mut self.path_segment_index, key, id);
        }
        self.free.push(id);
        true
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Score {
    points: u32,
    terms: u32,
    dependency: bool,
    recent: bool,
}

struct EntryKeys {
    symbols: HashSet<String>,
    definitions: HashSet<u64>,
    references: Vec<u64>,
    paths: HashSet<String>,
    symbol_segments: Vec<String>,
    path_segments: Vec<String>,
}

impl EntryKeys {
    fn of(entry: &RepoMapEntry) -> Self {
        let symbols = entry
            .outline
            .symbol_keys
            .iter()
            .filter(|key| !key.is_empty())
            .cloned()
            .collect::<HashSet<_>>();
        let definitions = symbols.iter().map(|key| key_hash(key)).collect();
        Self {
            symbols,
            definitions,
            references: entry.outline.reference_keys.clone(),
            paths: entry.path_keys.iter().cloned().collect(),
            symbol_segments: entry.outline.segment_keys.clone(),
            path_segments: entry.path_segments.clone(),
        }
    }
}

fn remove_posting<K: Hash + Eq>(index: &mut HashMap<K, Vec<u32>>, key: K, id: u32) {
    if let Some(ids) = index.get_mut(&key) {
        if let Some(position) = ids.iter().position(|candidate| *candidate == id) {
            ids.swap_remove(position);
        }
        if ids.is_empty() {
            index.remove(&key);
        }
    }
}

struct Known {
    digest: String,
    bytes: u64,
    modified: Option<SystemTime>,
    hashed_at: SystemTime,
    extraction: SymbolExtraction,
}

/// Owned copy of what a scan needs, so the IO and parse phase holds no lock on the map.
struct ScanState {
    root: PathBuf,
    root_label: String,
    config: RepoMapConfig,
    known: HashMap<String, Known>,
    outlines: HashMap<String, Arc<FileSymbols>>,
}

enum Change {
    Upsert(Box<RepoMapEntry>),
    Touch {
        relative: String,
        modified: Option<SystemTime>,
        hashed_at: SystemTime,
    },
    Remove(String),
}

enum Inspection {
    Unchanged,
    Changed(Change),
    Skipped,
}

struct Scanner<'a> {
    state: &'a ScanState,
    extractor: &'a mut OutlineExtractor,
    parsed: HashMap<String, Arc<FileSymbols>>,
}

impl Scanner<'_> {
    fn inspect(&mut self, relative: &str, path: &Path, stats: &mut RefreshStats) -> Inspection {
        let config = &self.state.config;
        if check_path(
            &format!("{}/{relative}", self.state.root_label),
            Direction::Read,
        )
        .is_some()
        {
            stats.denied += 1;
            return Inspection::Skipped;
        }
        let Ok(metadata) = std::fs::symlink_metadata(path) else {
            return Inspection::Skipped;
        };
        if metadata.file_type().is_symlink() {
            stats.unauthorized += 1;
            return Inspection::Skipped;
        }
        if !metadata.is_file() {
            return Inspection::Skipped;
        }
        let modified = metadata.modified().ok();
        let known = self.state.known.get(relative);
        let language = SyntaxLanguage::detect(relative);
        if metadata.len() > config.max_file_bytes {
            stats.large += 1;
            if known.is_some_and(|known| {
                known.extraction == SymbolExtraction::LargeFile
                    && known.bytes == metadata.len()
                    && known.modified == modified
            }) {
                return Inspection::Unchanged;
            }
            return Inspection::Changed(Change::Upsert(Box::new(self.entry(
                relative,
                path,
                String::new(),
                metadata.len(),
                modified,
                SystemTime::now(),
                Arc::new(FileSymbols::path_only(
                    language.id(),
                    SymbolExtraction::LargeFile,
                )),
            ))));
        }
        if let Some(known) = known {
            if known.extraction != SymbolExtraction::LargeFile
                && known.bytes == metadata.len()
                && known.modified == modified
                && !is_racy(modified, known.hashed_at)
            {
                stats.unchanged += 1;
                return Inspection::Unchanged;
            }
        }
        let hashed_at = SystemTime::now();
        let Some(bytes) = self.read_confined(path, stats) else {
            return Inspection::Skipped;
        };
        if bytes.len() as u64 > config.max_file_bytes {
            stats.large += 1;
            return Inspection::Changed(Change::Upsert(Box::new(self.entry(
                relative,
                path,
                String::new(),
                bytes.len() as u64,
                modified,
                hashed_at,
                Arc::new(FileSymbols::path_only(
                    language.id(),
                    SymbolExtraction::LargeFile,
                )),
            ))));
        }
        let digest = digest_hex(&bytes);
        if known.is_some_and(|known| known.digest == digest) {
            stats.rehashed += 1;
            return Inspection::Changed(Change::Touch {
                relative: relative.to_owned(),
                modified,
                hashed_at,
            });
        }
        let outline = match self
            .state
            .outlines
            .get(&digest)
            .or_else(|| self.parsed.get(&digest))
        {
            Some(outline) => {
                stats.reused += 1;
                Arc::clone(outline)
            }
            None => {
                let outline = Arc::new(extract(self.extractor, language, &bytes));
                if outline.extraction == SymbolExtraction::Binary {
                    stats.binary += 1;
                } else {
                    stats.parsed += 1;
                }
                self.parsed.insert(digest.clone(), Arc::clone(&outline));
                outline
            }
        };
        Inspection::Changed(Change::Upsert(Box::new(self.entry(
            relative,
            path,
            digest,
            bytes.len() as u64,
            modified,
            hashed_at,
            outline,
        ))))
    }

    /// Open once, then check confinement and link count on that handle before reading from it.
    fn read_confined(&self, path: &Path, stats: &mut RefreshStats) -> Option<Vec<u8>> {
        let canonical = std::fs::canonicalize(path).ok()?;
        if !canonical.starts_with(&self.state.root) {
            stats.unauthorized += 1;
            return None;
        }
        let file = std::fs::File::open(&canonical).ok()?;
        if link_count(&file) > 1 {
            stats.unauthorized += 1;
            return None;
        }
        let mut bytes = Vec::new();
        file.take(self.state.config.max_file_bytes + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        Some(bytes)
    }

    #[allow(clippy::too_many_arguments)]
    fn entry(
        &self,
        relative: &str,
        path: &Path,
        digest: String,
        bytes: u64,
        modified: Option<SystemTime>,
        hashed_at: SystemTime,
        outline: Arc<FileSymbols>,
    ) -> RepoMapEntry {
        RepoMapEntry {
            path: path.to_path_buf(),
            relative: relative.to_owned(),
            digest,
            bytes,
            generation: 0,
            outline,
            modified,
            hashed_at,
            path_keys: path_keys(relative),
            path_segments: path_segments(relative),
        }
    }
}

fn sweep(
    state: &ScanState,
    extractor: &mut OutlineExtractor,
    cancel: &AtomicBool,
    mut apply: impl FnMut(Vec<Change>),
) -> Result<RefreshStats, ContextError> {
    let index = FileIndex::build(&state.root, false)
        .map_err(|error| ContextError::Walk(error.to_string()))?;
    let mut files = index
        .entries()
        .iter()
        .filter(|entry| !entry.is_dir)
        .map(|entry| (entry.relative.clone(), entry.path.clone()))
        .collect::<Vec<_>>();
    let mut stats = RefreshStats {
        truncated: files.len() > state.config.max_files,
        ..RefreshStats::default()
    };
    files.truncate(state.config.max_files);
    stats.listed = files.len();
    let mut scanner = Scanner {
        state,
        extractor,
        parsed: HashMap::new(),
    };
    let mut seen = HashSet::with_capacity(files.len());
    let mut batch = Vec::new();
    for (relative, path) in files {
        if cancel.load(Ordering::Relaxed) {
            return Err(ContextError::Cancelled);
        }
        match scanner.inspect(&relative, &path, &mut stats) {
            Inspection::Unchanged => {
                seen.insert(relative);
            }
            Inspection::Changed(change) => {
                seen.insert(relative);
                batch.push(change);
                if batch.len() >= APPLY_BATCH {
                    apply(std::mem::take(&mut batch));
                }
            }
            Inspection::Skipped => {}
        }
    }
    for relative in state.known.keys() {
        if !seen.contains(relative) {
            stats.removed += 1;
            batch.push(Change::Remove(relative.clone()));
        }
    }
    if !batch.is_empty() {
        apply(batch);
    }
    Ok(stats)
}

/// `None` means a path is new to the map and only a full sweep can decide whether it is ignored.
fn scan_paths(
    state: &ScanState,
    extractor: &mut OutlineExtractor,
    paths: &[PathBuf],
) -> Option<(Vec<Change>, RefreshStats)> {
    let mut targets = Vec::new();
    for path in paths {
        let Some(relative) = relative_to(&state.root, path) else {
            continue;
        };
        if relative.is_empty() {
            return None;
        }
        if state.known.contains_key(&relative) {
            targets.push(relative);
            continue;
        }
        let prefix = format!("{relative}/");
        let under = state
            .known
            .keys()
            .filter(|known| known.starts_with(&prefix))
            .cloned()
            .collect::<Vec<_>>();
        if native_path(&state.root, &relative).exists() {
            return None;
        }
        targets.extend(under);
    }
    targets.sort();
    targets.dedup();
    let mut stats = RefreshStats::default();
    let mut scanner = Scanner {
        state,
        extractor,
        parsed: HashMap::new(),
    };
    let mut changes = Vec::new();
    for relative in targets {
        let path = native_path(&state.root, &relative);
        stats.listed += 1;
        match scanner.inspect(&relative, &path, &mut stats) {
            Inspection::Unchanged => {}
            Inspection::Changed(change) => changes.push(change),
            Inspection::Skipped => {
                stats.removed += 1;
                changes.push(Change::Remove(relative));
            }
        }
    }
    Some((changes, stats))
}

fn extract(
    extractor: &mut OutlineExtractor,
    language: SyntaxLanguage,
    bytes: &[u8],
) -> FileSymbols {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return FileSymbols::path_only(language.id(), SymbolExtraction::Binary);
    };
    if let Some(outline) = extractor.extract(language, text) {
        return FileSymbols::new(
            language.id(),
            SymbolExtraction::TreeSitter,
            outline.has_error,
            outline.symbols,
            outline.references,
        );
    }
    let symbols = if language == SyntaxLanguage::PlainText {
        lexical_symbols(text)
    } else {
        Vec::new()
    };
    let references = lexical_references(text, &symbols);
    FileSymbols::new(
        language.id(),
        SymbolExtraction::Lexical,
        false,
        symbols,
        references,
    )
}

/// Keyword heuristics for languages without a grammar (Ruby, Kotlin, Swift, shell, ...).
fn lexical_symbols(text: &str) -> Vec<OutlineSymbol> {
    let mut seen = HashSet::new();
    let mut symbols = Vec::new();
    for (row, line) in text.lines().enumerate() {
        let words = line
            .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>();
        for pair in words.windows(2) {
            let kind = match pair[0] {
                "fn" | "def" | "func" | "fun" | "function" | "sub" => SymbolKind::Function,
                "class" | "object" => SymbolKind::Class,
                "struct" => SymbolKind::Struct,
                "enum" => SymbolKind::Enum,
                "trait" | "protocol" => SymbolKind::Trait,
                "interface" => SymbolKind::Interface,
                "module" | "mod" | "namespace" => SymbolKind::Module,
                _ => continue,
            };
            let name = pair[1];
            if name.chars().next().is_some_and(char::is_alphabetic) && seen.insert(name) {
                let signature = line.split_whitespace().collect::<Vec<_>>().join(" ");
                symbols.push(OutlineSymbol {
                    name: name.to_owned(),
                    kind,
                    container: None,
                    signature: signature.chars().take(160).collect(),
                    start_line: row as u32 + 1,
                    end_line: row as u32 + 1,
                });
            }
        }
    }
    symbols
}

fn lexical_references(text: &str, symbols: &[OutlineSymbol]) -> Vec<String> {
    let defined = symbols
        .iter()
        .map(|symbol| symbol.name.as_str())
        .collect::<HashSet<_>>();
    let mut references = text
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .filter(|word| (3..=64).contains(&word.len()) && !defined.contains(word))
        .filter(|word| word.chars().next().is_some_and(char::is_alphabetic))
        .collect::<HashSet<_>>()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    references.sort();
    references.truncate(LEXICAL_REFERENCE_CAP);
    references
}

fn ordered_symbols<'a>(entry: &'a RepoMapEntry, terms: &[String]) -> Vec<&'a OutlineSymbol> {
    let mut indexed = entry
        .outline
        .symbols
        .iter()
        .zip(&entry.outline.symbol_keys)
        .map(|(symbol, key)| {
            let segments = word_segments(&symbol.name);
            let matched = terms.iter().any(|term| {
                key == term || (term.chars().count() >= MIN_PARTIAL_TERM && segments.contains(term))
            });
            (!matched, symbol)
        })
        .collect::<Vec<_>>();
    indexed.sort_by_key(|(unmatched, symbol)| (*unmatched, symbol.start_line));
    indexed.into_iter().map(|(_, symbol)| symbol).collect()
}

fn entry_header(entry: &RepoMapEntry) -> String {
    let note = match entry.outline.extraction {
        SymbolExtraction::TreeSitter if entry.outline.has_error => {
            format!("{}, partial parse", entry.outline.language)
        }
        SymbolExtraction::TreeSitter => entry.outline.language.to_owned(),
        SymbolExtraction::Lexical => format!("{}, lexical", entry.outline.language),
        SymbolExtraction::LargeFile => "large file, not parsed".to_owned(),
        SymbolExtraction::Binary => "binary".to_owned(),
    };
    format!("{} ({note})", entry.relative)
}

fn symbol_line(symbol: &OutlineSymbol) -> String {
    match &symbol.container {
        Some(container) => format!(
            "  L{} [{container}] {}",
            symbol.start_line, symbol.signature
        ),
        None => format!("  L{} {}", symbol.start_line, symbol.signature),
    }
}

const STOPWORDS: &[&str] = &[
    "the", "and", "or", "of", "to", "in", "on", "at", "by", "for", "with", "from", "into", "is",
    "are", "be", "was", "it", "its", "this", "that", "these", "those", "as", "not", "no", "but",
    "if", "so", "we", "you", "me", "my", "our", "can", "how", "what", "why", "when", "where",
    "which", "who", "please", "should", "would", "could", "does", "do", "did", "have", "has",
    "had", "will", "then", "than", "there", "here", "all", "any", "some",
];

fn query_terms(query: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    query
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .map(normalize)
        .filter(|term| term.chars().count() >= 2 && !STOPWORDS.contains(&term.as_str()))
        .filter(|term| seen.insert(term.clone()))
        .take(MAX_QUERY_TERMS)
        .collect()
}

/// Case- and separator-insensitive key: `AgentRuntime`, `agent_runtime` and `agent-runtime`
/// all become `agentruntime`.
fn normalize(name: &str) -> String {
    name.chars()
        .filter(|ch| *ch != '_' && *ch != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

fn path_keys(relative: &str) -> Vec<String> {
    let mut keys = HashSet::new();
    let components = relative.split('/').collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let component = if index + 1 == components.len() {
            component.split('.').next().unwrap_or(component)
        } else {
            component
        };
        let key = normalize(component);
        if !key.is_empty() {
            keys.insert(key);
        }
    }
    keys.into_iter().collect()
}

/// Lowercase words of an identifier or path component, split at separators and camelCase
/// boundaries: `parseHTTPHeader` gives `parse`, `http`, `header`. Partial matches are whole
/// words, so a lookup is one hash probe instead of a scan of every key.
fn word_segments(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    for part in name.split(|ch: char| !ch.is_alphanumeric()) {
        let chars = part.chars().collect::<Vec<_>>();
        let mut start = 0;
        for index in 1..chars.len() {
            let (previous, current) = (chars[index - 1], chars[index]);
            let next_is_lower = chars.get(index + 1).is_some_and(|ch| ch.is_lowercase());
            let boundary = (previous.is_lowercase() && current.is_uppercase())
                || (previous.is_uppercase() && current.is_uppercase() && next_is_lower)
                || (previous.is_alphabetic() != current.is_alphabetic());
            if boundary {
                words.push(chars[start..index].iter().collect::<String>());
                start = index;
            }
        }
        if start < chars.len() {
            words.push(chars[start..].iter().collect::<String>());
        }
    }
    let mut seen = HashSet::new();
    words
        .into_iter()
        .map(|word| word.to_lowercase())
        .filter(|word| seen.insert(word.clone()))
        .collect()
}

fn path_segments(relative: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    relative
        .split('/')
        .flat_map(word_segments)
        .filter(|word| seen.insert(word.clone()))
        .collect()
}

fn key_hash(key: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

fn digest_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn is_racy(modified: Option<SystemTime>, hashed_at: SystemTime) -> bool {
    match modified {
        Some(modified) => modified + RACY_WINDOW >= hashed_at,
        None => true,
    }
}

fn native_path(root: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .filter(|part| !part.is_empty())
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

fn relative_to(root: &Path, path: &Path) -> Option<String> {
    let candidates = [
        Some(path.to_path_buf()),
        std::fs::canonicalize(path).ok(),
        path.parent()
            .and_then(|parent| std::fs::canonicalize(parent).ok())
            .zip(path.file_name())
            .map(|(parent, name)| parent.join(name)),
    ];
    candidates.into_iter().flatten().find_map(|candidate| {
        let relative = candidate.strip_prefix(root).ok()?;
        if relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return None;
        }
        Some(
            relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/"),
        )
    })
}

#[cfg(unix)]
fn link_count(file: &std::fs::File) -> u64 {
    use std::os::unix::fs::MetadataExt;
    file.metadata()
        .map(|metadata| metadata.nlink())
        .unwrap_or(u64::MAX)
}

#[cfg(windows)]
fn link_count(file: &std::fs::File) -> u64 {
    winapi_util::file::information(file)
        .map(|information| information.number_of_links())
        .unwrap_or(u64::MAX)
}

#[cfg(not(any(unix, windows)))]
fn link_count(_file: &std::fs::File) -> u64 {
    1
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoMapStatus {
    /// A sweep is queued or running.
    pub indexing: bool,
    /// Completed sweeps since the service started.
    pub sweeps: u64,
    pub generation: u64,
    pub files: usize,
    pub truncated: bool,
    /// Stats of the latest sweep or path refresh.
    pub last_refresh: RefreshStats,
    /// Stats of the latest completed full sweep. Path refreshes do not overwrite it.
    pub last_sweep: RefreshStats,
    /// Wall time of the latest full sweep.
    pub last_refresh_ms: u64,
    pub last_error: Option<String>,
}

enum ServiceCommand {
    Refresh,
    Paths(Vec<PathBuf>),
}

struct Shared {
    map: RwLock<RepoMap>,
    status: Mutex<RepoMapStatus>,
    progress: Condvar,
    cancel: AtomicBool,
    refresh_queued: AtomicBool,
}

/// Owns a background indexing thread for one root. Dropping the service cancels the thread
/// between files without waiting for it.
pub struct RepoMapService {
    root: PathBuf,
    shared: Arc<Shared>,
    commands: mpsc::Sender<ServiceCommand>,
}

impl std::fmt::Debug for RepoMapService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepoMapService")
            .field("root", &self.root)
            .field("status", &self.status())
            .finish()
    }
}

impl RepoMapService {
    /// Validate the root on the calling thread, then index it in the background.
    pub fn spawn(root: &Path, config: RepoMapConfig) -> Result<Self, ContextError> {
        let map = RepoMap::new(root, config)?;
        let root = map.root().to_path_buf();
        let shared = Arc::new(Shared {
            map: RwLock::new(map),
            status: Mutex::new(RepoMapStatus {
                indexing: true,
                ..RepoMapStatus::default()
            }),
            progress: Condvar::new(),
            cancel: AtomicBool::new(false),
            refresh_queued: AtomicBool::new(false),
        });
        let (commands, receiver) = mpsc::channel();
        let worker = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("termior-repo-map".into())
            .spawn(move || worker_loop(&worker, &receiver))?;
        Ok(Self {
            root,
            shared,
            commands,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn status(&self) -> RepoMapStatus {
        lock(&self.shared.status).clone()
    }

    /// Queue a sweep. Requests made while one is already queued coalesce.
    pub fn request_refresh(&self) {
        if !self.shared.refresh_queued.swap(true, Ordering::SeqCst) {
            lock(&self.shared.status).indexing = true;
            let _ = self.commands.send(ServiceCommand::Refresh);
        }
    }

    pub fn notify_paths(&self, paths: Vec<PathBuf>) {
        if !paths.is_empty() {
            let _ = self.commands.send(ServiceCommand::Paths(paths));
        }
    }

    /// Slice the latest applied state. Stale hits are excluded and queued for re-indexing.
    pub fn slice(&self, query: &str, token_budget: u64) -> RepoSlice {
        let mut slice = read(&self.shared.map).slice(query, token_budget);
        slice.indexing = self.status().indexing;
        if !slice.stale_excluded.is_empty() {
            self.notify_paths(
                slice
                    .stale_excluded
                    .iter()
                    .map(|relative| native_path(&self.root, relative))
                    .collect(),
            );
        }
        slice
    }

    /// Run `f` against the current map under a read lock.
    pub fn with_map<T>(&self, f: impl FnOnce(&RepoMap) -> T) -> T {
        f(&read(&self.shared.map))
    }

    /// Block until at least `sweeps` sweeps have completed. For tests and benchmarks only; the
    /// UI never waits on the index.
    pub fn wait_for_sweeps(&self, sweeps: u64, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut status = lock(&self.shared.status);
        while status.sweeps < sweeps {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            status = match self.shared.progress.wait_timeout(status, deadline - now) {
                Ok((guard, _)) => guard,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
        true
    }

    /// Block until no sweep or path refresh is pending. For tests and benchmarks only.
    pub fn wait_until_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut status = lock(&self.shared.status);
        while status.indexing {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            status = match self.shared.progress.wait_timeout(status, deadline - now) {
                Ok((guard, _)) => guard,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
        true
    }
}

impl Drop for RepoMapService {
    fn drop(&mut self) {
        self.shared.cancel.store(true, Ordering::SeqCst);
    }
}

fn worker_loop(shared: &Shared, commands: &mpsc::Receiver<ServiceCommand>) {
    let mut extractor = OutlineExtractor::new();
    run_sweep(shared, &mut extractor);
    while let Ok(first) = commands.recv() {
        if shared.cancel.load(Ordering::SeqCst) {
            return;
        }
        let mut refresh = false;
        let mut paths = Vec::new();
        for command in std::iter::once(first).chain(commands.try_iter()) {
            match command {
                ServiceCommand::Refresh => refresh = true,
                ServiceCommand::Paths(more) => paths.extend(more),
            }
        }
        if refresh {
            run_sweep(shared, &mut extractor);
        } else {
            run_paths(shared, &mut extractor, &paths);
        }
    }
}

fn run_sweep(shared: &Shared, extractor: &mut OutlineExtractor) {
    shared.refresh_queued.store(false, Ordering::SeqCst);
    lock(&shared.status).indexing = true;
    let started = Instant::now();
    let state = read(&shared.map).scan_state();
    let result = sweep(&state, extractor, &shared.cancel, |changes| {
        let mut map = write(&shared.map);
        map.apply(changes);
        let (generation, files) = (map.generation(), map.len());
        drop(map);
        let mut status = lock(&shared.status);
        status.generation = generation;
        status.files = files;
    });
    drop(state);
    let mut map = write(&shared.map);
    if let Ok(stats) = &result {
        map.finish_sweep(stats.truncated);
    }
    let (generation, files, truncated) = (map.generation(), map.len(), map.truncated());
    drop(map);
    let mut status = lock(&shared.status);
    status.generation = generation;
    status.files = files;
    status.truncated = truncated;
    status.last_refresh_ms = started.elapsed().as_millis() as u64;
    match result {
        Ok(stats) => {
            status.last_refresh = stats;
            status.last_sweep = stats;
            status.last_error = None;
        }
        Err(error) => status.last_error = Some(error.to_string()),
    }
    status.sweeps += 1;
    status.indexing = shared.refresh_queued.load(Ordering::SeqCst);
    drop(status);
    shared.progress.notify_all();
}

fn run_paths(shared: &Shared, extractor: &mut OutlineExtractor, paths: &[PathBuf]) {
    let state = read(&shared.map).scan_state();
    let Some((changes, stats)) = scan_paths(&state, extractor, paths) else {
        drop(state);
        run_sweep(shared, extractor);
        return;
    };
    drop(state);
    let mut map = write(&shared.map);
    map.apply(changes);
    map.collect_unused_outlines();
    let (generation, files) = (map.generation(), map.len());
    drop(map);
    let mut status = lock(&shared.status);
    status.generation = generation;
    status.files = files;
    status.last_refresh = stats;
    drop(status);
    shared.progress.notify_all();
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_terms_normalize_and_drop_stopwords() {
        assert_eq!(
            query_terms("Fix the AgentRuntime retry in agent_runtime.rs, please"),
            vec!["fix", "agentruntime", "retry", "rs"]
        );
        assert!(query_terms("  ").is_empty());
        assert_eq!(query_terms(&"word ".repeat(100)).len(), 1);
    }

    #[test]
    fn path_keys_cover_directories_and_stem() {
        let mut keys = path_keys("crates/termior-ai/src/repo_map.rs");
        keys.sort();
        assert_eq!(keys, vec!["crates", "repomap", "src", "termiorai"]);
    }

    #[test]
    fn word_segments_split_separators_camel_case_and_digits() {
        assert_eq!(
            word_segments("parseHTTPHeader"),
            vec!["parse", "http", "header"]
        );
        assert_eq!(
            word_segments("RepoMapService"),
            vec!["repo", "map", "service"]
        );
        assert_eq!(
            word_segments("ledger_settle_42"),
            vec!["ledger", "settle", "42"]
        );
        assert_eq!(word_segments("MAX_FILES"), vec!["max", "files"]);
        assert!(word_segments("__").is_empty());
        let mut segments = path_segments("crates/termior-ai/src/repo_map.rs");
        segments.sort();
        assert_eq!(
            segments,
            vec!["ai", "crates", "map", "repo", "rs", "src", "termior"]
        );
    }

    #[test]
    fn racy_window_distrusts_fresh_mtimes() {
        let now = SystemTime::now();
        assert!(is_racy(Some(now), now));
        assert!(is_racy(None, now));
        assert!(!is_racy(Some(now - Duration::from_secs(10)), now));
    }

    #[test]
    fn lexical_fallback_finds_keyword_definitions() {
        let symbols = lexical_symbols("class Greeter\n  def greet(name)\n  end\nend\n");
        let names = symbols.iter().map(|s| s.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names, vec!["Greeter", "greet"]);
        assert_eq!(symbols[1].start_line, 2);
        let references = lexical_references("call helper_fn and other", &symbols);
        assert!(references.contains(&"helper_fn".to_owned()));
        assert!(!references.contains(&"Greeter".to_owned()));
    }

    #[test]
    fn relative_paths_reject_escape_and_normalize_separators() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("a")).unwrap();
        assert_eq!(
            relative_to(&root, &root.join("a").join("b.rs")).as_deref(),
            Some("a/b.rs")
        );
        assert_eq!(relative_to(&root, &root.join("..").join("x")), None);
        assert_eq!(native_path(&root, "a/b.rs"), root.join("a").join("b.rs"));
    }
}

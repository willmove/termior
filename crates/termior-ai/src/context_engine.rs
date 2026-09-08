//! Traceable, budgeted context assembly and extension metadata (Stage C / M8).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContextCategory {
    Invariant,
    Rules,
    Unresolved,
    Evidence,
    Code,
    Skill,
    Attachment,
    History,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "value")]
pub enum ContextSource {
    System,
    User,
    Workspace(PathBuf),
    Terminal(String),
    Task(String),
    Skill(String),
    Mcp(String),
    Hook(String),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "value")]
pub enum ContextScope {
    Global,
    Workspace,
    Directory(PathBuf),
    File(PathBuf),
    Turn(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sensitivity {
    Public,
    Workspace,
    Sensitive,
    Secret,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Retention {
    Ephemeral,
    Task,
    Persistent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum ContentRef {
    Inline {
        content: String,
    },
    FileRange {
        path: PathBuf,
        start_line: u64,
        end_line: u64,
        digest: String,
    },
    CommandOutputRange {
        session_id: String,
        start: u64,
        end: u64,
    },
    TerminalSelection {
        terminal_id: String,
        start: u64,
        end: u64,
    },
    TaskEventRange {
        task_id: String,
        start: u64,
        end: u64,
    },
    Artifact {
        path: PathBuf,
        digest: String,
        bytes: u64,
    },
}

/// Durable, redacted storage for large context bodies. The model receives a bounded summary and
/// this handle; inspectors and explicit follow-up reads can retrieve the complete retained body.
pub struct ContextContentStore {
    root: PathBuf,
    redactor: termior_store::StreamingRedactor,
}

impl ContextContentStore {
    pub fn new(root: impl Into<PathBuf>, known_secrets: impl IntoIterator<Item = String>) -> Self {
        Self {
            root: root.into(),
            redactor: termior_store::StreamingRedactor::new(known_secrets),
        }
    }

    pub fn put_tool_output(
        &self,
        task_id: &str,
        call_id: &str,
        output: &str,
    ) -> Result<ContentRef, ContextError> {
        let directory = self.root.join(safe_component(task_id)).join("tool-output");
        let path = directory.join(format!("{}.txt", safe_component(call_id)));
        let redacted = self.redactor.redact(output).text;
        termior_store::atomic_write(&path, &redacted)
            .map_err(|error| ContextError::Store(error.to_string()))?;
        Ok(ContentRef::Artifact {
            path,
            digest: stable_digest(redacted.as_bytes()),
            bytes: redacted.len() as u64,
        })
    }

    pub fn resolve(&self, reference: &ContentRef) -> Result<String, ContextError> {
        match reference {
            ContentRef::Inline { content } => Ok(content.clone()),
            ContentRef::Artifact { path, digest, .. } => {
                let root = absolute_or_canonical(&self.root)?;
                let path = std::fs::canonicalize(path)?;
                if !path.starts_with(root) {
                    return Err(ContextError::OutsideWorkspace(path));
                }
                let bytes = std::fs::read(&path)?;
                let actual = stable_digest(&bytes);
                if &actual != digest {
                    return Err(ContextError::StaleContentReference {
                        expected: digest.clone(),
                        actual,
                    });
                }
                String::from_utf8(bytes).map_err(|error| ContextError::Decode(error.to_string()))
            }
            ContentRef::FileRange {
                path,
                start_line,
                end_line,
                digest,
            } => resolve_file_range(path, *start_line, *end_line, digest),
            _ => Err(ContextError::UnsupportedContentReference),
        }
    }
}

fn resolve_file_range(
    path: &Path,
    start_line: u64,
    end_line: u64,
    expected_digest: &str,
) -> Result<String, ContextError> {
    if start_line == 0 || end_line < start_line {
        return Err(ContextError::InvalidContentRange);
    }
    let bytes = std::fs::read(path)?;
    let actual = stable_digest(&bytes);
    if actual != expected_digest {
        return Err(ContextError::StaleContentReference {
            expected: expected_digest.into(),
            actual,
        });
    }
    let text = String::from_utf8(bytes).map_err(|error| ContextError::Decode(error.to_string()))?;
    Ok(text
        .lines()
        .skip((start_line - 1) as usize)
        .take((end_line - start_line + 1) as usize)
        .collect::<Vec<_>>()
        .join("\n"))
}

fn safe_component(value: &str) -> String {
    let value = value
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
    if value.is_empty() {
        "unnamed".into()
    } else {
        value
    }
}

fn absolute_or_canonical(path: &Path) -> Result<PathBuf, ContextError> {
    if path.exists() {
        Ok(std::fs::canonicalize(path)?)
    } else if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextItem {
    pub id: String,
    pub category: ContextCategory,
    pub source: ContextSource,
    pub captured_at_ms: u64,
    pub scope: ContextScope,
    pub version: String,
    pub sensitivity: Sensitivity,
    pub retention: Retention,
    pub content: ContentRef,
    pub estimated_tokens: u64,
    pub estimate_is_exact: bool,
    pub priority: u16,
    pub required: bool,
}

impl ContextItem {
    pub fn inline(
        id: impl Into<String>,
        category: ContextCategory,
        source: ContextSource,
        content: impl Into<String>,
        priority: u16,
    ) -> Self {
        let content = content.into();
        Self {
            id: id.into(),
            category,
            source,
            captured_at_ms: now_ms(),
            scope: ContextScope::Workspace,
            version: stable_digest(content.as_bytes()),
            sensitivity: Sensitivity::Workspace,
            retention: Retention::Task,
            estimated_tokens: HeuristicTokenEstimator.estimate(&content).tokens,
            estimate_is_exact: false,
            content: ContentRef::Inline { content },
            priority,
            required: category == ContextCategory::Invariant,
        }
    }

    pub fn redacted_label(&self) -> String {
        if matches!(
            self.sensitivity,
            Sensitivity::Secret | Sensitivity::Sensitive
        ) {
            format!("{} · [redacted]", self.id)
        } else {
            format!("{} · {:?}", self.id, self.source)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenEstimate {
    pub tokens: u64,
    pub exact: bool,
}

pub trait TokenEstimator {
    fn estimate(&self, text: &str) -> TokenEstimate;
}
pub struct HeuristicTokenEstimator;
impl TokenEstimator for HeuristicTokenEstimator {
    fn estimate(&self, text: &str) -> TokenEstimate {
        TokenEstimate {
            tokens: (text.chars().count() as u64).div_ceil(4),
            exact: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContextDisposition {
    Included,
    Summarized,
    Truncated,
    Excluded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPlanEntry {
    pub item_id: String,
    pub category: ContextCategory,
    pub tokens: u64,
    pub disposition: ContextDisposition,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPlan {
    pub hard_limit: u64,
    pub total_tokens: u64,
    pub entries: Vec<ContextPlanEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedContextItem {
    pub item_id: String,
    pub source: ContextSource,
    pub disposition: ContextDisposition,
    pub content: String,
}

impl ContextPlan {
    /// Materializes only inline entries selected by this exact plan. Referenced content remains a
    /// handle and must be resolved through its owning service before it can be sent to a model.
    pub fn materialize_inline(&self, items: &[ContextItem]) -> Vec<MaterializedContextItem> {
        self.entries
            .iter()
            .filter(|entry| entry.disposition != ContextDisposition::Excluded)
            .filter_map(|entry| {
                let item = items.iter().find(|item| item.id == entry.item_id)?;
                let ContentRef::Inline { content } = &item.content else {
                    return None;
                };
                let content = if entry.disposition == ContextDisposition::Included {
                    content.clone()
                } else {
                    let budget = entry.tokens.saturating_mul(4).min(usize::MAX as u64) as usize;
                    let mut text = content.chars().take(budget).collect::<String>();
                    if text.chars().count() < content.chars().count() {
                        text.push_str("\n[context truncated at the selected token budget]");
                    }
                    text
                };
                Some(MaterializedContextItem {
                    item_id: entry.item_id.clone(),
                    source: item.source.clone(),
                    disposition: entry.disposition,
                    content,
                })
            })
            .collect()
    }
}

pub struct ContextAssembler {
    hard_limit: u64,
    category_budgets: BTreeMap<ContextCategory, u64>,
}
impl ContextAssembler {
    pub fn new(hard_limit: u64, category_budgets: BTreeMap<ContextCategory, u64>) -> Self {
        Self {
            hard_limit,
            category_budgets,
        }
    }
    pub fn plan(&self, items: &[ContextItem]) -> Result<ContextPlan, ContextError> {
        let required_total: u64 = items
            .iter()
            .filter(|item| item.required)
            .map(|item| item.estimated_tokens)
            .sum();
        if required_total > self.hard_limit {
            return Err(ContextError::RequiredItemExceedsHardLimit {
                required: required_total,
                hard_limit: self.hard_limit,
            });
        }
        let mut ordered = items.to_vec();
        ordered.sort_by(|a, b| {
            b.required
                .cmp(&a.required)
                .then(b.priority.cmp(&a.priority))
                .then(a.id.cmp(&b.id))
        });
        let mut used = 0;
        let mut category_used = BTreeMap::<ContextCategory, u64>::new();
        let mut entries = Vec::with_capacity(ordered.len());
        for item in ordered {
            let category_limit = self
                .category_budgets
                .get(&item.category)
                .copied()
                .unwrap_or(self.hard_limit);
            let in_category = category_used.get(&item.category).copied().unwrap_or(0);
            let fits = used + item.estimated_tokens <= self.hard_limit
                && (item.required || in_category + item.estimated_tokens <= category_limit);
            let disposition = if fits {
                ContextDisposition::Included
            } else if matches!(item.content, ContentRef::Inline { .. }) && used < self.hard_limit {
                ContextDisposition::Truncated
            } else {
                ContextDisposition::Excluded
            };
            let tokens = if disposition == ContextDisposition::Included {
                item.estimated_tokens
            } else if matches!(
                disposition,
                ContextDisposition::Summarized | ContextDisposition::Truncated
            ) {
                (self.hard_limit - used).min(item.estimated_tokens)
            } else {
                0
            };
            used += tokens;
            *category_used.entry(item.category).or_default() += tokens;
            entries.push(ContextPlanEntry {
                item_id: item.id,
                category: item.category,
                tokens,
                disposition,
                reason: if fits {
                    "selected by deterministic priority"
                } else {
                    "category or model hard limit"
                }
                .into(),
            });
        }
        Ok(ContextPlan {
            hard_limit: self.hard_limit,
            total_tokens: used,
            entries,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingInvariant {
    pub id: String,
    pub detail: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskBrief {
    pub schema_version: u32,
    pub goal: String,
    pub acceptance: Vec<String>,
    pub user_constraints: Vec<String>,
    pub decisions: Vec<String>,
    pub completed_actions: Vec<String>,
    pub pending_approvals: Vec<PendingInvariant>,
    pub pending_changes: Vec<PendingInvariant>,
    pub unknown_side_effects: Vec<PendingInvariant>,
    pub failure_evidence: Vec<String>,
    pub next_steps: Vec<String>,
    pub covered_event_range: (u64, u64),
    pub summary_model: Option<String>,
}
impl TaskBrief {
    pub fn minimal(goal: impl Into<String>) -> Self {
        Self {
            schema_version: 1,
            goal: goal.into(),
            acceptance: vec![],
            user_constraints: vec![],
            decisions: vec![],
            completed_actions: vec![],
            pending_approvals: vec![],
            pending_changes: vec![],
            unknown_side_effects: vec![],
            failure_evidence: vec![],
            next_steps: vec![],
            covered_event_range: (0, 0),
            summary_model: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionResult {
    pub brief: TaskBrief,
    pub narrative: String,
    pub version: u32,
}
pub struct Compactor {
    max_consecutive: u32,
}
impl Compactor {
    pub fn new(max_consecutive: u32) -> Self {
        Self { max_consecutive }
    }
    pub fn compact(
        &self,
        brief: &TaskBrief,
        narrative: &str,
        consecutive: u32,
    ) -> Result<CompactionResult, ContextError> {
        if consecutive >= self.max_consecutive {
            return Err(ContextError::ContextThrashing { consecutive });
        }
        Ok(CompactionResult {
            brief: brief.clone(),
            narrative: narrative.into(),
            version: consecutive + 1,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleDocument {
    pub path: PathBuf,
    pub scope: PathBuf,
    pub content: String,
    pub digest: String,
}
pub struct RuleResolver {
    root: PathBuf,
}
impl RuleResolver {
    pub fn new(root: &Path) -> Result<Self, ContextError> {
        Ok(Self {
            root: std::fs::canonicalize(root)?,
        })
    }
    pub fn for_path(&self, target: &Path) -> Result<Vec<RuleDocument>, ContextError> {
        let target = if target.exists() {
            std::fs::canonicalize(target)?
        } else {
            let parent = target
                .parent()
                .ok_or_else(|| ContextError::OutsideWorkspace(target.to_path_buf()))?;
            std::fs::canonicalize(parent)?.join(target.file_name().unwrap_or_default())
        };
        if !target.starts_with(&self.root) {
            return Err(ContextError::OutsideWorkspace(target));
        }
        let directory = if target.is_dir() {
            target
        } else {
            target.parent().unwrap_or(&self.root).to_path_buf()
        };
        let relative = directory
            .strip_prefix(&self.root)
            .map_err(|_| ContextError::OutsideWorkspace(directory.clone()))?;
        let mut current = self.root.clone();
        let mut result = Vec::new();
        for component in std::iter::once(None).chain(relative.components().map(Some)) {
            if let Some(component) = component {
                current.push(component);
            }
            let path = current.join("AGENTS.md");
            if path.is_file() {
                let content = std::fs::read_to_string(&path)?;
                result.push(RuleDocument {
                    path,
                    scope: current.clone(),
                    digest: stable_digest(content.as_bytes()),
                    content,
                });
            }
        }
        Ok(result)
    }
    pub fn common(sets: &[Vec<RuleDocument>]) -> Vec<RuleDocument> {
        let Some(first) = sets.first() else {
            return vec![];
        };
        first
            .iter()
            .filter(|candidate| {
                sets.iter().skip(1).all(|set| {
                    set.iter()
                        .any(|rule| rule.path == candidate.path && rule.digest == candidate.digest)
                })
            })
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub allowed_tools: BTreeSet<String>,
    pub body_loaded: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivatedSkill {
    pub metadata: SkillMetadata,
    pub body: String,
    pub effective_tools: BTreeSet<String>,
}
pub struct SkillIndex {
    skills: BTreeMap<String, SkillMetadata>,
}
impl SkillIndex {
    /// Scan the user source first and project source second, so an explicitly installed project
    /// skill wins on a duplicate name. Each source is independently authorization anchored.
    pub fn scan_sources(
        project_root: &Path,
        user_skills_root: Option<&Path>,
    ) -> Result<Self, ContextError> {
        let mut combined = BTreeMap::new();
        if let Some(user_root) = user_skills_root.filter(|root| root.exists()) {
            combined.extend(Self::scan(&[user_root.to_path_buf()], user_root)?.skills);
        }
        let project_skills = project_root.join(".agents").join("skills");
        if project_skills.exists() {
            combined.extend(Self::scan(&[project_skills], project_root)?.skills);
        }
        Ok(Self { skills: combined })
    }

    pub fn scan(roots: &[PathBuf], authorized_root: &Path) -> Result<Self, ContextError> {
        let authorized = std::fs::canonicalize(authorized_root)?;
        let mut skills = BTreeMap::new();
        for root in roots {
            if !root.exists() {
                continue;
            }
            let root = std::fs::canonicalize(root)?;
            if !root.starts_with(&authorized) {
                return Err(ContextError::OutsideWorkspace(root));
            }
            for entry in std::fs::read_dir(root)? {
                let path = entry?.path().join("SKILL.md");
                if !path.is_file() {
                    continue;
                }
                let raw = std::fs::read_to_string(&path)?;
                let (front, _) = split_frontmatter(&raw)?;
                let name = field(front, "name")
                    .ok_or_else(|| ContextError::InvalidSkill(path.clone(), "missing name".into()))?
                    .to_owned();
                if !name
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
                {
                    return Err(ContextError::InvalidSkill(path, "invalid name".into()));
                }
                let description = field(front, "description").unwrap_or("").to_owned();
                let allowed_tools = field(front, "allowed-tools")
                    .unwrap_or("")
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect();
                skills.insert(
                    name.clone(),
                    SkillMetadata {
                        name,
                        description,
                        path,
                        allowed_tools,
                        body_loaded: false,
                    },
                );
            }
        }
        Ok(Self { skills })
    }
    pub fn metadata(&self) -> Vec<SkillMetadata> {
        self.skills.values().cloned().collect()
    }
    pub fn activate(
        &self,
        name: &str,
        task_tools: &BTreeSet<String>,
    ) -> Result<ActivatedSkill, ContextError> {
        let metadata = self
            .skills
            .get(name)
            .cloned()
            .ok_or_else(|| ContextError::SkillNotFound(name.into()))?;
        let raw = std::fs::read_to_string(&metadata.path)?;
        let (_, body) = split_frontmatter(&raw)?;
        let effective_tools = if metadata.allowed_tools.is_empty() {
            task_tools.clone()
        } else {
            metadata
                .allowed_tools
                .intersection(task_tools)
                .cloned()
                .collect()
        };
        let mut loaded = metadata;
        loaded.body_loaded = true;
        Ok(ActivatedSkill {
            metadata: loaded,
            body: body.trim().into(),
            effective_tools,
        })
    }

    pub fn load_resource(&self, name: &str, relative: &Path) -> Result<ContextItem, ContextError> {
        let skill = self
            .skills
            .get(name)
            .ok_or_else(|| ContextError::SkillNotFound(name.into()))?;
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(ContextError::OutsideWorkspace(relative.to_path_buf()));
        }
        let directory = skill
            .path
            .parent()
            .ok_or_else(|| ContextError::InvalidSkill(skill.path.clone(), "no directory".into()))?;
        let directory = std::fs::canonicalize(directory)?;
        let path = std::fs::canonicalize(directory.join(relative))?;
        if !path.starts_with(&directory) {
            return Err(ContextError::OutsideWorkspace(path));
        }
        let content = std::fs::read_to_string(&path)?;
        Ok(ContextItem::inline(
            format!("skill:{name}:{}", relative.display()),
            ContextCategory::Skill,
            ContextSource::Skill(name.into()),
            content,
            100,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryCandidate {
    pub id: String,
    pub content: String,
    pub scope: ContextScope,
    pub evidence: Vec<String>,
    pub confidence: String,
    pub expires_at_ms: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub content: String,
    pub scope: ContextScope,
    pub evidence: Vec<String>,
    pub accepted_at_ms: u64,
    pub expires_at_ms: Option<u64>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryStatus {
    Active,
    Paused,
    Expired,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryStore {
    pub schema_version: u32,
    pub candidates: BTreeMap<String, MemoryCandidate>,
    pub entries: BTreeMap<String, MemoryEntry>,
    pub rejected: BTreeSet<String>,
}

impl MemoryStore {
    pub fn propose(&mut self, candidate: MemoryCandidate) {
        self.candidates.insert(candidate.id.clone(), candidate);
    }

    pub fn accept(&mut self, id: &str) -> Result<(), ContextError> {
        let candidate = self
            .candidates
            .remove(id)
            .ok_or_else(|| ContextError::MemoryNotFound(id.into()))?;
        let entry = candidate.accept()?;
        self.entries.insert(id.into(), entry);
        Ok(())
    }

    pub fn reject(&mut self, id: &str) {
        self.candidates.remove(id);
        self.rejected.insert(id.into());
    }

    pub fn edit(&mut self, id: &str, content: String) -> Result<(), ContextError> {
        termior_store::assert_no_persistent_secret(&content)
            .map_err(|_| ContextError::SensitiveMemory)?;
        let entry = self
            .entries
            .get_mut(id)
            .ok_or_else(|| ContextError::MemoryNotFound(id.into()))?;
        entry.content = content;
        Ok(())
    }

    pub fn pause(&mut self, id: &str, paused: bool) -> Result<(), ContextError> {
        let entry = self
            .entries
            .get_mut(id)
            .ok_or_else(|| ContextError::MemoryNotFound(id.into()))?;
        entry.enabled = !paused;
        Ok(())
    }

    pub fn delete(&mut self, id: &str) {
        self.entries.remove(id);
        self.candidates.remove(id);
    }

    pub fn status(&self, id: &str, now_ms: u64) -> Option<MemoryStatus> {
        self.entries.get(id).map(|entry| {
            if entry.expires_at_ms.is_some_and(|expiry| expiry <= now_ms) {
                MemoryStatus::Expired
            } else if !entry.enabled {
                MemoryStatus::Paused
            } else {
                MemoryStatus::Active
            }
        })
    }

    pub fn persist(&self, path: &Path) -> Result<(), ContextError> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|error| ContextError::Store(error.to_string()))?;
        termior_store::assert_no_persistent_secret(&json)
            .map_err(|_| ContextError::SensitiveMemory)?;
        termior_store::atomic_write(path, &json)
            .map_err(|error| ContextError::Store(error.to_string()))
    }

    pub fn load(path: &Path) -> Result<Self, ContextError> {
        match std::fs::read_to_string(path) {
            Ok(json) => {
                serde_json::from_str(&json).map_err(|error| ContextError::Store(error.to_string()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoMapEntry {
    pub path: PathBuf,
    pub digest: String,
    pub symbols: Vec<String>,
    pub references: Vec<String>,
    pub stale: bool,
}

#[derive(Debug, Default)]
pub struct RepoMap {
    entries: BTreeMap<PathBuf, RepoMapEntry>,
}

impl RepoMap {
    pub fn index_workspace(&mut self, root: &Path) -> Result<usize, ContextError> {
        let root = std::fs::canonicalize(root)?;
        let mut indexed = 0;
        for entry in ignore::WalkBuilder::new(&root)
            .hidden(false)
            .git_ignore(true)
            .git_exclude(true)
            .build()
        {
            let entry = entry.map_err(|error| ContextError::Walk(error.to_string()))?;
            if entry.file_type().is_some_and(|kind| kind.is_file())
                && std::fs::metadata(entry.path())
                    .is_ok_and(|metadata| metadata.len() <= 2 * 1024 * 1024)
                && self.index_file(&root, entry.path()).is_ok()
            {
                indexed += 1;
            }
        }
        Ok(indexed)
    }

    pub fn index_file(&mut self, root: &Path, path: &Path) -> Result<(), ContextError> {
        let root = std::fs::canonicalize(root)?;
        let path = std::fs::canonicalize(path)?;
        if !path.starts_with(&root) {
            return Err(ContextError::OutsideWorkspace(path));
        }
        let content = std::fs::read_to_string(&path)?;
        let symbols = lexical_symbols(&content);
        let references = lexical_references(&content, &symbols);
        self.entries.insert(
            path.clone(),
            RepoMapEntry {
                path,
                digest: stable_digest(content.as_bytes()),
                symbols,
                references,
                stale: false,
            },
        );
        Ok(())
    }

    pub fn invalidate_changed(&mut self) {
        for entry in self.entries.values_mut() {
            let current = std::fs::read(&entry.path)
                .ok()
                .map(|bytes| stable_digest(&bytes));
            entry.stale = current.as_deref() != Some(&entry.digest);
        }
    }

    pub fn relevant(&self, query: &str, token_budget: u64) -> Vec<RepoMapEntry> {
        let needles = query
            .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
            .filter(|part| !part.is_empty())
            .map(str::to_ascii_lowercase)
            .collect::<Vec<_>>();
        let mut candidates = self
            .entries
            .values()
            .filter(|entry| !entry.stale)
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort_by_key(|entry| {
            let haystack = format!(
                "{} {} {}",
                entry.path.display(),
                entry.symbols.join(" "),
                entry.references.join(" ")
            )
            .to_ascii_lowercase();
            let score = needles
                .iter()
                .filter(|word| haystack.contains(word.as_str()))
                .count();
            (std::cmp::Reverse(score), entry.path.clone())
        });
        let mut used = 0;
        candidates
            .into_iter()
            .take_while(|entry| {
                let cost = HeuristicTokenEstimator
                    .estimate(&format!(
                        "{} {}",
                        entry.path.display(),
                        entry.symbols.join(" ")
                    ))
                    .tokens;
                if used + cost > token_budget {
                    false
                } else {
                    used += cost;
                    true
                }
            })
            .collect()
    }
}
impl MemoryCandidate {
    pub fn accept(self) -> Result<MemoryEntry, ContextError> {
        if self.evidence.is_empty() {
            return Err(ContextError::MemoryMissingEvidence);
        }
        termior_store::assert_no_persistent_secret(&self.content)
            .map_err(|_| ContextError::SensitiveMemory)?;
        Ok(MemoryEntry {
            id: self.id,
            content: self.content,
            scope: self.scope,
            evidence: self.evidence,
            accepted_at_ms: now_ms(),
            expires_at_ms: self.expires_at_ms,
            enabled: true,
        })
    }
}

#[derive(Debug, Error)]
pub enum ContextError {
    #[error("required context needs {required} tokens but the hard limit is {hard_limit}")]
    RequiredItemExceedsHardLimit { required: u64, hard_limit: u64 },
    #[error("context compaction repeated {consecutive} times without making progress")]
    ContextThrashing { consecutive: u32 },
    #[error("path is outside the authorized workspace: {0}")]
    OutsideWorkspace(PathBuf),
    #[error("invalid skill {0}: {1}")]
    InvalidSkill(PathBuf, String),
    #[error("skill not found: {0}")]
    SkillNotFound(String),
    #[error("memory candidate has no evidence")]
    MemoryMissingEvidence,
    #[error("memory entry or candidate not found: {0}")]
    MemoryNotFound(String),
    #[error("memory candidate contains sensitive material")]
    SensitiveMemory,
    #[error("content store failed: {0}")]
    Store(String),
    #[error("content reference is stale (expected {expected}, actual {actual})")]
    StaleContentReference { expected: String, actual: String },
    #[error("content reference range is invalid")]
    InvalidContentRange,
    #[error("content reference requires a terminal or task-event resolver")]
    UnsupportedContentReference,
    #[error("context content is not UTF-8: {0}")]
    Decode(String),
    #[error("workspace walk failed: {0}")]
    Walk(String),
    #[error("I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

fn split_frontmatter(raw: &str) -> Result<(&str, &str), ContextError> {
    let rest = raw
        .strip_prefix("---\n")
        .ok_or_else(|| ContextError::InvalidSkill(PathBuf::new(), "missing frontmatter".into()))?;
    let end = rest.find("\n---").ok_or_else(|| {
        ContextError::InvalidSkill(PathBuf::new(), "unterminated frontmatter".into())
    })?;
    Ok((&rest[..end], &rest[end + 4..]))
}
fn field<'a>(front: &'a str, name: &str) -> Option<&'a str> {
    front.lines().find_map(|line| {
        line.split_once(':')
            .filter(|(key, _)| key.trim() == name)
            .map(|(_, value)| value.trim())
    })
}
fn stable_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn lexical_symbols(content: &str) -> Vec<String> {
    let words = content
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    let mut result = Vec::new();
    for pair in words.windows(2) {
        if matches!(
            pair[0],
            "fn" | "struct" | "enum" | "trait" | "class" | "def" | "function"
        ) {
            result.push(pair[1].to_owned());
        }
    }
    result.sort();
    result.dedup();
    result
}

fn lexical_references(content: &str, definitions: &[String]) -> Vec<String> {
    let definitions = definitions
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut references = content
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .filter(|word| word.len() > 2 && !definitions.contains(*word))
        .filter(|word| word.chars().next().is_some_and(|ch| ch.is_alphabetic()))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    references.sort();
    references.dedup();
    references.truncate(512);
    references
}
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

//! Agent 上下文延迟测量（仓库地图）。`nfr-context` 二进制与单元测试共用。
//!
//! 一轮测量：
//! 1. 生成一个多语言合成仓库，文件 mtime 回拨一小时，避开 racy 窗口。
//! 2. 在调用线程上启动 [`RepoMapService`]，后台全量建索引期间持续取切片并计时。
//! 3. 改动约 1% 的文件，外加少量改名、删除、新增，请求一次刷新，刷新期间继续取切片。
//! 4. 校验结果：改名后的符号指向新路径，删除的文件不再出现，增量 sweep 没有重读未改动文件。
//!
//! 切片延迟是 UI 线程能感知到的上下文延迟；增量耗时在后台线程上，只要求有界。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use termior_ai::repo_map::{RepoMapConfig, RepoMapService};
use termior_security::workspace::WorkspaceAuthRegistry;

use crate::payload::ContextLatencyPayload;

/// Token ceiling the Composer uses for the repo-map slice.
pub const SLICE_TOKEN_BUDGET: u64 = 1_200;

const WORDS: [&str; 12] = [
    "ledger", "invoice", "session", "config", "parser", "render", "socket", "cache", "worker",
    "index", "router", "storage",
];

const EXTENSIONS: [&str; 6] = ["rs", "py", "ts", "go", "c", "md"];

/// Pause between slices on the calling thread, so the sampler does not monopolise the lock the
/// way a real UI never would.
const SLICE_INTERVAL: Duration = Duration::from_millis(2);

const IDLE_SLICES: usize = 50;

#[derive(Debug, Clone, Copy)]
pub struct ContextHarnessConfig {
    pub files: usize,
    /// Deadline for each indexing phase.
    pub timeout: Duration,
}

impl Default for ContextHarnessConfig {
    fn default() -> Self {
        Self {
            files: termior_ai::repo_map::DEFAULT_MAX_FILES,
            timeout: Duration::from_secs(300),
        }
    }
}

/// Mutations applied between the full index and the incremental refresh.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoChurn {
    pub modified: Vec<usize>,
    pub renamed: Vec<(usize, PathBuf)>,
    pub deleted: Vec<usize>,
    pub added: Vec<usize>,
}

impl RepoChurn {
    pub fn total(&self) -> usize {
        self.modified.len() + self.renamed.len() + self.deleted.len() + self.added.len()
    }
}

pub fn relative_path(index: usize) -> PathBuf {
    let extension = EXTENSIONS[index % EXTENSIONS.len()];
    PathBuf::from("src")
        .join(format!("mod{}", index / 50))
        .join(format!("{}_{index}.{extension}", word(index)))
}

fn word(index: usize) -> &'static str {
    WORDS[index % WORDS.len()]
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
        .unwrap_or_default()
}

/// The function every file defines; queries and checks look for it by name.
pub fn load_symbol(index: usize) -> String {
    format!("{}_load_{index}", word(index))
}

/// Source for file `index`. `revision` changes the body so a modified file gets a new digest.
pub fn source(index: usize, files: usize, revision: u32) -> String {
    let w = word(index);
    let type_name = format!("{}Store{index}", capitalized(w));
    let settle = format!("{w}_settle_{index}");
    let load = load_symbol(index);
    let callee = load_symbol((index * 7 + 3) % files.max(1));
    let mut body = String::new();
    for line in 0..16 {
        body.push_str(&format!(
            "// {w} note {line}: revision {revision} keeps the {type_name} totals in sync.\n"
        ));
    }
    let code = match index % EXTENSIONS.len() {
        0 => format!(
            "pub struct {type_name} {{\n    total: u64,\n}}\n\nimpl {type_name} {{\n    pub fn {settle}(&self, amount: u64) -> u64 {{\n        {callee}(amount) + self.total + {revision}\n    }}\n}}\n\npub fn {load}(path: &str) -> usize {{\n    path.len()\n}}\n"
        ),
        1 => format!(
            "class {type_name}:\n    def {settle}(self, amount):\n        return {callee}(amount) + {revision}\n\n\ndef {load}(path):\n    return len(path)\n"
        ),
        2 => format!(
            "export interface {type_name}Shape {{\n  total: number;\n}}\n\nexport class {type_name} {{\n  {settle}(amount: number): number {{\n    return {callee}(amount) + {revision};\n  }}\n}}\n\nexport function {load}(path: string): number {{\n  return path.length;\n}}\n"
        ),
        3 => format!(
            "package mod{}\n\ntype {type_name} struct {{\n\tTotal int\n}}\n\nfunc (s *{type_name}) {settle}(amount int) int {{\n\treturn {callee}(amount) + s.Total + {revision}\n}}\n\nfunc {load}(path string) int {{\n\treturn len(path)\n}}\n",
            index / 50
        ),
        4 => format!(
            "struct {w}_store_{index} {{\n    int total;\n}};\n\nint {settle}(struct {w}_store_{index} *s, int amount) {{\n    return {callee}(amount) + s->total + {revision};\n}}\n\nint {load}(const char *path) {{\n    return path[0];\n}}\n"
        ),
        _ => format!(
            "# {type_name}\n\nfn {load}(path) explains how {settle} calls {callee} (revision {revision}).\n"
        ),
    };
    let comment = if index % EXTENSIONS.len() == 1 {
        body.replace("// ", "# ")
    } else if index % EXTENSIONS.len() == 5 {
        body.replace("// ", "")
    } else {
        body
    };
    format!("{comment}\n{code}")
}

/// Write the synthetic repository, plus ignored build output that must never be indexed.
pub fn generate_repo(root: &Path, files: usize) -> Result<(), String> {
    for index in 0..files {
        write_file(root, &relative_path(index), &source(index, files, 0))?;
    }
    write_file(root, Path::new(".gitignore"), "target/\n")?;
    for index in 0..files.min(200) {
        write_file(
            root,
            &Path::new("target")
                .join("debug")
                .join(format!("generated_{index}.rs")),
            &source(index, files, 0),
        )?;
    }
    backdate(root)
}

fn write_file(root: &Path, relative: &Path, contents: &str) -> Result<(), String> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    std::fs::write(&path, contents).map_err(|error| format!("write {}: {error}", path.display()))
}

/// Move every mtime an hour back, as an existing checkout would be, so the first sweep can
/// trust size and mtime on the next pass instead of rehashing racy files.
fn backdate(root: &Path) -> Result<(), String> {
    let old = SystemTime::now() - Duration::from_secs(3600);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).map_err(|error| format!("read {}: {error}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() {
                std::fs::File::options()
                    .write(true)
                    .open(&path)
                    .and_then(|file| file.set_modified(old))
                    .map_err(|error| format!("backdate {}: {error}", path.display()))?;
            }
        }
    }
    Ok(())
}

/// Modify about 1% of the files, and rename, delete, and add about 0.2% each.
pub fn churn(root: &Path, files: usize) -> Result<RepoChurn, String> {
    let step = |share: usize| (files / share).max(1);
    let mut plan = RepoChurn::default();
    let mut index = 1;
    while index < files && plan.modified.len() < step(100) {
        write_file(root, &relative_path(index), &source(index, files, 1))?;
        plan.modified.push(index);
        index += 97;
    }
    let mut index = 2;
    while index < files && plan.renamed.len() < step(500) {
        let from = root.join(relative_path(index));
        let to = relative_path(index).with_file_name(format!(
            "renamed_{index}.{}",
            EXTENSIONS[index % EXTENSIONS.len()]
        ));
        std::fs::rename(&from, root.join(&to))
            .map_err(|error| format!("rename {}: {error}", from.display()))?;
        plan.renamed.push((index, to));
        index += 499;
    }
    let mut index = 4;
    while index < files && plan.deleted.len() < step(500) {
        let path = root.join(relative_path(index));
        std::fs::remove_file(&path)
            .map_err(|error| format!("delete {}: {error}", path.display()))?;
        plan.deleted.push(index);
        index += 503;
    }
    for added in files..files + step(500) {
        write_file(root, &relative_path(added), &source(added, files, 0))?;
        plan.added.push(added);
    }
    Ok(plan)
}

/// Nearest-rank percentile of `values` (milliseconds). `None` when empty.
pub fn percentile(values: &[f64], quantile: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (quantile.clamp(0.0, 1.0) * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.clamp(1, sorted.len()) - 1])
}

fn queries(files: usize) -> Vec<String> {
    let mut out = vec![
        "fix rounding in the ledger settle path and the invoice totals".to_string(),
        "session token refresh races with the cache worker".to_string(),
        "config parser rejects nested router storage keys".to_string(),
    ];
    for index in [1, files / 3, files / 2, files.saturating_sub(1)] {
        out.push(format!(
            "why does {} return the wrong size",
            load_symbol(index)
        ));
    }
    out
}

struct Sampler {
    queries: Vec<String>,
    next: usize,
    millis: Vec<f64>,
}

impl Sampler {
    fn slice(&mut self, service: &RepoMapService) {
        let query = &self.queries[self.next % self.queries.len()];
        self.next += 1;
        let start = Instant::now();
        let slice = service.slice(query, SLICE_TOKEN_BUDGET);
        self.millis.push(start.elapsed().as_secs_f64() * 1_000.0);
        std::hint::black_box(slice);
    }

    /// Slice until the service reports idle. Returns the phase's wall time in milliseconds.
    fn while_indexing(
        &mut self,
        service: &RepoMapService,
        started: Instant,
        timeout: Duration,
    ) -> Result<f64, String> {
        loop {
            if !service.status().indexing {
                return Ok(started.elapsed().as_secs_f64() * 1_000.0);
            }
            if started.elapsed() > timeout {
                return Err(format!("repo map did not settle within {timeout:?}"));
            }
            self.slice(service);
            std::thread::sleep(SLICE_INTERVAL);
        }
    }
}

/// One full measurement round in a fresh directory under `scratch`.
pub fn measure_round(
    scratch: &Path,
    config: ContextHarnessConfig,
) -> Result<ContextLatencyPayload, String> {
    let root = scratch.join("repo");
    if root.exists() {
        std::fs::remove_dir_all(&root).map_err(|error| error.to_string())?;
    }
    generate_repo(&root, config.files)?;
    let authorization =
        WorkspaceAuthRegistry::with_roots([root.to_string_lossy().replace('\\', "/")]);
    let mut sampler = Sampler {
        queries: queries(config.files),
        next: 0,
        millis: Vec::new(),
    };

    let started = Instant::now();
    let service = RepoMapService::spawn(
        &root,
        RepoMapConfig {
            authorization: Some(authorization),
            ..RepoMapConfig::default()
        },
    )
    .map_err(|error| error.to_string())?;
    let spawn_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let full_index_ms = sampler.while_indexing(&service, started, config.timeout)?;
    let status = service.status();
    if let Some(error) = status.last_error {
        return Err(format!("full index failed: {error}"));
    }
    if status.files != config.files {
        return Err(format!(
            "full index saw {} files, expected {} (ignored output leaked or files were missed)",
            status.files, config.files
        ));
    }

    let plan = churn(&root, config.files)?;
    let started = Instant::now();
    service.request_refresh();
    let incremental_ms = sampler.while_indexing(&service, started, config.timeout)?;
    for _ in 0..IDLE_SLICES {
        sampler.slice(&service);
    }
    verify_incremental(&service, config.files, &plan)?;

    Ok(ContextLatencyPayload {
        files: config.files as u64,
        changed_files: plan.total() as u64,
        slice_p99_ms: percentile(&sampler.millis, 0.99),
        incremental_ms: Some(incremental_ms),
        slice_p50_ms: percentile(&sampler.millis, 0.50),
        slice_max_ms: percentile(&sampler.millis, 1.0),
        full_index_ms: Some(full_index_ms),
        spawn_ms: Some(spawn_ms),
        slices: sampler.millis.len() as u64,
        runs: Vec::new(),
    })
}

fn verify_incremental(
    service: &RepoMapService,
    files: usize,
    plan: &RepoChurn,
) -> Result<(), String> {
    let status = service.status();
    if let Some(error) = status.last_error {
        return Err(format!("incremental refresh failed: {error}"));
    }
    let expected = files - plan.deleted.len() + plan.added.len();
    if status.files != expected {
        return Err(format!(
            "after churn the map has {} files, expected {expected}",
            status.files
        ));
    }
    let stats = status.last_sweep;
    let untouched = files - plan.modified.len() - plan.renamed.len() - plan.deleted.len();
    if stats.unchanged < untouched {
        return Err(format!(
            "incremental sweep re-read untouched files: {} trusted, {untouched} untouched",
            stats.unchanged
        ));
    }
    if let Some((index, to)) = plan.renamed.first() {
        let wanted = to.to_string_lossy().replace('\\', "/");
        let slice = service.slice(&load_symbol(*index), SLICE_TOKEN_BUDGET);
        if !slice.entries.iter().any(|entry| entry.relative == wanted) {
            return Err(format!(
                "renamed symbol {} not found at {wanted}",
                load_symbol(*index)
            ));
        }
    }
    if let Some(index) = plan.deleted.first() {
        let gone = relative_path(*index).to_string_lossy().replace('\\', "/");
        if service.with_map(|map| map.entry(&gone).is_some()) {
            return Err(format!("deleted file {gone} is still indexed"));
        }
    }
    Ok(())
}

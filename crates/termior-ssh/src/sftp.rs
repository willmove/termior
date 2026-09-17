//! Structured SFTP operations used by the remote File Explorer.
//!
//! The implementation deliberately reuses the installed OpenSSH client and the
//! same profile/credential policy as interactive sessions. Commands are written
//! to SFTP's batch stdin; no remote shell command is ever assembled.

use crate::{quote_sftp_path, Profile, SessionKind};
use std::{
    io::Write as _,
    process::{Command, Stdio},
};

const MAX_RECURSIVE_DELETE_ENTRIES: usize = 20_000;
const MAX_RECURSIVE_DELETE_DEPTH: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteListing {
    /// Canonical working directory reported by SFTP for the requested path.
    pub cwd: String,
    pub entries: Vec<RemoteEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    CreateFile { path: String },
    CreateDirectory { path: String },
    Rename { from: String, to: String },
    RemoveFile { path: String },
    RemoveDirectory { path: String, recursive: bool },
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Profile(#[from] crate::Error),
    #[error("could not start OpenSSH SFTP: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("OpenSSH SFTP failed: {0}")]
    Failed(String),
    #[error("unexpected OpenSSH SFTP output: {0}")]
    InvalidOutput(String),
    #[error("remote directory is too large to delete safely")]
    DeleteLimit,
}

/// List one remote directory. `path` may be `.` to resolve the account's home
/// directory. The returned paths always use POSIX `/` separators.
pub fn list(profile: &Profile, path: &str) -> Result<RemoteListing, Error> {
    validate_remote_path(path)?;
    let quoted = quote_sftp_path(path)?;
    let output = run_batch(profile, &format!("cd {quoted}\npwd\nls -la\nbye\n"))?;
    parse_listing(&output)
}

pub fn execute(profile: &Profile, operation: &Operation) -> Result<(), Error> {
    match operation {
        Operation::CreateFile { path } => create_file(profile, path),
        Operation::CreateDirectory { path } => {
            validate_remote_path(path)?;
            let path = quote_sftp_path(path)?;
            run_batch(profile, &format!("mkdir {path}\nbye\n")).map(|_| ())
        }
        Operation::Rename { from, to } => {
            validate_remote_path(from)?;
            validate_remote_path(to)?;
            let from = quote_sftp_path(from)?;
            let to = quote_sftp_path(to)?;
            run_batch(profile, &format!("rename {from} {to}\nbye\n")).map(|_| ())
        }
        Operation::RemoveFile { path } => {
            validate_remote_path(path)?;
            let path = quote_sftp_path(path)?;
            run_batch(profile, &format!("rm {path}\nbye\n")).map(|_| ())
        }
        Operation::RemoveDirectory { path, recursive } => {
            validate_remote_path(path)?;
            if *recursive {
                remove_directory_recursive(profile, path)
            } else {
                let path = quote_sftp_path(path)?;
                run_batch(profile, &format!("rmdir {path}\nbye\n")).map(|_| ())
            }
        }
    }
}

fn create_file(profile: &Profile, remote_path: &str) -> Result<(), Error> {
    validate_remote_path(remote_path)?;
    let empty = tempfile::NamedTempFile::new()?;
    let local = empty.path().to_string_lossy().replace('\\', "/");
    let local = quote_sftp_path(&local)?;
    let remote = quote_sftp_path(remote_path)?;
    run_batch(profile, &format!("put {local} {remote}\nbye\n")).map(|_| ())
}

fn remove_directory_recursive(profile: &Profile, root: &str) -> Result<(), Error> {
    let mut files = Vec::new();
    let mut directories = Vec::new();
    collect_delete_tree(profile, root, 0, &mut files, &mut directories)?;
    let mut batch = String::new();
    for file in files {
        batch.push_str("rm ");
        batch.push_str(&quote_sftp_path(&file)?);
        batch.push('\n');
    }
    // Children were recorded before parents; delete deepest directories first.
    for directory in directories.into_iter().rev() {
        batch.push_str("rmdir ");
        batch.push_str(&quote_sftp_path(&directory)?);
        batch.push('\n');
    }
    batch.push_str("bye\n");
    run_batch(profile, &batch).map(|_| ())
}

fn collect_delete_tree(
    profile: &Profile,
    path: &str,
    depth: usize,
    files: &mut Vec<String>,
    directories: &mut Vec<String>,
) -> Result<(), Error> {
    if depth > MAX_RECURSIVE_DELETE_DEPTH
        || files.len().saturating_add(directories.len()) >= MAX_RECURSIVE_DELETE_ENTRIES
    {
        return Err(Error::DeleteLimit);
    }
    let listing = list(profile, path)?;
    directories.push(listing.cwd.clone());
    for entry in listing.entries {
        if files.len().saturating_add(directories.len()) >= MAX_RECURSIVE_DELETE_ENTRIES {
            return Err(Error::DeleteLimit);
        }
        if entry.is_dir && !entry.is_symlink {
            collect_delete_tree(profile, &entry.path, depth + 1, files, directories)?;
        } else {
            files.push(entry.path);
        }
    }
    Ok(())
}

fn run_batch(profile: &Profile, batch: &str) -> Result<String, Error> {
    profile.validate()?;
    let invocation = profile.invocation(SessionKind::Sftp)?;
    let askpass_state = tempfile::tempdir()?;
    let mut command = Command::new(invocation.program);
    // `-b` otherwise implies BatchMode=yes. Saved credentials use the existing
    // askpass helper; profiles without saved credentials fail fast unless public
    // key/agent auth succeeds instead of hanging a background Explorer task.
    command.args(["-q", "-o"]);
    command.arg("BatchMode=no");
    command.args(["-b", "-"]);
    command.args(invocation.args);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Explorer jobs have no terminal of their own. Always use the isolated
    // askpass helper: it may read an explicitly saved credential, or display a
    // one-shot masked prompt without persisting the answer.
    let helper = std::env::var_os("TERMIOR_SSH_ASKPASS_EXE")
        .map(std::path::PathBuf::from)
        .unwrap_or(std::env::current_exe()?);
    command
        .env("SSH_ASKPASS", helper)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("TERMIOR_SSH_ASKPASS", "1")
        .env(
            "TERMIOR_SSH_ASKPASS_PROFILE",
            serde_json::to_string(profile)
                .map_err(|error| Error::InvalidOutput(error.to_string()))?,
        )
        .env("TERMIOR_SSH_ASKPASS_STATE", askpass_state.path());

    let mut child = command.spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| Error::InvalidOutput("SFTP stdin was not available".into()))?
        .write_all(batch.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(Error::Failed(if detail.is_empty() {
            format!("exit status {}", output.status)
        } else {
            detail
        }));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn validate_remote_path(path: &str) -> Result<(), crate::Error> {
    quote_sftp_path(path).map(|_| ())
}

pub fn join(base: &str, name: &str) -> Result<String, crate::Error> {
    validate_remote_name(name)?;
    let base = base.trim_end_matches('/');
    Ok(if base.is_empty() {
        format!("/{name}")
    } else if base == "." {
        name.to_owned()
    } else {
        format!("{base}/{name}")
    })
}

pub fn parent(path: &str) -> Option<String> {
    let path = path.trim_end_matches('/');
    if path.is_empty() || path == "/" || path == "." {
        return None;
    }
    match path.rsplit_once('/') {
        Some(("", _)) => Some("/".into()),
        Some((parent, _)) => Some(parent.into()),
        None => Some(".".into()),
    }
}

pub fn file_name(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

pub fn validate_remote_name(name: &str) -> Result<(), crate::Error> {
    if name.is_empty()
        || name != name.trim()
        || matches!(name, "." | "..")
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
    {
        return Err(crate::Error::Invalid(
            "Remote name must be one non-empty path component".into(),
        ));
    }
    Ok(())
}

fn parse_listing(output: &str) -> Result<RemoteListing, Error> {
    let cwd = output
        .lines()
        .find_map(|line| line.trim().strip_prefix("Remote working directory:"))
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| Error::InvalidOutput("missing remote working directory".into()))?
        .to_owned();
    let mut entries = Vec::new();
    for line in output.lines().map(str::trim_end) {
        let line = line.trim_start();
        if line.is_empty()
            || line.starts_with("sftp>")
            || line.starts_with("Remote working directory:")
        {
            continue;
        }
        let Some((permissions, size, mut name)) = parse_long_listing_line(line) else {
            continue;
        };
        let is_symlink = permissions.starts_with('l');
        if is_symlink {
            name = name.split_once(" -> ").map_or(name, |(name, _)| name);
        }
        if matches!(name, "." | "..") {
            continue;
        }
        validate_remote_name(name)?;
        entries.push(RemoteEntry {
            name: name.to_owned(),
            path: join(&cwd, name)?,
            is_dir: permissions.starts_with('d'),
            is_symlink,
            size,
        });
    }
    entries.sort_by(|left, right| match (left.is_dir, right.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
    });
    Ok(RemoteListing { cwd, entries })
}

fn parse_long_listing_line(line: &str) -> Option<(&str, Option<u64>, &str)> {
    let mut rest = line;
    let mut fields = Vec::with_capacity(8);
    for _ in 0..8 {
        rest = rest.trim_start();
        let end = rest.find(char::is_whitespace)?;
        fields.push(&rest[..end]);
        rest = &rest[end..];
    }
    let permissions = fields.first().copied()?;
    if permissions.len() < 10 || !matches!(permissions.as_bytes()[0], b'd' | b'-' | b'l') {
        return None;
    }
    let name = rest.trim_start();
    if name.is_empty() {
        return None;
    }
    Some((
        permissions,
        fields.get(4).and_then(|size| size.parse().ok()),
        name,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pwd_and_long_listing_without_losing_spaces() {
        let output = r#"sftp> cd "/home/me"
sftp> pwd
Remote working directory: /home/me
sftp> ls -la
drwxr-xr-x    4 me users        4096 Sep 17 10:00 .
drwxr-xr-x    3 root root         42 Sep 16 09:00 ..
drwxr-xr-x    2 me users        4096 Sep 17 10:01 source folder
-rw-r--r--    1 me users          12 Sep 17 10:02 中文 file.txt
lrwxrwxrwx    1 me users           3 Sep 17 10:03 link -> src
"#;
        let listing = parse_listing(output).unwrap();
        assert_eq!(listing.cwd, "/home/me");
        assert_eq!(listing.entries.len(), 3);
        assert_eq!(listing.entries[0].path, "/home/me/source folder");
        assert!(listing.entries[0].is_dir);
        assert_eq!(listing.entries[1].name, "link");
        assert!(listing.entries[1].is_symlink);
        assert_eq!(listing.entries[2].name, "中文 file.txt");
        assert_eq!(listing.entries[2].size, Some(12));
    }

    #[test]
    fn remote_path_helpers_are_posix_and_reject_traversal_names() {
        assert_eq!(join("/home/me", "a b").unwrap(), "/home/me/a b");
        assert_eq!(parent("/home/me/a"), Some("/home/me".into()));
        assert_eq!(parent("/a"), Some("/".into()));
        assert_eq!(parent("/"), None);
        assert!(join("/home/me", "../escape").is_err());
        assert!(join("/home/me", "a/b").is_err());
    }

    #[test]
    fn long_listing_parser_ignores_diagnostics() {
        assert!(parse_long_listing_line("Connected to example.").is_none());
        assert!(parse_long_listing_line("Couldn't stat remote file").is_none());
    }

    #[test]
    fn public_api_does_not_accept_control_characters() {
        let profile = Profile {
            name: "example".into(),
            host: "example.test".into(),
            ..Profile::default()
        };
        assert!(list(&profile, "/tmp\nrm x").is_err());
        assert!(execute(
            &profile,
            &Operation::Rename {
                from: "/tmp/a".into(),
                to: "/tmp/b\nrm x".into(),
            }
        )
        .is_err());
    }

    #[test]
    fn tempfile_paths_are_valid_sftp_paths() {
        let file = tempfile::NamedTempFile::new().unwrap();
        assert!(std::path::Path::new(file.path()).exists());
        assert!(quote_sftp_path(&file.path().to_string_lossy().replace('\\', "/")).is_ok());
    }
}

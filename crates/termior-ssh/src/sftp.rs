//! Persistent, per-tab SFTP Explorer. OpenSSH owns authentication and encryption;
//! file operations use framed SFTP v3, never a shell or human-readable listings.
mod wire;

use crate::Profile;
use futures::channel::oneshot;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};
use wire::Transport;

const MAX_ENTRIES: usize = 20_000;
const MAX_DEPTH: usize = 128;
const CACHE_TTL: Duration = Duration::from_secs(30);
const CACHE_DIRECTORIES: usize = 32;
const CACHE_ENTRIES: usize = 50_000;

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
impl Operation {
    fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Rename { from, to } => {
                validate_path(from)?;
                validate_path(to)?;
            }
            Self::CreateFile { path }
            | Self::CreateDirectory { path }
            | Self::RemoveFile { path }
            | Self::RemoveDirectory { path, .. } => validate_path(path)?,
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Profile(#[from] crate::Error),
    #[error("OpenSSH SFTP I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("OpenSSH SFTP: {0}")]
    Failed(String),
    #[error("invalid SFTP response: {0}")]
    InvalidOutput(String),
    #[error("remote directory exceeds the safe enumeration limit")]
    DeleteLimit,
    #[error("Cancelled. A remote write may have partially completed; refresh before retrying")]
    Cancelled,
    #[error(
        "Timed out. Check authentication/connection; a remote write may have partially completed"
    )]
    Timeout,
    #[error("SFTP status {0}: {1}")]
    Status(u32, String),
}

#[derive(Clone)]
pub struct RequestControl(Arc<Control>);
struct Control {
    cancelled: AtomicBool,
    deadline: Instant,
    phase: AtomicUsize,
    completed: AtomicUsize,
}
impl Default for RequestControl {
    fn default() -> Self {
        Self::with_timeout(Duration::from_secs(300))
    }
}
impl RequestControl {
    pub fn with_timeout(timeout: Duration) -> Self {
        Self(Arc::new(Control {
            cancelled: AtomicBool::new(false),
            deadline: Instant::now() + timeout,
            phase: AtomicUsize::new(0),
            completed: AtomicUsize::new(0),
        }))
    }
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
    }
    pub fn status(&self) -> String {
        if self.0.cancelled.load(Ordering::Acquire) {
            return "Cancelling remote request…".into();
        }
        let count = self.0.completed.load(Ordering::Relaxed);
        match self.0.phase.load(Ordering::Relaxed) {
            1 => "Connecting / awaiting SSH authentication…".into(),
            2 => format!("Reading directory… {count} entries"),
            3 => format!("Applying remote operation… {count} items completed"),
            _ => "Waiting for SFTP…".into(),
        }
    }
    fn check(&self, stopped: &AtomicBool) -> Result<(), Error> {
        if self.0.cancelled.load(Ordering::Acquire) || stopped.load(Ordering::Acquire) {
            Err(Error::Cancelled)
        } else if Instant::now() >= self.0.deadline {
            Err(Error::Timeout)
        } else {
            Ok(())
        }
    }
    fn phase(&self, phase: usize) {
        self.0.phase.store(phase, Ordering::Relaxed);
    }
    fn advance(&self, count: usize) {
        self.0.completed.fetch_add(count, Ordering::Relaxed);
    }
}

enum Job {
    List(
        String,
        bool,
        RequestControl,
        oneshot::Sender<Result<RemoteListing, Error>>,
    ),
    Execute(
        Operation,
        RequestControl,
        oneshot::Sender<Result<(), Error>>,
    ),
    Invalidate,
}
struct Handle {
    tx: mpsc::SyncSender<Job>,
    stopped: Arc<AtomicBool>,
    invalidated: Arc<AtomicBool>,
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
}

/// One worker/connection per tab, shared by clones. Close/drop cancels work and
/// reaps the process off the UI thread. Mutations are never automatically retried.
#[derive(Clone)]
pub struct Client(Arc<Handle>);
impl Client {
    pub fn new(profile: Profile) -> Result<Self, Error> {
        Self::with_auth(profile, None)
    }
    pub fn with_auth(profile: Profile, auth: Option<crate::auth::Session>) -> Result<Self, Error> {
        profile.validate()?;
        let (tx, rx) = mpsc::sync_channel(16);
        let stopped = Arc::new(AtomicBool::new(false));
        let invalidated = Arc::new(AtomicBool::new(false));
        let invalidate = invalidated.clone();
        let stop = stopped.clone();
        std::thread::Builder::new()
            .name("sftp-explorer".into())
            .spawn(move || {
                let mut transport: Option<Transport> = None;
                let mut cache = DirectoryCache::default();
                while let Ok(job) = rx.recv() {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    if invalidate.swap(false, Ordering::AcqRel) {
                        cache.clear();
                    }
                    match job {
                        Job::Invalidate => cache.clear(),
                        Job::List(path, force, control, reply) => {
                            let result = (|| {
                                control.check(&stop)?;
                                if !force {
                                    if let Some(listing) = cache.get(&path) {
                                        return Ok(listing);
                                    }
                                }
                                let connection = connection(
                                    &mut transport,
                                    &profile,
                                    auth.as_ref(),
                                    &control,
                                    &stop,
                                )?;
                                control.phase(2);
                                let listing = connection.list(&path, &control, &stop)?;
                                if path != listing.cwd {
                                    cache.insert(listing.cwd.clone(), listing.clone());
                                }
                                cache.insert(path, listing.clone());
                                Ok(listing)
                            })();
                            if result.as_ref().is_err_and(discard_transport) {
                                transport = None;
                                cache.clear();
                            }
                            let _ = reply.send(result);
                        }
                        Job::Execute(operation, control, reply) => {
                            cache.clear(); // Interrupted mutations may still change state.
                            let result = (|| {
                                control.check(&stop)?;
                                let connection = connection(
                                    &mut transport,
                                    &profile,
                                    auth.as_ref(),
                                    &control,
                                    &stop,
                                )?;
                                control.phase(3);
                                connection.execute(&operation, &control, &stop)
                            })();
                            if result.as_ref().is_err_and(discard_transport) {
                                transport = None;
                            }
                            let _ = reply.send(result);
                        }
                    }
                }
            })?;
        Ok(Self(Arc::new(Handle {
            tx,
            stopped,
            invalidated,
        })))
    }
    pub fn close(&self) {
        self.0.stopped.store(true, Ordering::Release);
        let _ = self.0.tx.try_send(Job::Invalidate); // Wake an idle worker.
    }
    pub fn invalidate(&self) {
        // Invalidation must not be lost when the bounded queue is full.
        self.0.invalidated.store(true, Ordering::Release);
        let _ = self.0.tx.try_send(Job::Invalidate);
    }
    pub async fn list(
        &self,
        path: &str,
        force: bool,
        control: RequestControl,
    ) -> Result<RemoteListing, Error> {
        validate_path(path)?;
        let (tx, rx) = oneshot::channel();
        self.send(Job::List(path.to_owned(), force, control, tx))?;
        rx.await.unwrap_or(Err(Error::Cancelled))
    }
    pub async fn execute(
        &self,
        operation: &Operation,
        control: RequestControl,
    ) -> Result<(), Error> {
        operation.validate()?;
        let (tx, rx) = oneshot::channel();
        self.send(Job::Execute(operation.clone(), control, tx))?;
        rx.await.unwrap_or(Err(Error::Cancelled))
    }
    fn send(&self, job: Job) -> Result<(), Error> {
        if self.0.stopped.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        self.0
            .tx
            .try_send(job)
            .map_err(|_| Error::Failed("SFTP request queue unavailable".into()))
    }
}
fn connection<'a>(
    transport: &'a mut Option<Transport>,
    profile: &Profile,
    auth: Option<&crate::auth::Session>,
    control: &RequestControl,
    stopped: &AtomicBool,
) -> Result<&'a mut Transport, Error> {
    if transport.is_none() {
        control.phase(1);
        *transport = Some(Transport::connect(profile, auth, control, stopped)?);
    }
    Ok(transport.as_mut().expect("connected transport"))
}

fn discard_transport(error: &Error) -> bool {
    // A normal server rejection (missing path/permission/existing target) does
    // not invalidate the authenticated connection or justify another prompt.
    !matches!(
        error,
        Error::Status(..) | Error::DeleteLimit | Error::Profile(_)
    )
}

#[derive(Default)]
struct DirectoryCache(HashMap<String, (Instant, RemoteListing)>);
impl DirectoryCache {
    fn clear(&mut self) {
        self.0.clear();
    }
    fn get(&self, path: &str) -> Option<RemoteListing> {
        self.0
            .get(path)
            .filter(|(time, _)| time.elapsed() < CACHE_TTL)
            .map(|(_, listing)| listing.clone())
    }
    fn insert(&mut self, path: String, listing: RemoteListing) {
        self.0.retain(|_, (time, _)| time.elapsed() < CACHE_TTL);
        while !self.0.is_empty()
            && (self.0.len() >= CACHE_DIRECTORIES
                || self.0.values().map(|(_, l)| l.entries.len()).sum::<usize>()
                    + listing.entries.len()
                    > CACHE_ENTRIES)
        {
            let oldest = self
                .0
                .iter()
                .min_by_key(|(_, (time, _))| *time)
                .map(|(key, _)| key.clone())
                .unwrap();
            self.0.remove(&oldest);
        }
        self.0.insert(path, (Instant::now(), listing));
    }
}

/// One-shot convenience APIs. UI callers retain Client instead.
pub fn list(profile: &Profile, path: &str) -> Result<RemoteListing, Error> {
    validate_path(path)?;
    futures::executor::block_on(Client::new(profile.clone())?.list(
        path,
        false,
        RequestControl::default(),
    ))
}
pub fn execute(profile: &Profile, operation: &Operation) -> Result<(), Error> {
    operation.validate()?;
    futures::executor::block_on(
        Client::new(profile.clone())?.execute(operation, RequestControl::default()),
    )
}
fn validate_path(path: &str) -> Result<(), crate::Error> {
    crate::quote_sftp_path(path).map(|_| ())
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
    if path.is_empty() || path == "." {
        return None;
    }
    Some(
        match path.rsplit_once('/') {
            Some(("", _)) => "/",
            Some((parent, _)) => parent,
            None => ".",
        }
        .into(),
    )
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
        || name.contains('/')
        || (cfg!(windows) && name.contains('\\'))
        || name.chars().any(char::is_control)
    {
        return Err(crate::Error::Invalid(
            "Remote name must be one non-empty path component".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_reject_command_injection_and_traversal_names() {
        assert_eq!(join("/home/me", "a b").unwrap(), "/home/me/a b");
        assert_eq!(parent("/a"), Some("/".into()));
        assert_eq!(parent("/"), None);
        assert!(join("/home/me", "../escape").is_err());
        assert!(validate_path("/tmp\nrm x").is_err());
    }
    #[test]
    fn cancellation_and_deadlines_are_observed() {
        let stopped = AtomicBool::new(false);
        let control = RequestControl::default();
        control.cancel();
        assert!(matches!(control.check(&stopped), Err(Error::Cancelled)));
        assert!(matches!(
            RequestControl::with_timeout(Duration::ZERO).check(&stopped),
            Err(Error::Timeout)
        ));
        stopped.store(true, Ordering::Release);
        assert!(matches!(
            RequestControl::default().check(&stopped),
            Err(Error::Cancelled)
        ));
    }
    #[test]
    fn cache_is_bounded_expires_and_invalidates() {
        let mut cache = DirectoryCache::default();
        for i in 0..40 {
            cache.insert(
                i.to_string(),
                RemoteListing {
                    cwd: i.to_string(),
                    entries: vec![],
                },
            );
        }
        assert_eq!(cache.0.len(), CACHE_DIRECTORIES);
        assert!(cache.get("39").is_some());
        cache.0.get_mut("39").unwrap().0 = Instant::now() - CACHE_TTL;
        assert!(cache.get("39").is_none());
        cache.clear();
        assert!(cache.get("38").is_none());
    }
}

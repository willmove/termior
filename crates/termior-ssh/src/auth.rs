//! User-owned authentication coordinator shared by a host's PTY and SFTP clients.
//! Passwords remain in zeroizing memory or the opt-in OS vault. IPC uses owner-only
//! local sockets/pipes, never TCP, argv, environment variables or plaintext files.
use crate::{
    credentials::{self, Kind},
    Profile,
};
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use interprocess::local_socket::{prelude::*, ListenerNonblockingMode, ListenerOptions, Stream};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

const MAX_FRAME: usize = 16 * 1024;
const WAIT: Duration = Duration::from_millis(25);
const AUTH_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChallengeKind {
    Password,
    Passphrase,
    Other,
    Confirm,
    Info,
}
impl ChallengeKind {
    pub fn credential(self) -> Option<Kind> {
        match self {
            Self::Password => Some(Kind::Password),
            Self::Passphrase => Some(Kind::Passphrase),
            _ => None,
        }
    }
}

// Deliberately no Debug on challenges/answers/cache: never log a password.
pub struct Challenge {
    pub profile: Profile,
    pub prompt: String,
    pub kind: ChallengeKind,
    pub remember: bool,
    pub error: Option<String>,
    pub reply: mpsc::SyncSender<Answer>,
    pub cancelled: Arc<AtomicBool>,
}
pub enum Answer {
    Secret {
        value: Zeroizing<String>,
        remember: bool,
    },
    Confirm(bool),
    Acknowledge,
    Cancel,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    name: String,
    transport: u64,
    lifetime: PathBuf,
    broker_lifetime: PathBuf,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    transport: u64,
    prompt: String,
    mode: String,
}
struct Registration {
    path: PathBuf,
    epoch: u64,
}
struct Shared {
    stop: AtomicBool,
    epoch: AtomicU64,
    next: AtomicU64,
    remember: AtomicBool,
    transports: Mutex<HashMap<u64, Registration>>,
}
struct Handle {
    name: String,
    profile: Profile,
    shared: Arc<Shared>,
    _directory: tempfile::TempDir,
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
    }
}

#[derive(Clone)]
pub struct Session(Arc<Handle>);
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SSH authentication session")
    }
}
impl Session {
    pub fn new(profile: Profile) -> io::Result<(Self, UnboundedReceiver<Challenge>)> {
        profile.validate().map_err(io::Error::other)?;
        let directory = tempfile::Builder::new().prefix("ta-").tempdir()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        }
        #[cfg(windows)]
        let name = format!(
            "termior-auth-{}",
            directory.path().file_name().unwrap().to_string_lossy()
        );
        #[cfg(unix)]
        let name = directory.path().join("s").to_string_lossy().into_owned();
        let options = ListenerOptions::new()
            .name(socket_name(&name)?)
            .nonblocking(ListenerNonblockingMode::Both);
        #[cfg(windows)]
        let options = {
            use interprocess::os::windows::{
                local_socket::ListenerOptionsExt, security_descriptor::SecurityDescriptor,
            };
            // OWNER RIGHTS: only the token owner. Remote pipe clients are rejected
            // by interprocess's default; no Everyone/Authenticated Users grants.
            let sddl =
                widestring::U16CString::from_str("D:P(A;;GA;;;OW)").map_err(io::Error::other)?;
            options.security_descriptor(SecurityDescriptor::deserialize(&sddl)?)
        };
        let listener = options.create_sync()?;
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
            next: AtomicU64::new(1),
            remember: AtomicBool::new(profile.use_saved_credentials),
            transports: Mutex::new(HashMap::new()),
        });
        let (tx, rx) = unbounded();
        let control = shared.clone();
        let task_profile = profile.clone();
        std::thread::Builder::new()
            .name("ssh-auth".into())
            .spawn(move || {
                let mut state = State::default();
                // Only the local OpenSSH config is consulted; no connection or credential query.
                let destination = resolve_destination(&task_profile);
                while !control.stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok(mut stream) => {
                            let _ = serve(
                                &mut stream,
                                &task_profile,
                                &destination,
                                &control,
                                &tx,
                                &mut state,
                            );
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(100))
                        }
                        Err(_) => break,
                    }
                }
                // State drops here, zeroizing the session's cached credentials.
            })?;
        Ok((
            Self(Arc::new(Handle {
                name,
                profile,
                shared,
                _directory: directory,
            })),
            rx,
        ))
    }

    pub fn matches(&self, profile: &Profile) -> bool {
        credentials::same_target(&self.0.profile, profile)
    }
    pub fn set_remember(&self, remember: bool) {
        self.0.shared.remember.store(remember, Ordering::Release);
    }
    pub fn register(&self, lifetime: &Path) -> io::Result<Endpoint> {
        if !lifetime.is_dir() {
            return Err(io::Error::other("SSH transport has no lifetime directory"));
        }
        let mut transports = self
            .0
            .shared
            .transports
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        transports.retain(|_, entry| entry.path.is_dir());
        if transports.len() >= 256 {
            return Err(io::Error::other("Too many SSH authentication transports"));
        }
        let id = self.0.shared.next.fetch_add(1, Ordering::Relaxed);
        transports.insert(
            id,
            Registration {
                path: lifetime.into(),
                epoch: self.0.shared.epoch.load(Ordering::Acquire),
            },
        );
        Ok(Endpoint {
            name: self.0.name.clone(),
            transport: id,
            lifetime: lifetime.into(),
            broker_lifetime: self.0._directory.path().into(),
        })
    }
}

#[cfg(windows)]
fn socket_name(name: &str) -> io::Result<interprocess::local_socket::Name<'_>> {
    name.to_ns_name::<interprocess::local_socket::GenericNamespaced>()
}
#[cfg(unix)]
fn socket_name(name: &str) -> io::Result<interprocess::local_socket::Name<'_>> {
    name.to_fs_name::<interprocess::local_socket::GenericFilePath>()
}

/// Called only by the isolated OpenSSH askpass helper. A missing broker is a
/// failure, never an excuse to open a second independent password dialog.
pub fn request(
    endpoint: &Endpoint,
    prompt: &str,
    mode: &str,
) -> io::Result<Option<Zeroizing<String>>> {
    let mut stream = Stream::connect(socket_name(&endpoint.name)?)?;
    stream.set_nonblocking(true)?;
    let deadline = Instant::now() + AUTH_TIMEOUT;
    let check = || {
        if !endpoint.lifetime.is_dir() || !endpoint.broker_lifetime.is_dir() {
            Err(io::Error::other("SSH authentication session closed"))
        } else if Instant::now() < deadline {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "SSH authentication timed out",
            ))
        }
    };
    let bytes = serde_json::to_vec(&Request {
        transport: endpoint.transport,
        prompt: prompt.into(),
        mode: mode.into(),
    })
    .map_err(io::Error::other)?;
    write_frame(&mut stream, &bytes, &check)?;
    let bytes = read_frame(&mut stream, &check)?;
    if bytes.first() != Some(&1) {
        return Ok(None);
    }
    let value = String::from_utf8(bytes[1..].to_vec())
        .map_err(|_| io::Error::other("Invalid authentication reply"))?;
    Ok(Some(Zeroizing::new(value)))
}

struct Cached {
    revision: u64,
    value: Zeroizing<String>,
}
#[derive(Default)]
struct State {
    cache: HashMap<Kind, Cached>,
    attempts: HashMap<(u64, Kind), u64>,
    revision: u64,
    approved_fingerprint: Option<String>,
}

fn serve(
    stream: &mut Stream,
    profile: &Profile,
    destination: &Destination,
    shared: &Shared,
    prompts: &UnboundedSender<Challenge>,
    state: &mut State,
) -> io::Result<()> {
    let header_deadline = Instant::now() + Duration::from_secs(5);
    let bytes = read_frame(stream, &|| {
        if shared.stop.load(Ordering::Acquire) || Instant::now() >= header_deadline {
            Err(io::Error::other("Authentication request expired"))
        } else {
            Ok(())
        }
    })?;
    let request: Request = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    let (path, epoch) = {
        let transports = shared.transports.lock().unwrap_or_else(|e| e.into_inner());
        state
            .attempts
            .retain(|(id, _), _| transports.contains_key(id));
        let registration = transports
            .get(&request.transport)
            .ok_or_else(|| io::Error::other("Unknown SSH transport"))?;
        (registration.path.clone(), registration.epoch)
    };
    let deadline = Instant::now() + AUTH_TIMEOUT;
    let check = || {
        if shared.stop.load(Ordering::Acquire)
            || shared.epoch.load(Ordering::Acquire) != epoch
            || !path.is_dir()
            || Instant::now() >= deadline
        {
            Err(io::Error::other("SSH authentication cancelled or expired"))
        } else {
            Ok(())
        }
    };
    if check().is_err() {
        return write_frame(stream, &[0], &|| Ok(()));
    }
    let kind = if request.mode == "confirm"
        || request
            .prompt
            .contains("Are you sure you want to continue connecting")
    {
        ChallengeKind::Confirm
    } else if request.mode == "none" {
        ChallengeKind::Info
    } else if destination.direct {
        match credentials::prompt_kind(
            profile,
            &request.prompt,
            &destination.user,
            &destination.host,
        ) {
            Some(Kind::Password) => ChallengeKind::Password,
            Some(Kind::Passphrase) => ChallengeKind::Passphrase,
            None => ChallengeKind::Other,
        }
    } else {
        ChallengeKind::Other
    };
    let mut error = (!destination.direct && kind == ChallengeKind::Other)
        .then(|| "代理/跳板或无法确认目标的认证需逐次输入，不自动复用或保存。".into());
    if kind == ChallengeKind::Confirm
        && state.approved_fingerprint.as_deref() == Some(&request.prompt)
    {
        return write_secret(stream, "yes", &check);
    }
    if let Some(credential) = kind.credential() {
        let previous = state
            .attempts
            .get(&(request.transport, credential))
            .copied();
        if let Some(revision) = previous {
            if state
                .cache
                .get(&credential)
                .is_some_and(|cached| cached.revision == revision)
            {
                state.cache.remove(&credential);
                error = Some("上次认证未通过，请重新输入。保存的新密码会替换旧密码。".into());
            }
        }
        if !state.cache.contains_key(&credential)
            && previous.is_none()
            && shared.remember.load(Ordering::Acquire)
        {
            match credentials::load(profile, credential) {
                Ok(Some(value)) => {
                    state.revision += 1;
                    state.cache.insert(
                        credential,
                        Cached {
                            revision: state.revision,
                            value,
                        },
                    );
                }
                Ok(None) => (),
                Err(message) => error = Some(message),
            }
        }
        if let Some(cached) = state.cache.get(&credential) {
            state
                .attempts
                .insert((request.transport, credential), cached.revision);
            return write_secret(stream, &cached.value, &check);
        }
    }
    let (reply, receive) = mpsc::sync_channel(1);
    let cancelled = Arc::new(AtomicBool::new(false));
    prompts
        .unbounded_send(Challenge {
            profile: profile.clone(),
            prompt: request.prompt.clone(),
            kind,
            remember: shared.remember.load(Ordering::Acquire),
            error,
            reply,
            cancelled: cancelled.clone(),
        })
        .map_err(|_| io::Error::other("Authentication UI closed"))?;
    let answer = loop {
        if check().is_err() {
            cancelled.store(true, Ordering::Release);
            return Ok(());
        }
        match receive.recv_timeout(WAIT) {
            Ok(answer) => break answer,
            Err(mpsc::RecvTimeoutError::Timeout) => (),
            Err(_) => break Answer::Cancel,
        }
    };
    match answer {
        Answer::Cancel | Answer::Confirm(false) => {
            // Reject all already-started transports from this login attempt. A
            // later explicit reconnect registers against the new epoch.
            shared.epoch.fetch_add(1, Ordering::AcqRel);
            state.cache.clear();
            state.approved_fingerprint = None;
            write_frame(stream, &[0], &|| Ok(()))
        }
        Answer::Secret { value, remember } => {
            if let Some(credential) = kind.credential() {
                shared.remember.store(remember, Ordering::Release);
                state.revision += 1;
                state
                    .attempts
                    .insert((request.transport, credential), state.revision);
                state.cache.insert(
                    credential,
                    Cached {
                        revision: state.revision,
                        value: value.clone(),
                    },
                );
            }
            write_secret(stream, &value, &check)
        }
        Answer::Confirm(true) => {
            state.approved_fingerprint = Some(request.prompt);
            write_secret(stream, "yes", &check)
        }
        Answer::Acknowledge => write_secret(stream, "", &check),
    }
}

fn write_secret(
    stream: &mut Stream,
    value: &str,
    check: &impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    let mut bytes = Zeroizing::new(vec![1]);
    bytes.extend_from_slice(value.as_bytes());
    write_frame(stream, &bytes, check)
}
fn read_frame(
    stream: &mut Stream,
    check: &impl Fn() -> io::Result<()>,
) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut length = [0; 4];
    read_all(stream, &mut length, check)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME {
        return Err(io::Error::other("Authentication frame too large"));
    }
    let mut bytes = Zeroizing::new(vec![0; length]);
    read_all(stream, &mut bytes, check)?;
    Ok(bytes)
}
fn read_all(
    stream: &mut Stream,
    mut bytes: &mut [u8],
    check: &impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    while !bytes.is_empty() {
        check()?;
        match stream.read(bytes) {
            // Windows PIPE_NOWAIT reports ERROR_NO_DATA when the peer has not
            // replied yet. interprocess maps that to EOF, unlike Unix EAGAIN.
            // Lifetime directories and the deadline still bound the wait.
            #[cfg(windows)]
            Ok(0) => std::thread::sleep(WAIT),
            #[cfg(not(windows))]
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Authentication peer closed",
                ))
            }
            Ok(count) => bytes = &mut bytes[count..],
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                std::thread::sleep(WAIT)
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
fn write_frame(
    stream: &mut Stream,
    bytes: &[u8],
    check: &impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    if bytes.len() > MAX_FRAME {
        return Err(io::Error::other("Authentication frame too large"));
    }
    let mut frame = Zeroizing::new((bytes.len() as u32).to_be_bytes().to_vec());
    frame.extend_from_slice(bytes);
    let mut remaining: &[u8] = &frame;
    while !remaining.is_empty() {
        check()?;
        match stream.write(remaining) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "Authentication peer closed",
                ))
            }
            Ok(count) => remaining = &remaining[count..],
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                std::thread::sleep(WAIT)
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

pub struct Destination {
    pub user: String,
    pub host: String,
    pub direct: bool,
}
/// Fail closed (no caching/vault) when OpenSSH config cannot be resolved.
pub fn resolve_destination(profile: &Profile) -> Destination {
    let mut result = Destination {
        user: profile.user.clone(),
        host: profile.host.clone(),
        direct: false,
    };
    let mut command = Command::new("ssh");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
        .arg("-G")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if !profile.user.is_empty() {
        command.args(["-l", &profile.user]);
    }
    if let Some(port) = profile.port {
        command.args(["-p", &port.to_string()]);
    }
    command.arg("--").arg(&profile.host);
    let Ok(mut child) = command.spawn() else {
        return result;
    };
    let output = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = output.take(1024 * 1024).read_to_string(&mut text);
        let _ = tx.send(text);
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let success = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Err(_) => break false,
            _ if Instant::now() >= deadline => break false,
            _ => std::thread::sleep(WAIT),
        }
    };
    if !success {
        let _ = child.kill();
    }
    let _ = child.wait();
    if success {
        if let Ok(text) = rx.recv_timeout(Duration::from_millis(100)) {
            result.direct = profile.jump_host.is_empty();
            for line in text.lines() {
                if ["proxyjump ", "proxycommand "].iter().any(|prefix| {
                    line.strip_prefix(prefix)
                        .is_some_and(|value| value != "none")
                }) {
                    result.direct = false;
                }
                if let Some(user) = line.strip_prefix("user ") {
                    result.user = user.into();
                }
                if let Some(host) = line.strip_prefix("hostname ") {
                    result.host = host.into();
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn profile() -> Profile {
        Profile {
            name: "auth-test".into(),
            host: "127.0.0.1".into(),
            user: "test".into(),
            ..Profile::default()
        }
    }
    fn challenge(rx: &mut UnboundedReceiver<Challenge>) -> Challenge {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Ok(value) = rx.try_recv() {
                return value;
            }
            assert!(
                Instant::now() < deadline,
                "authentication challenge not delivered"
            );
            std::thread::sleep(WAIT);
        }
    }
    fn ask(
        endpoint: Endpoint,
        prompt: &str,
    ) -> std::thread::JoinHandle<io::Result<Option<Zeroizing<String>>>> {
        let prompt = prompt.to_owned();
        std::thread::spawn(move || request(&endpoint, &prompt, ""))
    }
    #[test]
    fn concurrent_requests_share_one_password_and_rejection_prompts_once() {
        let (session, mut rx) = Session::new(profile()).unwrap();
        let first_life = tempfile::tempdir().unwrap();
        let second_life = tempfile::tempdir().unwrap();
        let first = session.register(first_life.path()).unwrap();
        let second = session.register(second_life.path()).unwrap();
        let a = ask(first.clone(), "test@127.0.0.1's password:");
        let b = ask(second.clone(), "test@127.0.0.1's password:");
        let prompt = challenge(&mut rx);
        assert_eq!(prompt.kind, ChallengeKind::Password);
        prompt
            .reply
            .send(Answer::Secret {
                value: Zeroizing::new("old-secret".into()),
                remember: false,
            })
            .unwrap();
        assert_eq!(
            &**a.join().unwrap().unwrap().as_ref().unwrap(),
            "old-secret"
        );
        assert_eq!(
            &**b.join().unwrap().unwrap().as_ref().unwrap(),
            "old-secret"
        );
        assert!(rx.try_recv().is_err());
        // The same transport asking again means its previous password failed.
        let a = ask(first, "test@127.0.0.1's password:");
        let prompt = challenge(&mut rx);
        assert!(prompt.error.is_some());
        prompt
            .reply
            .send(Answer::Secret {
                value: Zeroizing::new("replacement".into()),
                remember: false,
            })
            .unwrap();
        assert_eq!(
            &**a.join().unwrap().unwrap().as_ref().unwrap(),
            "replacement"
        );
        let b = ask(second, "test@127.0.0.1's password:");
        assert_eq!(
            &**b.join().unwrap().unwrap().as_ref().unwrap(),
            "replacement"
        );
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn otp_is_not_cached_and_cancellation_rejects_already_started_transports() {
        let (session, mut rx) = Session::new(profile()).unwrap();
        let life = tempfile::tempdir().unwrap();
        let endpoint = session.register(life.path()).unwrap();
        for _ in 0..2 {
            let task = ask(endpoint.clone(), "Verification code:");
            let prompt = challenge(&mut rx);
            assert_eq!(prompt.kind, ChallengeKind::Other);
            prompt
                .reply
                .send(Answer::Secret {
                    value: Zeroizing::new("123456".into()),
                    remember: false,
                })
                .unwrap();
            assert!(task.join().unwrap().unwrap().is_some());
        }
        let task = ask(endpoint.clone(), "test@127.0.0.1's password:");
        challenge(&mut rx).reply.send(Answer::Cancel).unwrap();
        assert!(task.join().unwrap().unwrap().is_none());
        assert!(ask(endpoint, "test@127.0.0.1's password:")
            .join()
            .unwrap()
            .unwrap()
            .is_none());
        assert!(rx.try_recv().is_err());
        let fresh = session.register(life.path()).unwrap();
        let task = ask(fresh, "test@127.0.0.1's password:");
        challenge(&mut rx).reply.send(Answer::Cancel).unwrap();
        assert!(task.join().unwrap().unwrap().is_none());
        drop(session);
        assert!(futures::executor::block_on(rx.next()).is_none());
    }
    #[test]
    fn ending_transport_dismisses_pending_prompt_and_drop_ends_broker() {
        let (session, mut rx) = Session::new(profile()).unwrap();
        let life = tempfile::tempdir().unwrap();
        let task = ask(
            session.register(life.path()).unwrap(),
            "test@127.0.0.1's password:",
        );
        let prompt = challenge(&mut rx);
        drop(life);
        assert!(task.join().unwrap().is_err());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !prompt.cancelled.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(WAIT);
        }
        assert!(prompt.cancelled.load(Ordering::Acquire));
        drop(session);
        assert!(futures::executor::block_on(rx.next()).is_none());
    }
}

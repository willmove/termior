//! Minimal SFTP v3 filesystem client over `ssh -T -s host sftp`.
//! Wire format: draft-ietf-secsh-filexfer-02 (also implemented by OpenSSH).
use super::*;
use crate::SessionKind;
use std::{
    io::{Read, Write},
    process::{Child, Command, Stdio},
    sync::Mutex,
};

const MAX_PACKET: usize = 1024 * 1024;
const MAX_STDERR: usize = 8192;
const POLL: Duration = Duration::from_millis(50);

pub(super) struct Transport {
    child: Child,
    outgoing: mpsc::Sender<Vec<u8>>,
    incoming: mpsc::Receiver<std::io::Result<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_done: mpsc::Receiver<()>,
    next_id: u32,
    _askpass: tempfile::TempDir,
}

fn command(profile: &Profile, askpass: &std::path::Path) -> Result<Command, Error> {
    let mut invocation = profile.invocation(SessionKind::Shell)?;
    // Shell invocation ends with [-tt, --, host]. Remove only that flag,
    // not an identity-file argument which happens to have the same spelling.
    invocation.args.remove(invocation.args.len() - 3);
    let mut command = Command::new(invocation.program);
    if profile.authentication != crate::Authentication::Agent {
        command.args([
            "-o",
            "PreferredAuthentications=publickey,password,keyboard-interactive",
        ]);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW, not a hidden interactive console.
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
        .args([
            "-T",
            "-s",
            "-o",
            "BatchMode=no",
            "-o",
            "RequestTTY=no",
            "-o",
            "RemoteCommand=none",
            "-o",
            "ControlMaster=no",
            "-o",
            "ControlPath=none",
            "-o",
            "ControlPersist=no",
            "-o",
            "ForkAfterAuthentication=no",
            "-o",
            "StdinNull=no",
        ])
        .args(invocation.args)
        .arg("sftp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let helper = std::env::var_os("TERMIOR_SSH_ASKPASS_EXE")
        .map(std::path::PathBuf::from)
        .unwrap_or(std::env::current_exe()?);
    command
        .env("SSH_ASKPASS", helper)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("TERMIOR_SSH_ASKPASS", "1")
        .env(
            "TERMIOR_SSH_ASKPASS_PROFILE",
            serde_json::to_string(profile).map_err(|e| Error::InvalidOutput(e.to_string()))?,
        )
        .env("TERMIOR_SSH_ASKPASS_STATE", askpass)
        .env("TERMIOR_SSH_ASKPASS_CANCEL_DIR", askpass);
    Ok(command)
}

impl Transport {
    pub(super) fn connect(
        profile: &Profile,
        auth: Option<&crate::auth::Session>,
        control: &RequestControl,
        stop: &AtomicBool,
    ) -> Result<Self, Error> {
        control.check(stop)?;
        let askpass = tempfile::tempdir()?;
        let mut command = command(profile, askpass.path())?;
        if let Some(auth) = auth {
            let endpoint = auth.register(askpass.path())?;
            command.env(
                "TERMIOR_SSH_AUTH_ENDPOINT",
                serde_json::to_string(&endpoint)
                    .map_err(|e| Error::InvalidOutput(e.to_string()))?,
            );
        }
        let mut child = command.spawn()?;
        let mut input = child.stdin.take().expect("piped stdin");
        let mut output = child.stdout.take().expect("piped stdout");
        let mut errors = child.stderr.take().expect("piped stderr");
        let (outgoing, writes) = mpsc::channel::<Vec<u8>>();
        let (packets, incoming) = mpsc::sync_channel(1);
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let (errors_done, stderr_done) = mpsc::channel();
        let mut transport = Self {
            child,
            outgoing,
            incoming,
            stderr: stderr.clone(),
            stderr_done,
            next_id: 0,
            _askpass: askpass,
        };
        // Isolate both blocking pipe directions: a peer that stops reading or
        // writing must not prevent deadline/cancel checks in the worker.
        std::thread::Builder::new()
            .name("sftp-write".into())
            .spawn(move || {
                while let Ok(packet) = writes.recv() {
                    if input
                        .write_all(&(packet.len() as u32).to_be_bytes())
                        .and_then(|_| input.write_all(&packet))
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        std::thread::Builder::new()
            .name("sftp-read".into())
            .spawn(move || loop {
                let result = read_packet(&mut output);
                let failed = result.is_err();
                if packets.send(result).is_err() || failed {
                    break;
                }
            })?;
        std::thread::Builder::new()
            .name("sftp-errors".into())
            .spawn(move || {
                let mut buffer = [0; 1024];
                while let Ok(count) = errors.read(&mut buffer) {
                    if count == 0 {
                        break;
                    }
                    let mut text = stderr.lock().unwrap_or_else(|e| e.into_inner());
                    text.extend_from_slice(&buffer[..count]);
                    let excess = text.len().saturating_sub(MAX_STDERR);
                    text.drain(..excess);
                }
                let _ = errors_done.send(());
            })?;
        transport
            .outgoing
            .send(vec![1, 0, 0, 0, 3])
            .map_err(|_| Error::Failed("SSH stdin closed".into()))?;
        let version = transport.receive(control, stop)?;
        let mut version = Decoder(&version);
        if version.byte()? != 2 || version.u32()? != 3 {
            return Err(invalid("server does not support SFTP v3"));
        }
        Ok(transport)
    }

    fn receive(&mut self, control: &RequestControl, stop: &AtomicBool) -> Result<Vec<u8>, Error> {
        loop {
            control.check(stop)?;
            match self.incoming.recv_timeout(POLL) {
                Ok(Ok(packet)) => return Ok(packet),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                other => {
                    // stdout EOF and the final authentication diagnostic race on
                    // different pipes. Allow a bounded drain, never an unbounded join.
                    let _ = self.stderr_done.recv_timeout(Duration::from_millis(100));
                    let detail = self.stderr.lock().unwrap_or_else(|e| e.into_inner());
                    let detail = String::from_utf8_lossy(&detail).trim().to_owned();
                    if !detail.is_empty() {
                        return Err(Error::Failed(detail));
                    }
                    return match other {
                        Ok(Err(error)) => Err(error.into()),
                        _ => Err(Error::Failed("SSH connection closed".into())),
                    };
                }
            }
        }
    }

    fn request(
        &mut self,
        kind: u8,
        payload: Vec<u8>,
        control: &RequestControl,
        stop: &AtomicBool,
    ) -> Result<Vec<u8>, Error> {
        control.check(stop)?;
        self.next_id = self.next_id.wrapping_add(1);
        let mut packet = vec![kind];
        put_u32(&mut packet, self.next_id);
        packet.extend(payload);
        if packet.len() > MAX_PACKET {
            return Err(invalid("request too large"));
        }
        self.outgoing
            .send(packet)
            .map_err(|_| Error::Failed("SSH stdin closed".into()))?;
        let packet = self.receive(control, stop)?;
        let mut decoder = Decoder(&packet);
        let kind = decoder.byte()?;
        if decoder.u32()? != self.next_id {
            return Err(invalid("mismatched request ID"));
        }
        if kind == 101 {
            let code = decoder.u32()?;
            let message = decoder.string()?;
            if code != 0 {
                return Err(Error::Status(code, message));
            }
        }
        Ok(packet)
    }

    fn status(
        &mut self,
        kind: u8,
        payload: Vec<u8>,
        control: &RequestControl,
        stop: &AtomicBool,
    ) -> Result<(), Error> {
        let packet = self.request(kind, payload, control, stop)?;
        if packet[0] != 101 {
            return Err(invalid("expected status"));
        }
        Ok(())
    }
    fn handle(
        &mut self,
        kind: u8,
        payload: Vec<u8>,
        control: &RequestControl,
        stop: &AtomicBool,
    ) -> Result<Vec<u8>, Error> {
        let packet = self.request(kind, payload, control, stop)?;
        if packet[0] != 102 {
            return Err(invalid("expected handle"));
        }
        Ok(Decoder(&packet[5..]).bytes()?.to_vec())
    }
    fn realpath(
        &mut self,
        path: &str,
        control: &RequestControl,
        stop: &AtomicBool,
    ) -> Result<String, Error> {
        let packet = self.request(16, string(path.as_bytes()), control, stop)?;
        if packet[0] != 104 {
            return Err(invalid("expected canonical path"));
        }
        let mut decoder = Decoder(&packet[5..]);
        if decoder.u32()? != 1 {
            return Err(invalid("expected one canonical path"));
        }
        let path = decoder.string()?;
        validate_path(&path)?;
        // Some servers return // for the root; no local filesystem resolution.
        Ok(if path.starts_with('/') {
            format!("/{}", path.trim_start_matches('/'))
        } else {
            path
        })
    }

    pub(super) fn list(
        &mut self,
        path: &str,
        control: &RequestControl,
        stop: &AtomicBool,
    ) -> Result<RemoteListing, Error> {
        let cwd = self.realpath(path, control, stop)?;
        let handle = self.handle(11, string(cwd.as_bytes()), control, stop)?;
        let result = (|| {
            let mut entries = Vec::new();
            loop {
                let packet = match self.request(12, string(&handle), control, stop) {
                    Err(Error::Status(1, _)) => break, // EOF is not an error for READDIR.
                    result => result?,
                };
                if packet[0] != 104 {
                    return Err(invalid("expected directory entries"));
                }
                let mut decoder = Decoder(&packet[5..]);
                let count = decoder.u32()? as usize;
                if count == 0 || count > MAX_ENTRIES.saturating_sub(entries.len()) {
                    return Err(Error::DeleteLimit);
                }
                for _ in 0..count {
                    let name = decoder.string()?;
                    decoder.bytes()?; // Ignore server-formatted longname.
                    let (size, mode) = decoder.attributes()?;
                    if matches!(name.as_str(), "." | "..") {
                        continue;
                    }
                    let path = join(&cwd, &name)?;
                    let mode = mode.ok_or_else(|| invalid("missing file type"))? & 0o170000;
                    entries.push(RemoteEntry {
                        name,
                        path,
                        size,
                        is_dir: mode == 0o040000,
                        is_symlink: mode == 0o120000,
                    });
                }
                control.advance(count);
            }
            entries.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            Ok(RemoteListing { cwd, entries })
        })();
        let close = self.status(4, string(&handle), control, stop);
        match result {
            Ok(listing) => {
                close?;
                Ok(listing)
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn execute(
        &mut self,
        operation: &Operation,
        control: &RequestControl,
        stop: &AtomicBool,
    ) -> Result<(), Error> {
        match operation {
            Operation::CreateFile { path } => {
                let mut payload = string(path.as_bytes());
                put_u32(&mut payload, 2 | 8 | 32); // WRITE | CREAT | EXCL: never truncate an existing file.
                put_u32(&mut payload, 0); // Empty ATTRS.
                let handle = self.handle(3, payload, control, stop)?;
                self.status(4, string(&handle), control, stop)?;
            }
            Operation::CreateDirectory { path } => {
                let mut payload = string(path.as_bytes());
                put_u32(&mut payload, 0);
                self.status(14, payload, control, stop)?;
            }
            Operation::Rename { from, to } => {
                let mut payload = string(from.as_bytes());
                payload.extend(string(to.as_bytes()));
                self.status(18, payload, control, stop)?; // v3 RENAME must reject existing destinations.
            }
            Operation::RemoveFile { path } => {
                self.status(13, string(path.as_bytes()), control, stop)?
            }
            Operation::RemoveDirectory {
                path,
                recursive: false,
            } => self.status(15, string(path.as_bytes()), control, stop)?,
            Operation::RemoveDirectory {
                path,
                recursive: true,
            } => {
                let root = self.realpath(path, control, stop)?;
                if root == "/" {
                    return Err(Error::Failed(
                        "refusing recursive deletion of the remote root".into(),
                    ));
                }
                // Refuse a symlink as the recursion root; never enumerate its target.
                let attrs = self.request(7, string(path.as_bytes()), control, stop)?;
                if attrs[0] != 105
                    || Decoder(&attrs[5..]).attributes()?.1.map(|m| m & 0o170000) != Some(0o040000)
                {
                    return Err(Error::Failed(
                        "recursive deletion requires a real directory".into(),
                    ));
                }
                let mut files = Vec::new();
                let mut directories = Vec::new();
                control.phase(2);
                self.collect(&root, 0, &mut files, &mut directories, control, stop)?;
                control.phase(3);
                control.0.completed.store(0, Ordering::Relaxed);
                for file in files {
                    self.status(13, string(file.as_bytes()), control, stop)?;
                    control.advance(1);
                }
                for directory in directories.into_iter().rev() {
                    self.status(15, string(directory.as_bytes()), control, stop)?;
                    control.advance(1);
                }
                return Ok(());
            }
        }
        control.advance(1);
        Ok(())
    }

    fn collect(
        &mut self,
        path: &str,
        depth: usize,
        files: &mut Vec<String>,
        dirs: &mut Vec<String>,
        control: &RequestControl,
        stop: &AtomicBool,
    ) -> Result<(), Error> {
        if depth > MAX_DEPTH || files.len() + dirs.len() >= MAX_ENTRIES {
            return Err(Error::DeleteLimit);
        }
        let listing = self.list(path, control, stop)?;
        dirs.push(listing.cwd);
        for entry in listing.entries {
            if files.len() + dirs.len() >= MAX_ENTRIES {
                return Err(Error::DeleteLimit);
            }
            if entry.is_dir && !entry.is_symlink {
                self.collect(&entry.path, depth + 1, files, dirs, control, stop)?;
            } else {
                files.push(entry.path);
            }
        }
        Ok(())
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        // Worker-only cleanup. Also terminate ProxyJump/askpass descendants.
        if self.child.try_wait().ok().flatten().is_none() {
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                let _ = Command::new("taskkill")
                    .args(["/PID", &self.child.id().to_string(), "/T", "/F"])
                    .creation_flags(0x08000000)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            #[cfg(unix)]
            {
                let _ = Command::new("kill")
                    .args(["-TERM", "--", &format!("-{}", self.child.id())])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn invalid(message: &str) -> Error {
    Error::InvalidOutput(message.into())
}
fn put_u32(buffer: &mut Vec<u8>, value: u32) {
    buffer.extend(value.to_be_bytes());
}
fn string(value: &[u8]) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(value.len() + 4);
    put_u32(&mut buffer, value.len() as u32);
    buffer.extend(value);
    buffer
}
fn read_packet(reader: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut length = [0; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if !(1..=MAX_PACKET).contains(&length) {
        return Err(std::io::Error::other("invalid SFTP packet length"));
    }
    let mut packet = vec![0; length];
    reader.read_exact(&mut packet)?;
    Ok(packet)
}
struct Decoder<'a>(&'a [u8]);
impl<'a> Decoder<'a> {
    fn take(&mut self, size: usize) -> Result<&'a [u8], Error> {
        if size > self.0.len() {
            return Err(invalid("truncated packet"));
        }
        let (value, rest) = self.0.split_at(size);
        self.0 = rest;
        Ok(value)
    }
    fn byte(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn bytes(&mut self) -> Result<&'a [u8], Error> {
        let size = self.u32()? as usize;
        self.take(size)
    }
    fn string(&mut self) -> Result<String, Error> {
        String::from_utf8(self.bytes()?.to_vec())
            .map_err(|_| invalid("non-UTF-8 filename or message"))
    }
    fn attributes(&mut self) -> Result<(Option<u64>, Option<u32>), Error> {
        let flags = self.u32()?;
        if flags & !0x8000000f != 0 {
            return Err(invalid("unknown attribute flags"));
        }
        let size = if flags & 1 != 0 {
            Some(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
        } else {
            None
        };
        if flags & 2 != 0 {
            self.take(8)?;
        }
        let mode = if flags & 4 != 0 {
            Some(self.u32()?)
        } else {
            None
        };
        if flags & 8 != 0 {
            self.take(8)?;
        }
        if flags & 0x80000000 != 0 {
            let count = self.u32()? as usize;
            if count > self.0.len() / 8 {
                return Err(invalid("invalid extended attributes"));
            }
            for _ in 0..count {
                self.bytes()?;
                self.bytes()?;
            }
        }
        Ok((size, mode))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounds_and_attributes() {
        assert!(read_packet(&mut &u32::MAX.to_be_bytes()[..]).is_err());
        assert!(Decoder(&[0, 0, 0, 8, 1]).bytes().is_err());
        let mut attrs = vec![];
        put_u32(&mut attrs, 5);
        attrs.extend(123u64.to_be_bytes());
        put_u32(&mut attrs, 0o120777);
        assert_eq!(
            Decoder(&attrs).attributes().unwrap(),
            (Some(123), Some(0o120777))
        );
    }
    #[test]
    fn subsystem_uses_noninteractive_ssh_with_profile_security() {
        let profile = Profile {
            name: "test".into(),
            host: "::1".into(),
            ..Profile::default()
        };
        let cmd = command(&profile, std::path::Path::new("test-state")).unwrap();
        let args = cmd
            .get_args()
            .map(|s| s.to_str().unwrap())
            .collect::<Vec<_>>();
        assert!(args.contains(&"-s"));
        assert!(args.contains(&"-T"));
        assert!(!args.contains(&"-tt"));
        assert!(args.contains(&"StrictHostKeyChecking=ask"));
        assert_eq!(&args[args.len() - 3..], &["--", "::1", "sftp"]);
    }
}

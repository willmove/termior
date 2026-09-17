//! Real OpenSSH over the production PTY bridge, with an isolated loopback fixture.
//! Run: TERMIOR_SSH_TEST_PYTHON=<venv python> cargo test -p termior-terminal --test ssh_roundtrip -- --ignored
use alacritty_terminal::{
    term::{test::TermSize, Config, Term},
    vte::ansi::{Processor, StdSyncHandler},
};
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use termior_ssh::{Authentication, Connection, Profile, SessionKind, Transfer};
use termior_terminal::{PtySessionConfig, TerminalBridge, TerminalEventProxy};

struct Server(Child);
struct SavedCredential(Profile, termior_ssh::credentials::Kind);
impl Drop for SavedCredential {
    fn drop(&mut self) {
        termior_ssh::credentials::delete(&self.0, self.1).expect("clean up test credential");
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run(connection: Connection, password: Option<&str>, shell: bool) -> (Option<i32>, String) {
    run_with_auth(connection, password, shell, None)
}

fn run_with_auth(
    connection: Connection,
    password: Option<&str>,
    shell: bool,
    auth_session: Option<termior_ssh::auth::Session>,
) -> (Option<i32>, String) {
    let mut bridge = TerminalBridge::spawn(&PtySessionConfig {
        remote: Some(connection),
        auth_session,
        ..Default::default()
    })
    .unwrap();
    let mut rx = bridge.take_output().unwrap();
    let mut exit = bridge.take_exit().unwrap();
    let writer = bridge.writer();
    let (proxy, _events) = TerminalEventProxy::new(writer.clone());
    let mut term = Term::new(Config::default(), &TermSize::new(80, 24), proxy);
    let mut processor = Processor::<StdSyncHandler>::default();
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut output = String::new();
    let mut sent_password = false;
    let mut sent_exit = false;
    loop {
        while let Ok(data) = rx.try_recv() {
            processor.advance(&mut term, &data.bytes);
            output.push_str(&String::from_utf8_lossy(&data.bytes));
        }
        if !sent_password
            && (output.contains("password:")
                || output.contains("Enter passphrase")
                || output.contains("Verification code:"))
        {
            if let Some(password) = password {
                writer
                    .write_all(format!("{password}\r").as_bytes())
                    .unwrap();
                sent_password = true;
            }
        }
        if shell && !sent_exit && output.contains("fixture-shell-ready") {
            bridge.resize(40, 120).unwrap();
            writer.write_all(b"exit\r").unwrap();
            sent_exit = true;
        }
        if let Ok(Some(code)) = exit.try_recv() {
            std::thread::sleep(Duration::from_millis(200));
            while let Ok(data) = rx.try_recv() {
                output.push_str(&String::from_utf8_lossy(&data.bytes));
            }
            return (code, output);
        }
        assert!(Instant::now() < deadline, "SSH fixture timeout: {output}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "requires Python paramiko and system OpenSSH; all server keys/files are temporary"]
fn ssh_and_sftp_real_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let python = std::env::var("TERMIOR_SSH_TEST_PYTHON")
        .expect("set TERMIOR_SSH_TEST_PYTHON to a paramiko environment");
    let mut server = Server(
        Command::new(python)
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/ssh_server.py"
            ))
            .arg(dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    BufReader::new(server.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port: u16 = line
        .trim()
        .trim_start_matches("{\"port\": ")
        .trim_end_matches('}')
        .parse()
        .expect("fixture port");
    let profile = Profile {
        name: "fixture".into(),
        host: "127.0.0.1".into(),
        user: "test".into(),
        port: Some(port),
        authentication: Authentication::Key,
        identity_file: dir.path().join("identity").display().to_string(),
        known_hosts_file: dir.path().join("known_hosts").display().to_string(),
        ..Profile::default()
    };
    // The production Explorer retains one SFTP subsystem for all these requests.
    use futures::executor::block_on;
    use termior_ssh::sftp::{Client, Error as SftpError, RequestControl};
    let explorer = Client::new(profile.clone()).unwrap();
    let count = |name: &str| {
        std::fs::read_to_string(dir.path().join(name))
            .unwrap_or_default()
            .lines()
            .count()
    };
    std::fs::write(dir.path().join("remote/existing file.txt"), b"existing").unwrap();
    std::fs::write(dir.path().join("remote/literal ; [1] file.txt"), b"literal").unwrap();
    #[cfg(unix)]
    std::fs::write(dir.path().join("remote/literal -> target.txt"), b"literal").unwrap();
    let listing = block_on(explorer.list("/", false, RequestControl::default())).unwrap();
    assert_eq!(listing.cwd, "/");
    assert!(listing
        .entries
        .iter()
        .any(|entry| entry.name == "existing file.txt" && !entry.is_dir));
    assert!(listing
        .entries
        .iter()
        .any(|entry| entry.name == "literal ; [1] file.txt"));
    #[cfg(unix)]
    assert!(listing
        .entries
        .iter()
        .any(|entry| entry.name == "literal -> target.txt"));
    assert_eq!(count("connections.log"), 1);
    let scans = count("listings.log");
    block_on(explorer.list("/", false, RequestControl::default())).unwrap();
    assert_eq!(
        count("listings.log"),
        scans,
        "cached directory does not issue READDIR"
    );
    block_on(explorer.list("/", true, RequestControl::default())).unwrap();
    assert_eq!(count("listings.log"), scans + 1, "refresh bypasses cache");
    for operation in [
        termior_ssh::sftp::Operation::CreateDirectory {
            path: "/远程 folder".into(),
        },
        termior_ssh::sftp::Operation::CreateFile {
            path: "/远程 folder/[draft] file.txt".into(),
        },
        termior_ssh::sftp::Operation::Rename {
            from: "/远程 folder/[draft] file.txt".into(),
            to: "/远程 folder/final file.txt".into(),
        },
    ] {
        block_on(explorer.execute(&operation, RequestControl::default())).unwrap();
    }
    let root = block_on(explorer.list("/", false, RequestControl::default())).unwrap();
    assert!(
        root.entries.iter().any(|entry| entry.name == "远程 folder"),
        "mutation invalidates cached parent"
    );
    let listing =
        block_on(explorer.list("/远程 folder", false, RequestControl::default())).unwrap();
    assert!(listing
        .entries
        .iter()
        .any(|entry| entry.name == "final file.txt"));
    block_on(explorer.execute(
        &termior_ssh::sftp::Operation::RemoveDirectory {
            path: "/远程 folder".into(),
            recursive: true,
        },
        RequestControl::default(),
    ))
    .unwrap();
    assert!(!dir.path().join("remote/远程 folder").exists());
    let error = block_on(explorer.execute(
        &termior_ssh::sftp::Operation::CreateFile {
            path: "/existing file.txt".into(),
        },
        RequestControl::default(),
    ))
    .unwrap_err();
    assert!(matches!(error, SftpError::Status(..)), "{error}");
    assert_eq!(
        std::fs::read(dir.path().join("remote/existing file.txt")).unwrap(),
        b"existing"
    );
    assert!(block_on(explorer.list("/missing", true, RequestControl::default())).is_err());
    block_on(explorer.list("/", true, RequestControl::default())).unwrap();
    assert_eq!(
        count("connections.log"),
        1,
        "navigation, mutations and ordinary errors reuse authentication"
    );

    // A hung READDIR must be cancellable, reap its transport, and permit an
    // explicit retry without replaying any mutations.
    std::fs::create_dir(dir.path().join("remote/slow")).unwrap();
    let control = RequestControl::default();
    let task_control = control.clone();
    let task_client = explorer.clone();
    let pending =
        std::thread::spawn(move || block_on(task_client.list("/slow", true, task_control)));
    let deadline = Instant::now() + Duration::from_secs(3);
    while !std::fs::read_to_string(dir.path().join("listings.log"))
        .unwrap()
        .contains("/slow")
    {
        assert!(Instant::now() < deadline, "slow request reached server");
        std::thread::sleep(Duration::from_millis(10));
    }
    let start = Instant::now();
    control.cancel();
    assert!(matches!(pending.join().unwrap(), Err(SftpError::Cancelled)));
    assert!(start.elapsed() < Duration::from_secs(2));
    block_on(explorer.list("/", true, RequestControl::default())).unwrap();
    assert_eq!(count("connections.log"), 2);
    let start = Instant::now();
    assert!(matches!(
        block_on(explorer.list(
            "/slow",
            true,
            RequestControl::with_timeout(Duration::from_millis(250))
        )),
        Err(SftpError::Timeout)
    ));
    assert!(start.elapsed() < Duration::from_secs(2));
    block_on(explorer.list("/", true, RequestControl::default())).unwrap();
    assert_eq!(count("connections.log"), 3);
    explorer.close();
    assert!(matches!(
        block_on(explorer.list("/", false, RequestControl::default())),
        Err(SftpError::Cancelled)
    ));
    let deadline = Instant::now() + Duration::from_secs(3);
    while count("disconnected.log") < 3 {
        assert!(
            Instant::now() < deadline,
            "close must tear down the idle SSH transport"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let disposable = Client::new(profile.clone()).unwrap();
    block_on(disposable.list("/", true, RequestControl::default())).unwrap();
    drop(disposable);
    let deadline = Instant::now() + Duration::from_secs(3);
    while count("disconnected.log") < 4 {
        assert!(
            Instant::now() < deadline,
            "dropping the final handle must tear down SSH"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut untrusted = profile.clone();
    untrusted.known_hosts_file = dir.path().join("wrong_hosts").display().to_string();
    let rejected = block_on(Client::new(untrusted).unwrap().list(
        "/",
        true,
        RequestControl::default(),
    ))
    .unwrap_err();
    assert!(
        rejected
            .to_string()
            .contains("REMOTE HOST IDENTIFICATION HAS CHANGED"),
        "host-key diagnostic is retained in-app: {rejected}"
    );
    let (code, output) = run(
        Connection {
            profile: profile.clone(),
            kind: SessionKind::Shell,
            transfer: None,
        },
        None,
        true,
    );
    assert_eq!(code, Some(0), "{output}");
    let mut encrypted = profile.clone();
    encrypted.identity_file = dir.path().join("encrypted_identity").display().to_string();
    let (code, output) = run(
        Connection {
            profile: encrypted,
            kind: SessionKind::Shell,
            transfer: None,
        },
        Some("fixture-password"),
        true,
    );
    assert_eq!(code, Some(0), "{output}");
    let source = dir.path().join("上传 [1] file.txt");
    if std::env::var_os("TERMIOR_SSH_ASKPASS_EXE").is_some() {
        use futures::StreamExt;
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        use termior_ssh::auth::{Answer, ChallengeKind, Session};
        use termior_ssh::credentials::{self, Kind};
        let mut shared_profile = profile.clone();
        shared_profile.authentication = Authentication::Password;
        let (auth, mut prompts) = Session::new(shared_profile.clone()).unwrap();
        let prompt_count = Arc::new(AtomicUsize::new(0));
        let observed = prompt_count.clone();
        let responder = std::thread::spawn(move || {
            block_on(async move {
                while let Some(prompt) = prompts.next().await {
                    assert_eq!(prompt.kind, ChallengeKind::Password);
                    observed.fetch_add(1, Ordering::SeqCst);
                    prompt
                        .reply
                        .send(Answer::Secret {
                            value: "fixture-password".to_owned().into(),
                            remember: false,
                        })
                        .unwrap();
                }
            })
        });
        let shared_explorer =
            Client::with_auth(shared_profile.clone(), Some(auth.clone())).unwrap();
        let pending_explorer = shared_explorer.clone();
        let listing = std::thread::spawn(move || {
            block_on(pending_explorer.list(
                "/",
                true,
                RequestControl::with_timeout(Duration::from_secs(20)),
            ))
        });
        for _ in 0..2 {
            let (code, output) = run_with_auth(
                Connection {
                    profile: shared_profile.clone(),
                    kind: SessionKind::Shell,
                    transfer: None,
                },
                None,
                true,
                Some(auth.clone()),
            );
            assert_eq!(code, Some(0), "shared authentication: {output}");
            assert!(
                !output.contains("password:") && !output.contains("fixture-password"),
                "terminal must not ask for or echo the shared password"
            );
        }
        listing.join().unwrap().unwrap();
        assert_eq!(
            prompt_count.load(Ordering::SeqCst),
            1,
            "PTY, Explorer and another tab share one password"
        );
        shared_explorer.close();
        drop(shared_explorer);
        drop(auth);
        responder.join().unwrap();
        for kind in [Kind::Password, Kind::Passphrase] {
            let mut saved = profile.clone();
            saved.use_saved_credentials = true;
            saved.authentication = if kind == Kind::Password {
                Authentication::Password
            } else {
                Authentication::Key
            };
            if kind == Kind::Passphrase {
                saved.identity_file = dir.path().join("encrypted_identity").display().to_string();
            }
            credentials::save(&saved, kind, "fixture-password").unwrap();
            let _cleanup = SavedCredential(saved.clone(), kind);
            // A fresh coordinator (as after app restart) reads the opt-in vault
            // without raising a UI challenge, for both PTY and Explorer.
            let (saved_auth, mut prompts) = Session::new(saved.clone()).unwrap();
            let broker_explorer =
                Client::with_auth(saved.clone(), Some(saved_auth.clone())).unwrap();
            block_on(broker_explorer.list(
                "/",
                true,
                RequestControl::with_timeout(Duration::from_secs(20)),
            ))
            .unwrap();
            let (code, output) = run_with_auth(
                Connection {
                    profile: saved.clone(),
                    kind: SessionKind::Shell,
                    transfer: None,
                },
                None,
                true,
                Some(saved_auth.clone()),
            );
            assert_eq!(code, Some(0), "saved shared {kind:?}: {output}");
            assert!(
                prompts.try_recv().is_err(),
                "saved credentials must not prompt"
            );
            broker_explorer.close();
            drop(saved_auth);
            let saved_explorer = Client::new(saved.clone()).unwrap();
            let connections = count("connections.log");
            for _ in 0..2 {
                block_on(saved_explorer.list(
                    "/",
                    true,
                    RequestControl::with_timeout(Duration::from_secs(20)),
                ))
                .unwrap();
            }
            assert_eq!(
                count("connections.log"),
                connections + 1,
                "saved {kind:?}: Explorer reuses authentication"
            );
            saved_explorer.close();
            // Two separate connections must both authenticate without any PTY input.
            for _ in 0..2 {
                let (code, output) = run(
                    Connection {
                        profile: saved.clone(),
                        kind: SessionKind::Shell,
                        transfer: None,
                    },
                    None,
                    true,
                );
                assert_eq!(code, Some(0), "saved {kind:?}: {output}");
                assert!(!output.contains("fixture-password"), "secret leaked to PTY");
            }
            let destination = dir.path().join(format!("vault-{kind:?}.txt"));
            std::fs::write(dir.path().join("remote/vault.txt"), b"vault transfer").unwrap();
            let (code, output) = run(
                Connection {
                    profile: saved,
                    kind: SessionKind::Sftp,
                    transfer: Some(Transfer {
                        upload: false,
                        local_path: destination.display().to_string(),
                        remote_path: "/vault.txt".into(),
                        recursive: false,
                        resume: false,
                    }),
                },
                None,
                false,
            );
            assert_eq!(code, Some(0), "saved SFTP {kind:?}: {output}");
            assert_eq!(std::fs::read(destination).unwrap(), b"vault transfer");
        }
    }
    let mut mfa = profile.clone();
    mfa.user = "mfa".into();
    mfa.authentication = Authentication::Password;
    let (code, output) = run(
        Connection {
            profile: mfa,
            kind: SessionKind::Shell,
            transfer: None,
        },
        Some("123456"),
        true,
    );
    assert_eq!(code, Some(0), "{output}");
    let bytes = b"SSH SFTP roundtrip\0binary\n".repeat(10000);
    std::fs::write(&source, &bytes).unwrap();
    let transfer = Transfer {
        upload: true,
        local_path: source.display().to_string(),
        remote_path: "/上传 [1] file.txt".into(),
        recursive: false,
        resume: false,
    };
    let (code, output) = run(
        Connection {
            profile: profile.clone(),
            kind: SessionKind::Sftp,
            transfer: Some(transfer.clone()),
        },
        None,
        false,
    );
    assert_eq!(code, Some(0), "{output}");
    assert_eq!(
        std::fs::read(dir.path().join("remote/上传 [1] file.txt")).unwrap(),
        bytes
    );
    let mut password_profile = profile.clone();
    password_profile.authentication = Authentication::Password;
    let destination = dir.path().join("download.txt");
    let (code, output) = run(
        Connection {
            profile: password_profile,
            kind: SessionKind::Sftp,
            transfer: Some(Transfer {
                upload: false,
                local_path: destination.display().to_string(),
                ..transfer.clone()
            }),
        },
        Some("fixture-password"),
        false,
    );
    assert_eq!(code, Some(0), "{output}");
    assert_eq!(std::fs::read(destination).unwrap(), bytes);
    // Resume a partial remote upload and local download, checking exact bytes.
    std::fs::write(dir.path().join("remote/上传 [1] file.txt"), &bytes[..16384]).unwrap();
    let (code, output) = run(
        Connection {
            profile: profile.clone(),
            kind: SessionKind::Sftp,
            transfer: Some(Transfer {
                resume: true,
                ..transfer.clone()
            }),
        },
        None,
        false,
    );
    assert_eq!(code, Some(0), "{output}");
    assert_eq!(
        std::fs::read(dir.path().join("remote/上传 [1] file.txt")).unwrap(),
        bytes
    );
    let resumed = dir.path().join("resumed.txt");
    std::fs::write(&resumed, &bytes[..8192]).unwrap();
    let (code, output) = run(
        Connection {
            profile: profile.clone(),
            kind: SessionKind::Sftp,
            transfer: Some(Transfer {
                upload: false,
                resume: true,
                local_path: resumed.display().to_string(),
                ..transfer.clone()
            }),
        },
        None,
        false,
    );
    assert_eq!(code, Some(0), "{output}");
    assert_eq!(std::fs::read(resumed).unwrap(), bytes);
    let folder = dir.path().join("folder");
    std::fs::create_dir_all(folder.join("child")).unwrap();
    std::fs::write(folder.join("child/data"), &bytes).unwrap();
    let (code, output) = run(
        Connection {
            profile: profile.clone(),
            kind: SessionKind::Sftp,
            transfer: Some(Transfer {
                recursive: true,
                local_path: folder.display().to_string(),
                remote_path: "/folder".into(),
                ..transfer.clone()
            }),
        },
        None,
        false,
    );
    assert_eq!(code, Some(0), "{output}");
    assert_eq!(
        std::fs::read(dir.path().join("remote/folder/child/data")).unwrap(),
        bytes
    );
    // Failed commands must be nonzero, never displayed as successful transfers.
    let (code, _) = run(
        Connection {
            profile: profile.clone(),
            kind: SessionKind::Sftp,
            transfer: Some(Transfer {
                upload: false,
                remote_path: "/missing-file".into(),
                local_path: dir.path().join("missing").display().to_string(),
                ..transfer
            }),
        },
        None,
        false,
    );
    assert!(code.is_some_and(|code| code != 0));
    // A changed trust record is rejected before authentication or transfer.
    let mut wrong = profile;
    wrong.known_hosts_file = dir.path().join("wrong_hosts").display().to_string();
    let (code, output) = run(
        Connection {
            profile: wrong,
            kind: SessionKind::Shell,
            transfer: None,
        },
        None,
        true,
    );
    assert!(code.is_some_and(|code| code != 0), "{output}");
    assert!(!output.contains("fixture-shell-ready"));
}

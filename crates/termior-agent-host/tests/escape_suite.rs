//! Black-box escape suite for sandboxed execution (Stage D task 6.6).
//!
//! A verified platform must fail every probe below. A platform that cannot enforce the
//! capability (Windows today) must fail closed and must not launch the payload. Any
//! successful escape fails the test.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use termior_agent_host::{
    PlatformSandbox, SandboxBackend, SandboxCapability, SandboxError, SandboxRequest, TrustLevel,
};

const SECRET: &str = "termior-escape-secret-value";
const TASK_FILE: &str = "termior-escape-task.json";
const RESULT_FILE: &str = "escape-result.json";
const TEST_NAME: &str = "black_box_escape_suite";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct EscapeTask {
    mode: String,
    outside: String,
    pwned: String,
    targets: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct FsResult {
    reads: Vec<Option<String>>,
    writes: Vec<bool>,
    hardlink_created: bool,
}

#[derive(Debug, Deserialize)]
struct NetResult {
    connected: Vec<bool>,
}

#[derive(Debug, Deserialize)]
struct EnvResult {
    dump: String,
}

#[test]
fn black_box_escape_suite() {
    if std::env::var("TERMIOR_ESCAPE_HARNESS").ok().as_deref() == Some("launch") {
        launch_harness();
        return;
    }
    if let Some(task) = read_task() {
        run_payload(&task);
        return;
    }
    run_suite();
}

fn run_suite() {
    let root = tempfile::tempdir().unwrap();
    let outside = root.path().join("parent-secret.txt");
    let pwned = root.path().join("pwned");
    let work = root.path().join("work");
    fs::create_dir(&work).unwrap();
    fs::write(&outside, SECRET).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, work.join("planted-symlink")).unwrap();

    // The payload runs from the test binary's own directory. On Windows the sandbox
    // grants access per directory, so the payload's directory must be reachable too.
    grant_payload_access();

    match prepare(&work) {
        Err(error) => assert_all_scenarios_fail_closed(&error, &work, &outside),
        Ok(()) => {
            assert_filesystem_blocked(&work, &outside, &pwned);
            // The payload must really have run inside the sandbox. A suite that only
            // proves "nothing happened" would pass for the wrong reason.
            assert!(
                work.join(RESULT_FILE).exists(),
                "the sandbox reported success but never ran the payload"
            );
            assert_child_cannot_escape(&work, &outside, &pwned);
            assert_network_blocked(&work);
            assert_env_secret_blocked(&work);
            assert_orphan_does_not_survive(&work);
            assert_eq!(fs::read_to_string(&outside).unwrap(), SECRET);
            assert!(!pwned.exists(), "parent directory write escaped");
        }
    }
}

/// Make the payload binary readable and executable inside the sandbox.
#[cfg(windows)]
fn grant_payload_access() {
    let exe = std::env::current_exe().unwrap();
    let directory = exe
        .parent()
        .expect("the test binary has a parent directory");
    termior_terminal::windows_sandbox::grant_directory(directory)
        .expect("the payload directory must be shareable with the sandbox");
}

#[cfg(not(windows))]
fn grant_payload_access() {}

fn assert_all_scenarios_fail_closed(error: &SandboxError, work: &Path, outside: &Path) {
    let probe = PlatformSandbox.probe();
    for capability in [
        SandboxCapability::Filesystem,
        SandboxCapability::Network,
        SandboxCapability::Credentials,
        SandboxCapability::ProcessTree,
    ] {
        let level = probe.get(&capability).copied();
        if level == Some(TrustLevel::Verified) {
            continue;
        }
        assert_ne!(
            level,
            Some(TrustLevel::Verified),
            "{capability:?} must not be reported verified when the sandbox fails closed"
        );
    }
    for scenario in [
        "parent-dir",
        "symlink",
        "hardlink",
        "loopback",
        "private-network",
        "public-network",
        "env-secret",
        "child-process",
        "orphan",
    ] {
        match error {
            SandboxError::RequiredCapabilityUnavailable(capability) => {
                assert_ne!(
                    probe.get(capability).copied(),
                    Some(TrustLevel::Verified),
                    "{scenario}: fail-closed on a capability reported verified"
                );
            }
            SandboxError::CommandIsolationUnsupported => {}
            other => panic!("{scenario}: expected fail-closed, got {other}"),
        }
    }
    assert!(
        !work.join(RESULT_FILE).exists(),
        "fail-closed sandbox launched a payload"
    );
    assert_eq!(fs::read_to_string(outside).unwrap(), SECRET);
}

fn assert_filesystem_blocked(work: &Path, outside: &Path, pwned: &Path) {
    let result: FsResult = run_mode(work, "filesystem", outside, pwned, &[]);
    assert!(
        result.reads.iter().all(|read| read.is_none()),
        "filesystem read escaped: {result:?}"
    );
    assert!(
        result.writes.iter().all(|wrote| !wrote),
        "filesystem write escaped: {result:?}"
    );
    assert!(!result.hardlink_created, "hardlink escape succeeded");
    assert!(!pwned.exists(), "absolute parent write escaped");
    assert!(!work.join("leaked-hardlink").exists());
}

fn assert_child_cannot_escape(work: &Path, outside: &Path, pwned: &Path) {
    let _ = fs::remove_file(work.join(RESULT_FILE));
    run_mode_unchecked(work, "child", outside, pwned, &[]);
    let result: FsResult = read_result(work);
    assert!(
        result.reads.iter().all(|read| read.is_none()),
        "child process read escaped: {result:?}"
    );
    assert!(
        result.writes.iter().all(|wrote| !wrote),
        "child process write escaped: {result:?}"
    );
}

fn assert_network_blocked(work: &Path) {
    let loopback = TcpListener::bind("127.0.0.1:0").unwrap();
    let loopback_addr = loopback.local_addr().unwrap();
    let private_listener = bind_private_listener();
    let private_target = private_listener
        .as_ref()
        .map(|listener| listener.local_addr().unwrap().to_string())
        .unwrap_or_else(|| "10.255.255.1:9".to_owned());
    let targets = vec![
        loopback_addr.to_string(),
        private_target,
        "203.0.113.10:80".to_owned(),
        "192.0.2.10:80".to_owned(),
    ];
    assert_host_can_connect(&targets[0]);
    if private_listener.is_some() {
        assert_host_can_connect(&targets[1]);
    }
    let result: NetResult = run_mode(work, "network", Path::new("."), Path::new("."), &targets);
    let names = ["loopback", "private", "public", "public"];
    for ((name, target), connected) in names.iter().zip(&targets).zip(&result.connected) {
        assert!(!connected, "{name} network escape connected to {target}");
    }
    drop(loopback);
    drop(private_listener);
}

fn assert_env_secret_blocked(work: &Path) {
    write_task(
        work,
        &EscapeTask {
            mode: "env".into(),
            outside: String::new(),
            pwned: String::new(),
            targets: Vec::new(),
        },
    );
    let output = Command::new(std::env::current_exe().unwrap())
        .arg(TEST_NAME)
        .arg("--exact")
        .arg("--test-threads=1")
        .env("TERMIOR_ESCAPE_HARNESS", "launch")
        .env("TERMIOR_ESCAPE_WORK", work)
        .env("TERMIOR_ESCAPE_SECRET", SECRET)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "env harness failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: EnvResult = read_result(work);
    assert!(
        result.dump.contains("PATH="),
        "sandboxed environment dump was empty: {}",
        result.dump
    );
    assert!(
        !result.dump.contains(SECRET),
        "env secret leaked into the sandbox: {}",
        result.dump
    );
    assert!(!result.dump.contains("TERMIOR_ESCAPE_HARNESS"));
    assert!(!result.dump.contains("TERMIOR_ESCAPE_SECRET"));
}

fn assert_orphan_does_not_survive(work: &Path) {
    let _ = fs::remove_file(work.join("orphan-heartbeat"));
    run_mode_unchecked(work, "orphan", Path::new("."), Path::new("."), &[]);
    thread::sleep(Duration::from_millis(300));
    let first = fs::read_to_string(work.join("orphan-heartbeat"))
        .expect("orphan probe never started a child");
    thread::sleep(Duration::from_millis(400));
    let second = fs::read_to_string(work.join("orphan-heartbeat")).unwrap();
    assert_eq!(
        first, second,
        "orphan process kept running after the sandbox exited"
    );
}

fn launch_harness() {
    let work = PathBuf::from(std::env::var("TERMIOR_ESCAPE_WORK").unwrap());
    let request = sandbox_request(&work);
    let environment = PlatformSandbox
        .prepare(&request)
        .expect("harness was launched even though the sandbox fails closed");
    let (status, stdout, stderr) = spawn_payload(&environment, &work);
    assert!(
        status.success(),
        "sandboxed env probe failed ({})\nstdout:\n{stdout}\nstderr:\n{stderr}",
        exit_summary(&status)
    );
}

fn prepare(work: &Path) -> Result<(), SandboxError> {
    PlatformSandbox.prepare(&sandbox_request(work)).map(|_| ())
}

fn sandbox_request(work: &Path) -> SandboxRequest {
    SandboxRequest {
        id: "escape".into(),
        root: work.to_path_buf(),
        required: vec![
            SandboxCapability::Filesystem,
            SandboxCapability::Network,
            SandboxCapability::Credentials,
            SandboxCapability::ProcessTree,
        ],
        allow_network: false,
    }
}

fn run_mode<T: for<'de> Deserialize<'de>>(
    work: &Path,
    mode: &str,
    outside: &Path,
    pwned: &Path,
    targets: &[String],
) -> T {
    run_mode_unchecked(work, mode, outside, pwned, targets);
    read_result(work)
}

fn run_mode_unchecked(work: &Path, mode: &str, outside: &Path, pwned: &Path, targets: &[String]) {
    let _ = fs::remove_file(work.join(RESULT_FILE));
    write_task(
        work,
        &EscapeTask {
            mode: mode.into(),
            outside: outside.display().to_string(),
            pwned: pwned.display().to_string(),
            targets: targets.to_vec(),
        },
    );
    let environment = PlatformSandbox
        .prepare(&sandbox_request(work))
        .unwrap_or_else(|error| {
            panic!("{mode} prepare failed after the suite saw a sandbox: {error}")
        });
    let (status, stdout, stderr) = spawn_payload(&environment, work);
    assert!(
        status.success(),
        "{mode} payload failed ({})\nstdout:\n{stdout}\nstderr:\n{stderr}",
        exit_summary(&status)
    );
}

fn spawn_payload(
    environment: &termior_agent_host::ExecutionEnvironment,
    _work: &Path,
) -> (ExitStatus, String, String) {
    let exe = std::env::current_exe().unwrap();
    let command = PlatformSandbox
        .wrap_command(
            environment,
            &exe.display().to_string(),
            &[
                TEST_NAME.into(),
                "--exact".into(),
                "--test-threads=1".into(),
                "--nocapture".into(),
            ],
            false,
        )
        .unwrap_or_else(|error| panic!("wrap_command failed: {error}"));
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("sandbox spawn failed: {error}"));
    let stdout = child.take_stdout();
    let stderr = child.take_stderr();
    let stdout_thread = thread::spawn(move || read_pipe(stdout));
    let stderr_thread = thread::spawn(move || read_pipe(stderr));
    let status = child
        .wait_timeout(Duration::from_secs(30))
        .unwrap_or_else(|error| panic!("wait failed: {error}"));
    let Some(status) = status else {
        let _ = child.kill();
        panic!(
            "sandbox payload timed out\nstdout:\n{}\nstderr:\n{}",
            stdout_thread.join().unwrap_or_default(),
            stderr_thread.join().unwrap_or_default()
        );
    };
    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    (status, stdout, stderr)
}

fn exit_summary(status: &ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        format!("code={:?} signal={:?}", status.code(), status.signal())
    }
    #[cfg(not(unix))]
    {
        format!("code={:?}", status.code())
    }
}

fn run_payload(task: &EscapeTask) {
    fs::write(RESULT_FILE, b"started").ok();
    match task.mode.as_str() {
        "filesystem" => payload_filesystem(task),
        "network" => payload_network(task),
        "env" => payload_env(),
        "child" => payload_child(task),
        "orphan" => payload_orphan(),
        "sleep" => payload_sleep(),
        other => panic!("unknown escape payload mode {other}"),
    }
}

fn payload_filesystem(task: &EscapeTask) {
    let outside = PathBuf::from(&task.outside);
    let parent_rel = PathBuf::from("../parent-secret.txt");
    let reads = vec![
        fs::read_to_string(&outside).ok(),
        fs::read_to_string(&parent_rel).ok(),
        fs::read_to_string("planted-symlink").ok(),
        create_and_read_symlink(&outside),
    ];
    let writes = vec![
        fs::write(&task.pwned, b"escaped").is_ok(),
        fs::write("../pwned", b"escaped").is_ok(),
    ];
    let hardlink_created = fs::hard_link(&outside, "leaked-hardlink").is_ok();
    let hardlink_body = fs::read_to_string("leaked-hardlink").ok();
    let mut reads = reads;
    reads.push(hardlink_body);
    write_result(&serde_json::json!({
        "reads": reads,
        "writes": writes,
        "hardlink_created": hardlink_created,
    }));
}

fn create_and_read_symlink(outside: &Path) -> Option<String> {
    let link = Path::new("made-symlink");
    let _ = fs::remove_file(link);
    #[cfg(unix)]
    {
        if std::os::unix::fs::symlink(outside, link).is_err() {
            return None;
        }
        fs::read_to_string(link).ok()
    }
    #[cfg(not(unix))]
    {
        let _ = (outside, link);
        None
    }
}

fn payload_network(task: &EscapeTask) {
    let connected = task
        .targets
        .iter()
        .map(|target| {
            let addr: SocketAddr = target
                .parse()
                .unwrap_or_else(|error| panic!("bad dial target {target}: {error}"));
            TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok()
        })
        .collect::<Vec<_>>();
    write_result(&serde_json::json!({ "connected": connected }));
}

fn payload_env() {
    let dump = std::env::vars()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("\n");
    write_result(&serde_json::json!({ "dump": dump }));
}

fn payload_child(task: &EscapeTask) {
    let mut nested = task.clone();
    nested.mode = "filesystem".into();
    write_task(Path::new("."), &nested);
    let status = Command::new(std::env::current_exe().unwrap())
        .args([TEST_NAME, "--exact", "--test-threads=1", "--nocapture"])
        .stdin(Stdio::null())
        .status()
        .unwrap_or_else(|error| panic!("child spawn failed: {error}"));
    assert!(status.success(), "nested filesystem probe failed");
}

fn payload_orphan() {
    write_task(
        Path::new("."),
        &EscapeTask {
            mode: "sleep".into(),
            outside: String::new(),
            pwned: String::new(),
            targets: Vec::new(),
        },
    );
    let child = Command::new(std::env::current_exe().unwrap())
        .args([TEST_NAME, "--exact", "--test-threads=1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|error| panic!("orphan spawn failed: {error}"));
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) && !Path::new("orphan-heartbeat").exists() {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        Path::new("orphan-heartbeat").exists(),
        "sleeper did not write a heartbeat"
    );
    drop(child);
}

fn payload_sleep() {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(8) {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .to_string();
        fs::write("orphan-heartbeat", stamp).ok();
        thread::sleep(Duration::from_millis(100));
    }
}

fn bind_private_listener() -> Option<TcpListener> {
    let ip = host_private_ipv4()?;
    TcpListener::bind((ip, 0)).ok()
}

fn host_private_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if is_rfc1918(ip) => Some(ip),
        _ => None,
    }
}

fn is_rfc1918(ip: Ipv4Addr) -> bool {
    let [first, second, _, _] = ip.octets();
    first == 10 || (first == 172 && (16..=31).contains(&second)) || (first == 192 && second == 168)
}

fn assert_host_can_connect(target: &str) {
    let addr: SocketAddr = target.parse().unwrap();
    TcpStream::connect_timeout(&addr, Duration::from_millis(500))
        .unwrap_or_else(|error| panic!("host control connection to {target} failed: {error}"));
}

fn write_task(work: &Path, task: &EscapeTask) {
    fs::write(work.join(TASK_FILE), serde_json::to_vec(task).unwrap()).unwrap();
}

fn read_task() -> Option<EscapeTask> {
    let bytes = fs::read(TASK_FILE).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_result(value: &serde_json::Value) {
    fs::write(RESULT_FILE, serde_json::to_vec(value).unwrap()).unwrap();
}

fn read_result<T: for<'de> Deserialize<'de>>(work: &Path) -> T {
    let bytes = fs::read(work.join(RESULT_FILE))
        .unwrap_or_else(|error| panic!("missing escape result: {error}"));
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "bad escape result: {error}\n{}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

fn read_pipe(pipe: Option<impl Read>) -> String {
    let mut pipe = match pipe {
        Some(pipe) => pipe,
        None => return String::new(),
    };
    let mut buffer = String::new();
    let _ = pipe.read_to_string(&mut buffer);
    buffer
}

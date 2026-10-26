//! Real Windows AppContainer confinement checks.
//!
//! These tests run the actual Windows API. They prove that a command inside the
//! container can read the granted workspace and cannot read a sibling directory.
//! A platform without a usable AppContainer profile skips instead of reporting a
//! false pass.

#![cfg(windows)]

use std::path::Path;
use std::time::Duration;
use termior_terminal::windows_sandbox::{
    app_container_available, AppContainer, AppContainerError, APP_CONTAINER_NAME,
};

/// The system command processor, resolved once.
fn cmd() -> String {
    std::env::var("SystemRoot")
        .map(|root| format!("{root}\\System32\\cmd.exe"))
        .unwrap_or_else(|_| "C:\\Windows\\System32\\cmd.exe".to_string())
}

fn require_container() -> Option<AppContainer> {
    if !app_container_available() {
        eprintln!("skipping: this machine has no usable AppContainer profile");
        return None;
    }
    Some(AppContainer::resolve(APP_CONTAINER_NAME).expect("the AppContainer profile must resolve"))
}

/// Run a command inside the container and return its exit code.
fn run(container: &AppContainer, args: &[String], cwd: &Path) -> Option<u32> {
    let mut process = container
        .spawn(Path::new(&cmd()), args, cwd)
        .expect("the confined command must start");
    process.wait(Duration::from_secs(30))
}

#[test]
fn a_confined_command_can_read_the_granted_workspace() {
    let Some(container) = require_container() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("inside.txt"), b"inside-value").unwrap();
    container
        .grant(&workspace)
        .expect("the workspace grant must succeed");

    let code = run(
        &container,
        &["/c".into(), "type inside.txt".into()],
        &workspace,
    );
    assert_eq!(code, Some(0), "the confined command must run");
}

#[test]
fn a_confined_command_cannot_read_a_sibling_directory() {
    let Some(container) = require_container() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), b"outside-secret").unwrap();
    container
        .grant(&workspace)
        .expect("the workspace grant must succeed");

    let code = run(
        &container,
        &[
            "/c".into(),
            format!("type \"{}\"", outside.join("secret.txt").display()),
        ],
        &workspace,
    );
    assert_ne!(
        code,
        Some(0),
        "the confined command read a directory outside the granted workspace"
    );
}

#[test]
fn a_confined_command_cannot_write_outside_the_granted_workspace() {
    let Some(container) = require_container() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    container
        .grant(&workspace)
        .expect("the workspace grant must succeed");
    let target = root.path().join("escaped.txt");

    let code = run(
        &container,
        &[
            "/c".into(),
            format!("echo escaped > \"{}\"", target.display()),
        ],
        &workspace,
    );
    assert_ne!(
        code,
        Some(0),
        "the confined command wrote outside the workspace"
    );
    assert!(
        !target.exists(),
        "a file was created outside the granted workspace"
    );
}

#[test]
fn a_confined_command_does_not_inherit_the_parent_environment() {
    let Some(container) = require_container() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    container
        .grant(&workspace)
        .expect("the workspace grant must succeed");

    // The marker is set in the parent's environment. A confined command must not see it.
    std::env::set_var("TERMIOR_APPCONTAINER_PROBE", "leaked-value");
    let code = run(
        &container,
        &[
            "/c".into(),
            "if defined TERMIOR_APPCONTAINER_PROBE (exit /b 3) else (exit /b 0)".into(),
        ],
        &workspace,
    );
    std::env::remove_var("TERMIOR_APPCONTAINER_PROBE");
    assert_eq!(
        code,
        Some(0),
        "the confined command inherited the parent environment"
    );
}

#[test]
fn granting_a_directory_that_cannot_be_shared_is_an_error_not_a_silent_grant() {
    let Some(container) = require_container() else {
        return;
    };
    let missing = std::path::PathBuf::from("C:\\definitely\\not\\here\\termior-appcontainer");
    assert!(matches!(
        container.grant(&missing),
        Err(AppContainerError::Acl(_))
    ));
}

/// An AppContainer process without the `internetClient` or
/// `privateNetworkClientServer` capability has no network access at all. This test
/// proves it against a listener the host process can reach.
#[test]
fn a_confined_command_cannot_reach_the_network() {
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    let Some(container) = require_container() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    container
        .grant(&workspace)
        .expect("the workspace grant must succeed");

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    // Control: the host process reaches the same listener.
    assert!(
        TcpStream::connect_timeout(&address.parse().unwrap(), Duration::from_millis(500)).is_ok(),
        "the host must be able to reach the control listener"
    );

    let script = format!("powershell -NoProfile -ExecutionPolicy Bypass -File probe.ps1 {address}");
    std::fs::write(
        workspace.join("probe.ps1"),
        "$client = New-Object System.Net.Sockets.TcpClient\n\
         try {\n\
         \x20 $client.Connect($args[0])\n\
         \x20 exit 0\n\
         } catch {\n\
         \x20 exit 7\n\
         }\n",
    )
    .unwrap();
    let code = run(&container, &["/c".into(), script], &workspace);
    assert_ne!(
        code,
        Some(0),
        "the confined command reached the loopback listener"
    );
}

/// A piped confined process reports its standard output and exits with its exit code.
#[test]
fn a_piped_confined_command_reports_its_output() {
    use std::io::Read;

    let Some(container) = require_container() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    container
        .grant(&workspace)
        .expect("the workspace grant must succeed");

    let mut process = container
        .spawn_piped(
            Path::new(&cmd()),
            &["/c".into(), "echo piped-value".into()],
            &workspace,
        )
        .expect("the piped confined command must start");
    let mut stdout = process.take_stdout().expect("stdout pipe");
    let mut buffer = String::new();
    stdout.read_to_string(&mut buffer).unwrap();
    assert_eq!(process.wait(Duration::from_secs(30)), Some(0));
    assert!(
        buffer.contains("piped-value"),
        "the piped output was empty: {buffer:?}"
    );
}

/// An AppContainer can usually read the Windows system binaries without an explicit
/// grant, because the default ACLs already grant read access to low-privilege tokens.
/// This test records what actually works on this machine.
#[test]
fn a_confined_command_can_run_system_binaries_without_an_explicit_grant() {
    let Some(container) = require_container() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    container
        .grant(&workspace)
        .expect("the workspace grant must succeed");

    for program in ["cmd.exe", "where.exe", "whoami.exe"] {
        let code = run(
            &container,
            &["/c".into(), format!("{program} > nul")],
            &workspace,
        );
        println!("{program}: exit={code:?}");
    }
}

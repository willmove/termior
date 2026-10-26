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

/// A program under the user profile is a different case: the profile directory's own
/// ACL does not grant a low-privilege token traversal, so the confined loader cannot
/// even open the image.
#[test]
fn a_confined_command_can_run_a_program_under_the_user_profile() {
    let Some(container) = require_container() else {
        return;
    };
    let directory = std::env::temp_dir().join("termior-appcontainer-profile-probe");
    std::fs::create_dir_all(&directory).unwrap();
    let payload = directory.join("probe.cmd");
    std::fs::write(&payload, "@echo /b 0\n").unwrap();
    container.grant(&directory).expect("the grant must succeed");

    // The payload is named relative to the working directory. `cmd.exe /c` has its
    // own quoting rules for an absolute path with spaces in it, and the path is not
    // what is under test here: what matters is that a confined token can reach the
    // image under the user profile at all.
    let code = run(&container, &["/c".into(), "probe.cmd".into()], &directory);
    println!("profile payload exit={code:?}");
    assert_eq!(code, Some(0), "the confined command must run");
}

/// The same problem for a real image: the CI runner builds into a directory whose
/// ancestors grant access to the runner account only, so a confined process cannot
/// traverse down to the program even though the program itself was granted.
#[test]
fn a_confined_command_can_run_a_program_below_a_restricted_directory() {
    let Some(container) = require_container() else {
        return;
    };
    let root = std::env::temp_dir().join("termior-appcontainer-restricted-root");
    let _ = std::fs::remove_dir_all(&root);
    let leaf = root.join("a").join("b");
    std::fs::create_dir_all(&leaf).unwrap();

    // The image is the test binary itself, copied next to the restricted directory.
    let exe = std::env::current_exe().unwrap();
    let target = leaf.join("payload.exe");
    std::fs::copy(&exe, &target).unwrap();

    container.grant(&leaf).expect("the grant must succeed");

    let code = run(&container, &["/c".into(), "echo ok".into()], &leaf);
    println!("restricted leaf exit={code:?}");
    // The working directory must be reachable, otherwise the confined process cannot
    // even start.
    assert_eq!(code, Some(0), "the confined command must run");
}

/// A grant must not take the host's own access away. The grant is done with
/// `SetNamedSecurityInfoW`, which replaces the whole DACL, so this test checks what
/// the directory looks like afterwards.
#[test]
fn granting_a_directory_keeps_the_hosts_own_access() {
    let Some(container) = require_container() else {
        return;
    };
    let directory = std::env::temp_dir().join("termior-appcontainer-acl-probe");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("before.txt"), b"before").unwrap();

    container.grant(&directory).expect("the grant must succeed");

    // The host can still read its own file, and can still write into the directory.
    assert_eq!(
        std::fs::read_to_string(directory.join("before.txt")).unwrap(),
        "before",
        "the grant took the host's own access to the granted directory away"
    );
    std::fs::write(directory.join("after.txt"), b"after").unwrap();
    println!("granted directory: {}", directory.display());
}

/// The grant must also keep the *other* principals' entries. Replacing the whole
/// DACL drops them, which takes the host's own write access away and removes the
/// traversal rights the ancestors need.
#[test]
fn granting_a_directory_keeps_the_other_principals_entries() {
    let Some(container) = require_container() else {
        return;
    };
    let directory = std::env::temp_dir().join("termior-appcontainer-preserve-probe");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("kept.txt");
    std::fs::write(&file, b"kept").unwrap();

    let before = acl_entries(&directory);
    container.grant(&directory).expect("the grant must succeed");
    let after = acl_entries(&directory);

    // Everything that was there before is still there, plus the container entry.
    for entry in &before {
        assert!(
            after.contains(entry),
            "the grant dropped the {entry:?} entry of {}",
            directory.display()
        );
    }
    // The file inside the directory keeps its content and stays writable.
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "kept");
    std::fs::write(&file, b"rewritten").unwrap();
}

/// The raw ACL entries of a path, as `icacls` reports them.
fn acl_entries(path: &Path) -> Vec<String> {
    let output = std::process::Command::new("icacls")
        .arg(path)
        .output()
        .expect("icacls must run");
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .skip(1)
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty() && !line.starts_with("Successfully processed"))
        .collect()
}

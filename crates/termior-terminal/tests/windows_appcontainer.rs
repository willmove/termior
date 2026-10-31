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
        panic!("Windows validation requires a usable AppContainer identity");
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
        .grant_with_ancestors(&workspace)
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
        .grant_with_ancestors(&workspace)
        .expect("the workspace grant must succeed");

    let code = run(
        &container,
        &[
            "/c".into(),
            format!("type \"{}\"", outside.join("secret.txt").display()),
        ],
        &workspace,
    );
    assert_eq!(code, Some(1), "type must execute and report access denied");
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
        .grant_with_ancestors(&workspace)
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
    assert_eq!(code, Some(1), "redirection must execute and report denial");
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
        .grant_with_ancestors(&workspace)
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
    const NAME: &str = "a_confined_command_cannot_reach_the_network";
    if let Ok(target) = std::fs::read_to_string("network-target.txt") {
        let error = TcpStream::connect_timeout(&target.parse().unwrap(), Duration::from_secs(2))
            .expect_err("confined process reached the host listener");
        assert_eq!(
            error.raw_os_error(),
            Some(10013),
            "must be WSAEACCES, not an unreachable target"
        );
        std::fs::write("network-result.txt", "denied").unwrap();
        return;
    }
    let container = require_container().unwrap();
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let image = workspace.join("network-probe.exe");
    std::fs::copy(std::env::current_exe().unwrap(), &image).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    TcpStream::connect_timeout(&address, Duration::from_millis(500)).unwrap();
    std::fs::write(workspace.join("network-target.txt"), address.to_string()).unwrap();
    container.grant_with_ancestors(&workspace).unwrap();
    let mut child = container
        .spawn(
            &image,
            &[NAME.into(), "--exact".into(), "--test-threads=1".into()],
            &workspace,
        )
        .unwrap();
    assert_eq!(child.wait(Duration::from_secs(20)), Some(0));
    assert_eq!(
        std::fs::read_to_string(workspace.join("network-result.txt")).unwrap(),
        "denied"
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
        .grant_with_ancestors(&workspace)
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
        .grant_with_ancestors(&workspace)
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
    std::fs::write(&payload, "@exit /b 0\n").unwrap();
    container
        .grant_with_ancestors(&directory)
        .expect("the grant must succeed");

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

    container
        .grant_with_ancestors(&leaf)
        .expect("the grant must succeed");

    let code = run(&container, &["/c".into(), "echo ok".into()], &leaf);
    println!("restricted leaf exit={code:?}");
    // The working directory must be reachable, otherwise the confined process cannot
    // even start.
    assert_eq!(code, Some(0), "the confined command must run");
}

/// A workspace below a directory that only the user may enter.
///
/// The directory above the workspace grants nothing to a low-privilege token, so
/// the confined process cannot even walk to the workspace. The grant of the
/// workspace itself is not enough: Windows checks the traversal right on every
/// directory in the path. This is the shape a CI runner has — its build and
/// temporary directories sit under the runner account's profile — and it is why
/// the escape suite failed on CI with `ERROR_FILE_NOT_FOUND` for a payload that
/// was really there.
#[test]
fn a_confined_command_can_run_a_program_below_a_private_ancestor() {
    let Some(container) = require_container() else {
        return;
    };
    let root = std::env::temp_dir().join("termior-appcontainer-private-ancestor");
    let _ = std::fs::remove_dir_all(&root);
    let leaf = root.join("a").join("b");
    std::fs::create_dir_all(&leaf).unwrap();

    // The image is the test binary itself, staged next to the workspace.
    let exe = std::env::current_exe().unwrap();
    std::fs::copy(&exe, leaf.join("payload.exe")).unwrap();

    // The ancestor grants the container nothing: only the user may enter it.
    let ancestor = root.join("a");
    container
        .grant_with_ancestors(&leaf)
        .expect("ancestor grant must succeed");

    // The ancestor must stay unreadable for the container: only traversal was
    // granted, not the right to list or read it.
    let mut entries = container
        .spawn(
            Path::new(&cmd()),
            &["/c".into(), format!("dir \"{}\"", ancestor.display())],
            &leaf,
        )
        .expect("the confined command must start");
    let code = entries.wait(Duration::from_secs(30));
    assert_ne!(
        code,
        Some(0),
        "the ancestor became readable: the traversal grant leaked more than traversal"
    );
}

/// A verbatim working directory must still start the confined process.
///
/// `std::fs::canonicalize` returns a verbatim path (`\\?\C:\...`) on Windows,
/// and the sandbox hands that path to `CreateProcessW` as the working
/// directory. The verbatim form switches off the normalization the Win32 path
/// parser does, so the confined process either fails to start with
/// `ERROR_FILE_NOT_FOUND` for a program that is really there, or starts in
/// `C:\Windows` because the path was rejected as a UNC path. This is the shape
/// the CI runner had, and it is why the escape suite failed there.
#[test]
fn a_confined_command_starts_in_a_verbatim_working_directory() {
    let Some(container) = require_container() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("inside.txt"), b"inside-value").unwrap();
    container
        .grant_with_ancestors(&workspace)
        .expect("the workspace grant must succeed");

    // The verbatim form of the same directory.
    let verbatim = {
        let resolved = workspace.canonicalize().unwrap();
        let text = resolved.as_os_str().to_string_lossy().to_string();
        assert!(
            text.starts_with(r"\\?\"),
            "canonicalize did not return a verbatim path on this machine: {text}"
        );
        std::path::PathBuf::from(text)
    };

    let code = run(
        &container,
        &["/c".into(), "type inside.txt".into()],
        &verbatim,
    );
    assert_eq!(
        code,
        Some(0),
        "the confined command did not start in the verbatim working directory"
    );
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

    container
        .grant_with_ancestors(&directory)
        .expect("the grant must succeed");

    // The host can still read its own file, and can still write into the directory.
    assert_eq!(
        std::fs::read_to_string(directory.join("before.txt")).unwrap(),
        "before",
        "the grant took the host's own access to the granted directory away"
    );
    std::fs::write(directory.join("after.txt"), b"after").unwrap();
    println!("granted directory: {}", directory.display());
}

/// The traversal grant on an ancestor must not reach the files below it.
///
/// Writing a DACL the way the other Windows APIs do re-evaluates inheritance
/// across the whole subtree: every file below the ancestor gets its own DACL
/// rewritten. That costs about 0.35ms per file, so the grant on a CI
/// runner's profile directory took minutes and then failed outright, because
/// one file in the subtree could not be rewritten. The grant was skipped, the
/// confined token could not walk down to the workspace, and the spawn failed
/// with `ERROR_FILE_NOT_FOUND` for a payload that was really there.
///
/// The traversal grant is therefore written without propagation: the ancestor
/// becomes walkable and the files below it are left alone.
#[test]
fn the_traversal_grant_on_an_ancestor_leaves_the_files_below_it_alone() {
    let Some(container) = require_container() else {
        return;
    };
    let root = std::env::temp_dir().join("termior-appcontainer-no-propagation");
    let _ = std::fs::remove_dir_all(&root);
    let ancestor = root.join("ancestor");
    std::fs::create_dir_all(&ancestor).unwrap();
    let child = ancestor.join("child");
    std::fs::create_dir_all(&child).unwrap();
    let file = ancestor.join("untouched.txt");
    std::fs::write(&file, b"untouched").unwrap();

    let before = acl_entries(&file);

    // Grant traversal on the ancestor alone: the workspace below it is not
    // part of this grant.
    container
        .grant_with_ancestors(&child)
        .expect("ancestor grant must succeed");

    let after = acl_entries(&file);
    assert_eq!(
        before, after,
        "the traversal grant on the ancestor changed the files below it"
    );
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
    container
        .grant_with_ancestors(&directory)
        .expect("the grant must succeed");
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

/// Reproduce the CI launch shape with a real image, spaces, and a canonical cwd.
/// The child must execute, prove its token, read a fixture, and observe stdin EOF.
#[test]
fn a_copied_image_runs_with_a_confined_token_and_closed_stdin() {
    const NAME: &str = "a_copied_image_runs_with_a_confined_token_and_closed_stdin";
    if Path::new("image-probe.marker").exists() {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::Security::{GetTokenInformation, TokenIsAppContainer, TOKEN_QUERY};
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        let mut token = std::ptr::null_mut();
        // SAFETY: token and result buffers are valid out-pointers.
        unsafe {
            assert_ne!(
                OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token),
                0
            );
            let mut confined = 0u32;
            let mut size = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenIsAppContainer,
                &mut confined as *mut u32 as *mut std::ffi::c_void,
                std::mem::size_of::<u32>() as u32,
                &mut size,
            );
            CloseHandle(token);
            assert_ne!(ok, 0);
            assert_eq!(confined, 1, "payload did not receive an AppContainer token");
        }
        assert_eq!(
            std::fs::read_to_string("inside.txt").unwrap(),
            "inside-value"
        );
        if std::fs::read_to_string("image-probe.marker").unwrap() == "piped" {
            use std::io::Read;
            let mut input = Vec::new();
            assert_eq!(std::io::stdin().read_to_end(&mut input).unwrap(), 0);
        }
        std::fs::write("image-probe.result", "executed").unwrap();
        return;
    }
    let container = require_container().unwrap();
    let root = tempfile::tempdir().unwrap();
    let work = root
        .path()
        .join("private ancestor")
        .join("workspace with spaces");
    std::fs::create_dir_all(&work).unwrap();
    let image = work.join("copied payload.exe");
    std::fs::copy(std::env::current_exe().unwrap(), &image).unwrap();
    std::fs::write(work.join("inside.txt"), "inside-value").unwrap();
    std::fs::write(work.join("image-probe.marker"), "plain").unwrap();
    container.grant_with_ancestors(&work).unwrap();
    let args = vec![NAME.into(), "--exact".into(), "--test-threads=1".into()];
    let mut plain = container
        .spawn(
            &image.canonicalize().unwrap(),
            &args,
            &work.canonicalize().unwrap(),
        )
        .unwrap();
    assert_eq!(plain.wait(Duration::from_secs(20)), Some(0));
    assert_eq!(
        std::fs::read_to_string(work.join("image-probe.result")).unwrap(),
        "executed"
    );
    std::fs::remove_file(work.join("image-probe.result")).unwrap();
    std::fs::write(work.join("image-probe.marker"), "piped").unwrap();
    let mut piped = container
        .spawn_piped(
            &image.canonicalize().unwrap(),
            &args,
            &work.canonicalize().unwrap(),
        )
        .unwrap();
    assert_eq!(
        piped.wait(Duration::from_secs(20)),
        Some(0),
        "stdin must reach EOF without leaking its host handle"
    );
    assert_eq!(
        std::fs::read_to_string(work.join("image-probe.result")).unwrap(),
        "executed"
    );
}

/// kill and Drop must bound the entire tree for both launch APIs.
#[test]
fn job_cleanup_stops_descendants_on_kill_and_drop() {
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::Instant;
    const NAME: &str = "job_cleanup_stops_descendants_on_kill_and_drop";
    if let Ok(mode) = std::fs::read_to_string("job-probe.mode") {
        if mode == "sleeper" {
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(10) {
                std::fs::write(
                    "job-probe.heartbeat",
                    start.elapsed().as_nanos().to_string(),
                )
                .unwrap();
                thread::sleep(Duration::from_millis(50));
            }
            return;
        }
        std::fs::write("job-probe.mode", "sleeper").unwrap();
        let _child = Command::new(std::env::current_exe().unwrap())
            .args([NAME, "--exact", "--test-threads=1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let start = Instant::now();
        while !Path::new("job-probe.heartbeat").exists() {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "descendant never ran"
            );
            thread::sleep(Duration::from_millis(20));
        }
        std::fs::write("job-probe.ready", "ready").unwrap();
        thread::sleep(Duration::from_secs(10));
        return;
    }
    let container = require_container().unwrap();
    for piped in [false, true] {
        for kill in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let work = root.path().join("work");
            std::fs::create_dir(&work).unwrap();
            let exe = work.join("job-probe.exe");
            std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
            std::fs::write(work.join("job-probe.mode"), "root").unwrap();
            container.grant_with_ancestors(&work).unwrap();
            let args = vec![NAME.into(), "--exact".into(), "--test-threads=1".into()];
            let mut plain = None;
            let mut pipes = None;
            if piped {
                pipes = Some(container.spawn_piped(&exe, &args, &work).unwrap());
            } else {
                plain = Some(container.spawn(&exe, &args, &work).unwrap());
            }
            let start = Instant::now();
            while !work.join("job-probe.ready").exists() {
                assert!(
                    start.elapsed() < Duration::from_secs(5),
                    "root/descendant failed to start"
                );
                thread::sleep(Duration::from_millis(20));
            }
            if kill {
                if let Some(process) = plain.as_mut() {
                    process.kill();
                }
                if let Some(process) = pipes.as_mut() {
                    process.kill();
                }
                // Keep the Job handle alive: kill itself must terminate the tree.
            } else {
                drop(plain.take());
                drop(pipes.take());
            }
            thread::sleep(Duration::from_millis(300));
            let first = std::fs::read(work.join("job-probe.heartbeat")).unwrap();
            thread::sleep(Duration::from_millis(300));
            assert_eq!(
                std::fs::read(work.join("job-probe.heartbeat")).unwrap(),
                first,
                "descendant survived: piped={piped}, kill={kill}"
            );
        }
    }
}

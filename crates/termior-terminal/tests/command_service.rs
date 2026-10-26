use std::time::Duration;

#[cfg(windows)]
#[test]
fn owner_process_crash_does_not_leave_a_background_command_alive() {
    const ROOT_ENV: &str = "TERMIOR_COMMAND_CRASH_ROOT";
    if let Some(root) = std::env::var_os(ROOT_ENV) {
        let service = LocalTerminalService::new(64 * 1024);
        let mut launch = request("pwsh -NoProfile -File background.ps1");
        launch.shell = "pwsh".into();
        launch.project_dir = std::path::PathBuf::from(&root).display().to_string();
        launch.cwd = launch.project_dir.clone();
        service.create(launch).unwrap();
        loop {
            std::thread::park();
        }
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("background.ps1"),
        "Set-Content background.pid $PID\nStart-Sleep -Seconds 60\n",
    )
    .unwrap();
    let mut owner = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "owner_process_crash_does_not_leave_a_background_command_alive",
        ])
        .env(ROOT_ENV, dir.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let ready_deadline = std::time::Instant::now() + Duration::from_secs(10);
    let child_pid = loop {
        if let Some(pid) = std::fs::read_to_string(dir.path().join("background.pid"))
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
        {
            break pid;
        }
        if std::time::Instant::now() >= ready_deadline {
            let _ = owner.kill();
            let _ = owner.wait();
            panic!("background command did not publish its PID");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    owner.kill().unwrap();
    owner.wait().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let survived = loop {
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {child_pid}"), "/NH"])
            .output()
            .unwrap();
        let alive = String::from_utf8_lossy(&output.stdout).contains(&child_pid.to_string());
        if !alive {
            break false;
        }
        if std::time::Instant::now() >= deadline {
            break true;
        }
    };
    if survived {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &child_pid.to_string(), "/T", "/F"])
            .output();
    }
    assert!(
        !survived,
        "background process {child_pid} survived a real owner-process crash"
    );
}

use termior_terminal::PtySessionConfig;
use termior_terminal::{
    CommandCreate, CommandOwner, CommandState, Controller, LocalTerminalService, TerminalBridge,
    TerminalService, TerminalServiceError,
};

fn request(command: &str) -> CommandCreate {
    CommandCreate {
        owner: CommandOwner::Task("task-local".into()),
        project_dir: std::env::current_dir().unwrap().display().to_string(),
        environment_id: "direct".into(),
        cwd: std::env::current_dir().unwrap().display().to_string(),
        shell: if cfg!(windows) { "cmd" } else { "sh" }.into(),
        command: command.into(),
        interactive: false,
        rows: 24,
        cols: 80,
    }
}

#[test]
fn real_command_retains_output_and_exit_evidence() {
    let service = LocalTerminalService::new(64 * 1024);
    let id = service.create(request("echo stage-b-output")).unwrap();
    let done = service.wait(&id, Duration::from_secs(5)).unwrap();
    assert_eq!(done.state, CommandState::Exited);
    assert_eq!(done.exit_code, Some(0));

    let output = service.read_output(&id, None, 64 * 1024).unwrap();
    assert!(String::from_utf8_lossy(&output.bytes()).contains("stage-b-output"));
}

#[test]
fn wait_timeout_keeps_real_process_alive_until_killed() {
    let service = LocalTerminalService::new(64 * 1024);
    let command = if cfg!(windows) {
        "ping -n 6 127.0.0.1 >NUL"
    } else {
        "sleep 5"
    };
    let id = service.create(request(command)).unwrap();
    let waiting = service.wait(&id, Duration::from_millis(20)).unwrap();
    assert!(waiting.timed_out);
    assert_eq!(waiting.state, CommandState::Running);

    service.kill(&id).unwrap();
    let stopped = service.wait(&id, Duration::from_secs(2)).unwrap();
    assert!(matches!(
        stopped.state,
        CommandState::Terminated | CommandState::Exited
    ));
}

#[test]
fn background_command_returns_a_session_instead_of_a_pid() {
    let service = LocalTerminalService::new(64 * 1024);
    let id = service.create(request("echo background")).unwrap();
    assert!(id.0.starts_with("cmd-"));
    assert!(!id.0.chars().all(|character| character.is_ascii_digit()));
    service.wait(&id, Duration::from_secs(5)).unwrap();
    assert!(service.session(&id).unwrap().exit_code.is_some());
}

#[test]
fn interactive_pty_enforces_takeover_and_supports_resize() {
    let service = LocalTerminalService::new(64 * 1024);
    let mut interactive = request(if cfg!(windows) { "more" } else { "cat" });
    interactive.interactive = true;
    let id = service.create(interactive).unwrap();
    service.resize(&id, 40, 120).unwrap();
    service.take_over(&id).unwrap();
    let error = service
        .write_stdin(&id, &Controller::Task("task-local".into()), b"blocked\n")
        .unwrap_err();
    assert!(matches!(
        error,
        TerminalServiceError::ControllerMismatch { .. }
    ));
    service
        .write_stdin(&id, &Controller::User, b"user-input\n")
        .unwrap();
    service
        .hand_back(&id, Controller::Task("task-local".into()))
        .unwrap();
    service.kill(&id).unwrap();
}

#[test]
fn registered_pane_shares_stable_identity_output_and_controller_state() {
    let service = LocalTerminalService::new(64 * 1024);
    let mut bridge = TerminalBridge::spawn(&PtySessionConfig::default()).unwrap();
    let project_dir = std::env::current_dir().unwrap().display().to_string();
    let id = service
        .register_pane(
            CommandCreate {
                owner: CommandOwner::User,
                project_dir: project_dir.clone(),
                environment_id: "pane-test".into(),
                cwd: project_dir,
                shell: "test-shell".into(),
                command: String::new(),
                interactive: true,
                rows: 24,
                cols: 80,
            },
            bridge.writer(),
        )
        .unwrap();

    assert!(service.session_ids().contains(&id));
    service
        .observe_pane_output(&id, b"visible-pane-output".to_vec())
        .unwrap();
    assert!(
        String::from_utf8_lossy(&service.read_output(&id, None, 1024).unwrap().bytes())
            .contains("visible-pane-output")
    );

    service
        .hand_back(&id, Controller::Task("task-pane".into()))
        .unwrap();
    assert!(matches!(
        service.write_stdin(&id, &Controller::User, b"blocked"),
        Err(TerminalServiceError::ControllerMismatch { .. })
    ));
    service.take_over(&id).unwrap();
    service
        .write_stdin(&id, &Controller::User, b"echo pane-control\n")
        .unwrap();

    service.observe_pane_exit(&id, Some(0)).unwrap();
    let session = service.session(&id).unwrap();
    assert_eq!(session.state, CommandState::Exited);
    assert_eq!(session.exit_code, Some(0));
    service.release(&id, false).unwrap();
    let _ = bridge.kill();
}

#[test]
fn kill_terminates_a_spawned_child_process_tree() {
    let dir = tempfile::tempdir().unwrap();
    let command = if cfg!(windows) {
        "$p=Start-Process ping -ArgumentList '-t','127.0.0.1' -WindowStyle Hidden -PassThru; Set-Content -LiteralPath child.pid -Value $p.Id; Wait-Process -Id $p.Id"
    } else {
        "sleep 30 & echo $! > child.pid; wait"
    };
    let mut launch = request(command);
    if cfg!(windows) {
        launch.shell = "pwsh".into();
    }
    launch.project_dir = dir.path().display().to_string();
    launch.cwd = dir.path().display().to_string();
    let service = LocalTerminalService::new(64 * 1024);
    let id = service.create(launch).unwrap();
    let pid_path = dir.path().join("child.pid");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    // File creation precedes the PID write; wait for a complete value rather
    // than treating existence as proof the child is ready.
    let child_pid = loop {
        if let Some(pid) = std::fs::read_to_string(&pid_path)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
        {
            break pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "child PID was not published"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    service.kill(&id).unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let alive = if cfg!(windows) {
            let output = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {child_pid}"), "/NH"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout).contains(&child_pid.to_string())
        } else {
            std::process::Command::new("kill")
                .args(["-0", &child_pid.to_string()])
                .status()
                .is_ok_and(|status| status.success())
        };
        if !alive {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "child process {child_pid} survived command-session kill"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

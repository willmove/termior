use std::time::Duration;

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
        "$p=Start-Process ping -ArgumentList '-t','127.0.0.1' -PassThru; Set-Content -LiteralPath child.pid -Value $p.Id; Wait-Process -Id $p.Id"
    } else {
        "sleep 30 & echo $! > child.pid; wait"
    };
    let mut launch = request(command);
    if cfg!(windows) {
        launch.shell = "powershell".into();
    }
    launch.project_dir = dir.path().display().to_string();
    launch.cwd = dir.path().display().to_string();
    let service = LocalTerminalService::new(64 * 1024);
    let id = service.create(launch).unwrap();
    let pid_path = dir.path().join("child.pid");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !pid_path.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let child_pid = std::fs::read_to_string(pid_path)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
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

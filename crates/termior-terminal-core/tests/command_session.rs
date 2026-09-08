use std::time::Duration;

use termior_terminal_core::command::{
    CommandCapabilities, CommandCreate, CommandOwner, CommandSessionId, CommandState, Controller,
    FakeTerminalService, OutputStream, TerminalService, TerminalServiceError,
};

fn command() -> CommandCreate {
    CommandCreate {
        owner: CommandOwner::Task("task-7".into()),
        project_dir: "/workspace/project".into(),
        environment_id: "direct".into(),
        cwd: "/workspace/project".into(),
        shell: "sh".into(),
        command: "build".into(),
        interactive: true,
        rows: 24,
        cols: 80,
    }
}

#[test]
fn output_cursors_are_incremental_and_report_truncation() {
    let service = FakeTerminalService::with_output_budget(7);
    let id = service.create(command()).unwrap();

    service
        .push_output(&id, OutputStream::Stdout, b"first".to_vec())
        .unwrap();
    let first = service.read_output(&id, None, 1024).unwrap();
    assert_eq!(first.bytes(), b"first");
    assert_eq!(first.next.sequence, 1);

    service
        .push_output(&id, OutputStream::Stderr, b"second".to_vec())
        .unwrap();
    let incremental = service.read_output(&id, Some(first.next), 1024).unwrap();
    assert_eq!(incremental.bytes(), b"second");
    assert_eq!(incremental.next.sequence, 2);

    let slow = service
        .read_output(
            &id,
            Some(termior_terminal_core::command::OutputCursor::START),
            1024,
        )
        .unwrap();
    assert_eq!(slow.truncated_before.map(|cursor| cursor.sequence), Some(1));
    assert_eq!(slow.bytes(), b"second");
}

#[test]
fn wait_timeout_does_not_kill_the_command() {
    let service = FakeTerminalService::default();
    let id = service.create(command()).unwrap();

    let result = service.wait(&id, Duration::from_millis(1)).unwrap();
    assert!(result.timed_out);
    assert_eq!(result.state, CommandState::Running);

    service.finish(&id, 0).unwrap();
    let result = service.wait(&id, Duration::from_millis(1)).unwrap();
    assert!(!result.timed_out);
    assert_eq!(result.state, CommandState::Exited);
    assert_eq!(result.exit_code, Some(0));
}

#[test]
fn user_takeover_is_enforced_at_the_writer_boundary() {
    let service = FakeTerminalService::default();
    let id = service.create(command()).unwrap();
    service
        .hand_back(&id, Controller::Task("task-7".into()))
        .unwrap();
    service
        .write_stdin(&id, &Controller::Task("task-7".into()), b"1 + 1\n")
        .unwrap();

    service.take_over(&id).unwrap();
    let error = service
        .write_stdin(&id, &Controller::Task("task-7".into()), b"2 + 2\n")
        .unwrap_err();
    assert!(matches!(
        error,
        TerminalServiceError::ControllerMismatch { .. }
    ));
    service
        .write_stdin(&id, &Controller::User, b"2 + 2\n")
        .unwrap();
}

#[test]
fn running_session_cannot_be_released_without_detach() {
    let service = FakeTerminalService::default();
    let id = service.create(command()).unwrap();
    assert_eq!(
        service.release(&id, false).unwrap_err(),
        TerminalServiceError::RunningReleaseDenied
    );
    service.release(&id, true).unwrap();
    assert_eq!(
        service.session(&id).unwrap_err(),
        TerminalServiceError::NotFound(id)
    );
}

#[test]
fn capabilities_reject_unsupported_operations() {
    let service = FakeTerminalService::default();
    let mut create = command();
    create.interactive = false;
    let id = service
        .create_with_capabilities(
            create,
            CommandCapabilities {
                stdin: false,
                resize: false,
                kill: true,
                detach: true,
            },
        )
        .unwrap();

    assert!(matches!(
        service.resize(&id, 40, 120),
        Err(TerminalServiceError::Unsupported("resize"))
    ));
}

#[test]
fn command_ids_are_stable_and_never_reused() {
    let service = FakeTerminalService::default();
    let first = service.create(command()).unwrap();
    service.kill(&first).unwrap();
    service.release(&first, false).unwrap();
    let second = service.create(command()).unwrap();

    assert_ne!(first, second);
    assert!(first.0.starts_with("cmd-"));
    assert_eq!(
        service.session(&first).unwrap_err(),
        TerminalServiceError::NotFound(first)
    );
    assert_ne!(second, CommandSessionId(String::new()));
}

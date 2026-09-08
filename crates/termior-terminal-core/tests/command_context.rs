use termior_terminal_core::{OscCommandTracker, OscEvent, OutputCursor, TerminalContextReference};

#[test]
fn osc_command_boundaries_create_structured_records_without_guessing() {
    let mut tracker = OscCommandTracker::new("terminal-1", "/project");
    tracker.observe(
        &OscEvent::CommandStart {
            cmd: "cargo test".into(),
        },
        OutputCursor { sequence: 10 },
    );
    tracker.observe(
        &OscEvent::CommandExit { code: Some(1) },
        OutputCursor { sequence: 28 },
    );

    let record = tracker.records().last().unwrap();
    assert_eq!(record.command.as_deref(), Some("cargo test"));
    assert_eq!(record.exit_code, Some(1));
    assert_eq!(record.output_start.sequence, 10);
    assert_eq!(record.output_end.unwrap().sequence, 28);
}

#[test]
fn selection_without_shell_integration_keeps_unknown_fields_unknown() {
    let reference = TerminalContextReference::selection(
        "terminal-plain",
        "/project",
        OutputCursor { sequence: 4 },
        OutputCursor { sequence: 9 },
        "compiler output",
    );
    assert_eq!(reference.command_session_id, None);
    assert_eq!(reference.command, None);
    assert_eq!(reference.exit_code, None);
    assert_eq!(reference.output_start.sequence, 4);
    assert_eq!(reference.output_end.sequence, 9);
}

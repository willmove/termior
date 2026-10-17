//! stdio stand-in for `codex app-server`.
//!
//! `CodexBackend` spawns an executable, checks `--version`, then speaks JSON-RPC on
//! stdin/stdout. This file is compiled by the contract test (no crate dependencies)
//! and replays one redacted `item/permissions/requestApproval` line from `request.jsonl`
//! next to the executable. The client's reply is written verbatim to
//! `captured-response.json`.

use std::env;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    if args.next().as_deref() == Some("--version") {
        println!("codex-cli 0.153.0");
        return ExitCode::SUCCESS;
    }
    match serve() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fake codex app-server: {error}");
            ExitCode::from(1)
        }
    }
}

fn serve() -> io::Result<()> {
    let dir = env::current_exe()?
        .parent()
        .ok_or_else(|| io::Error::other("fake codex executable has no directory"))?
        .to_path_buf();
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let mut stdout = io::stdout().lock();

    let initialize = next_line(&mut lines, "initialize")?;
    write_result(
        &mut stdout,
        &raw_id(&initialize)?,
        r#"{"userAgent":"termior-fixture"}"#,
    )?;
    let _initialized = next_line(&mut lines, "initialized")?;

    let thread_start = next_line(&mut lines, "thread/start")?;
    write_result(
        &mut stdout,
        &raw_id(&thread_start)?,
        r#"{"thread":{"id":"thr_fixture"}}"#,
    )?;

    let turn_start = next_line(&mut lines, "turn/start")?;
    write_result(
        &mut stdout,
        &raw_id(&turn_start)?,
        r#"{"turn":{"id":"turn_fixture"}}"#,
    )?;

    let request = fs::read_to_string(dir.join("request.jsonl"))?;
    let request = request.trim();
    if request.is_empty() {
        return Err(io::Error::other("request.jsonl is empty"));
    }
    writeln!(stdout, "{request}")?;
    stdout.flush()?;

    let response = next_line(&mut lines, "approval response")?;
    write_capture(&dir.join("captured-response.json"), &response)?;

    writeln!(
        stdout,
        "{}",
        r#"{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thr_fixture","turn":{"id":"turn_fixture","status":"completed","items":[]}}}"#
    )?;
    stdout.flush()?;
    while lines.next().transpose()?.is_some() {}
    Ok(())
}

fn next_line(
    lines: &mut impl Iterator<Item = io::Result<String>>,
    step: &str,
) -> io::Result<String> {
    lines
        .next()
        .transpose()?
        .filter(|line| !line.is_empty())
        .ok_or_else(|| io::Error::other(format!("stdin closed before {step}")))
}

fn raw_id(line: &str) -> io::Result<String> {
    let marker = "\"id\":";
    let start = line
        .find(marker)
        .ok_or_else(|| io::Error::other(format!("JSON-RPC line has no id: {line}")))?
        + marker.len();
    let rest = &line[start..];
    let end = rest
        .find(|ch: char| ch == ',' || ch == '}')
        .ok_or_else(|| io::Error::other(format!("JSON-RPC id is truncated: {line}")))?;
    let id = rest[..end].trim();
    if id.is_empty() {
        return Err(io::Error::other(format!("JSON-RPC id is empty: {line}")));
    }
    Ok(id.to_string())
}

fn write_result(stdout: &mut impl Write, id: &str, result: &str) -> io::Result<()> {
    writeln!(
        stdout,
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{result}}}"
    )?;
    stdout.flush()
}

fn write_capture(path: &PathBuf, line: &str) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    writeln!(file, "{line}")?;
    file.sync_all()
}

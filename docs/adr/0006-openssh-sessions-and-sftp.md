# ADR 0006: User-owned OpenSSH sessions and SFTP

Status: accepted, amended for active-tab Remote Explorer (2026-09-17)

## Context

Termior needs SSH sessions, authentication and SFTP transfers without confusing remote
hosts with the local workspace or copying credentials into application state. Existing
portable-pty, the terminal renderer and Windows Job Object already provide terminal
interaction and process ownership.

## Decision

Use installed OpenSSH `ssh` and `sftp`, invoked directly with validated argv. Add the
GPUI-independent `termior-ssh` crate for profiles, invocation and SFTP command quoting.
Use the normal PTY bridge for authentication, output, resizing and cancellation.

The connection manager saves only non-secret metadata using the store's atomic JSON
writer. OpenSSH owns password/keyboard-interactive prompts, encrypted keys, agent
communication, config aliases, ProxyJump and known_hosts. Force host checking to `ask`,
disable agent/X11/port forwarding and LocalCommand for these ordinary sessions.
Respect config and system trust stores; never offer an insecure ignore-host-key mode.

SFTP offers an interactive terminal and user-reviewed file/directory transfer jobs.
Jobs use a private temporary batch file retained for the child lifetime. `BatchMode=no`
precedes `-b`, preserving PTY authentication, and `-N` enables transfer output. Filenames
are quoted using the SFTP parser's rules, not shell rules. On Windows OpenSSH rewrites
backslashes before parsing; quoted glob characters are escaped by its own lexer.

Remote terminal streams cannot update local cwd/project_dir, localhost previews or OSC agent
state, and are excluded from LocalTerminalService registration and automatic Composer context.
Remote OSC 7 may be retained only as session-scoped cwd metadata for that tab's SFTP Explorer.
When a user-owned SSH/SFTP tab is active, the File Explorer uses a separate OpenSSH SFTP batch
connection for remote listing and mutations; switching tabs restores the appropriate local or
remote source. The Agent and Git remain local. Closing a tab terminates its transport.
Connection failure retains output; app restart never reconnects or replays a job.

## Consequences

- Requires system OpenSSH (SFTP job progress uses `-N`; tested with Windows OpenSSH 9.5p2).
- No new crypto, network runtime or UI dependency; production reuses existing crates.
- Opt-in password/passphrase reuse uses the existing OS keyring dependency, with no
  plaintext fallback. A separate mode of the application executable acts as OpenSSH
  askpass before any logger, workspace or panic log is initialized. Only profile metadata
  and a per-session retry marker directory enter the environment; replies use SSH's pipe.
- The vault scope is a hash of destination/user/port/key/jump/trust-file metadata, not the
  editable display name. Changing a target never forwards an old secret to it. Unused
  old credentials are cleaned up on target changes, disabling storage or profile deletion.
- Saved responses are used once per credential per session, only for exact destination
  password or configured private-key prompts. Rejected values fall back to a masked
  interactive window. Host trust remains explicit, OTP is never retained. ProxyJump and
  ProxyCommand use interactive responses/agent because askpass cannot authenticate which
  hop generated a server-controlled keyboard-interactive prompt.
- Connection settings remain schema v1 with a default-false `use_saved_credentials`
  flag. Private key files remain managed by OpenSSH; no raw key import or plaintext copy.
- Preferences and the sidebar share the same manager. Profile change events refresh
  the saved connection list; sidebar actions use the normal remote tab lifecycle.
- SFTP browsing/file management is available both in the interactive terminal and in the single
  File Explorer surface selected by the active tab. The GUI backend writes validated commands to
  SFTP batch stdin and parses directory listings; it never builds a remote shell command. Remote
  editing and Agent execution remain separate future work.
- Explorer SFTP jobs use the same isolated askpass helper and host-key policy. They may establish
  additional authenticated SFTP connections; saved credentials or SSH Agent avoid repeated
  prompts. Remote recursive deletion is implemented by bounded enumeration followed by quoted
  `rm`/`rmdir` SFTP commands, with no shell fallback.
- Cancelling a transfer can leave a partial destination. Resume is explicit and requires
  matching existing contents; no automatic retries or claims of atomic remote writes.
- Localhost fixture tests are opt-in, use temporary keys and require a Python Paramiko
  environment. Paramiko is test infrastructure only and is not shipped.
- Native dependencies, cold-start and idle architecture are unchanged; new code size
  has not been separately benchmarked across release platforms.

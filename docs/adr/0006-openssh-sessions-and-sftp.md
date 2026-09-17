# ADR 0006: User-owned OpenSSH sessions and SFTP

Status: accepted, amended for active-tab Remote Explorer and shared authentication (2026-09-17)

## Context

Termior needs SSH sessions, authentication and SFTP transfers without confusing remote
hosts with the local workspace or copying credentials into application state. Existing
portable-pty, the terminal renderer and Windows Job Object already provide terminal
interaction and process ownership.

## Decision

Use installed OpenSSH `ssh` and `sftp`, invoked directly with validated argv. Add the
GPUI-independent `termior-ssh` crate for profiles, invocation and SFTP command quoting.
Use the normal PTY bridge for output, resizing and cancellation; route desktop authentication
through one coordinator per credential target shared by PTY, Explorer and transfer tabs.

The connection manager saves only non-secret metadata using the store's atomic JSON
writer. OpenSSH owns password/keyboard-interactive prompts, encrypted keys, agent
communication, config aliases, ProxyJump and known_hosts. Force host checking to `ask`,
disable agent/X11/port forwarding and LocalCommand for these ordinary sessions.
Respect config and system trust stores; never offer an insecure ignore-host-key mode.

SFTP sessions open a graphical local/remote file browser, reusing the persistent subsystem
client without spawning an interactive SFTP PTY. Transfers remain user-reviewed file/directory jobs.
Jobs use a private temporary batch file retained for the child lifetime. `BatchMode=no`
precedes `-b`, preserving PTY authentication, and `-N` enables transfer output. Filenames
are quoted using the SFTP parser's rules, not shell rules. On Windows OpenSSH rewrites
backslashes before parsing; quoted glob characters are escaped by its own lexer.

Remote terminal streams cannot update local cwd/project_dir, localhost previews or OSC agent
state, and are excluded from LocalTerminalService registration and automatic Composer context.
Remote OSC 7 may be retained only as session-scoped cwd metadata for that tab's SFTP Explorer.
When a user-owned SSH/SFTP tab is active, the File Explorer uses a separate, persistent OpenSSH
SFTP subsystem connection for remote listing and mutations; switching tabs restores the appropriate local or
remote source. The Agent and Git remain local. Closing a tab terminates its transport.
Connection failure retains output; app restart never reconnects or replays a job.

## Consequences

- Requires system OpenSSH (SFTP job progress uses `-N`; tested with Windows OpenSSH 9.5p2).
- No new crypto, network runtime or UI dependency. Local authentication IPC uses `interprocess`
  2.4.2 (0BSD) and Windows security-descriptor strings use `widestring` (MIT); see NOTICE.
- Opt-in password/passphrase reuse uses the existing OS keyring dependency, with no
  plaintext fallback. A separate mode of the application executable acts as OpenSSH
  askpass before any logger, workspace or panic log is initialized. Only profile metadata
  and non-secret IPC/lifetime identifiers enter the environment; replies use SSH's pipe.
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
- The Explorer uses a small, bounded SFTP v3 protocol client over system `ssh -T -s … sftp`,
  replacing per-operation batch processes and human-readable `ls` parsing. This amends the initial
  Remote Explorer decision: one worker/connection per tab, with no new SSH crypto implementation.
  Transfer jobs continue to use the existing OpenSSH `sftp` PTY implementation; ordinary SFTP tabs no longer spawn that PTY.
- Explorer uses `CREATE_NO_WINDOW` on Windows, piped IO, bounded packets/diagnostics, a cancellable
  five-minute total request deadline, and worker-owned process cleanup (including process-tree
  termination). Askpass prompts close when their Explorer session's lifetime directory disappears.
- Directory caching lasts 30 seconds, bounded to 32 keys/50,000 entries per tab. Refresh bypasses
  cache; mutations and transfer completion invalidate it, including failed/partial writes. UI
  navigation coalesces identical requests and retains only the latest pending destination. Normal
  SFTP status errors retain authentication; transport failure is retried only by explicit user action.
- Desktop PTY and Explorer force askpass even without saved credentials. Helpers forward to a
  host-scoped coordinator over owner-only Windows named pipes (remote access denied), or Unix
  sockets in mode-0700 directories. There is no TCP listener or secret in IPC endpoint metadata.
  Missing coordinators fail closed, never opening an independent fallback prompt. Requests and
  frames are bounded; transport lifetime/timeout cancels pending prompts.
- Coordinators serialize prompts and keep ordinary destination passwords/configured key passphrases
  in zeroizing memory until the final owning session closes. Each transport consumes a credential
  revision once; a repeated challenge invalidates it and prompts for replacement. Cancel rejects
  already-started transports in that login epoch; explicit reconnect can register a new attempt.
  Unknown/MFA/proxy challenges are never cached or persisted. Host-key confirmation can only be
  reused for the exact prompt already explicitly approved in the live coordinator.
- The shared prompt offers opt-in OS-vault persistence on first entry, using the existing per-profile
  flag. Unchecking storage removes unreferenced saved credentials. Manager settings refresh without
  discarding unsaved edits. Network connections remain independent: this is shared authentication,
  not cross-process SSH multiplexing or a claim that OTP is reusable across connections.
- Recursive deletion enumerates with depth/entry bounds, removes symlinks without following them,
  and refuses the remote root. Creation uses exclusive OPEN (no silent truncation); standard v3
  RENAME does not overwrite an existing destination. Remote editing and Agent execution stay out of scope.
- Cancelling a transfer can leave a partial destination. Resume is explicit and requires
  matching existing contents; no automatic retries or claims of atomic remote writes.
- Localhost fixture tests are opt-in, use temporary keys and require a Python Paramiko
  environment. Paramiko is test infrastructure only and is not shipped.
- Native dependencies, cold-start and idle architecture are unchanged; new code size
  has not been separately benchmarked across release platforms.

## Graphical session browser amendment (2026-09-17)

- Saved connections use one compact row per profile. Single click selects; double click
  connects SSH. SFTP is a context-menu action on that profile, avoiding redundant saved
  entries and an additional settings toggle. Open sessions remain selectable separately.
- SFTP tabs render a local and remote detail list in the main area. Local directories are
  read shallowly off the UI thread, bounded to 20,000 entries, including hidden/ignored
  files. They never change the workspace/Agent/Git root. Remote listing and mutations
  continue to use the per-tab SFTP worker and authentication coordinator.
- Cross-pane drag/drop selects the hovered directory or the current pane directory.
  A prompt identifies source, destination and overwrite semantics before creating the
  existing cancellable transfer tabs in the background. The browser keeps focus and shows
  a bounded job summary with status, cancel and detailed-progress links. A pending confirmation cannot revive a closed or
  disconnected source browser. File and recursive folder transfers share the same backend.
- Closing a browser drops its transport and local listing; disconnect retains the local
  browser for explicit reconnect. Restored tabs remain disconnected, with no automatic
  replay. Existing remote-terminal Explorer remains available alongside SSH shell tabs.

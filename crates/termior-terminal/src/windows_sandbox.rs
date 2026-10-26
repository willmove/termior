//! Windows AppContainer confinement (Stage D task 6.2).
//!
//! An AppContainer is a low-privilege Windows identity whose filesystem access is granted
//! per directory through an ACL. A command launched inside the container can read and
//! write the granted roots and nothing else, and it does not inherit the parent's
//! environment or credentials.
//!
//! The escape suite in `termior-agent-host` uses this module to prove the confinement
//! actually holds. Capabilities this module cannot enforce stay unsupported so a task
//! that requires them fails closed.

use std::path::Path;

/// AppContainer profile name. All Termior sandboxed tasks share one profile so the
/// granted ACLs stay stable across restarts and a task can recover its own environment.
pub const APP_CONTAINER_NAME: &str = "TermiorSandbox";

/// Errors from the AppContainer backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppContainerError {
    /// The AppContainer profile could not be resolved or created.
    Profile(String),
    /// The directory ACL could not be written.
    Acl(String),
    /// The confined process could not be started.
    Spawn(String),
}

impl std::fmt::Display for AppContainerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Profile(detail) => write!(formatter, "appcontainer profile failed: {detail}"),
            Self::Acl(detail) => write!(formatter, "appcontainer acl failed: {detail}"),
            Self::Spawn(detail) => write!(formatter, "appcontainer spawn failed: {detail}"),
        }
    }
}

impl std::error::Error for AppContainerError {}

/// Whether this machine can resolve an AppContainer profile.
pub fn app_container_available() -> bool {
    AppContainer::resolve(APP_CONTAINER_NAME).is_ok()
}

/// Grant the Termior AppContainer read/write access to one directory.
pub fn grant_directory(path: &Path) -> Result<(), AppContainerError> {
    let container = AppContainer::resolve(APP_CONTAINER_NAME)?;
    container.grant(path)
}

/// Grant the Termior AppContainer access to one directory and to the path to it.
///
/// A confined token has to walk every directory between the volume root and the
/// workspace before it can open anything inside the workspace. The directories
/// above a workspace often belong to the user alone, so they need an explicit
/// traversal grant. The grant is not inherited by the files below them: the
/// ancestors stay unreadable, they only become walkable.
pub fn grant_directory_with_ancestors(path: &Path) -> Result<(), AppContainerError> {
    let container = AppContainer::resolve(APP_CONTAINER_NAME)?;
    container.grant_with_ancestors(path)
}

/// Grant the Termior AppContainer access to one path.
///
/// A directory grants access to the whole tree below it. A file grants access to that
/// file only, which is what the sandbox needs for the system tools it runs.
pub fn grant_path(path: &Path) -> Result<(), AppContainerError> {
    let container = AppContainer::resolve(APP_CONTAINER_NAME)?;
    container.grant(path)
}

/// Remove the Termior AppContainer profile. Used by tests and uninstall paths.
pub fn delete_profile(name: &str) {
    imp::delete_profile(name)
}

/// Quote an argument for the Windows command-line parser.
pub fn quote_argument(argument: &str) -> String {
    if !argument.is_empty()
        && !argument.contains([' ', '\t', '\n', '"', '&', '|', '<', '>', '^', '%'])
    {
        return argument.to_string();
    }
    let mut quoted = String::with_capacity(argument.len() + 2);
    quoted.push('"');
    let mut backslashes = 0usize;
    for character in argument.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            other => {
                if backslashes > 0 {
                    quoted.push_str(&"\\".repeat(backslashes));
                    backslashes = 0;
                }
                quoted.push(other);
            }
        }
    }
    if backslashes > 0 {
        quoted.push_str(&"\\".repeat(backslashes * 2));
    }
    quoted.push('"');
    quoted
}

/// Build a Windows command line for a program and its arguments.
pub fn build_command_line(program: &Path, args: &[String]) -> String {
    let mut parts = vec![quote_argument(&program.display().to_string())];
    parts.extend(args.iter().map(|argument| quote_argument(argument)));
    parts.join(" ")
}

#[cfg(windows)]
#[path = "windows_sandbox/imp.rs"]
mod imp;

#[cfg(windows)]
pub use imp::{AppContainer, ConfinedProcess, PipedConfinedProcess};

#[cfg(not(windows))]
mod imp {
    use super::*;

    /// Placeholder so the shared API compiles on non-Windows platforms.
    pub struct AppContainer;

    /// Placeholder for a confined process on non-Windows platforms.
    pub struct ConfinedProcess;

    /// Placeholder for a piped confined process on non-Windows platforms.
    pub struct PipedConfinedProcess;

    impl AppContainer {
        pub fn resolve(_name: &str) -> Result<Self, AppContainerError> {
            Err(AppContainerError::Profile(
                "the Windows AppContainer backend is only available on Windows".into(),
            ))
        }

        pub fn grant(&self, _path: &Path) -> Result<(), AppContainerError> {
            Err(AppContainerError::Acl(
                "the Windows AppContainer backend is only available on Windows".into(),
            ))
        }

        pub fn grant_with_ancestors(&self, _path: &Path) -> Result<(), AppContainerError> {
            Err(AppContainerError::Acl(
                "the Windows AppContainer backend is only available on Windows".into(),
            ))
        }

        pub fn spawn(
            &self,
            _program: &Path,
            _args: &[String],
            _cwd: &Path,
        ) -> Result<ConfinedProcess, AppContainerError> {
            Err(AppContainerError::Spawn(
                "the Windows AppContainer backend is only available on Windows".into(),
            ))
        }

        pub fn spawn_piped(
            &self,
            _program: &Path,
            _args: &[String],
            _cwd: &Path,
        ) -> Result<PipedConfinedProcess, AppContainerError> {
            Err(AppContainerError::Spawn(
                "the Windows AppContainer backend is only available on Windows".into(),
            ))
        }
    }

    pub fn delete_profile(_name: &str) {}
}

#[cfg(not(windows))]
pub use imp::{AppContainer, ConfinedProcess, PipedConfinedProcess};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn plain_arguments_are_not_quoted() {
        assert_eq!(quote_argument("simple"), "simple");
        assert_eq!(quote_argument("with space"), "\"with space\"");
        assert_eq!(quote_argument(""), "\"\"");
    }

    #[test]
    fn a_backslash_before_a_quote_is_escaped() {
        assert_eq!(quote_argument("a\\"), "a\\");
        assert_eq!(quote_argument("a\\\"b"), "\"a\\\\\\\"b\"");
    }

    #[test]
    fn the_command_line_quotes_the_program_path() {
        let program = PathBuf::from(r"C:\Windows\System32\cmd.exe");
        assert_eq!(
            build_command_line(&program, &["/c".into()]),
            "C:\\Windows\\System32\\cmd.exe /c"
        );
    }

    /// The quoting must match what Windows itself parses. `CommandLineToArgvW` is the
    /// parser `CreateProcessW` and every shell use, so a round trip through it is the
    /// authoritative check.
    #[cfg(windows)]
    #[test]
    fn the_command_line_survives_the_windows_argument_parser() {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

        let cases = [
            vec!["plain".to_string()],
            vec!["with space".to_string()],
            vec!["trailing\\".to_string()],
            vec!["quote\\\"inside".to_string()],
            vec!["".to_string()],
            vec!["a".to_string(), "b c".to_string(), "d\\".to_string()],
        ];
        for case in cases {
            let command_line = build_command_line(Path::new("program.exe"), &case);
            let wide: Vec<u16> = command_line
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let mut count: i32 = 0;
            // SAFETY: the command line is a valid null-terminated UTF-16 string and
            // `count` is a valid out-pointer.
            let parsed = unsafe { CommandLineToArgvW(wide.as_ptr(), &mut count) };
            assert!(
                !parsed.is_null(),
                "the command line was rejected: {command_line}"
            );
            let arguments = (0..count)
                .map(|index| {
                    // SAFETY: `parsed` holds `count` null-terminated UTF-16 strings.
                    let start = unsafe { *parsed.offset(index as isize) };
                    let mut length = 0usize;
                    // SAFETY: the string is null-terminated, so the scan stops at the
                    // terminator.
                    unsafe {
                        while *start.add(length) != 0 {
                            length += 1;
                        }
                        String::from_utf16_lossy(std::slice::from_raw_parts(start, length))
                    }
                })
                .collect::<Vec<_>>();
            // SAFETY: the array was allocated by `CommandLineToArgvW`.
            unsafe {
                LocalFree(parsed as *mut std::ffi::c_void);
            }
            // The program path comes first, then the arguments unchanged.
            assert_eq!(
                arguments,
                std::iter::once("program.exe".to_string())
                    .chain(case.iter().cloned())
                    .collect::<Vec<_>>(),
                "the command line did not round trip: {command_line:?}"
            );
        }
    }
}

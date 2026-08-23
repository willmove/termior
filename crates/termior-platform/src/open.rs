use std::path::Path;
use std::process::{Command, Stdio};
use url::Url;

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("invalid external URL: {0}")]
    InvalidUrl(String),
    #[error("failed to launch external browser: {0}")]
    Launch(#[from] std::io::Error),
    #[error("external browser command failed")]
    Failed,
}

/// Spawns the OS default handler without imposing a scheme boundary.
/// Callers are responsible for validating the value before invoking this.
fn launch_external(value: &str) -> Result<(), PlatformError> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("rundll32");
        command.args(["url.dll,FileProtocolHandler", value]);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(value);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(value);
        command
    };
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(PlatformError::Failed)
    }
}

/// Opens an http(s) URL in the default browser. Enforces the HTTP/HTTPS-only boundary so
/// the system-browser opener never receives a non-web scheme.
pub fn open_external(value: &str) -> Result<(), PlatformError> {
    let url = Url::parse(value).map_err(|_| PlatformError::InvalidUrl(value.to_owned()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(PlatformError::InvalidUrl(value.to_owned()));
    }
    launch_external(value)
}

/// Opens a local file with the OS default handler (e.g. the default browser for an .html
/// document). The path originates from the user's own workspace, not an untrusted URL, so it
/// is exempt from the http/https-only boundary enforced by open_external. Uses a file://
/// URL so Windows never passes the raw path through cmd /c.
pub fn open_local_file(path: &Path) -> Result<(), PlatformError> {
    let url = Url::from_file_path(path)
        .map_err(|_| PlatformError::InvalidUrl(path.display().to_string()))?;
    launch_external(url.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_web_schemes_without_launching() {
        assert!(matches!(
            open_external("file:///etc/passwd"),
            Err(PlatformError::InvalidUrl(_))
        ));
        assert!(matches!(
            open_external("not a url"),
            Err(PlatformError::InvalidUrl(_))
        ));
    }

    #[test]
    fn local_file_needs_an_absolute_path() {
        assert!(matches!(
            open_local_file(Path::new("relative.html")),
            Err(PlatformError::InvalidUrl(_))
        ));
    }
}

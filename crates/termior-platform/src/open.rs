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

pub fn open_external(value: &str) -> Result<(), PlatformError> {
    let url = Url::parse(value).map_err(|_| PlatformError::InvalidUrl(value.to_owned()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(PlatformError::InvalidUrl(value.to_owned()));
    }
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
}

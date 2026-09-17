//! OpenSSH profiles and OS credential-vault integration. Profiles never contain secrets.
#![forbid(unsafe_code)]

pub mod auth;
pub mod credentials;
pub mod sftp;

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Store(#[from] termior_store::JsonStoreError),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authentication {
    #[default]
    Auto,
    Password,
    Key,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Profile {
    /// Opt-in OS credential vault. Secrets themselves are never serialized.
    pub use_saved_credentials: bool,
    pub name: String,
    /// DNS name, IP literal or an existing OpenSSH Host alias.
    pub host: String,
    /// Empty inherits User from OpenSSH config.
    pub user: String,
    /// None inherits Port from OpenSSH config.
    pub port: Option<u16>,
    pub authentication: Authentication,
    pub identity_file: String,
    /// OpenSSH ProxyJump chain, e.g. alice@bastion:2222.
    pub jump_host: String,
    /// Optional alternate OpenSSH trust store. Empty uses the user's known_hosts.
    pub known_hosts_file: String,
    pub connect_timeout_secs: u32,
    pub keepalive_secs: u32,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            use_saved_credentials: false,
            name: String::new(),
            host: String::new(),
            user: String::new(),
            port: None,
            authentication: Authentication::Auto,
            identity_file: String::new(),
            jump_host: String::new(),
            known_hosts_file: String::new(),
            connect_timeout_secs: 15,
            keepalive_secs: 30,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    Shell,
    Sftp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub profile: Profile,
    pub kind: SessionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer: Option<Transfer>,
}

/// A user-reviewed SFTP transfer. Restart never replays it automatically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transfer {
    pub upload: bool,
    pub local_path: String,
    pub remote_path: String,
    pub recursive: bool,
    pub resume: bool,
}

impl Transfer {
    pub fn batch(&self) -> Result<String, Error> {
        let local_path = if cfg!(windows) {
            self.local_path.replace('\\', "/")
        } else {
            self.local_path.clone()
        };
        let local = quote_sftp_path(&local_path)?;
        let remote = quote_sftp_path(&self.remote_path)?;
        let verb = match (self.upload, self.resume) {
            (true, false) => "put",
            (true, true) => "reput",
            (false, false) => "get",
            (false, true) => "reget",
        };
        let flags = if self.recursive { " -R" } else { "" };
        let (source, destination) = if self.upload {
            (local, remote)
        } else {
            (remote, local)
        };
        Ok(format!("{verb}{flags} {source} {destination}\nbye\n"))
    }
}

/// Escape both the SFTP command lexer and glob expansion; never allow commands/newlines.
pub fn quote_sftp_path(path: &str) -> Result<String, Error> {
    if path.is_empty() || path.chars().any(char::is_control) {
        return Err(invalid(
            "SFTP path cannot be empty or contain control characters",
        ));
    }
    if cfg!(windows) && path.contains('\\') {
        return Err(invalid(
            "Windows OpenSSH cannot preserve literal backslashes in remote filenames",
        ));
    }
    let path = if path.starts_with('-') {
        format!("./{path}")
    } else {
        path.to_owned()
    };
    let mut quoted = String::from("\"");
    for ch in path.chars() {
        match ch {
            // OpenSSH's quoted lexer escapes glob characters itself. Pre-escaping
            // them corrupts paths, especially on Windows (which rewrites '\\').
            '"' => quoted.push_str("\"'\"'\""),
            '\\' => quoted.push_str("\\\\"),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    Ok(quoted)
}

/// Program and individual argv values, never a shell command string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub program: &'static str,
    pub args: Vec<String>,
}

fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
fn safe_identifier(value: &str, punctuation: &str) -> bool {
    !value.starts_with('-')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || punctuation.contains(c))
}

impl Profile {
    pub fn validate(&self) -> Result<(), Error> {
        if self.name.trim().is_empty()
            || self.name.len() > 256
            || self.name.chars().any(char::is_control)
        {
            return Err(invalid(
                "Connection name must be 1–256 bytes without control characters",
            ));
        }
        if self.host.is_empty() || self.host.len() > 253 || !safe_identifier(&self.host, ".-_:") {
            return Err(invalid(
                "Host must be a DNS name, IP address or SSH config alias (without user or port)",
            ));
        }
        if self.user.len() > 256 || !safe_identifier(&self.user, ".-_\\") {
            return Err(invalid("Invalid SSH username"));
        }
        if self.port == Some(0) {
            return Err(invalid("Port must be between 1 and 65535"));
        }
        if !(1..=300).contains(&self.connect_timeout_secs)
            || !(1..=3600).contains(&self.keepalive_secs)
        {
            return Err(invalid(
                "Connection timeout must be 1–300s; keepalive must be 1–3600s",
            ));
        }
        if self.identity_file.chars().any(char::is_control) || self.identity_file.contains('%') {
            return Err(invalid(
                "Identity path cannot contain control characters or OpenSSH expansion tokens",
            ));
        }
        if self.known_hosts_file.chars().any(char::is_control)
            || self.known_hosts_file.contains(['%', '"'])
        {
            return Err(invalid("Invalid known_hosts path"));
        }
        if self.authentication == Authentication::Key && self.identity_file.trim().is_empty() {
            return Err(invalid("Select a private key for key authentication"));
        }
        if !self.jump_host.is_empty()
            && (self.jump_host.len() > 1024
                || self
                    .jump_host
                    .split(',')
                    .any(|hop| hop.is_empty() || !safe_identifier(hop, ".-_:@[]")))
        {
            return Err(invalid("Invalid jump host chain"));
        }
        Ok(())
    }

    pub fn invocation(&self, kind: SessionKind) -> Result<Invocation, Error> {
        self.validate()?;
        let mut args = Vec::<String>::new();
        let mut option = |value: String| {
            args.push("-o".into());
            args.push(value);
        };
        // Command-line options override permissive config. Never silently trust a new key.
        option("StrictHostKeyChecking=ask".into());
        if !self.known_hosts_file.is_empty() {
            option(format!(
                "UserKnownHostsFile=\"{}\"",
                self.known_hosts_file.replace('\\', "/")
            ));
        }
        option("ForwardAgent=no".into());
        option("ForwardX11=no".into());
        option("PermitLocalCommand=no".into());
        option("ClearAllForwardings=yes".into());
        option(format!("ConnectTimeout={}", self.connect_timeout_secs));
        option(format!("ServerAliveInterval={}", self.keepalive_secs));
        option("ServerAliveCountMax=3".into());
        if !self.user.is_empty() {
            option(format!("User={}", self.user));
        }
        if let Some(port) = self.port {
            option(format!("Port={port}"));
        }
        match self.authentication {
            Authentication::Auto => {
                if self.use_saved_credentials {
                    option(
                        "PreferredAuthentications=publickey,password,keyboard-interactive".into(),
                    );
                }
            }
            Authentication::Password => {
                option("PubkeyAuthentication=no".into());
                option(
                    if self.use_saved_credentials {
                        "PreferredAuthentications=password,keyboard-interactive"
                    } else {
                        "PreferredAuthentications=keyboard-interactive,password"
                    }
                    .into(),
                );
            }
            Authentication::Key => {
                option("IdentitiesOnly=yes".into());
                option(
                    if self.use_saved_credentials {
                        "PreferredAuthentications=publickey,password,keyboard-interactive"
                    } else {
                        "PreferredAuthentications=publickey,keyboard-interactive,password"
                    }
                    .into(),
                );
            }
            Authentication::Agent => {
                option("IdentitiesOnly=no".into());
                option("PreferredAuthentications=publickey".into());
                option("IdentityFile=none".into());
            }
        }
        if !self.identity_file.is_empty() && self.authentication != Authentication::Agent {
            args.extend(["-i".into(), self.identity_file.clone()]);
        }
        if !self.jump_host.is_empty() {
            args.extend(["-J".into(), self.jump_host.clone()]);
        }
        if kind == SessionKind::Shell {
            args.push("-tt".into());
        }
        // sftp interprets colons as a path separator, so IPv6 literals must be bracketed.
        let host = if kind == SessionKind::Sftp && self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        args.extend(["--".into(), host]);
        Ok(Invocation {
            program: if kind == SessionKind::Shell {
                "ssh"
            } else {
                "sftp"
            },
            args,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profiles {
    pub version: u32,
    pub connections: Vec<Profile>,
}
impl Default for Profiles {
    fn default() -> Self {
        Self {
            version: 1,
            connections: Vec::new(),
        }
    }
}
impl Profiles {
    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 1 {
            return Err(invalid("Unsupported SSH profiles version"));
        }
        let mut names = std::collections::HashSet::new();
        for profile in &self.connections {
            profile.validate()?;
            if !names.insert(&profile.name) {
                return Err(invalid("SSH connection names must be unique"));
            }
        }
        Ok(())
    }
    pub fn load(dir: &Path) -> Result<Self, Error> {
        let value: Self = termior_store::JsonStore::new(dir, "Termior-ssh.json").load()?;
        value.validate()?;
        Ok(value)
    }
    pub fn save(&self, dir: &Path) -> Result<(), Error> {
        self.validate()?;
        termior_store::JsonStore::new(dir, "Termior-ssh.json").save(self)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile() -> Profile {
        Profile {
            name: "测试".into(),
            host: "server".into(),
            ..Profile::default()
        }
    }
    #[test]
    fn transfer_paths_are_quoted_and_cannot_inject_commands() {
        assert_eq!(quote_sftp_path("-file").unwrap(), "\"./-file\"");
        assert_eq!(quote_sftp_path("a*[1]?\"b").unwrap(), "\"a*[1]?\"'\"'\"b\"");
        assert!(quote_sftp_path("a\n!touch evil").is_err());
        let t = Transfer {
            upload: true,
            local_path: "C:/my files/data".into(),
            remote_path: "/tmp/test file".into(),
            resume: true,
            recursive: true,
        };
        assert_eq!(
            t.batch().unwrap(),
            "reput -R \"C:/my files/data\" \"/tmp/test file\"\nbye\n"
        );
    }
    #[test]
    fn injection_is_rejected() {
        for host in [
            "-oProxyCommand=evil",
            "a\nHost *",
            "a;id",
            "user@host",
            "a b",
            "$(id)",
        ] {
            let mut p = profile();
            p.host = host.into();
            assert!(p.validate().is_err(), "{host}");
        }
        let mut p = profile();
        p.jump_host = "a,$(id)".into();
        assert!(p.validate().is_err());
    }
    #[test]
    fn argv_preserves_key_paths_and_security() {
        let mut p = profile();
        p.authentication = Authentication::Key;
        p.identity_file = "C:/key files/private key".into();
        p.port = Some(2222);
        let cmd = p.invocation(SessionKind::Shell).unwrap();
        assert!(cmd.args.contains(&p.identity_file));
        for option in [
            "StrictHostKeyChecking=ask",
            "Port=2222",
            "ForwardAgent=no",
            "-tt",
        ] {
            assert!(cmd.args.contains(&option.to_owned()));
        }
        assert_eq!(&cmd.args[cmd.args.len() - 2..], &["--", "server"]);
    }
    #[test]
    fn ipv6_sftp_destination_is_not_a_remote_path() {
        let mut p = profile();
        p.host = "2001:db8::1".into();
        assert_eq!(
            p.invocation(SessionKind::Sftp)
                .unwrap()
                .args
                .last()
                .unwrap(),
            "[2001:db8::1]"
        );
    }
    #[test]
    fn store_roundtrip_and_no_secret_fields() {
        let dir = tempfile::tempdir().unwrap();
        let profiles = Profiles {
            connections: vec![profile()],
            ..Profiles::default()
        };
        profiles.save(dir.path()).unwrap();
        assert_eq!(
            Profiles::load(dir.path()).unwrap().connections,
            profiles.connections
        );
        let raw = r#"{"name":"a","host":"b","password":"secret"}"#;
        assert!(serde_json::from_str::<Profile>(raw).is_err());
    }
    #[test]
    fn invalid_configuration_cannot_overwrite_saved_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let mut profiles = Profiles {
            connections: vec![profile()],
            ..Profiles::default()
        };
        profiles.save(dir.path()).unwrap();
        profiles.connections.push(profile());
        assert!(profiles.save(dir.path()).is_err());
        assert_eq!(Profiles::load(dir.path()).unwrap().connections.len(), 1);
    }
}

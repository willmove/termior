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
    /// Group name from `Profiles::groups`. Empty means ungrouped.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    /// Free-form labels for future filtering (v3). Single line, no commas.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Free-form single-line reminder (v3). Never rendered outside the editor.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    /// Remembered default directory for SFTP sessions and transfers (v3).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sftp_remote_path: String,
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
            group: String::new(),
            tags: Vec::new(),
            notes: String::new(),
            sftp_remote_path: String::new(),
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

fn valid_group_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.chars().any(char::is_control)
        && name.trim() == name
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
        if !self.group.is_empty() && !valid_group_name(&self.group) {
            return Err(invalid(
                "Group name must be 1–64 bytes without control characters or surrounding whitespace",
            ));
        }
        if self.tags.len() > 16
            || self.tags.iter().any(|tag| {
                tag.is_empty()
                    || tag.len() > 64
                    || tag.contains(',')
                    || tag.chars().any(char::is_control)
            })
        {
            return Err(invalid(
                "Tags: at most 16, each 1–64 bytes, no commas or control characters",
            ));
        }
        if self.notes.len() > 2048 || self.notes.chars().any(char::is_control) {
            return Err(invalid("Notes must be at most 2048 bytes on a single line"));
        }
        if self.sftp_remote_path.len() > 1024 || self.sftp_remote_path.chars().any(char::is_control)
        {
            return Err(invalid(
                "SFTP remote path cannot contain control characters",
            ));
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

/// Schema version of `Termior-ssh.json`. v1 predated connection groups;
/// v3 added per-connection tags, notes and the remembered SFTP remote path.
pub const PROFILES_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profiles {
    pub version: u32,
    /// Persisted group names in display order. Groups exist independently of
    /// connections: empty groups are legal and outlive their members.
    #[serde(default)]
    pub groups: Vec<String>,
    pub connections: Vec<Profile>,
}
impl Default for Profiles {
    fn default() -> Self {
        Self {
            version: PROFILES_VERSION,
            groups: Vec::new(),
            connections: Vec::new(),
        }
    }
}
/// Render-time partition of connections: ungrouped indices first, then every
/// stored group (including empty ones) in `groups` order.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ConnectionGroups {
    pub ungrouped: Vec<usize>,
    pub groups: Vec<(String, Vec<usize>)>,
}
pub fn group_connections(connections: &[Profile], groups: &[String]) -> ConnectionGroups {
    let mut partition = ConnectionGroups {
        ungrouped: Vec::new(),
        groups: groups
            .iter()
            .map(|name| (name.clone(), Vec::new()))
            .collect(),
    };
    for (index, profile) in connections.iter().enumerate() {
        if profile.group.is_empty() {
            partition.ungrouped.push(index);
        } else if let Some((_, members)) = partition
            .groups
            .iter_mut()
            .find(|(name, _)| *name == profile.group)
        {
            members.push(index);
        } else {
            // A missing group reference cannot pass validation, but render totally.
            partition.ungrouped.push(index);
        }
    }
    partition
}
impl Profiles {
    pub fn validate(&self) -> Result<(), Error> {
        if self.version != PROFILES_VERSION {
            return Err(invalid("Unsupported SSH profiles version"));
        }
        let mut group_names = std::collections::HashSet::new();
        for group in &self.groups {
            if !valid_group_name(group) {
                return Err(invalid(
                    "SSH group names must be 1–64 bytes without control characters or surrounding whitespace",
                ));
            }
            if !group_names.insert(group) {
                return Err(invalid("SSH group names must be unique"));
            }
        }
        let mut names = std::collections::HashSet::new();
        for profile in &self.connections {
            profile.validate()?;
            if !profile.group.is_empty() && !group_names.contains(&profile.group) {
                return Err(invalid(&format!(
                    "Connection “{}” refers to missing group “{}”",
                    profile.name, profile.group
                )));
            }
            if !names.insert(&profile.name) {
                return Err(invalid("SSH connection names must be unique"));
            }
        }
        Ok(())
    }
    /// Ensure a group exists so a connection may reference it, preserving order.
    pub fn upsert_group(&mut self, group: &str) {
        if !group.is_empty() && !self.groups.iter().any(|name| name == group) {
            self.groups.push(group.to_owned());
        }
    }
    pub fn load(dir: &Path) -> Result<Self, Error> {
        let mut value: Self = termior_store::JsonStore::new(dir, "Termior-ssh.json").load()?;
        // v1 predated groups; v3 fields (tags/notes/sftp_remote_path) all have
        // serde defaults, so every older file upgrades in place.
        if value.version < PROFILES_VERSION {
            value.version = PROFILES_VERSION;
        }
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
    #[test]
    fn v1_and_v2_files_migrate_to_v3_on_load() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Termior-ssh.json"),
            r#"{"version":1,"connections":[{"use_saved_credentials":false,"name":"legacy","host":"server","user":"","port":null,"authentication":"auto","identity_file":"","jump_host":"","known_hosts_file":"","connect_timeout_secs":15,"keepalive_secs":30}]}"#,
        )
        .unwrap();
        let migrated = Profiles::load(dir.path()).unwrap();
        assert_eq!(migrated.version, PROFILES_VERSION);
        assert!(migrated.groups.is_empty());
        assert_eq!(migrated.connections[0].group, "");
        assert_eq!(migrated.connections[0].tags, Vec::<String>::new());
        assert_eq!(migrated.connections[0].notes, "");
        assert_eq!(migrated.connections[0].sftp_remote_path, "");
        std::fs::write(
            dir.path().join("Termior-ssh.json"),
            r#"{"version":2,"groups":["prod"],"connections":[{"use_saved_credentials":false,"name":"v2","host":"server","user":"","port":null,"authentication":"auto","identity_file":"","jump_host":"","known_hosts_file":"","connect_timeout_secs":15,"keepalive_secs":30,"group":"prod"}]}"#,
        )
        .unwrap();
        let migrated = Profiles::load(dir.path()).unwrap();
        assert_eq!(migrated.version, PROFILES_VERSION);
        assert_eq!(migrated.connections[0].group, "prod");
        let future = r#"{"version":4,"connections":[]}"#;
        std::fs::write(dir.path().join("Termior-ssh.json"), future).unwrap();
        assert!(Profiles::load(dir.path()).is_err());
    }
    #[test]
    fn tags_notes_and_remote_path_roundtrip_with_validation() {
        let mut p = profile();
        p.tags = vec!["prod".into(), "数据库".into()];
        p.notes = "笔记本".into();
        p.sftp_remote_path = "/srv/data".into();
        let dir = tempfile::tempdir().unwrap();
        Profiles {
            connections: vec![p.clone()],
            ..Profiles::default()
        }
        .save(dir.path())
        .unwrap();
        assert_eq!(Profiles::load(dir.path()).unwrap().connections[0], p);
        for bad in [
            Profile {
                tags: vec!["a,b".into()],
                ..profile()
            },
            Profile {
                tags: vec!["x".repeat(65)],
                ..profile()
            },
            Profile {
                tags: (0..17).map(|i| i.to_string()).collect(),
                ..profile()
            },
            Profile {
                notes: "a\nb".into(),
                ..profile()
            },
            Profile {
                sftp_remote_path: "a\tb".into(),
                ..profile()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }
    #[test]
    fn groups_persist_without_members_and_upsert_preserves_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut profiles = Profiles::default();
        profiles.upsert_group("生产");
        profiles.upsert_group("测试");
        profiles.upsert_group("生产");
        let mut member = profile();
        member.group = "测试".into();
        profiles.connections.push(member);
        profiles.save(dir.path()).unwrap();
        let reloaded = Profiles::load(dir.path()).unwrap();
        assert_eq!(reloaded.groups, vec!["生产".to_owned(), "测试".to_owned()]);
        assert_eq!(reloaded.connections[0].group, "测试");
    }
    #[test]
    fn ungrouped_profiles_serialize_without_a_group_key() {
        let json = serde_json::to_string(&profile()).unwrap();
        assert!(!json.contains("group"));
        let mut grouped = profile();
        grouped.group = "prod".into();
        let json = serde_json::to_string(&grouped).unwrap();
        assert!(json.contains(r#""group":"prod""#));
    }
    #[test]
    fn group_validation_rejects_bad_and_dangling_names() {
        let mut profiles = Profiles::default();
        for bad in ["", " leading", "trailing ", "x\ny", &"x".repeat(65)] {
            profiles.groups = vec![bad.into()];
            assert!(profiles.validate().is_err(), "{bad:?}");
        }
        profiles.groups = vec!["prod".into(), "prod".into()];
        assert!(profiles.validate().is_err());
        profiles.groups = vec!["prod".into()];
        let mut member = profile();
        member.group = "missing".into();
        profiles.connections.push(member);
        let error = profiles.validate().unwrap_err().to_string();
        assert!(error.contains("missing group"), "{error}");
        assert!(profiles.save(tempfile::tempdir().unwrap().path()).is_err());
    }
    #[test]
    fn group_connections_keeps_order_and_empty_groups() {
        let ungrouped = profile();
        let mut a = profile();
        a.name = "a".into();
        a.group = "web".into();
        let mut b = profile();
        b.name = "b".into();
        b.group = "db".into();
        let mut c = profile();
        c.name = "c".into();
        c.group = "web".into();
        let partition = group_connections(
            &[ungrouped, a, b, c],
            &["empty".into(), "web".into(), "db".into()],
        );
        assert_eq!(partition.ungrouped, vec![0]);
        assert_eq!(
            partition.groups,
            vec![
                ("empty".to_owned(), vec![]),
                ("web".to_owned(), vec![1, 3]),
                ("db".to_owned(), vec![2]),
            ]
        );
        assert_eq!(
            group_connections(&[], &[]),
            ConnectionGroups::default(),
            "no groups means a flat list"
        );
    }
}

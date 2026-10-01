//! Explicit, per-profile OpenSSH port forwards (`-L` / `-R` / `-D`).
//!
//! Forwards are opt-in profile data: they are only requested by interactive
//! shell sessions, never by SFTP transports, and are passed to OpenSSH as
//! individual argv values. Runtime state is observed without touching the
//! tunnel itself: local listeners are probed with a bind attempt (connecting
//! would open a channel to the remote target) and OpenSSH's own diagnostics
//! are recognised in the session output.
use serde::{Deserialize, Serialize};
use std::fmt;

use crate::{invalid, safe_identifier, Error};

/// Upper bound per profile; keeps argv and the status popover bounded.
pub const MAX_FORWARDS: usize = 32;
/// Target used when a local/remote spec omits the host (`L 8080`, `L 8080:3000`).
pub const DEFAULT_TARGET_HOST: &str = "127.0.0.1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardKind {
    /// `-L`: listen locally, connect from the remote host.
    Local,
    /// `-R`: listen on the remote host, connect from this machine.
    Remote,
    /// `-D`: local SOCKS proxy whose connections exit on the remote host.
    Dynamic,
}

impl ForwardKind {
    pub fn flag(self) -> &'static str {
        match self {
            Self::Local => "-L",
            Self::Remote => "-R",
            Self::Dynamic => "-D",
        }
    }
    fn letter(self) -> char {
        match self {
            Self::Local => 'L',
            Self::Remote => 'R',
            Self::Dynamic => 'D',
        }
    }
    /// Whether the listening socket lives on this machine (and can be probed).
    pub fn listens_locally(self) -> bool {
        self != Self::Remote
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Forward {
    pub kind: ForwardKind,
    /// Empty uses OpenSSH's default (loopback unless GatewayPorts says otherwise).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bind_address: String,
    pub listen_port: u16,
    /// Empty for dynamic forwards.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub target_host: String,
    /// Zero for dynamic forwards.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub target_port: u16,
}

fn is_zero(port: &u16) -> bool {
    *port == 0
}

fn valid_host(host: &str) -> bool {
    host == "*" || (!host.is_empty() && host.len() <= 253 && safe_identifier(host, ".-_:"))
}

/// IPv6 literals must be bracketed inside OpenSSH forward specs.
fn spec_host(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

fn is_loopback(host: &str) -> bool {
    host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || host == "::1"
        || host.starts_with("127.")
}

impl Forward {
    pub fn local(listen_port: u16, target_host: &str, target_port: u16) -> Self {
        Self {
            kind: ForwardKind::Local,
            bind_address: String::new(),
            listen_port,
            target_host: target_host.into(),
            target_port,
        }
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.listen_port == 0 {
            return Err(invalid("Forward listen port must be between 1 and 65535"));
        }
        if !self.bind_address.is_empty() && !valid_host(&self.bind_address) {
            return Err(invalid("Invalid forward bind address"));
        }
        match self.kind {
            ForwardKind::Dynamic => {
                if !self.target_host.is_empty() || self.target_port != 0 {
                    return Err(invalid("A dynamic (SOCKS) forward has no target"));
                }
            }
            ForwardKind::Local | ForwardKind::Remote => {
                if self.target_host == "*" || !valid_host(&self.target_host) {
                    return Err(invalid("Invalid forward target host"));
                }
                if self.target_port == 0 {
                    return Err(invalid("Forward target port must be between 1 and 65535"));
                }
            }
        }
        Ok(())
    }

    /// The value following `-L` / `-R` / `-D` on the OpenSSH command line.
    pub fn ssh_spec(&self) -> String {
        let mut spec = String::new();
        if !self.bind_address.is_empty() {
            spec.push_str(&spec_host(&self.bind_address));
            spec.push(':');
        }
        spec.push_str(&self.listen_port.to_string());
        if self.kind != ForwardKind::Dynamic {
            spec.push(':');
            spec.push_str(&spec_host(&self.target_host));
            spec.push(':');
            spec.push_str(&self.target_port.to_string());
        }
        spec
    }

    /// Where the listener lives, as shown to the user (`localhost:8080`).
    pub fn listen_endpoint(&self) -> String {
        let host = if self.bind_address.is_empty() {
            "localhost"
        } else {
            &self.bind_address
        };
        format!("{}:{}", spec_host(host), self.listen_port)
    }

    /// Where accepted connections go (`127.0.0.1:5432`); `None` for SOCKS.
    pub fn target_endpoint(&self) -> Option<String> {
        (self.kind != ForwardKind::Dynamic)
            .then(|| format!("{}:{}", spec_host(&self.target_host), self.target_port))
    }

    /// The listener accepts connections from other machines, not just this one.
    pub fn exposed_beyond_loopback(&self) -> bool {
        !is_loopback(&self.bind_address)
    }

    /// Address a local client should use to reach the listener.
    pub fn local_client_address(&self) -> Option<String> {
        if !self.kind.listens_locally() {
            return None;
        }
        let host = match self.bind_address.as_str() {
            "" | "*" | "0.0.0.0" | "localhost" => "127.0.0.1",
            "::" => "::1",
            other => other,
        };
        Some(format!("{}:{}", spec_host(host), self.listen_port))
    }

    /// The (kind, bind, port) triple that cannot be requested twice.
    fn listener_key(&self) -> (bool, String, u16) {
        let bind = if is_loopback(&self.bind_address) {
            String::new()
        } else {
            self.bind_address.to_ascii_lowercase()
        };
        (self.kind.listens_locally(), bind, self.listen_port)
    }
}

impl fmt::Display for Forward {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.kind.letter(), self.ssh_spec())
    }
}

/// Validate a profile's forward list as a whole.
pub fn validate_all(forwards: &[Forward]) -> Result<(), Error> {
    if forwards.len() > MAX_FORWARDS {
        return Err(invalid("At most 32 port forwards per connection"));
    }
    let mut listeners = std::collections::HashSet::new();
    for forward in forwards {
        forward.validate()?;
        if !listeners.insert(forward.listener_key()) {
            return Err(Error::Invalid(format!(
                "Port {} is forwarded more than once on the same side",
                forward.listen_port
            )));
        }
    }
    Ok(())
}

/// Split `[a]:b:c` on colons while keeping bracketed IPv6 literals intact.
fn split_spec(spec: &str) -> Result<Vec<String>, Error> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut bracketed = false;
    let mut was_bracketed = false;
    for ch in spec.chars() {
        match ch {
            '[' if !bracketed && current.is_empty() => {
                bracketed = true;
                was_bracketed = true;
            }
            ']' if bracketed => bracketed = false,
            ':' if !bracketed => {
                parts.push(std::mem::take(&mut current));
                was_bracketed = false;
            }
            '[' | ']' => return Err(invalid("Unbalanced brackets in port forward")),
            _ => {
                if was_bracketed && !bracketed {
                    return Err(invalid("Unexpected text after ']' in port forward"));
                }
                current.push(ch);
            }
        }
    }
    if bracketed {
        return Err(invalid("Unbalanced brackets in port forward"));
    }
    parts.push(current);
    Ok(parts)
}

fn parse_port(value: &str) -> Result<u16, Error> {
    match value.trim().parse::<u16>() {
        Ok(port) if port > 0 => Ok(port),
        _ => Err(Error::Invalid(format!(
            "“{value}” is not a port between 1 and 65535"
        ))),
    }
}

/// Parse one entry: `L 8080:127.0.0.1:80`, `-R 9000:localhost:3000`,
/// `D 1080`, or a bare local spec. Short local/remote forms default the
/// target to [`DEFAULT_TARGET_HOST`]: `8080` ≡ `8080:127.0.0.1:8080`,
/// `8080:3000` ≡ `8080:127.0.0.1:3000`.
pub fn parse(entry: &str) -> Result<Forward, Error> {
    let entry = entry.trim();
    let flagged = entry.starts_with('-');
    let unflagged = entry.strip_prefix('-').unwrap_or(entry);
    let mut chars = unflagged.chars();
    let first = chars.next().map(|c| c.to_ascii_uppercase());
    // A letter only acts as the kind when it stands apart from the spec, so a
    // bare host-like entry is diagnosed as such instead of losing its first letter.
    let separated = flagged
        || chars.clone().next().map_or(true, |c| {
            c.is_whitespace() || c.is_ascii_digit() || matches!(c, '[' | '*')
        });
    let (kind, spec) = match first {
        Some(letter @ ('L' | 'R' | 'D')) if separated => (
            match letter {
                'L' => ForwardKind::Local,
                'R' => ForwardKind::Remote,
                _ => ForwardKind::Dynamic,
            },
            chars.as_str().trim_start(),
        ),
        _ if flagged => {
            return Err(Error::Invalid(format!(
                "“{entry}”: use L (local), R (remote) or D (SOCKS)"
            )))
        }
        _ => (ForwardKind::Local, entry),
    };
    if spec.is_empty() {
        return Err(Error::Invalid(format!("“{entry}” is missing a port")));
    }
    if spec.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(Error::Invalid(format!(
            "“{entry}”: write the forward as port:host:port without spaces"
        )));
    }
    let parts = split_spec(spec)?;
    let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
    let forward = match (kind, parts.as_slice()) {
        (ForwardKind::Dynamic, [port]) => Forward {
            kind,
            bind_address: String::new(),
            listen_port: parse_port(port)?,
            target_host: String::new(),
            target_port: 0,
        },
        (ForwardKind::Dynamic, [bind, port]) => Forward {
            kind,
            bind_address: (*bind).into(),
            listen_port: parse_port(port)?,
            target_host: String::new(),
            target_port: 0,
        },
        (ForwardKind::Dynamic, _) => {
            return Err(Error::Invalid(format!(
                "“{entry}”: a SOCKS forward is D [bind:]port"
            )))
        }
        (_, [port]) => {
            let port = parse_port(port)?;
            Forward {
                kind,
                bind_address: String::new(),
                listen_port: port,
                target_host: DEFAULT_TARGET_HOST.into(),
                target_port: port,
            }
        }
        (_, [port, target_port]) => Forward {
            kind,
            bind_address: String::new(),
            listen_port: parse_port(port)?,
            target_host: DEFAULT_TARGET_HOST.into(),
            target_port: parse_port(target_port)?,
        },
        (_, [port, host, target_port]) => Forward {
            kind,
            bind_address: String::new(),
            listen_port: parse_port(port)?,
            target_host: (*host).into(),
            target_port: parse_port(target_port)?,
        },
        (_, [bind, port, host, target_port]) => Forward {
            kind,
            bind_address: (*bind).into(),
            listen_port: parse_port(port)?,
            target_host: (*host).into(),
            target_port: parse_port(target_port)?,
        },
        _ => {
            return Err(Error::Invalid(format!(
                "“{entry}”: expected [bind:]port:host:port"
            )))
        }
    };
    forward
        .validate()
        .map_err(|error| Error::Invalid(format!("“{entry}”: {error}")))?;
    Ok(forward)
}

/// Parse the editor's single-line list; entries are separated by `,` or `;`
/// (full-width forms included). Empty input means no forwards.
pub fn parse_list(input: &str) -> Result<Vec<Forward>, Error> {
    let forwards = input
        .split([',', ';', '，', '；'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(parse)
        .collect::<Result<Vec<_>, _>>()?;
    validate_all(&forwards)?;
    Ok(forwards)
}

/// Inverse of [`parse_list`]: canonical, round-trippable text for the editor.
pub fn format_list(forwards: &[Forward]) -> String {
    forwards
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Result of probing a local listener address without connecting to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// Nothing listens; a bind succeeded and was released immediately.
    Free,
    /// Something listens (after the tunnel started: OpenSSH itself).
    InUse,
    /// The OS refused the bind (privileged port, policy).
    Denied,
    /// The bind address is not usable on this machine.
    Unavailable(String),
}

/// Probe the listener of a local or dynamic forward. Remote forwards cannot
/// be observed from here and report [`Probe::Unavailable`].
pub fn probe_local(forward: &Forward) -> Probe {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, ToSocketAddrs};
    if !forward.kind.listens_locally() {
        return Probe::Unavailable("remote listener".into());
    }
    let port = forward.listen_port;
    let address = match forward.bind_address.as_str() {
        "" | "localhost" => SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        "*" => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port),
        host => match host.parse::<IpAddr>() {
            Ok(ip) => SocketAddr::new(ip, port),
            Err(_) => match (host, port).to_socket_addrs().map(|mut a| a.next()) {
                Ok(Some(address)) => address,
                Ok(None) => return Probe::Unavailable(format!("{host} did not resolve")),
                Err(error) => return Probe::Unavailable(error.to_string()),
            },
        },
    };
    match TcpListener::bind(address) {
        Ok(listener) => {
            drop(listener);
            Probe::Free
        }
        Err(error) => match error.kind() {
            std::io::ErrorKind::AddrInUse => Probe::InUse,
            std::io::ErrorKind::PermissionDenied => Probe::Denied,
            _ => Probe::Unavailable(error.to_string()),
        },
    }
}

/// A forwarding failure OpenSSH reported in the session output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// `channel_setup_fwd_listener_tcpip: cannot listen to port: N` (-L / -D).
    LocalListenFailed(u16),
    /// `Warning: remote port forwarding failed for listen port N` (-R).
    RemoteListenFailed(u16),
}

/// Incremental recogniser for OpenSSH forwarding diagnostics in PTY output.
///
/// OpenSSH reports these right after authentication, so only the first
/// [`NoticeScanner::BUDGET`] bytes are examined; the scanner then becomes a
/// no-op to keep the terminal hot path untouched. A remote host can print
/// look-alike text, which at worst marks a tunnel failed — never active.
#[derive(Debug, Default)]
pub struct NoticeScanner {
    line: Vec<u8>,
    seen: usize,
}

impl NoticeScanner {
    pub const BUDGET: usize = 256 * 1024;
    const MAX_LINE: usize = 512;

    pub fn finished(&self) -> bool {
        self.seen >= Self::BUDGET
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Notice> {
        let mut notices = Vec::new();
        if self.finished() {
            return notices;
        }
        let take = bytes.len().min(Self::BUDGET - self.seen);
        self.seen += take;
        for &byte in &bytes[..take] {
            if byte == b'\n' || byte == b'\r' {
                if let Some(notice) = Self::recognise(&self.line) {
                    notices.push(notice);
                }
                self.line.clear();
            } else if self.line.len() < Self::MAX_LINE {
                self.line.push(byte);
            }
        }
        if self.finished() {
            self.line = Vec::new();
        }
        notices
    }

    fn recognise(line: &[u8]) -> Option<Notice> {
        let line = std::str::from_utf8(line).ok()?;
        let port_after = |marker: &str| -> Option<u16> {
            let rest = &line[line.find(marker)? + marker.len()..];
            let digits: String = rest
                .trim_start()
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits.parse().ok().filter(|port| *port > 0)
        };
        if let Some(port) = port_after("remote port forwarding failed for listen port") {
            return Some(Notice::RemoteListenFailed(port));
        }
        port_after("cannot listen to port:").map(Notice::LocalListenFailed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openssh_syntax_and_short_forms() {
        assert_eq!(
            parse("-L 8080:127.0.0.1:8080").unwrap(),
            Forward::local(8080, "127.0.0.1", 8080)
        );
        assert_eq!(
            parse("8080").unwrap(),
            Forward::local(8080, "127.0.0.1", 8080)
        );
        assert_eq!(
            parse("l8080:3000").unwrap(),
            Forward::local(8080, "127.0.0.1", 3000)
        );
        let remote = parse("R 0.0.0.0:9000:localhost:3000").unwrap();
        assert_eq!(remote.kind, ForwardKind::Remote);
        assert_eq!(remote.bind_address, "0.0.0.0");
        assert!(remote.exposed_beyond_loopback());
        let socks = parse("D 1080").unwrap();
        assert_eq!(socks.kind, ForwardKind::Dynamic);
        assert_eq!(socks.target_endpoint(), None);
        assert_eq!(socks.ssh_spec(), "1080");
        let v6 = parse("L [::1]:8443:[2001:db8::5]:443").unwrap();
        assert_eq!(v6.bind_address, "::1");
        assert_eq!(v6.target_host, "2001:db8::5");
        assert_eq!(v6.ssh_spec(), "[::1]:8443:[2001:db8::5]:443");
        assert!(!v6.exposed_beyond_loopback());
    }

    #[test]
    fn list_round_trips_through_canonical_text() {
        let forwards =
            parse_list("8080, R 9000:localhost:3000；D *:1080,  L 5432:db.internal:5432").unwrap();
        assert_eq!(forwards.len(), 4);
        let text = format_list(&forwards);
        assert_eq!(
            text,
            "L 8080:127.0.0.1:8080, R 9000:localhost:3000, D *:1080, L 5432:db.internal:5432"
        );
        assert_eq!(parse_list(&text).unwrap(), forwards);
        assert!(parse_list("  ").unwrap().is_empty());
    }

    #[test]
    fn rejects_injection_and_malformed_specs() {
        for bad in [
            "L 0:host:80",
            "L 8080:host:70000",
            "L 8080:-oProxyCommand=x:80",
            "L 8080:$(id):80",
            "L 8080 :host:80",
            "X 8080",
            "-X 8080",
            "D 1080:host:80",
            "L [::1:8080:host:80",
            "L 1:2:3:4:5",
            "L",
            "L 8080:*:80",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert!(
            parse_list("8080, L 8080:other:9000").is_err(),
            "duplicate local listener"
        );
        assert!(
            parse_list("8080, R 8080:localhost:8080").is_ok(),
            "different sides"
        );
        let many = (1..=33)
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(",");
        assert!(parse_list(&many).is_err());
    }

    #[test]
    fn scanner_recognises_openssh_diagnostics_across_chunks() {
        let mut scanner = NoticeScanner::default();
        let mut notices = scanner.feed(b"bind [127.0.0.1]:8080: Address already in use\r\nchannel_setup_fwd_listener_tcpip: cannot list");
        notices
            .extend(scanner.feed(b"en to port: 8080\r\nCould not request local forwarding.\r\n"));
        notices
            .extend(scanner.feed(b"Warning: remote port forwarding failed for listen port 9000\n"));
        assert_eq!(
            notices,
            vec![
                Notice::LocalListenFailed(8080),
                Notice::RemoteListenFailed(9000)
            ]
        );
        let mut scanner = NoticeScanner::default();
        scanner.feed(&vec![b'x'; NoticeScanner::BUDGET]);
        assert!(scanner.finished());
        assert!(scanner
            .feed(b"\nWarning: remote port forwarding failed for listen port 1\n")
            .is_empty());
    }

    #[test]
    fn probe_distinguishes_free_and_listening_ports() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let forward = Forward::local(port, "127.0.0.1", 80);
        assert_eq!(probe_local(&forward), Probe::InUse);
        drop(listener);
        assert_eq!(probe_local(&forward), Probe::Free);
        let remote = Forward {
            kind: ForwardKind::Remote,
            ..forward
        };
        assert!(matches!(probe_local(&remote), Probe::Unavailable(_)));
    }
}

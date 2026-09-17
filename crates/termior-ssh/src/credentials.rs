//! OS-backed credential storage. There is deliberately no plaintext fallback.
use crate::Profile;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Password,
    Passphrase,
}

fn account(profile: &Profile, kind: Kind) -> String {
    // Bind credentials to the destination, not its editable display name.
    let scope = serde_json::to_vec(&(
        &profile.host,
        &profile.user,
        profile.port,
        &profile.jump_host,
        &profile.identity_file,
        &profile.known_hosts_file,
    ))
    .expect("serializable credential scope");
    format!("{:x}-{kind:?}", Sha256::digest(scope))
}

pub fn same_target(a: &Profile, b: &Profile) -> bool {
    account(a, Kind::Password) == account(b, Kind::Password)
}

pub fn delete_all(profile: &Profile) -> Result<(), String> {
    delete(profile, Kind::Password)?;
    delete(profile, Kind::Passphrase)
}

fn entry(profile: &Profile, kind: Kind) -> Result<keyring::Entry, String> {
    keyring::Entry::new("Termior-ssh", &account(profile, kind))
        .map_err(|_| "系统凭据库不可用；未写入任何明文凭据".into())
}

pub fn save(profile: &Profile, kind: Kind, secret: &str) -> Result<(), String> {
    profile.validate().map_err(|e| e.to_string())?;
    if secret.is_empty() || secret.len() > 1023 || secret.contains(['\0', '\r', '\n']) {
        return Err("凭据不能为空、超过 1023 字节或包含换行及 NUL".into());
    }
    entry(profile, kind)?
        .set_password(secret)
        .map_err(|_| "无法保存到系统凭据库，请检查系统钥匙串是否已解锁".into())
}

pub fn load(profile: &Profile, kind: Kind) -> Result<Option<zeroize::Zeroizing<String>>, String> {
    match entry(profile, kind)?.get_password() {
        Ok(secret) => Ok(Some(zeroize::Zeroizing::new(secret))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => Err("无法读取系统凭据库，请解锁系统钥匙串后重试".into()),
    }
}

pub fn delete(profile: &Profile, kind: Kind) -> Result<(), String> {
    match entry(profile, kind)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err("无法删除系统凭据库中的凭据".into()),
    }
}

/// Only OpenSSH's destination-specific prompts may consume a saved secret.
/// Arbitrary keyboard-interactive/OTP prompts always require user input.
pub fn prompt_kind(profile: &Profile, prompt: &str, user: &str, host: &str) -> Option<Kind> {
    let prompt = prompt.trim();
    if !user.is_empty()
        && (prompt == format!("{user}@{host}'s password:")
            || prompt == format!("{user}@{}'s password:", profile.host))
    {
        Some(Kind::Password)
    } else if !profile.identity_file.is_empty()
        && prompt == format!("Enter passphrase for key '{}':", profile.identity_file)
    {
        Some(Kind::Passphrase)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_askpass_payload_never_reaches_the_vault() {
        let profile = Profile {
            name: "test".into(),
            host: "host".into(),
            ..Default::default()
        };
        for secret in [
            String::new(),
            "line\nbreak".into(),
            "nul\0value".into(),
            "x".repeat(1024),
        ] {
            assert!(save(&profile, Kind::Password, &secret).is_err());
        }
    }
    #[test]
    fn scope_follows_destination_not_label() {
        let p = Profile {
            name: "one".into(),
            host: "host".into(),
            ..Default::default()
        };
        let mut q = p.clone();
        q.name = "two".into();
        assert_eq!(account(&p, Kind::Password), account(&q, Kind::Password));
        q.port = Some(2222);
        assert_ne!(account(&p, Kind::Password), account(&q, Kind::Password));
        assert_ne!(account(&p, Kind::Password), account(&p, Kind::Passphrase));
    }
    #[test]
    fn never_fill_jump_host_otp_or_generic_prompts() {
        let p = Profile {
            host: "target".into(),
            identity_file: "/key".into(),
            ..Default::default()
        };
        assert_eq!(
            prompt_kind(&p, "alice@target's password: ", "alice", "target"),
            Some(Kind::Password)
        );
        assert_eq!(
            prompt_kind(&p, "Enter passphrase for key '/key': ", "alice", "target"),
            Some(Kind::Passphrase)
        );
        for prompt in [
            "alice@bastion's password:",
            "Password:",
            "OTP:",
            "yes/no",
            "Enter passphrase for key '/other':",
        ] {
            assert_eq!(prompt_kind(&p, prompt, "alice", "target"), None);
        }
    }
}

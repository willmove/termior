//! Stable GitHub release updates. Network and disk work must run off the UI thread.
//! Only explicit user action may launch an installer; this module never quits the app.

use reqwest::blocking::Client;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
    time::Duration,
};

pub const RELEASES_URL: &str = "https://github.com/willmove/termior/releases/latest";
const API_URL: &str = "https://api.github.com/repos/willmove/termior/releases/latest";
const DOWNLOAD_PREFIX: &str = "https://github.com/willmove/termior/releases/download/";
const MAX_INSTALLER: u64 = 100 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[derive(Debug, Clone)]
pub struct AvailableUpdate {
    pub version: String,
    name: String,
    url: String,
    checksum_url: String,
    size: u64,
}

pub struct PreparedUpdate {
    pub version: String,
    directory: tempfile::TempDir,
    path: PathBuf,
    digest: [u8; 32],
}

fn client() -> Result<Client, String> {
    Client::builder()
        .user_agent(concat!("Termior/", env!("CARGO_PKG_VERSION")))
        .https_only(true)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url();
            // GitHub serves release assets from its dedicated CDN.
            if attempt.previous().len() >= 5 {
                attempt.error("too many update redirects")
            } else if url.scheme() == "https"
                && matches!(
                    url.host_str(),
                    Some(
                        "github.com"
                            | "release-assets.githubusercontent.com"
                            | "objects.githubusercontent.com"
                    )
                )
            {
                attempt.follow()
            } else {
                attempt.error("untrusted update redirect")
            }
        }))
        .build()
        .map_err(|e| e.to_string())
}

fn read_limited(reader: impl Read, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Update response exceeds the size limit".into());
    }
    Ok(bytes)
}

fn asset_name(version: &Version, os: &str, arch: &str) -> Result<String, String> {
    let (platform, suffix) = match (os, arch) {
        ("windows", "x86_64") => ("windows", "-setup.exe"),
        ("macos", "x86_64" | "aarch64") => ("macos", ".dmg"),
        ("linux", "x86_64" | "aarch64") => ("linux", ".deb"),
        _ => return Err("No installer for this platform; use the release downloads".into()),
    };
    Ok(format!("termior-{version}-{platform}-{arch}{suffix}"))
}

fn select_release(
    release: Release,
    current: &str,
    os: &str,
    arch: &str,
) -> Result<Option<AvailableUpdate>, String> {
    let version = Version::parse(
        release
            .tag_name
            .strip_prefix('v')
            .unwrap_or(&release.tag_name),
    )
    .map_err(|e| format!("Invalid release version: {e}"))?;
    let current = Version::parse(current).map_err(|e| e.to_string())?;
    if release.draft
        || release.prerelease
        || !version.pre.is_empty()
        || version.cmp_precedence(&current).is_le()
    {
        return Ok(None);
    }
    let name = asset_name(&version, os, arch)?;
    let find_asset = |name: &str| -> Result<&Asset, String> {
        let mut matches = release.assets.iter().filter(|asset| asset.name == name);
        let asset = matches
            .next()
            .ok_or_else(|| format!("Release {version} has no {name}; use the release downloads"))?;
        if matches.next().is_some()
            || asset.browser_download_url != format!("{DOWNLOAD_PREFIX}{}/{name}", release.tag_name)
        {
            return Err("Invalid update asset URL or duplicate asset".into());
        }
        Ok(asset)
    };
    let installer = find_asset(&name)?;
    let checksum = find_asset(&format!("{name}.sha256"))?;
    if installer.size == 0 || installer.size > MAX_INSTALLER || checksum.size > 4096 {
        return Err("Invalid update asset size".into());
    }
    Ok(Some(AvailableUpdate {
        version: version.to_string(),
        name,
        url: installer.browser_download_url.clone(),
        checksum_url: checksum.browser_download_url.clone(),
        size: installer.size,
    }))
}

pub fn check() -> Result<Option<AvailableUpdate>, String> {
    check_for(env!("CARGO_PKG_VERSION"))
}

fn check_for(current: &str) -> Result<Option<AvailableUpdate>, String> {
    let response = client()?
        .get(API_URL)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .timeout(Duration::from_secs(30))
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;
    let release = serde_json::from_slice(&read_limited(response, 1024 * 1024)?)
        .map_err(|e| format!("Invalid release response: {e}"))?;
    select_release(
        release,
        current,
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

fn parse_checksum(bytes: &[u8], name: &str) -> Result<[u8; 32], String> {
    let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    let fields: Vec<_> = text.split_whitespace().collect();
    if fields.len() != 2 || fields[0].len() != 64 || fields[1].trim_start_matches('*') != name {
        return Err("Invalid SHA-256 checksum file".into());
    }
    let mut digest = [0; 32];
    for (index, chunk) in fields[0].as_bytes().chunks_exact(2).enumerate() {
        let hex = std::str::from_utf8(chunk).map_err(|e| e.to_string())?;
        digest[index] = u8::from_str_radix(hex, 16).map_err(|_| "Invalid SHA-256 digest")?;
    }
    Ok(digest)
}

fn copy_verified(
    mut reader: impl Read,
    mut writer: impl Write,
    size: u64,
    expected: [u8; 32],
) -> Result<(), String> {
    let mut hash = Sha256::new();
    let mut total = 0;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > size || total > MAX_INSTALLER {
            return Err("Installer exceeds expected size".into());
        }
        hash.update(&buffer[..count]);
        writer
            .write_all(&buffer[..count])
            .map_err(|e| e.to_string())?;
    }
    if total != size || <[u8; 32]>::from(hash.finalize()) != expected {
        return Err("Installer SHA-256 or size mismatch; update rejected".into());
    }
    Ok(())
}

pub fn download(update: AvailableUpdate) -> Result<PreparedUpdate, String> {
    let client = client()?;
    let checksum = client
        .get(&update.checksum_url)
        .timeout(Duration::from_secs(30))
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;
    let digest = parse_checksum(&read_limited(checksum, 4096)?, &update.name)?;
    let directory = tempfile::Builder::new()
        .prefix("termior-update-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let path = directory.path().join(&update.name);
    let mut file = File::create(&path).map_err(|e| e.to_string())?;
    let response = client
        .get(&update.url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;
    copy_verified(response, &mut file, update.size, digest)?;
    file.sync_all().map_err(|e| e.to_string())?;
    Ok(PreparedUpdate {
        version: update.version,
        directory,
        path,
        digest,
    })
}

impl PreparedUpdate {
    /// Recheck immediately before dispatching to the native installer. Keep its directory
    /// after handoff: the installer may still need it after Termior exits.
    pub fn launch(self) -> Result<PathBuf, String> {
        let file = File::open(&self.path).map_err(|e| e.to_string())?;
        let size = file.metadata().map_err(|e| e.to_string())?.len();
        copy_verified(file, std::io::sink(), size, self.digest)?;
        super::open_local_file(&self.path).map_err(|e| e.to_string())?;
        let _ = self.directory.keep();
        Ok(self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Explicit network smoke test; downloads and verifies, never launches an installer.
    #[test]
    #[ignore = "requires GitHub access and a published installer for this platform"]
    fn live_release_download() {
        let update = check_for("0.0.0")
            .unwrap()
            .expect("published stable release");
        let prepared = download(update).unwrap();
        assert!(prepared.path.is_file());
        let path = prepared.path.clone();
        drop(prepared);
        assert!(!path.exists());
    }

    fn release(tag: &str) -> Release {
        let version = tag.trim_start_matches('v');
        let name = format!("termior-{version}-windows-x86_64-setup.exe");
        Release {
            tag_name: tag.into(),
            draft: false,
            prerelease: false,
            assets: [name.clone(), format!("{name}.sha256")]
                .into_iter()
                .map(|name| Asset {
                    browser_download_url: format!("{DOWNLOAD_PREFIX}{tag}/{name}"),
                    name,
                    size: 100,
                })
                .collect(),
        }
    }

    #[test]
    fn stable_semver_ordering_and_no_downgrades() {
        for tag in ["v0.1.6", "v0.1.5", "v0.1.7-beta.1", "v0.1.6+build.7"] {
            assert!(select_release(release(tag), "0.1.6", "windows", "x86_64")
                .unwrap()
                .is_none());
        }
        assert!(
            select_release(release("v0.1.10"), "0.1.9", "windows", "x86_64")
                .unwrap()
                .is_some()
        );
        for draft in [true, false] {
            let mut r = release("v0.2.0");
            r.draft = draft;
            r.prerelease = !draft;
            assert!(select_release(r, "0.1.6", "windows", "x86_64")
                .unwrap()
                .is_none());
        }
    }

    #[test]
    fn asset_contract_rejects_wrong_platform_missing_checksum_and_untrusted_url() {
        assert!(select_release(release("v0.2.0"), "0.1.6", "macos", "aarch64").is_err());
        let mut r = release("v0.2.0");
        r.assets.pop();
        assert!(select_release(r, "0.1.6", "windows", "x86_64").is_err());
        let mut r = release("v0.2.0");
        r.assets[0].browser_download_url = "https://evil.example/setup.exe".into();
        assert!(select_release(r, "0.1.6", "windows", "x86_64").is_err());
        let v = Version::parse("0.2.0").unwrap();
        assert_eq!(
            asset_name(&v, "macos", "aarch64").unwrap(),
            "termior-0.2.0-macos-aarch64.dmg"
        );
        assert_eq!(
            asset_name(&v, "linux", "x86_64").unwrap(),
            "termior-0.2.0-linux-x86_64.deb"
        );
        assert!(asset_name(&v, "windows", "aarch64").is_err());
    }

    #[test]
    fn rejects_corruption_truncation_oversize_and_wrong_checksum_filename() {
        let data = b"installer fixture";
        let digest: [u8; 32] = Sha256::digest(data).into();
        assert!(copy_verified(&data[..], Vec::new(), data.len() as u64, digest).is_ok());
        assert!(copy_verified(&data[..], Vec::new(), data.len() as u64 + 1, digest).is_err());
        assert!(copy_verified(&data[..], Vec::new(), 1, digest).is_err());
        assert!(copy_verified(&data[..], Vec::new(), data.len() as u64, [0; 32]).is_err());
        let checksum = format!("{:x}  setup.exe\n", Sha256::digest(data));
        assert_eq!(
            parse_checksum(checksum.as_bytes(), "setup.exe").unwrap(),
            digest
        );
        assert!(parse_checksum(checksum.as_bytes(), "other.exe").is_err());
        assert!(parse_checksum(b"invalid  setup.exe", "setup.exe").is_err());
        assert!(read_limited(&data[..], 1).is_err());
    }

    #[test]
    fn modified_staged_installer_is_rejected_and_cleaned_before_launch() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("setup.exe");
        std::fs::write(&path, b"tampered").unwrap();
        let prepared = PreparedUpdate {
            version: "1.0.0".into(),
            directory,
            path: path.clone(),
            digest: Sha256::digest(b"original").into(),
        };
        assert!(prepared.launch().unwrap_err().contains("mismatch"));
        assert!(!path.exists());
    }
}

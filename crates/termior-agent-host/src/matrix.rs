//! User-visible capability matrix for direct, worktree, sandboxed, and external backends
//! (Stage D 8.2). Sandboxed trust words come from [`PlatformSandbox::probe`]. A probe never
//! raises the pinned permission ceiling.

use crate::environment::{PlatformSandbox, SandboxBackend, SandboxCapability, TrustLevel};
use std::collections::BTreeMap;
use termior_ai::{EnvironmentProfile, PinnedEnvironment};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityMatrixRow {
    pub backend: &'static str,
    pub host: String,
    pub root: String,
    pub file: String,
    pub network: String,
    pub credentials: String,
    pub process: String,
}

pub fn capability_matrix() -> Vec<CapabilityMatrixRow> {
    let host = std::env::consts::OS.to_owned();
    let direct = PinnedEnvironment::display_only(EnvironmentProfile::Direct, "workspace");
    let worktree = PinnedEnvironment::display_only(EnvironmentProfile::Worktree, "worktree path");
    let mut sandboxed = PinnedEnvironment::display_only(
        EnvironmentProfile::Sandboxed {
            allow_network: false,
        },
        "workspace",
    );
    apply_sandbox_probe(&mut sandboxed, &PlatformSandbox.probe());
    let external =
        PinnedEnvironment::display_only(EnvironmentProfile::external("external"), "workspace");
    vec![
        row("direct", &host, &direct),
        row("worktree", &host, &worktree),
        row("sandboxed", &host, &sandboxed),
        row("external", &host, &external),
    ]
}

fn row(backend: &'static str, host: &str, pin: &PinnedEnvironment) -> CapabilityMatrixRow {
    CapabilityMatrixRow {
        backend,
        host: host.to_owned(),
        root: pin.root.display().to_string(),
        file: pin.file.clone(),
        network: pin.network.clone(),
        credentials: pin.credentials.clone(),
        process: pin.process.clone(),
    }
}

/// Copy probe trust onto a sandboxed pin. Ceiling words and the grant stay as pinned.
pub fn apply_sandbox_probe(
    pin: &mut PinnedEnvironment,
    probe: &BTreeMap<SandboxCapability, TrustLevel>,
) {
    if !matches!(pin.profile, EnvironmentProfile::Sandboxed { .. }) {
        return;
    }
    let grant = pin.grant.clone();
    pin.relabel_trust(
        trust_word(probed(probe, SandboxCapability::Filesystem)),
        trust_word(probed(probe, SandboxCapability::Network)),
        trust_word(probed(probe, SandboxCapability::Credentials)),
        trust_word(probed(probe, SandboxCapability::ProcessTree)),
    );
    debug_assert_eq!(pin.grant, grant);
}

fn probed(
    probe: &BTreeMap<SandboxCapability, TrustLevel>,
    capability: SandboxCapability,
) -> TrustLevel {
    probe
        .get(&capability)
        .copied()
        .unwrap_or(TrustLevel::Unsupported)
}

fn trust_word(level: TrustLevel) -> &'static str {
    match level {
        TrustLevel::Verified => "verified",
        TrustLevel::Reported => "reported",
        TrustLevel::Unknown => "unknown",
        TrustLevel::Unsupported => "unsupported",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::SandboxBackend;

    #[test]
    fn matrix_covers_four_backends_and_external_is_never_verified() {
        let rows = capability_matrix();
        assert_eq!(
            rows.iter().map(|row| row.backend).collect::<Vec<_>>(),
            vec!["direct", "worktree", "sandboxed", "external"]
        );
        let external = rows.iter().find(|row| row.backend == "external").unwrap();
        for cell in [
            &external.file,
            &external.network,
            &external.credentials,
            &external.process,
        ] {
            assert!(!cell.contains("verified"), "{cell}");
        }
        let worktree = rows.iter().find(|row| row.backend == "worktree").unwrap();
        assert!(worktree.file.starts_with("not-provided"));
        assert!(worktree.network.starts_with("not-provided"));
        assert_eq!(worktree.root, "worktree path");
    }

    #[test]
    fn sandboxed_row_follows_the_platform_probe_without_widening_a_blocked_network() {
        let probe = PlatformSandbox.probe();
        let rows = capability_matrix();
        let sandboxed = rows.iter().find(|row| row.backend == "sandboxed").unwrap();
        assert!(sandboxed
            .file
            .ends_with(trust_word(probed(&probe, SandboxCapability::Filesystem))));
        assert!(sandboxed
            .network
            .ends_with(trust_word(probed(&probe, SandboxCapability::Network))));
        assert!(sandboxed
            .credentials
            .ends_with(trust_word(probed(&probe, SandboxCapability::Credentials))));
        assert!(sandboxed
            .process
            .ends_with(trust_word(probed(&probe, SandboxCapability::ProcessTree))));
        assert!(sandboxed.network.starts_with("blocked/"));
        assert!(sandboxed.credentials.starts_with("cleared/"));
        let mut pin = PinnedEnvironment::pin(
            EnvironmentProfile::Sandboxed {
                allow_network: false,
            },
            "/ws",
        );
        let grant = pin.grant.clone();
        apply_sandbox_probe(&mut pin, &probe);
        assert_eq!(pin.grant, grant);
        assert_eq!(pin.grant.network, termior_ai::AccessGrant::None);
    }
}

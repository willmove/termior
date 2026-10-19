//! Pinned execution environments (Stage D 5.2).
//!
//! A task records one environment id and the permission ceiling that id was created with.
//! Restoring a task returns that same id and ceiling. A different profile allocates a new id.
//! Nothing in this module mutates an existing id into a wider grant.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

static NEXT_ENVIRONMENT: AtomicU64 = AtomicU64::new(1);

fn allocate_id(profile: &EnvironmentProfile) -> String {
    let sequence = NEXT_ENVIRONMENT.fetch_add(1, Ordering::Relaxed);
    format!("{}-{sequence:x}", profile.slug())
}

/// Permission ceiling. A higher rank is a wider grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccessGrant {
    /// No access on this dimension (blocked network, cleared credentials).
    None,
    /// Access limited to the environment root.
    Confined,
    /// Current-user or backend-declared access.
    Host,
}

impl AccessGrant {
    fn rank(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Confined => 1,
            Self::Host => 2,
        }
    }

    pub fn widens(self, previous: Self) -> bool {
        self.rank() > previous.rank()
    }
}

/// Frozen ceilings for file, network, credential, and process access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentGrant {
    pub filesystem: AccessGrant,
    pub network: AccessGrant,
    pub credentials: AccessGrant,
    pub process: AccessGrant,
}

impl EnvironmentGrant {
    pub fn widens(&self, previous: &Self) -> bool {
        self.filesystem.widens(previous.filesystem)
            || self.network.widens(previous.network)
            || self.credentials.widens(previous.credentials)
            || self.process.widens(previous.process)
    }

    fn host() -> Self {
        Self {
            filesystem: AccessGrant::Host,
            network: AccessGrant::Host,
            credentials: AccessGrant::Host,
            process: AccessGrant::Host,
        }
    }

    fn sandbox(allow_network: bool) -> Self {
        Self {
            filesystem: AccessGrant::Confined,
            network: if allow_network {
                AccessGrant::Host
            } else {
                AccessGrant::None
            },
            credentials: AccessGrant::None,
            process: AccessGrant::Confined,
        }
    }
}

/// Profile selected for an environment. Equality ignores display text and ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum EnvironmentProfile {
    Direct,
    Worktree,
    Sandboxed { allow_network: bool },
    External { backend: String },
}

impl EnvironmentProfile {
    pub fn external(backend: impl Into<String>) -> Self {
        Self::External {
            backend: backend.into(),
        }
    }

    pub fn slug(&self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Worktree => "worktree",
            Self::Sandboxed { .. } => "sandboxed",
            Self::External { .. } => "external",
        }
    }

    fn grant(&self) -> EnvironmentGrant {
        match self {
            Self::Direct | Self::Worktree | Self::External { .. } => EnvironmentGrant::host(),
            Self::Sandboxed { allow_network } => EnvironmentGrant::sandbox(*allow_network),
        }
    }

    /// User-visible capability cells. The left side is the ceiling; the right side is trust.
    /// Sandbox trust starts as `unprobed` until the host probe replaces only that word.
    fn cells(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        match self {
            Self::Direct => (
                "workspace-confined/reported",
                "provider-tool-policy/unknown",
                "keyring-references/unknown",
                "local-session-tree/verified",
            ),
            Self::Worktree => (
                "not-provided/unsupported",
                "not-provided/unsupported",
                "keyring-references/unknown",
                "local-session-tree/verified",
            ),
            Self::Sandboxed { allow_network } => (
                "confined/unprobed",
                if *allow_network {
                    "host/unprobed"
                } else {
                    "blocked/unprobed"
                },
                "cleared/unprobed",
                "isolated/unprobed",
            ),
            Self::External { .. } => (
                "workspace-write/reported",
                "backend-declared/reported",
                "backend-managed/reported",
                "backend-declared/unknown",
            ),
        }
    }
}

/// One environment id and the permissions that id is allowed to keep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedEnvironment {
    pub id: String,
    pub profile: EnvironmentProfile,
    pub host: String,
    pub root: PathBuf,
    pub grant: EnvironmentGrant,
    pub file: String,
    pub network: String,
    pub credentials: String,
    pub process: String,
}

impl PinnedEnvironment {
    pub fn pin(profile: EnvironmentProfile, root: impl Into<PathBuf>) -> Self {
        Self::from_parts(allocate_id(&profile), profile, root.into())
    }

    /// Pin a caller-chosen id. Worktree environments keep the id allocated with the worktree.
    pub fn with_id(
        id: impl Into<String>,
        profile: EnvironmentProfile,
        root: impl Into<PathBuf>,
    ) -> Self {
        Self::from_parts(id.into(), profile, root.into())
    }

    /// Header preview before a task exists. This id is not a persisted pin.
    pub fn display_only(profile: EnvironmentProfile, root: impl Into<PathBuf>) -> Self {
        Self::from_parts(format!("preview-{}", profile.slug()), profile, root.into())
    }

    fn from_parts(id: String, profile: EnvironmentProfile, root: PathBuf) -> Self {
        let (file, network, credentials, process) = profile.cells();
        Self {
            id,
            grant: profile.grant(),
            profile,
            host: std::env::consts::OS.to_owned(),
            root,
            file: file.to_owned(),
            network: network.to_owned(),
            credentials: credentials.to_owned(),
            process: process.to_owned(),
        }
    }

    /// Replace only the trust word. The ceiling word and [`EnvironmentGrant`] stay put,
    /// so a probe cannot turn a blocked sandbox into host access.
    pub fn relabel_trust(&mut self, file: &str, network: &str, credentials: &str, process: &str) {
        self.file = relabel_trust(&self.file, file);
        self.network = relabel_trust(&self.network, network);
        self.credentials = relabel_trust(&self.credentials, credentials);
        self.process = relabel_trust(&self.process, process);
    }
}

fn relabel_trust(label: &str, trust: &str) -> String {
    let ceiling = label.split('/').next().unwrap_or(label);
    format!("{ceiling}/{trust}")
}

/// Environments keyed by id. Entries are inserted and replaced only by allocating a new id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EnvironmentPins {
    #[serde(default)]
    entries: BTreeMap<String, PinnedEnvironment>,
}

impl EnvironmentPins {
    pub fn pin_new(
        &mut self,
        profile: EnvironmentProfile,
        root: impl Into<PathBuf>,
    ) -> PinnedEnvironment {
        let pin = PinnedEnvironment::pin(profile, root);
        self.entries.insert(pin.id.clone(), pin.clone());
        pin
    }

    pub fn get(&self, id: &str) -> Option<&PinnedEnvironment> {
        self.entries.get(id)
    }

    /// Re-attach a pin that was stored with a task.
    ///
    /// The same id and the same record succeed. A different record for that id is refused,
    /// and a wider grant is reported separately from any other in-place edit.
    pub fn restore_pin(&mut self, pin: PinnedEnvironment) -> Result<(), PinError> {
        if let Some(existing) = self.entries.get(&pin.id) {
            if existing == &pin {
                return Ok(());
            }
            if pin.grant.widens(&existing.grant) {
                return Err(PinError::WouldWiden { id: pin.id });
            }
            return Err(PinError::InPlaceChangeForbidden { id: pin.id });
        }
        self.entries.insert(pin.id.clone(), pin);
        Ok(())
    }

    /// A different profile or root allocates a new environment. The previous id is unchanged.
    pub fn change_profile(
        &mut self,
        id: &str,
        profile: EnvironmentProfile,
        root: impl Into<PathBuf>,
    ) -> Result<PinnedEnvironment, PinError> {
        let root = root.into();
        let current = self
            .entries
            .get(id)
            .cloned()
            .ok_or_else(|| PinError::Missing(id.to_owned()))?;
        if current.profile == profile && current.root == root {
            return Ok(current);
        }
        let created = PinnedEnvironment::pin(profile, root);
        self.entries.insert(created.id.clone(), created.clone());
        Ok(created)
    }

    /// Reject every in-place grant edit. A wider grant is [`PinError::WouldWiden`].
    pub fn try_apply_grant(&mut self, id: &str, grant: EnvironmentGrant) -> Result<(), PinError> {
        let current = self
            .entries
            .get(id)
            .ok_or_else(|| PinError::Missing(id.to_owned()))?;
        if grant.widens(&current.grant) {
            return Err(PinError::WouldWiden { id: id.to_owned() });
        }
        if grant != current.grant {
            return Err(PinError::InPlaceChangeForbidden { id: id.to_owned() });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PinError {
    #[error("environment {0} is not pinned")]
    Missing(String),
    #[error("refusing to widen environment {id} in place")]
    WouldWiden { id: String },
    #[error("refusing to change environment {id} in place; create a new environment")]
    InPlaceChangeForbidden { id: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_keeps_the_pinned_id_and_refuses_a_wider_grant() {
        let mut pins = EnvironmentPins::default();
        let pin = pins.pin_new(
            EnvironmentProfile::Sandboxed {
                allow_network: false,
            },
            "/ws",
        );
        let json = serde_json::to_string(&pins).unwrap();
        let mut restored: EnvironmentPins = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.get(&pin.id).unwrap(), &pin);

        restored.restore_pin(pin.clone()).unwrap();
        assert_eq!(restored.get(&pin.id).unwrap().grant, pin.grant);

        let mut wider = pin.clone();
        wider.grant.network = AccessGrant::Host;
        wider.network = "host/unprobed".into();
        assert_eq!(
            restored.restore_pin(wider),
            Err(PinError::WouldWiden { id: pin.id.clone() })
        );
        assert_eq!(
            restored.get(&pin.id).unwrap().grant.network,
            AccessGrant::None
        );
    }

    #[test]
    fn profile_change_allocates_a_new_environment_and_leaves_the_old_grant() {
        let mut pins = EnvironmentPins::default();
        let pinned = pins.pin_new(
            EnvironmentProfile::Sandboxed {
                allow_network: false,
            },
            "/ws",
        );
        let original = pinned.grant.clone();

        let mut opened = original.clone();
        opened.network = AccessGrant::Host;
        assert!(matches!(
            pins.try_apply_grant(&pinned.id, opened),
            Err(PinError::WouldWiden { .. })
        ));
        assert_eq!(pins.get(&pinned.id).unwrap().grant, original);

        let created = pins
            .change_profile(&pinned.id, EnvironmentProfile::Direct, "/ws")
            .unwrap();
        assert_ne!(created.id, pinned.id);
        assert!(created.grant.widens(&original));
        assert_eq!(pins.get(&pinned.id).unwrap().grant, original);
        assert_eq!(pins.get(&created.id).unwrap(), &created);
    }

    #[test]
    fn same_profile_and_root_keeps_the_id_and_narrowing_in_place_is_refused() {
        let mut pins = EnvironmentPins::default();
        let pinned = pins.pin_new(EnvironmentProfile::Direct, "/ws");
        let again = pins
            .change_profile(&pinned.id, EnvironmentProfile::Direct, "/ws")
            .unwrap();
        assert_eq!(again.id, pinned.id);

        let mut narrower = pinned.grant.clone();
        narrower.network = AccessGrant::None;
        assert!(matches!(
            pins.try_apply_grant(&pinned.id, narrower),
            Err(PinError::InPlaceChangeForbidden { .. })
        ));
        assert_eq!(
            pins.get(&pinned.id).unwrap().grant.network,
            AccessGrant::Host
        );
    }

    #[test]
    fn allowing_network_is_a_new_sandbox_not_an_edit_of_the_blocked_one() {
        let mut pins = EnvironmentPins::default();
        let blocked = pins.pin_new(
            EnvironmentProfile::Sandboxed {
                allow_network: false,
            },
            "/ws",
        );
        let opened = pins
            .change_profile(
                &blocked.id,
                EnvironmentProfile::Sandboxed {
                    allow_network: true,
                },
                "/ws",
            )
            .unwrap();
        assert_ne!(opened.id, blocked.id);
        assert_eq!(
            pins.get(&blocked.id).unwrap().grant.network,
            AccessGrant::None
        );
        assert_eq!(opened.grant.network, AccessGrant::Host);
    }

    #[test]
    fn worktree_does_not_claim_file_or_network_isolation_and_external_is_not_verified() {
        let worktree = PinnedEnvironment::display_only(EnvironmentProfile::Worktree, "/wt");
        assert!(worktree.file.starts_with("not-provided"));
        assert!(worktree.network.starts_with("not-provided"));
        let external =
            PinnedEnvironment::display_only(EnvironmentProfile::external("codex"), "/ws");
        for cell in [
            &external.file,
            &external.network,
            &external.credentials,
            &external.process,
        ] {
            assert!(!cell.contains("verified"), "{cell}");
        }
    }

    #[test]
    fn relabel_trust_does_not_raise_the_grant() {
        let mut pin = PinnedEnvironment::pin(
            EnvironmentProfile::Sandboxed {
                allow_network: false,
            },
            "/ws",
        );
        let grant = pin.grant.clone();
        pin.relabel_trust("verified", "verified", "verified", "verified");
        assert_eq!(pin.grant, grant);
        assert_eq!(pin.network, "blocked/verified");
        assert_eq!(pin.grant.network, AccessGrant::None);
    }
}

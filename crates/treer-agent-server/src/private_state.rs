//! Controller-owned per-Agent private state directories and their quota.
//!
//! Layout: `<Treer state dir>/agent-private/<server_id>/<agent_id>`. The three
//! Treer levels are created with mode 0700 (Windows: the protected current-user
//! DACL) and re-verified before every use: a directory, not a symlink or
//! reparse point, owned by the Controller user and closed to group/others. The
//! canonical ancestors above `agent-private` must be directories owned by the
//! Controller user or root and must not be writable by others unless sticky.
//! The path derives only from the Controller's own server ID and the stable
//! Agent ID; callers, requests, profiles and inherited environments never
//! supply it.
//!
//! Directories survive stop, restart, recovery and Controller replacement and
//! are removed only by an explicit purge after the Proxy confirms permanent
//! deletion (`deleted_agents` tombstone). The Controller never reads file
//! content below an Agent directory; it only sizes and deletes its own
//! namespace. This is a same-user privacy boundary (see docs/security.md), not
//! operating-system isolation, and the quota is cooperative admission control.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use tracing::warn;
use treer_protocol::ProtocolError;
use uuid::Uuid;

/// Injected into every Agent/App process start by the Controller.
pub const ENVIRONMENT_VARIABLE: &str = "TREER_AGENT_PRIVATE_STATE_DIR";
/// Directory name below the Treer state directory.
pub const ROOT_DIRECTORY_NAME: &str = "agent-private";
const MAX_IDENTITY_LENGTH: usize = 128;
/// Entries walked per Agent directory before the measurement is treated as
/// over quota. Bounds Controller work on a runaway directory.
pub const MAX_MEASURED_ENTRIES: usize = 65_536;

/// Stable identities that may become a single path component. Matches the
/// Agent reservation fence rule: no dots, separators, whitespace or controls.
pub fn valid_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTITY_LENGTH
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub fn invalid_identity() -> ProtocolError {
    ProtocolError::new(
        "agent_private_state_invalid_identity",
        "a stable server and Agent identity is required for private state",
    )
}

fn unavailable(path: &Path, error: io::Error) -> ProtocolError {
    warn!(path = %path.display(), %error, "Agent private state directory is unavailable");
    ProtocolError::new(
        "agent_private_state_unavailable",
        "Agent private state directory could not be prepared; inspect the Controller log",
    )
}

fn unsafe_directory(path: &Path, reason: &str) -> ProtocolError {
    warn!(path = %path.display(), reason, "Agent private state directory failed its safety check");
    ProtocolError::new(
        "agent_private_state_unsafe",
        "Agent private state directory failed its safety check; inspect the Controller log",
    )
}

/// Disk usage of one Agent directory as the Controller measured it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Measurement {
    pub bytes: u64,
    pub entries: usize,
    /// The walk stopped at `MAX_MEASURED_ENTRIES`; treat as over quota.
    pub overflow: bool,
}

#[derive(Debug)]
pub struct PrivateStateRoot {
    /// Canonical `agent-private` directory.
    root: PathBuf,
    server: PathBuf,
}

impl PrivateStateRoot {
    /// `root` is the Controller-derived `agent-private` directory. Its parent
    /// (the Treer state directory) must exist or be creatable; the root and the
    /// server level are created and verified now, so the root exists for
    /// file-transfer protection before any Agent starts.
    pub fn open(root: &Path, server_id: &str) -> Result<Self, ProtocolError> {
        if !root.is_absolute() {
            return Err(ProtocolError::new(
                "agent_private_state_unavailable",
                "Agent private state root must be an absolute path",
            ));
        }
        if !valid_identity(server_id) {
            return Err(invalid_identity());
        }
        let (parent, name) = match (root.parent(), root.file_name()) {
            (Some(parent), Some(name)) => (parent, name),
            _ => {
                return Err(ProtocolError::new(
                    "agent_private_state_unavailable",
                    "Agent private state root has no parent directory",
                ))
            }
        };
        fs::create_dir_all(parent).map_err(|error| unavailable(parent, error))?;
        let parent = fs::canonicalize(parent).map_err(|error| unavailable(parent, error))?;
        let root = parent.join(name);
        let state = Self {
            server: root.join(server_id),
            root,
        };
        state.verify_levels()?;
        Ok(state)
    }

    #[cfg(test)]
    pub fn server_directory(&self) -> &Path {
        &self.server
    }

    /// Path only; nothing is touched on disk.
    pub fn agent_directory(&self, agent_id: &str) -> Result<PathBuf, ProtocolError> {
        if !valid_identity(agent_id) {
            return Err(invalid_identity());
        }
        Ok(self.server.join(agent_id))
    }

    /// Ancestors above the root, then the root and server levels. Creates the
    /// two Treer levels when absent.
    fn verify_levels(&self) -> Result<(), ProtocolError> {
        verify_ancestors(&self.root)?;
        ensure_private_directory(&self.root)?;
        ensure_private_directory(&self.server)
    }

    /// Create or re-verify the Agent directory and every level above it. Any
    /// unsafe level fails closed; nothing below the Agent directory is read.
    pub fn prepare(&self, agent_id: &str) -> Result<PathBuf, ProtocolError> {
        let directory = self.agent_directory(agent_id)?;
        self.verify_levels()?;
        ensure_private_directory(&directory)?;
        Ok(directory)
    }

    /// Re-verify an existing Agent directory without creating anything. Quota
    /// operations use this so a purged or never-started Agent cannot recreate
    /// its directory through the local API.
    pub fn verify_existing(&self, agent_id: &str) -> Result<PathBuf, ProtocolError> {
        let directory = self.agent_directory(agent_id)?;
        self.verify_levels()?;
        match fs::symlink_metadata(&directory) {
            Ok(metadata) => verify_private_directory(&directory, &metadata)?,
            Err(error) => return Err(unavailable(&directory, error)),
        }
        Ok(directory)
    }

    /// Agent directories currently present under this server. Only names that
    /// are valid identities and real (non-link) directories are reported.
    pub fn agent_ids(&self) -> io::Result<Vec<String>> {
        let mut ids = Vec::new();
        for entry in fs::read_dir(&self.server)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let file_type = entry.file_type()?;
            if valid_identity(&name) && file_type.is_dir() && !file_type.is_symlink() {
                ids.push(name);
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Sum of the sizes below an existing Agent directory. Symlinks count by
    /// their own size and are never followed; the walk is bounded.
    pub fn measure(&self, agent_id: &str) -> Result<Measurement, ProtocolError> {
        let directory = self.verify_existing(agent_id)?;
        measure_directory(&directory).map_err(|error| unavailable(&directory, error))
    }

    /// Remove an Agent directory after the Proxy confirmed permanent deletion.
    /// Returns false when nothing was present. The directory is re-checked
    /// before and after removal; a link at the Agent level is never followed.
    pub fn purge(&self, agent_id: &str) -> Result<bool, ProtocolError> {
        let directory = self.agent_directory(agent_id)?;
        verify_ancestors(&self.root)?;
        for level in [&self.root, &self.server] {
            match fs::symlink_metadata(level) {
                Ok(metadata) => verify_private_directory(level, &metadata)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(unavailable(level, error)),
            }
        }
        match fs::symlink_metadata(&directory) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(unsafe_directory(&directory, "not_a_private_directory"));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(unavailable(&directory, error)),
        }
        // std::fs::remove_dir_all does not follow symbolic links: a link below
        // the Agent directory is unlinked and its target is left untouched.
        fs::remove_dir_all(&directory).map_err(|error| unavailable(&directory, error))?;
        match fs::symlink_metadata(&directory) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
            Ok(_) => Err(unavailable(
                &directory,
                io::Error::other("directory still present after removal"),
            )),
            Err(error) => Err(unavailable(&directory, error)),
        }
    }
}

fn measure_directory(directory: &Path) -> io::Result<Measurement> {
    let mut pending = vec![directory.to_path_buf()];
    let mut measurement = Measurement {
        bytes: 0,
        entries: 0,
        overflow: false,
    };
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current)? {
            let entry = entry?;
            measurement.entries += 1;
            if measurement.entries > MAX_MEASURED_ENTRIES {
                measurement.overflow = true;
                return Ok(measurement);
            }
            // DirEntry::file_type/metadata do not follow symbolic links.
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                pending.push(entry.path());
            } else {
                measurement.bytes = measurement.bytes.saturating_add(entry.metadata()?.len());
            }
        }
    }
    Ok(measurement)
}

/// Canonical ancestors of the root (exclusive). Each must be a real directory
/// owned by the Controller user or root and must not be writable by others
/// unless the sticky bit prevents renaming foreign entries (for example /tmp).
fn verify_ancestors(root: &Path) -> Result<(), ProtocolError> {
    let mut ancestors: Vec<&Path> = root.ancestors().skip(1).collect();
    ancestors.reverse();
    for ancestor in ancestors {
        let metadata =
            fs::symlink_metadata(ancestor).map_err(|error| unavailable(ancestor, error))?;
        if metadata.file_type().is_symlink() {
            return Err(unsafe_directory(ancestor, "ancestor_symlink"));
        }
        if !metadata.is_dir() {
            return Err(unsafe_directory(ancestor, "ancestor_not_a_directory"));
        }
        #[cfg(windows)]
        if is_reparse_point(&metadata) {
            return Err(unsafe_directory(ancestor, "ancestor_reparse_point"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let uid = metadata.uid();
            if uid != nix::unistd::geteuid().as_raw() && uid != 0 {
                return Err(unsafe_directory(ancestor, "ancestor_foreign_owner"));
            }
            let mode = metadata.permissions().mode();
            if mode & 0o022 != 0 && mode & 0o1000 == 0 {
                return Err(unsafe_directory(ancestor, "ancestor_writable_by_others"));
            }
        }
    }
    Ok(())
}

fn ensure_private_directory(path: &Path) -> Result<(), ProtocolError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => verify_private_directory(path, &metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match create_private_directory(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(unavailable(path, error)),
            }
            // Re-check after creation: a concurrent same-user writer may have
            // placed a link or a foreign directory at this name meanwhile.
            let metadata = fs::symlink_metadata(path).map_err(|error| unavailable(path, error))?;
            verify_private_directory(path, &metadata)
        }
        Err(error) => Err(unavailable(path, error)),
    }
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path)?;
        treer_platform::secure_directory(path)
    }
}

#[cfg(windows)]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

fn verify_private_directory(path: &Path, metadata: &fs::Metadata) -> Result<(), ProtocolError> {
    if metadata.file_type().is_symlink() {
        return Err(unsafe_directory(path, "symlink"));
    }
    if !metadata.is_dir() {
        return Err(unsafe_directory(path, "not_a_directory"));
    }
    #[cfg(windows)]
    {
        if is_reparse_point(metadata) {
            return Err(unsafe_directory(path, "reparse_point"));
        }
        // Existing directories (and later DACL changes) are re-read, not
        // trusted from the creation branch: owner must be the current user and
        // the protected DACL may only allow the current user and SYSTEM.
        match treer_platform::private_path_is_secure(path) {
            Ok(true) => {}
            Ok(false) => {
                // SIDs, control flags and masks only: enough to diagnose a
                // foreign owner or inherited entry without exposing content.
                let report = treer_platform::private_path_security_report(path)
                    .unwrap_or_else(|error| format!("report unavailable: {error}"));
                warn!(path = %path.display(), %report, "Agent private state directory DACL is not private");
                return Err(unsafe_directory(path, "insecure_dacl"));
            }
            Err(error) => return Err(unavailable(path, error)),
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != nix::unistd::geteuid().as_raw() {
            return Err(unsafe_directory(path, "foreign_owner"));
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(unsafe_directory(path, "group_or_world_accessible"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Quota ledger: Controller-enforced admission for private state writes.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaLimits {
    pub agent_bytes: u64,
    pub global_bytes: u64,
    pub reservation_bytes: u64,
    pub reservations_per_agent: usize,
    pub reservation_ttl: Duration,
    /// Refuse new reservations when the volume would drop below this.
    pub free_space_floor_bytes: u64,
}

impl Default for QuotaLimits {
    fn default() -> Self {
        Self {
            agent_bytes: 256 * 1024 * 1024,
            global_bytes: 4 * 1024 * 1024 * 1024,
            reservation_bytes: 64 * 1024 * 1024,
            reservations_per_agent: 16,
            reservation_ttl: Duration::from_secs(15 * 60),
            free_space_floor_bytes: 1024 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    pub id: String,
    pub agent_id: String,
    pub bytes: u64,
    expires_at: Instant,
}

/// Usage reported to the owning Agent only. `available_bytes` folds the
/// global headroom in without disclosing other Agents' usage.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentUsage {
    pub used_bytes: u64,
    pub agent_quota_bytes: u64,
    pub available_bytes: u64,
    pub reservation_ids: Vec<String>,
}

#[derive(Debug, Default)]
pub struct QuotaLedger {
    limits: QuotaLimits,
    /// False until every Agent directory under the server was enumerated and
    /// measured once; an incomplete ledger refuses new reservations rather
    /// than treating unknown usage as zero.
    inventory_incomplete: bool,
    /// Last Controller measurement per Agent; `None` means the directory could
    /// not be measured (that Agent is refused; it counts as zero globally).
    measured: HashMap<String, Option<u64>>,
    reservations: BTreeMap<String, Reservation>,
    /// Agents removed after confirmed deletion; refused until re-created.
    purged: HashSet<String>,
}

fn reservation_expired() -> ProtocolError {
    ProtocolError::new(
        "private_state_reservation_expired",
        "the private state reservation is unknown, expired or not owned by this Agent",
    )
}

impl QuotaLedger {
    pub fn new(limits: QuotaLimits) -> Self {
        Self {
            limits,
            ..Self::default()
        }
    }

    pub fn limits(&self) -> QuotaLimits {
        self.limits
    }

    pub fn mark_inventory_incomplete(&mut self) {
        self.inventory_incomplete = true;
    }

    pub fn inventory_incomplete(&self) -> bool {
        self.inventory_incomplete
    }

    /// Replace the whole measured inventory after a successful enumeration.
    pub fn replace_inventory(&mut self, measured: Vec<(String, Option<Measurement>)>) {
        self.measured.clear();
        for (agent_id, measurement) in measured {
            self.record(&agent_id, measurement);
        }
        self.inventory_incomplete = false;
    }

    #[cfg(test)]
    pub fn set_limits(&mut self, limits: QuotaLimits) {
        self.limits = limits;
    }

    pub fn record(&mut self, agent_id: &str, measurement: Option<Measurement>) {
        let bytes = measurement.map(|measurement| {
            if measurement.overflow {
                u64::MAX
            } else {
                measurement.bytes
            }
        });
        self.purged.remove(agent_id);
        self.measured.insert(agent_id.to_owned(), bytes);
    }

    /// Confirmed deletion: drop accounting and refuse the identity afterwards.
    pub fn forget(&mut self, agent_id: &str) {
        self.measured.remove(agent_id);
        self.reservations
            .retain(|_, reservation| reservation.agent_id != agent_id);
        self.purged.insert(agent_id.to_owned());
    }

    pub fn is_purged(&self, agent_id: &str) -> bool {
        self.purged.contains(agent_id)
    }

    pub fn expire(&mut self, now: Instant) {
        self.reservations
            .retain(|_, reservation| reservation.expires_at > now);
    }

    fn used(&self, agent_id: &str) -> Option<u64> {
        self.measured.get(agent_id).copied().unwrap_or(Some(0))
    }

    fn open_bytes(&self, agent_id: Option<&str>) -> u64 {
        self.reservations
            .values()
            .filter(|reservation| agent_id.is_none_or(|id| reservation.agent_id == id))
            .fold(0, |sum, reservation| sum.saturating_add(reservation.bytes))
    }

    /// Each Agent contributes at most its own cap to the machine total, so an
    /// overflowing or unmeasurable directory refuses only that Agent instead of
    /// saturating (or escaping) the global quota.
    fn global_used(&self) -> u64 {
        self.measured
            .values()
            .map(|bytes| {
                bytes.map_or(self.limits.agent_bytes, |bytes| {
                    bytes.min(self.limits.agent_bytes)
                })
            })
            .fold(0, u64::saturating_add)
    }

    pub fn usage(&self, agent_id: &str) -> AgentUsage {
        let used = self.used(agent_id).map_or(self.limits.agent_bytes, |used| {
            used.min(self.limits.agent_bytes)
        });
        let agent_headroom = self
            .limits
            .agent_bytes
            .saturating_sub(used)
            .saturating_sub(self.open_bytes(Some(agent_id)));
        let global_headroom = self
            .limits
            .global_bytes
            .saturating_sub(self.global_used())
            .saturating_sub(self.open_bytes(None));
        AgentUsage {
            used_bytes: used,
            agent_quota_bytes: self.limits.agent_bytes,
            available_bytes: agent_headroom.min(global_headroom),
            reservation_ids: self
                .reservations
                .values()
                .filter(|reservation| reservation.agent_id == agent_id)
                .map(|reservation| reservation.id.clone())
                .collect(),
        }
    }

    /// Admission check for `bytes` of new content. `free_bytes` is the volume's
    /// currently available space as measured by the Controller.
    pub fn reserve(
        &mut self,
        agent_id: &str,
        bytes: u64,
        free_bytes: u64,
        now: Instant,
    ) -> Result<Reservation, ProtocolError> {
        self.expire(now);
        if self.inventory_incomplete {
            return Err(ProtocolError::new(
                "agent_private_state_unavailable",
                "Agent private state inventory is incomplete; reservations resume after a successful re-enumeration",
            ));
        }
        if self.purged.contains(agent_id) {
            return Err(ProtocolError::new(
                "agent_private_state_unavailable",
                "Agent private state was removed after deletion",
            ));
        }
        if bytes == 0 || bytes > self.limits.reservation_bytes {
            return Err(ProtocolError::new(
                "invalid_private_state_reservation",
                format!(
                    "reservation bytes must be between 1 and {}",
                    self.limits.reservation_bytes
                ),
            ));
        }
        let Some(used) = self.used(agent_id) else {
            return Err(ProtocolError::new(
                "agent_private_state_unavailable",
                "Agent private state could not be measured; inspect the Controller log",
            ));
        };
        if self
            .reservations
            .values()
            .filter(|reservation| reservation.agent_id == agent_id)
            .count()
            >= self.limits.reservations_per_agent
        {
            return Err(ProtocolError::new(
                "private_state_too_many_reservations",
                "too many open private state reservations; commit or release them first",
            ));
        }
        if used
            .saturating_add(self.open_bytes(Some(agent_id)))
            .saturating_add(bytes)
            > self.limits.agent_bytes
        {
            return Err(ProtocolError::new(
                "private_state_agent_quota_exceeded",
                "the Agent private state quota would be exceeded",
            ));
        }
        if self
            .global_used()
            .saturating_add(self.open_bytes(None))
            .saturating_add(bytes)
            > self.limits.global_bytes
        {
            return Err(ProtocolError::new(
                "private_state_global_quota_exceeded",
                "the machine-wide private state quota would be exceeded",
            ));
        }
        // Open reservations are promises not yet on disk: subtract all of them
        // (conservatively, even those partially written) before the floor.
        if free_bytes
            .saturating_sub(self.open_bytes(None))
            .saturating_sub(bytes)
            < self.limits.free_space_floor_bytes
        {
            return Err(ProtocolError::new(
                "private_state_disk_space_low",
                "the machine is low on disk space; no private state reservation was made",
            ));
        }
        let reservation = Reservation {
            id: format!("psr_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
            agent_id: agent_id.to_owned(),
            bytes,
            expires_at: now + self.limits.reservation_ttl,
        };
        self.reservations
            .insert(reservation.id.clone(), reservation.clone());
        Ok(reservation)
    }

    /// Close a reservation owned by `agent_id`. Unknown, expired and foreign
    /// reservations share one fixed error. Repeated commits therefore fail.
    pub fn commit(&mut self, agent_id: &str, id: &str, now: Instant) -> Result<(), ProtocolError> {
        self.expire(now);
        match self.reservations.get(id) {
            Some(reservation) if reservation.agent_id == agent_id => {
                self.reservations.remove(id);
                Ok(())
            }
            _ => Err(reservation_expired()),
        }
    }

    /// Idempotent release of an owned reservation; foreign IDs are untouched.
    pub fn release(&mut self, agent_id: &str, id: &str, now: Instant) -> bool {
        self.expire(now);
        match self.reservations.get(id) {
            Some(reservation) if reservation.agent_id == agent_id => {
                self.reservations.remove(id);
                true
            }
            _ => false,
        }
    }

    /// Measured usage against the caps after a commit. The Controller's own
    /// measurement decides; callers never report their byte counts.
    pub fn check_caps(&self, agent_id: &str) -> Result<(), ProtocolError> {
        match self.used(agent_id) {
            Some(used) if used <= self.limits.agent_bytes => {}
            _ => {
                return Err(ProtocolError::new(
                    "private_state_agent_quota_exceeded",
                    "the Agent private state exceeds its quota; remove the new content",
                ))
            }
        }
        if self.global_used() > self.limits.global_bytes {
            return Err(ProtocolError::new(
                "private_state_global_quota_exceeded",
                "the machine-wide private state quota is exceeded; remove the new content",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("treer-private-state-{}", Uuid::new_v4().simple()));
            fs::create_dir_all(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
            }
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn identities_are_single_safe_path_components() {
        for value in ["ag_0123abcd", "srv_1", "command", "a-b_C9"] {
            assert!(valid_identity(value), "{value}");
        }
        for value in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "a b",
            "a.b",
            "ag\n",
            "ag\0",
            &"x".repeat(MAX_IDENTITY_LENGTH + 1),
        ] {
            assert!(!valid_identity(value), "{value:?}");
        }
    }

    #[test]
    fn root_and_server_levels_are_created_private_and_identities_are_checked() {
        let fixture = Fixture::new();
        let root = fixture.0.join("state").join("agent-private");
        assert_eq!(
            PrivateStateRoot::open(Path::new("relative"), "srv_a")
                .unwrap_err()
                .code,
            "agent_private_state_unavailable"
        );
        assert_eq!(
            PrivateStateRoot::open(&root, "srv/a").unwrap_err().code,
            "agent_private_state_invalid_identity"
        );
        let state = PrivateStateRoot::open(&root, "srv_a").unwrap();
        assert_eq!(state.server_directory(), root.join("srv_a"));
        assert_eq!(
            state.agent_directory("../x").unwrap_err().code,
            "agent_private_state_invalid_identity"
        );
        assert_eq!(
            state.verify_existing("ag_absent").unwrap_err().code,
            "agent_private_state_unavailable",
            "verification never creates an Agent directory"
        );
        let agent = state.prepare("ag_1").unwrap();
        assert_eq!(agent, root.join("srv_a").join("ag_1"));
        assert_eq!(state.verify_existing("ag_1").unwrap(), agent);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for level in [&root, &root.join("srv_a"), &agent] {
                assert_eq!(
                    fs::metadata(level).unwrap().permissions().mode() & 0o777,
                    0o700,
                    "{}",
                    level.display()
                );
            }
        }
        // Re-preparing is idempotent and keeps existing content.
        fs::write(agent.join("kept"), b"x").unwrap();
        assert_eq!(state.prepare("ag_1").unwrap(), agent);
        assert!(agent.join("kept").exists());
        assert_eq!(state.agent_ids().unwrap(), vec!["ag_1".to_string()]);
    }

    #[test]
    fn measurement_is_bounded_and_never_follows_links() {
        let fixture = Fixture::new();
        let root = fixture.0.join("agent-private");
        let state = PrivateStateRoot::open(&root, "srv_a").unwrap();
        let agent = state.prepare("ag_1").unwrap();
        fs::create_dir(agent.join("images")).unwrap();
        fs::write(agent.join("images").join("a.png"), vec![1; 1000]).unwrap();
        fs::write(agent.join("journal.json"), vec![2; 24]).unwrap();
        #[cfg(unix)]
        {
            let outside = fixture.0.join("outside");
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("huge"), vec![3; 100_000]).unwrap();
            std::os::unix::fs::symlink(&outside, agent.join("link-dir")).unwrap();
            std::os::unix::fs::symlink(outside.join("huge"), agent.join("link-file")).unwrap();
        }
        let measurement = state.measure("ag_1").unwrap();
        assert!(!measurement.overflow);
        #[cfg(unix)]
        {
            // 1024 bytes of real content plus the two links' own sizes.
            let link_sizes = fs::symlink_metadata(agent.join("link-dir")).unwrap().len()
                + fs::symlink_metadata(agent.join("link-file")).unwrap().len();
            assert_eq!(measurement.bytes, 1024 + link_sizes);
            assert_eq!(measurement.entries, 5);
        }
        #[cfg(not(unix))]
        assert_eq!(measurement.bytes, 1024);
        assert_eq!(
            state.measure("ag_missing").unwrap_err().code,
            "agent_private_state_unavailable"
        );
        let mut ledger = QuotaLedger::new(QuotaLimits::default());
        ledger.record(
            "ag_1",
            Some(Measurement {
                bytes: 5,
                entries: MAX_MEASURED_ENTRIES + 1,
                overflow: true,
            }),
        );
        assert_eq!(
            ledger
                .reserve("ag_1", 1, u64::MAX, Instant::now())
                .unwrap_err()
                .code,
            "private_state_agent_quota_exceeded",
            "an overflowing walk is treated as over quota"
        );
    }

    #[cfg(unix)]
    #[test]
    fn purge_removes_only_the_agent_directory_and_never_follows_links() {
        let fixture = Fixture::new();
        let root = fixture.0.join("agent-private");
        let state = PrivateStateRoot::open(&root, "srv_a").unwrap();
        let agent = state.prepare("ag_1").unwrap();
        let outside = fixture.0.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, agent.join("escape")).unwrap();
        fs::write(agent.join("own"), b"own").unwrap();
        state.prepare("ag_2").unwrap();
        assert!(state.purge("ag_1").unwrap());
        assert!(!agent.exists());
        assert!(
            outside.join("keep").exists(),
            "link targets survive a purge"
        );
        assert!(state.verify_existing("ag_2").is_ok());
        assert!(!state.purge("ag_1").unwrap(), "second purge is a no-op");
        // A link at the Agent level is never removed through, nor reported.
        std::os::unix::fs::symlink(&outside, root.join("srv_a").join("ag_link")).unwrap();
        assert_eq!(
            state.purge("ag_link").unwrap_err().code,
            "agent_private_state_unsafe"
        );
        assert!(outside.join("keep").exists());
    }

    #[cfg(unix)]
    #[test]
    fn links_foreign_files_open_modes_and_unsafe_ancestors_fail_closed() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let fixture = Fixture::new();
        let root = fixture.0.join("agent-private");
        let state = PrivateStateRoot::open(&root, "srv_a").unwrap();
        let elsewhere = fixture.0.join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        symlink(&elsewhere, root.join("srv_a").join("linked")).unwrap();
        assert_eq!(
            state.prepare("linked").unwrap_err().code,
            "agent_private_state_unsafe"
        );
        fs::write(root.join("srv_a").join("file"), b"not a dir").unwrap();
        assert_eq!(
            state.prepare("file").unwrap_err().code,
            "agent_private_state_unsafe"
        );
        let open = root.join("srv_a").join("open");
        fs::create_dir(&open).unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(0o750)).unwrap();
        assert_eq!(
            state.prepare("open").unwrap_err().code,
            "agent_private_state_unsafe"
        );
        // A link at the server level poisons every Agent below it.
        let other = fixture.0.join("other-root");
        fs::create_dir(&other).unwrap();
        symlink(&other, root.join("srv_b")).unwrap();
        assert_eq!(
            PrivateStateRoot::open(&root, "srv_b").unwrap_err().code,
            "agent_private_state_unsafe"
        );
        // Listing ignores links and files; only real directories are Agents.
        assert_eq!(state.agent_ids().unwrap(), vec!["open".to_string()]);
        // The root itself may not be a link, even to a private directory.
        let linked_root = fixture.0.join("linked-root");
        symlink(&root, &linked_root).unwrap();
        assert_eq!(
            PrivateStateRoot::open(&linked_root, "srv_a")
                .unwrap_err()
                .code,
            "agent_private_state_unsafe"
        );
        // An ancestor writable by others (without sticky) is rejected, both at
        // open and on later re-verification; sticky world-writable passes.
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(
            PrivateStateRoot::open(&root, "srv_a").unwrap_err().code,
            "agent_private_state_unsafe"
        );
        assert_eq!(
            state.prepare("ag_late").unwrap_err().code,
            "agent_private_state_unsafe"
        );
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o1777)).unwrap();
        assert!(PrivateStateRoot::open(&root, "srv_a").is_ok());
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn ledger_admits_commits_releases_expires_and_scopes_by_agent() {
        let mut ledger = QuotaLedger::new(QuotaLimits {
            agent_bytes: 100,
            global_bytes: 150,
            reservation_bytes: 60,
            reservations_per_agent: 3,
            reservation_ttl: Duration::from_secs(60),
            free_space_floor_bytes: 10,
        });
        let now = Instant::now();
        let full = Measurement {
            bytes: 0,
            entries: 0,
            overflow: false,
        };
        ledger.record("a", Some(full));
        ledger.record("b", Some(Measurement { bytes: 40, ..full }));
        for (bytes, code) in [
            (0, "invalid_private_state_reservation"),
            (61, "invalid_private_state_reservation"),
        ] {
            assert_eq!(
                ledger.reserve("a", bytes, u64::MAX, now).unwrap_err().code,
                code
            );
        }
        assert_eq!(
            ledger.reserve("a", 50, 55, now).unwrap_err().code,
            "private_state_disk_space_low"
        );
        // Open reservations count against free space before they are written:
        // two 50-byte promises on a 115-byte volume leave 15 >= floor 10, a
        // third promise of 10 would leave 5 and is refused until one is released.
        let promise_a = ledger.reserve("a", 50, 115, now).unwrap();
        let promise_b = ledger.reserve("b", 50, 115, now).unwrap();
        assert_eq!(
            ledger.reserve("a", 10, 115, now).unwrap_err().code,
            "private_state_disk_space_low"
        );
        assert!(ledger.release("b", &promise_b.id, now));
        let promise_c = ledger.reserve("a", 10, 115, now).unwrap();
        for promise in [&promise_a, &promise_c] {
            assert!(ledger.release("a", &promise.id, now));
        }
        let first = ledger.reserve("a", 50, u64::MAX, now).unwrap();
        assert!(first.id.starts_with("psr_") && first.id.len() == 68);
        assert_eq!(ledger.usage("a").available_bytes, 50);
        assert_eq!(
            ledger.reserve("a", 51, u64::MAX, now).unwrap_err().code,
            "private_state_agent_quota_exceeded"
        );
        let second = ledger.reserve("a", 50, u64::MAX, now).unwrap();
        assert_eq!(
            ledger.reserve("a", 1, u64::MAX, now).unwrap_err().code,
            "private_state_agent_quota_exceeded"
        );
        // Global: 40 measured (b) + 100 open (a) = 140; 11 more exceeds 150.
        assert_eq!(
            ledger.reserve("b", 11, u64::MAX, now).unwrap_err().code,
            "private_state_global_quota_exceeded"
        );
        // Foreign commit/release never touch another Agent's reservation.
        assert_eq!(
            ledger.commit("b", &first.id, now).unwrap_err().code,
            "private_state_reservation_expired"
        );
        assert!(!ledger.release("b", &first.id, now));
        assert_eq!(ledger.usage("a").reservation_ids.len(), 2);
        assert!(ledger.usage("b").reservation_ids.is_empty());
        assert_eq!(
            ledger.reserve("a", 1, u64::MAX, now).unwrap_err().code,
            "private_state_agent_quota_exceeded"
        );
        ledger.commit("a", &first.id, now).unwrap();
        assert_eq!(
            ledger.commit("a", &first.id, now).unwrap_err().code,
            "private_state_reservation_expired",
            "a second commit never counts twice"
        );
        assert!(ledger.release("a", &second.id, now));
        assert!(
            !ledger.release("a", &second.id, now),
            "release is idempotent"
        );
        let third = ledger.reserve("a", 10, u64::MAX, now).unwrap();
        let later = now + Duration::from_secs(61);
        assert_eq!(
            ledger.commit("a", &third.id, later).unwrap_err().code,
            "private_state_reservation_expired"
        );
        assert!(ledger.usage("a").reservation_ids.is_empty());
        let fourth = ledger.reserve("a", 10, u64::MAX, later).unwrap();
        let fifth = ledger.reserve("a", 10, u64::MAX, later).unwrap();
        let sixth = ledger.reserve("a", 10, u64::MAX, later).unwrap();
        assert_eq!(
            ledger.reserve("a", 10, u64::MAX, later).unwrap_err().code,
            "private_state_too_many_reservations"
        );
        for reservation in [&fourth, &fifth, &sixth] {
            assert!(ledger.release("a", &reservation.id, later));
        }
        // Controller measurement decides the caps after commit.
        ledger.record("a", Some(Measurement { bytes: 101, ..full }));
        assert_eq!(
            ledger.check_caps("a").unwrap_err().code,
            "private_state_agent_quota_exceeded"
        );
        ledger.record("a", Some(Measurement { bytes: 90, ..full }));
        ledger.record("b", Some(Measurement { bytes: 90, ..full }));
        assert_eq!(
            ledger.check_caps("a").unwrap_err().code,
            "private_state_global_quota_exceeded"
        );
        // Unmeasurable or overflowing Agents are refused themselves and count
        // as exactly one Agent cap (100) globally: neither saturating the
        // machine total nor escaping it.
        ledger.record("a", None);
        ledger.record("b", Some(Measurement { bytes: 40, ..full }));
        assert_eq!(
            ledger.reserve("a", 1, u64::MAX, later).unwrap_err().code,
            "agent_private_state_unavailable"
        );
        assert_eq!(ledger.usage("a").used_bytes, 100);
        assert_eq!(
            ledger.reserve("b", 11, u64::MAX, later).unwrap_err().code,
            "private_state_global_quota_exceeded"
        );
        let within = ledger.reserve("b", 10, u64::MAX, later).unwrap();
        assert!(ledger.release("b", &within.id, later));
        ledger.record(
            "a",
            Some(Measurement {
                bytes: 5,
                entries: MAX_MEASURED_ENTRIES + 1,
                overflow: true,
            }),
        );
        assert_eq!(
            ledger.reserve("a", 1, u64::MAX, later).unwrap_err().code,
            "private_state_agent_quota_exceeded"
        );
        assert_eq!(
            ledger.reserve("b", 11, u64::MAX, later).unwrap_err().code,
            "private_state_global_quota_exceeded"
        );
        assert!(ledger.reserve("b", 10, u64::MAX, later).is_ok());
        ledger.record("b", Some(full));
        // Deletion forgets accounting and refuses the identity afterwards.
        ledger.forget("b");
        assert!(ledger.is_purged("b"));
        assert!(ledger.usage("b").reservation_ids.is_empty());
        assert_eq!(
            ledger.reserve("b", 1, u64::MAX, later).unwrap_err().code,
            "agent_private_state_unavailable"
        );
        ledger.record("b", Some(full));
        assert!(
            !ledger.is_purged("b"),
            "a fresh Create re-admits the identity"
        );
        // An incomplete inventory never admits new content; a full
        // re-enumeration restores admission with the real usage.
        ledger.mark_inventory_incomplete();
        assert_eq!(
            ledger.reserve("b", 1, u64::MAX, later).unwrap_err().code,
            "agent_private_state_unavailable"
        );
        assert!(ledger.inventory_incomplete());
        ledger.replace_inventory(vec![
            ("b".into(), Some(full)),
            ("c".into(), Some(Measurement { bytes: 95, ..full })),
        ]);
        assert!(!ledger.inventory_incomplete());
        assert_eq!(
            ledger.reserve("b", 56, u64::MAX, later).unwrap_err().code,
            "private_state_global_quota_exceeded"
        );
        assert!(ledger.reserve("b", 55, u64::MAX, later).is_ok());
    }

    /// Windows: the live owner/DACL is what decides, not the creation branch.
    /// Executed by the native Windows gate; it shells out to icacls to loosen
    /// a test-private directory and removes that directory afterwards.
    #[cfg(windows)]
    #[test]
    fn windows_dacl_is_verified_live_not_assumed_from_creation() {
        fn grant_everyone(path: &Path) {
            let status = std::process::Command::new("icacls")
                .arg(path)
                .args(["/grant", "*S-1-1-0:(OI)(CI)F"])
                .status()
                .expect("run icacls");
            assert!(status.success(), "icacls grant failed");
        }
        fn report(path: &Path) -> String {
            treer_platform::private_path_security_report(path)
                .unwrap_or_else(|error| format!("report unavailable: {error}"))
        }
        let fixture = Fixture::new();
        let root = fixture.0.join("agent-private");
        // Baseline: secure_directory must produce the protected private DACL
        // on this machine; on failure print what the system actually reports.
        treer_platform::secure_directory(&root).unwrap();
        assert!(
            treer_platform::private_path_is_secure(&root).unwrap(),
            "fresh private root is not accepted: {} (fixture {})",
            report(&root),
            report(&fixture.0)
        );
        let state = PrivateStateRoot::open(&root, "srv_a").unwrap_or_else(|error| {
            panic!(
                "{}: root {} / server {}",
                error.code,
                report(&root),
                report(&root.join("srv_a"))
            )
        });
        let agent = state.prepare("ag_1").unwrap();
        assert!(
            treer_platform::private_path_is_secure(&agent).unwrap(),
            "{}",
            report(&agent)
        );
        // A pre-existing directory with a wide DACL is never adopted.
        let wide = root.join("srv_a").join("ag_wide");
        fs::create_dir(&wide).unwrap();
        grant_everyone(&wide);
        assert!(
            !treer_platform::private_path_is_secure(&wide).unwrap(),
            "{}",
            report(&wide)
        );
        assert_eq!(
            state.prepare("ag_wide").unwrap_err().code,
            "agent_private_state_unsafe"
        );
        // Loosening after creation fails the next verification of a
        // directory that passed before.
        grant_everyone(&agent);
        assert!(
            !treer_platform::private_path_is_secure(&agent).unwrap(),
            "{}",
            report(&agent)
        );
        assert_eq!(
            state.verify_existing("ag_1").unwrap_err().code,
            "agent_private_state_unsafe"
        );
        assert_eq!(
            state.prepare("ag_1").unwrap_err().code,
            "agent_private_state_unsafe"
        );
        // Everything else below the root stays usable.
        assert!(state.prepare("ag_2").is_ok());
    }
}

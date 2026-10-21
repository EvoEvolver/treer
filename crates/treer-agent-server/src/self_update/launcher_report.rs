//! Parse and compare launcher install/status metadata, not activation proof.
//! The future executor must obtain both outputs from the authenticated new
//! Controller, under original ownership, and independently recheck live health.
//! Calling this parser with arbitrary/replayed bytes grants no mutation or
//! journal-transition capability. No child is executed here.
use std::collections::HashSet;

use anyhow::{bail, Result};
use serde::Deserialize;

use super::hex;

const LIMIT: usize = 4 * 1024 * 1024;
const MAX_VERSIONS: usize = 1024;
const MAX_PROFILES: usize = 32;

// Deliberate projections of the existing launcher contract. Extra fields (such
// as local entry/shim paths and Agent startup details) are ignored, not stored
// or returned. Serde rejects duplicate known fields, including nested ones.
#[derive(Deserialize)]
struct Compatibility {
    status: String,
}

#[derive(Deserialize)]
struct Install {
    schema: String,
    operation: String,
    version: String,
    source_commit: String,
    compatibility: Compatibility,
    profiles: Vec<String>,
    portable: bool,
}

#[derive(Deserialize)]
struct Version {
    version: String,
    // Older retained versions may fail inspection and report no provenance.
    source_commit: Option<String>,
    compatibility: Compatibility,
    portable: Option<bool>,
}

#[derive(Deserialize)]
struct Status {
    schema: String,
    operation: String,
    current: String,
    versions: Vec<Version>,
    entry_integrity: String,
}

/// Metadata consistency only; not a sealed install/health proof. No paths,
/// arbitrary diagnostics or credentials are retained. It cannot advance a job.
pub struct LauncherReport {
    version: String,
    source_commit: String,
    profile_count: usize,
}

impl LauncherReport {
    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn source_commit(&self) -> &str {
        &self.source_commit
    }

    pub fn profile_count(&self) -> usize {
        self.profile_count
    }
}

fn content_version(value: &str) -> bool {
    value
        .strip_prefix("sha256-")
        .is_some_and(|digest| hex(digest, 64))
}

/// `artifact_source_sha` is the authenticated native artifact's build identity,
/// which can differ from a wrapper release SHA when artifacts were reused.
/// Malformed/missing/duplicate fields fail with one fixed diagnostic; never
/// expose serde errors, profile names or ignored path-valued metadata.
pub fn compare(
    install_output: &[u8],
    status_output: &[u8],
    artifact_source_sha: &str,
) -> Result<LauncherReport> {
    let result = (|| -> Result<LauncherReport> {
        if !hex(artifact_source_sha, 40)
            || install_output.len() > LIMIT
            || status_output.len() > LIMIT
        {
            bail!("invalid");
        }
        let install: Install = serde_json::from_slice(install_output)?;
        let status: Status = serde_json::from_slice(status_output)?;
        if install.schema != "treer.launcher-install-result/v1"
            || status.schema != install.schema
            || install.operation != "install"
            || status.operation != "status"
            || !content_version(&install.version)
            || install.source_commit != artifact_source_sha
            || install.compatibility.status != "compatible"
            || !install.portable
            || status.current != install.version
            || status.entry_integrity != "valid"
            || status.versions.len() > MAX_VERSIONS
            || install.profiles.is_empty()
            || install.profiles.len() > MAX_PROFILES
        {
            bail!("invalid");
        }
        let mut profiles = HashSet::new();
        for profile in &install.profiles {
            if profile.is_empty()
                || profile.len() > 128
                || profile.chars().any(char::is_control)
                || !profiles.insert(profile.as_str())
            {
                bail!("invalid");
            }
        }
        let mut versions = HashSet::new();
        let mut found = false;
        for version in &status.versions {
            if !content_version(&version.version) || !versions.insert(version.version.as_str()) {
                bail!("invalid");
            }
            if version.version == install.version {
                if version.source_commit.as_deref() != Some(artifact_source_sha)
                    || version.compatibility.status != "compatible"
                    || version.portable != Some(true)
                {
                    bail!("invalid");
                }
                found = true;
            }
        }
        if !found {
            bail!("invalid");
        }
        Ok(LauncherReport {
            version: install.version,
            source_commit: install.source_commit,
            profile_count: profiles.len(),
        })
    })();
    result.map_err(|_| anyhow::anyhow!("update_launcher_report_mismatch"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn documents() -> (Value, Value, String) {
        let sha = "b".repeat(40);
        let version = format!("sha256-{}", "a".repeat(64));
        (
            json!({"schema":"treer.launcher-install-result/v1", "operation":"install",
                "version":version,"source_commit":sha,"compatibility":{"status":"compatible"},
                "portable":true,"profiles":["Codex","Claude"],
                "entry":"/private/not-public/run.mjs","shim":"/private/not-public/bin/treer-agent"}),
            json!({"schema":"treer.launcher-install-result/v1", "operation":"status",
                "current":version,"entry_integrity":"valid", "versions":[
                    {"version":format!("sha256-{}", "c".repeat(64)),"compatibility":{"status":"incompatible"}},
                    {"version":version,"source_commit":sha,"compatibility":{"status":"compatible"},"portable":true}
                ],"agent_startups":{"partial":true,"reason":"not used for launcher integrity"}}),
            sha,
        )
    }

    fn check(install: &Value, status: &Value, sha: &str) -> Result<LauncherReport> {
        compare(
            &serde_json::to_vec(install)?,
            &serde_json::to_vec(status)?,
            sha,
        )
    }

    #[test]
    fn compares_current_portable_build_and_ignores_old_version_and_private_metadata() {
        let (install, status, sha) = documents();
        let report = check(&install, &status, &sha).unwrap();
        assert_eq!(report.version(), install["version"]);
        assert_eq!(report.source_commit(), sha);
        assert_eq!(report.profile_count(), 2);
        // Wrapper release identity must not substitute for native build identity.
        assert!(check(&install, &status, &"d".repeat(40)).is_err());
    }

    #[test]
    fn mismatched_missing_or_ambiguous_installation_is_rejected() {
        let (install, status, sha) = documents();
        for (pointer, value) in [
            ("/schema", json!("other")),
            ("/operation", json!("update")),
            ("/version", json!("sha256-invalid")),
            ("/source_commit", json!("d".repeat(40))),
            ("/compatibility/status", json!("unknown")),
            ("/portable", json!(false)),
            ("/profiles", json!([])),
            ("/profiles", json!(["duplicate", "duplicate"])),
            ("/profiles", json!(["control\nname"])),
            ("/profiles", json!(["x".repeat(129)])),
            (
                "/profiles",
                json!((0..=MAX_PROFILES)
                    .map(|i| format!("profile-{i}"))
                    .collect::<Vec<_>>()),
            ),
        ] {
            let mut changed = install.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert_eq!(
                check(&changed, &status, &sha).err().unwrap().to_string(),
                "update_launcher_report_mismatch"
            );
        }
        for (pointer, value) in [
            ("/schema", json!("other")),
            ("/operation", json!("install")),
            ("/current", json!(format!("sha256-{}", "e".repeat(64)))),
            ("/entry_integrity", json!("mismatch")),
            ("/versions", json!([])),
            ("/versions/1/source_commit", Value::Null),
            ("/versions/1/portable", Value::Null),
            ("/versions/1/compatibility/status", json!("incompatible")),
            ("/versions/0/version", json!("../private/path")),
        ] {
            let mut changed = status.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert_eq!(
                check(&install, &changed, &sha).err().unwrap().to_string(),
                "update_launcher_report_mismatch"
            );
        }
        let mut duplicate = status.clone();
        duplicate["versions"]
            .as_array_mut()
            .unwrap()
            .push(status["versions"][1].clone());
        assert!(check(&install, &duplicate, &sha).is_err());
        let mut excess = status.clone();
        let mut versions = vec![status["versions"][1].clone()];
        versions.extend((0..MAX_VERSIONS).map(|i| {
            json!({
                "version":format!("sha256-{i:064x}"), "compatibility":{"status":"incompatible"}
            })
        }));
        excess["versions"] = json!(versions);
        assert!(check(&install, &excess, &sha).is_err());
        for name in ["schema", "version", "portable", "profiles"] {
            let mut missing = install.clone();
            missing.as_object_mut().unwrap().remove(name);
            assert!(check(&missing, &status, &sha).is_err());
        }
    }

    #[test]
    fn malformed_oversized_and_duplicate_known_fields_never_expose_text() {
        let (install, status, sha) = documents();
        // Valid otherwise acceptable documents: without the byte cap these
        // would pass, unlike an oversized buffer of invalid JSON whitespace.
        let mut oversized_install = install.clone();
        oversized_install["ignored"] = json!("x".repeat(LIMIT));
        assert!(check(&oversized_install, &status, &sha).is_err());
        let mut oversized_status = status.clone();
        oversized_status["ignored"] = json!("x".repeat(LIMIT));
        assert!(check(&install, &oversized_status, &sha).is_err());
        let install = serde_json::to_vec(&install).unwrap();
        let status = serde_json::to_vec(&status).unwrap();
        let duplicated = |bytes: &[u8], prefix: &str| {
            let mut result = prefix.as_bytes().to_vec();
            result.extend_from_slice(&bytes[1..]);
            result
        };
        for bytes in [
            b"/private/credential=value".to_vec(),
            vec![b' '; LIMIT + 1],
            duplicated(&install, "{\"version\":\"secret/private\","),
            duplicated(&install, "{\"portable\":true,"),
        ] {
            assert_eq!(
                compare(&bytes, &status, &sha).err().unwrap().to_string(),
                "update_launcher_report_mismatch"
            );
        }
        for bytes in [
            vec![b' '; LIMIT + 1],
            duplicated(&status, "{\"current\":\"secret/private\","),
        ] {
            assert_eq!(
                compare(&install, &bytes, &sha).err().unwrap().to_string(),
                "update_launcher_report_mismatch"
            );
        }
        let nested = String::from_utf8(install).unwrap().replace(
            "\"status\":\"compatible\"",
            "\"status\":\"compatible\",\"status\":\"compatible\"",
        );
        assert!(compare(nested.as_bytes(), &status, &sha).is_err());
    }
}

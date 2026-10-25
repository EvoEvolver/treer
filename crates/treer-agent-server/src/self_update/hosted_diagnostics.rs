//! Non-default hosted fixture evidence only. No recovery or mutation authority.
use super::*;
use serde::Serialize;
use std::sync::Mutex;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::self_update) enum Stage {
    Binding,
    Selection,
    Intent,
    Runtime,
    Catalog,
    Current,
    Target,
    State,
    Claim,
    Admission,
    Checksums,
    Artifacts,
    Host,
    Baseline,
    StartupBackup,
    Activator,
    Images,
    Probes,
    Reattest,
    Prepared,
    Handoff,
    Observation,
    Completion,
}

pub(in crate::self_update) struct Diagnostic {
    directory: PrivateDirectory,
    stage: Mutex<Stage>,
}
impl Diagnostic {
    pub(in crate::self_update) fn new(directory: PrivateDirectory) -> Self {
        Self {
            directory,
            stage: Mutex::new(Stage::Binding),
        }
    }
    pub(in crate::self_update) fn mark(&self, stage: Stage) {
        *self.stage.lock().unwrap() = stage;
        // Each fixed stage has one exclusive evidence leaf. Failure to persist
        // diagnostics never grants a retry or changes the transaction outcome.
        let name = format!(
            "native-stage-{}.json",
            serde_json::to_value(stage).unwrap().as_str().unwrap()
        );
        let _ = write(
            &self.directory,
            &name,
            &serde_json::json!({"schema":1,"stage":stage}),
        );
    }
    pub(in crate::self_update) fn failed(&self, error: &anyhow::Error) {
        let _ = write(
            &self.directory,
            "native-submit-failure.json",
            &serde_json::json!({
                "schema":1,"stage":*self.stage.lock().unwrap(),"category":category(error),
                "observation":snapshot(&self.directory),"recovery_authority":false,
            }),
        );
    }
}
fn category(error: &anyhow::Error) -> &'static str {
    if let Some(error) = error.downcast_ref::<std::io::Error>() {
        return match error.kind() {
            std::io::ErrorKind::PermissionDenied => "permission_denied",
            std::io::ErrorKind::NotFound => "not_found",
            std::io::ErrorKind::AlreadyExists => "already_exists",
            std::io::ErrorKind::TimedOut => "timeout",
            _ => "io_error",
        };
    }
    if error.downcast_ref::<serde_json::Error>().is_some() {
        return "invalid_document";
    }
    // Equality against an allowlist returns only our static label, never the
    // error's text. Unknown or decorated errors remain a coarse category.
    for code in [
        "controller_update_catalog_invalid",
        "controller_update_current_unknown",
        "controller_update_scope_mismatch",
        "controller_update_preview_changed",
        "controller_update_target_not_upgrade",
        "controller_update_target_unavailable",
        "controller_update_unsupported",
        "update_artifact_pin_mismatch",
        "update_recovery_required",
        "update_controller_health_mismatch",
        "update_health_unavailable",
        "unsafe_update_image_probe",
        "update_image_conflict",
        "update_image_probe_failed",
        "update_image_probe_timeout",
        "invalid_update_image_version",
        "update_image_versions_disagree",
        "hosted_update_not_complete",
        "hosted_update_observation_timeout",
    ] {
        if error.chain().any(|cause| cause.to_string() == code) {
            return code;
        }
    }
    // Never serialize Display/Debug or arbitrary error text, even when wrapped.
    "guard_or_operation_failed"
}
fn read<T: serde::de::DeserializeOwned>(dir: &PrivateDirectory, name: &str) -> Result<T> {
    let mut bytes = Vec::new();
    dir.read_file(name)?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        bail!("diagnostic_document_too_large");
    }
    Ok(serde_json::from_slice(&bytes)?)
}
fn write(dir: &PrivateDirectory, name: &str, value: &serde_json::Value) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    if bytes.len() > 64 * 1024 {
        bail!("diagnostic_document_too_large");
    }
    let mut file = dir.create_file(name)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    dir.sync()?;
    Ok(())
}
fn presence(dir: &PrivateDirectory, name: &str) -> &'static str {
    match dir.read_file(name) {
        Ok(_) => "present",
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "absent_at_observation",
        Err(_) => "unavailable",
    }
}
fn snapshot(root: &PrivateDirectory) -> serde_json::Value {
    let mut result = serde_json::json!({"operation_id":null,"jobs":"unavailable","journal":"unavailable",
        "phase":null,"claim":"unavailable","worker_record":"unavailable","worker_identity":"not_observed","worker_image":"not_attested",
        "quiescence":"unproven","claim_release_authority":false});
    let Ok(binding) = read::<DispatchBinding>(root, "binding.json") else {
        return result;
    };
    if !binding.validate()
        || binding.workspace_id != "ws_native_acceptance"
        || binding.server_id != "srv_native_acceptance"
    {
        return result;
    }
    result["operation_id"] = binding.operation_id.clone().into();
    let jobs = root
        .open_child("state")
        .and_then(|d| d.open_child("self-update"))
        .and_then(|d| d.open_child("jobs"));
    let jobs = match jobs {
        Ok(jobs) => jobs,
        Err(e) => {
            result["jobs"] = if e.kind() == std::io::ErrorKind::NotFound {
                "absent_at_observation"
            } else {
                "unavailable"
            }
            .into();
            return result;
        }
    };
    result["jobs"] = "present".into();
    result["claim"] = presence(&jobs, "web-active.json").into();
    if let Ok(active) = read::<DispatchBinding>(&jobs, "web-active.json") {
        result["claim"] = if active == binding {
            "matching_selected_operation"
        } else {
            "present_other_or_invalid_binding"
        }
        .into();
    }
    let journal = match crate::self_update::Journal::open(&jobs, &binding.operation_id) {
        Ok(journal) => journal,
        Err(_) => {
            result["journal"] = "absent_or_unavailable".into();
            return result;
        }
    };
    let Ok(receipt) = journal.read() else {
        result["journal"] = "invalid_or_unavailable".into();
        return result;
    };
    if crate::self_update::web_adapter::check(&receipt, &binding).is_err() {
        result["journal"] = "binding_mismatch".into();
        return result;
    }
    result["journal"] = "valid_selected_operation".into();
    result["phase"] = serde_json::to_value(receipt.phase).unwrap();
    result["worker_record"] = presence(&journal.directory, "ownership.json").into();
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Ownership {
        schema_version: u32,
        prepared: crate::self_update::Receipt,
        pid: u32,
        creation: String,
    }
    if let Ok(owner) = read::<Ownership>(&journal.directory, "ownership.json") {
        if owner.schema_version == 1
            && owner.prepared.job_id == receipt.job_id
            && crate::self_update::web_adapter::check(&owner.prepared, &binding).is_ok()
            && owner.pid != 0
            && !owner.creation.is_empty()
        {
            result["worker_identity"] =
                match treer_platform::process_image::ProcessImage::capture(owner.pid) {
                    Ok(image) if image.creation_identity() == owner.creation => {
                        "recorded_creation_observed"
                    }
                    Ok(_) => "recorded_creation_mismatch",
                    Err(_) => "recorded_identity_not_observable",
                }
                .into();
        } else {
            result["worker_identity"] = "record_invalid".into();
        }
    }
    result
}
/// Called by the parent before fixture cleanup even if the submitter died.
#[cfg(test)]
pub(in crate::self_update) fn fixture_failure(root: &PrivateDirectory) {
    let _ = write(
        root,
        "native-fixture-failure.json",
        &serde_json::json!({"schema":1,
        "observation":snapshot(root),"submit_failure_receipt":presence(root,"native-submit-failure.json"),
        "category":"fixture_outcome_failed","recovery_authority":false}),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn bounded_diagnostics_exclude_errors_and_refuse_replacing_evidence() {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("treer-diag-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let dir = PrivateDirectory::open(&path).unwrap();
        let diag = Diagnostic::new(PrivateDirectory::open(&path).unwrap());
        diag.mark(Stage::Catalog);
        diag.failed(&anyhow::anyhow!(
            "SECRET /credential/path arbitrary user data"
        ));
        let value: serde_json::Value = read(&dir, "native-submit-failure.json").unwrap();
        assert_eq!(value["stage"], "catalog");
        assert_eq!(value["category"], "guard_or_operation_failed");
        assert!(!value.to_string().contains("SECRET"));
        assert_eq!(value["observation"]["quiescence"], "unproven");
        assert!(write(
            &dir,
            "native-submit-failure.json",
            &serde_json::json!({"overwrite":true})
        )
        .is_err());
        let io = anyhow::Error::from(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "SECRET",
        ));
        assert_eq!(category(&io), "permission_denied");
        assert_eq!(
            category(&anyhow::anyhow!("controller_update_current_unknown")),
            "controller_update_current_unknown"
        );
        assert_eq!(
            category(&anyhow::anyhow!("controller_update_current_unknown SECRET")),
            "guard_or_operation_failed"
        );
        assert_eq!(
            category(&anyhow::anyhow!("update_controller_health_mismatch")),
            "update_controller_health_mismatch"
        );
        let identity = wire::Identity {
            release_sha: "a".repeat(40),
            artifact_source_sha: "a".repeat(40),
            controller_sha256: "b".repeat(64),
            cli_sha256: "c".repeat(64),
        };
        let mut binding = DispatchBinding {
            operation_id: uuid::Uuid::new_v4().to_string(),
            workspace_id: "ws_native_acceptance".into(),
            server_id: "srv_native_acceptance".into(),
            current: identity.clone(),
            target: wire::Target {
                target_id: "new".into(),
                identity: wire::Identity {
                    controller_sha256: "d".repeat(64),
                    cli_sha256: "e".repeat(64),
                    ..identity
                },
                platform: wire::Platform::LinuxX86_64,
                version_label: "test".into(),
            },
            catalog_revision: 1,
            plan_digest: "0".repeat(64),
            created_at: chrono::Utc::now(),
        };
        binding.plan_digest = binding.expected_plan_digest().unwrap();
        assert!(binding.validate());
        write(
            &dir,
            "binding.json",
            &serde_json::to_value(&binding).unwrap(),
        )
        .unwrap();
        let jobs = dir
            .create_child("state")
            .unwrap()
            .create_child("self-update")
            .unwrap()
            .create_child("jobs")
            .unwrap();
        write(
            &jobs,
            "web-active.json",
            &serde_json::to_value(&binding).unwrap(),
        )
        .unwrap();
        let observation = snapshot(&dir);
        assert_eq!(observation["claim"], "matching_selected_operation");
        assert_eq!(observation["journal"], "absent_or_unavailable");
        assert_eq!(observation["quiescence"], "unproven");
        assert!(jobs.read_file("web-active.json").is_ok());
        let receipt = crate::self_update::Receipt {
            schema_version: 1,
            job_id: uuid::Uuid::parse_str(&binding.operation_id).unwrap(),
            revision: 0,
            workspace_id: binding.workspace_id.clone(),
            server_id: binding.server_id.clone(),
            release_sha: binding.target.identity.release_sha.clone(),
            artifact_source_sha: binding.target.identity.artifact_source_sha.clone(),
            controller_sha256: binding.target.identity.controller_sha256.clone(),
            cli_sha256: binding.target.identity.cli_sha256.clone(),
            publications: crate::self_update::publication::Publications::new(
                binding.current.controller_sha256.clone(),
                binding.current.cli_sha256.clone(),
                &binding.target.identity.controller_sha256,
                &binding.target.identity.cli_sha256,
            ),
            source: crate::self_update::Source::Pin {
                sums_sha256: "d".repeat(64),
                allow_pinned: true,
                allow_artifact_origin: true,
                allow_same: false,
                allow_downgrade: false,
            },
            phase: crate::self_update::Phase::Preparing,
            created_at: binding.created_at,
            updated_at: binding.created_at,
        };
        let journal = crate::self_update::Journal::create(&jobs, &receipt).unwrap();
        write(&journal.directory,"ownership.json", &serde_json::json!({"schema_version":1,"prepared":receipt,"pid":std::process::id(),
            "creation":treer_platform::process_image::ProcessImage::capture(std::process::id()).unwrap().creation_identity()})).unwrap();
        let observation = snapshot(&dir);
        assert_eq!(observation["journal"], "valid_selected_operation");
        assert_eq!(observation["worker_identity"], "recorded_creation_observed");
        assert_eq!(observation["worker_image"], "not_attested");
        assert_eq!(observation["claim_release_authority"], false);
        assert!(!observation.to_string().contains("pid"));

        let mut oversized = dir.create_file("oversized.json").unwrap();
        oversized.write_all(&vec![b' '; 65537]).unwrap();
        assert!(read::<serde_json::Value>(&dir, "oversized.json").is_err());
        fixture_failure(&dir);
        assert!(dir.read_file("native-fixture-failure.json").is_ok());
        std::fs::remove_dir_all(path).unwrap();
    }
}

/// Feature-only, read-only observation after actual peer authentication and
/// before SAME-lock transfer. It cannot grant ownership or retry authority.
pub(in crate::self_update) fn manager_cgroup(pid: u32) -> Result<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(format!("/proc/{pid}/cgroup"))?
        .take(4097)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        bail!("hosted_manager_cgroup_invalid");
    }
    parse_manager_cgroup(std::str::from_utf8(&bytes)?)
}
fn parse_manager_cgroup(text: &str) -> Result<String> {
    let mut lines = text.lines();
    let group = lines
        .next()
        .and_then(|line| line.strip_prefix("0::"))
        .ok_or_else(|| anyhow::anyhow!("hosted_manager_cgroup_v2_required"))?;
    if lines.next().is_some()
        || !group.starts_with('/')
        || group.len() > 2048
        || group.split('/').skip(1).any(|c| {
            c.is_empty()
                || c == "."
                || c == ".."
                || !c
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.:@\\".contains(&b))
        })
    {
        bail!("hosted_manager_cgroup_invalid");
    }
    Ok(group.to_owned())
}
pub(in crate::self_update) fn manager_worker(pid: u32, image: &std::fs::File) -> Result<()> {
    if std::env::var_os("TREER_ACCEPTANCE_SYSTEMD").is_none() {
        return Ok(());
    }
    if std::env::var("TREER_ACCEPTANCE_SYSTEMD").as_deref() != Ok("1") || !cfg!(target_os = "linux")
    {
        bail!("hosted_manager_context_invalid");
    }
    let root = super::root()?;
    let directory = PrivateDirectory::open(&root)?;
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Expected {
        schema: u32,
        unit: String,
        control_group: String,
    }
    let expected: Expected = read(&directory, "native-manager-expected.json")?;
    let unit = format!(
        "treer-native-{}.service",
        &root.file_name().unwrap().to_str().unwrap()[3..]
    );
    if expected.schema != 1
        || expected.unit != unit
        || !expected.control_group.ends_with(&format!("/{unit}"))
    {
        bail!("hosted_manager_binding_invalid");
    }
    let child = treer_platform::process_image::ProcessImage::capture(pid)?;
    if !child.matches_file(image)? {
        bail!("hosted_manager_worker_image_invalid");
    }
    child.verify_private_session()?;
    if manager_cgroup(std::process::id())? != expected.control_group
        || manager_cgroup(pid)? != expected.control_group
    {
        bail!("hosted_manager_worker_cgroup_invalid");
    }
    child.verify_current()?;
    write(
        &directory,
        "native-manager-worker.json",
        &serde_json::json!({"schema":1,
        "authenticated_worker_image":true,"same_unit_cgroup":true,"private_session":true,"recovery_authority":false}),
    )
}
#[cfg(test)]
mod manager_tests {
    use super::*;
    #[test]
    fn manager_cgroup_parser_refuses_unsupported_or_ambiguous_observations() {
        assert_eq!(
            parse_manager_cgroup("0::/user.slice/fixture.service\n").unwrap(),
            "/user.slice/fixture.service"
        );
        for invalid in [
            "",
            "1:cpu:/unit",
            "0::/unit\n0::/other",
            "0::/../unit",
            "0:://unit",
            "0::relative",
            "0::/unit bad",
        ] {
            assert!(parse_manager_cgroup(invalid).is_err());
        }
    }
}

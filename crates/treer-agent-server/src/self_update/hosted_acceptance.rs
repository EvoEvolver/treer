//! Dedicated hosted native acceptance build only; absent release/default builds.
//! No public route, capability enabling, arbitrary Exec or replay/recovery API.
//! Release provisions the synthetic Host/Controller fixture before this entry.
use std::{
    io::{Read, Write},
    path::PathBuf,
    time::Duration,
};

use anyhow::{bail, Result};
use treer_platform::private_directory::PrivateDirectory;
use treer_protocol::{controller_update as wire, controller_update_runtime::DispatchBinding};

fn eligible(
    disposable: Option<&str>,
    actions: Option<&str>,
    runner: Option<&str>,
    managed: bool,
    uid: u32,
) -> bool {
    disposable == Some("1")
        && actions == Some("true")
        && runner == Some("github-hosted")
        && !managed
        && uid != 0
}

fn root() -> Result<PathBuf> {
    if !eligible(
        std::env::var("TREER_DISPOSABLE_RUNNER").ok().as_deref(),
        std::env::var("GITHUB_ACTIONS").ok().as_deref(),
        std::env::var("RUNNER_ENVIRONMENT").ok().as_deref(),
        std::env::var_os("TREER_AGENT_ID").is_some()
            || std::env::var_os("TREER_WORKLOAD_CREDENTIAL").is_some(),
        nix::unistd::geteuid().as_raw(),
    ) {
        bail!("hosted_update_acceptance_refused");
    }
    // Eligibility env is a prerequisite, not independent hosted attestation.
    // Release must bind the dedicated workflow/run and approve this account.
    let home = nix::unistd::User::from_uid(nix::unistd::geteuid())?
        .ok_or_else(|| anyhow::anyhow!("hosted_update_account_unavailable"))?
        .dir
        .canonicalize()?;
    let ambient = PathBuf::from(
        std::env::var_os("HOME")
            .ok_or_else(|| anyhow::anyhow!("hosted_update_account_unavailable"))?,
    )
    .canonicalize()?;
    if ambient != home {
        bail!("hosted_update_account_unavailable");
    }
    treer_platform::installation_directory::InstallationDirectory::open(&home)?;
    let supplied = PathBuf::from(
        std::env::var_os("TREER_ACCEPTANCE_ROOT")
            .ok_or_else(|| anyhow::anyhow!("hosted_update_root_unavailable"))?,
    );
    let physical = supplied.canonicalize()?;
    let id = physical
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("treer-native-acceptance-"))
        .ok_or_else(|| anyhow::anyhow!("hosted_update_root_unavailable"))?;
    super::job_id(id)?;
    if supplied != physical
        || physical.parent() != Some(home.as_path())
        || std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) != Some(physical.join("xdg"))
        || std::env::var_os("TREER_STATE_DIR").map(PathBuf::from) != Some(physical.join("state"))
    {
        bail!("hosted_update_root_unavailable");
    }
    PrivateDirectory::open(&physical)?;
    Ok(physical)
}

/// Exact private entry: the executing binary must be the old installed image.
/// All arbitrary errors collapse to a fixed diagnostic before main prints them.
pub(crate) fn submit() -> Result<()> {
    submit_inner().map_err(|_| anyhow::anyhow!("hosted_update_acceptance_failed"))
}

fn submit_inner() -> Result<()> {
    let root = root()?;
    let directory = PrivateDirectory::open(&root)?;
    let mut bytes = Vec::new();
    directory
        .read_file("binding.json")?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        bail!("hosted_update_binding_invalid");
    }
    let binding: DispatchBinding = serde_json::from_slice(&bytes)?;
    if !binding.validate()
        || binding.workspace_id != "ws_native_acceptance"
        || binding.server_id != "srv_native_acceptance"
    {
        bail!("hosted_update_binding_invalid");
    }
    let selected =
        super::selection::Inventory::local()?.select(Some(&binding.workspace_id), true)?;
    if selected.controller_path() != root.join("bin/treer-agent-server")
        || selected.cli_path() != root.join("bin/treer")
        || selected.host_socket() != root.join("host.sock")
    {
        bail!("hosted_update_scope_invalid");
    }
    // Exclusive synced intent before any preflight, staging or spawn. Any
    // existing/partial intent is an ambiguous prior invocation, never retried.
    let mut intent = directory.create_file("acceptance-submit-intent.json")?;
    intent.write_all(&serde_json::to_vec(
        &serde_json::json!({"schema":1,"operation_id":binding.operation_id}),
    )?)?;
    intent.sync_all()?;
    drop(intent);
    directory.sync()?;
    let mut readback = Vec::new();
    directory
        .read_file("acceptance-submit-intent.json")?
        .read_to_end(&mut readback)?;
    if serde_json::from_slice::<serde_json::Value>(&readback)?["operation_id"]
        != binding.operation_id
    {
        bail!("hosted_update_intent_unconfirmed");
    }
    let artifacts = directory.open_child("artifacts")?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        // The real fixed Node/HOME recipe runs in finish_launcher; do not
        // substitute PATH Node, ambient HOME or a fabricated CoreHealthy.
        let prepared = super::web_preparation::prepare_local(binding.clone(), artifacts).await?;
        let operation = tokio::task::spawn_blocking(move || prepared.handoff()).await??;
        if operation.operation_id != binding.operation_id { bail!("hosted_update_binding_invalid"); }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(360);
        loop {
            // Read-only observation: no second prepare, POST, child or handoff.
            if let Ok(operation) = super::web_adapter::operation(&binding.workspace_id, &binding.server_id, &binding.operation_id) {
                if operation.state == wire::State::Succeeded {
                    let jobs = PrivateDirectory::open(&root.join("state/self-update/jobs"))?;
                    let journal = super::Journal::open(&jobs, &binding.operation_id)?;
                    if journal.read()?.phase != super::Phase::Complete {
                        bail!("hosted_update_completion_unproven");
                    }
                    match jobs.read_file("web-active.json") {
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                        _ => bail!("hosted_update_claim_unreleased"),
                    }
                    // Reopen through the real read-only projection once more;
                    // receipt labels alone do not authorize success.
                    if super::web_adapter::operation(&binding.workspace_id, &binding.server_id, &binding.operation_id)? != operation {
                        bail!("hosted_update_completion_unproven");
                    }
                    let mut output = directory.create_file("native-acceptance-result.json")?;
                    output.write_all(&serde_json::to_vec_pretty(&serde_json::json!({
                        "schema":1,"operation":operation,"native_update_acceptance":"passed",
                        "network_distributor_acceptance":"not_run","proxy_database_acceptance":"not_run",
                        "manager_lifecycle_acceptance":"not_run","capability_enabled":false,
                        "cleanup":"release_must_reconcile_owned_host_controller_and_launcher_state"
                    }))?)?;
                    output.sync_all()?;
                    directory.sync()?;
                    return Ok::<(), anyhow::Error>(());
                }
                if matches!(operation.state, wire::State::Partial | wire::State::RollbackFailed | wire::State::Failed | wire::State::RolledBack | wire::State::Unconfirmed) {
                    bail!("hosted_update_not_complete");
                }
            }
            if tokio::time::Instant::now() >= deadline { bail!("hosted_update_observation_timeout"); }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hosted_entry_requires_explicit_unmanaged_nonroot_disposable_context() {
        assert!(eligible(
            Some("1"),
            Some("true"),
            Some("github-hosted"),
            false,
            1001
        ));
        for input in [
            (None, Some("true"), Some("github-hosted"), false, 1001),
            (Some("1"), None, Some("github-hosted"), false, 1001),
            (Some("1"), Some("true"), Some("self-hosted"), false, 1001),
            (Some("1"), Some("true"), Some("github-hosted"), true, 1001),
            (Some("1"), Some("true"), Some("github-hosted"), false, 0),
        ] {
            assert!(!eligible(input.0, input.1, input.2, input.3, input.4));
        }
    }
}

#[cfg(test)]
#[path = "hosted_acceptance_tests.rs"]
mod fixture_tests;

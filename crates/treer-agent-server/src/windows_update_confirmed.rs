//! Internal plan-to-physical-input comparison, not remote caller authorization.
//! Catalog provenance, key trust and selected-runtime authority remain distinct.
use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use treer_protocol::controller_update_runtime::DispatchBinding;

use super::Binding;
use crate::controller_update::catalog::Catalog;

pub(super) fn refused() -> anyhow::Error {
    anyhow::anyhow!("windows_update_confirmed_plan_refused")
}

fn check_dispatch(dispatch: &DispatchBinding, now: DateTime<Utc>) -> Result<()> {
    if !dispatch.validate()
        || dispatch.plan_digest != dispatch.expected_plan_digest()?
        || dispatch.created_at > now + Duration::seconds(30)
        || dispatch.created_at < now - Duration::minutes(2)
    {
        return Err(refused());
    }
    Ok(())
}

/// Original in-memory data retained through claim admission. Not Clone,
/// Deserialize, a replay token, persisted authorization or a catalog-write CAS.
pub(super) struct ConfiguredPlan {
    dispatch: DispatchBinding,
    catalog_sha256: String,
}

impl ConfiguredPlan {
    #[cfg(windows)]
    pub(super) fn dispatch(&self) -> &DispatchBinding {
        &self.dispatch
    }
    fn check(
        dispatch: &DispatchBinding,
        catalog: &Catalog,
        inputs: &Binding,
        now: DateTime<Utc>,
    ) -> Result<()> {
        check_dispatch(dispatch, now)?;
        inputs.validate()?;
        catalog.select_plan(dispatch)?;
        if dispatch.operation_id != inputs.operation_id.to_string()
            || dispatch.workspace_id != inputs.workspace_id
            || dispatch.server_id != inputs.server_id
            || dispatch.current.controller_sha256 != inputs.old_controller_sha256
            || dispatch.current.cli_sha256 != inputs.old_cli_sha256
            || dispatch.target.identity.controller_sha256 != inputs.controller_sha256
            || dispatch.target.identity.cli_sha256 != inputs.cli_sha256
        {
            return Err(refused());
        }
        Ok(())
    }

    pub(super) fn new(
        dispatch: DispatchBinding,
        catalog: &Catalog,
        inputs: &Binding,
        now: DateTime<Utc>,
    ) -> Result<Self> {
        Self::check(&dispatch, catalog, inputs, now).map_err(|_| refused())?;
        Ok(Self {
            dispatch,
            catalog_sha256: catalog.raw_digest().into(),
        })
    }

    pub(super) fn recheck(
        &self,
        catalog: &Catalog,
        inputs: &Binding,
        now: DateTime<Utc>,
    ) -> Result<()> {
        Self::check(&self.dispatch, catalog, inputs, now).map_err(|_| refused())?;
        if self.catalog_sha256 != catalog.raw_digest() {
            return Err(refused());
        }
        Ok(())
    }
}

#[cfg(windows)]
impl super::AdmittedInputs {
    /// The normal authenticated/Policy-confirmed Controller route must call
    /// this with its selected runtime and independently configured public key.
    /// A well-formed DispatchBinding is never evidence of those prerequisites.
    /// No URL/path/key override is taken from that binding or signed metadata.
    #[allow(dead_code)] // No production Windows coordinator/capability entry.
    pub(crate) fn prepare_confirmed_for_controller(
        installed: treer_platform::windows_update_inputs::LockedInstallation,
        target: treer_platform::windows_update_inputs::PinnedPair,
        documents: &super::Documents<'_>,
        key: &ed25519_dalek::VerifyingKey,
        selected: &crate::controller::ControllerRuntime,
        dispatch: DispatchBinding,
    ) -> Result<Self> {
        let prepare = || -> Result<Self> {
            check_dispatch(&dispatch, Utc::now())?;
            let operation = uuid::Uuid::parse_str(&dispatch.operation_id)?;
            let state = selected.windows_update_state_for_operation(
                operation,
                &dispatch.workspace_id,
                &dispatch.server_id,
            )?;
            let catalog = Catalog::local()?;
            catalog.select_plan(&dispatch)?;
            // Existing admission authenticates both signatures and hashes all
            // four SAME held files from zero. Compare those observed bindings;
            // never replace expected pins with claimed or recomputed values.
            let mut admitted =
                Self::prepare_for_controller(installed, target, documents, key, state)?;
            admitted
                .state
                .as_ref()
                .ok_or_else(refused)?
                .matches_selected(selected, &admitted.binding)?;
            admitted.confirmed_plan = Some(ConfiguredPlan::new(
                dispatch,
                &catalog,
                &admitted.binding,
                Utc::now(),
            )?);
            Ok(admitted)
        };
        prepare().map_err(|_| refused())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use treer_protocol::controller_update as wire;

    fn fixture() -> (Value, Catalog, Binding, DispatchBinding) {
        let platform = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => wire::Platform::DarwinAarch64,
            ("macos", _) => wire::Platform::DarwinX86_64,
            ("linux", "aarch64") => wire::Platform::LinuxAarch64,
            ("linux", _) => wire::Platform::LinuxX86_64,
            _ => wire::Platform::WindowsX86_64,
        };
        let entry = |sequence, id, hash: &str| {
            json!({"release_sequence":sequence,
            "target":{"target_id":id,"identity":{"release_sha":hash.repeat(40),
            "artifact_source_sha":hash.repeat(40),"controller_sha256":hash.repeat(64),
            "cli_sha256":hash.repeat(64)},"platform":platform,"version_label":id},
            "sums_sha256":"c".repeat(64),"artifact_base":"https://example.test/fixed/",
            "allow_pinned":true,"allow_artifact_origin":false})
        };
        let doc = json!({"schema_version":1,"revision":2,"accept_shared_binaries":false,
            "entries":[entry(1,"old","a"),entry(2,"new","b")]});
        let catalog = Catalog::parse(&serde_json::to_vec(&doc).unwrap()).unwrap();
        let inputs = Binding {
            operation_id: uuid::Uuid::new_v4(),
            workspace_id: "ws_fixture".into(),
            server_id: "srv_fixture".into(),
            installation_volume: 1,
            installation_file_id: 2,
            old_controller_sha256: "a".repeat(64),
            old_cli_sha256: "a".repeat(64),
            controller_sha256: "b".repeat(64),
            cli_sha256: "b".repeat(64),
            signed_manifest_sha256: "d".repeat(64),
        };
        let mut dispatch = DispatchBinding {
            operation_id: inputs.operation_id.to_string(),
            workspace_id: inputs.workspace_id.clone(),
            server_id: inputs.server_id.clone(),
            current: catalog.entries[0].target.identity.clone(),
            target: catalog.entries[1].target.clone(),
            catalog_revision: 2,
            plan_digest: "0".repeat(64),
            created_at: Utc::now(),
        };
        dispatch.plan_digest = dispatch.expected_plan_digest().unwrap();
        (doc, catalog, inputs, dispatch)
    }

    #[test]
    fn windows_update_confirmed_plan_full_scope_and_physical_digests() {
        let (_, catalog, inputs, dispatch) = fixture();
        ConfiguredPlan::new(dispatch.clone(), &catalog, &inputs, Utc::now()).unwrap();
        for field in [
            "operation_id",
            "workspace_id",
            "server_id",
            "old_controller_sha256",
            "old_cli_sha256",
            "controller_sha256",
            "cli_sha256",
        ] {
            let mut changed = serde_json::to_value(&inputs).unwrap();
            changed[field] = match field {
                "operation_id" => json!(uuid::Uuid::new_v4()),
                "workspace_id" => json!("ws_other"),
                "server_id" => json!("srv_other"),
                _ => json!("e".repeat(64)),
            };
            let changed = serde_json::from_value(changed).unwrap();
            assert!(
                ConfiguredPlan::new(dispatch.clone(), &catalog, &changed, Utc::now()).is_err(),
                "{field}"
            );
        }
        for case in 0..3 {
            let mut bad = dispatch.clone();
            match case {
                0 => bad.catalog_revision += 1,
                1 => bad.target.version_label = "override".into(),
                _ => bad.plan_digest = "0".repeat(64),
            }
            assert!(ConfiguredPlan::new(bad, &catalog, &inputs, Utc::now()).is_err());
        }
    }

    #[test]
    fn windows_update_confirmed_plan_freshness_and_catalog_before_claim() {
        let (doc, catalog, inputs, dispatch) = fixture();
        let now = dispatch.created_at;
        let plan = ConfiguredPlan::new(dispatch, &catalog, &inputs, now).unwrap();
        plan.recheck(&catalog, &inputs, now).unwrap();
        assert!(plan
            .recheck(&catalog, &inputs, now + Duration::seconds(121))
            .is_err());
        assert!(plan
            .recheck(&catalog, &inputs, now - Duration::seconds(31))
            .is_err());
        let bytes = serde_json::to_vec_pretty(&doc).unwrap();
        let same_records = Catalog::parse(&bytes).unwrap();
        assert!(plan.recheck(&same_records, &inputs, now).is_err());
    }
}

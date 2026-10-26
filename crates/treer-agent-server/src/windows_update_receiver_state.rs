//! Runtime-derived, point-in-time startup snapshot comparison only. No restore,
//! freeze, state authority deserialization or original-owner release permit.
use super::{backup::StartupDigest, Binding};
use anyhow::{anyhow, Result};

fn refused() -> anyhow::Error {
    anyhow!("windows_update_receiver_state_refused")
}

fn checked_snapshot(
    snapshot: Option<(u64, Vec<u8>)>,
    binding: &Binding,
) -> Result<Option<(u64, StartupDigest)>> {
    binding.validate()?;
    snapshot
        .map(|(id, bytes)| {
            if id == 0 {
                return Err(refused());
            }
            let digest =
                super::backup::startup_digest(&bytes, &binding.workspace_id, &binding.server_id)
                    .map_err(|_| refused())?;
            Ok((id, digest))
        })
        .transpose()
}

#[cfg(windows)]
use super::ReceivedInputs;

#[cfg(windows)]
impl<'a> ReceivedInputs<'a> {
    /// Read only the selected runtime's loaded-store authority. Neither a
    /// caller pathname nor a private same-volume decoy can construct this stage.
    /// Consumes image admission, retaining its original sender borrow throughout.
    #[allow(dead_code)] // No production worker/IPC/activation entry.
    pub(crate) fn verify_runtime_snapshot(self) -> Result<ReceivedStateInputs<'a>> {
        let capture = || -> Result<_> {
            let sender = self.descriptor.sender;
            let state = sender.controller_state()?;
            state.matches_selected(self.descriptor.selected, sender.binding())?;
            let directory = state.directory().repin()?;
            let name = format!("{}-agent-startup.json", sender.binding().server_id);
            let expected =
                checked_snapshot(state.directory().startup_snapshot(&name)?, sender.binding())?;
            if checked_snapshot(directory.startup_snapshot(&name)?, sender.binding())? != expected {
                return Err(refused());
            }
            state.matches_selected(self.descriptor.selected, sender.binding())?;
            Ok((directory, expected))
        };
        let (directory, snapshot) = capture().map_err(|_| refused())?;
        Ok(ReceivedStateInputs {
            received: self,
            directory,
            snapshot,
            invalidated: false,
        })
    }
}

/// Independently pinned directory plus validated complete raw snapshot digest
/// and file ID, not a restore token. Raw credentials/entries are never exposed
/// or cached here. The source runtime, images, lease and claim remain borrowed.
#[cfg(windows)]
pub(crate) struct ReceivedStateInputs<'a> {
    received: ReceivedInputs<'a>,
    directory: treer_platform::windows_update_backup::BackupDirectory,
    snapshot: Option<(u64, StartupDigest)>,
    invalidated: bool,
}

#[cfg(windows)]
impl ReceivedStateInputs<'_> {
    /// Read-only reconciliation, never an automatic retry/resume. A failed
    /// read, changed snapshot or invalid fence permanently invalidates this
    /// observation; later bytes cannot repair it. Success is NOT a write CAS:
    /// legitimate StartupStore writes remain possible immediately afterwards.
    #[allow(dead_code)]
    pub(crate) fn inspect_snapshot(&mut self) -> Result<()> {
        if self.invalidated {
            return Err(refused());
        }
        self.invalidated = true;
        let inspect = || -> Result<()> {
            let sender = self.received.descriptor.sender;
            let state = sender.controller_state()?;
            state.matches_selected(self.received.descriptor.selected, sender.binding())?;
            if state.directory().identity()? != self.directory.identity()? {
                return Err(refused());
            }
            let name = format!("{}-agent-startup.json", sender.binding().server_id);
            if checked_snapshot(self.directory.startup_snapshot(&name)?, sender.binding())?
                != self.snapshot
            {
                return Err(refused());
            }
            state.matches_selected(self.received.descriptor.selected, sender.binding())?;
            Ok(())
        };
        inspect().map_err(|_| refused())?;
        self.invalidated = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn binding() -> Binding {
        Binding {
            operation_id: uuid::Uuid::new_v4(),
            workspace_id: "ws_fixture".into(),
            server_id: "srv_fixture".into(),
            installation_volume: 1,
            installation_file_id: 2,
            old_controller_sha256: "1".repeat(64),
            old_cli_sha256: "2".repeat(64),
            controller_sha256: "3".repeat(64),
            cli_sha256: "4".repeat(64),
            signed_manifest_sha256: "5".repeat(64),
        }
    }

    #[test]
    fn windows_update_receiver_state_complete_raw_fences_identity_and_absence() {
        let binding = binding();
        let bytes = br#"{"version":2,"entries":[],"denied_agent_ids":["ag_deleted"]}"#;
        let checked =
            |id, bytes: &[u8]| checked_snapshot(Some((id, bytes.to_vec())), &binding).unwrap();
        let expected = checked(7, bytes);
        assert!(checked(7, bytes) == expected);
        assert!(checked(8, bytes) != expected);
        assert!(checked(7, &[bytes.as_slice(), b" "].concat()) != expected);
        assert!(
            checked(
                7,
                br#"{"version":2,"entries":[],"denied_agent_ids":["ag_later"]}"#
            ) != expected
        );
        assert!(checked_snapshot(None, &binding).unwrap().is_none());
        assert!(expected.is_some());
        assert!(checked(7, br#"{"version":1,"entries":[]}"#).is_some());
    }

    #[test]
    fn windows_update_receiver_state_invalid_fence_scope_and_bounds_refuse() {
        let binding = binding();
        for value in [
            json!({"version":1,"entries":[],"denied_agent_ids":["ag_deleted"]}),
            json!({"version":2,"entries":[],"denied_agent_ids":["ag_deleted","ag_deleted"]}),
            json!({"version":2,"entries":[],"denied_agent_ids":["bad"]}),
            json!({"version":2,"entries":[],"denied_agent_ids":(0..4097).map(|i| format!("ag_{i}")).collect::<Vec<_>>()}),
            json!({"version":2,"denied_agent_ids":[],"entries":[{
                "agent_id":"ag_kept","server_id":"srv_other","workspace_id":"ws_fixture",
                "generation":1,"kind":"shell","name":"fixture","cwd":"private",
                "command":"fixture","args":[],"enabled":false,
                "workload_credential":"private","last_host_epoch":"fixture"}]}),
        ] {
            let error = checked_snapshot(Some((7, serde_json::to_vec(&value).unwrap())), &binding)
                .unwrap_err();
            assert_eq!(error.to_string(), "windows_update_receiver_state_refused");
        }
        for bytes in [vec![], vec![b'x'; 16 * 1024 * 1024 + 1]] {
            assert!(checked_snapshot(Some((7, bytes)), &binding).is_err());
        }
        assert!(checked_snapshot(Some((0, b"{}".to_vec())), &binding).is_err());
    }
}

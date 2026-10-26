//! Child-side signed input reception only. No ownership release, publication,
//! recovery, restart, creator-exit adoption or production bootstrap dispatch.
use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use treer_protocol::{
    controller_update_worker::{WorkerBindingFrame, WORKER_BINDING_FRAME_BYTES},
    controller_update_worker_inputs::{InputOffer, InputReceipt, INPUT_HEADER_BYTES},
};

fn refused() -> anyhow::Error {
    anyhow!("windows_update_worker_receive_refused")
}

#[cfg(windows)]
#[path = "windows_update_worker_backup.rs"]
mod backup_preparation;

fn receipt(offer: &InputOffer, bytes: &[u8]) -> InputReceipt {
    InputReceipt(WorkerBindingFrame {
        operation: offer.operation,
        challenge: offer.challenge,
        context_sha256: Sha256::digest(bytes).into(),
    })
}

#[cfg(any(windows, test))]
fn check_snapshot(
    expected: &Option<treer_protocol::controller_update_worker_inputs::Snapshot>,
    observed: Option<(u64, Vec<u8>)>,
    binding: &crate::windows_update_intent::Binding,
) -> Result<()> {
    match (observed, expected) {
        (None, None) => Ok(()),
        (Some((id, bytes)), Some(expected)) => {
            let digest =
                super::backup::startup_digest(&bytes, &binding.workspace_id, &binding.server_id)?;
            if id != expected.file_id
                || digest.size != expected.size
                || digest.sha256 != expected.sha256
            {
                return Err(refused());
            }
            Ok(())
        }
        _ => Err(refused()),
    }
}

async fn read_offer<S: AsyncRead + Unpin>(stream: &mut S) -> Result<(InputOffer, Vec<u8>)> {
    let mut header = [0; INPUT_HEADER_BYTES];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|_| refused())?;
    let size = InputOffer::body_length(&header).ok_or_else(refused)?;
    let mut bytes = header.to_vec();
    bytes.resize(INPUT_HEADER_BYTES + size, 0);
    stream
        .read_exact(&mut bytes[INPUT_HEADER_BYTES..])
        .await
        .map_err(|_| refused())?;
    let offer = InputOffer::decode(&bytes).ok_or_else(refused)?;
    Ok((offer, bytes))
}

/// Owns the only sender-side stream. All outcomes close it, including cancelled
/// futures. Success says only "received"; sender's borrowed reservation lives on.
pub(crate) async fn exchange<S>(mut stream: S, offer: InputOffer) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    exchange_retained(&mut stream, offer).await.map(|_| ())
}

/// Same initial exchange, but retain the transport for read-only custody control.
pub(crate) async fn exchange_retained<S>(
    stream: &mut S,
    offer: InputOffer,
) -> Result<WorkerBindingFrame>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    tokio::time::timeout_at(deadline, async {
        let bytes = offer.encode().ok_or_else(refused)?;
        let expected = receipt(&offer, &bytes).0;
        stream.write_all(&bytes).await.map_err(|_| refused())?;
        let mut ack = [0; WORKER_BINDING_FRAME_BYTES];
        stream.read_exact(&mut ack).await.map_err(|_| refused())?;
        if tokio::time::Instant::now() >= deadline
            || InputReceipt::decode(&ack).map(|r| r.0) != Some(expected)
        {
            return Err(refused());
        }
        Ok(expected)
    })
    .await
    .map_err(|_| refused())?
}

#[cfg(windows)]
pub(crate) mod native {
    use super::*;
    use crate::{
        controller::ControllerRuntime,
        windows_update_admission::{Documents, ReservedInputs},
    };
    use treer_platform::{
        windows_update_backup::BackupDirectory,
        windows_update_inputs::{InheritedInstallationLock, PairIdentity, PinnedPair},
        windows_update_pipe::CreationParentConnection,
    };
    use treer_protocol::controller_update_worker_inputs::{PairLocation, Snapshot, StateLocation};

    fn pair(pair: &PinnedPair) -> Result<PairLocation> {
        let id = pair.pair_identity()?;
        Ok(PairLocation {
            directory: pair.directory_path().to_str().ok_or_else(refused)?.into(),
            identities: [id.directory, id.controller, id.cli],
        })
    }

    impl ReservedInputs {
        /// Normal identity/Policy and confirmed local-plan prerequisites remain
        /// upstream. Caller cannot select paths or replace the runtime authority.
        /// The creator retains this entire reservation after receipt or unknown.
        pub(crate) fn worker_input_offer(
            &self,
            selected: &ControllerRuntime,
            context: WorkerBindingFrame,
            documents: &Documents<'_>,
        ) -> Result<InputOffer> {
            let mint = || -> Result<_> {
                let state = self.controller_state()?;
                state.matches_selected(selected, self.binding())?;
                if context.operation != *self.binding().operation_id.as_bytes() {
                    return Err(refused());
                }
                let plan = self.0.confirmed_plan.as_ref().ok_or_else(refused)?;
                plan.recheck(
                    &crate::controller_update::catalog::Catalog::local()?,
                    self.binding(),
                    chrono::Utc::now(),
                )?;
                let installed = self.0._installed.repin_images()?;
                let target = self.0._target.repin()?;
                let key = ed25519_dalek::VerifyingKey::from_bytes(&self.0.verifying_key)?;
                let (mut old_c, mut old_cli) = installed.files();
                let (mut new_c, mut new_cli) = target.files();
                super::super::verify_received_inputs(
                    documents,
                    &key,
                    self.binding(),
                    [&mut old_c, &mut old_cli, &mut new_c, &mut new_cli],
                )?;
                let snapshot = state
                    .directory()
                    .startup_snapshot(&format!("{}-agent-startup.json", self.binding().server_id))?
                    .map(|(file_id, bytes)| {
                        let digest = super::super::backup::startup_digest(
                            &bytes,
                            &self.binding().workspace_id,
                            &self.binding().server_id,
                        )?;
                        Ok::<_, anyhow::Error>(Snapshot {
                            file_id,
                            size: digest.size,
                            sha256: digest.sha256,
                        })
                    })
                    .transpose()?;
                state.matches_selected(selected, self.binding())?;
                Ok(InputOffer {
                    schema_version: 1,
                    operation: context.operation,
                    challenge: context.challenge,
                    runtime_context_sha256: context.context_sha256,
                    dispatch: plan.dispatch().clone(),
                    signed_manifest_sha256: self.binding().signed_manifest_sha256.clone(),
                    lock: self.installation_lock_identity()?,
                    installed: pair(&installed)?,
                    target: pair(&target)?,
                    state: StateLocation {
                        directory: state
                            .directory()
                            .directory_path()
                            .to_str()
                            .ok_or_else(refused)?
                            .into(),
                        identity: state.directory().identity()?,
                        snapshot,
                    },
                    channel_name: documents.channel_name.into(),
                    channel: documents.channel.into(),
                    channel_signature: documents.channel_signature.into(),
                    manifest: documents.manifest.into(),
                    manifest_signature: documents.manifest_signature.into(),
                })
            };
            mint().map_err(|_| refused())
        }
    }

    /// Independent receiver custody. No conversion to ReservedInputs, mutation,
    /// resume or release token exists; dropping it never proves an update.
    #[allow(dead_code)]
    pub(crate) struct WorkerReceivedInputs {
        stream: CreationParentConnection,
        lock: InheritedInstallationLock,
        installed: PinnedPair,
        target: PinnedPair,
        state: BackupDirectory,
        binding: crate::windows_update_intent::Binding,
        snapshot: Option<Snapshot>,
        runtime_context_sha256: [u8; 32],
        snapshot_invalidated: bool,
        custody_context: WorkerBindingFrame,
        backup_attempted: bool,
        backup: Option<super::backup_preparation::WorkerBackup>,
    }

    #[allow(dead_code)] // Internal continuation; production coordinator is not wired.
    impl WorkerReceivedInputs {
        /// Finite preparation session only. Keep all receiver guards while
        /// serving exact ordered inspections; close is explicit and read-only.
        pub(crate) async fn serve_custody(mut self) -> Result<()> {
            use treer_protocol::controller_update_worker_inputs::{
                CustodyFrame, CustodyKind, CUSTODY_BYTES, CUSTODY_MAX_STEPS,
            };
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
            tokio::time::timeout_at(deadline, async {
                for sequence in 1..=CUSTODY_MAX_STEPS {
                    let mut bytes = [0; CUSTODY_BYTES];
                    self.stream
                        .read_exact(&mut bytes)
                        .await
                        .map_err(|_| refused())?;
                    let (frame, reply, close, backup) = if let Some(frame) =
                        CustodyFrame::decode(&bytes, CustodyKind::Inspect)
                    {
                        (frame, CustodyKind::Inspected, false, false)
                    } else if let Some(frame) = CustodyFrame::decode(&bytes, CustodyKind::Close) {
                        (frame, CustodyKind::Closed, true, false)
                    } else if let Some(frame) =
                        CustodyFrame::decode(&bytes, CustodyKind::PrepareBackup)
                    {
                        (frame, CustodyKind::BackupPrepared, false, true)
                    } else {
                        return Err(refused());
                    };
                    if frame.context != self.custody_context || frame.sequence != sequence {
                        return Err(refused());
                    }
                    self.inspect_snapshot()?;
                    if backup {
                        if self.backup_attempted {
                            return Err(refused());
                        }
                        // Explicit command precedes every backup write. Consume
                        // before the first effect; partial/unknown is not retry.
                        self.backup_attempted = true;
                        let prepared = super::backup_preparation::prepare(
                            &self.installed,
                            &self.target,
                            &self.state,
                            &self.binding,
                            &self.snapshot,
                            self.custody_context,
                        )
                        .map_err(|_| refused())?;
                        // Keep independent receiver inputs AND backup guards
                        // before the reply await. ACK does not release sender.
                        self.backup = Some(prepared);
                        self.inspect_snapshot()?;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Err(refused());
                    }
                    self.stream
                        .write_all(&frame.encode(reply).ok_or_else(refused)?)
                        .await
                        .map_err(|_| refused())?;
                    if tokio::time::Instant::now() >= deadline {
                        return Err(refused());
                    }
                    if close {
                        return Ok(());
                    }
                }
                Err(refused())
            })
            .await
            .map_err(|_| refused())?
        }

        /// Read-only point-in-time recheck against this custody's original raw
        /// snapshot, never a freeze, CAS, authorization or release permit. Any
        /// observation error permanently poisons this object, even if a later
        /// caller restores the old bytes. Custody itself remains held on error.
        pub(crate) fn inspect_snapshot(&mut self) -> Result<()> {
            if self.snapshot_invalidated {
                return Err(refused());
            }
            let check = || -> Result<()> {
                if !self.stream.creator_is_live()? {
                    return Err(refused());
                }
                let observed = self
                    .state
                    .startup_snapshot(&format!("{}-agent-startup.json", self.binding.server_id))?;
                check_snapshot(&self.snapshot, observed, &self.binding)?;
                if let Some(backup) = &self.backup {
                    backup.inspect(&self.binding, &self.snapshot)?;
                }
                if !self.stream.creator_is_live()? {
                    return Err(refused());
                }
                Ok(())
            };
            if check().is_err() {
                self.snapshot_invalidated = true;
                return Err(refused());
            }
            Ok(())
        }
    }

    fn open_pair(location: &PairLocation) -> Result<PinnedPair> {
        Ok(PinnedPair::open_matching(
            std::path::Path::new(&location.directory),
            PairIdentity {
                directory: location.identities[0],
                controller: location.identities[1],
                cli: location.identities[2],
            },
        )?)
    }

    fn verify(
        offer: &InputOffer,
        key: &ed25519_dalek::VerifyingKey,
    ) -> Result<(
        PinnedPair,
        PinnedPair,
        BackupDirectory,
        crate::windows_update_intent::Binding,
    )> {
        let dispatch = &offer.dispatch;
        let op = uuid::Uuid::parse_str(&dispatch.operation_id)?;
        if *op.as_bytes() != offer.operation
            || dispatch.target.platform
                != treer_protocol::controller_update::Platform::WindowsX86_64
            || dispatch.expected_plan_digest()? != dispatch.plan_digest
            || dispatch.created_at > chrono::Utc::now() + chrono::Duration::seconds(30)
            || dispatch.created_at < chrono::Utc::now() - chrono::Duration::minutes(2)
        {
            return Err(refused());
        }
        let installed = open_pair(&offer.installed)?;
        let target = open_pair(&offer.target)?;
        if installed.identity()? == target.identity()? {
            return Err(refused());
        }
        let binding = crate::windows_update_intent::Binding {
            operation_id: op,
            workspace_id: dispatch.workspace_id.clone(),
            server_id: dispatch.server_id.clone(),
            installation_volume: offer.installed.identities[0].0,
            installation_file_id: offer.installed.identities[0].1,
            old_controller_sha256: dispatch.current.controller_sha256.clone(),
            old_cli_sha256: dispatch.current.cli_sha256.clone(),
            controller_sha256: dispatch.target.identity.controller_sha256.clone(),
            cli_sha256: dispatch.target.identity.cli_sha256.clone(),
            signed_manifest_sha256: offer.signed_manifest_sha256.clone(),
        };
        let documents = Documents {
            channel: &offer.channel,
            channel_signature: &offer.channel_signature,
            manifest: &offer.manifest,
            manifest_signature: &offer.manifest_signature,
            channel_name: &offer.channel_name,
        };
        let (mut old_c, mut old_cli) = installed.files();
        let (mut new_c, mut new_cli) = target.files();
        super::super::verify_received_inputs(
            &documents,
            key,
            &binding,
            [&mut old_c, &mut old_cli, &mut new_c, &mut new_cli],
        )?;
        let state = BackupDirectory::open(std::path::Path::new(&offer.state.directory))?;
        if state.identity()? != offer.state.identity
            || [installed.identity()?, target.identity()?].contains(&state.identity()?)
        {
            return Err(refused());
        }
        let observed =
            state.startup_snapshot(&format!("{}-agent-startup.json", binding.server_id))?;
        check_snapshot(&offer.state.snapshot, observed, &binding)?;
        Ok((installed, target, state, binding))
    }

    /// Child-side one-shot receive from its original creator's pipe. The key
    /// MUST come from independent installed trust, never the offer. Creator
    /// identity and runtime context are not independently established Policy.
    /// The separate private terminal receiver supplies configuration-selected
    /// trust; product coordinator/capability activation remains unwired.
    #[allow(dead_code)]
    pub(crate) async fn receive_creator_inputs(
        selector: &str,
        operation: [u8; 16],
        key: &ed25519_dalek::VerifyingKey,
    ) -> Result<WorkerReceivedInputs> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut stream =
            treer_platform::windows_update_pipe::connect_creation_parent_pipe(selector)
                .map_err(|_| refused())?;
        tokio::time::timeout_at(deadline, async move {
            let (offer, bytes) = read_offer(&mut stream).await?;
            if offer.operation != operation || !stream.creator_is_live()? {
                return Err(refused());
            }
            let lock = InheritedInstallationLock::retain_from_creator(&stream, offer.lock)?;
            let (installed, target, state, binding) = verify(&offer, key)?;
            if tokio::time::Instant::now() >= deadline || !stream.creator_is_live()? {
                return Err(refused());
            }
            stream
                .write_all(&receipt(&offer, &bytes).encode().ok_or_else(refused)?)
                .await
                .map_err(|_| refused())?;
            if tokio::time::Instant::now() >= deadline || !stream.creator_is_live()? {
                return Err(refused());
            }
            let custody_context = receipt(&offer, &bytes).0;
            Ok(WorkerReceivedInputs {
                stream,
                lock,
                installed,
                target,
                state,
                binding,
                snapshot: offer.state.snapshot,
                runtime_context_sha256: offer.runtime_context_sha256,
                snapshot_invalidated: false,
                custody_context,
                backup_attempted: false,
                backup: None,
            })
        })
        .await
        .map_err(|_| refused())?
        .map_err(|_| refused())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_update_worker_state_current_raw_fences_identity_and_absence() {
        use treer_protocol::controller_update_worker_inputs::Snapshot;
        let binding = crate::windows_update_intent::Binding {
            operation_id: uuid::Uuid::new_v4(),
            workspace_id: "ws_fixture".into(),
            server_id: "srv_fixture".into(),
            installation_volume: 1,
            installation_file_id: 2,
            old_controller_sha256: "a".repeat(64),
            old_cli_sha256: "b".repeat(64),
            controller_sha256: "c".repeat(64),
            cli_sha256: "d".repeat(64),
            signed_manifest_sha256: "e".repeat(64),
        };
        let raw = br#"{"version":2,"entries":[],"denied_agent_ids":["ag_deleted"]}"#;
        let expected = Some(Snapshot {
            file_id: 9,
            size: raw.len() as u64,
            sha256: format!("{:x}", Sha256::digest(raw)),
        });
        assert!(check_snapshot(&expected, Some((9, raw.to_vec())), &binding).is_ok());
        assert!(check_snapshot(&expected, Some((8, raw.to_vec())), &binding).is_err());
        assert!(check_snapshot(&expected, None, &binding).is_err());
        assert!(check_snapshot(&None, None, &binding).is_ok());
        assert!(check_snapshot(&None, Some((9, raw.to_vec())), &binding).is_err());
        for changed in [
            br#"{"version":2,"entries":[],"denied_agent_ids":["ag_deleted","ag_later"]}"#
                .as_slice(),
            br#"{"version":1,"entries":[],"denied_agent_ids":["ag_deleted"]}"#,
            br#"{"version":2,"entries":[],"denied_agent_ids":["ag_deleted","ag_deleted"]}"#,
        ] {
            assert!(check_snapshot(&expected, Some((9, changed.to_vec())), &binding).is_err());
        }
    }

    fn offer() -> InputOffer {
        use treer_protocol::{
            controller_update::{Identity, Platform, Target},
            controller_update_runtime::DispatchBinding,
            controller_update_worker_inputs::{PairLocation, StateLocation},
        };
        let identity = |s: &str| Identity {
            release_sha: s.repeat(40),
            artifact_source_sha: s.repeat(40),
            controller_sha256: s.repeat(64),
            cli_sha256: s.repeat(64),
        };
        let op = uuid::Uuid::new_v4();
        let mut dispatch = DispatchBinding {
            operation_id: op.to_string(),
            workspace_id: "ws_fixture".into(),
            server_id: "srv_fixture".into(),
            current: identity("a"),
            target: Target {
                target_id: "new".into(),
                identity: identity("b"),
                platform: Platform::WindowsX86_64,
                version_label: "new".into(),
            },
            catalog_revision: 1,
            plan_digest: "0".repeat(64),
            created_at: chrono::Utc::now(),
        };
        dispatch.plan_digest = dispatch.expected_plan_digest().unwrap();
        InputOffer {
            schema_version: 1,
            operation: *op.as_bytes(),
            challenge: *uuid::Uuid::new_v4().as_bytes(),
            runtime_context_sha256: [4; 32],
            dispatch,
            signed_manifest_sha256: "c".repeat(64),
            lock: (1, 2),
            installed: PairLocation {
                directory: "C:/installed".into(),
                identities: [(1, 3), (1, 4), (1, 5)],
            },
            target: PairLocation {
                directory: "C:/target".into(),
                identities: [(1, 6), (1, 7), (1, 8)],
            },
            state: StateLocation {
                directory: "C:/state".into(),
                identity: (1, 9),
                snapshot: None,
            },
            channel_name: "stable".into(),
            channel: vec![1],
            channel_signature: vec![2],
            manifest: vec![3],
            manifest_signature: vec![4],
        }
    }
    #[tokio::test]
    async fn windows_update_worker_receive_partial_header_deadline_closes_transport() {
        for wrong in [false, true] {
            let (parent, mut child) = tokio::io::duplex(8192);
            let child = async {
                let (offer, bytes) = read_offer(&mut child).await.unwrap();
                let mut ack = receipt(&offer, &bytes).encode().unwrap();
                if wrong {
                    ack[24] ^= 1;
                }
                child.write_all(&ack).await.unwrap();
            };
            let (result, ()) = tokio::join!(exchange(parent, offer()), child);
            assert_eq!(result.is_err(), wrong);
        }
        let (parent, mut child) = tokio::io::duplex(64);
        let task = tokio::spawn(async move {
            tokio::time::timeout(std::time::Duration::from_millis(10), async {
                let mut parent = parent;
                read_offer(&mut parent).await
            })
            .await
        });
        child.write_all(b"TRWIR").await.unwrap();
        assert!(task.await.unwrap().is_err());
        assert_eq!(child.read(&mut [0]).await.unwrap(), 0);
    }
}

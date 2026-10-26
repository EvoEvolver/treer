//! In-process state and created-Child custody only. No wire constructor,
//! environment/default resolver, admitted worker or resumable operation.
use super::ControllerRuntime;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Component, Path};
#[cfg(test)]
use std::process::Command;
use std::process::{Child, ExitStatus};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ControllerStateEvidence {
    controller_instance_id: Uuid,
    host_epoch_sha256: String,
}

/// Not Clone or Deserialize. Keeping the runtime and physical directory alive
/// does not freeze startup-store writes or confer Host/process ownership.
pub(crate) struct ControllerUpdateState {
    runtime: ControllerRuntime,
    operation_id: Uuid,
    workspace_id: String,
    server_id: String,
    evidence: ControllerStateEvidence,
    restart_attempted: bool,
    #[cfg(windows)]
    directory: treer_platform::windows_update_backup::BackupDirectory,
}

fn validate_path(path: &Path, server_id: &str) -> Result<()> {
    let filename = format!("{server_id}-agent-startup.json");
    if !path.is_absolute()
        || path.file_name() != Some(std::ffi::OsStr::new(&filename))
        || path
            .components()
            .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
        || path.parent().is_none()
    {
        bail!("windows_update_runtime_state_path_refused");
    }
    Ok(())
}

impl ControllerRuntime {
    #[cfg(windows)]
    pub(crate) fn check_update_enrollment(
        &self,
        workspace: &str,
        server: &str,
        root: &Path,
    ) -> Result<()> {
        if self.inner.workspace_id != workspace
            || self.inner.server_id != server
            || self.inner.root != root
        {
            bail!("windows_update_installation_runtime_mismatch");
        }
        Ok(())
    }
    /// Called only after normal caller authentication/Policy and operation
    /// selection. No exposed update command/capability is introduced here.
    #[allow(dead_code)] // Production Windows coordinator is not wired yet.
    pub(crate) fn windows_update_state_for_operation(
        &self,
        operation_id: Uuid,
        workspace_id: &str,
        server_id: &str,
    ) -> Result<ControllerUpdateState> {
        if operation_id.get_version_num() != 4
            || operation_id.get_variant() != uuid::Variant::RFC4122
            || workspace_id != self.inner.workspace_id
            || server_id != self.inner.server_id
            || !crate::windows_update_intent::identifier(workspace_id, "ws_")
            || !crate::windows_update_intent::identifier(server_id, "srv_")
            || self.inner.host_epoch.is_empty()
        {
            bail!("windows_update_runtime_scope_refused");
        }
        // This is the SAME store loaded by from_sync, not caller-supplied config
        // or a second resolution under the updater's ambient environment.
        let path = self.inner.startup_store.configured_path();
        validate_path(path, server_id)?;
        Ok(ControllerUpdateState {
            runtime: self.clone(),
            operation_id,
            workspace_id: workspace_id.into(),
            server_id: server_id.into(),
            evidence: ControllerStateEvidence {
                controller_instance_id: self.inner.update_instance_id,
                host_epoch_sha256: format!(
                    "{:x}",
                    Sha256::digest(self.inner.host_epoch.as_bytes())
                ),
            },
            restart_attempted: false,
            #[cfg(windows)]
            directory: treer_platform::windows_update_backup::BackupDirectory::open(
                path.parent().unwrap(),
            )?,
        })
    }
}

impl ControllerUpdateState {
    /// Internal creator-side request only; not a worker authorization or exposed
    /// coordinator. The creator may itself stop before receiving an ACK, so no
    /// transport result proves health/completion or permits automatic retry.
    #[allow(dead_code)] // Production mutation admission/coordinator remains unwired.
    pub(crate) async fn request_selected_controller_restart(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<()> {
        self.matches_selected(selected, binding)?;
        if self.restart_attempted {
            bail!("windows_update_selected_restart_consumed");
        }
        // Before the first await, including the observation: cancellation or
        // lost response must not allow this owner to initiate another attempt.
        self.restart_attempted = true;
        let refused = || anyhow::anyhow!("windows_update_selected_restart_unconfirmed");
        let expected = self
            .observe_selected_controller(selected, binding)
            .await
            .map_err(|_| refused())?;
        self.matches_selected(selected, binding)?;
        let response = self
            .runtime
            .inner
            .host
            .request(
                treer_host_protocol::HostCommand::RestartControllerIfCurrent { expected },
                Some(self.operation_id.to_string()),
            )
            .await
            .map_err(|_| refused())?;
        self.matches_selected(selected, binding)?;
        if response != treer_host_protocol::HostResponse::Ack {
            return Err(refused());
        }
        Ok(())
    }

    /// Read through this selected runtime's existing Host connection only.
    /// The returned wire value is point-in-time observation, NOT an authority
    /// constructor, persisted proof, restart token or readiness claim.
    #[allow(dead_code)] // Internal observation; production coordinator remains unwired.
    pub(crate) async fn observe_selected_controller(
        &self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<treer_host_protocol::HostControllerIdentity> {
        self.matches_selected(selected, binding)?;
        let refused = || anyhow::anyhow!("windows_update_selected_host_unconfirmed");
        let response = self
            .runtime
            .inner
            .host
            .request(treer_host_protocol::HostCommand::ObserveController, None)
            .await
            .map_err(|_| refused())?;
        self.matches_selected(selected, binding)?;
        let treer_host_protocol::HostResponse::ControllerIdentity { identity } = response else {
            return Err(refused());
        };
        let instance = Uuid::parse_str(&identity.instance_id).map_err(|_| refused())?;
        if identity.host_epoch != self.runtime.inner.host_epoch
            || identity.pid != std::process::id()
            || instance.to_string() != identity.instance_id
            || instance.get_version_num() != 4
            || instance.get_variant() != uuid::Variant::RFC4122
        {
            return Err(refused());
        }
        Ok(identity)
    }

    pub(crate) fn scope(&self) -> (Uuid, String, String) {
        (
            self.operation_id,
            self.workspace_id.clone(),
            self.server_id.clone(),
        )
    }

    pub(crate) fn matches(&self, binding: &crate::windows_update_intent::Binding) -> Result<()> {
        binding.validate()?;
        if self.scope()
            != (
                binding.operation_id,
                binding.workspace_id.clone(),
                binding.server_id.clone(),
            )
            || self.runtime.inner.update_instance_id != self.evidence.controller_instance_id
            || self.runtime.inner.workspace_id != self.workspace_id
            || self.runtime.inner.server_id != self.server_id
        {
            bail!("windows_update_runtime_binding_refused");
        }
        Ok(())
    }

    pub(crate) fn evidence(&self) -> &ControllerStateEvidence {
        &self.evidence
    }

    pub(crate) fn matches_selected(
        &self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<()> {
        self.matches(binding)?;
        if selected.inner.update_instance_id != self.evidence.controller_instance_id
            || format!("{:x}", Sha256::digest(selected.inner.host_epoch.as_bytes()))
                != self.evidence.host_epoch_sha256
            || selected.inner.workspace_id != self.workspace_id
            || selected.inner.server_id != self.server_id
        {
            bail!("windows_update_child_runtime_refused");
        }
        Ok(())
    }

    #[cfg(windows)]
    pub(crate) fn directory(&self) -> &treer_platform::windows_update_backup::BackupDirectory {
        &self.directory
    }
}

/// Linear custody of the Child returned by this creator's spawn, never a PID,
/// ProcessObservation or deserialized receipt. No image authentication, Job
/// breakaway, restart authority or readiness claim follows from this object.
/// The Windows reservation retains pins/lease/state while this owner exists;
/// dropping after unknown teardown releases custody without proving child exit.
pub(crate) struct ControllerUpdateChild {
    child: Child,
    runtime: ControllerRuntime,
    evidence: ControllerStateEvidence,
    binding: crate::windows_update_intent::Binding,
    stop_attempted: bool,
    job_check_attempted: bool,
    pipe_attempted: bool,
    handshake_attempted: bool,
    binding_acknowledged: bool,
    completion_attempted: bool,
    #[cfg(windows)]
    _worker_pipe: Option<tokio::net::windows::named_pipe::NamedPipeServer>,
    #[cfg(windows)]
    _bootstrap_pipe: Option<treer_platform::windows_update_pipe::CreatedChildPipe>,
    #[cfg(windows)]
    _reservation: Option<crate::windows_update_admission::ReservedInputs>,
}

#[allow(dead_code)] // Internal primitive; production worker admission is not wired.
impl ControllerUpdateChild {
    #[cfg(all(windows, test))]
    pub(crate) fn spawn_reserved(
        reservation: crate::windows_update_admission::ReservedInputs,
        selected: &ControllerRuntime,
        command: &mut Command,
    ) -> Result<Self> {
        let state = reservation.controller_state()?;
        state.matches_selected(selected, reservation.binding())?;
        let runtime = state.runtime.clone();
        let evidence = state.evidence.clone();
        let binding = reservation.binding().clone();
        // There is no fallible operation after spawn before Child is owned.
        let child = command
            .spawn()
            .map_err(|_| anyhow::anyhow!("windows_update_child_spawn_failed"))?;
        Ok(Self {
            child,
            runtime,
            evidence,
            binding,
            stop_attempted: false,
            job_check_attempted: false,
            pipe_attempted: false,
            handshake_attempted: false,
            binding_acknowledged: false,
            completion_attempted: false,
            _worker_pipe: None,
            _bootstrap_pipe: None,
            _reservation: Some(reservation),
        })
    }

    #[cfg(windows)]
    pub(crate) fn spawn_verified_reserved(
        mut reservation: crate::windows_update_admission::ReservedInputs,
        selected: &ControllerRuntime,
    ) -> Result<Self> {
        let state = reservation.controller_state()?;
        state.matches_selected(selected, reservation.binding())?;
        let runtime = state.runtime.clone();
        let evidence = state.evidence.clone();
        let binding = reservation.binding().clone();
        let selector = Uuid::new_v4();
        let pipe = treer_platform::windows_update_pipe::CreatedChildPipe::bind(
            &selector.simple().to_string(),
        )?;
        let child = reservation.spawn_worker_process_for_pipe(selector)?;
        Ok(Self {
            child,
            runtime,
            evidence,
            binding,
            stop_attempted: false,
            job_check_attempted: false,
            pipe_attempted: false,
            handshake_attempted: false,
            binding_acknowledged: false,
            completion_attempted: false,
            _worker_pipe: None,
            _bootstrap_pipe: Some(pipe),
            _reservation: Some(reservation),
        })
    }

    /// Fixed signed image + configuration-selected receiver trust. Failure
    /// returns the original reservation so callers retain pins/lease/claim even
    /// after an unknown creation result. No fallback to the legacy ACK worker.
    #[cfg(windows)]
    pub(crate) fn spawn_verified_input_reserved(
        reservation: crate::windows_update_admission::ReservedInputs,
        selected: &ControllerRuntime,
        trust: &crate::windows_update_worker_entry::Trust,
    ) -> std::result::Result<
        Self,
        (
            anyhow::Error,
            Box<crate::windows_update_admission::ReservedInputs>,
        ),
    > {
        Self::spawn_input_mode(reservation, selected, trust, false)
    }

    #[cfg(windows)]
    pub(crate) fn spawn_holding_input_reserved(
        reservation: crate::windows_update_admission::ReservedInputs,
        selected: &ControllerRuntime,
        trust: &crate::windows_update_worker_entry::Trust,
    ) -> std::result::Result<
        Self,
        (
            anyhow::Error,
            Box<crate::windows_update_admission::ReservedInputs>,
        ),
    > {
        Self::spawn_input_mode(reservation, selected, trust, true)
    }

    #[cfg(windows)]
    fn spawn_input_mode(
        mut reservation: crate::windows_update_admission::ReservedInputs,
        selected: &ControllerRuntime,
        trust: &crate::windows_update_worker_entry::Trust,
        hold: bool,
    ) -> std::result::Result<
        Self,
        (
            anyhow::Error,
            Box<crate::windows_update_admission::ReservedInputs>,
        ),
    > {
        let mut spawn = || -> Result<_> {
            let state = reservation.controller_state()?;
            state.matches_selected(selected, reservation.binding())?;
            let runtime = state.runtime.clone();
            let evidence = state.evidence.clone();
            let binding = reservation.binding().clone();
            let selector = Uuid::new_v4();
            let pipe = treer_platform::windows_update_pipe::CreatedChildPipe::bind(
                &selector.simple().to_string(),
            )?;
            let child = if hold {
                reservation.spawn_holding_process_for_pipe(selector, trust)?
            } else {
                reservation.spawn_input_process_for_pipe(selector, trust)?
            };
            Ok((runtime, evidence, binding, pipe, child))
        };
        match spawn() {
            Ok((runtime, evidence, binding, pipe, child)) => Ok(Self {
                child,
                runtime,
                evidence,
                binding,
                stop_attempted: false,
                job_check_attempted: false,
                pipe_attempted: false,
                handshake_attempted: false,
                binding_acknowledged: false,
                completion_attempted: false,
                _worker_pipe: None,
                _bootstrap_pipe: Some(pipe),
                _reservation: Some(reservation),
            }),
            Err(error) => Err((error, Box::new(reservation))),
        }
    }

    #[cfg(test)]
    pub(super) fn spawn_fixture(
        state: &ControllerUpdateState,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
        command: &mut Command,
    ) -> Result<Self> {
        state.matches_selected(selected, binding)?;
        let child = command.spawn()?;
        Ok(Self {
            child,
            runtime: state.runtime.clone(),
            evidence: state.evidence.clone(),
            binding: binding.clone(),
            stop_attempted: false,
            job_check_attempted: false,
            pipe_attempted: false,
            handshake_attempted: false,
            binding_acknowledged: false,
            completion_attempted: false,
            #[cfg(windows)]
            _worker_pipe: None,
            #[cfg(windows)]
            _bootstrap_pipe: None,
            #[cfg(windows)]
            _reservation: None,
        })
    }

    fn matches(
        &self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<()> {
        binding.validate()?;
        if binding != &self.binding
            || selected.inner.update_instance_id != self.evidence.controller_instance_id
            || self.runtime.inner.update_instance_id != self.evidence.controller_instance_id
            || selected.inner.workspace_id != binding.workspace_id
            || selected.inner.server_id != binding.server_id
            || format!("{:x}", Sha256::digest(selected.inner.host_epoch.as_bytes()))
                != self.evidence.host_epoch_sha256
        {
            bail!("windows_update_child_scope_refused");
        }
        Ok(())
    }

    #[cfg(windows)]
    pub(crate) fn verify_receiver_scope(
        &self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<()> {
        self.matches(selected, binding)
    }

    /// Exact original exit observation moves custody back to the coordinator;
    /// it does not retire the durable claim or authorize another worker.
    #[cfg(windows)]
    pub(crate) fn retain_after_receiver_exit(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<crate::windows_update_admission::ReservedInputs> {
        self.matches(selected, binding)?;
        match self.child.try_wait() {
            Ok(Some(status)) if status.success() => {
                self.stop_attempted = true;
                self._reservation
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("windows_update_receiver_custody_missing"))
            }
            _ => bail!("windows_update_receiver_exit_unconfirmed"),
        }
    }

    pub(crate) fn poll(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<Option<ExitStatus>> {
        self.matches(selected, binding)?;
        if self.stop_attempted {
            bail!("windows_update_child_stop_already_attempted");
        }
        match self.child.try_wait() {
            Ok(status) => Ok(status),
            Err(_) => {
                self.stop_attempted = true;
                bail!("windows_update_child_observation_unknown");
            }
        }
    }

    // Scope refusal does not consume another runtime's check. Once the exact
    // owner starts the query, success OR error consumes it. No serialized proof
    // or new lifecycle authority is returned; pins/reservation remain owned.
    #[cfg(any(windows, test))]
    fn check_job_boundary(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
        query: impl FnOnce(&Child) -> Result<()>,
    ) -> Result<()> {
        self.matches(selected, binding)?;
        if self.stop_attempted || self.job_check_attempted {
            bail!("windows_update_child_job_check_refused");
        }
        self.job_check_attempted = true;
        query(&self.child)
    }

    /// Only the created Child, never ProcessObservation/PID adoption. This is
    /// an instantaneous prerequisite for a future handoff, not readiness,
    /// authenticated IPC, successful breakaway or continued survival evidence.
    #[cfg(windows)]
    pub(crate) fn check_worker_job_boundary(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<()> {
        self.check_job_boundary(selected, binding, |child| {
            treer_platform::windows_update_process::require_created_child_outside_jobs(child)
                .map_err(|_| anyhow::anyhow!("windows_update_child_job_unconfirmed"))
        })
    }

    fn begin_pipe_accept(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<()> {
        self.matches(selected, binding)?;
        if self.stop_attempted || self.pipe_attempted {
            bail!("windows_update_child_pipe_already_attempted");
        }
        // Before awaiting: cancellation must not make the owner reconnectable.
        self.pipe_attempted = true;
        Ok(())
    }

    /// Parent-side transport peer check only. The selected scope and original
    /// Child remain owned; no wire value can construct them. This does not
    /// authenticate the server to the child, transfer locks, parse commands,
    /// establish outside-Job survival or claim worker readiness.
    #[cfg(windows)]
    pub(crate) async fn accept_worker_pipe(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
        pipe: treer_platform::windows_update_pipe::CreatedChildPipe,
    ) -> Result<()> {
        self.begin_pipe_accept(selected, binding)?;
        self._worker_pipe = Some(
            pipe.connect(&self.child)
                .await
                .map_err(|_| anyhow::anyhow!("windows_update_child_pipe_unconfirmed"))?,
        );
        Ok(())
    }

    fn begin_handshake(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<treer_protocol::controller_update_worker::WorkerBindingFrame> {
        self.matches(selected, binding)?;
        if self.stop_attempted || !self.pipe_attempted || self.handshake_attempted {
            bail!("windows_update_handshake_already_attempted");
        }
        // Latch before any fallible encoding or await. No cancellation or
        // unknown ACK permits another exchange on this owner.
        self.handshake_attempted = true;
        let mut digest = Sha256::new();
        digest.update(b"treer-windows-worker-binding-v1\0");
        digest.update(serde_json::to_vec(&(&self.binding, &self.evidence))?);
        Ok(
            treer_protocol::controller_update_worker::WorkerBindingFrame {
                operation: *self.binding.operation_id.as_bytes(),
                context_sha256: digest.finalize().into(),
                challenge: *Uuid::new_v4().as_bytes(),
            },
        )
    }

    /// One distinct receipt only after the real receiver verifies signed whole
    /// inputs and the runtime-derived state snapshot. Reservation/pins remain
    /// owned here; neither receipt nor creator exit releases them or grants a
    /// lifecycle action. Shares the handshake latch: no downgrade or retry.
    #[cfg(windows)]
    pub(crate) async fn exchange_worker_inputs(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
        documents: &crate::windows_update_admission::Documents<'_>,
    ) -> Result<()> {
        let context = self.begin_handshake(selected, binding)?;
        let stream = self
            ._worker_pipe
            .take()
            .ok_or_else(|| anyhow::anyhow!("windows_update_handshake_transport_unavailable"))?;
        let offer = self
            ._reservation
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("windows_update_worker_receive_refused"))?
            .worker_input_offer(selected, context, documents)?;
        crate::windows_update_admission::worker_inputs::exchange(stream, offer).await?;
        self.matches(selected, binding)?;
        // Deliberately do not set binding_acknowledged or any readiness flag.
        Ok(())
    }

    /// Test-only corruption after normal trusted mint; never a product override.
    #[cfg(all(windows, test))]
    pub(super) async fn exchange_fixture_worker_inputs(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
        documents: &crate::windows_update_admission::Documents<'_>,
        mutate: impl FnOnce(&mut treer_protocol::controller_update_worker_inputs::InputOffer),
    ) -> Result<()> {
        let context = self.begin_handshake(selected, binding)?;
        let stream = self
            ._worker_pipe
            .take()
            .ok_or_else(|| anyhow::anyhow!("fixture transport missing"))?;
        let mut offer = self
            ._reservation
            .as_ref()
            .unwrap()
            .worker_input_offer(selected, context, documents)?;
        mutate(&mut offer);
        crate::windows_update_admission::worker_inputs::exchange(stream, offer).await
    }

    /// Bind one ACK to the already selected runtime/evidence/full inputs and a
    /// fresh challenge. The original Child/lease/pins/claim remain retained.
    /// This agreement does not grant a command, readiness or restart authority.
    #[cfg(windows)]
    pub(crate) async fn exchange_worker_binding(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<()> {
        let request = self.begin_handshake(selected, binding)?;
        // Taking before await closes the only transport on error/cancellation;
        // success also consumes it. No post-handshake command parser is exposed.
        let stream = self
            ._worker_pipe
            .take()
            .ok_or_else(|| anyhow::anyhow!("windows_update_handshake_transport_unavailable"))?;
        crate::windows_update_handshake::exchange(stream, request).await?;
        self.binding_acknowledged = true;
        Ok(())
    }

    /// Separate, one-shot agreement requiring a child's actual inherited lock
    /// reference. Shares the context handshake latch: no legacy fallback or
    /// second attempt. This does not release the parent's reservation or mint
    /// a restart permission/complete ownership transfer.
    #[cfg(windows)]
    pub(crate) async fn exchange_worker_lock_binding(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<()> {
        let context = self.begin_handshake(selected, binding)?;
        let (volume, file_id) = self
            ._reservation
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("windows_update_lock_reservation_missing"))?
            .installation_lock_identity()?;
        let expected = treer_protocol::controller_update_worker::WorkerLockFrame {
            binding: context,
            volume,
            file_id,
        };
        let stream = self
            ._worker_pipe
            .take()
            .ok_or_else(|| anyhow::anyhow!("windows_update_handshake_transport_unavailable"))?;
        crate::windows_update_handshake::exchange_lock(stream, expected).await?;
        self.matches(selected, binding)?;
        // ACK is still only a context/lock reference agreement. This same flag
        // permits the existing bounded zero-exit observation, not mutation.
        self.binding_acknowledged = true;
        Ok(())
    }

    /// Only the pipe bound by THIS verified creator before CreateProcess.
    /// The private image merely ACKs and exits: no transaction is authorized.
    #[cfg(windows)]
    pub(crate) async fn exchange_created_bootstrap(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<()> {
        self.matches(selected, binding)?;
        let pipe = self
            ._bootstrap_pipe
            .take()
            .ok_or_else(|| anyhow::anyhow!("windows_update_bootstrap_already_attempted"))?;
        self.accept_worker_pipe(selected, binding, pipe).await?;
        self.exchange_worker_binding(selected, binding).await
    }

    /// Accept only this creator's original Child, then exchange signed inputs.
    /// Neither receipt nor terminal exit changes readiness/completion or sender
    /// custody. Unknown/cancellation consumes the original transport/latches.
    #[cfg(windows)]
    pub(crate) async fn exchange_created_inputs(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
        documents: &crate::windows_update_admission::Documents<'_>,
    ) -> Result<()> {
        self.matches(selected, binding)?;
        let pipe = self
            ._bootstrap_pipe
            .take()
            .ok_or_else(|| anyhow::anyhow!("windows_update_worker_input_already_attempted"))?;
        self.accept_worker_pipe(selected, binding, pipe).await?;
        self.exchange_worker_inputs(selected, binding, documents)
            .await
    }

    #[cfg(windows)]
    pub(crate) async fn open_created_custody(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
        documents: &crate::windows_update_admission::Documents<'_>,
    ) -> Result<
        crate::windows_update_custody::Channel<tokio::net::windows::named_pipe::NamedPipeServer>,
    > {
        self.matches(selected, binding)?;
        let pipe = self
            ._bootstrap_pipe
            .take()
            .ok_or_else(|| anyhow::anyhow!("windows_update_receiver_already_attempted"))?;
        self.accept_worker_pipe(selected, binding, pipe).await?;
        let context = self.begin_handshake(selected, binding)?;
        let mut stream = self
            ._worker_pipe
            .take()
            .ok_or_else(|| anyhow::anyhow!("windows_update_receiver_pipe_missing"))?;
        let offer = self
            ._reservation
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("windows_update_receiver_custody_missing"))?
            .worker_input_offer(selected, context, documents)?;
        let context =
            crate::windows_update_admission::worker_inputs::exchange_retained(&mut stream, offer)
                .await?;
        self.matches(selected, binding)?;
        Ok(crate::windows_update_custody::Channel::new(stream, context))
    }

    /// Observe only the retained original Child after a successful binding ACK.
    /// Zero exit completes this terminal exchange, NOT an update or readiness.
    /// Cancellation/timeout/error consumes this observation; the reservation
    /// stays owned and the independent one-shot teardown remains available.
    pub(crate) async fn wait_for_bound_child_exit(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<()> {
        self.matches(selected, binding)?;
        if self.stop_attempted || !self.binding_acknowledged || self.completion_attempted {
            bail!("windows_update_child_completion_refused");
        }
        self.completion_attempted = true;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        tokio::time::timeout_at(deadline, async {
            loop {
                if tokio::time::Instant::now() >= deadline {
                    bail!("windows_update_child_completion_unconfirmed");
                }
                match self.child.try_wait() {
                    Ok(Some(status))
                        if status.success() && tokio::time::Instant::now() < deadline =>
                    {
                        return Ok(());
                    }
                    Ok(Some(_)) | Err(_) => {
                        bail!("windows_update_child_completion_unconfirmed");
                    }
                    Ok(None) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
                }
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("windows_update_child_completion_unconfirmed"))?
    }

    #[cfg(test)]
    pub(super) fn fixture_handshake_request(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<treer_protocol::controller_update_worker::WorkerBindingFrame> {
        self.begin_handshake(selected, binding)
    }

    #[cfg(test)]
    pub(super) async fn fixture_binding_exchange<S>(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
        stream: S,
    ) -> Result<()>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let request = self.begin_handshake(selected, binding)?;
        crate::windows_update_handshake::exchange(stream, request).await
    }

    /// One teardown attempt, on the original Child handle only. Set the latch
    /// BEFORE querying/signalling: an error must not cause Drop or a caller to
    /// repeat an uncertain lifecycle mutation. No PID reopen or Host operation.
    pub(crate) fn stop(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<ExitStatus> {
        self.matches(selected, binding)?;
        if self.stop_attempted {
            bail!("windows_update_child_stop_already_attempted");
        }
        self.stop_attempted = true;
        match self.child.try_wait() {
            Ok(Some(status)) => Ok(status),
            Ok(None) => {
                self.child
                    .kill()
                    .map_err(|_| anyhow::anyhow!("windows_update_child_stop_unknown"))?;
                self.child
                    .wait()
                    .map_err(|_| anyhow::anyhow!("windows_update_child_wait_unknown"))
            }
            Err(_) => bail!("windows_update_child_observation_unknown"),
        }
    }

    #[cfg(test)]
    pub(super) fn begin_fixture_pipe_accept(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<()> {
        self.begin_pipe_accept(selected, binding)
    }

    #[cfg(all(windows, test))]
    pub(super) async fn read_fixture_pipe_byte(&mut self) -> u8 {
        use tokio::io::AsyncReadExt;
        let mut byte = [0];
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            self._worker_pipe.as_mut().unwrap().read_exact(&mut byte),
        )
        .await
        .unwrap()
        .unwrap();
        byte[0]
    }

    #[cfg(test)]
    pub(super) fn check_fixture_job_boundary(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
        query: impl FnOnce(&Child) -> Result<()>,
    ) -> Result<()> {
        self.check_job_boundary(selected, binding, query)
    }

    #[cfg(test)]
    pub(super) fn finish_fixture_input(&mut self) {
        use std::io::Write;
        self.child.stdin.take().unwrap().write_all(b"x").unwrap();
    }

    #[cfg(all(windows, test))]
    pub(super) fn assign_fixture_job(&self) -> treer_platform::ProcessTreeGuard {
        // Test-only assignment of our OWN original Child. No production PID
        // adoption/termination helper is exposed by this leaf.
        treer_platform::ProcessTreeGuard::for_process(self.child.id())
            .unwrap()
            .unwrap()
    }
}

impl Drop for ControllerUpdateChild {
    fn drop(&mut self) {
        if !self.stop_attempted {
            self.stop_attempted = true;
            // Only an owned child which has had no prior teardown attempt.
            // Unknown query/kill/wait outcomes never get an automatic retry.
            if matches!(self.child.try_wait(), Ok(None)) && self.child.kill().is_ok() {
                let _ = self.child.wait();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_update_state_path_requires_exact_configured_server_file() {
        let root = std::env::temp_dir();
        validate_path(&root.join("srv_fixture-agent-startup.json"), "srv_fixture").unwrap();
        for path in [
            root.join("other.json"),
            root.join("nested/../srv_fixture-agent-startup.json"),
            "srv_fixture-agent-startup.json".into(),
        ] {
            assert!(validate_path(&path, "srv_fixture").is_err());
        }
    }
}

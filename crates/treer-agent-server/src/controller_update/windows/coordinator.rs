//! Product coordinator preparation seam, called AFTER normal identity/Policy
//! and confirmed installation selection. No public dispatch/capability bypass.
use crate::{
    controller::{ControllerRuntime, ControllerUpdateChild},
    windows_update_admission::{AdmittedInputs, Documents, ReservedInputs},
    windows_update_custody::Channel,
    windows_update_worker_entry::Trust,
};
use anyhow::{anyhow, Result};
use tokio::net::windows::named_pipe::NamedPipeServer;

fn refused() -> anyhow::Error {
    anyhow!("controller_update_receiver_unconfirmed")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Prepared,
    Holding,
    Unknown,
    Closed,
    Retained,
}

/// One selected operation, normal configured trust and original sender custody.
/// No Clone/Deserialize, remote paths, process observation or retry constructor.
pub(crate) struct Coordinator {
    binding: crate::windows_update_intent::Binding,
    reserved: Option<ReservedInputs>,
    trust: Trust,
    child: Option<ControllerUpdateChild>,
    channel: Option<Channel<NamedPipeServer>>,
    phase: Phase,
}

#[allow(dead_code)] // Public capability remains OFF until full transaction acceptance.
impl Coordinator {
    /// One explicit limited write grant on the SAME Holding worker connection.
    /// Independent receiver backups are not installation/creator-exit authority.
    pub(crate) async fn prepare_worker_backup(
        &mut self,
        selected: &ControllerRuntime,
    ) -> Result<()> {
        self.scope(selected)?;
        if self.phase != Phase::Holding {
            return Err(refused());
        }
        self.phase = Phase::Unknown;
        self.require_live(selected)?;
        self.trust.recheck()?;
        let permit = self.child.as_ref().ok_or_else(refused)?.backup_permit(
            selected,
            &self.binding,
            self.trust.verification_key(),
        )?;
        self.channel
            .as_mut()
            .ok_or_else(refused)?
            .prepare_backup(&permit)
            .await?;
        permit.check(*self.binding.operation_id.as_bytes())?;
        self.require_live(selected)?;
        self.phase = Phase::Holding;
        Ok(())
    }
    /// Uses the existing confirmed-plan admission, not a second Policy gate.
    /// Installed/target handles and job store come from the upstream confirmed
    /// installation contract; trust comes from normal official/self-host config.
    pub(crate) fn prepare(
        selected: &ControllerRuntime,
        dispatch: treer_protocol::controller_update_runtime::DispatchBinding,
        installed: treer_platform::windows_update_inputs::LockedInstallation,
        target: treer_platform::windows_update_inputs::PinnedPair,
        documents: &Documents<'_>,
        store: treer_platform::windows_update_store::UpdateStore,
        trust: Trust,
    ) -> Result<Self> {
        let admitted = AdmittedInputs::prepare_confirmed_for_controller(
            installed,
            target,
            documents,
            trust.verification_key(),
            selected,
            dispatch,
        )?;
        let binding = admitted.binding().clone();
        let reserved = admitted.reserve(store)?;
        Ok(Self {
            binding,
            reserved: Some(reserved),
            trust,
            child: None,
            channel: None,
            phase: Phase::Prepared,
        })
    }

    fn scope(&self, selected: &ControllerRuntime) -> Result<()> {
        if let Some(child) = &self.child {
            child.verify_receiver_scope(selected, &self.binding)
        } else {
            self.reserved
                .as_ref()
                .ok_or_else(refused)?
                .controller_state()?
                .matches_selected(selected, &self.binding)
        }
    }

    pub(crate) async fn start(
        &mut self,
        selected: &ControllerRuntime,
        documents: &Documents<'_>,
    ) -> Result<()> {
        self.scope(selected)?;
        if self.phase != Phase::Prepared {
            return Err(refused());
        }
        self.phase = Phase::Unknown;
        let reserved = self.reserved.take().ok_or_else(refused)?;
        let child = match ControllerUpdateChild::spawn_holding_input_reserved(
            reserved,
            selected,
            &self.trust,
        ) {
            Ok(child) => child,
            Err((_, reserved)) => {
                self.reserved = Some(*reserved);
                return Err(refused());
            }
        };
        // Save custody BEFORE awaiting transport. Dropped/cancelled start never
        // destroys the original child/reservation or constructs another worker.
        self.child = Some(child);
        let channel = self
            .child
            .as_mut()
            .unwrap()
            .open_created_custody(selected, &self.binding, documents)
            .await?;
        self.channel = Some(channel);
        self.require_live(selected)?;
        self.phase = Phase::Holding;
        Ok(())
    }

    fn require_live(&mut self, selected: &ControllerRuntime) -> Result<()> {
        match self
            .child
            .as_mut()
            .ok_or_else(refused)?
            .poll(selected, &self.binding)?
        {
            None => Ok(()),
            Some(_) => Err(refused()),
        }
    }

    /// Read-only raw-state recheck on the same original child/offer, no mutation.
    pub(crate) async fn inspect(&mut self, selected: &ControllerRuntime) -> Result<()> {
        self.scope(selected)?;
        if self.phase != Phase::Holding {
            return Err(refused());
        }
        self.phase = Phase::Unknown;
        self.require_live(selected)?;
        self.channel
            .as_mut()
            .ok_or_else(refused)?
            .request(false)
            .await?;
        self.require_live(selected)?;
        self.phase = Phase::Holding;
        Ok(())
    }

    /// Explicitly end this finite preparation session. An ACK alone
    /// cannot complete close: observe the original child exit zero within 3s.
    /// Original sender reservation remains owned here even after that proof.
    pub(crate) async fn close(&mut self, selected: &ControllerRuntime) -> Result<()> {
        self.scope(selected)?;
        if self.phase != Phase::Holding {
            return Err(refused());
        }
        self.phase = Phase::Unknown;
        self.channel
            .as_mut()
            .ok_or_else(refused)?
            .request(true)
            .await?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let status = self
                .child
                .as_mut()
                .ok_or_else(refused)?
                .poll(selected, &self.binding)?;
            if tokio::time::Instant::now() >= deadline {
                return Err(refused());
            }
            if let Some(status) = status {
                if !status.success() {
                    return Err(refused());
                }
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        self.scope(selected)?;
        self.phase = Phase::Closed;
        Ok(())
    }

    /// Return the SAME original custody only after explicit close + exact exit.
    /// This MOVES guards; it does not drop them, retire claim, or reset attempts.
    pub(crate) fn retain_closed(&mut self, selected: &ControllerRuntime) -> Result<ReservedInputs> {
        self.scope(selected)?;
        if self.phase != Phase::Closed {
            return Err(refused());
        }
        let reserved = self
            .child
            .as_mut()
            .ok_or_else(refused)?
            .retain_after_receiver_exit(selected, &self.binding)?;
        self.phase = Phase::Retained;
        self.channel.take();
        Ok(reserved)
    }
}

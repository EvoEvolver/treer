//! Only this private leaf module constructs connection-bound launcher frames.
//! Sibling handoff modules cannot initialize its private fields.
use std::os::unix::net::UnixStream;

use anyhow::{bail, Result};
use treer_platform::process_image::ProcessImage;

use super::{revalidate_launcher_image, AuthenticatedPeer};
use crate::self_update::launcher_arbitration::{transport, Arbitration, Binding, Decision};

/// A single read from the actual authenticated connection, retaining its
/// exclusive receiver borrow and process image. Only the authenticated reader
/// below can construct it. No Clone, raw-stream accessor or public constructor.
/// This is not proof of durable registration, owner ACK or writer death.
pub struct LauncherFrame<'a> {
    observation: transport::Observation<'a>,
    image: &'a ProcessImage,
}

impl LauncherFrame<'_> {
    pub fn kind(&self) -> transport::Kind {
        self.observation.kind()
    }

    /// Ordinary metadata for diagnostics/comparison; copying it does not copy
    /// this connection-bound observation or permit another acceptance.
    pub fn binding(&self) -> Option<&Binding> {
        self.observation.binding()
    }

    pub(in crate::self_update) fn permit_binding(
        &self,
        arbitration: &mut Arbitration,
    ) -> Result<&Binding> {
        // A caller may delay acceptance after reading. Revalidate while the
        // actual receiver is still borrowed, not just when the frame was read.
        revalidate_launcher_image(self.image, arbitration)?;
        match self.binding() {
            Some(binding) => Ok(binding),
            None => {
                arbitration.invalidate();
                bail!("update_recovery_required");
            }
        }
    }
}

impl AuthenticatedPeer {
    /// Revalidate this exact connected peer before a single raw launcher read,
    /// and its creation identity/image again before returning successful input.
    /// Complete frames must match the pending arbitration's exact binding.
    /// This compares metadata only: the future supervisor must derive that
    /// binding from verified durable records, not from incoming frame contents.
    /// Every failure invalidates pending arbitration, including cancellation and
    /// repeat reads. Success does not accept a permit or establish authenticated
    /// EOF/all-writers-gone evidence; the peer may die while the read drains.
    /// The future supervisor must privately own both cancellation endpoints and
    /// keep the write end exclusively inside its signal/shutdown handling.
    pub fn read_launcher_frame<'a>(
        &'a mut self,
        arbitration: &mut Arbitration,
        cancellation: &UnixStream,
    ) -> Result<LauncherFrame<'a>> {
        if self.launcher_read_started || arbitration.decision() != Decision::Pending {
            arbitration.invalidate();
            bail!("update_recovery_required");
        }
        self.launcher_read_started = true;
        if let Err(error) = self.verify() {
            arbitration.invalidate();
            return Err(error);
        }
        match transport::read(&mut self.stream, cancellation) {
            Ok(observation) => {
                // The exclusive stream borrow prevents descriptor replacement;
                // UID/PID are connection credentials already checked above.
                // Recheck the independent image field while the observation
                // borrows the receiver: a same-PID exec during reading must
                // not turn raw input into a successfully verified observation.
                revalidate_launcher_image(&self.image, arbitration)?;
                if let Some(binding) = observation.binding() {
                    arbitration.match_binding(binding)?;
                }
                Ok(LauncherFrame {
                    observation,
                    image: &self.image,
                })
            }
            Err(error) => {
                arbitration.invalidate();
                Err(error)
            }
        }
    }
}

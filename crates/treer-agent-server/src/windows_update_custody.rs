//! Ordered, bounded custody control. The backup-only write grant is explicit;
//! neither its receipt nor a close reply permits sender release.
use anyhow::{anyhow, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use treer_protocol::{
    controller_update_worker::WorkerBindingFrame,
    controller_update_worker_inputs::{CustodyFrame, CustodyKind, CUSTODY_BYTES},
};

fn refused() -> anyhow::Error {
    anyhow!("windows_update_custody_unconfirmed")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Holding,
    Unknown,
    Closed,
}

pub(crate) struct Channel<S> {
    stream: S,
    context: WorkerBindingFrame,
    sequence: u32,
    phase: Phase,
    backup_attempted: bool,
    #[cfg(windows)]
    startup: Option<Option<treer_protocol::controller_update_worker_inputs::Snapshot>>,
    #[cfg(windows)]
    startup_attempted: bool,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl<S: AsyncRead + AsyncWrite + Unpin> Channel<S> {
    pub(crate) fn new(stream: S, context: WorkerBindingFrame) -> Self {
        Self {
            stream,
            context,
            sequence: 1,
            phase: Phase::Holding,
            backup_attempted: false,
            #[cfg(windows)]
            startup: None,
            #[cfg(windows)]
            startup_attempted: false,
        }
    }
    #[cfg(windows)]
    pub(crate) fn with_startup(
        mut self,
        snapshot: Option<treer_protocol::controller_update_worker_inputs::Snapshot>,
    ) -> Self {
        self.startup = Some(snapshot);
        self
    }
    #[cfg(windows)]
    pub(crate) fn startup(
        &self,
    ) -> Result<&Option<treer_protocol::controller_update_worker_inputs::Snapshot>> {
        if self.phase != Phase::Holding || !self.backup_attempted || self.startup_attempted {
            return Err(refused());
        }
        self.startup.as_ref().ok_or_else(refused)
    }
    #[cfg(windows)]
    pub(crate) async fn accept_startup(
        &mut self,
        committed: &crate::startup_store::update::Committed,
    ) -> Result<()> {
        let previous = self.startup()?.clone();
        self.startup_attempted = true;
        let next = committed.snapshot(self.context.operation)?.clone();
        let payload = serde_json::to_vec(
            &treer_protocol::controller_update_worker_inputs::StartupTransition {
                previous,
                next: next.clone(),
            },
        )?;
        self.exchange_payload(
            CustodyKind::StartupCommitted,
            CustodyKind::StartupAccepted,
            false,
            Some(&payload),
        )
        .await?;
        committed.snapshot(self.context.operation)?;
        self.startup = Some(next);
        Ok(())
    }
    /// Set Unknown before the first await: cancellation/error cannot repeat or
    /// close over a transport whose last request may already have been consumed.
    pub(crate) async fn request(&mut self, close: bool) -> Result<()> {
        self.exchange(
            if close {
                CustodyKind::Close
            } else {
                CustodyKind::Inspect
            },
            if close {
                CustodyKind::Closed
            } else {
                CustodyKind::Inspected
            },
            close,
        )
        .await
    }

    #[cfg(windows)]
    pub(crate) async fn prepare_backup(
        &mut self,
        permit: &crate::windows_update_admission::ConfirmedBackupPermit<'_>,
    ) -> Result<()> {
        permit.check(self.context.operation)?;
        self.request_backup().await
    }

    #[cfg(any(windows, test))]
    async fn request_backup(&mut self) -> Result<()> {
        if self.backup_attempted || self.phase != Phase::Holding {
            return Err(refused());
        }
        // This latch survives cancellation and every possible peer response.
        self.backup_attempted = true;
        self.exchange(
            CustodyKind::PrepareBackup,
            CustodyKind::BackupPrepared,
            false,
        )
        .await
    }

    async fn exchange(
        &mut self,
        request: CustodyKind,
        reply: CustodyKind,
        close: bool,
    ) -> Result<()> {
        self.exchange_payload(request, reply, close, None).await
    }
    async fn exchange_payload(
        &mut self,
        request: CustodyKind,
        reply: CustodyKind,
        close: bool,
        payload: Option<&[u8]>,
    ) -> Result<()> {
        if self.phase != Phase::Holding {
            return Err(refused());
        }
        self.phase = Phase::Unknown;
        let frame = CustodyFrame {
            context: self.context,
            sequence: self.sequence,
        };
        let bytes = frame.encode(request).ok_or_else(refused)?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        tokio::time::timeout_at(deadline, async {
            self.stream.write_all(&bytes).await.map_err(|_| refused())?;
            if let Some(payload) = payload {
                if payload.is_empty()
                    || payload.len()
                        > treer_protocol::controller_update_worker_inputs::STARTUP_TRANSITION_LIMIT
                {
                    return Err(refused());
                }
                self.stream
                    .write_u32_le(payload.len() as u32)
                    .await
                    .map_err(|_| refused())?;
                self.stream
                    .write_all(payload)
                    .await
                    .map_err(|_| refused())?;
            }
            let mut response = [0; CUSTODY_BYTES];
            self.stream
                .read_exact(&mut response)
                .await
                .map_err(|_| refused())?;
            if tokio::time::Instant::now() >= deadline
                || CustodyFrame::decode(&response, reply) != Some(frame)
            {
                return Err(refused());
            }
            Ok(())
        })
        .await
        .map_err(|_| refused())??;
        self.sequence += 1;
        self.phase = if close { Phase::Closed } else { Phase::Holding };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context() -> WorkerBindingFrame {
        WorkerBindingFrame {
            operation: *uuid::Uuid::new_v4().as_bytes(),
            challenge: *uuid::Uuid::new_v4().as_bytes(),
            context_sha256: [3; 32],
        }
    }
    #[tokio::test]
    async fn windows_update_startup_cas_ordered_transition_and_wrong_reply_are_single_use() {
        use treer_protocol::controller_update_worker_inputs::{
            Snapshot, StartupTransition, STARTUP_TRANSITION_LIMIT,
        };
        for wrong_reply in [false, true] {
            let context = context();
            let payload = serde_json::to_vec(&StartupTransition {
                previous: None,
                next: Some(Snapshot {
                    file_id: 7,
                    size: 41,
                    sha256: "c".repeat(64),
                }),
            })
            .unwrap();
            let (a, mut b) = tokio::io::duplex(1024);
            let peer = tokio::spawn(async move {
                let mut bytes = [0; CUSTODY_BYTES];
                b.read_exact(&mut bytes).await.unwrap();
                let frame = CustodyFrame::decode(&bytes, CustodyKind::StartupCommitted).unwrap();
                assert_eq!(
                    frame,
                    CustodyFrame {
                        context,
                        sequence: 1
                    }
                );
                let size = b.read_u32_le().await.unwrap() as usize;
                assert!(size <= STARTUP_TRANSITION_LIMIT);
                let mut body = vec![0; size];
                b.read_exact(&mut body).await.unwrap();
                let parsed: StartupTransition = serde_json::from_slice(&body).unwrap();
                assert!(parsed.previous.is_none());
                assert_eq!(parsed.next.unwrap().file_id, 7);
                b.write_all(
                    &frame
                        .encode(if wrong_reply {
                            CustodyKind::Inspected
                        } else {
                            CustodyKind::StartupAccepted
                        })
                        .unwrap(),
                )
                .await
                .unwrap();
            });
            let mut channel = Channel::new(a, context);
            let result = channel
                .exchange_payload(
                    CustodyKind::StartupCommitted,
                    CustodyKind::StartupAccepted,
                    false,
                    Some(&payload),
                )
                .await;
            assert_eq!(result.is_err(), wrong_reply);
            if wrong_reply {
                assert!(channel.request(false).await.is_err());
                assert!(channel.request(true).await.is_err());
            }
            peer.await.unwrap();
        }
    }
    #[tokio::test]
    async fn windows_update_backup_transition_ordered_grant_is_distinct_and_single_use() {
        let context = context();
        let frame = CustodyFrame {
            context,
            sequence: 1,
        };
        let bytes = frame.encode(CustodyKind::PrepareBackup).unwrap();
        assert!(CustodyFrame::decode(&bytes, CustodyKind::Inspect).is_none());
        assert!(CustodyFrame::decode(&bytes, CustodyKind::BackupPrepared).is_none());
        let (a, mut b) = tokio::io::duplex(512);
        let peer = tokio::spawn(async move {
            for (sequence, request, response) in [
                (1, CustodyKind::Inspect, CustodyKind::Inspected),
                (2, CustodyKind::PrepareBackup, CustodyKind::BackupPrepared),
                (3, CustodyKind::Inspect, CustodyKind::Inspected),
                (4, CustodyKind::Close, CustodyKind::Closed),
            ] {
                let mut bytes = [0; CUSTODY_BYTES];
                b.read_exact(&mut bytes).await.unwrap();
                let frame = CustodyFrame::decode(&bytes, request).unwrap();
                assert_eq!(frame, CustodyFrame { context, sequence });
                b.write_all(&frame.encode(response).unwrap()).await.unwrap();
            }
        });
        let mut channel = Channel::new(a, context);
        channel.request(false).await.unwrap();
        channel.request_backup().await.unwrap();
        assert!(channel.request_backup().await.is_err());
        channel.request(false).await.unwrap();
        channel.request(true).await.unwrap();
        assert!(channel.request_backup().await.is_err());
        peer.await.unwrap();
    }
    #[tokio::test]
    async fn windows_update_backup_transition_cancel_and_wrong_receipt_never_authorise_replay() {
        for cancel in [false, true] {
            let context = context();
            let (a, mut b) = tokio::io::duplex(256);
            let mut channel = Channel::new(a, context);
            if cancel {
                let mut grant = Box::pin(channel.request_backup());
                let mut bytes = [0; CUSTODY_BYTES];
                tokio::select! {
                    _ = &mut grant => panic!("unexpected completion"),
                    _ = b.read_exact(&mut bytes) => {}
                }
                assert_eq!(
                    CustodyFrame::decode(&bytes, CustodyKind::PrepareBackup)
                        .unwrap()
                        .context,
                    context
                );
                drop(grant);
            } else {
                let peer = tokio::spawn(async move {
                    let mut bytes = [0; CUSTODY_BYTES];
                    b.read_exact(&mut bytes).await.unwrap();
                    let frame = CustodyFrame::decode(&bytes, CustodyKind::PrepareBackup).unwrap();
                    // A normal receipt or inspection reply is not the explicit
                    // response to the granted backup write.
                    b.write_all(&frame.encode(CustodyKind::Inspected).unwrap())
                        .await
                        .unwrap();
                });
                assert!(channel.request_backup().await.is_err());
                peer.await.unwrap();
            }
            assert!(channel.request_backup().await.is_err());
            assert!(channel.request(false).await.is_err());
            assert!(channel.request(true).await.is_err());
        }
    }
    #[tokio::test]
    async fn windows_update_coordinator_ordered_inspection_close_and_exact_wire_roles() {
        let context = context();
        let (a, mut b) = tokio::io::duplex(256);
        let peer = tokio::spawn(async move {
            for (sequence, kind, reply) in [
                (1, CustodyKind::Inspect, CustodyKind::Inspected),
                (2, CustodyKind::Close, CustodyKind::Closed),
            ] {
                let mut bytes = [0; CUSTODY_BYTES];
                b.read_exact(&mut bytes).await.unwrap();
                let frame = CustodyFrame::decode(&bytes, kind).unwrap();
                assert_eq!(frame, CustodyFrame { context, sequence });
                assert!(CustodyFrame::decode(&bytes, reply).is_none());
                b.write_all(&frame.encode(reply).unwrap()).await.unwrap();
            }
        });
        let mut channel = Channel::new(a, context);
        channel.request(false).await.unwrap();
        channel.request(true).await.unwrap();
        assert!(channel.request(false).await.is_err());
        peer.await.unwrap();
    }
    #[tokio::test]
    async fn windows_update_coordinator_cancel_or_wrong_reply_poison_without_replay() {
        let context = context();
        for cancel in [false, true] {
            let (a, mut b) = tokio::io::duplex(256);
            let mut channel = Channel::new(a, context);
            if cancel {
                let mut query = Box::pin(channel.request(false));
                tokio::select! { _ = &mut query => panic!("unexpected reply"), _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {} }
                drop(query);
            } else {
                let peer = tokio::spawn(async move {
                    let mut bytes = [0; CUSTODY_BYTES];
                    b.read_exact(&mut bytes).await.unwrap();
                    let mut frame = CustodyFrame::decode(&bytes, CustodyKind::Inspect).unwrap();
                    frame.sequence += 1;
                    b.write_all(&frame.encode(CustodyKind::Inspected).unwrap())
                        .await
                        .unwrap();
                });
                assert!(channel.request(false).await.is_err());
                peer.await.unwrap();
            }
            assert!(channel.request(false).await.is_err());
            assert!(channel.request(true).await.is_err());
        }
    }
}

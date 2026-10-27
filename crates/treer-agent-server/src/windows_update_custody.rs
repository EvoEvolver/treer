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
    candidates_attempted: bool,
    #[cfg(any(windows, test))]
    publication_attempted: bool,
    #[cfg(windows)]
    startup: Option<Option<treer_protocol::controller_update_worker_inputs::Snapshot>>,
    #[cfg(windows)]
    startup_attempted: bool,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl<S: AsyncRead + AsyncWrite + Unpin> Channel<S> {
    #[cfg(windows)]
    pub(crate) async fn publish_pair(
        &mut self,
        permit: &crate::windows_update_admission::ConfirmedPublicationPermit<'_>,
        transfer: &crate::windows_update_transfer::Transfer,
    ) -> Result<()> {
        permit.check(self.context.operation)?;
        transfer.remaining(chrono::Utc::now().timestamp_millis())?;
        self.request_publication(&transfer.encode()?).await?;
        permit.check(self.context.operation)?;
        transfer.remaining(chrono::Utc::now().timestamp_millis())?;
        Ok(())
    }
    #[cfg(any(windows, test))]
    async fn request_publication(&mut self, payload: &[u8]) -> Result<()> {
        if self.phase != Phase::Holding || self.publication_attempted {
            return Err(refused());
        }
        self.publication_attempted = true;
        self.exchange_payload(
            CustodyKind::PublishPair,
            CustodyKind::PairPublished,
            false,
            Some(payload),
        )
        .await
    }
    #[cfg(windows)]
    pub(crate) async fn transfer_custody(
        &mut self,
        binding: &crate::windows_update_intent::Binding,
        committed: &crate::startup_store::update::Committed,
    ) -> Result<crate::windows_update_transfer::Transfer> {
        if self.phase != Phase::Holding || !self.candidates_attempted || !self.startup_attempted {
            return Err(refused());
        }
        let snapshot = committed.fenced_snapshot(self.context.operation)?.clone();
        if self.startup.as_ref() != Some(&snapshot) {
            return Err(refused());
        }
        let transfer = crate::windows_update_transfer::Transfer::new(
            binding,
            self.context,
            snapshot,
            chrono::Utc::now().timestamp_millis(),
        );
        self.exchange_payload(
            CustodyKind::TransferCustody,
            CustodyKind::CustodyTransferred,
            false,
            Some(&transfer.encode()?),
        )
        .await?;
        committed.fenced_snapshot(self.context.operation)?;
        transfer.remaining(chrono::Utc::now().timestamp_millis())?;
        Ok(transfer)
    }
    pub(crate) fn new(stream: S, context: WorkerBindingFrame) -> Self {
        Self {
            stream,
            context,
            sequence: 1,
            phase: Phase::Holding,
            backup_attempted: false,
            candidates_attempted: false,
            #[cfg(any(windows, test))]
            publication_attempted: false,
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

    #[cfg(windows)]
    pub(crate) async fn prepare_candidates(
        &mut self,
        permit: &crate::windows_update_admission::ConfirmedCandidatePermit<'_>,
    ) -> Result<()> {
        permit.check(self.context.operation)?;
        self.request_candidates().await?;
        permit.check(self.context.operation)
    }

    #[cfg(any(windows, test))]
    async fn request_candidates(&mut self) -> Result<()> {
        if !self.backup_attempted || self.candidates_attempted || self.phase != Phase::Holding {
            return Err(refused());
        }
        self.candidates_attempted = true;
        self.exchange(
            CustodyKind::PrepareCandidates,
            CustodyKind::CandidatesPrepared,
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
                if payload.is_empty() || payload.len() > crate::windows_update_transfer::LIMIT {
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
    #[tokio::test]
    async fn windows_update_worker_publication_sequenced_payload_ack_is_single_use() {
        let context = context();
        let (a, mut b) = tokio::io::duplex(1024);
        let mut channel = Channel::new(a, context);
        let peer = tokio::spawn(async move {
            let mut raw = [0; CUSTODY_BYTES];
            b.read_exact(&mut raw).await.unwrap();
            let frame = CustodyFrame::decode(&raw, CustodyKind::PublishPair).unwrap();
            assert_eq!(frame.context, context);
            assert!(CustodyFrame::decode(&raw, CustodyKind::TransferCustody).is_none());
            assert_eq!(b.read_u32_le().await.unwrap(), 4);
            let mut payload = [0; 4];
            b.read_exact(&mut payload).await.unwrap();
            assert_eq!(&payload, b"test");
            b.write_all(&frame.encode(CustodyKind::PairPublished).unwrap())
                .await
                .unwrap();
        });
        channel.request_publication(b"test").await.unwrap();
        assert!(channel.request_publication(b"test").await.is_err());
        peer.await.unwrap();
    }
    #[tokio::test]
    async fn windows_update_worker_publication_cancel_or_foreign_ack_never_replays() {
        for cancel in [true, false] {
            let context = context();
            let (a, mut b) = tokio::io::duplex(1024);
            let mut channel = Channel::new(a, context);
            let (sent, received) = tokio::sync::oneshot::channel();
            let peer = tokio::spawn(async move {
                let mut raw = [0; CUSTODY_BYTES];
                b.read_exact(&mut raw).await.unwrap();
                let mut frame = CustodyFrame::decode(&raw, CustodyKind::PublishPair).unwrap();
                let size = b.read_u32_le().await.unwrap();
                let mut payload = vec![0; size as usize];
                b.read_exact(&mut payload).await.unwrap();
                sent.send(()).unwrap();
                if cancel {
                    std::future::pending::<()>().await;
                }
                frame.sequence += 1;
                b.write_all(&frame.encode(CustodyKind::PairPublished).unwrap())
                    .await
                    .unwrap();
            });
            if cancel {
                let future = channel.request_publication(b"test");
                tokio::pin!(future);
                tokio::select! { _ = received => {}, _ = &mut future => panic!("unexpected completion") }
            } else {
                assert!(channel.request_publication(b"test").await.is_err());
            }
            assert!(channel.request_publication(b"test").await.is_err());
            assert!(channel.request(true).await.is_err());
            peer.abort();
        }
    }
    #[tokio::test]
    async fn windows_update_transfer_cancel_and_foreign_ack_consume_original_exchange() {
        for cancel in [false, true] {
            let context = context();
            let (a, mut b) = tokio::io::duplex(1024);
            let mut channel = Channel::new(a, context);
            let (sent, received) = tokio::sync::oneshot::channel();
            let peer = tokio::spawn(async move {
                let mut raw = [0; CUSTODY_BYTES];
                b.read_exact(&mut raw).await.unwrap();
                let frame = CustodyFrame::decode(&raw, CustodyKind::TransferCustody).unwrap();
                sent.send(()).unwrap();
                if cancel {
                    std::future::pending::<()>().await;
                }
                let mut wrong = frame;
                wrong.context.challenge = *uuid::Uuid::new_v4().as_bytes();
                b.write_all(&wrong.encode(CustodyKind::CustodyTransferred).unwrap())
                    .await
                    .unwrap();
            });
            if cancel {
                let future = channel.exchange(
                    CustodyKind::TransferCustody,
                    CustodyKind::CustodyTransferred,
                    false,
                );
                tokio::pin!(future);
                tokio::select! { _ = received => {}, _ = &mut future => panic!("unexpected completion") }
            } else {
                assert!(channel
                    .exchange(
                        CustodyKind::TransferCustody,
                        CustodyKind::CustodyTransferred,
                        false
                    )
                    .await
                    .is_err());
            }
            assert!(channel.request(false).await.is_err());
            peer.abort();
        }
    }
    #[tokio::test]
    async fn windows_update_worker_candidates_order_reply_and_cancel_are_single_use() {
        for outcome in 0..3 {
            let context = context();
            let (a, mut b) = tokio::io::duplex(1024);
            let (sent, received) = tokio::sync::oneshot::channel();
            let peer = tokio::spawn(async move {
                let mut bytes = [0; CUSTODY_BYTES];
                b.read_exact(&mut bytes).await.unwrap();
                let backup = CustodyFrame::decode(&bytes, CustodyKind::PrepareBackup).unwrap();
                b.write_all(&backup.encode(CustodyKind::BackupPrepared).unwrap())
                    .await
                    .unwrap();
                b.read_exact(&mut bytes).await.unwrap();
                let frame = CustodyFrame::decode(&bytes, CustodyKind::PrepareCandidates).unwrap();
                assert_eq!(frame.context, context);
                assert_eq!(frame.sequence, 2);
                sent.send(()).unwrap();
                if outcome == 2 {
                    std::future::pending::<()>().await;
                } else {
                    b.write_all(
                        &frame
                            .encode(if outcome == 0 {
                                CustodyKind::CandidatesPrepared
                            } else {
                                CustodyKind::BackupPrepared
                            })
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                }
            });
            let mut channel = Channel::new(a, context);
            assert!(channel.request_candidates().await.is_err());
            assert!(
                !channel.candidates_attempted,
                "wrong order does not consume grant"
            );
            channel.request_backup().await.unwrap();
            if outcome == 2 {
                let mut request = std::pin::pin!(channel.request_candidates());
                tokio::select! {
                    _ = &mut request => panic!("peer has not replied"),
                    _ = received => (),
                }
            } else {
                assert_eq!(channel.request_candidates().await.is_ok(), outcome == 0);
            }
            assert!(channel.candidates_attempted);
            assert!(channel.request_candidates().await.is_err());
            if outcome != 0 {
                assert!(channel.request(true).await.is_err());
            }
            peer.abort();
        }
    }
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

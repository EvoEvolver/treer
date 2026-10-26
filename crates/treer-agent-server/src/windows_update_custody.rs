//! Ordered, bounded, read-only control; a close reply is never sender release.
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
}

#[cfg_attr(not(windows), allow(dead_code))]
impl<S: AsyncRead + AsyncWrite + Unpin> Channel<S> {
    pub(crate) fn new(stream: S, context: WorkerBindingFrame) -> Self {
        Self {
            stream,
            context,
            sequence: 1,
            phase: Phase::Holding,
        }
    }
    /// Set Unknown before the first await: cancellation/error cannot repeat or
    /// close over a transport whose last request may already have been consumed.
    pub(crate) async fn request(&mut self, close: bool) -> Result<()> {
        if self.phase != Phase::Holding {
            return Err(refused());
        }
        self.phase = Phase::Unknown;
        let frame = CustodyFrame {
            context: self.context,
            sequence: self.sequence,
        };
        let request = if close {
            CustodyKind::Close
        } else {
            CustodyKind::Inspect
        };
        let reply = if close {
            CustodyKind::Closed
        } else {
            CustodyKind::Inspected
        };
        let bytes = frame.encode(request).ok_or_else(refused)?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        tokio::time::timeout_at(deadline, async {
            self.stream.write_all(&bytes).await.map_err(|_| refused())?;
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

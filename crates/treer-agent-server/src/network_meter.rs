use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use treer_protocol::NetworkUsageTotals;

#[derive(Default)]
pub(crate) struct WriteCounter {
    bytes: AtomicU64,
    chunks: AtomicU64,
}

pub(crate) fn totals(sent: &WriteCounter, received: &WriteCounter) -> NetworkUsageTotals {
    NetworkUsageTotals {
        sent_bytes: sent.bytes.load(Ordering::Relaxed),
        received_bytes: received.bytes.load(Ordering::Relaxed),
        sent_chunks: sent.chunks.load(Ordering::Relaxed),
        received_chunks: received.chunks.load(Ordering::Relaxed),
    }
}

/// Counts accepted writes, including partial writes; reads and Pending do not
/// inflate totals. Counters outlive a cancelled copy future so resets can report
/// progress without relying on copy_bidirectional's success-only return value.
pub(crate) struct Metered<S> {
    pub stream: S,
    pub written: Arc<WriteCounter>,
}

impl<S: AsyncRead + Unpin> AsyncRead for Metered<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Metered<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.stream).poll_write(cx, data);
        if let Poll::Ready(Ok(n)) = &result {
            if *n > 0 {
                this.written.bytes.fetch_add(*n as u64, Ordering::Relaxed);
                this.written.chunks.fetch_add(1, Ordering::Relaxed);
            }
        }
        result
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn network_meter_counts_partial_writes_without_counting_reads() {
        let (stream, mut peer) = tokio::io::duplex(3);
        let counter = Arc::new(WriteCounter::default());
        let mut metered = Metered {
            stream,
            written: counter.clone(),
        };
        assert_eq!(metered.write(b"abcdef").await.unwrap(), 3);
        assert_eq!(totals(&counter, &WriteCounter::default()).sent_bytes, 3);
        let mut data = [0; 3];
        peer.read_exact(&mut data).await.unwrap();
        assert_eq!(&data, b"abc");
        peer.write_all(b"\x00\xff").await.unwrap();
        metered.read_exact(&mut data[..2]).await.unwrap();
        assert_eq!(totals(&counter, &WriteCounter::default()).sent_chunks, 1);
        metered.shutdown().await.unwrap();
        assert_eq!(totals(&counter, &WriteCounter::default()).sent_bytes, 3);
    }
}

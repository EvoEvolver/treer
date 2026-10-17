//! Body wrappers for public ingress (#146): a request body that is capped and
//! must keep flowing, and a response body that releases the in-flight permit
//! when it ends and must not stall forever. Both report what stopped them so
//! the handler can answer 413/408 instead of a generic gateway error.
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::task::{ready, Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use http_body::{Frame, SizeHint};

use crate::ingress_guard::Permit;

pub(crate) const STOP_NONE: u8 = 0;
pub(crate) const STOP_TOO_LARGE: u8 = 1;
pub(crate) const STOP_TIMEOUT: u8 = 2;

/// Shared between the wrapper and the handler: why the body stopped.
#[derive(Clone, Default)]
pub(crate) struct BodyOutcome(Arc<AtomicU8>);

impl BodyOutcome {
    pub(crate) fn stopped_by(&self) -> u8 {
        self.0.load(Ordering::Acquire)
    }

    fn stop(&self, reason: u8) {
        let _ = self
            .0
            .compare_exchange(STOP_NONE, reason, Ordering::AcqRel, Ordering::Acquire);
    }
}

#[derive(Debug)]
struct BodyStopped(&'static str);

impl std::fmt::Display for BodyStopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for BodyStopped {}

/// Request body: at most `cap` bytes in total and never more than `idle`
/// between frames. Exceeding either ends the stream with an error.
pub(crate) struct LimitedBody {
    inner: Body,
    remaining: u64,
    idle: Duration,
    deadline: Pin<Box<tokio::time::Sleep>>,
    /// The whole body must arrive before this; a trickle that satisfies the
    /// idle rule cannot hold a permit indefinitely.
    total: Pin<Box<tokio::time::Sleep>>,
    outcome: BodyOutcome,
}

impl LimitedBody {
    pub(crate) fn new(
        inner: Body,
        cap: u64,
        idle: Duration,
        total: Duration,
    ) -> (Self, BodyOutcome) {
        let outcome = BodyOutcome::default();
        (
            Self {
                inner,
                remaining: cap,
                idle,
                deadline: Box::pin(tokio::time::sleep(idle)),
                total: Box::pin(tokio::time::sleep(total)),
                outcome: outcome.clone(),
            },
            outcome,
        )
    }
}

impl http_body::Body for LimitedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    let size = data.len() as u64;
                    if size > this.remaining {
                        this.outcome.stop(STOP_TOO_LARGE);
                        return Poll::Ready(Some(Err(axum::Error::new(BodyStopped(
                            "request body exceeds the ingress limit",
                        )))));
                    }
                    this.remaining -= size;
                }
                let idle = this.idle;
                this.deadline
                    .as_mut()
                    .reset(tokio::time::Instant::now() + idle);
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(other) => Poll::Ready(other),
            Poll::Pending => {
                if this.total.as_mut().poll(cx).is_ready() {
                    this.outcome.stop(STOP_TIMEOUT);
                    return Poll::Ready(Some(Err(axum::Error::new(BodyStopped(
                        "request body took too long",
                    )))));
                }
                ready!(this.deadline.as_mut().poll(cx));
                this.outcome.stop(STOP_TIMEOUT);
                Poll::Ready(Some(Err(axum::Error::new(BodyStopped(
                    "request body stalled",
                )))))
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// Response body: holds the in-flight permit until it ends (or is dropped
/// when the client goes away), fails when the upstream stalls for `idle`,
/// and leases the permit for at most `lease`: a client that never reads the
/// response (so this body is never polled) cannot keep the slot beyond that.
pub(crate) struct GuardedResponseBody {
    inner: Body,
    idle: Duration,
    deadline: Pin<Box<tokio::time::Sleep>>,
    _permit: Option<Arc<Permit>>,
    /// The lease task; aborted when the body ends so finished responses do
    /// not leave sleeping tasks behind.
    lease: Option<tokio::task::JoinHandle<()>>,
}

impl GuardedResponseBody {
    pub(crate) fn new(
        inner: Body,
        idle: Duration,
        lease: Duration,
        permit: Option<Arc<Permit>>,
    ) -> Self {
        let lease = permit.clone().map(|permit| {
            tokio::spawn(async move {
                tokio::time::sleep(lease).await;
                permit.release();
            })
        });
        Self {
            inner,
            idle,
            deadline: Box::pin(tokio::time::sleep(idle)),
            _permit: permit,
            lease,
        }
    }
}

impl Drop for GuardedResponseBody {
    fn drop(&mut self) {
        // The lease task also holds the permit; releasing here (idempotent)
        // returns the slot as soon as the response ends or the client leaves,
        // and the lease task is cancelled instead of sleeping on.
        if let Some(permit) = &self._permit {
            permit.release();
        }
        if let Some(lease) = self.lease.take() {
            lease.abort();
        }
    }
}

impl http_body::Body for GuardedResponseBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(frame) => {
                let idle = this.idle;
                this.deadline
                    .as_mut()
                    .reset(tokio::time::Instant::now() + idle);
                Poll::Ready(frame)
            }
            Poll::Pending => {
                ready!(this.deadline.as_mut().poll(cx));
                Poll::Ready(Some(Err(axum::Error::new(BodyStopped(
                    "upstream response stalled",
                )))))
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[tokio::test]
    async fn limited_body_caps_bytes_and_reports_the_reason() {
        let (body, outcome) = LimitedBody::new(
            Body::from(vec![0_u8; 10]),
            4,
            Duration::from_secs(5),
            Duration::from_secs(60),
        );
        let error = Body::new(body).collect().await.unwrap_err();
        assert!(error.to_string().contains("exceeds"));
        assert_eq!(outcome.stopped_by(), STOP_TOO_LARGE);

        let (body, outcome) = LimitedBody::new(
            Body::from(vec![1_u8; 4]),
            4,
            Duration::from_secs(5),
            Duration::from_secs(60),
        );
        let bytes = Body::new(body).collect().await.unwrap().to_bytes();
        assert_eq!(bytes.len(), 4);
        assert_eq!(outcome.stopped_by(), STOP_NONE);
    }

    #[tokio::test(start_paused = true)]
    async fn limited_body_times_out_when_no_bytes_arrive() {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, axum::Error>>(4);
        let stream = tokio_stream_from(rx);
        let (body, outcome) = LimitedBody::new(
            Body::new(stream),
            1 << 20,
            Duration::from_secs(10),
            Duration::from_secs(60),
        );
        tx.try_send(Ok(Frame::data(Bytes::from_static(b"start"))))
            .unwrap();
        let collect = tokio::spawn(async move { Body::new(body).collect().await });
        tokio::time::advance(Duration::from_secs(11)).await;
        let error = collect.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("stalled"));
        assert_eq!(outcome.stopped_by(), STOP_TIMEOUT);
        drop(tx);
    }

    #[tokio::test(start_paused = true)]
    async fn guarded_response_body_releases_the_permit_and_times_out_idle() {
        let guard =
            crate::ingress_guard::IngressGuard::new(crate::ingress_guard::TrustedProxies::none());
        let limits = treer_protocol::EffectiveIngressLimits::default();
        let admission = guard
            .admit(
                "ing_x",
                &limits,
                "203.0.113.1".parse().unwrap(),
                &axum::http::HeaderMap::new(),
            )
            .unwrap();
        assert_eq!(guard.in_flight("ing_x"), 1);
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, axum::Error>>(4);
        let body = GuardedResponseBody::new(
            Body::new(tokio_stream_from(rx)),
            Duration::from_secs(300),
            Duration::from_secs(600),
            Some(Arc::new(admission.permit)),
        );
        let collect = tokio::spawn(async move { Body::new(body).collect().await });
        assert_eq!(guard.in_flight("ing_x"), 1, "held while the body streams");
        tokio::time::advance(Duration::from_secs(301)).await;
        let error = collect.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("stalled"));
        tokio::task::yield_now().await;
        assert_eq!(guard.in_flight("ing_x"), 0, "released when the body ended");
        drop(tx);

        // Dropping an unfinished body (client gone) also releases it.
        let admission = guard
            .admit(
                "ing_x",
                &limits,
                "203.0.113.1".parse().unwrap(),
                &axum::http::HeaderMap::new(),
            )
            .unwrap();
        let (_tx2, rx2) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, axum::Error>>(4);
        let body = GuardedResponseBody::new(
            Body::new(tokio_stream_from(rx2)),
            Duration::from_secs(300),
            Duration::from_secs(600),
            Some(Arc::new(admission.permit)),
        );
        assert_eq!(guard.in_flight("ing_x"), 1);
        drop(body);
        assert_eq!(guard.in_flight("ing_x"), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn trickled_body_hits_the_total_deadline_and_unread_response_lease_expires() {
        // One byte every 5 s keeps the idle rule happy; the total deadline
        // (20 s) still ends it.
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, axum::Error>>(16);
        let (body, outcome) = LimitedBody::new(
            Body::new(tokio_stream_from(rx)),
            1 << 20,
            Duration::from_secs(10),
            Duration::from_secs(20),
        );
        let feeder = tokio::spawn(async move {
            for _ in 0..10 {
                if tx
                    .send(Ok(Frame::data(Bytes::from_static(b"x"))))
                    .await
                    .is_err()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
        let collect = tokio::spawn(async move { Body::new(body).collect().await });
        tokio::time::advance(Duration::from_secs(21)).await;
        let error = collect.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("too long"), "{error}");
        assert_eq!(outcome.stopped_by(), STOP_TIMEOUT);
        feeder.abort();

        // A response body that is never polled (client not reading) still
        // gives its permit back when the lease ends.
        let guard =
            crate::ingress_guard::IngressGuard::new(crate::ingress_guard::TrustedProxies::none());
        let limits = treer_protocol::EffectiveIngressLimits::default();
        let admission = guard
            .admit(
                "ing_y",
                &limits,
                "203.0.113.1".parse().unwrap(),
                &axum::http::HeaderMap::new(),
            )
            .unwrap();
        let (_tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, axum::Error>>(4);
        let body = GuardedResponseBody::new(
            Body::new(tokio_stream_from(rx)),
            Duration::from_secs(300),
            Duration::from_secs(120),
            Some(Arc::new(admission.permit)),
        );
        assert_eq!(guard.in_flight("ing_y"), 1);
        // Let the lease task start its timer before the clock jumps, then
        // park once so the expired timer is delivered.
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(guard.in_flight("ing_y"), 1, "the lease has not expired yet");
        tokio::time::advance(Duration::from_secs(121)).await;
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(
            guard.in_flight("ing_y"),
            0,
            "lease expired while the body sat unpolled"
        );
        drop(body);
        assert_eq!(guard.in_flight("ing_y"), 0);

        // A body that ends early cancels its lease task.
        let admission = guard
            .admit(
                "ing_z",
                &limits,
                "203.0.113.1".parse().unwrap(),
                &axum::http::HeaderMap::new(),
            )
            .unwrap();
        let body = GuardedResponseBody::new(
            Body::from("done"),
            Duration::from_secs(300),
            Duration::from_secs(120),
            Some(Arc::new(admission.permit)),
        );
        let lease = body.lease.as_ref().unwrap().abort_handle();
        assert!(!lease.is_finished());
        drop(body);
        tokio::task::yield_now().await;
        assert!(lease.is_finished(), "lease task cancelled with the body");
        assert_eq!(guard.in_flight("ing_z"), 0);
    }

    fn tokio_stream_from(
        rx: tokio::sync::mpsc::Receiver<Result<Frame<Bytes>, axum::Error>>,
    ) -> http_body_util::StreamBody<
        impl futures_util::Stream<Item = Result<Frame<Bytes>, axum::Error>>,
    > {
        http_body_util::StreamBody::new(futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        }))
    }
}

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
    outcome: BodyOutcome,
}

impl LimitedBody {
    pub(crate) fn new(inner: Body, cap: u64, idle: Duration) -> (Self, BodyOutcome) {
        let outcome = BodyOutcome::default();
        (
            Self {
                inner,
                remaining: cap,
                idle,
                deadline: Box::pin(tokio::time::sleep(idle)),
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
/// when the client goes away) and fails when the upstream stalls for `idle`.
pub(crate) struct GuardedResponseBody {
    inner: Body,
    idle: Duration,
    deadline: Pin<Box<tokio::time::Sleep>>,
    _permit: Option<Arc<Permit>>,
}

impl GuardedResponseBody {
    pub(crate) fn new(inner: Body, idle: Duration, permit: Option<Arc<Permit>>) -> Self {
        Self {
            inner,
            idle,
            deadline: Box::pin(tokio::time::sleep(idle)),
            _permit: permit,
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
        let (body, outcome) =
            LimitedBody::new(Body::from(vec![0_u8; 10]), 4, Duration::from_secs(5));
        let error = Body::new(body).collect().await.unwrap_err();
        assert!(error.to_string().contains("exceeds"));
        assert_eq!(outcome.stopped_by(), STOP_TOO_LARGE);

        let (body, outcome) =
            LimitedBody::new(Body::from(vec![1_u8; 4]), 4, Duration::from_secs(5));
        let bytes = Body::new(body).collect().await.unwrap().to_bytes();
        assert_eq!(bytes.len(), 4);
        assert_eq!(outcome.stopped_by(), STOP_NONE);
    }

    #[tokio::test(start_paused = true)]
    async fn limited_body_times_out_when_no_bytes_arrive() {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, axum::Error>>(4);
        let stream = tokio_stream_from(rx);
        let (body, outcome) = LimitedBody::new(Body::new(stream), 1 << 20, Duration::from_secs(10));
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
            Some(Arc::new(admission.permit)),
        );
        assert_eq!(guard.in_flight("ing_x"), 1);
        drop(body);
        assert_eq!(guard.in_flight("ing_x"), 0);
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

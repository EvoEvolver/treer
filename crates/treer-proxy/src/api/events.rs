use super::*;
use std::time::Instant;

/// A subscriber is re-checked before a delivery once this much time passed
/// since the last check, and on the idle tick below, so a revoked member or
/// an ended session stops receiving events within that window (#141).
pub(super) const STREAM_REVALIDATE_AFTER: Duration = Duration::from_secs(10);
pub(super) const STREAM_IDLE_TICK: Duration = Duration::from_secs(30);
static REVALIDATE_AFTER_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(STREAM_REVALIDATE_AFTER.as_millis() as u64);
static IDLE_TICK_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(STREAM_IDLE_TICK.as_millis() as u64);

fn revalidate_after() -> Duration {
    Duration::from_millis(REVALIDATE_AFTER_MS.load(std::sync::atomic::Ordering::Relaxed))
}

fn idle_tick() -> Duration {
    Duration::from_millis(IDLE_TICK_MS.load(std::sync::atomic::Ordering::Relaxed))
}

/// Tests shrink the windows; production never changes them.
#[cfg(test)]
pub(super) fn set_stream_revalidation_for_test(after: Duration, idle: Duration) {
    REVALIDATE_AFTER_MS.store(
        after.as_millis() as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
    IDLE_TICK_MS.store(
        idle.as_millis() as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
}
/// A re-check that keeps failing (database unavailable) may keep a subscriber
/// at most this long before the stream closes instead (#141 P3).
pub(super) const STREAM_UNVERIFIED_MAX: Duration = Duration::from_secs(120);
/// Fixed WebSocket close codes; the reason is always empty.
pub(super) const CLOSE_SESSION_ENDED: u16 = 4401;
pub(super) const CLOSE_MEMBERSHIP_ENDED: u16 = 4403;
pub(super) const CLOSE_UNVERIFIED: u16 = 4503;

pub(super) async fn workspace_events(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(browser): Extension<BrowserAccess>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiFailure> {
    browser.validate_if_present(&headers)?;
    state.snapshot(&workspace_id).await?;
    Ok(ws.on_upgrade(move |socket| {
        stream_workspace_events(socket, state, auth, session, workspace_id)
    }))
}

/// Why a subscriber stops receiving events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StreamRevocation {
    /// The session was logged out, expired or its user was deleted.
    Session,
    /// The user is no longer a current member or the workspace is gone.
    Membership,
    /// The checks could not be completed for too long.
    Unverified,
}

impl StreamRevocation {
    fn close_code(self) -> u16 {
        match self {
            Self::Session => CLOSE_SESSION_ENDED,
            Self::Membership => CLOSE_MEMBERSHIP_ENDED,
            Self::Unverified => CLOSE_UNVERIFIED,
        }
    }
}

/// `Ok(true)` verified, `Ok(false)` could not be verified right now.
type Recheck = Result<bool, StreamRevocation>;

/// The same checks the route middleware ran at connect time: the session
/// lookup `authenticate_request` uses and current workspace membership
/// (organization role, group grants, user grants, workspace existence). A
/// database failure keeps the subscriber (nothing new is granted) and is
/// retried at the next check.
pub(super) async fn revalidate_subscriber(
    auth: &AuthStore,
    session: &CurrentSession,
    workspace_id: &str,
) -> Recheck {
    match auth.revalidate_session(&session.token).await {
        Ok(Some(current)) if current.user_id == session.user_id => {}
        Ok(_) => return Err(StreamRevocation::Session),
        Err(error) => {
            tracing::warn!(code = error.code(), %workspace_id, "event stream session re-check failed; keeping subscriber until the next check");
            return Ok(false);
        }
    }
    match auth
        .require_workspace_member(workspace_id, &session.user_id)
        .await
    {
        Ok(()) => Ok(true),
        Err(error) if error.code() == "database_error" => {
            tracing::warn!(%workspace_id, "event stream membership re-check failed; keeping subscriber until the next check");
            Ok(false)
        }
        Err(_) => Err(StreamRevocation::Membership),
    }
}

/// When the subscriber was last verified and when a check is next due.
struct SubscriberCheck {
    verified_at: Instant,
    due_at: Instant,
}

impl SubscriberCheck {
    fn fresh() -> Self {
        let now = Instant::now();
        Self {
            verified_at: now,
            due_at: now + revalidate_after(),
        }
    }

    fn force(&mut self) {
        self.due_at = Instant::now();
    }
}

pub(super) async fn stream_workspace_events(
    socket: WebSocket,
    state: AppState,
    auth: AuthStore,
    session: CurrentSession,
    workspace_id: String,
) {
    let (mut outgoing, mut incoming) = socket.split();
    let mut events = state.subscribe();
    // The route middleware authenticated and authorized this connection just
    // now; every later delivery re-checks after the window.
    let mut check = SubscriberCheck::fresh();
    if let Ok(snapshot) = state.snapshot(&workspace_id).await {
        let initial = WorkspaceEvent {
            revision: snapshot.revision,
            workspace_id: workspace_id.clone(),
            event: "workspace.snapshot".to_string(),
            data: serde_json::to_value(snapshot).unwrap_or(Value::Null),
        };
        if send_event(&mut outgoing, &initial).await.is_err() {
            return;
        }
    }
    let mut idle = tokio::time::interval(idle_tick());
    idle.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    idle.reset();

    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) if event.workspace_id == workspace_id => {
                    if !still_subscribed(&auth, &session, &workspace_id, &mut check, &mut outgoing).await {
                        break;
                    }
                    if send_event(&mut outgoing, &event).await.is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    if !still_subscribed(&auth, &session, &workspace_id, &mut check, &mut outgoing).await {
                        break;
                    }
                    let Ok(snapshot) = state.snapshot(&workspace_id).await else { break };
                    let event = WorkspaceEvent {
                        revision: snapshot.revision,
                        workspace_id: workspace_id.clone(),
                        event: "workspace.snapshot".to_string(),
                        data: serde_json::to_value(snapshot).unwrap_or(Value::Null),
                    };
                    if send_event(&mut outgoing, &event).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            _ = idle.tick() => {
                // Force a check: an idle subscriber must also lose a revoked
                // membership within the tick.
                check.force();
                if !still_subscribed(&auth, &session, &workspace_id, &mut check, &mut outgoing).await {
                    break;
                }
            }
            message = incoming.next() => {
                if message.is_none() || message.is_some_and(|item| item.is_err()) {
                    break;
                }
            }
        }
    }
}

/// Re-checks the subscriber when a check is due; on revocation, or when the
/// checks could not complete for `STREAM_UNVERIFIED_MAX`, sends the fixed
/// close frame and reports false. Nothing about the session or the reason is
/// logged beyond the fixed code.
async fn still_subscribed(
    auth: &AuthStore,
    session: &CurrentSession,
    workspace_id: &str,
    check: &mut SubscriberCheck,
    outgoing: &mut futures_util::stream::SplitSink<WebSocket, Message>,
) -> bool {
    let now = Instant::now();
    if now < check.due_at {
        return true;
    }
    let outcome = match revalidate_subscriber(auth, session, workspace_id).await {
        Ok(true) => {
            *check = SubscriberCheck::fresh();
            return true;
        }
        Ok(false) if now.saturating_duration_since(check.verified_at) < STREAM_UNVERIFIED_MAX => {
            // Retry at the next window; the subscriber gains nothing meanwhile.
            check.due_at = now + revalidate_after();
            return true;
        }
        Ok(false) => Err(StreamRevocation::Unverified),
        Err(revocation) => Err(revocation),
    };
    match outcome {
        Ok(()) => true,
        Err(revocation) => {
            tracing::info!(%workspace_id, code = revocation.close_code(), "event stream subscriber revoked");
            let _ = outgoing
                .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                    code: revocation.close_code(),
                    reason: "".into(),
                })))
                .await;
            false
        }
    }
}

pub(super) async fn send_event(
    outgoing: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    event: &WorkspaceEvent,
) -> Result<(), axum::Error> {
    let encoded = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string());
    outgoing.send(Message::Text(encoded.into())).await
}

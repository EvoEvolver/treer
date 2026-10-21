//! Tunnelled WebSocket revalidation (#194): through the real server module and
//! the fake machine tunnel, an upgraded socket on an Agent interface origin is
//! closed with 4401/4403 within one re-check interval after its session or
//! access is gone, stays open otherwise, and never extends a session.
use super::agent_origin_tests::{
    agent_fixture, allow, deny, establish_session, AGENT_HOST, AGENT_ID,
};
use super::embed_session_tests::establish_embed_session;
use super::ingress_protection_tests::{config, Fixture};
use super::network::{
    set_tunnel_check_timeout_for_test, set_tunnel_revalidation_for_test, websocket_close_frame,
    AbortOnDrop, RevalidationOutcome, UpgradeRevalidation, TUNNEL_CLOSE_ACCESS_REVOKED,
    TUNNEL_CLOSE_SESSION_ENDED, TUNNEL_CLOSE_UNVERIFIED,
};
use super::*;
use crate::state::SocketFrame;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use treer_protocol::{NetworkBinaryFrame, NetworkBinaryKind};

const INTERVAL: Duration = Duration::from_millis(250);
const CHECK_TIMEOUT: Duration = Duration::from_millis(300);
const DEADLINE: Duration = Duration::from_secs(10);

struct Harness {
    fixture: Fixture,
    config: IngressConfig,
    address: std::net::SocketAddr,
    upstream_closed: Arc<AtomicBool>,
    _stop: tokio::sync::oneshot::Sender<()>,
}

/// Fake App behind the machine tunnel: completes every WebSocket handshake,
/// echoes every later byte, and notes when the Proxy closes its side.
async fn harness(policy: PolicyEngine) -> Harness {
    set_tunnel_revalidation_for_test(INTERVAL, Duration::ZERO);
    set_tunnel_check_timeout_for_test(CHECK_TIMEOUT);
    let mut fixture = agent_fixture().await;
    let state = fixture.state.clone();
    let connection = fixture.connection;
    let mut rx = fixture.rx.take().unwrap();
    let upstream_closed = Arc::new(AtomicBool::new(false));
    let closed = upstream_closed.clone();
    tokio::spawn(async move {
        let mut handshaken: std::collections::HashSet<String> = Default::default();
        while let Some(frame) = rx.recv().await {
            let SocketFrame::Binary(bytes) = frame else {
                continue;
            };
            let Ok(frame) = NetworkBinaryFrame::decode(&bytes) else {
                continue;
            };
            let reply = |kind, payload| NetworkBinaryFrame {
                kind,
                stream_id: frame.stream_id.clone(),
                payload,
            };
            let out = match frame.kind {
                NetworkBinaryKind::Open => Some(reply(NetworkBinaryKind::Opened, vec![])),
                NetworkBinaryKind::Data => {
                    if handshaken.contains(&frame.stream_id) {
                        Some(reply(NetworkBinaryKind::Data, frame.payload.clone()))
                    } else if frame.payload.windows(4).any(|w| w == b"\r\n\r\n") {
                        handshaken.insert(frame.stream_id.clone());
                        Some(reply(NetworkBinaryKind::Data, b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n".to_vec()))
                    } else {
                        None
                    }
                }
                NetworkBinaryKind::HalfClose | NetworkBinaryKind::Reset => {
                    closed.store(true, Ordering::SeqCst);
                    Some(reply(NetworkBinaryKind::HalfClose, vec![]))
                }
                _ => None,
            };
            if let Some(out) = out {
                let _ = state
                    .relay_network_frame("default", "machine-a", connection, out)
                    .await;
            }
        }
    });
    let config = config(None);
    let router = Router::new()
        .fallback(axum::routing::any(proxy_service_ingress))
        .layer(Extension(fixture.auth.clone()))
        .layer(Extension(policy))
        .layer(Extension(config.clone()))
        .layer(Extension(fixture.identity.clone()))
        .with_state(fixture.state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stop_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        crate::server::serve(
            listener,
            router,
            crate::server::ServerLimits::default(),
            async move {
                let _ = stop_rx.await;
            },
        )
        .await
        .unwrap();
    });
    Harness {
        fixture,
        config,
        address,
        upstream_closed,
        _stop: stop,
    }
}

/// Opens a WebSocket through the Agent origin with the given cookie pair and
/// returns the raw upgraded stream after the 101.
async fn open_socket(harness: &Harness, cookie: &str) -> TcpStream {
    let mut client = TcpStream::connect(harness.address).await.unwrap();
    let request = format!(
        "GET /ws HTTP/1.1\r\nHost: {AGENT_HOST}\r\nOrigin: https://{AGENT_HOST}\r\nCookie: {cookie}\r\n\
         Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
    );
    client.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    let mut buffer = [0_u8; 1024];
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        let read = tokio::time::timeout_at(deadline, client.read(&mut buffer))
            .await
            .expect("upgrade response")
            .unwrap();
        response.extend_from_slice(&buffer[..read]);
        if read == 0 || response.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&response);
    assert!(text.starts_with("HTTP/1.1 101"), "{text}");
    assert!(
        !response.ends_with(b"\r\n\r\n")
            || response.windows(4).filter(|w| *w == b"\r\n\r\n").count() == 1,
        "nothing beyond the head yet"
    );
    client
}

/// A masked client text frame; the echo upstream returns the same bytes.
fn masked_text_frame(text: &str) -> Vec<u8> {
    let mask = [0x12, 0x34, 0x56, 0x78];
    let mut frame = vec![0x81, 0x80 | text.len() as u8];
    frame.extend_from_slice(&mask);
    frame.extend(text.bytes().enumerate().map(|(i, byte)| byte ^ mask[i % 4]));
    frame
}

async fn read_exact_within(client: &mut TcpStream, len: usize) -> Vec<u8> {
    let mut out = vec![0_u8; len];
    tokio::time::timeout(DEADLINE, client.read_exact(&mut out))
        .await
        .expect("bytes within the deadline")
        .unwrap();
    out
}

async fn assert_echo(client: &mut TcpStream) {
    let frame = masked_text_frame("ping");
    client.write_all(&frame).await.unwrap();
    assert_eq!(read_exact_within(client, frame.len()).await, frame);
}

/// The next bytes are a close frame with `code`, then EOF.
async fn assert_closed(client: &mut TcpStream, code: u16) {
    let head = read_exact_within(client, 2).await;
    assert_eq!(head[0], 0x88, "close opcode, got {head:?}");
    let len = usize::from(head[1]);
    assert!(len >= 2, "{head:?}");
    let payload = read_exact_within(client, len).await;
    assert_eq!(u16::from_be_bytes([payload[0], payload[1]]), code);
    let mut rest = [0_u8; 16];
    let read = tokio::time::timeout(DEADLINE, client.read(&mut rest))
        .await
        .expect("EOF within the deadline")
        .unwrap();
    assert_eq!(read, 0, "stream ends after the close frame");
}

async fn wait_upstream_closed(harness: &Harness) {
    let deadline = tokio::time::Instant::now() + DEADLINE;
    while !harness.upstream_closed.load(Ordering::SeqCst) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "upstream half closed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn standalone_cookie(harness: &Harness, user: &str) -> String {
    establish_session(
        &harness.fixture,
        allow(),
        &harness.config,
        AGENT_HOST,
        &format!("tok-{user}"),
    )
    .await
}

#[tokio::test]
async fn deleted_ingress_session_closes_the_socket_with_4401_and_the_upstream() {
    let harness = harness(allow()).await;
    let cookie = standalone_cookie(&harness, "member").await;
    let mut client = open_socket(&harness, &cookie).await;
    assert_echo(&mut client).await;
    sqlx::query("DELETE FROM ingress_sessions WHERE user_id = 'member'")
        .execute(&harness.fixture.auth.pool())
        .await
        .unwrap();
    assert_closed(&mut client, TUNNEL_CLOSE_SESSION_ENDED).await;
    wait_upstream_closed(&harness).await;
}

#[tokio::test]
async fn expired_ingress_session_closes_the_socket_with_4401() {
    let harness = harness(allow()).await;
    let cookie = standalone_cookie(&harness, "member").await;
    let mut client = open_socket(&harness, &cookie).await;
    assert_echo(&mut client).await;
    sqlx::query(
        "UPDATE ingress_sessions SET expires_at = '2000-01-01T00:00:00Z' WHERE user_id = 'member'",
    )
    .execute(&harness.fixture.auth.pool())
    .await
    .unwrap();
    assert_closed(&mut client, TUNNEL_CLOSE_SESSION_ENDED).await;
}

#[tokio::test]
async fn removed_membership_closes_the_socket_with_4403() {
    let harness = harness(allow()).await;
    let cookie = standalone_cookie(&harness, "member").await;
    let mut client = open_socket(&harness, &cookie).await;
    assert_echo(&mut client).await;
    sqlx::query("DELETE FROM organization_members WHERE user_id = 'member'")
        .execute(&harness.fixture.auth.pool())
        .await
        .unwrap();
    assert_closed(&mut client, TUNNEL_CLOSE_ACCESS_REVOKED).await;
    wait_upstream_closed(&harness).await;
}

#[tokio::test]
async fn an_unchanged_session_keeps_the_socket_and_is_not_extended() {
    let harness = harness(allow()).await;
    let cookie = standalone_cookie(&harness, "member").await;
    let before: (String, String) = sqlx::query_as(
        "SELECT (SELECT expires_at FROM ingress_sessions WHERE user_id = 'member'), \
         (SELECT expires_at FROM sessions WHERE token = 'tok-member')",
    )
    .fetch_one(&harness.fixture.auth.pool())
    .await
    .unwrap();
    let mut client = open_socket(&harness, &cookie).await;
    assert_echo(&mut client).await;
    tokio::time::sleep(INTERVAL * 4).await;
    assert_echo(&mut client).await;
    let after: (String, String) = sqlx::query_as(
        "SELECT (SELECT expires_at FROM ingress_sessions WHERE user_id = 'member'), \
         (SELECT expires_at FROM sessions WHERE token = 'tok-member')",
    )
    .fetch_one(&harness.fixture.auth.pool())
    .await
    .unwrap();
    assert_eq!(before, after, "re-checks never slide a session");
    assert!(!harness.upstream_closed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn console_logout_closes_the_embed_socket_and_leaves_other_sessions_alone() {
    let harness = harness(allow()).await;
    // Member: one embed socket; owner: one standalone socket.
    let embed_cookie = establish_embed_session(&harness.fixture, &harness.config, "member").await;
    let owner_cookie = standalone_cookie(&harness, "owner").await;
    let mut embed = open_socket(&harness, &embed_cookie).await;
    let mut owner = open_socket(&harness, &owner_cookie).await;
    assert_echo(&mut embed).await;
    assert_echo(&mut owner).await;
    harness.fixture.auth.logout("tok-member").await.unwrap();
    assert_closed(&mut embed, TUNNEL_CLOSE_SESSION_ENDED).await;
    assert_echo(&mut owner).await;

    // The same user with two standalone sessions: deleting one closes only
    // its socket; the other socket and its requests are unaffected.
    let first = standalone_cookie(&harness, "owner").await;
    assert_ne!(first, owner_cookie);
    let mut first_socket = open_socket(&harness, &first).await;
    assert_echo(&mut first_socket).await;
    let token = first.split_once('=').unwrap().1;
    let deleted = sqlx::query("DELETE FROM ingress_sessions WHERE token = $1")
        .bind(token)
        .execute(&harness.fixture.auth.pool())
        .await
        .unwrap()
        .rows_affected();
    assert_eq!(deleted, 1);
    assert_closed(&mut first_socket, TUNNEL_CLOSE_SESSION_ENDED).await;
    assert_echo(&mut owner).await;
    let page = super::agent_origin_tests::send(
        &harness.fixture,
        allow(),
        &harness.config,
        Request::builder()
            .uri("/ui")
            .header(header::HOST, AGENT_HOST)
            .header(header::COOKIE, &owner_cookie)
            .header("sec-fetch-site", "same-origin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    // The surviving session still authenticates on the HTTP path (the fake
    // upstream only answers upgrades, so the tunnel itself may fail later).
    assert!(
        page.as_ref()
            .map(|r| r.status() != StatusCode::SEE_OTHER && r.status() != StatusCode::UNAUTHORIZED)
            .unwrap_or(true),
        "{:?}",
        page.as_ref().map(|r| r.status())
    );
}

#[tokio::test]
async fn database_outage_closes_the_socket_after_three_failed_checks() {
    let harness = harness(allow()).await;
    let cookie = standalone_cookie(&harness, "member").await;
    let mut client = open_socket(&harness, &cookie).await;
    assert_echo(&mut client).await;
    let started = tokio::time::Instant::now();
    harness.fixture.auth.pool().close().await;
    assert_closed(&mut client, TUNNEL_CLOSE_UNVERIFIED).await;
    assert!(
        started.elapsed() >= INTERVAL * 3,
        "two failed checks keep the socket: {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn console_session_revalidation_tracks_logout_membership_and_policy() {
    let fixture = agent_fixture().await;
    let agent = fixture
        .state
        .resolve_agent("default", AGENT_ID)
        .await
        .unwrap();
    let verify = |policy: PolicyEngine| {
        UpgradeRevalidation::console(fixture.auth.clone(), "tok-member", "default", "member")
            .with_agent_gate(policy, agent.clone())
    };
    let allowed = verify(allow());
    let expires_before: String =
        sqlx::query_scalar("SELECT expires_at FROM sessions WHERE token = 'tok-member'")
            .fetch_one(&fixture.auth.pool())
            .await
            .unwrap();
    for _ in 0..3 {
        assert_eq!(allowed.check().await, RevalidationOutcome::Verified);
    }
    let expires_after: String =
        sqlx::query_scalar("SELECT expires_at FROM sessions WHERE token = 'tok-member'")
            .fetch_one(&fixture.auth.pool())
            .await
            .unwrap();
    assert_eq!(expires_before, expires_after);
    // Debug never prints the token.
    let shown = format!("{allowed:?}");
    assert!(!shown.contains("tok-member"), "{shown}");
    assert!(
        shown.contains("console") && shown.contains("member"),
        "{shown}"
    );
    // Policy denial of the interface gate revokes access without a record.
    assert_eq!(
        verify(deny()).check().await,
        RevalidationOutcome::AccessRevoked
    );
    // Another user's session is not this socket's session.
    let other =
        UpgradeRevalidation::console(fixture.auth.clone(), "tok-owner", "default", "member");
    assert_eq!(other.check().await, RevalidationOutcome::SessionEnded);
    // Membership removal.
    sqlx::query("DELETE FROM organization_members WHERE user_id = 'member'")
        .execute(&fixture.auth.pool())
        .await
        .unwrap();
    assert_eq!(allowed.check().await, RevalidationOutcome::AccessRevoked);
    // Logout ends the session itself (the owner stays a member).
    let owner = UpgradeRevalidation::console(fixture.auth.clone(), "tok-owner", "default", "owner");
    assert_eq!(owner.check().await, RevalidationOutcome::Verified);
    fixture.auth.logout("tok-owner").await.unwrap();
    assert_eq!(owner.check().await, RevalidationOutcome::SessionEnded);
}

#[test]
fn close_frames_are_unmasked_with_code_and_reason() {
    assert_eq!(
        websocket_close_frame(4401, "session ended"),
        [&[0x88, 15, 0x11, 0x31][..], b"session ended"].concat()
    );
    let long = websocket_close_frame(1011, &"x".repeat(200));
    assert_eq!(long[1], 125);
    assert_eq!(long.len(), 127);
}

/// A lookup that hangs (every pool connection held elsewhere) never pauses
/// the relay: echoes keep flowing while checks time out, and the socket closes
/// with 1011 after three timed-out checks.
#[tokio::test]
async fn stalled_lookups_keep_bytes_flowing_and_close_after_three_timeouts() {
    let harness = harness(allow()).await;
    let cookie = standalone_cookie(&harness, "member").await;
    let mut client = open_socket(&harness, &cookie).await;
    assert_echo(&mut client).await;
    let pool = harness.fixture.auth.pool();
    let max = pool.options().get_max_connections();
    let mut held = Vec::new();
    for _ in 0..max {
        held.push(
            tokio::time::timeout(DEADLINE, pool.acquire())
                .await
                .expect("acquire within the deadline")
                .unwrap(),
        );
    }
    let started = tokio::time::Instant::now();
    // Past the first check's start and well into its timeout: still echoing.
    tokio::time::sleep(INTERVAL + CHECK_TIMEOUT / 2).await;
    assert_echo(&mut client).await;
    tokio::time::sleep(INTERVAL + CHECK_TIMEOUT).await;
    assert_echo(&mut client).await;
    assert_closed(&mut client, TUNNEL_CLOSE_UNVERIFIED).await;
    let elapsed = started.elapsed();
    assert!(
        elapsed >= (INTERVAL + CHECK_TIMEOUT) * 3 - Duration::from_millis(50),
        "three timed-out checks: {elapsed:?}"
    );
    drop(held);
}

/// Dropping the relay (cancellation, panic) stops the checker task and with
/// it the held session token (security robustness item on e1a77ab0).
#[tokio::test]
async fn dropping_the_relay_aborts_the_checker_task() {
    let checker = tokio::spawn(std::future::pending::<()>());
    let guard = AbortOnDrop(checker.abort_handle());
    tokio::task::yield_now().await;
    assert!(!checker.is_finished());
    drop(guard);
    let outcome = tokio::time::timeout(DEADLINE, checker)
        .await
        .expect("checker ends once the guard is dropped");
    assert!(outcome.unwrap_err().is_cancelled());
}

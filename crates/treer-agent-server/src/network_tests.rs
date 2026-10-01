use super::*;
use std::time::Duration;
use tokio::net::TcpListener;

async fn next(runtime: &NetworkRuntime) -> NetworkBinaryFrame {
    tokio::time::timeout(Duration::from_secs(3), runtime.next_outgoing())
        .await
        .unwrap()
        .unwrap()
}

fn frame(kind: NetworkBinaryKind, id: &str, payload: Vec<u8>) -> NetworkBinaryFrame {
    NetworkBinaryFrame {
        kind,
        stream_id: id.to_string(),
        payload,
    }
}

fn open(id: &str, port: u16) -> NetworkBinaryFrame {
    frame(
        NetworkBinaryKind::Open,
        id,
        serde_json::to_vec(&NetworkConnectRequest {
            source_server_id: "server-source".to_string(),
            source_agent_id: None,
            host: "127.0.0.1".to_string(),
            port,
            destination_agent_id: Some("agent-a".to_string()),
        })
        .unwrap(),
    )
}

#[tokio::test]
async fn explicit_service_preserves_binary_payload_and_directional_eof() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let service = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut body = Vec::new();
        socket.read_to_end(&mut body).await.unwrap();
        assert_eq!(body, [0, 255, 1, 128]);
        socket.write_all(&[9, 0, 254]).await.unwrap();
        socket.shutdown().await.unwrap();
    });
    let runtime = NetworkRuntime::new();
    runtime.set_proxy_connected();
    runtime.handle_incoming(open("test", port)).await.unwrap();
    assert_eq!(next(&runtime).await.kind, NetworkBinaryKind::Opened);
    runtime
        .handle_incoming(frame(NetworkBinaryKind::Data, "test", vec![0, 255, 1, 128]))
        .await
        .unwrap();
    runtime
        .handle_incoming(frame(NetworkBinaryKind::HalfClose, "test", vec![]))
        .await
        .unwrap();
    let mut output = Vec::new();
    let mut eof = false;
    loop {
        let incoming = next(&runtime).await;
        match incoming.kind {
            NetworkBinaryKind::Data => output.extend(incoming.payload),
            NetworkBinaryKind::HalfClose => eof = true,
            NetworkBinaryKind::Reset => break,
            NetworkBinaryKind::WindowUpdate => {}
            other => panic!("unexpected frame {other:?}"),
        }
    }
    assert_eq!(output, [9, 0, 254]);
    assert!(eof);
    service.await.unwrap();
}

#[tokio::test]
async fn reconnect_discards_queued_frames_and_cancels_old_streams() {
    let runtime = NetworkRuntime::new();
    runtime.set_proxy_connected();
    runtime
        .send(frame(NetworkBinaryKind::Data, "old", vec![1]))
        .await
        .unwrap();
    let stale = NetworkRuntime {
        inner: runtime.inner.clone(),
        epoch: Some(0),
    };
    runtime.reset_all().await;
    runtime.set_proxy_connected();
    assert!(stale
        .send(frame(NetworkBinaryKind::Data, "old", vec![2]))
        .await
        .is_err());
    runtime
        .send(frame(NetworkBinaryKind::Data, "new", vec![3]))
        .await
        .unwrap();
    assert_eq!(next(&runtime).await.stream_id, "new");
}

#[tokio::test]
async fn explicit_service_rejects_retired_datagram_and_direct_frames() {
    let runtime = NetworkRuntime::new();
    runtime.set_proxy_connected();
    for kind in [NetworkBinaryKind::OpenDatagram, NetworkBinaryKind::Direct] {
        runtime
            .handle_incoming(frame(kind, "retired", vec![]))
            .await
            .unwrap();
        let rejected = next(&runtime).await;
        assert_eq!(rejected.kind, NetworkBinaryKind::Reset);
        let error: ProtocolError = serde_json::from_slice(&rejected.payload).unwrap();
        assert_eq!(error.code, "unsupported_transport");
    }
}

#[tokio::test]
async fn disconnected_transport_cannot_dial_new_service() {
    let runtime = NetworkRuntime::new();
    assert!(runtime.handle_incoming(open("offline", 1)).await.is_err());
    assert!(runtime.inner.streams.lock().await.is_empty());
}

#[tokio::test]
async fn reset_closes_service_without_blocking_the_controller() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let runtime = NetworkRuntime::new();
    runtime.set_proxy_connected();
    runtime.handle_incoming(open("reset", port)).await.unwrap();
    let (mut socket, _) = listener.accept().await.unwrap();
    assert_eq!(next(&runtime).await.kind, NetworkBinaryKind::Opened);
    runtime.reset_all().await;
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), socket.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn invalid_window_credit_resets_only_its_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let runtime = NetworkRuntime::new();
    runtime.set_proxy_connected();
    runtime
        .handle_incoming(open("window", listener.local_addr().unwrap().port()))
        .await
        .unwrap();
    let (_socket, _) = listener.accept().await.unwrap();
    assert_eq!(next(&runtime).await.kind, NetworkBinaryKind::Opened);
    runtime
        .handle_incoming(frame(
            NetworkBinaryKind::WindowUpdate,
            "window",
            1_u32.to_be_bytes().to_vec(),
        ))
        .await
        .unwrap();
    let rejected = next(&runtime).await;
    assert_eq!(rejected.kind, NetworkBinaryKind::Reset);
    assert!(String::from_utf8(rejected.payload)
        .unwrap()
        .contains("window credit"));
}

#[tokio::test]
async fn service_output_obeys_credit_before_resuming() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let runtime = NetworkRuntime::new();
    runtime.set_proxy_connected();
    runtime
        .handle_incoming(open("flow", listener.local_addr().unwrap().port()))
        .await
        .unwrap();
    let (mut socket, _) = listener.accept().await.unwrap();
    let write = tokio::spawn(async move {
        socket
            .write_all(&vec![42; INITIAL_WINDOW + 1])
            .await
            .unwrap();
        socket.shutdown().await.unwrap();
    });
    assert_eq!(next(&runtime).await.kind, NetworkBinaryKind::Opened);
    let mut count = 0;
    while count < INITIAL_WINDOW {
        let incoming = next(&runtime).await;
        assert_eq!(incoming.kind, NetworkBinaryKind::Data);
        count += incoming.payload.len();
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(50), runtime.next_outgoing())
            .await
            .is_err()
    );
    runtime
        .handle_incoming(frame(
            NetworkBinaryKind::WindowUpdate,
            "flow",
            1_u32.to_be_bytes().to_vec(),
        ))
        .await
        .unwrap();
    assert_eq!(next(&runtime).await.payload, [42]);
    runtime.reset_all().await;
    write.await.unwrap();
}

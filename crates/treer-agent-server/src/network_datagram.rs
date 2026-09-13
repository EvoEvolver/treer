//! Datagram boundaries are preserved across both the local framed transport and
//! Proxy Data frames. Every association is authorized through the normal Open
//! path. There is no unauthenticated UDP listener or direct policy bypass.
use std::time::Duration;

use anyhow::{anyhow, bail, ensure, Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use treer_protocol::{
    NetworkBinaryFrame, NetworkBinaryKind as Kind, NetworkConnectRequest, NetworkDirectTarget,
    NetworkUsageTotals,
};

use crate::network::NetworkRuntime;

const LIMIT: usize = 65_507;
const IDLE: Duration = Duration::from_secs(60);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const WINDOW: usize = 256 * 1024;

fn charge(length: usize) -> usize {
    length + 64
}

fn add_credit(window: &mut usize, payload: &[u8]) -> Result<()> {
    let bytes: [u8; 4] = payload
        .try_into()
        .context("invalid datagram window update")?;
    let credit = u32::from_be_bytes(bytes) as usize;
    ensure!(
        credit > 0 && credit <= WINDOW.saturating_sub(*window),
        "invalid datagram window credit"
    );
    *window += credit;
    Ok(())
}

async fn read_packet(reader: &mut (impl AsyncRead + Unpin)) -> Result<Vec<u8>> {
    let length = usize::from(reader.read_u16().await?);
    ensure!(length <= LIMIT, "datagram exceeds UDP payload limit");
    let mut data = vec![0; length];
    reader.read_exact(&mut data).await?;
    Ok(data)
}

async fn write_packet(writer: &mut (impl AsyncWrite + Unpin), data: &[u8]) -> Result<()> {
    ensure!(data.len() <= LIMIT, "datagram exceeds UDP payload limit");
    tokio::time::timeout(WRITE_TIMEOUT, async {
        writer.write_u16(data.len() as u16).await?;
        writer.write_all(data).await
    })
    .await
    .context("datagram client stopped reading")??;
    Ok(())
}

async fn connect(host: &str, port: u16) -> Result<UdpSocket> {
    ensure!(port > 0, "UDP destination port is zero");
    let address = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::lookup_host((host, port)),
    )
    .await??
    .next()
    .context("UDP destination has no address")?;
    let socket = UdpSocket::bind(if address.is_ipv6() {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    })
    .await?;
    socket.connect(address).await?;
    Ok(socket)
}

async fn send(runtime: &NetworkRuntime, id: &str, kind: Kind, payload: Vec<u8>) -> Result<()> {
    tokio::time::timeout(
        WRITE_TIMEOUT,
        runtime.send(NetworkBinaryFrame {
            kind,
            stream_id: id.to_owned(),
            payload,
        }),
    )
    .await
    .context("datagram transport stalled")?
}

pub(crate) async fn source(
    mut socket: TcpStream,
    mut incoming: mpsc::Receiver<NetworkBinaryFrame>,
    runtime: &NetworkRuntime,
    id: &str,
) -> Result<()> {
    let setup = async {
        let route = tokio::time::timeout(Duration::from_secs(15), incoming.recv())
            .await?
            .context("datagram authorization channel closed")?;
        match route.kind {
            Kind::Opened => Ok(None),
            Kind::Direct => {
                let target: NetworkDirectTarget = serde_json::from_slice(&route.payload)?;
                let udp = match connect(&target.host, target.port).await {
                    Ok(udp) => udp,
                    Err(error) => {
                        if target.usage_ticket.is_some() {
                            let _ = runtime
                                .report_usage(
                                    id,
                                    target.usage_ticket.as_deref(),
                                    NetworkUsageTotals::default(),
                                    true,
                                )
                                .await;
                        }
                        return Err(error);
                    }
                };
                Ok(Some((udp, target.report_usage, target.usage_ticket)))
            }
            _ => bail!("datagram route denied or unsupported"),
        }
    }
    .await;
    let direct = match setup {
        Ok(value) => value,
        Err(error) => {
            let _ = socket.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).await;
            return Err(error);
        }
    };
    socket.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    let (mut reader, mut writer) = socket.into_split();
    let (tx, mut packets) = mpsc::channel(8);
    // A separate bounded reader owns partial length/payload state, so a timer
    // or an incoming remote datagram cannot cancel and corrupt framing.
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        loop {
            let packet = read_packet(&mut reader).await;
            let failed = packet.is_err();
            if tx.send(packet).await.is_err() || failed {
                break;
            }
        }
    });
    let mut buffer = vec![0; LIMIT + 1];
    let mut totals = NetworkUsageTotals::default();
    let mut send_window = WINDOW;
    let mut pending_packet: Option<Vec<u8>> = None;
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    tick.tick().await;
    let idle = tokio::time::sleep(IDLE);
    tokio::pin!(idle);
    let result: Result<()> = async {
        loop {
            if pending_packet.as_ref().is_some_and(|data| charge(data.len()) <= send_window) {
                let data = pending_packet.take().unwrap();
                send_window -= charge(data.len());
                send(runtime, id, Kind::Data, data).await?;
                idle.as_mut().reset(tokio::time::Instant::now() + IDLE);
                continue;
            }
            tokio::select! {
                packet = packets.recv(), if pending_packet.is_none() => {
                    let data = packet.context("datagram client closed")??;
                    if let Some((udp, _, _)) = &direct {
                        let sent = udp.send(&data).await?;
                        ensure!(sent == data.len(), "partial UDP send");
                        totals.sent_bytes += sent as u64;
                        totals.sent_chunks += 1;
                    } else { pending_packet = Some(data); }
                    idle.as_mut().reset(tokio::time::Instant::now() + IDLE);
                }
                received = async { direct.as_ref().unwrap().0.recv(&mut buffer).await }, if direct.is_some() => {
                    let length = received?;
                    ensure!(length <= LIMIT, "oversized UDP reply");
                    write_packet(&mut writer, &buffer[..length]).await?;
                    totals.received_bytes += length as u64;
                    totals.received_chunks += 1;
                    idle.as_mut().reset(tokio::time::Instant::now() + IDLE);
                }
                frame = incoming.recv() => {
                    let frame = frame.context("datagram authorization channel closed")?;
                    match frame.kind {
                        Kind::Data if direct.is_none() => {
                            write_packet(&mut writer, &frame.payload).await?;
                            send(runtime, id, Kind::WindowUpdate, (charge(frame.payload.len()) as u32).to_be_bytes().to_vec()).await?;
                            idle.as_mut().reset(tokio::time::Instant::now() + IDLE);
                        }
                        Kind::WindowUpdate if direct.is_none() => add_credit(&mut send_window, &frame.payload)?,
                        Kind::Reset => bail!("datagram authorization reset"),
                        _ => bail!("unexpected datagram frame"),
                    }
                }
                _ = tick.tick(), if direct.as_ref().is_some_and(|(_, report, _)| *report) => {
                    runtime.report_usage(id, direct.as_ref().and_then(|(_, _, ticket)| ticket.as_deref()), totals, false).await?;
                }
                _ = &mut idle => break Ok(()),
            }
        }
    }.await;
    if direct.as_ref().is_some_and(|(_, report, _)| *report) {
        let _ = runtime
            .report_usage(
                id,
                direct.as_ref().and_then(|(_, _, ticket)| ticket.as_deref()),
                totals,
                true,
            )
            .await;
    }
    result
}

pub(crate) async fn destination(
    request: NetworkConnectRequest,
    mut incoming: mpsc::Receiver<NetworkBinaryFrame>,
    runtime: &NetworkRuntime,
    id: &str,
) -> Result<()> {
    // Linux private namespaces currently expose a TCP-only service bridge.
    // Never send an Agent-targeted datagram to the machine's unrelated port.
    ensure!(
        !(cfg!(target_os = "linux") && request.destination_agent_id.is_some()),
        "Linux Agent namespace UDP ingress is not supported"
    );
    let host = if request.destination_agent_id.is_some() {
        "127.0.0.1"
    } else {
        &request.host
    };
    let udp = connect(host, request.port).await?;
    send(runtime, id, Kind::Opened, Vec::new()).await?;
    let mut buffer = vec![0; LIMIT + 1];
    let mut send_window = WINDOW;
    loop {
        tokio::time::timeout(IDLE, async {
            tokio::select! {
                received = udp.recv(&mut buffer), if send_window >= charge(LIMIT) => {
                    let length = received?;
                    ensure!(length <= LIMIT, "oversized UDP reply");
                    send_window -= charge(length);
                    send(runtime, id, Kind::Data, buffer[..length].to_vec()).await
                }
                frame = incoming.recv() => {
                    let frame = frame.context("datagram stream closed")?;
                    if frame.kind == Kind::WindowUpdate {
                        add_credit(&mut send_window, &frame.payload)?;
                        return Ok(());
                    }
                    ensure!(frame.kind == Kind::Data, "datagram stream reset or invalid frame");
                    ensure!(frame.payload.len() <= LIMIT, "oversized UDP datagram");
                    ensure!(udp.send(&frame.payload).await? == frame.payload.len(), "partial UDP send");
                    send(runtime, id, Kind::WindowUpdate, (charge(frame.payload.len()) as u32).to_be_bytes().to_vec()).await
                }
            }
        }).await.map_err(|_| anyhow!("datagram association idle"))??;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_datagram_credit_bounds_empty_packets_and_rejects_inflation() {
        assert_eq!(charge(0), 64);
        let mut window = WINDOW - charge(0);
        add_credit(&mut window, &64_u32.to_be_bytes()).unwrap();
        assert_eq!(window, WINDOW);
        assert!(add_credit(&mut window, &1_u32.to_be_bytes()).is_err());
        assert!(add_credit(&mut window, &[0]).is_err());
    }

    #[tokio::test]
    async fn framing_preserves_empty_binary_and_large_datagrams() {
        let (mut writer, mut reader) = tokio::io::duplex(17);
        let expected = vec![vec![], vec![0, 255, 0], vec![42; LIMIT]];
        let data = expected.clone();
        let task = tokio::spawn(async move {
            for packet in data {
                write_packet(&mut writer, &packet).await.unwrap();
            }
        });
        for packet in expected {
            assert_eq!(read_packet(&mut reader).await.unwrap(), packet);
        }
        task.await.unwrap();
    }

    async fn next(runtime: &NetworkRuntime) -> NetworkBinaryFrame {
        tokio::time::timeout(Duration::from_secs(3), runtime.next_outgoing())
            .await
            .unwrap()
            .unwrap()
    }

    async fn request(runtime: &NetworkRuntime, port: u16) -> TcpStream {
        let mut client = TcpStream::connect(runtime.listen_address()).await.unwrap();
        client.write_all(&[5, 1, 2]).await.unwrap();
        assert_eq!(client.read_u16().await.unwrap(), 0x0502);
        client.write_all(b"\x01\x05agent\x05treer").await.unwrap();
        assert_eq!(client.read_u16().await.unwrap(), 0x0100);
        client
            .write_all(&[5, 0xf0, 0, 1, 127, 0, 0, 1])
            .await
            .unwrap();
        client.write_u16(port).await.unwrap();
        client
    }

    #[tokio::test]
    async fn direct_udp_preserves_boundaries_counts_payload_and_stops_on_reset() {
        let runtime = NetworkRuntime::bind_near("127.0.0.1:0".parse().unwrap(), true)
            .await
            .unwrap();
        runtime.set_proxy_connected();
        let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = echo.local_addr().unwrap().port();
        let echo_task = tokio::spawn(async move {
            let mut buffer = vec![0; LIMIT + 1];
            for _ in 0..3 {
                let (count, peer) = echo.recv_from(&mut buffer).await.unwrap();
                echo.send_to(&buffer[..count], peer).await.unwrap();
            }
        });
        let mut client = request(&runtime, port).await;
        let open = next(&runtime).await;
        assert_eq!(open.kind, Kind::OpenDatagram);
        runtime
            .handle_incoming(NetworkBinaryFrame {
                kind: Kind::Direct,
                stream_id: open.stream_id.clone(),
                payload: serde_json::to_vec(&NetworkDirectTarget {
                    host: "127.0.0.1".into(),
                    port,
                    report_usage: true,
                    usage_ticket: None,
                })
                .unwrap(),
            })
            .await
            .unwrap();
        let mut reply = [0; 10];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[1], 0);
        for packet in [vec![], vec![0, 255, 1], vec![42; 8_192]] {
            write_packet(&mut client, &packet).await.unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(3), read_packet(&mut client))
                    .await
                    .unwrap()
                    .unwrap(),
                packet
            );
        }
        runtime
            .handle_incoming(NetworkBinaryFrame {
                kind: Kind::Reset,
                stream_id: open.stream_id.clone(),
                payload: vec![],
            })
            .await
            .unwrap();
        let usage = next(&runtime).await;
        assert_eq!(usage.kind, Kind::Usage);
        let totals: NetworkUsageTotals = serde_json::from_slice(&usage.payload).unwrap();
        assert_eq!((totals.sent_bytes, totals.received_bytes), (8_195, 8_195));
        assert_eq!((totals.sent_chunks, totals.received_chunks), (3, 3));
        assert_eq!(next(&runtime).await.kind, Kind::Reset);
        assert!(
            tokio::time::timeout(Duration::from_secs(3), client.read_u8())
                .await
                .unwrap()
                .is_err()
        );
        echo_task.await.unwrap();
    }

    #[tokio::test]
    async fn relayed_udp_uses_one_data_frame_per_datagram_and_honors_denial() {
        let runtime = NetworkRuntime::bind_near("127.0.0.1:0".parse().unwrap(), true)
            .await
            .unwrap();
        runtime.set_proxy_connected();
        let mut client = request(&runtime, 9999).await;
        let open = next(&runtime).await;
        runtime
            .handle_incoming(NetworkBinaryFrame {
                kind: Kind::Opened,
                stream_id: open.stream_id.clone(),
                payload: vec![],
            })
            .await
            .unwrap();
        let mut reply = [0; 10];
        client.read_exact(&mut reply).await.unwrap();
        for packet in [vec![], vec![0, 255, 1]] {
            write_packet(&mut client, &packet).await.unwrap();
            let frame = next(&runtime).await;
            assert_eq!(frame.kind, Kind::Data);
            assert_eq!(frame.payload, packet);
            runtime.handle_incoming(frame).await.unwrap();
            assert_eq!(read_packet(&mut client).await.unwrap(), packet);
            let credit = next(&runtime).await;
            assert_eq!(credit.kind, Kind::WindowUpdate);
            runtime.handle_incoming(credit).await.unwrap();
        }
        runtime.reset_all().await;
        assert!(
            tokio::time::timeout(Duration::from_secs(3), client.read_u8())
                .await
                .unwrap()
                .is_err()
        );
        runtime.set_proxy_connected();
        let mut denied = request(&runtime, 9999).await;
        let open = next(&runtime).await;
        runtime
            .handle_incoming(NetworkBinaryFrame {
                kind: Kind::Reset,
                stream_id: open.stream_id,
                payload: vec![],
            })
            .await
            .unwrap();
        denied.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[1], 5);
    }
}

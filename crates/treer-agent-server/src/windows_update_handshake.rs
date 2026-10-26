//! One bounded binding exchange; no commands, lock transfer or readiness.
use anyhow::{anyhow, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use treer_protocol::controller_update_worker::{
    WorkerBindingFrame, WorkerBindingKind, WORKER_BINDING_FRAME_BYTES,
};

fn refused() -> anyhow::Error {
    anyhow!("windows_update_handshake_unconfirmed")
}

/// Strict private argv only. No environment, installed configuration, Host,
/// identity route or service initialization is consulted on this entry.
#[cfg(any(windows, test))]
pub(crate) fn bootstrap_arguments(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(uuid::Uuid, String)> {
    let operation_text = args
        .next()
        .and_then(|v| v.into_string().ok())
        .ok_or_else(refused)?;
    let selector = args
        .next()
        .and_then(|v| v.into_string().ok())
        .ok_or_else(refused)?;
    let operation = uuid::Uuid::parse_str(&operation_text).map_err(|_| refused())?;
    let endpoint = uuid::Uuid::parse_str(&selector).map_err(|_| refused())?;
    if args.next().is_some()
        || operation.to_string() != operation_text
        || endpoint.simple().to_string() != selector
        || [operation, endpoint]
            .iter()
            .any(|id| id.get_version_num() != 4 || id.get_variant() != uuid::Variant::RFC4122)
    {
        return Err(refused());
    }
    Ok((operation, selector))
}

#[cfg(windows)]
pub(crate) fn run_bootstrap(args: impl Iterator<Item = std::ffi::OsString>) -> Result<()> {
    let (operation, selector) = bootstrap_arguments(args)?;
    // Bootstrap agreement only: it exits after one ACK. No normal Controller,
    // update engine, publication, Host connection or recovery runs here.
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|_| refused())?
        .block_on(acknowledge_creator(&selector, *operation.as_bytes()))
}

/// Owns and closes this transport on success, error OR cancellation. There is
/// no reusable post-handshake command channel or second frame dispatch.
pub(crate) async fn exchange<S>(mut stream: S, expected: WorkerBindingFrame) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let request = expected
            .encode(WorkerBindingKind::Request)
            .ok_or_else(refused)?;
        stream.write_all(&request).await.map_err(|_| refused())?;
        let mut ack = [0; WORKER_BINDING_FRAME_BYTES];
        stream.read_exact(&mut ack).await.map_err(|_| refused())?;
        if WorkerBindingFrame::decode(&ack, WorkerBindingKind::Ack) != Some(expected) {
            return Err(refused());
        }
        Ok(())
    })
    .await
    .map_err(|_| refused())?
}

/// Compare the private bootstrap operation and echo the opaque context/challenge.
/// Success returns this SAME transport; error/cancellation closes it. Retaining
/// a connection after ACK is not independent authorization or lifecycle authority.
async fn acknowledge<S>(mut stream: S, operation: [u8; 16]) -> Result<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let mut bytes = [0; WORKER_BINDING_FRAME_BYTES];
        stream.read_exact(&mut bytes).await.map_err(|_| refused())?;
        let frame = WorkerBindingFrame::decode(&bytes, WorkerBindingKind::Request)
            .filter(|frame| frame.operation == operation)
            .ok_or_else(refused)?;
        stream
            .write_all(&frame.encode(WorkerBindingKind::Ack).ok_or_else(refused)?)
            .await
            .map_err(|_| refused())?;
        Ok::<(), anyhow::Error>(())
    })
    .await
    .map_err(|_| refused())??;
    Ok(stream)
}

#[cfg(windows)]
#[allow(dead_code)] // No early production worker entry/coordinator is wired.
pub(crate) async fn acknowledge_creator(selector: &str, operation: [u8; 16]) -> Result<()> {
    // The existing terminal bootstrap still closes immediately after ACK.
    acknowledge_creator_for_handoff(selector, operation)
        .await
        .map(drop)
}

#[cfg(windows)]
#[allow(dead_code)] // Internal transport continuation; no transaction engine is wired.
pub(crate) async fn acknowledge_creator_for_handoff(
    selector: &str,
    operation: [u8; 16],
) -> Result<treer_platform::windows_update_pipe::CreationParentConnection> {
    let stream = treer_platform::windows_update_pipe::connect_creation_parent_pipe(selector)
        .map_err(|_| refused())?;
    acknowledge(stream, operation).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[tokio::test]
    async fn windows_update_retained_ack_same_stream_is_kept_after_exact_frame() {
        let frame = WorkerBindingFrame {
            operation: *Uuid::new_v4().as_bytes(),
            context_sha256: [0x35; 32],
            challenge: *Uuid::new_v4().as_bytes(),
        };
        let (mut parent, child) = tokio::io::duplex(256);
        let source = async {
            parent
                .write_all(&frame.encode(WorkerBindingKind::Request).unwrap())
                .await
                .unwrap();
            let mut bytes = [0; WORKER_BINDING_FRAME_BYTES];
            parent.read_exact(&mut bytes).await.unwrap();
            assert_eq!(
                WorkerBindingFrame::decode(&bytes, WorkerBindingKind::Ack),
                Some(frame)
            );
            // Fixture-only byte verifies transport ownership, not a command.
            parent.write_all(b"x").await.unwrap();
        };
        let ((), result) = tokio::join!(source, acknowledge(child, frame.operation));
        let mut retained = result.unwrap();
        let mut byte = [0];
        retained.read_exact(&mut byte).await.unwrap();
        assert_eq!(byte, *b"x");
    }

    #[tokio::test]
    async fn windows_update_retained_ack_invalid_deadline_and_cancel_close_transport() {
        let frame = WorkerBindingFrame {
            operation: *Uuid::new_v4().as_bytes(),
            context_sha256: [0x35; 32],
            challenge: *Uuid::new_v4().as_bytes(),
        };
        for offset in [0, 8] {
            let (mut parent, child) = tokio::io::duplex(256);
            let mut bytes = frame.encode(WorkerBindingKind::Request).unwrap();
            bytes[offset] ^= 1;
            parent.write_all(&bytes).await.unwrap();
            assert!(acknowledge(child, frame.operation).await.is_err());
            assert_eq!(parent.read(&mut [0]).await.unwrap(), 0);
        }
        let (mut parent, child) = tokio::io::duplex(256);
        assert!(acknowledge(child, frame.operation).await.is_err());
        assert_eq!(parent.read(&mut [0]).await.unwrap(), 0);
        let (mut parent, child) = tokio::io::duplex(256);
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(10),
            acknowledge(child, frame.operation)
        )
        .await
        .is_err());
        assert_eq!(parent.read(&mut [0]).await.unwrap(), 0);
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "task-owned retained-ACK helper only"]
    async fn retained_ack_fixture_child() {
        let selector = std::env::var("TREER_TEST_RETAINED_SELECTOR").unwrap();
        let operation =
            Uuid::parse_str(&std::env::var("TREER_TEST_RETAINED_OPERATION").unwrap()).unwrap();
        let retained = acknowledge_creator_for_handoff(&selector, *operation.as_bytes())
            .await
            .unwrap();
        assert!(retained.creator_is_live().unwrap());
        // Creator remains live; pipe closure after ACK must not mean exit.
        let started = std::time::Instant::now();
        assert!(retained.wait_for_creator_exit().await.is_err());
        assert!(started.elapsed() >= std::time::Duration::from_secs(3));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_update_retained_ack_native_original_creator_handle_survives_ack() {
        use std::process::{Child, Command, Stdio};
        struct Owned(Child);
        impl Drop for Owned {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let selector = Uuid::new_v4().simple().to_string();
        let operation = Uuid::new_v4();
        let pipe = treer_platform::windows_update_pipe::CreatedChildPipe::bind(&selector).unwrap();
        let mut child = Owned(
            Command::new(std::env::current_exe().unwrap())
                .env_clear()
                .env("TREER_TEST_RETAINED_SELECTOR", &selector)
                .env("TREER_TEST_RETAINED_OPERATION", operation.to_string())
                .args([
                    "--exact",
                    "windows_update_handshake::tests::retained_ack_fixture_child",
                    "--ignored",
                    "--nocapture",
                ])
                .stdin(treer_platform::windows_update_pipe::creator_process_stdio().unwrap())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let server = pipe.connect(&child.0).await.unwrap();
        exchange(
            server,
            WorkerBindingFrame {
                operation: *operation.as_bytes(),
                context_sha256: [0x35; 32],
                challenge: *Uuid::new_v4().as_bytes(),
            },
        )
        .await
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn windows_update_bootstrap_private_arguments_are_exact_and_nonambient() {
        let operation = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_string();
        let selector = "bbbbbbbbbbbb4bbb8bbbbbbbbbbbbbbb".to_string();
        let parse = |values: Vec<String>| bootstrap_arguments(values.into_iter().map(Into::into));
        assert_eq!(
            parse(vec![operation.clone(), selector.clone()]).unwrap(),
            (Uuid::parse_str(&operation).unwrap(), selector.clone())
        );
        for values in [
            vec![],
            vec![operation.clone()],
            vec![operation.clone(), selector.clone(), "extra".into()],
            vec![Uuid::nil().to_string(), selector.clone()],
            vec![operation.to_uppercase(), selector.clone()],
            vec![operation.clone(), Uuid::nil().simple().to_string()],
            vec![operation.clone(), "../pipe".into()],
            vec![operation, selector.to_uppercase()],
        ] {
            assert!(parse(values).is_err());
        }
    }

    #[tokio::test]
    async fn windows_update_handshake_bounded_frames_match_and_refuse_corruption() {
        let frame = WorkerBindingFrame {
            operation: *Uuid::new_v4().as_bytes(),
            context_sha256: [0x52; 32],
            challenge: *Uuid::new_v4().as_bytes(),
        };
        let (parent, child) = tokio::io::duplex(256);
        let (a, b) = tokio::join!(exchange(parent, frame), acknowledge(child, frame.operation));
        a.unwrap();
        b.unwrap();
        for offset in [0, 8, 24, 56] {
            let (parent, mut child) = tokio::io::duplex(256);
            let response = async {
                let mut request = [0; WORKER_BINDING_FRAME_BYTES];
                child.read_exact(&mut request).await.unwrap();
                let mut ack = frame.encode(WorkerBindingKind::Ack).unwrap();
                ack[offset] ^= 1;
                child.write_all(&ack).await.unwrap();
            };
            let (result, ()) = tokio::join!(exchange(parent, frame), response);
            assert_eq!(
                result.unwrap_err().to_string(),
                "windows_update_handshake_unconfirmed"
            );
        }
        let (parent, child) = tokio::io::duplex(256);
        let (a, b) = tokio::join!(
            exchange(parent, frame),
            acknowledge(child, *Uuid::new_v4().as_bytes())
        );
        assert!(a.is_err());
        assert!(b.is_err());
        // Keep an unresponsive peer alive: the helper's own deadline must fire.
        let (parent, _silent) = tokio::io::duplex(256);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), exchange(parent, frame))
                .await
                .unwrap()
                .is_err()
        );
    }
}

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

/// Compare the private bootstrap operation, echo the creator's opaque context
/// and fresh challenge. Echo is not independent runtime authorization. The
/// connection is consumed; this helper cannot return lifecycle authority.
async fn acknowledge<S>(mut stream: S, operation: [u8; 16]) -> Result<()>
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
        Ok(())
    })
    .await
    .map_err(|_| refused())?
}

#[cfg(windows)]
#[allow(dead_code)] // No early production worker entry/coordinator is wired.
pub(crate) async fn acknowledge_creator(selector: &str, operation: [u8; 16]) -> Result<()> {
    let stream = treer_platform::windows_update_pipe::connect_creation_parent_pipe(selector)
        .map_err(|_| refused())?;
    acknowledge(stream, operation).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

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

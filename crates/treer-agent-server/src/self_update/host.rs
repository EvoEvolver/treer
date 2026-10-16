//! Read-only live Host observation for a transaction. PID/UID come from the same
//! Unix socket used for Sync; no PID file, argv/environment, or PATH lookup.
use anyhow::{bail, Result};
use treer_platform::process_image::ProcessImage;

use super::{
    images::digest_file,
    safety::{HostIdentity, HostSnapshot},
    selection::Selected,
};
use crate::host_client::{HostClient, HostEvents};

pub struct Observation {
    client: HostClient,
    events: HostEvents,
    image: ProcessImage,
    snapshot: HostSnapshot,
}

fn identity(image: &ProcessImage) -> Result<HostIdentity> {
    let digest = digest_file(image.executable())
        .map_err(|_| anyhow::anyhow!("update_host_image_unavailable"))?;
    image
        .verify_current()
        .map_err(|_| anyhow::anyhow!("update_host_identity_changed"))?;
    Ok(HostIdentity {
        pid: image.pid(),
        creation_identity: image.creation_identity().to_owned(),
        executable_sha256: digest,
    })
}

impl Observation {
    pub async fn capture(selected: &Selected) -> Result<Self> {
        Self::at(
            selected.host_socket(),
            selected.workspace(),
            selected.server_id(),
        )
        .await
    }

    async fn at(socket: &std::path::Path, workspace: &str, server: &str) -> Result<Self> {
        let (client, events, image) = HostClient::connect_for_update(socket).await?;
        let before = identity(&image)?;
        let response = client
            .sync(Default::default())
            .await
            .map_err(|_| anyhow::anyhow!("update_host_sync_failed"))?;
        image
            .verify_current()
            .map_err(|_| anyhow::anyhow!("update_host_identity_changed"))?;
        let snapshot = HostSnapshot::capture(&response, before, workspace, server)?;
        Ok(Self {
            client,
            events,
            image,
            snapshot,
        })
    }

    /// The held socket must still belong to the same live Host. Hash the
    /// OS-reported image again and compare workload generations/PIDs/output
    /// witnesses. Never reconnect to a replacement Host and bless it as unchanged.
    pub async fn verify(&self) -> Result<()> {
        if *self.events.disconnected.borrow() {
            bail!("update_host_disconnected");
        }
        let current = identity(&self.image)?;
        let response = self
            .client
            .sync(self.snapshot.cursors())
            .await
            .map_err(|_| anyhow::anyhow!("update_host_sync_failed"))?;
        self.image
            .verify_current()
            .map_err(|_| anyhow::anyhow!("update_host_identity_changed"))?;
        self.snapshot.verify(&response, &current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::UnixListener,
    };
    use treer_host_protocol::{
        decode_message, encode_message, HostBuildInfo, HostCommand, HostMessage, HostRequest,
        HostResponse, HOST_PROTOCOL_VERSION, MAX_HOST_FRAME_BYTES,
    };

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("tu-{}", uuid::Uuid::new_v4().simple()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&root)
                .unwrap();
            Self(root)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[tokio::test]
    async fn same_socket_peer_image_and_real_framed_sync_are_rechecked_without_mutations() {
        let f = Fixture::new();
        let socket = f.0.join("h");
        let listener = UnixListener::bind(&socket).unwrap();
        let (release, held) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            for index in 0..3 {
                let length = stream.read_u32().await.unwrap() as usize;
                assert!(length <= MAX_HOST_FRAME_BYTES);
                let mut bytes = vec![0; length];
                stream.read_exact(&mut bytes).await.unwrap();
                let request: HostRequest = decode_message(&bytes).unwrap();
                assert_eq!(request.protocol, HOST_PROTOCOL_VERSION);
                assert!(matches!(request.command, HostCommand::Sync { .. }));
                assert!(request.operation_id.is_none());
                let response = HostResponse::Synced {
                    host_epoch: if index < 2 {
                        "same-host"
                    } else {
                        "replacement-host"
                    }
                    .into(),
                    host_build: HostBuildInfo {
                        version: "0.1.14".into(),
                        git_commit: "a".repeat(40),
                    },
                    processes: vec![],
                    replay: vec![],
                };
                let frame = encode_message(&HostMessage::Response {
                    request_id: request.request_id,
                    response: Some(response),
                    error: None,
                })
                .unwrap();
                stream.write_u32(frame.len() as u32).await.unwrap();
                stream.write_all(&frame).await.unwrap();
            }
            let _ = held.await;
        });
        let observation = Observation::at(&socket, "ws_fixture", "srv_fixture")
            .await
            .unwrap();
        assert_eq!(observation.image.pid(), std::process::id());
        observation.verify().await.unwrap();
        assert!(observation.verify().await.is_err());
        release.send(()).unwrap();
        server.await.unwrap();
        drop(observation);
        assert!(Observation::at(&socket, "ws_fixture", "srv_fixture")
            .await
            .is_err());
    }
}

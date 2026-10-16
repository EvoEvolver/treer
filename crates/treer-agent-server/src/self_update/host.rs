//! Read-only live Host observation for a transaction. PID/UID come from the same
//! Unix socket used for Sync; no PID file, argv/environment, or PATH lookup.
use anyhow::{bail, Result};
use treer_platform::process_image::ProcessImage;

use super::{
    artifacts::Binary,
    hex,
    images::digest_file,
    safety::{HostIdentity, HostSnapshot},
    selection::{Selected, SelectionLocks},
};
use crate::host_client::{HostClient, HostEvents};

pub struct Observation {
    client: HostClient,
    events: HostEvents,
    image: ProcessImage,
    snapshot: HostSnapshot,
    workspace: String,
    server: String,
    socket: std::path::PathBuf,
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
            workspace: workspace.to_owned(),
            server: server.to_owned(),
            socket: socket.to_path_buf(),
        })
    }

    /// Find the selected installed Controller inode among this exact Host's
    /// direct OS children. Neither a healthy HTTP responder nor an installed
    /// filename/digest alone proves that the successor is actually running.
    pub fn controller_image(
        &self,
        selected: &Selected,
        locks: &SelectionLocks,
        expected_digest: &str,
    ) -> Result<ProcessImage> {
        if selected.workspace() != self.workspace
            || selected.server_id() != self.server
            || selected.host_socket() != self.socket
            || !hex(expected_digest, 64)
        {
            bail!("update_controller_identity_binding_mismatch");
        }
        if *self.events.disconnected.borrow() {
            bail!("update_host_disconnected");
        }
        let file = locks
            .directory(selected, Binary::Controller)?
            .read_file(Binary::Controller.name())
            .map_err(|_| anyhow::anyhow!("unsafe_update_controller_image"))?;
        if digest_file(&file)? != expected_digest {
            bail!("update_controller_image_mismatch");
        }
        let mut found = None;
        for image in self
            .image
            .children()
            .map_err(|_| anyhow::anyhow!("update_host_children_unavailable"))?
        {
            if !image
                .matches_file(&file)
                .map_err(|_| anyhow::anyhow!("update_controller_identity_unavailable"))?
            {
                continue;
            }
            if found.is_some() {
                bail!("update_multiple_running_controllers");
            }
            if digest_file(image.executable())? != expected_digest {
                bail!("update_controller_image_mismatch");
            }
            image
                .verify_current()
                .map_err(|_| anyhow::anyhow!("update_controller_identity_changed"))?;
            found = Some(image);
        }
        found.ok_or_else(|| anyhow::anyhow!("update_running_controller_unproven"))
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

    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    #[ignore = "test-owned subprocess, invoked only by the image identity fixture"]
    fn image_child() {
        assert_eq!(
            std::env::var("TREER_TEST_UPDATE_IMAGE_CHILD")
                .ok()
                .as_deref(),
            Some("1")
        );
        // Lifetime follows the owning test's pipe, not a timing-sensitive sleep.
        // EOF also exits if the parent disappears before its Child guard runs.
        let mut bytes = [0_u8; 1];
        let _ = std::io::Read::read(&mut std::io::stdin(), &mut bytes);
    }

    #[tokio::test]
    async fn healthy_response_requires_the_selected_inode_running_as_a_host_child() {
        use super::super::{health::Monitor, safety::ControllerExpectation, selection::Inventory};
        use std::{
            io::Write,
            os::unix::fs::PermissionsExt,
            process::{Command, Stdio},
        };
        use treer_platform::private_directory::PrivateDirectory;

        let f = Fixture::new();
        let root = PrivateDirectory::open(&f.0).unwrap();
        let config = root.create_child("config").unwrap();
        let bin = root.create_child("bin").unwrap();
        let controller = f.0.join("bin/treer-agent-server");
        // Synthetic runtime image only, never a real enrolled Controller.
        std::fs::copy(std::env::current_exe().unwrap(), &controller).unwrap();
        std::fs::set_permissions(&controller, std::fs::Permissions::from_mode(0o700)).unwrap();
        bin.create_file("treer")
            .unwrap()
            .write_all(b"unused-test-cli")
            .unwrap();
        let mut child = Child(
            Command::new(&controller)
                .args([
                    "--exact",
                    "self_update::host::tests::image_child",
                    "--ignored",
                ])
                .env_clear()
                .env("TREER_TEST_UPDATE_IMAGE_CHILD", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "synthetic child exited before observation"
        );
        let live_image =
            ProcessImage::capture(child.0.id()).expect("capture synthetic child image");
        assert!(
            live_image
                .matches_file(&std::fs::File::open(&controller).unwrap())
                .unwrap(),
            "synthetic child uses another inode"
        );
        let socket = f.0.join("h");
        let listener = UnixListener::bind(&socket).unwrap();
        let http = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        for (name, value) in [
            (
                "srv_fixture-controller.json",
                serde_json::json!({"proxy":"https://example.invalid/", "workspace":"ws_fixture", "server_id":"srv_fixture", "root":f.0, "listen":http.local_addr().unwrap().to_string(), "host_socket":socket, "install_hostname":"fixture"}),
            ),
            (
                "srv_fixture-host.json",
                serde_json::json!({"socket_path":socket, "controller_path":controller, "controller_config_path":f.0.join("config/srv_fixture-controller.json"), "root":f.0}),
            ),
        ] {
            config
                .create_file(name)
                .unwrap()
                .write_all(&serde_json::to_vec(&value).unwrap())
                .unwrap();
        }
        let selected = Inventory::load(&f.0.join("config"), "fixture")
            .unwrap()
            .select(None, false)
            .unwrap();
        let locks = selected.lock().unwrap();
        let (release, held) = tokio::sync::oneshot::channel::<()>();
        let host_server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            for _ in 0..2 {
                let length = stream.read_u32().await.unwrap() as usize;
                assert!(length <= MAX_HOST_FRAME_BYTES);
                let mut bytes = vec![0; length];
                stream.read_exact(&mut bytes).await.unwrap();
                let request: HostRequest = decode_message(&bytes).unwrap();
                assert!(matches!(request.command, HostCommand::Sync { .. }));
                let frame = encode_message(&HostMessage::Response {
                    request_id: request.request_id,
                    response: Some(HostResponse::Synced {
                        host_epoch: "host".into(),
                        host_build: HostBuildInfo {
                            version: "0.1.14".into(),
                            git_commit: "a".repeat(40),
                        },
                        processes: vec![],
                        replay: vec![],
                    }),
                    error: None,
                })
                .unwrap();
                stream.write_u32(frame.len() as u32).await.unwrap();
                stream.write_all(&frame).await.unwrap();
            }
            let _ = held.await;
        });
        let http_server = tokio::spawn(async move {
            let (mut stream, _) = http.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(stream.read_u8().await.unwrap());
                assert!(request.len() < 8192);
            }
            let body = serde_json::json!({"service":"treer-agent-server", "workspace_id":"ws_fixture", "server_id":"srv_fixture", "controller_epoch":"new", "controller_build":{"version":"0.1.14", "git_commit":"b".repeat(40)}, "proxy_connected":true}).to_string();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let observation = Observation::capture(&selected).await.unwrap();
        assert_eq!(observation.image.pid(), std::process::id());
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "synthetic child exited during capture"
        );
        assert!(
            observation
                .image
                .children()
                .unwrap()
                .iter()
                .any(|image| image.pid() == child.0.id()),
            "synthetic child missing from direct children"
        );
        let digest = digest_file(&std::fs::File::open(&controller).unwrap()).unwrap();
        assert_eq!(
            observation
                .controller_image(&selected, &locks, &digest)
                .unwrap()
                .pid(),
            child.0.id()
        );
        let expected = ControllerExpectation {
            workspace: "ws_fixture".into(),
            server: "srv_fixture".into(),
            previous_epoch: "old".into(),
            artifact_source_sha: "b".repeat(40),
            executable_sha256: digest.clone(),
        };
        assert_eq!(
            Monitor::new()
                .unwrap()
                .observe_live(&selected, &locks, &expected, &observation)
                .await
                .unwrap(),
            "new"
        );
        http_server.await.unwrap();
        // Same bytes + a new installed inode must not attest the old running
        // process as the successor, even if HTTP and source SHA would match.
        std::fs::copy(&controller, f.0.join("bin/next")).unwrap();
        std::fs::rename(f.0.join("bin/next"), &controller).unwrap();
        assert_eq!(
            observation
                .controller_image(&selected, &locks, &digest)
                .err()
                .unwrap()
                .to_string(),
            "update_running_controller_unproven"
        );
        release.send(()).unwrap();
        host_server.await.unwrap();
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

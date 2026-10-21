//! Read-only live Host observation for a transaction. PID/UID come from the same
//! Unix socket used for Sync; no PID file, argv/environment, or PATH lookup.
use anyhow::{bail, Result};
use std::io::Write;
use treer_host_protocol::{HostCommand, HostResponse};
use treer_platform::process_image::ProcessImage;

use super::{
    artifacts::Binary,
    diagnostics, hex,
    images::digest_file,
    publication::FileStep,
    safety::{HostIdentity, HostSnapshot},
    selection::{Selected, SelectionLocks},
    Journal, Phase,
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
    /// Require a typed old-Controller observation, not a caller-supplied epoch.
    pub async fn save_initial(
        &self,
        selected: &Selected,
        locks: &SelectionLocks,
        journal: &Journal,
        revision: u64,
        initial: &super::health::InitialController,
    ) -> Result<()> {
        let receipt = journal.read()?;
        if receipt.workspace_id != self.workspace
            || receipt.server_id != self.server
            || selected.workspace() != self.workspace
            || selected.server_id() != self.server
            || selected.host_socket() != self.socket
        {
            bail!("update_baseline_binding_mismatch");
        }
        for binary in [Binary::Controller, Binary::Cli] {
            locks.directory(selected, binary)?;
        }
        initial.validate(&receipt)?;
        self.verify().await?;
        let current = journal.read()?;
        let previous_controller_epoch = initial.validate(&current)?;
        self.snapshot.persist(
            journal,
            revision,
            previous_controller_epoch,
            initial.source_sha(),
        )
    }

    /// Reconnect only to prove the SAME saved pre-update Host and inventory.
    /// Never call capture() here: doing so would bless changes made while the
    /// activator was dead as the new baseline. A missing/invalid file is fatal.
    pub async fn recover(
        selected: &Selected,
        locks: &SelectionLocks,
        journal: &Journal,
    ) -> Result<(Self, String)> {
        let receipt = journal.read()?;
        if receipt.workspace_id != selected.workspace() || receipt.server_id != selected.server_id()
        {
            bail!("update_baseline_binding_mismatch");
        }
        for binary in [Binary::Controller, Binary::Cli] {
            locks.directory(selected, binary)?;
        }
        let (snapshot, previous_epoch, _) = HostSnapshot::load(journal)?;
        let (client, events, image) =
            HostClient::connect_for_update(selected.host_socket()).await?;
        let current = identity(&image)?;
        let response = client
            .sync(snapshot.cursors())
            .await
            .map_err(|_| anyhow::anyhow!("update_host_sync_failed"))?;
        image
            .verify_current()
            .map_err(|_| anyhow::anyhow!("update_host_identity_changed"))?;
        snapshot.verify(&response, &current)?;
        if *events.disconnected.borrow() {
            bail!("update_host_disconnected");
        }
        Ok((
            Self {
                client,
                events,
                image,
                snapshot,
                workspace: selected.workspace().to_owned(),
                server: selected.server_id().to_owned(),
                socket: selected.host_socket().to_path_buf(),
            },
            previous_epoch,
        ))
    }

    /// The detached activation coordinator must own both locks before calling.
    /// Durable intent precedes the one request. ACK means accepted, never healthy.
    /// A lost response or crash leaves intent behind: observe/reconcile that job,
    /// do not automatically send another restart (even with the same operation ID).
    pub async fn request_restart_once(
        &self,
        selected: &Selected,
        locks: &SelectionLocks,
        journal: &Journal,
        revision: u64,
    ) -> Result<()> {
        let receipt = journal.read()?;
        if receipt.revision != revision
            || receipt.workspace_id != self.workspace
            || receipt.server_id != self.server
            || selected.workspace() != self.workspace
            || selected.server_id() != self.server
            || selected.host_socket() != self.socket
        {
            bail!("update_restart_binding_conflict");
        }
        let (marker, suffix, restoring) = match receipt.phase {
            Phase::Activating => ("controller-restart-intent.json", "activate", false),
            Phase::RollingBack => ("controller-rollback-intent.json", "rollback", true),
            _ => bail!("update_restart_phase_conflict"),
        };
        match journal.directory.read_file(marker) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => bail!("update_restart_intent_exists_or_unavailable"),
        }
        for binary in [Binary::Controller, Binary::Cli] {
            let step = receipt.publications.step(binary);
            if !matches!(step, FileStep::Unchanged)
                && step
                    != if restoring {
                        FileStep::Restored
                    } else {
                        FileStep::Installed
                    }
            {
                bail!("update_restart_publication_incomplete");
            }
            let expected = if restoring {
                receipt.publications.original_digest(binary)
            } else {
                match binary {
                    Binary::Controller => &receipt.controller_sha256,
                    Binary::Cli => &receipt.cli_sha256,
                }
            };
            let file = locks
                .directory(selected, binary)?
                .read_file(binary.name())
                .map_err(|_| anyhow::anyhow!("update_restart_image_unavailable"))?;
            if digest_file(&file)? != expected {
                bail!("update_restart_image_mismatch");
            }
        }
        let (original, _, _) = HostSnapshot::load(journal)?;
        if original != self.snapshot {
            bail!("update_original_baseline_mismatch");
        }
        self.verify().await?;
        let operation = format!("update-{}-{suffix}", receipt.job_id);
        // EXCL also refuses an empty/partial previous intent. Neither recovery
        // nor a later caller may remove this marker to blindly retry the request.
        let mut intent = journal
            .directory
            .create_file(marker)
            .map_err(|_| anyhow::anyhow!("update_restart_intent_exists_or_unavailable"))?;
        let bytes = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "job_id": receipt.job_id,
            "revision": revision,
            "workspace_id": receipt.workspace_id,
            "server_id": receipt.server_id,
            "operation_id": operation,
        }))?;
        intent.write_all(&bytes)?;
        intent.sync_all()?;
        drop(intent);
        journal.directory.sync()?;
        match self
            .client
            .request(HostCommand::RestartController, Some(operation))
            .await
        {
            Ok(HostResponse::Ack) => Ok(()),
            // Even a protocol/error response is conservatively ambiguous; a
            // reply cannot prove that a restart was never scheduled.
            _ => bail!("update_restart_outcome_unknown"),
        }
    }

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
        if *events.disconnected.borrow() {
            bail!("update_host_disconnected");
        }
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
                .map_err(|_| anyhow::anyhow!(diagnostics::CONTROLLER_IDENTITY_UNAVAILABLE))?
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
                .map_err(|_| anyhow::anyhow!(diagnostics::CONTROLLER_IDENTITY_CHANGED))?;
            found = Some(image);
        }
        found.ok_or_else(|| anyhow::anyhow!(diagnostics::RUNNING_CONTROLLER_UNPROVEN))
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
        activation_fixture(true, false).await;
    }

    #[tokio::test]
    async fn lost_restart_response_leaves_durable_intent_and_is_never_resent() {
        activation_fixture(false, false).await;
    }

    #[tokio::test]
    async fn rollback_restart_has_a_separate_durable_operation_and_intent() {
        activation_fixture(true, true).await;
    }

    async fn activation_fixture(acknowledge: bool, restoring: bool) {
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
            let mut held_streams = Vec::new();
            for connection in 0..3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                for index in 0..if connection == 0 { 6 } else { 1 } {
                    let length = stream.read_u32().await.unwrap() as usize;
                    assert!(length <= MAX_HOST_FRAME_BYTES);
                    let mut bytes = vec![0; length];
                    stream.read_exact(&mut bytes).await.unwrap();
                    let request: HostRequest = decode_message(&bytes).unwrap();
                    let response = if connection != 0 || index < 5 {
                        assert!(matches!(request.command, HostCommand::Sync { .. }));
                        assert!(request.operation_id.is_none());
                        HostResponse::Synced {
                            host_epoch: if connection == 2 {
                                "changed-host"
                            } else {
                                "host"
                            }
                            .into(),
                            host_build: HostBuildInfo {
                                version: "0.1.14".into(),
                                git_commit: "a".repeat(40),
                            },
                            processes: vec![],
                            replay: vec![],
                        }
                    } else {
                        assert!(matches!(request.command, HostCommand::RestartController));
                        let operation = request.operation_id.unwrap();
                        let id = operation
                            .strip_prefix("update-")
                            .unwrap()
                            .strip_suffix(if restoring { "-rollback" } else { "-activate" })
                            .unwrap();
                        assert!(!uuid::Uuid::parse_str(id).unwrap().is_nil());
                        if !acknowledge {
                            // The Host received the mutation but the reply is lost.
                            // Close this synthetic socket, not a real Host process.
                            return;
                        }
                        HostResponse::Ack
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
                held_streams.push(stream);
            }
            let _ = held.await;
        });
        let http_server = tokio::spawn(async move {
            for epoch in ["new", "observed-old"] {
                let (mut stream, _) = http.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(stream.read_u8().await.unwrap());
                    assert!(request.len() < 8192);
                }
                let body = serde_json::json!({"service":"treer-agent-server", "workspace_id":"ws_fixture", "server_id":"srv_fixture", "controller_epoch":epoch, "controller_build":{"version":"0.1.14", "git_commit":"b".repeat(40)}, "proxy_connected":true}).to_string();
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
            }
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
        let cli_digest = digest_file(&bin.read_file("treer").unwrap()).unwrap();
        let now = chrono::Utc::now();
        let receipt = super::super::Receipt {
            schema_version: 1,
            job_id: uuid::Uuid::new_v4(),
            revision: 0,
            workspace_id: selected.workspace().into(),
            server_id: selected.server_id().into(),
            release_sha: "c".repeat(40),
            artifact_source_sha: "b".repeat(40),
            controller_sha256: digest.clone(),
            cli_sha256: cli_digest.clone(),
            publications: super::super::publication::Publications::new(
                digest.clone(),
                cli_digest.clone(),
                &digest,
                &cli_digest,
            ),
            source: super::super::Source::Pin {
                sums_sha256: "d".repeat(64),
                allow_pinned: true,
                allow_artifact_origin: false,
                allow_same: true,
                allow_downgrade: false,
            },
            phase: Phase::Preparing,
            created_at: now,
            updated_at: now,
        };
        let jobs = root.create_child("jobs").unwrap();
        let mut incomplete = receipt.clone();
        incomplete.job_id = uuid::Uuid::new_v4();
        incomplete.publications = super::super::publication::Publications::new(
            "0".repeat(64),
            "1".repeat(64),
            &digest,
            &cli_digest,
        );
        let incomplete = Journal::create(&jobs, &incomplete).unwrap();
        incomplete.prepare_fixture().unwrap();
        incomplete.advance(1, Phase::Activating).unwrap();
        assert_eq!(
            observation
                .request_restart_once(&selected, &locks, &incomplete, 2)
                .await
                .unwrap_err()
                .to_string(),
            "update_restart_publication_incomplete"
        );
        let journal = Journal::create(&jobs, &receipt).unwrap();
        let mut missing_receipt = receipt.clone();
        missing_receipt.job_id = uuid::Uuid::new_v4();
        let missing = Journal::create(&jobs, &missing_receipt).unwrap();
        missing.prepare_fixture().unwrap();
        missing.advance(1, Phase::Activating).unwrap();
        assert_eq!(
            observation
                .request_restart_once(&selected, &locks, &missing, 2)
                .await
                .unwrap_err()
                .to_string(),
            "update_baseline_missing_or_unsafe"
        );
        let initial = Monitor::new()
            .unwrap()
            .observe_initial(&selected, &locks, &journal, &observation)
            .await
            .unwrap();
        let mut other_job = receipt.clone();
        other_job.job_id = uuid::Uuid::new_v4();
        assert!(initial.validate(&other_job).is_err());
        let mut other_image = receipt.clone();
        other_image.publications = super::super::publication::Publications::new(
            "0".repeat(64),
            cli_digest.clone(),
            &digest,
            &cli_digest,
        );
        assert!(initial.validate(&other_image).is_err());
        assert!(initial.validate(&missing.read().unwrap()).is_err());
        observation
            .save_initial(&selected, &locks, &journal, 0, &initial)
            .await
            .unwrap();
        http_server.await.unwrap();
        journal.prepare_fixture().unwrap();
        assert!(Monitor::new()
            .unwrap()
            .observe_initial(&selected, &locks, &journal, &observation)
            .await
            .is_err());
        assert_eq!(
            observation
                .request_restart_once(&selected, &locks, &journal, 1)
                .await
                .unwrap_err()
                .to_string(),
            "update_restart_phase_conflict"
        );
        journal.advance(1, Phase::Activating).unwrap();
        assert_eq!(
            observation
                .request_restart_once(&selected, &locks, &journal, 1)
                .await
                .unwrap_err()
                .to_string(),
            "update_restart_binding_conflict"
        );
        let revision = if restoring {
            journal.advance(2, Phase::RollingBack).unwrap().revision
        } else {
            2
        };
        std::fs::write(f.0.join("bin/treer"), b"changed-test-cli").unwrap();
        assert_eq!(
            observation
                .request_restart_once(&selected, &locks, &journal, revision)
                .await
                .unwrap_err()
                .to_string(),
            "update_restart_image_mismatch"
        );
        std::fs::write(f.0.join("bin/treer"), b"unused-test-cli").unwrap();
        let result = observation
            .request_restart_once(&selected, &locks, &journal, revision)
            .await;
        if acknowledge {
            result.unwrap();
        } else {
            assert_eq!(
                result.unwrap_err().to_string(),
                "update_restart_outcome_unknown"
            );
        }
        // Reopening a job after acceptance must not request a second restart.
        let reopened = Journal::open(&jobs, &receipt.job_id.to_string()).unwrap();
        assert_eq!(
            observation
                .request_restart_once(&selected, &locks, &reopened, revision)
                .await
                .unwrap_err()
                .to_string(),
            "update_restart_intent_exists_or_unavailable"
        );
        if !acknowledge {
            host_server.await.unwrap();
            return;
        }
        let (recovered, previous_epoch) = Observation::recover(&selected, &locks, &reopened)
            .await
            .unwrap();
        assert_eq!(previous_epoch, "observed-old");
        let (_, _, previous_source_sha) = HostSnapshot::load(&reopened).unwrap();
        assert_eq!(previous_source_sha, "b".repeat(40));
        assert_eq!(recovered.image.pid(), observation.image.pid());
        assert_eq!(
            Observation::recover(&selected, &locks, &reopened)
                .await
                .err()
                .unwrap()
                .to_string(),
            "update_host_changed"
        );
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

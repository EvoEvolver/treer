use super::*;
use crate::release_update::tests::{fixture, server, signing_key, BINARY, PLATFORM, VERSION};
use ed25519_dalek::SigningKey;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use treer_host_protocol::{decode_message, encode_message, HostMessage, HostRequest, HostResponse};

/// A Host that acknowledges every request and records its commands.
fn fake_host() -> (HostClient, Arc<std::sync::Mutex<Vec<HostCommand>>>) {
    let (client, mut host) = tokio::io::duplex(64 * 1024);
    let commands = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = commands.clone();
    tokio::spawn(async move {
        while let Ok(length) = host.read_u32().await {
            let mut frame = vec![0; length as usize];
            host.read_exact(&mut frame).await.unwrap();
            let request: HostRequest = decode_message(&frame).unwrap();
            seen.lock().unwrap().push(request.command);
            let reply = encode_message(&HostMessage::Response {
                request_id: request.request_id,
                response: Some(HostResponse::Ack),
                error: None,
            })
            .unwrap();
            host.write_u32(reply.len() as u32).await.unwrap();
            host.write_all(&reply).await.unwrap();
        }
    });
    (HostClient::connected(Box::new(client)).0, commands)
}

struct Installation(PathBuf);

impl Installation {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("treer-update-{}", Uuid::new_v4().simple()));
        fs::create_dir(&root).unwrap();
        for name in ["treer-agent-server", "treer", "treer-agent-host"] {
            fs::write(root.join(name), format!("old {name}")).unwrap();
        }
        Self(root)
    }

    fn binaries(&self) -> InstalledBinaries {
        InstalledBinaries {
            controller: self.0.join("treer-agent-server"),
            cli: Some(self.0.join("treer")),
            host: Some(self.0.join("treer-agent-host")),
            launcher: Some(self.0.join("treer-launch")),
        }
    }

    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for Installation {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn job(base: Url, key: VerifyingKey, installation: &Installation) -> Job {
    Job {
        version: VERSION.into(),
        base,
        key,
        platform: PLATFORM,
        binaries: installation.binaries(),
    }
}

#[tokio::test]
async fn update_downloads_verifies_replaces_and_requests_restart() {
    let installation = Installation::new();
    let (base, _, task) = server(fixture()).await;
    let (host, commands) = fake_host();
    run(
        job(base, signing_key().verifying_key(), &installation),
        &host,
    )
    .await
    .unwrap();
    task.abort();

    for name in ["treer-agent-server", "treer", "treer-agent-host"] {
        assert_eq!(fs::read(installation.0.join(name)).unwrap(), BINARY);
        assert_eq!(
            fs::read_to_string(installation.0.join(format!("{name}.old"))).unwrap(),
            format!("old {name}")
        );
    }
    // The launcher is installed even where it was absent, with the
    // deprecated alias for profiles saved before the rename.
    assert_eq!(fs::read(installation.0.join("treer-launch")).unwrap(), BINARY);
    let alias = format!("treer-agent{}", std::env::consts::EXE_SUFFIX);
    // Exactly one previous version is kept and nothing is left staged.
    assert_eq!(
        installation.entries(),
        [
            "treer",
            alias.as_str(),
            "treer-agent-host",
            "treer-agent-host.old",
            "treer-agent-server",
            "treer-agent-server.old",
            "treer-launch",
            "treer.old"
        ]
    );
    #[cfg(unix)]
    assert_eq!(
        fs::read_link(installation.0.join("treer-agent")).unwrap(),
        Path::new("treer-launch")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(installation.0.join("treer-agent-server"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o100, 0o100);
    }
    assert_eq!(*commands.lock().unwrap(), [HostCommand::RestartController]);
    assert_eq!(status().state, UpdateState::Restarting);

    // Manual rollback is a rename of `.old` back into place.
    restore_previous(&installation.0.join("treer-agent-server")).unwrap();
    assert_eq!(
        fs::read_to_string(installation.0.join("treer-agent-server")).unwrap(),
        "old treer-agent-server"
    );
}

#[tokio::test]
async fn update_with_untrusted_signature_changes_nothing() {
    let installation = Installation::new();
    let before = installation.entries();
    let (base, requests, task) = server(fixture()).await;
    let (host, commands) = fake_host();
    let untrusted = SigningKey::from_bytes(&[9; 32]).verifying_key();
    let error = run(job(base, untrusted, &installation), &host)
        .await
        .unwrap_err();
    task.abort();
    assert!(format!("{error:#}").contains("signature"), "{error:#}");
    assert_eq!(installation.entries(), before);
    assert_eq!(
        fs::read_to_string(installation.0.join("treer-agent-server")).unwrap(),
        "old treer-agent-server"
    );
    assert!(commands.lock().unwrap().is_empty());
    assert!(requests
        .lock()
        .unwrap()
        .iter()
        .all(|path| path.ends_with("manifest.json") || path.ends_with(".sig")));
}

#[test]
fn unchanged_binaries_are_not_replaced() {
    let installation = Installation::new();
    let current = |name: &str| fs::read(installation.0.join(name)).unwrap();
    let release = VerifiedRelease {
        controller: current("treer-agent-server"),
        cli: b"new cli".to_vec(),
        host: None,
        launcher: None,
    };
    let replaced = installation.binaries().install(&release).unwrap();
    assert_eq!(replaced, [installation.0.join("treer")]);
    assert_eq!(current("treer"), b"new cli");
    assert!(!installation.0.join("treer-agent-server.old").exists());
    assert!(!installation.0.join("treer-agent-host.old").exists());
}

#[tokio::test]
async fn start_rejects_a_malformed_version_without_starting() {
    let (host, commands) = fake_host();
    for version in ["latest", "0.1.2", "v1.2.3/../../x"] {
        let error = start(version.into(), host.clone()).unwrap_err();
        assert_eq!(error.code, "controller_update_invalid_version");
    }
    assert!(commands.lock().unwrap().is_empty());
}

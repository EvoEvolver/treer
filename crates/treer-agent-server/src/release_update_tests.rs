use super::*;
use axum::{
    body::Body,
    http::{Request, Response},
    routing::any,
    Router,
};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

pub(crate) const VERSION: &str = "v0.1.13";
pub(crate) const PLATFORM: &str = "darwin-aarch64";
const CHANNEL: &str = "channels/stable.json";
const MANIFEST: &str = "releases/v0.1.13/manifest.json";
const CONTROLLER: &str = "releases/v0.1.13/darwin-aarch64/treer-agent-server";
const CLI: &str = "releases/v0.1.13/darwin-aarch64/treer";
const HOST: &str = "releases/v0.1.13/darwin-aarch64/treer-agent-host";
const LAUNCHER: &str = "releases/v0.1.13/darwin-aarch64/treer-launch";
pub(crate) const BINARY: &[u8] = b"a verified binary";

pub(crate) fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

fn signature(bytes: &[u8]) -> Vec<u8> {
    let key = signing_key();
    serde_json::to_vec(&json!({
        "schema_version": 1, "algorithm": "ed25519",
        "key_id": key_id(&key.verifying_key()).unwrap(),
        "signature": STANDARD.encode(key.sign(bytes).to_bytes()),
    }))
    .unwrap()
}

pub(crate) type Objects = HashMap<String, Vec<u8>>;
fn put_signed(objects: &mut Objects, path: &str, bytes: Vec<u8>) {
    objects.insert(format!("{path}.sig"), signature(&bytes));
    objects.insert(path.into(), bytes);
}

pub(crate) fn fixture() -> Objects {
    let mut objects = Objects::new();
    let manifest = json!({
        "schema_version": 2, "version": VERSION,
        "signing_key_id": key_id(&signing_key().verifying_key()).unwrap(),
        "git_commit": "test-commit", "builds": [],
        "artifacts": [
            { "binary": "treer-agent-server", "platform": PLATFORM, "path": CONTROLLER, "size": BINARY.len(), "sha256": digest(BINARY) },
            { "binary": "treer", "platform": PLATFORM, "path": CLI, "size": BINARY.len(), "sha256": digest(BINARY) },
            { "binary": "treer-agent-host", "platform": PLATFORM, "path": HOST, "size": BINARY.len(), "sha256": digest(BINARY) },
            { "binary": "treer-launch", "platform": PLATFORM, "path": LAUNCHER, "size": BINARY.len(), "sha256": digest(BINARY) },
        ],
    });
    put_signed(
        &mut objects,
        MANIFEST,
        serde_json::to_vec(&manifest).unwrap(),
    );
    resign_channel(&mut objects);
    objects.insert(CONTROLLER.into(), BINARY.to_vec());
    objects.insert(CLI.into(), BINARY.to_vec());
    objects.insert(HOST.into(), BINARY.to_vec());
    objects.insert(LAUNCHER.into(), BINARY.to_vec());
    objects
}

fn resign_channel(objects: &mut Objects) {
    put_signed(
        objects,
        CHANNEL,
        serde_json::to_vec(&json!({
            "schema_version": 1, "channel": "stable", "version": VERSION,
            "manifest_path": MANIFEST,
        }))
        .unwrap(),
    );
}

pub(crate) fn change_manifest(objects: &mut Objects, change: impl FnOnce(&mut Value)) {
    let mut manifest: Value = serde_json::from_slice(&objects[MANIFEST]).unwrap();
    change(&mut manifest);
    put_signed(objects, MANIFEST, serde_json::to_vec(&manifest).unwrap());
    resign_channel(objects);
}

pub(crate) async fn server(
    objects: Objects,
) -> (Url, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    let objects = Arc::new(objects);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = requests.clone();
    let app = Router::new().route(
        "/{*path}",
        any(move |request: Request<Body>| {
            let objects = objects.clone();
            let observed = observed.clone();
            async move {
                let path = request.uri().path().trim_start_matches('/');
                observed.lock().unwrap().push(path.to_owned());
                match objects.get(path) {
                    Some(bytes) => Response::builder().body(Body::from(bytes.clone())).unwrap(),
                    None => Response::builder().status(404).body(Body::empty()).unwrap(),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (base, requests, task)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap()
}

#[test]
fn official_release_key_matches_designated_authority() {
    let key = VerifyingKey::from_public_key_pem(OFFICIAL_PUBLIC_KEY_PEM).unwrap();
    assert_eq!(
        digest(key.to_public_key_der().unwrap().as_bytes()),
        "634761ff47a6a113b4b8925f0db0e030d8bfce07f2fa228fdc0a4904874dc41f"
    );
    assert_eq!(key_id(&key).unwrap(), "634761ff47a6a113");
    assert!(verify_signed(b"channel", &signature(b"channel"), &key).is_err());
}

#[test]
fn release_trust_resolution_preserves_explicit_and_existing_installation_keys() {
    use ed25519_dalek::pkcs8::EncodePublicKey;
    let directory =
        std::env::temp_dir().join(format!("treer-release-trust-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let local = directory.join("release-signing-key.pub.pem");
    let explicit = directory.join("explicit.pub.pem");
    let official = Url::parse("https://releases.treer.ai/").unwrap();
    let custom = Url::parse("https://self-hosted.example/releases/").unwrap();
    let bundled = signing_key()
        .verifying_key()
        .to_public_key_pem(Default::default())
        .unwrap();
    let other = SigningKey::from_bytes(&[8; 32]).verifying_key();
    let other_pem = other.to_public_key_pem(Default::default()).unwrap();

    assert_eq!(
        resolve_public_key(&official, None, &local, Some(&bundled)).unwrap(),
        signing_key().verifying_key()
    );
    assert!(resolve_public_key(&custom, None, &local, Some(&bundled)).is_err());
    assert!(resolve_public_key(
        &Url::parse("https://releases.treer.ai/another-publisher/").unwrap(),
        None,
        &local,
        Some(&bundled),
    )
    .is_err());
    assert!(resolve_public_key(&official, None, &local, None).is_err());
    assert!(resolve_public_key(&official, None, &local, Some("invalid bundled key")).is_err());
    assert!(resolve_public_key(&official, Some(&explicit), &local, Some(&bundled)).is_err());

    std::fs::write(&local, &other_pem).unwrap();
    assert_eq!(
        resolve_public_key(&official, None, &local, Some(&bundled)).unwrap(),
        other
    );
    assert_eq!(
        resolve_public_key(&custom, None, &local, Some(&bundled)).unwrap(),
        other
    );
    std::fs::write(&explicit, &bundled).unwrap();
    assert_eq!(
        resolve_public_key(&custom, Some(&explicit), &local, None).unwrap(),
        signing_key().verifying_key()
    );
    std::fs::write(&local, "invalid local key").unwrap();
    assert!(resolve_public_key(&official, None, &local, Some(&bundled)).is_err());
    std::fs::write(&explicit, "invalid explicit key").unwrap();
    assert!(resolve_public_key(&official, Some(&explicit), &local, Some(&bundled)).is_err());

    // The bundled key does not confer trust on another publisher's signatures.
    let selected =
        resolve_public_key(&official, None, &directory.join("absent"), Some(&other_pem)).unwrap();
    assert!(verify_signed(b"channel", &signature(b"channel"), &selected).is_err());
    std::fs::remove_file(local).unwrap();
    std::fs::remove_file(explicit).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

#[cfg(unix)]
#[test]
fn release_trust_resolution_does_not_ignore_broken_local_key_links() {
    let directory =
        std::env::temp_dir().join(format!("treer-release-trust-link-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let local = directory.join("key.pub.pem");
    std::os::unix::fs::symlink(directory.join("absent.pem"), &local).unwrap();
    let official = Url::parse("https://releases.treer.ai/").unwrap();
    assert!(resolve_public_key(&official, None, &local, None)
        .unwrap_err()
        .to_string()
        .starts_with("cannot read trusted release public key"));
    std::fs::remove_file(local).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

#[test]
fn release_verifier_accepts_node_crypto_publisher_wire_format() {
    // Generated with Node crypto from the public test seed [7; 32], using the
    // publisher's SPKI key ID and detached signature format (not a release key).
    let key = VerifyingKey::from_public_key_pem("-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEA6kpsY+KcUgq+9VB7Ey7F+ZVHdq6+vnuSQh7qaRRG0iw=\n-----END PUBLIC KEY-----\n").unwrap();
    let bytes = b"{\"schema_version\":1,\"channel\":\"stable\"}\n";
    let detached = serde_json::to_vec(&json!({
        "schema_version": 1, "algorithm": "ed25519", "key_id": "324be2dea8bc4446",
        "signature": "MKkMahEu8T2nbto7BHFtlEeOUaLz0aPEuWneBP2TEcB4PpRzf3ARYNrwd/4Xq7/B7U+/5iARdbJmDGOvMVv+Cw==",
    })).unwrap();
    verify_signed(bytes, &detached, &key).unwrap();
    assert!(verify_signed(b"tampered", &detached, &key).is_err());
    assert!(verify_signed(
        bytes,
        &detached,
        &SigningKey::from_bytes(&[8; 32]).verifying_key()
    )
    .is_err());
}

async fn fetch_release(objects: Objects, version: &str) -> (Result<VerifiedRelease>, Vec<String>) {
    let (base, requests, task) = server(objects).await;
    let result = download(
        &client(),
        base,
        version,
        &signing_key().verifying_key(),
        PLATFORM,
    )
    .await;
    task.abort();
    let observed = requests.lock().unwrap().clone();
    (result, observed)
}

#[tokio::test]
async fn signed_release_download_verifies_all_binaries() {
    let (release, requests) = fetch_release(fixture(), VERSION).await;
    let release = release.unwrap();
    assert_eq!(release.controller, BINARY);
    assert_eq!(release.cli, BINARY);
    assert_eq!(release.host.as_deref(), Some(BINARY));
    assert!(requests.contains(&CONTROLLER.to_owned()));
    // An exact version never consults a channel.
    assert!(!requests.iter().any(|path| path.starts_with("channels/")));

    assert_eq!(release.launcher.as_deref(), Some(BINARY));

    // Manifests that predate the launcher, then the Host, still verify.
    let mut older = fixture();
    change_manifest(&mut older, |m| {
        m["artifacts"].as_array_mut().unwrap().pop();
    });
    let release = fetch_release(older.clone(), VERSION).await.0.unwrap();
    assert!(release.launcher.is_none() && release.host.is_some());
    change_manifest(&mut older, |m| {
        m["artifacts"].as_array_mut().unwrap().pop();
    });
    assert!(fetch_release(older, VERSION).await.0.unwrap().host.is_none());
}

#[tokio::test]
async fn launcher_only_download_fetches_just_the_launcher() {
    let (base, requests, task) = server(fixture()).await;
    let key = signing_key().verifying_key();
    let bytes = download_launcher(&client(), base.clone(), VERSION, &key, PLATFORM)
        .await
        .unwrap();
    assert_eq!(bytes, BINARY);
    let fetched = requests.lock().unwrap().clone();
    assert!(fetched.contains(&LAUNCHER.to_owned()));
    assert!(!fetched.iter().any(|path| path == CONTROLLER || path == CLI || path == HOST));
    let untrusted = SigningKey::from_bytes(&[9; 32]).verifying_key();
    assert!(download_launcher(&client(), base, VERSION, &untrusted, PLATFORM)
        .await
        .is_err());
    task.abort();
}

#[tokio::test]
async fn signed_channel_names_the_version() {
    let (base, _, task) = server(fixture()).await;
    let key = signing_key().verifying_key();
    assert_eq!(
        channel_version(&client(), base.clone(), "stable", &key)
            .await
            .unwrap(),
        VERSION
    );
    assert!(channel_version(&client(), base, "canary", &key)
        .await
        .is_err());
    task.abort();
}

#[tokio::test]
async fn signed_release_rejects_bad_metadata_before_fetching_executables() {
    for case in [
        "missing signature",
        "forged signature",
        "wrong key",
        "manifest tamper",
        "version mismatch",
        "artifact path",
        "duplicate",
        "missing controller",
        "oversize",
        "metadata limit",
        "invalid version",
    ] {
        let mut objects = fixture();
        let mut version = VERSION;
        match case {
            "missing signature" => {
                objects.remove(&format!("{MANIFEST}.sig"));
            }
            "forged signature" | "wrong key" => {
                let mut detached: Value =
                    serde_json::from_slice(&objects[&format!("{MANIFEST}.sig")]).unwrap();
                let other = SigningKey::from_bytes(&[8; 32]);
                detached["signature"] =
                    json!(STANDARD.encode(other.sign(&objects[MANIFEST]).to_bytes()));
                if case == "wrong key" {
                    detached["key_id"] = json!(key_id(&other.verifying_key()).unwrap());
                }
                objects.insert(
                    format!("{MANIFEST}.sig"),
                    serde_json::to_vec(&detached).unwrap(),
                );
            }
            "manifest tamper" => {
                objects.get_mut(MANIFEST).unwrap().push(b' ');
            }
            "version mismatch" => {
                change_manifest(&mut objects, |m| m["version"] = json!("v0.1.12"))
            }
            "artifact path" => change_manifest(&mut objects, |m| {
                m["artifacts"][0]["path"] = json!("../evil")
            }),
            "duplicate" => change_manifest(&mut objects, |m| {
                let duplicate = m["artifacts"][0].clone();
                m["artifacts"].as_array_mut().unwrap().push(duplicate);
            }),
            "missing controller" => change_manifest(&mut objects, |m| {
                m["artifacts"].as_array_mut().unwrap().remove(0);
            }),
            "oversize" => change_manifest(&mut objects, |m| {
                m["artifacts"][0]["size"] = json!(MAX_BINARY_BYTES + 1)
            }),
            "metadata limit" => {
                objects.insert(MANIFEST.into(), vec![b' '; MAX_METADATA_BYTES + 1]);
            }
            "invalid version" => version = "v0.1.13/../../x",
            _ => unreachable!(),
        }
        let (result, requests) = fetch_release(objects, version).await;
        assert!(result.is_err(), "accepted {case}");
        assert!(
            !requests
                .iter()
                .any(|path| path == CONTROLLER || path == CLI || path == HOST),
            "fetched executable before rejecting {case}"
        );
    }
}

#[tokio::test]
async fn signed_release_rejects_corrupt_or_wrong_size_binary() {
    for bytes in [
        vec![b'x'; BINARY.len()],
        vec![b'x'; BINARY.len() - 1],
        vec![b'x'; BINARY.len() + 1],
    ] {
        let mut objects = fixture();
        objects.insert(HOST.into(), bytes);
        assert!(fetch_release(objects, VERSION).await.0.is_err());
    }
}

#[test]
fn update_rejects_channel_path_injection_and_non_ed25519_signatures() {
    for channel in ["", "../stable", "Stable", "stable?x=1", "https://evil/"] {
        assert!(validate_channel_name(channel).is_err());
    }
    let key = signing_key().verifying_key();
    for value in [
        json!({}),
        json!({"schema_version": 1, "algorithm": "ed25519", "key_id": key_id(&key).unwrap(), "signature": "not-base64"}),
    ] {
        assert!(verify_signed(b"test", &serde_json::to_vec(&value).unwrap(), &key).is_err());
    }
}
